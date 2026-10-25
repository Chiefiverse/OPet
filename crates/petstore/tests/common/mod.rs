//! 红线测试的共用零件。放在这里而不是各测试文件抄一遍，是因为
//! "摘掉 CHECK" 那段字符串必须和 `schema.sql` 逐字符一致 —— 抄两份就会有第二份悄悄失效。

#![allow(dead_code)] // 每个 test 文件只用到其中一部分

use petstore::db::migrate::SCHEMA_V1;
use rusqlite::Connection;

/// `gacha_banner.currency` 的那道 CHECK，逐字符取自 `schema.sql`（§3.4:456-457）。
pub const CURRENCY_CHECK: &str = "CHECK (currency IN ('companion_time','shards'))";

/// 一眼就能看出"这是钱"的写法。白名单外就必须写不进去。
pub const PAID_NAMES: [&str; 9] = [
    "real_money",
    "money",
    "cash",
    "usd",
    "cny",
    "credit",
    "iap",
    "steam",
    "itch",
];

/// 一份**摘掉了**货币 CHECK 的建库 SQL（第 5 层元测试用）。
///
/// 找不到目标就 panic：那说明有人改了 CHECK 的写法，元测试必须当场失效，
/// 而不是退化成"摘了个不存在的东西"从而永远通过。
pub fn ddl_without_currency_check() -> String {
    assert!(
        SCHEMA_V1.contains(CURRENCY_CHECK),
        "schema.sql 里找不到 {CURRENCY_CHECK} ⇒ CHECK 被改写或删除了，元测试失去意义，先读 §3.11.4"
    );
    SCHEMA_V1.replace(CURRENCY_CHECK, "/* 元测试临时摘掉 */")
}

/// 建好库的连接（跑了迁移，所以与 `Store::open` 看到的是同一份 DDL）。
pub fn migrated_conn() -> Connection {
    let c = Connection::open_in_memory().expect("内存库");
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    petstore::db::migrate::migrate(&c).expect("建库");
    c
}

/// 库里 `gacha_banner` 的建表 SQL 原文（SQLite 会存下来，所以这是**事实**，不是代码注释）。
pub fn stored_banner_ddl(conn: &Connection) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='gacha_banner'",
        [],
        |r| r.get::<_, String>(0),
    )
    .expect("gacha_banner 表应在库里")
}
