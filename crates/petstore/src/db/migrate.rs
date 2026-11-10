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
pub const SCHEMA_VERSION: i64 = 2;

/// 建库 SQL（设计书 §3.4 的 12 表 + `profiles`）。
pub const SCHEMA_V1: &str = include_str!("schema.sql");

/// v1→v2：`accessories.slot` 的 CHECK 加第六值 `'feet'`（靴子 = 一份网格镜像成两件，
/// 挂在左右脚那对骨上；前五值里没有能诚实表达"脚"的值，`hand` 顶上去就是给以后留一笔假账）。
///
/// SQLite **不能** `ALTER TABLE ... DROP/ADD CHECK` ⇒ 只能整表重建再改名。这张表没有
/// 任何外键指向它、也没有索引/触发器挂在它上面（`grep -n 'accessories' schema.sql` 只有建表
/// 那一行和 `achievements.kind` 那个字符串枚举），所以重建是安全的：列清单逐列照抄 v1，
/// `INSERT … SELECT` 显式列出列名（不写 `SELECT *` ⇒ 万一以后 v1 表被加过列，这里会红而不是静默错位）。
const V1_TO_V2: &str = "
CREATE TABLE accessories_v2 (
  id TEXT PRIMARY KEY, name TEXT NOT NULL,
  slot TEXT NOT NULL CHECK (slot IN ('head','face','neck','back','hand','feet')),
  rarity TEXT NOT NULL CHECK (rarity IN ('N','R','SR','SSR')),
  anchor_offset TEXT,
  z_order INTEGER DEFAULT 10, palette TEXT, manifest_path TEXT NOT NULL
);
INSERT INTO accessories_v2 (id, name, slot, rarity, anchor_offset, z_order, palette, manifest_path)
  SELECT id, name, slot, rarity, anchor_offset, z_order, palette, manifest_path FROM accessories;
DROP TABLE accessories;
ALTER TABLE accessories_v2 RENAME TO accessories;
";

/// `from → to` 的增量迁移。`from` 必须严格等于它在链条里的位置，[`migrate`] 按序应用。
///
/// ⚠️ 加新版本的人必须在这里追加，而不是去改 `SCHEMA_V1`：那份 SQL 同时是**建库**用的，
/// 就地改它 = 新库带上 v2 的约束却仍盖章 v1，老库停在 v1 的约束也盖章 v1 ⇒ 同一个版本号
/// 替两种结构背书，而 SQLite 不会告诉你它对不上。
pub const MIGRATIONS: &[(i64, &str)] = &[(1, V1_TO_V2)];

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
        // 迁移链必须是 **1,2,…,SCHEMA_VERSION-1** 的连续 `from`，且每个都小于 SCHEMA_VERSION。
        // ⚠️ 原先这条写的是"从 0 连续排"，那是**空表时的一条假话**（`&[]` 让循环体一次都没跑，
        // 所以它从来没检查过任何东西）。而 `migrate()` 的循环是 `for from in start..SCHEMA_VERSION`，
        // 老库最低只能是 v1（v0 = 空库，走 `build_fresh` 而不是迁移）⇒ 键 0 永远不会被查到，
        // 真按那条写就会把第一条真实迁移判红。
        for (i, (from, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*from, (i + 1) as i64, "迁移链必须从 v1 起连续排");
            assert!(
                *from < SCHEMA_VERSION,
                "v{from}→v{} 超出了当前版本",
                SCHEMA_VERSION
            );
        }
        assert_eq!(
            MIGRATIONS.len(),
            (SCHEMA_VERSION - 1) as usize,
            "v1 到 v{SCHEMA_VERSION} 之间每一步都要有迁移，缺一步就是 NoPath 而不是升版"
        );
    }

    /// v1 的 `accessories` 表**原样**（五值 CHECK）。这是迁移测试要的"旧形状"，
    /// 不是第二份现行 schema —— 现行的唯一来源仍是 `schema.sql`。
    const V1_ACCESSORIES: &str = "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE accessories (
  id TEXT PRIMARY KEY, name TEXT NOT NULL,
  slot TEXT NOT NULL CHECK (slot IN ('head','face','neck','back','hand')),
  rarity TEXT NOT NULL CHECK (rarity IN ('N','R','SR','SSR')),
  anchor_offset TEXT,
  z_order INTEGER DEFAULT 10, palette TEXT, manifest_path TEXT NOT NULL
);";

    fn slot_error(conn: &rusqlite::Connection, slot: &str) -> Option<String> {
        conn.execute(
            "INSERT INTO accessories(id, name, slot, rarity, manifest_path) VALUES (?1,'n',?2,'N','m')",
            rusqlite::params![format!("probe_{slot}"), slot],
        )
        .err()
        .map(|e| e.to_string())
    }

    /// 升上来的库和新建的库，对同一个 slot 必须给**同一个答案**。
    /// 这条比"迁移后 CHECK 长什么样"有用：读的是行为，不依赖 sqlite_master 的文本格式。
    #[test]
    fn migrated_and_fresh_databases_answer_the_slot_check_identically() {
        let built = conn();
        assert_eq!(migrate(&built).unwrap(), SCHEMA_VERSION);

        let up = conn();
        up.execute_batch(V1_ACCESSORIES).unwrap();
        up.execute(
            "INSERT INTO meta(key, value) VALUES (?1, '1')",
            rusqlite::params![KEY_VERSION],
        )
        .unwrap();
        up.execute(
            "INSERT INTO accessories(id, name, slot, rarity, manifest_path) VALUES ('bell','帽','head','R','m')",
            [],
        )
        .unwrap();
        assert_eq!(current_version(&up).unwrap(), Some(1));
        assert_eq!(migrate(&up).unwrap(), SCHEMA_VERSION);

        assert_eq!(
            up.query_row("SELECT slot FROM accessories WHERE id = 'bell'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "head",
            "整表重建必须把老行原样搬过去"
        );

        // ⚠️ 每个 slot 在每份库里**只插一次**：`slot_error` 用的是确定性的 id（`probe_<slot>`），
        // 同一个 slot 问两遍会先撞上 PRIMARY KEY 而返回一个和 CHECK 无关的错（本轮实测踩过，
        // 那条假失败长得就像"升版没生效"）。
        let answers: Vec<(&str, Option<String>, Option<String>)> =
            ["head", "face", "neck", "back", "hand", "feet", "wing"]
                .into_iter()
                .map(|slot| (slot, slot_error(&up, slot), slot_error(&built, slot)))
                .collect();
        let answer = |slot: &str| {
            answers
                .iter()
                .find(|(s, _, _)| *s == slot)
                .unwrap_or_else(|| panic!("没有问过 {slot}"))
        };

        for (slot, up_a, built_a) in &answers {
            assert_eq!(
                up_a.is_none(),
                built_a.is_none(),
                "slot {slot}：升上来的库和新建的库答案不一样"
            );
        }
        for (which, err) in [
            ("升上来的库", answer("feet").1.as_ref()),
            ("新建的库", answer("feet").2.as_ref()),
        ] {
            assert!(
                err.is_none(),
                "v2 之后 {which} 里 'feet' 必须写得进去（升版失败的最坏形状就是它悄悄还是五值）：{:?}",
                err
            );
        }
        for (which, err) in [
            ("升上来的库", answer("wing").1.as_ref()),
            ("新建的库", answer("wing").2.as_ref()),
        ] {
            let msg =
                err.unwrap_or_else(|| panic!("{which}：'wing' 不在六值里，必须被 CHECK 拒掉"));
            assert!(
                msg.contains("CHECK"),
                "{which}：拒绝必须来自 CHECK 而不是别的错（元测试要证明这道门真的在）：{msg}"
            );
        }
        assert_eq!(current_version(&up).unwrap(), Some(SCHEMA_VERSION));
    }
}
