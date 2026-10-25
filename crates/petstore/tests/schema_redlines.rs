//! 红线 1（"随机 ≠ 付费"）在 **schema 层**的落点。
//!
//! 这里断言的是"插 `real_money` 会失败"。但**光这条不够**：失败可能来自 SQLite 的别的原因，
//! 那样第 1 层就是假绿。所以配套的元测试（摘掉 CHECK 后同一批写入必须成功）放在
//! `paid_randomness.rs` 第 5 层，两边共用 `tests/common` 里的同一段 CHECK 文本 ——
//! 复制一份就会有第二份悄悄失效。

mod common;

use common::PAID_NAMES;
use rusqlite::Connection;

fn fresh() -> Connection {
    common::migrated_conn()
}

fn is_constraint(msg: &str) -> bool {
    msg.contains("CHECK") || msg.contains("constraint failed") || msg.contains("UNIQUE")
}

/// §3.4 的表一张都不能少（少一张 = 迁移没跑对，而 SQLite 不会主动告诉你）。
#[test]
fn every_table_from_design_3_4_exists() {
    let c = fresh();
    let want: Vec<&str> = vec![
        "meta",
        "pets",
        "accessories",
        "inventory",
        "shards",
        "equipped",
        "content_packs",
        "entitlements",
        "license_state",
        "gacha_banner",
        "gacha_pull",
        "settings",
        "session_log",
        "profiles", // 偏离 1
    ];
    let have = tables(&c);
    for w in &want {
        assert!(have.iter().any(|t| t == w), "库里缺表 {w}；实际有 {have:?}");
    }
    assert_eq!(
        have.len(),
        want.len(),
        "表数与设计书不符：期望 {want:?}，实际 {have:?}"
    );
}

/// **红线第 1 层**：真钱货币写不进 `gacha_banner`，插与改都不行。
#[test]
fn paid_currency_is_rejected_by_the_schema() {
    let c = fresh();
    for bad in PAID_NAMES {
        // 每次用不同的 id：否则"主键冲突"会冒充"CHECK 拦住了"，第 5 层元测试也就失去意义。
        let err = c
            .execute(
                "INSERT INTO gacha_banner(id, base_rate, soft_pity, hard_pity, currency)
                 VALUES (?2, '{}', 40, 80, ?1)",
                rusqlite::params![bad, format!("bad_{bad}")],
            )
            .expect_err(&format!("插 {bad} 居然成功了 ⇒ CHECK 没在起作用"));
        assert!(
            err.to_string().contains("CHECK"),
            "插 {bad} 失败了，但挡它的不是 CHECK: {err}"
        );
    }
    // 两个合法值必须真能写：否则上面那 9 次"失败"可能只是整列都写不进去。
    for (ok, id) in [("companion_time", "b_time"), ("shards", "b_shards")] {
        c.execute(
            "INSERT INTO gacha_banner(id, base_rate, soft_pity, hard_pity, currency)
             VALUES (?1, '{}', 40, 80, ?2)",
            rusqlite::params![id, ok],
        )
        .unwrap_or_else(|e| panic!("合法货币 {ok} 本该写进去: {e}"));
    }
    // UPDATE 也挡得住（CHECK 不只管插入）。
    let upd = c.execute(
        "UPDATE gacha_banner SET currency = 'real_money' WHERE id = 'b_time'",
        [],
    );
    assert!(
        upd.is_err(),
        "改值绕过了 CHECK ⇒ 第 1 层只在 INSERT 上成立，不够"
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM gacha_banner WHERE currency NOT IN ('companion_time','shards')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0,
        "库里出现了第三种货币"
    );
}

/// 其余 CHECK 白名单也得真的在挡。每条只违反**一个**列，其余列都给全 ——
/// 不然失败可能来自 NOT NULL 或主键，断言就成了空过。
#[test]
fn the_other_enum_checks_hold_too() {
    let c = fresh();
    // 先把父行备好：否则"失败"可能来自 FK 而不是 CHECK，断言就成了空过。
    c.execute(
        "INSERT INTO profiles(id, created_at) VALUES ('local', 0)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO gacha_banner(id, base_rate, soft_pity, hard_pity) VALUES ('b','{}',40,80)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO content_packs(id, title, kind, pack_path, store)
         VALUES ('p', 'P', 'pets', 'x.petpack', 'itch')",
        [],
    )
    .unwrap();
    let bad: [(&str, &str); 8] = [
        (
            "inventory.kind",
            "INSERT INTO inventory(kind, item_id) VALUES ('loot', 'x')",
        ),
        (
            "gacha_pull.rarity",
            "INSERT INTO gacha_pull(profile_id, banner_id, ts, rarity, item_kind, item_id,
                     pity_counter, batch_index, dice_used, is_duplicate, shards_gained,
                     ssr_prob, rng_state_hash, audit_prev_hash)
             VALUES ('local', 'b', 0, 'UR', 'pet', 'x', 0, 0, 2, 0, 0, 20000, '00', '00')",
        ),
        (
            "gacha_pull.item_kind",
            "INSERT INTO gacha_pull(profile_id, banner_id, ts, rarity, item_kind, item_id,
                     pity_counter, batch_index, dice_used, is_duplicate, shards_gained,
                     ssr_prob, rng_state_hash, audit_prev_hash)
             VALUES ('local', 'b', 0, 'N', 'currency', 'x', 0, 0, 2, 0, 0, 20000, '00', '00')",
        ),
        (
            "accessories.slot",
            "INSERT INTO accessories(id, name, slot, rarity, manifest_path)
             VALUES ('a', 'A', 'wing', 'N', 'm.json')",
        ),
        (
            "content_packs.kind",
            "INSERT INTO content_packs(id, title, kind, pack_path, store)
             VALUES ('p2', 'P2', 'lootbox', 'y.petpack', 'itch')",
        ),
        (
            "entitlements.status",
            "INSERT INTO entitlements(pack_id, source, status) VALUES ('p', 'free', 'pending')",
        ),
        (
            "pets.rarity",
            "INSERT INTO pets(id, name, rarity, atlas_path, manifest_path)
             VALUES ('q', 'Q', 'Legendary', 'a.webp', 'm.json')",
        ),
        (
            "gacha_banner.ten_pull_sr_floor",
            "INSERT INTO gacha_banner(id, base_rate, soft_pity, hard_pity, ten_pull_sr_floor)
             VALUES ('b2', '{}', 40, 80, 2)",
        ),
    ];
    for (column, sql) in bad {
        let e = c
            .execute(sql, [])
            .expect_err(&format!("{column} 的白名单外值居然写进去了"));
        assert!(
            e.to_string().contains("CHECK"),
            "{column}: 失败了，但不是 CHECK 挡的（可能是 FK/NOT NULL 挡住了，那这条测试是空过）: {e}"
        );
    }
}

/// 付费包**结构上**不能有概率字段（§3.11.4 第 2 条）。这里查的是列，不是代码。
#[test]
fn content_packs_has_no_probability_column() {
    let c = fresh();
    let cols = column_names(&c, "content_packs");
    for forbidden in [
        "weight",
        "rarity",
        "drop_rate",
        "pool",
        "rate",
        "probability",
    ] {
        assert!(
            !cols.iter().any(|x| x == forbidden),
            "content_packs 出现了概率字段 {forbidden} ⇒ 明码标价可以买随机了"
        );
    }
    assert!(cols.iter().any(|x| x == "price_cents"));
    assert!(cols.iter().any(|x| x == "store"));
}

/// 偏离 2 的落点：`canonical()` 摘要吃进的那些字段，库里必须全都读得回来。
/// 少一列，`audit verify` 就从"校验"退化成"相信代码没被改过"。
#[test]
fn gacha_pull_carries_every_column_the_digest_needs() {
    let c = fresh();
    let cols = column_names(&c, "gacha_pull");
    for need in [
        "seq",
        "profile_id",
        "banner_id",
        "ts",
        "rarity",
        "item_kind",
        "item_id",
        "pity_counter",
        "batch_index",
        "dice_used",
        "is_duplicate",
        "shards_gained",
        "ssr_prob",
        "rng_state_hash",
        "audit_prev_hash",
    ] {
        assert!(
            cols.iter().any(|x| x == need),
            "gacha_pull 缺列 {need}；实有 {cols:?}"
        );
    }
}

/// 偏离 3 的落点：决定**综合概率**的 8 个量 + 货币，全都得能在库里读回来。
#[test]
fn gacha_banner_carries_every_number_that_touches_the_dice() {
    let c = fresh();
    let cols = column_names(&c, "gacha_banner");
    for need in [
        "base_rate",
        "soft_pity",
        "hard_pity",
        "soft_step",
        "batch_size",
        "ten_pull_sr_floor",
        "dup_shards",
        "ssr_redeem_cost",
        "currency",
    ] {
        assert!(
            cols.iter().any(|x| x == need),
            "gacha_banner 缺列 {need} ⇒ 公示少了一个数；实有 {cols:?}"
        );
    }
}

/// 账本必须挂在档案与 banner 上（`profile_id` 是偏离 1 加的，FK 真的生效才算约束住）。
#[test]
fn ledger_rows_cannot_float_free_of_a_profile_or_banner() {
    let c = fresh();
    c.execute(
        "INSERT INTO gacha_banner(id, base_rate, soft_pity, hard_pity) VALUES ('b','{}',40,80)",
        [],
    )
    .unwrap();
    let insert = |profile: &str, banner: &str| {
        c.execute(
            "INSERT INTO gacha_pull(profile_id, banner_id, ts, rarity, item_kind, item_id,
                     pity_counter, batch_index, dice_used, is_duplicate, shards_gained,
                     ssr_prob, rng_state_hash, audit_prev_hash)
             VALUES (?1, ?2, 0, 'N', 'pet', 'x', 0, 0, 2, 0, 0, 20000, '00', '00')",
            rusqlite::params![profile, banner],
        )
    };
    let e = insert("ghost", "b").expect_err("引用不存在的 profile 本该被 FK 拦下");
    assert!(
        e.to_string().contains("FOREIGN KEY") || is_constraint(&e.to_string()),
        "{e}"
    );
    insert("local", "nope").expect_err("引用不存在的 banner 本该被 FK 拦下");
    c.execute(
        "INSERT INTO profiles(id, created_at) VALUES ('local', 0)",
        [],
    )
    .unwrap();
    insert("local", "b").expect("profile 与 banner 都齐全时该写进去");
}

fn tables(c: &Connection) -> Vec<String> {
    let mut stmt = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn column_names(c: &Connection, table: &str) -> Vec<String> {
    // 表名只来自本文件的字面量，不接受外部输入
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}
