//! `assert_no_paid_randomness` —— 硬约束 #1（"随机 ≠ 付费"）的 CI 入口。
//!
//! 五条独立的路径，任何一条松了都必须有测试变红：
//!
//! | 层 | 落点 | 挡住什么 |
//! |---|---|---|
//! | 1 | `gacha_banner.currency` 的 DDL `CHECK` | 写进库的真钱货币 |
//! | 2 | `petcore::gacha::Currency` 只有两个变体 | 代码里根本表达不出"用钱抽" |
//! | 3 | `ContentPack` 无概率字段 + `deny_unknown_fields` | 付费包夹带权重 |
//! | 4 | 写路径（`Store::pull` / `ensure_banner`）不接受货币参数 | CLI/调用方绕过 1、2 |
//! | 5 | **元测试**：摘掉 CHECK 后坏写入必须成功 | 第 1 层其实是假绿 |
//!
//! 第 5 层是这一族测试里最重要的一条：它证明"绿"是 CHECK 给的，不是 SQLite 顺手给的。
//! （`schema_redlines.rs` 里那条 `meta_test_...` 与本文件共用 `tests/common` 的同一段摘取逻辑。）
//!
//! 设计依据：设计书 §2.7、§3.11.4。

mod common;

use common::{
    CURRENCY_CHECK, PAID_NAMES, ddl_without_currency_check, migrated_conn, stored_banner_ddl,
};
use petcore::gacha::{Catalog, Currency, GachaParams};
use petstore::db::Store;
use petstore::licenses::ContentPack;
use tempfile::TempDir;

/// 把库里存的建表 SQL 里的货币白名单抠出来（'xx' 的引号内内容）。
fn whitelisted_currencies(ddl: &str) -> Vec<String> {
    let tail = ddl
        .split("currency TEXT")
        .nth(1)
        .unwrap_or_else(|| panic!("建表 SQL 里找不到 currency 列: {ddl}"));
    let start = tail
        .find("IN (")
        .unwrap_or_else(|| panic!("currency 列没有 IN (...) 白名单: {tail}"));
    let body = &tail[start + 4..];
    let end = body
        .find(')')
        .unwrap_or_else(|| panic!("白名单没有收尾: {body}"));
    body[..end]
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_string())
        .collect()
}

/// **第 2 层 + 与第 1 层对齐**：类型枚举与 schema 白名单必须逐字符相同。
///
/// 两边不一致就是"公示一套、代码一套"的另一条路：库里允许 `shards`，
/// 而代码只认 `companion_time`（或反过来），CLI 就会把合法值报成非法。
#[test]
fn layer2_the_currency_type_matches_the_schema_whitelist() {
    let c = migrated_conn();
    let ddl = stored_banner_ddl(&c);
    assert!(
        ddl.contains(CURRENCY_CHECK),
        "库里的 gacha_banner 不带这道 CHECK ⇒ 第 1 层不存在: {ddl}"
    );
    let from_ddl = whitelisted_currencies(&ddl);
    let from_type: Vec<String> = Currency::ALL
        .iter()
        .map(|x| x.as_str().to_string())
        .collect();
    assert_eq!(
        from_ddl, from_type,
        "schema 白名单与 Currency 枚举漂移了 ⇒ 公示与实际能写的值不一致"
    );
    assert_eq!(Currency::ALL.len(), 2, "货币种类该只有陪伴时长与碎片");

    // 类型层面写不出"钱"：paid 的名字一个都反查不到变体
    for bad in PAID_NAMES {
        assert!(
            Currency::parse(bad).is_none(),
            "Currency::parse({bad}) 居然给了个变体 ⇒ 第 2 层失效"
        );
        for ok in Currency::ALL {
            assert_ne!(ok.as_str(), bad, "{bad} 不该等于任何合法货币名");
        }
    }
}

/// **第 3 层**：付费内容包的结构里就没有概率字段。
#[test]
fn layer3_a_paid_pack_cannot_carry_weights() {
    let base = r#"{"format":1,"id":"p","title":"P","kind":"pets","price_cents":990,
        "currency_code":"USD","store":"itch","pack_path":"packs/p.petpack","is_dlc":true,
        "items":["cat_shiro""#;
    let tail = "]}";
    for forbidden in [
        r#","weight":1"#,
        r#","rarity":"SSR""#,
        r#","drop_rate":0.5"#,
        r#","pool":"ssr_only""#,
        r#","rates":{"SSR":1}"#,
    ] {
        let json = format!("{base}{forbidden}{tail}");
        assert!(
            ContentPack::from_json(&json).is_err(),
            "带 {forbidden} 的包 JSON 本该在解析期失败（不是'解析后忽略'）"
        );
    }
    // 去掉多余字段后必须能解析，否则上面那五次"失败"可能是因为整个 JSON 都坏了
    let clean = ContentPack::from_json(&format!("{base}{tail}")).expect("正常包该能解析");
    clean.validate().expect("正常包该能过校验");

    // 明码标价与"免费"不能同时成立；付费包不许伪装成 free 来绕开价格审查
    let mut liar = clean.clone();
    liar.store = "free".into();
    assert!(
        liar.validate().unwrap_err().contains("free"),
        "store=free 还带价格，本该被拒"
    );
}

/// **第 1 层（Store 视角）**：走真实写路径之后，库里存的仍是软通货。
#[test]
fn layer1_a_paid_currency_never_reaches_the_database() {
    let dir = TempDir::new().unwrap();
    let mut s = Store::open(&dir.path().join("opet.db")).unwrap();
    s.reseed(3).unwrap();
    let cat = Catalog::default_v1().unwrap();
    s.pull(&GachaParams::V1, &cat, "cat_series", 1).unwrap();

    // 第 4 层顺带成立：`pull`/`ensure_banner` 的签名里没有"货币"这个参数，
    // 调用方想传真钱都没有地方传 ⇒ 唯一可能的值是软通货。
    assert_eq!(
        s.banner_currency("cat_series").unwrap(),
        Currency::CompanionTime
    );
    for bad in PAID_NAMES {
        let e = s
            .conn()
            .execute(
                "UPDATE gacha_banner SET currency = ?1 WHERE id = 'cat_series'",
                rusqlite::params![bad],
            )
            .expect_err(&format!("改成 {bad} 本该被 CHECK 拒绝"));
        assert!(e.to_string().contains("CHECK"), "{bad}: {e}");
    }
    // 碎片余额也只能挂在这两种货币下
    let mut stmt = s.conn().prepare("SELECT currency FROM shards").unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for got in &rows {
        assert!(
            Currency::parse(got).is_some(),
            "shards 表里出现了非软通货的货币 {got:?}"
        );
    }
}

/// **第 5 层（元测试）**：摘掉 CHECK，同一批坏写入必须成功。
///
/// 这一条是"红线测试必须能证明它会红"的落点。
#[test]
fn layer5_meta_test_the_check_is_what_blocks_it() {
    let real = migrated_conn();
    assert!(
        stored_banner_ddl(&real).contains(CURRENCY_CHECK),
        "真库带 CHECK ⇒ 才能拿它和摘掉的对照"
    );
    let stripped = ddl_without_currency_check();
    let bare = rusqlite::Connection::open_in_memory().unwrap();
    bare.execute_batch(&stripped)
        .expect("摘掉 CHECK 后建库应成功");
    assert!(
        !stored_banner_ddl(&bare).contains(CURRENCY_CHECK),
        "摘取没生效 ⇒ 下面那次'成功'不能证明任何事"
    );
    for bad in PAID_NAMES {
        bare.execute(
            "INSERT INTO gacha_banner(id, base_rate, soft_pity, hard_pity, currency)
             VALUES (?2, '{}', 40, 80, ?1)",
            rusqlite::params![bad, format!("m_{bad}")],
        )
        .unwrap_or_else(|e| panic!("没有 CHECK 时插 {bad} 本该成功，却失败了 ⇒ 另有原因: {e}"));
    }
    let paid: i64 = bare
        .query_row(
            "SELECT COUNT(*) FROM gacha_banner WHERE currency NOT IN ('companion_time','shards')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(paid, PAID_NAMES.len() as i64, "摘掉 CHECK 后这些行都该进去");
}

/// CI 入口：一次跑完五层，输出里能直接 grep 到这个名字（硬约束 #1 点名的测试）。
#[test]
fn assert_no_paid_randomness() {
    layer2_the_currency_type_matches_the_schema_whitelist();
    layer3_a_paid_pack_cannot_carry_weights();
    layer1_a_paid_currency_never_reaches_the_database();
    layer5_meta_test_the_check_is_what_blocks_it();
    // 第 4 层的 CLI 面（`opet pull --currency real_money` ⇒ SCHEMA_ERROR）在
    // `crates/opet-cli` 的USAGE/参数解析里，由 step 7 的手测 + §10.2 口径核对；
    // 本文件能钉住的是"库与类型两侧都写不出真钱"。
}
