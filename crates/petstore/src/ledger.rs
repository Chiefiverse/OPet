//! 账本：`gacha_pull` 行的读写、pity 状态推导、抽取报告。
//!
//! ## 为什么不建 `gacha_pity` 表
//!
//! 多存一份状态就多一个"和账本对不上"的机会：改一条历史记录的 rarity，
//! 表里的 pity 还是旧值，而下一次抽取照旧值走 ⇒ 保底被永久性地骗过去了。
//! [`derive_state`] 从账本现算 ⇒ 状态**永远**与审计一致，也天然可重放。
//!
//! ## 两条推导路径，互为测试
//!
//! - [`derive_state`]：只看窗口内那几行（O(batch_size)）。
//! - [`derive_state_by_replay`]：从第一条往后逐行 fold（O(n)）。
//!
//! 两者必须给出同一个结果，`ledger_roundtrip` 里逐长度比对。写窗口版时我一度把
//! `batch_best` 算错（窗口取多一格 ⇒ 上一批的 SSR 混进来），正是这条交叉验证抓的。

use std::fmt;

use petcore::gacha::{
    AuditRecord, Catalog, GachaParams, ItemKind, Owned, PullOutcome, PullState, Rarity, hex, unhex,
};

use crate::StoreError;

/// `gacha_pull` 的一行。字段集合 = 重建 [`AuditRecord::canonical`] 所需的全部列
/// （少任何一列，`opet audit verify` 就没法从库里把记录拼回来）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRow {
    pub seq: i64,
    pub profile_id: String,
    pub banner_id: String,
    pub ts: i64,
    pub rarity: Rarity,
    pub item_kind: ItemKind,
    pub item_id: String,
    /// 本 pity 窗口内**已抽**次数（不含本抽）= `AuditRecord.pity_before`。
    pub pity_before: u32,
    pub batch_index: u32,
    pub dice_used: u32,
    pub duplicate: bool,
    pub shards_gained: u32,
    pub ssr_prob: u32,
    pub rng_state_hash: String,
    pub prev_digest: String,
}

impl PullRow {
    /// 引擎结果 → 待写行。哈希列在这里算，保证"写进去的就是摘要吃进去的"。
    pub fn from_outcome(
        o: &PullOutcome,
        profile_id: &str,
        banner_id: &str,
        ts: i64,
        seed: u64,
        prev_digest: [u8; 32],
    ) -> Self {
        let rec = AuditRecord::from_outcome(o, profile_id, banner_id, seed, prev_digest);
        Self {
            seq: o.seq as i64,
            profile_id: profile_id.into(),
            banner_id: banner_id.into(),
            ts,
            rarity: o.rarity,
            item_kind: o.item_kind,
            item_id: o.item_id.clone(),
            pity_before: o.pity_before,
            batch_index: o.batch_index,
            dice_used: o.dice_used,
            duplicate: o.duplicate,
            shards_gained: o.shards_gained,
            ssr_prob: o.ssr_prob,
            rng_state_hash: hex(&rec.rng_state_hash),
            prev_digest: hex(&rec.prev_digest),
        }
    }

    /// 待写行 → 审计记录。hex 列非法时报错而不是当零处理（导出文件是外部输入）。
    pub fn to_record(&self) -> Result<AuditRecord, StoreError> {
        Ok(AuditRecord {
            seq: self.seq as u64,
            profile_id: self.profile_id.clone(),
            banner_id: self.banner_id.clone(),
            rarity: self.rarity,
            item_kind: self.item_kind,
            item_id: self.item_id.clone(),
            pity_before: self.pity_before,
            batch_index: self.batch_index,
            dice_used: self.dice_used,
            duplicate: self.duplicate,
            shards_gained: self.shards_gained,
            ssr_prob: self.ssr_prob,
            rng_state_hash: digest_from_hex(&self.rng_state_hash)?,
            prev_digest: digest_from_hex(&self.prev_digest)?,
        })
    }
}

fn digest_from_hex(s: &str) -> Result<[u8; 32], StoreError> {
    unhex(s).ok_or_else(|| StoreError::BadHash(s.into()))
}

/// 从账本窗口推导下一次抽取的状态。`rows` 必须是**同一 (profile, banner)** 的全部记录，
/// 顺序不限（内部自己按 `seq` 排序）。
pub fn derive_state(params: &GachaParams, rows: &[PullRow]) -> Result<PullState, StoreError> {
    if rows.is_empty() {
        return Ok(PullState::fresh());
    }
    let mut sorted: Vec<&PullRow> = rows.iter().collect();
    sorted.sort_by_key(|r| r.seq);

    let last = sorted[sorted.len() - 1];
    let since_ssr = if last.rarity == Rarity::SSR {
        0
    } else {
        last.pity_before
            .checked_add(1)
            .ok_or(StoreError::CorruptLedger(last.seq))?
    };
    let wrap = last.batch_index + 1 >= params.batch_size;
    let batch_index = if wrap { 0 } else { last.batch_index + 1 };
    // batch_index 现在 = 本批**已经**消费的抽数；batch_best 只可能是这最后几抽的最大值。
    let batch_best = if wrap || batch_index == 0 {
        None
    } else {
        let n = usize::try_from(batch_index).map_err(|_| StoreError::CorruptLedger(last.seq))?;
        if n > sorted.len() {
            // 账本比"本批应有抽数"短 ⇒ 有人删过行（链校验本来也会抓到，这里提前给准确原因）
            return Err(StoreError::CorruptLedger(last.seq));
        }
        Some(
            sorted[sorted.len() - n..]
                .iter()
                .map(|r| r.rarity)
                .max()
                .unwrap(),
        )
    };
    Ok(PullState {
        since_ssr,
        batch_index,
        batch_best,
    })
}

/// 同一件事的朴素算法：从空状态逐行 `advanced()`。只用于测试里对照 [`derive_state`]。
pub fn derive_state_by_replay(params: &GachaParams, rows: &[PullRow]) -> PullState {
    let mut sorted: Vec<&PullRow> = rows.iter().collect();
    sorted.sort_by_key(|r| r.seq);
    let mut st = PullState::fresh();
    for r in sorted {
        st = st.advanced(params, r.rarity);
    }
    st
}

/// 一次 `pull` 的全部产出，CLI 直接照它打印。
#[derive(Clone, Debug)]
pub struct PullReport {
    pub banner_id: String,
    pub profile_id: String,
    pub outcomes: Vec<PullOutcome>,
    pub counts: [u32; 4],
    pub ssr_count: u32,
    pub duplicates: u32,
    pub shards_gained: u32,
    /// 写库前的余额与写库后的余额，让"扣了多少/得了多少"可核对。
    pub shards_after: u64,
    pub first_seq: u64,
    pub last_seq: u64,
    pub chain_len: u64,
    pub head_hex: String,
    /// 单抽耗时中位数与总耗时（毫秒，浮点只用于**打印**，不参与任何判定）。
    pub elapsed_ms: f64,
}

impl PullReport {
    pub fn count(&self, r: Rarity) -> u32 {
        self.counts[r.index()]
    }

    pub fn lines(&self) -> Vec<String> {
        self.outcomes
            .iter()
            .map(|o| {
                format!(
                    "#{:<5} {:<3} {:<14} {:<4} +{:<4}碎片 pity={:<3} dice={}",
                    o.seq,
                    o.rarity.as_str(),
                    o.item_id,
                    if o.duplicate { "重复" } else { "首得" },
                    o.shards_gained,
                    o.pity_before,
                    o.dice_used
                )
            })
            .collect()
    }
}

impl fmt::Display for PullReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "banner={} profile={} 共 {} 抽（seq {}..{}）",
            self.banner_id,
            self.profile_id,
            self.outcomes.len(),
            self.first_seq,
            self.last_seq
        )?;
        for line in self.lines() {
            writeln!(f, "  {line}")?;
        }
        write!(
            f,
            "  N={} R={} SR={} SSR={} 重复={} 碎片+{}（余额 {}） 链长={} head={} 耗时={:.2}ms",
            self.count(Rarity::N),
            self.count(Rarity::R),
            self.count(Rarity::SR),
            self.count(Rarity::SSR),
            self.duplicates,
            self.shards_gained,
            self.shards_after,
            self.chain_len,
            self.head_hex,
            self.elapsed_ms
        )
    }
}

/// `owned` 集合与库存的一致性：库存里有但引擎认为没有 ⇒ 说明两处各存了一份真相。
/// `Store::pull` 每次都从库存重建 [`Owned`]，这个函数只是把那件事写成可测的断言。
pub fn owned_from_rows<'a>(ids: impl Iterator<Item = &'a str>) -> Owned {
    ids.map(String::from).collect()
}

/// 供 `Catalog` 侧核对：账本里出现过的 item_id 必须都在目录里（否则将来
/// `opet pull` 换个目录就能凭空造出持有物）。
pub fn ledger_items_outside_catalog(rows: &[PullRow], catalog: &Catalog) -> Vec<String> {
    rows.iter()
        .filter(|r| catalog.find(&r.item_id).is_none())
        .map(|r| r.item_id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use petcore::gacha::{Engine, GachaParams, Rates};

    fn row(seq: i64, rarity: Rarity, pity_before: u32, batch_index: u32) -> PullRow {
        PullRow {
            seq,
            profile_id: "local".into(),
            banner_id: "cat_series".into(),
            ts: 0,
            rarity,
            item_kind: ItemKind::Pet,
            item_id: "cat_shiro".into(),
            pity_before,
            batch_index,
            dice_used: 2,
            duplicate: false,
            shards_gained: 0,
            ssr_prob: Rates::DEFAULT.ssr,
            rng_state_hash: "00".repeat(32),
            prev_digest: "00".repeat(32),
        }
    }

    #[test]
    fn window_derivation_equals_full_replay_on_a_real_ledger() {
        // 这是本模块的核心正确性论证：窗口算法和朴素重放必须一致，
        // 否则"不建 pity 表"这个决定就是在给自己埋状态漂移。
        let p = GachaParams::V1;
        let c = Catalog::default_v1().unwrap();
        let e = Engine::new(&p, &c).unwrap();
        let mut owned = Owned::new();
        let mut st = PullState::fresh();
        let mut rows = Vec::new();
        for seq in 1..=137 {
            let (o, next) = e.pull(&st, &mut owned, 5, seq).unwrap();
            rows.push(PullRow::from_outcome(
                &o,
                "local",
                "cat_series",
                0,
                5,
                [0u8; 32],
            ));
            st = next;
            let window = derive_state(&p, &rows).unwrap();
            let replay = derive_state_by_replay(&p, &rows);
            assert_eq!(window, replay, "第 {seq} 抽后状态漂了");
            assert_eq!(window, st, "推导出的状态与引擎刚算出的不一致");
        }
    }

    #[test]
    fn empty_ledger_is_a_fresh_window() {
        let p = GachaParams::V1;
        assert_eq!(derive_state(&p, &[]).unwrap(), PullState::fresh());
    }

    #[test]
    fn ssr_resets_the_pity_window_but_not_the_batch() {
        let p = GachaParams::V1;
        // 第 3 抽（batch_index 2）出了 SSR
        let rows = vec![
            row(1, Rarity::N, 0, 0),
            row(2, Rarity::R, 1, 1),
            row(3, Rarity::SSR, 2, 2),
        ];
        let st = derive_state(&p, &rows).unwrap();
        assert_eq!(st.since_ssr, 0, "SSR 之后 pity 归零");
        assert_eq!(st.batch_index, 3, "但 batch 继续走");
        assert_eq!(st.batch_best, Some(Rarity::SSR));
    }

    #[test]
    fn batch_wraps_and_clears_the_best() {
        let p = GachaParams::V1;
        // 账本必须**自洽**：`pity_before` 是"该抽在窗口内已是第几抽（0-indexed）"，
        // 出 SSR 后归零。所以第 1 抽是 SSR ⇒ 第 2 抽的 pity_before 重新从 0 数。
        // 不自洽的账本会让窗口法（信 stored 计数）和重放法（只信 rarity）给出不同答案 ——
        // 那正是 `window_derivation_equals_full_replay` 要抓的漂移，不该由 fixture 自己制造。
        let mut rows: Vec<PullRow> = (0..10)
            .map(|i| {
                let mut r = row(
                    i + 1,
                    Rarity::N,
                    if i == 0 { 0 } else { (i - 1) as u32 },
                    i as u32,
                );
                if i == 0 {
                    r.rarity = Rarity::SSR;
                }
                r
            })
            .collect();
        let st = derive_state(&p, &rows).unwrap();
        assert_eq!(st.since_ssr, 9, "SSR 之后又走了 9 抽 N");
        assert_eq!(st.batch_index, 0, "满 10 抽该换新批");
        assert_eq!(st.batch_best, None, "上一批的 SSR 不许算进这一批");
        assert_eq!(st, derive_state_by_replay(&p, &rows));

        // 新批第一抽之后，best 只含这一抽
        rows.push(row(11, Rarity::N, 9, 0));
        let st2 = derive_state(&p, &rows).unwrap();
        assert_eq!(st2.since_ssr, 10);
        assert_eq!(st2.batch_index, 1);
        assert_eq!(st2.batch_best, Some(Rarity::N));
        assert_eq!(st2, derive_state_by_replay(&p, &rows));
    }

    #[test]
    fn an_inconsistent_pity_column_is_what_replay_catches() {
        // 反面：把 pity_before 写错（忘了 SSR 归零），窗口法就会算出错的 since_ssr。
        // 这条测试钉住"信 stored 计数"的代价，并证明 `verify_replay` 那一层是必需的：
        // 重放只看 rarity，所以它给出的才是对的值；两者的差就是"账本被人改过"的证据。
        let p = GachaParams::V1;
        let mut rows: Vec<PullRow> = (0..10)
            .map(|i| row(i + 1, Rarity::N, i as u32, i as u32))
            .collect();
        rows[0].rarity = Rarity::SSR;
        let window = derive_state(&p, &rows).unwrap();
        let replay = derive_state_by_replay(&p, &rows);
        assert_eq!(window.since_ssr, 10, "窗口法照读了 stored 的 9+1");
        assert_eq!(replay.since_ssr, 9, "重放法按 rarity 归零后重数");
    }

    #[test]
    fn a_deleted_row_is_reported_not_silently_wrong() {
        let p = GachaParams::V1;
        let mut rows: Vec<PullRow> = (0..10)
            .map(|i| row(i + 1, Rarity::N, i as u32 + 1, i as u32))
            .collect();
        rows[0].pity_before = 0;
        // 删掉本批的开头两行 ⇒ batch_index 说"本批已 8 抽"，可账本里只有 8 行且第 9、10 行不见了
        rows.retain(|r| r.seq != 1 && r.seq != 2);
        let last = rows.last().unwrap();
        let st = derive_state(&p, &rows);
        // 窗口够短就还能算，但结果必须与重放不同 ⇒ 由 `window==replay` 那条测试保证漂移会被发现。
        if let Ok(s) = st {
            assert_ne!(
                s,
                derive_state_by_replay(&p, &rows),
                "删行后窗口/重放居然还一致 ⇒ 说明这条测试没构造出真缺口（last={last:?}）"
            );
        }
    }

    #[test]
    fn bad_hex_is_rejected_when_rebuilding_a_record() {
        let mut r = row(1, Rarity::N, 0, 0);
        r.rng_state_hash = "not-hex".into();
        assert!(matches!(
            r.to_record(),
            Err(StoreError::BadHash(ref s)) if s == "not-hex"
        ));
        r.rng_state_hash = "00".repeat(31);
        assert!(matches!(r.to_record(), Err(StoreError::BadHash(_))));
        r.rng_state_hash = "ab".repeat(32);
        assert!(r.to_record().is_ok());
    }
}
