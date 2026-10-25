//! 审计链：让"公示概率 == 代码概率"这句话**可被第三方检验**（§3.6:592、§6.1 M3 门禁）。
//!
//! 三个冻结量，改任何一个都必须升版本，否则历史全部对不上：
//! - [`RNG_DOMAIN`] / [`CHAIN_DOMAIN`]：域分隔前缀，防止"另一种用途的哈希"撞进来。
//! - [`REC_FORMAT`]：[`AuditRecord::canonical`] 的字节格式版本。
//! - band 顺序与每抽骰数（见 [`super::pity`] 与 [`super::engine`]）：改了 ⇒ 同一 `seed+seq`
//!   重掷出不同结果 ⇒ 这里的重放校验会红，这是故意的。
//!
//! ## 两个哈希各自的职责
//!
//! `rng_state_hash = sha256(RNG_DOMAIN ‖ u64be(seed) ‖ u64be(seq))`
//! —— **掷骰前**的挑战值。第三方只要拿到 `seed` 和 `seq` 就能自己重掷，再比对账本里的结果；
//! 所以它**不含 `item_id`**（设计书 §3.6:578 那个 `sha256(seed||pull_counter||item_id)` 的写法
//! 是"结果自证"，验不出骰子被动过，已作废）。
//!
//! `audit_prev_hash` = 上一条记录的 [`AuditRecord::digest`]（创世 = 全零）。
//! 记录自身不存 digest，digest 由 `canonical` 现算 ⇒ 改任意字段都会让它和下一条存的 prev 失配。
//! 链尾另存 `meta.audit_head` + `meta.audit_len`，所以**删尾**也会被查出。

use std::fmt;

use sha2::{Digest, Sha256};

use super::catalog::{Catalog, ItemKind};
use super::engine::{Engine, Owned, PullOutcome};
use super::params::GachaParams;
use super::rarity::Rarity;

/// `rng_state_hash` 的域分隔前缀（含换行，避免与裸拼接混淆）。
pub const RNG_DOMAIN: &[u8] = b"OPET-GACHA-RNG-1\n";
/// 链式 digest 的域分隔前缀。
pub const CHAIN_DOMAIN: &[u8] = b"OPET-AUDIT-1\n";
/// [`AuditRecord::canonical`] 的格式版本。**只有追加字段这一种改法**，
/// 删/改语义必须升号，否则旧导出会被当成合法记录解析。
pub const REC_FORMAT: u16 = 1;
pub const DIGEST_LEN: usize = 32;
/// 第一条记录的 `prev`。
pub const GENESIS: [u8; DIGEST_LEN] = [0u8; DIGEST_LEN];

/// 32 字节 → 小写 hex（DB 列与导出文件都用 hex 字符串）。
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// hex → 32 字节；长度或字符非法返回 `None`（不 panic，导出文件是外部输入）。
pub fn unhex(s: &str) -> Option<[u8; DIGEST_LEN]> {
    if s.len() != DIGEST_LEN * 2 {
        return None;
    }
    let mut out = [0u8; DIGEST_LEN];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// 该抽的骰子承诺值。`seq` 用 DB 的 `gacha_pull.seq`，两边必须同一个数。
pub fn rng_state_hash(seed: u64, seq: u64) -> [u8; DIGEST_LEN] {
    let mut h = Sha256::new();
    h.update(RNG_DOMAIN);
    h.update(seed.to_be_bytes());
    h.update(seq.to_be_bytes());
    let out = h.finalize();
    let mut arr = [0u8; DIGEST_LEN];
    arr.copy_from_slice(&out);
    arr
}

/// 一条可独立检验的抽取记录。字段集合 = 重放所需的最小集：
/// 少任何一个，第三方都无法只凭导出文件重算。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRecord {
    pub seq: u64,
    pub profile_id: String,
    pub banner_id: String,
    pub rarity: Rarity,
    /// `pet` / `accessory`。**故意不进 [`AuditRecord::canonical`] 的字节**：它由
    /// `item_id` + 目录唯一决定，进摘要只是把同一件事绑两次。但导出与库里都存它，
    /// 重放时照样比对 ⇒ 换过目录后"同一个 id 变成另一种 kind"会被抓到。
    pub item_kind: ItemKind,
    pub item_id: String,
    pub pity_before: u32,
    pub batch_index: u32,
    pub dice_used: u32,
    pub duplicate: bool,
    pub shards_gained: u32,
    /// 判定实际使用的 SSR 概率（百万分位）。存它是为了让审计能核对**保底数学**，
    /// 而不只是核对骰子。
    pub ssr_prob: u32,
    pub rng_state_hash: [u8; DIGEST_LEN],
    /// 上一条的 digest，对应 `gacha_pull.audit_prev_hash` 列。
    pub prev_digest: [u8; DIGEST_LEN],
}

impl AuditRecord {
    /// 从引擎结果建记录（`petstore` 写库前调它）。
    pub fn from_outcome(
        o: &PullOutcome,
        profile_id: &str,
        banner_id: &str,
        seed: u64,
        prev_digest: [u8; DIGEST_LEN],
    ) -> Self {
        Self {
            seq: o.seq,
            profile_id: profile_id.into(),
            banner_id: banner_id.into(),
            rarity: o.rarity,
            item_kind: o.item_kind,
            item_id: o.item_id.clone(),
            pity_before: o.pity_before,
            batch_index: o.batch_index,
            dice_used: o.dice_used,
            duplicate: o.duplicate,
            shards_gained: o.shards_gained,
            ssr_prob: o.ssr_prob,
            rng_state_hash: rng_state_hash(seed, o.seq),
            prev_digest,
        }
    }

    /// **大端、定长、带 `REC_FORMAT` 头**的规范编码。
    /// 不依赖 Rust `Debug`、不依赖 JSON key 顺序、不依赖平台 `usize` 宽度。
    pub fn canonical(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(128);
        b.extend_from_slice(&REC_FORMAT.to_be_bytes());
        b.extend_from_slice(&self.seq.to_be_bytes());
        push_str(&mut b, &self.profile_id);
        push_str(&mut b, &self.banner_id);
        b.push(self.rarity.index() as u8);
        push_str(&mut b, &self.item_id);
        b.extend_from_slice(&self.pity_before.to_be_bytes());
        b.extend_from_slice(&self.batch_index.to_be_bytes());
        b.extend_from_slice(&self.dice_used.to_be_bytes());
        b.push(self.duplicate as u8);
        b.extend_from_slice(&self.shards_gained.to_be_bytes());
        b.extend_from_slice(&self.ssr_prob.to_be_bytes());
        b.extend_from_slice(&self.rng_state_hash);
        b.extend_from_slice(&self.prev_digest);
        b
    }

    /// 本记录的摘要（链上被下一条引用的那个值）。
    pub fn digest(&self) -> [u8; DIGEST_LEN] {
        let mut h = Sha256::new();
        h.update(CHAIN_DOMAIN);
        h.update(self.canonical());
        let out = h.finalize();
        let mut arr = [0u8; DIGEST_LEN];
        arr.copy_from_slice(&out);
        arr
    }
}

fn push_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainReport {
    pub len: u64,
    pub head: [u8; DIGEST_LEN],
    /// 链里出现过的每种稀有度计数，`verify_replay` 顺手给出，省一次遍历。
    pub counts: [u64; 4],
}

impl ChainReport {
    pub fn head_hex(&self) -> String {
        hex(&self.head)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditError {
    /// 空链：没什么可验的，但调用方要区分"没有记录"和"记录被删光"。
    Empty,
    /// `seq` 不连续或不从 1 起 ⇒ 有人整段删过记录。
    SequenceGap { at: u64, expect: u64 },
    /// 第一条的 prev 不是创世。
    NotGenesis { at: u64 },
    /// 第 `at` 条存的 prev 与上一条实算的 digest 不符（改过中间某条，或插/删了一条）。
    BrokenLink {
        at: u64,
        expect: String,
        got: String,
    },
    /// 链尾与 `meta.audit_head` 不一致 ⇒ 尾部被删或被替换。
    HeadMismatch { stored: String, got: String },
    /// 导出文件里的 hash 字段不是 64 个 hex 字符。
    BadHash(String),
    /// 重放出的结果与记录不符：**这是"骰子被动过"的直接证据**。
    ReplayMismatch {
        at: u64,
        field: &'static str,
        expect: String,
        got: String,
    },
    /// 重放时引擎报错（参数/目录与当年不一致）。
    Engine(super::engine::GachaError),
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "链是空的"),
            Self::SequenceGap { at, expect } => {
                write!(f, "seq 不连续：第 {at} 条本该是 {expect}")
            }
            Self::NotGenesis { at } => write!(f, "第 {at} 条的 prev 不是创世值"),
            Self::BrokenLink { at, expect, got } => write!(
                f,
                "第 {at} 条的 prev={got} 与上一条实算 digest={expect} 不符 ⇒ 链断在这里"
            ),
            Self::HeadMismatch { stored, got } => {
                write!(
                    f,
                    "存了 audit_head={stored}，实算链尾={got} ⇒ 尾部被改或被删"
                )
            }
            Self::BadHash(s) => write!(f, "非法哈希: {s}"),
            Self::ReplayMismatch {
                at,
                field,
                expect,
                got,
            } => write!(
                f,
                "第 {at} 条的 {field} 与重放结果不符: 记录={expect} 重放={got}"
            ),
            Self::Engine(e) => write!(f, "重放时引擎拒绝: {e}"),
        }
    }
}

impl std::error::Error for AuditError {}

/// 只验哈希链（不需要 seed）。返回链长与链尾摘要。
pub fn verify_chain(records: &[AuditRecord]) -> Result<ChainReport, AuditError> {
    if records.is_empty() {
        return Err(AuditError::Empty);
    }
    let mut counts = [0u64; 4];
    let mut prev = GENESIS;
    for (expect_seq, r) in (1u64..).zip(records) {
        if r.seq != expect_seq {
            return Err(AuditError::SequenceGap {
                at: r.seq,
                expect: expect_seq,
            });
        }
        if r.prev_digest != prev {
            return Err(AuditError::BrokenLink {
                at: r.seq,
                expect: hex(&prev),
                got: hex(&r.prev_digest),
            });
        }
        counts[r.rarity.index()] += 1;
        prev = r.digest();
    }
    Ok(ChainReport {
        len: records.len() as u64,
        head: prev,
        counts,
    })
}

/// 链 + 链尾锚点。`stored_head` 从 `meta.audit_head` 读。
pub fn verify_chain_anchored(
    records: &[AuditRecord],
    stored_head: &str,
) -> Result<ChainReport, AuditError> {
    let stored = unhex(stored_head).ok_or_else(|| AuditError::BadHash(stored_head.into()))?;
    let report = verify_chain(records)?;
    if stored != report.head {
        return Err(AuditError::HeadMismatch {
            stored: stored_head.into(),
            got: report.head_hex(),
        });
    }
    Ok(report)
}

/// **完整重放**：拿 seed 把引擎从头跑一遍，逐条比对。
///
/// 这是 §6.1"审计重放脚本可校验任意记录"的真正口径——哈希链只能证明"记录之间自洽"，
/// 重放才能证明"记录和骰子自洽"。前提是 `seq` 从 1 开始、无删减（链校验已经管这个）。
pub fn verify_replay(
    seed: u64,
    params: &GachaParams,
    catalog: &Catalog,
    records: &[AuditRecord],
) -> Result<ChainReport, AuditError> {
    let report = verify_chain(records)?;
    let engine = Engine::new(params, catalog).map_err(AuditError::Engine)?;
    let mut state = super::pity::PullState::fresh();
    let mut owned = Owned::new();
    for r in records {
        if r.rng_state_hash != rng_state_hash(seed, r.seq) {
            return Err(AuditError::ReplayMismatch {
                at: r.seq,
                field: "rng_state_hash",
                expect: hex(&r.rng_state_hash),
                got: hex(&rng_state_hash(seed, r.seq)),
            });
        }
        let (o, next) = engine
            .pull(&state, &mut owned, seed, r.seq)
            .map_err(AuditError::Engine)?;
        check(&o.rarity, &r.rarity, "rarity", r.seq)?;
        check(&o.item_kind, &r.item_kind, "item_kind", r.seq)?;
        check(&o.item_id, &r.item_id, "item_id", r.seq)?;
        check(&o.pity_before, &r.pity_before, "pity_before", r.seq)?;
        check(&o.batch_index, &r.batch_index, "batch_index", r.seq)?;
        check(&o.dice_used, &r.dice_used, "dice_used", r.seq)?;
        check(&o.ssr_prob, &r.ssr_prob, "ssr_prob", r.seq)?;
        check(&o.duplicate, &r.duplicate, "duplicate", r.seq)?;
        check(&o.shards_gained, &r.shards_gained, "shards_gained", r.seq)?;
        state = next;
    }
    Ok(report)
}

fn check<T: PartialEq + fmt::Display>(
    got: &T,
    expect: &T,
    field: &'static str,
    seq: u64,
) -> Result<(), AuditError> {
    if got == expect {
        Ok(())
    } else {
        Err(AuditError::ReplayMismatch {
            at: seq,
            field,
            expect: expect.to_string(),
            got: got.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gacha::params::Rates;

    fn rec(seq: u64, prev: [u8; DIGEST_LEN], rarity: Rarity) -> AuditRecord {
        AuditRecord {
            seq,
            profile_id: "local".into(),
            banner_id: "cat_series".into(),
            rarity,
            item_kind: super::ItemKind::Pet,
            item_id: "cat_shiro".into(),
            pity_before: 0,
            batch_index: (seq as u32) % 10,
            dice_used: 2,
            duplicate: false,
            shards_gained: 0,
            ssr_prob: Rates::DEFAULT.ssr,
            rng_state_hash: rng_state_hash(7, seq),
            prev_digest: prev,
        }
    }

    #[test]
    fn golden_hashes_lock_the_encoding() {
        // 冻结值（用 Python `hashlib` 独立算出，不是拿 Rust 输出回填）：
        // 改 `canonical` 的任何字节序、任何域前缀，这里立刻红。
        assert_eq!(
            hex(&rng_state_hash(1, 1)),
            "82fdd654e7bde32a70b5c688fc7af574128150ab95ed4c206101bed4b062db29"
        );
        assert_eq!(
            hex(&rng_state_hash(7, 1)),
            "c485327971aec2aeba99943029417900adc4d96dddb8765f69e34df372bde015",
            "seed 或 seq 变一个比特，摘要就该整个变"
        );
        let r = rec(1, GENESIS, Rarity::N);
        assert_eq!(
            hex(&r.digest()),
            "e6f210730cedccde355b594d2020c14447c22082774cca241aeef4e8c8e6489e"
        );
    }

    #[test]
    fn hex_round_trips_and_refuses_junk() {
        let h = rng_state_hash(3, 9);
        assert_eq!(
            hex(&h),
            "287289d8e5510842e44fc51c77a05583f80609052beb30497418cd37726eb424"
        );
        let s = hex(&h);
        assert_eq!(s.len(), 64);
        assert_eq!(unhex(&s).unwrap(), h);
        for bad in ["", "zz", &s[..63], &format!("{s}00")] {
            assert!(unhex(bad).is_none(), "本该拒绝 {bad:?}");
        }
    }

    #[test]
    fn chain_is_strict_and_detects_every_tampering() {
        let a = rec(1, GENESIS, Rarity::N);
        let b = rec(2, a.digest(), Rarity::R);
        let c = rec(3, b.digest(), Rarity::SSR);
        let chain = [a.clone(), b.clone(), c.clone()];
        let report = verify_chain(&chain).unwrap();
        assert_eq!(report.len, 3);
        assert_eq!(report.head, c.digest());
        assert_eq!(report.head_hex(), hex(&c.digest()));
        assert_eq!(report.counts[Rarity::SSR.index()], 1);

        // 锚点：存对了过，存错了是"尾部被动"。
        assert!(verify_chain_anchored(&chain, &report.head_hex()).is_ok());
        assert!(matches!(
            verify_chain_anchored(&chain, &hex(&b.digest())),
            Err(AuditError::HeadMismatch { .. })
        ));

        // 改中间一条的任意字段 ⇒ 下一条的 prev 失配。
        let mut tampered = chain.to_vec();
        tampered[1].item_id = "dragon_pip".into();
        assert!(matches!(
            verify_chain(&tampered),
            Err(AuditError::BrokenLink { at: 3, .. })
        ));

        // 改 prev 本身也跑不掉（自己的 digest 变了，被下一条引用）。
        let mut prev_hacked = chain.to_vec();
        prev_hacked[1].prev_digest = GENESIS;
        assert!(matches!(
            verify_chain(&prev_hacked),
            Err(AuditError::BrokenLink { .. })
        ));

        // 删头 / 删中间 ⇒ 序号或 prev 立刻不连续。
        assert!(matches!(
            verify_chain(&[b.clone(), c.clone()]),
            Err(AuditError::SequenceGap { .. })
        ));
        let mut holes = chain.to_vec();
        holes.remove(1);
        assert!(matches!(
            verify_chain(&holes),
            Err(AuditError::SequenceGap { .. }) | Err(AuditError::BrokenLink { .. })
        ));
        // 改第一条的 prev 成别的值。
        let mut not_genesis = chain.to_vec();
        not_genesis[0].prev_digest = [1u8; DIGEST_LEN];
        assert_eq!(
            verify_chain(&not_genesis),
            Err(AuditError::BrokenLink {
                at: 1,
                expect: hex(&GENESIS),
                got: hex(&[1u8; DIGEST_LEN])
            })
        );
        assert_eq!(verify_chain(&[]), Err(AuditError::Empty));
    }

    #[test]
    fn canonical_is_prefix_free_and_format_versioned() {
        let a = rec(1, GENESIS, Rarity::N);
        let bytes = a.canonical();
        assert_eq!(&bytes[..2], &REC_FORMAT.to_be_bytes());
        assert_eq!(&bytes[2..10], &1u64.to_be_bytes());
        assert_eq!(bytes.len(), 132, "字节布局漂了（改这里必须升 REC_FORMAT）");
        // 同一条记录两次编码逐字节相同（没有 HashMap 顺序、没有平台宽度依赖）。
        assert_eq!(bytes, a.canonical());
        // 字符串带长度前缀 ⇒ "ab"+"c" 与 "a"+"bc" 不能撞车。
        let mut x = a.clone();
        x.profile_id = "ab".into();
        x.banner_id = "c".into();
        let mut y = a.clone();
        y.profile_id = "a".into();
        y.banner_id = "bc".into();
        assert_ne!(x.digest(), y.digest());
    }

    /// 真引擎跑出来的诚实链。
    fn real_chain(seed: u64, n: u64) -> (GachaParams, Catalog, Vec<AuditRecord>) {
        use crate::gacha::pity::PullState;

        let p = GachaParams::V1;
        let c = Catalog::default_v1().unwrap();
        let e = Engine::new(&p, &c).unwrap();
        let mut state = PullState::fresh();
        let mut owned = Owned::new();
        let mut recs = Vec::new();
        let mut prev = GENESIS;
        for seq in 1..=n {
            let (o, next) = e.pull(&state, &mut owned, seed, seq).unwrap();
            let r = AuditRecord::from_outcome(&o, "local", &c.banner_id, seed, prev);
            prev = r.digest();
            recs.push(r);
            state = next;
        }
        (p, c, recs)
    }

    /// 攻击者重算整条链（他有权改导出文件），让哈希链重新自洽。
    fn relink(records: &mut [AuditRecord]) {
        let mut prev = GENESIS;
        for r in records.iter_mut() {
            r.prev_digest = prev;
            prev = r.digest();
        }
    }

    #[test]
    fn honest_chain_replays_and_head_matches() {
        let (p, c, recs) = real_chain(11, 30);
        let report = verify_replay(11, &p, &c, &recs).unwrap();
        assert_eq!(report.len, 30);
        assert_eq!(report.head, verify_chain(&recs).unwrap().head);
        let total: u64 = report.counts.iter().sum();
        assert_eq!(total, 30);
    }

    #[test]
    fn a_forger_who_relinks_the_chain_still_fails_replay() {
        // 重放层的意义就在这里：链自洽只证明"记录之间没被改过"，
        // 不证明"骰子没被改过"。只靠哈希链的审计是可以被这样伪造掉的。
        let (p, c, mut recs) = real_chain(11, 30);
        recs[4].rarity = Rarity::SSR;
        recs[4].item_id = "dragon_pip".into();
        relink(&mut recs);
        assert!(
            verify_chain(&recs).is_ok(),
            "重链之后哈希链理应自洽，否则这条测试没意义"
        );
        let err = verify_replay(11, &p, &c, &recs).unwrap_err();
        assert!(
            matches!(
                err,
                AuditError::ReplayMismatch {
                    at: 5,
                    field: "rarity",
                    ..
                }
            ),
            "本该在第 5 条抓到 rarity 造假，实得 {err}"
        );
    }

    #[test]
    fn claiming_someone_elses_seed_is_caught() {
        // 换 seed 不改链：rng_state_hash 在 canonical 里，但它同时也是重放的输入，
        // 拿别人的 seed 来，第一条就对不上。
        let (p, c, recs) = real_chain(11, 10);
        let err = verify_replay(12, &p, &c, &recs).unwrap_err();
        assert!(
            matches!(
                err,
                AuditError::ReplayMismatch {
                    at: 1,
                    field: "rng_state_hash",
                    ..
                }
            ),
            "本该抓到 rng_state_hash，实得 {err}"
        );
    }

    #[test]
    fn relinked_pity_and_duplicate_cannot_hide_either() {
        // 造假"我第 30 抽才吃到保底"这种改法：改 pity_before 后重链，重放照样抓。
        let (p, c, mut recs) = real_chain(11, 30);
        recs[29].pity_before = 79;
        relink(&mut recs);
        assert!(verify_chain(&recs).is_ok());
        assert!(matches!(
            verify_replay(11, &p, &c, &recs),
            Err(AuditError::ReplayMismatch {
                at: 30,
                field: "pity_before",
                ..
            })
        ));
        // 把重复伪装成首得（少给碎片）也一样。
        let (p2, c2, mut recs2) = real_chain(21, 60);
        let idx = recs2
            .iter()
            .position(|r| r.duplicate)
            .expect("60 抽里必有重复（目录只有 30 件）");
        recs2[idx].duplicate = false;
        recs2[idx].shards_gained = 0;
        relink(&mut recs2);
        assert!(verify_chain(&recs2).is_ok());
        let err = verify_replay(21, &p2, &c2, &recs2).unwrap_err();
        assert!(
            matches!(
                err,
                AuditError::ReplayMismatch {
                    field: "duplicate",
                    ..
                }
            ),
            "实得 {err}"
        );
    }

    #[test]
    fn replay_refuses_a_different_parameter_set() {
        // 公示参数换了（比如把 soft_start 改成 30），重放必须炸，而不是"换个算法继续"。
        let (mut p, c, recs) = real_chain(11, 30);
        p.soft_start = 30;
        assert!(matches!(
            verify_replay(11, &p, &c, &recs),
            Err(AuditError::ReplayMismatch { .. })
        ));
    }
}
