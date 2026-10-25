//! 建库与升版。**只有顺序升版一条路**：`meta.schema_version` 落后就按号补齐，超前就拒开。
//!
//! 为什么把 SQL 用 `include_str!` 嵌进二进制，而不是运行时读文件：
//! `opet pull` 必须能在任意 cwd（包括 AUR 装完后从 systemd user service 起）跑，
//! 读相对路径的 SQL 等于把"装没装对"变成运行时故障。
//!
//! ⚠️ 改 `schema.sql` 的任何一列都要：① 升 [`SCHEMA_VERSION`]；② 在 [`MIGRATIONS`] 里
//! 追加一条"从 v-1 升到 v"的 SQL；③ 想清楚旧库里的行怎么填新列。就地改 v1 的 SQL
//! 会让已经存在的用户库和代码对不上，而 SQLite **不会**告诉你它对不上。

/// 当前代码期待的 schema 版本。
pub const SCHEMA_VERSION: i64 = 1;

/// 建库 SQL（设计书 §3.4 的 12 表 + `profiles`）。
pub const SCHEMA_V1: &str = include_str!("schema.sql");

/// `from → to` 的增量迁移。`from` 必须严格等于它在链条里的位置，[`migrate`] 按序应用。
///
/// 现在只有一条链的头，所以这张表是空的——但**空表本身就是文档**：
/// 加第二个版本的人必须在这里追加，而不是去改 `SCHEMA_V1`。
pub const MIGRATIONS: &[(i64, &str)] = &[];

pub const KEY_VERSION: &str = "schema_version";

#[derive(Debug, PartialEq, Eq)]
pub enum MigrateError {
    /// `meta.schema_version` 不是合法整数 ⇒ 库被人手改过或文件截断。
    UnreadableVersion(String),
    /// 库版本比代码新（用户降级了程序，或另一个版本的 opet 写过这个库）。
    TooNew { found: i64, expected: i64 },
    /// 版本差落在没有迁移路径的位置（比如跳过了一号）。
    NoPath { from: i64, to: i64 },
    /// 表建了但 `meta.schema_version` 那行不在 ⇒ 库是半写的（建表后、盖章前崩过）。
    /// **不猜**它是 v1：结构看着像，但没人证明过，盖章等于把一个未知状态说成已知状态。
    Unversioned,
    /// SQL 本身失败，带上下文。
    Sqlite(String),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnreadableVersion(raw) => write!(f, "meta.schema_version = {raw:?} 不是整数"),
            Self::TooNew { found, expected } => write!(
                f,
                "库版本 v{found} 比这份程序的 v{expected} 新 ⇒ 拒绝打开（可能是降级安装）"
            ),
            Self::NoPath { from, to } => {
                write!(
                    f,
                    "没有从 v{from} 升到 v{to} 的迁移路径（MIGRATIONS 缺一条）"
                )
            }
            Self::Unversioned => write!(
                f,
                "库里建了 meta 表却没有 {KEY_VERSION} 行 ⇒ 这份存档是半写的，拒绝猜测它的版本"
            ),
            Self::Sqlite(e) => write!(f, "SQLite: {e}"),
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<rusqlite::Error> for MigrateError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e.to_string())
    }
}

/// 库现在的版本；`meta` 表不存在（空文件）时返回 `None`。
pub fn current_version(conn: &rusqlite::Connection) -> Result<Option<i64>, MigrateError> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| MigrateError::Sqlite(e.to_string()))?;
    if exists == 0 {
        return Ok(None);
    }
    match conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        rusqlite::params![KEY_VERSION],
        |r| r.get::<_, String>(0),
    ) {
        Ok(raw) => {
            let v = raw
                .trim()
                .parse::<i64>()
                .map_err(|_| MigrateError::UnreadableVersion(raw))?;
            Ok(Some(v))
        }
        // 表在但行不在：文件是半写的（建表成功、写版本号前崩溃）
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Some(0)),
        Err(e) => Err(MigrateError::Sqlite(e.to_string())),
    }
}

/// 在**一个事务**里跑一段 SQL 并把版本号盖章成 `to`；任何一步失败就回滚，
/// 库停在原样。SQLite 支持事务性 DDL，所以"建了一半"这种状态根本不该存在。
///
/// 建库和升版共用这一条：两条路都必须满足同一个不变量 ——
/// **要么表齐了且版本号对，要么什么都没发生**。否则下次打开读到的版本号
/// 就在替一个未知状态背书。
fn apply_in_tx(
    conn: &rusqlite::Connection,
    sql: &str,
    ctx: &str,
    to: i64,
) -> Result<(), MigrateError> {
    conn.execute_batch("BEGIN")?;
    let done = conn
        .execute_batch(sql)
        .and_then(|_| {
            conn.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                rusqlite::params![KEY_VERSION, to.to_string()],
            )
        })
        .and_then(|_| conn.execute_batch("COMMIT"));
    if let Err(e) = done {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(MigrateError::Sqlite(format!("{ctx}: {e}")));
    }
    Ok(())
}

/// 空库 ⇒ 按 `sql` 建全套表并盖章到 [`SCHEMA_VERSION`]。
///
/// `schema.sql` 首行的 `PRAGMA foreign_keys` 在事务里是 no-op，但这里只有 DDL、
/// 不插行，且连接级 pragma 已由 `Store::open` 在 `migrate` 之前设好。
fn build_fresh(conn: &rusqlite::Connection, sql: &str) -> Result<(), MigrateError> {
    apply_in_tx(conn, sql, "建表失败", SCHEMA_VERSION)
}

/// 建库或升版，结束后库一定处于 [`SCHEMA_VERSION`]。
pub fn migrate(conn: &rusqlite::Connection) -> Result<i64, MigrateError> {
    let found = current_version(conn)?;
    let start = match found {
        None => {
            build_fresh(conn, SCHEMA_V1)?;
            return Ok(SCHEMA_VERSION);
        }
        Some(v) => v,
    };

    if start == 0 {
        return Err(MigrateError::Unversioned);
    }
    if start > SCHEMA_VERSION {
        return Err(MigrateError::TooNew {
            found: start,
            expected: SCHEMA_VERSION,
        });
    }
    if start == SCHEMA_VERSION {
        return Ok(start);
    }
    for from in start..SCHEMA_VERSION {
        let sql = MIGRATIONS
            .iter()
            .find(|(f, _)| *f == from)
            .map(|(_, sql)| *sql)
            .ok_or(MigrateError::NoPath {
                from,
                to: SCHEMA_VERSION,
            })?;
        // 失败就停在旧版本号上，下次重试，不会留半升的库。
        apply_in_tx(conn, sql, &format!("v{from}→v{}", from + 1), from + 1)?;
    }
    Ok(SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> rusqlite::Connection {
        rusqlite::Connection::open_in_memory().unwrap()
    }

    #[test]
    fn fresh_database_lands_on_the_current_version() {
        let c = conn();
        assert_eq!(current_version(&c).unwrap(), None, "空库不该有 meta 表");
        assert_eq!(migrate(&c).unwrap(), SCHEMA_VERSION);
        assert_eq!(current_version(&c).unwrap(), Some(SCHEMA_VERSION));
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let c = conn();
        migrate(&c).unwrap();
        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM gacha_pull", [], |r| r.get(0))
            .unwrap();
        assert_eq!(migrate(&c).unwrap(), SCHEMA_VERSION);
        let after: i64 = c
            .query_row("SELECT COUNT(*) FROM gacha_pull", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, after, "第二次 migrate 不该重建任何东西");
    }

    #[test]
    fn refusing_a_newer_database_is_explicit() {
        let c = conn();
        migrate(&c).unwrap();
        c.execute(
            "UPDATE meta SET value = '99' WHERE key = ?1",
            rusqlite::params![KEY_VERSION],
        )
        .unwrap();
        assert_eq!(
            migrate(&c),
            Err(MigrateError::TooNew {
                found: 99,
                expected: SCHEMA_VERSION
            })
        );
    }

    #[test]
    fn a_non_numeric_version_is_not_silently_treated_as_zero() {
        let c = conn();
        migrate(&c).unwrap();
        c.execute(
            "UPDATE meta SET value = 'one' WHERE key = ?1",
            rusqlite::params![KEY_VERSION],
        )
        .unwrap();
        assert!(matches!(
            migrate(&c),
            Err(MigrateError::UnreadableVersion(_))
        ));
    }

    #[test]
    fn a_table_without_a_version_row_is_refused_not_repaired() {
        // 建表成功、写 meta 前崩 ⇒ 版本号缺失。这种库**拒绝打开**：
        // 结构"看着像 v1"不等于它就是 v1，替用户猜一个版本号就是把未知状态说成已知状态。
        let c = conn();
        c.execute_batch(SCHEMA_V1).unwrap();
        c.execute(
            "DELETE FROM meta WHERE key = ?1",
            rusqlite::params![KEY_VERSION],
        )
        .unwrap();
        assert_eq!(
            current_version(&c).unwrap(),
            Some(0),
            "没有版本行的库必须读成 0，不能读成 None（None 的含义是'一个表都没有'）"
        );
        assert_eq!(migrate(&c), Err(MigrateError::Unversioned));
    }

    #[test]
    fn a_failed_create_leaves_no_half_built_database() {
        // 事务化建库的证据：把建库 SQL 换成"前面能建、后面必崩"的一条，
        // 崩完之后库里必须一张表都不留 ⇒ 下次 migrate 走"空库重建"，而不是读到 Unversioned。
        let c = conn();
        let boom = "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                    CREATE TABLE gacha_pull (seq INTEGER PRIMARY KEY);
                    CREATE TABLE dup (x INTEGER);
                    CREATE TABLE dup (x INTEGER);";
        assert!(build_fresh(&c, boom).is_err());
        assert_eq!(
            current_version(&c).unwrap(),
            None,
            "建库失败后必须回到'空库'，不能留下半套表"
        );
        assert_eq!(migrate(&c).unwrap(), SCHEMA_VERSION, "空库可以重建");
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM gacha_pull", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "重建出来的库是干净的"
        );
    }

    #[test]
    fn migration_table_is_ordered_and_in_range() {
        // MIGRATIONS 必须是 (0,1),(1,2)... 的连续链，且不超过 SCHEMA_VERSION。
        for (i, (from, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*from as usize, i, "迁移链必须从 0 连续排");
            assert!(*from < SCHEMA_VERSION);
        }
    }
}
