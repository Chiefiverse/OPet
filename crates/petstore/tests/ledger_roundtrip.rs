//! `Store` 的账本往返：抽 → 落库 → 读回来 → 推导状态 → 再抽。
//!
//! 本文件最重要的三条（其余都是它们的边角）：
//! - [`splitting_the_batch_does_not_change_the_history`]：分批抽与一次抽完**逐字段相同**。
//!   这是"每抽独立派生骰子流"（`gacha::stream`）的全部意义 —— 如果骰子是一条长流，
//!   "用户点了 3 次单抽还是点了一次十连"就会改变历史，第三方重放立刻作废。
//! - `same_seed_replays_identically` 与 `a_different_seed_actually_changes_the_ledger`
//!   两条一起看才成立。只写前者，可能测的是"常量返回常量"这种假绿。
//!
//! 全部走 `tempfile` 里的库：这些测试会真写数据。

use petcore::gacha::{Catalog, Currency, GachaParams, PullState, Rarity};
use petstore::db::{AUDIT_HEAD_KEY, AUDIT_LEN_KEY, SEED_KEY};
use petstore::ledger::{PullRow, ledger_items_outside_catalog};
use petstore::{DbPath, Store};
use tempfile::TempDir;

const SEED: u64 = 7;
const BANNER: &str = "cat_series";
const PROFILE: &str = "local";

/// 返回 `(临时目录, 库)`；目录必须跟着库一起活着，否则 SQLite 文件先被删。
fn store(seed: u64) -> (TempDir, Store) {
    let dir = TempDir::new().expect("tempdir");
    let mut s = Store::open(&dir.path().join("opet.db")).expect("建库");
    s.reseed(seed).expect("播种");
    (dir, s)
}

fn catalog() -> Catalog {
    Catalog::default_v1().expect("默认目录")
}

/// 只比"会被算进摘要"的字段：`ts` 是墙钟，本来就不参与可重放。
/// 用结构体而不是十元组，失败时打印的是字段名而不是位置。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Replayable {
    seq: i64,
    rarity: String,
    item_id: String,
    pity_before: u32,
    batch_index: u32,
    dice_used: u32,
    duplicate: bool,
    shards_gained: u32,
    ssr_prob: u32,
    rng_state_hash: String,
    prev_digest: String,
}

impl From<&PullRow> for Replayable {
    fn from(r: &PullRow) -> Self {
        Self {
            seq: r.seq,
            rarity: r.rarity.as_str().to_string(),
            item_id: r.item_id.clone(),
            pity_before: r.pity_before,
            batch_index: r.batch_index,
            dice_used: r.dice_used,
            duplicate: r.duplicate,
            shards_gained: r.shards_gained,
            ssr_prob: r.ssr_prob,
            rng_state_hash: r.rng_state_hash.clone(),
            prev_digest: r.prev_digest.clone(),
        }
    }
}

fn replayable(rows: &[PullRow]) -> Vec<Replayable> {
    rows.iter().map(Replayable::from).collect()
}

#[test]
fn pull_lands_the_ledger_inventory_and_shards() {
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    let rep = s.pull(&p, &cat, BANNER, 10).expect("十连");

    assert_eq!(rep.first_seq, 1);
    assert_eq!(rep.last_seq, 10);
    assert_eq!(rep.outcomes.len(), 10);
    let rows = s.rows(PROFILE, BANNER).unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(
        rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
        (1..=10).collect::<Vec<_>>()
    );

    // 稀有度计数与 outcomes 一致
    let counts: [u32; 4] = rep.counts;
    assert_eq!(
        counts.iter().sum::<u32>(),
        10,
        "四档计数之和必须等于抽数: {counts:?}"
    );
    for r in &rows {
        let want = cat.find(&r.item_id).expect("账本里的 id 必须在目录里");
        assert_eq!(want.rarity, r.rarity, "{} 的稀有度对不上目录", r.item_id);
        assert_eq!(want.kind, r.item_kind);
    }
    assert!(ledger_items_outside_catalog(&rows, &cat).is_empty());

    // 库存：10 抽 = 10 个 count；重复数与碎片对得上
    let total: i64 = s
        .conn()
        .query_row("SELECT COALESCE(SUM(count),0) FROM inventory", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(total, 10, "每抽都该进库存");
    let dups: i64 = s
        .conn()
        .query_row(
            "SELECT COALESCE(SUM(duplicates),0) FROM inventory",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dups as u32, rep.duplicates, "库存的重复数与报告不符");
    let gained: u64 = rows.iter().map(|r| u64::from(r.shards_gained)).sum();
    assert_eq!(u64::from(rep.shards_gained), gained);
    assert_eq!(s.shards(Currency::Shards).unwrap(), gained);
    assert_eq!(rep.shards_after, gained, "报告里的余额该是写库之后的");

    // 链锚点与长度都盖进了 meta
    assert_eq!(
        s.meta_get(AUDIT_LEN_KEY).unwrap().as_deref(),
        Some("10"),
        "audit_len 该等于账本条数"
    );
    assert!(s.meta_get(AUDIT_HEAD_KEY).unwrap().is_some());
    assert_eq!(s.meta_get(SEED_KEY).unwrap().as_deref(), Some("7"));
    assert_eq!(s.seed(), SEED);

    // 十连地板：§3.6:593 说这一批里至少一件 SR 以上
    assert!(
        rows.iter()
            .any(|r| r.rarity == Rarity::SR || r.rarity == Rarity::SSR),
        "十连里一件 SR+ 都没有 ⇒ 地板失效"
    );
    // 抽完之后 pity 该从账本推出来
    let (state, next_seq) = s.pity_state(&p, PROFILE, BANNER).unwrap();
    assert_eq!(next_seq, 11);
    assert_eq!(state.batch_index, 0, "刚满一批，下一抽是新批的第 0 抽");
    assert_eq!(state.batch_best, None, "新批不许继承上一批的最好档");
    assert_eq!(state.since_ssr, 10, "这一批没出 SSR ⇒ 窗口累计 10");

    // 库里存的参数与代码里的参数是同一件事
    assert_eq!(s.banner_params(BANNER).unwrap(), p);
    assert_eq!(s.banner_currency(BANNER).unwrap(), Currency::CompanionTime);
    assert_eq!(s.verify(&cat, PROFILE, BANNER).unwrap().len, 10);
}

#[test]
fn same_seed_replays_identically() {
    let (_d1, mut a) = store(SEED);
    let (_d2, mut b) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    let ra = a.pull(&p, &cat, BANNER, 10).unwrap();
    let rb = b.pull(&p, &cat, BANNER, 10).unwrap();
    assert_eq!(ra.head_hex, rb.head_hex, "同 seed 的链尾该逐位相同");
    assert_eq!(
        replayable(&a.rows(PROFILE, BANNER).unwrap()),
        replayable(&b.rows(PROFILE, BANNER).unwrap())
    );
}

#[test]
fn a_different_seed_actually_changes_the_ledger() {
    // 上一条测试的反面：如果两个 seed 也给出同一份账本，那"可重放"测的是常量。
    let (_d1, mut a) = store(SEED);
    let (_d2, mut b) = store(SEED + 1);
    let cat = catalog();
    let p = GachaParams::V1;
    a.pull(&p, &cat, BANNER, 10).unwrap();
    b.pull(&p, &cat, BANNER, 10).unwrap();
    assert_ne!(
        replayable(&a.rows(PROFILE, BANNER).unwrap()),
        replayable(&b.rows(PROFILE, BANNER).unwrap()),
        "换了 seed 账本却一模一样 ⇒ 上面那条 same_seed 是假绿"
    );
}

#[test]
fn splitting_the_batch_does_not_change_the_history() {
    // 3 + 7 与一次 10：逐字段相同（含链摘要）。
    let (_d, mut once) = store(SEED);
    let (_d2, mut split) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    let r10 = once.pull(&p, &cat, BANNER, 10).unwrap();
    split.pull(&p, &cat, BANNER, 3).unwrap();
    let r7 = split.pull(&p, &cat, BANNER, 7).unwrap();
    assert_eq!(r7.first_seq, 4);
    assert_eq!(r7.last_seq, 10);
    assert_eq!(
        replayable(&once.rows(PROFILE, BANNER).unwrap()),
        replayable(&split.rows(PROFILE, BANNER).unwrap()),
        "分批抽改变了历史 ⇒ 骰子流不再与 (seed, seq) 一一对应，审计重放会作废"
    );
    assert_eq!(
        r10.head_hex,
        split.meta_get(AUDIT_HEAD_KEY).unwrap().unwrap()
    );
    assert_eq!(
        once.verify(&cat, PROFILE, BANNER).unwrap().len,
        split.verify(&cat, PROFILE, BANNER).unwrap().len
    );
}

#[test]
fn state_derived_from_the_ledger_equals_the_engines_own_state() {
    // "不建 pity 表"这个决定的持续担保：库外没人再存状态，推导必须永远等于引擎推进。
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    let mut st = PullState::fresh();
    for batch in [1u32, 10, 7, 4, 9] {
        let rep = s.pull(&p, &cat, BANNER, batch).unwrap();
        for o in &rep.outcomes {
            st = st.advanced(&p, o.rarity);
        }
        let (got, next) = s.pity_state(&p, PROFILE, BANNER).unwrap();
        assert_eq!(got, st, "第 {} 抽后状态漂了", rep.last_seq);
        assert_eq!(next, rep.last_seq + 1);
    }
    assert_eq!(s.verify(&cat, PROFILE, BANNER).unwrap().len, 31);
}

#[test]
fn published_params_are_compared_not_trusted() {
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 1).unwrap();

    let mut drifted = p;
    drifted.hard_pity = 90;
    let err = s
        .pull(&drifted, &cat, BANNER, 1)
        .expect_err("改了参数没重跑公示，本该拒绝");
    assert!(
        matches!(err, petstore::db::StoreError::PublishedParamsDiffer { .. }),
        "错误类型不对: {err}"
    );
    // 库里仍是公示的那份 ⇒ 重放用的是"当年公示的参数"，不是"现在的代码常量"
    assert_eq!(s.banner_params(BANNER).unwrap(), p);
    assert_eq!(s.banner_params(BANNER).unwrap().hard_pity, 80);
}

#[test]
fn one_ledger_one_banner_is_enforced() {
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 2).unwrap();
    let err = s
        .pull(&p, &cat, "dog_series", 1)
        .expect_err("第二张 banner 混进同一条链，本该拒绝");
    assert!(
        matches!(err, petstore::db::StoreError::SingleLedgerRequired { .. }),
        "错误类型不对: {err}"
    );
    assert_eq!(s.rows(PROFILE, "dog_series").unwrap().len(), 0);
}

#[test]
fn redeem_ssr_is_deterministic_and_outside_the_chain() {
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 1).unwrap();
    let len_before = s.meta_get(AUDIT_LEN_KEY).unwrap().unwrap();
    let head_before = s.meta_get(AUDIT_HEAD_KEY).unwrap().unwrap();

    // 碎片不够：先报错，什么都不写
    let err = s.redeem_ssr(&p, &cat, "hat_straw").unwrap_err();
    assert!(
        matches!(err, petstore::db::StoreError::NotEnoughShards { .. }),
        "该先报碎片不足: {err}"
    );
    // 非 SSR 不能自选
    let n_id = cat
        .items
        .iter()
        .find(|i| i.rarity == Rarity::N)
        .unwrap()
        .id
        .clone();
    assert!(s.redeem_ssr(&p, &cat, &n_id).is_err(), "{n_id} 不是 SSR");

    s.add_shards(Currency::Shards, 200).unwrap();
    let after = s.redeem_ssr(&p, &cat, "hat_straw").unwrap();
    assert_eq!(after, 200 - u64::from(p.ssr_redeem_cost));
    assert_eq!(s.item_count("hat_straw").unwrap(), 1);
    // 兑换不进链：长度与链尾都不许动
    assert_eq!(s.meta_get(AUDIT_LEN_KEY).unwrap().unwrap(), len_before);
    assert_eq!(s.meta_get(AUDIT_HEAD_KEY).unwrap().unwrap(), head_before);
    assert_eq!(s.verify(&cat, PROFILE, BANNER).unwrap().len, 1);
    let from: String = s
        .conn()
        .query_row(
            "SELECT obtained_from FROM inventory WHERE item_id = 'hat_straw'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(from, "redeem:shards", "兑换的来源要写清楚，不能混进 gacha");
}

#[test]
fn reseed_clears_history_so_the_chain_stays_valid() {
    // 换 seed 不清账本 ⇒ 重放必然对不上（旧记录里的哈希来自旧 seed）。
    // 这条测试钉住 `reseed` 的清理范围。
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 5).unwrap();
    s.reseed(99).unwrap();
    assert_eq!(s.rows(PROFILE, BANNER).unwrap().len(), 0);
    assert_eq!(s.item_count("cat_shiro").unwrap(), 0);
    assert!(s.meta_get(AUDIT_HEAD_KEY).unwrap().is_none());
    assert!(s.meta_get(AUDIT_LEN_KEY).unwrap().is_none());
    assert_eq!(s.shards(Currency::Shards).unwrap(), 0);
    assert_eq!(s.seed(), 99);
    let rep = s.pull(&p, &cat, BANNER, 3).unwrap();
    assert_eq!(rep.first_seq, 1, "清空后该从第 1 抽重新起链");
    assert_eq!(s.verify(&cat, PROFILE, BANNER).unwrap().len, 3);
}

#[test]
fn install_catalog_is_idempotent() {
    let (_d, mut s) = store(SEED);
    let cat = catalog();
    let a = s.install_catalog(&cat).unwrap();
    let b = s.install_catalog(&cat).unwrap();
    assert_eq!(a, b, "第二次 install 不该改动任何行数");
    let pets: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM pets", [], |r| r.get(0))
        .unwrap();
    let acc: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM accessories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pets, 10);
    assert_eq!(acc, 20);
}

#[test]
fn the_default_archive_refuses_writes_unless_explicitly_allowed() {
    // 计划"风险"表最后一条：元测试/探针绝不能落到用户真存档。
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opet.db");
    let db = DbPath {
        path: path.clone(),
        source: petstore::paths::PathSource::XdgDataHome,
    };
    let err = match Store::open_with(&db, false) {
        Ok(_) => panic!("默认解析出的路径居然被允许写入"),
        Err(e) => e,
    };
    assert!(
        matches!(err, petstore::db::StoreError::DefaultArchiveNotAllowed(_)),
        "错误类型不对: {err}"
    );
    assert!(!path.exists(), "被拒时连文件都不该创建");
    Store::open_with(&db, true).expect("显式放行后可以打开");

    // 相对路径：`opet pull` 会被 systemd user service 起，cwd 不可预测 ⇒ 明确拒绝
    let rel = DbPath {
        path: "opet.db".into(),
        source: petstore::paths::PathSource::CliArg,
    };
    let err2 = match Store::open_with(&rel, true) {
        Ok(_) => panic!("相对路径居然被接受"),
        Err(e) => e,
    };
    assert!(err2.to_string().contains("绝对路径"), "{err2}");
}
