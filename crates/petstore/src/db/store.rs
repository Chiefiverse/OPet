//! [`Store`]：`opet pull` / `audit` / `catalog` 的落库入口。
//!
//! 一次抽取的所有状态变化都在**一个事务**里（`BEGIN … COMMIT`）：账本行、库存、碎片、
//! 链尾锚点四样必须同时成立，否则下一次抽取的 pity 推导就会建立在一个残缺的历史上。
//! 崩在中间 ⇒ SQLite 回滚，链停在上一条完整状态（§2.4"可复现、可审计"的落地）。

use std::path::Path;
use std::time::Instant;

use petcore::gacha::{
    AuditRecord, Catalog, ChainReport, Currency, Engine, GachaError, GachaParams, Owned, PullState,
    Rarity, unhex, verify_chain, verify_chain_anchored, verify_replay,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::migrate;
use crate::clock;
use crate::ledger::{self, PullReport, PullRow};
use crate::paths::{DbPath, PathSource};

/// `meta.seed`：抽取骰子的根种子。第三方重放只需要这一个数 + 公示参数。
pub const SEED_KEY: &str = "seed";
/// `meta.audit_head`：链尾摘要（hex）。删尾记录会被它抓住。
pub const AUDIT_HEAD_KEY: &str = "audit_head";
/// `meta.audit_len`：链长。
pub const AUDIT_LEN_KEY: &str = "audit_len";
/// 允许在"路径由默认解析得出（= 用户真实存档）"时写库的显式开关。
pub const ALLOW_DEFAULT_ENV: &str = "OPET_ALLOW_DEFAULT_ARCHIVE";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(String),
    Io(String),
    Migrate(migrate::MigrateError),
    /// 库里缺 `seed` 且系统随机源不可用。**不退化成墙钟**（那会让重放失败）。
    NoRandomSource(String),
    BadSeed(String),
    BadHash(String),
    /// 账本自相矛盾（batch_index 说本批 N 抽，可账本里不够 N 行）。
    CorruptLedger(i64),
    /// 库里存的 kind/rarity 值不在 DDL 的 CHECK 白名单里。
    BadEnum {
        column: &'static str,
        value: String,
    },
    Engine(GachaError),
    Audit(petcore::gacha::AuditError),
    /// 库里的 `base_rate/soft_pity/hard_pity` 与传入的 `GachaParams` 不一致
    /// ⇒ "公示概率 != 代码概率"的**运行时**版本（硬约束 #2）。
    PublishedParamsDiffer {
        banner_id: String,
        stored: String,
        wanted: String,
    },
    /// v1 一库一档案一 banner。混了别的 banner 就不许再抽，
    /// 因为重放的前提（seq 从 1 连续、单一目录）会被破坏。
    SingleLedgerRequired {
        banner_id: String,
        other: String,
    },
    NotEnoughShards {
        need: u64,
        have: u64,
    },
    UnknownItem(String),
    /// 路径来自默认解析（用户真实存档）而调用方没显式授权写入。
    DefaultArchiveNotAllowed(String),
    LedgerOutsideCatalog(Vec<String>),
    /// 库里没有这条 banner。
    NoBanner(String),
    /// 导出文件的格式问题（schema 版本、hex、seq 连续性）。
    Export(super::export::ExportError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use StoreError::*;
        match self {
            Sqlite(e) => write!(f, "SQLite: {e}"),
            Io(e) => write!(f, "文件系统: {e}"),
            Migrate(e) => write!(f, "{e}"),
            NoRandomSource(e) => write!(f, "拿不到系统随机源，无法首次播种 seed: {e}"),
            BadSeed(s) => write!(f, "meta.seed = {s:?} 不是合法 u64"),
            BadHash(s) => write!(f, "非法哈希: {s}"),
            CorruptLedger(seq) => {
                write!(f, "账本自相矛盾（seq={seq}）：batch 计数与实际行数不符")
            }
            BadEnum { column, value } => write!(f, "{column} = {value:?} 不在 CHECK 白名单"),
            Engine(e) => write!(f, "引擎拒绝: {e}"),
            Audit(e) => write!(f, "审计不通过: {e}"),
            PublishedParamsDiffer {
                banner_id,
                stored,
                wanted,
            } => write!(
                f,
                "banner {banner_id} 库里公示 {stored}，代码要 {wanted} \
                 ⇒ 改了参数没重跑公示（硬约束 #2）"
            ),
            SingleLedgerRequired { banner_id, other } => write!(
                f,
                "本库已有 banner {other} 的账本；v1 只支持单档案单 banner 的可重放链，\
                 要抽 {banner_id} 请另开一个 --db"
            ),
            NotEnoughShards { need, have } => write!(f, "碎片不足：需要 {need}，余额 {have}"),
            UnknownItem(id) => write!(f, "目录里没有 {id:?}"),
            DefaultArchiveNotAllowed(p) => write!(
                f,
                "路径来自默认解析（{p}）＝用户真实存档，本次拒绝写入。\
                 要么显式加 --db PATH，要么设 {ALLOW_DEFAULT_ENV}=1"
            ),
            LedgerOutsideCatalog(ids) => write!(
                f,
                "账本里有 {} 个 id 不在当前目录中（换过内容包？）: {:?}",
                ids.len(),
                &ids[..ids.len().min(5)]
            ),
            NoBanner(id) => write!(
                f,
                "库里没有 banner {id:?}（先跑一次 opet pull 或 opet catalog validate）"
            ),
            Export(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e.to_string())
    }
}

impl From<migrate::MigrateError> for StoreError {
    fn from(e: migrate::MigrateError) -> Self {
        Self::Migrate(e)
    }
}

impl From<GachaError> for StoreError {
    fn from(e: GachaError) -> Self {
        Self::Engine(e)
    }
}

impl From<petcore::gacha::AuditError> for StoreError {
    fn from(e: petcore::gacha::AuditError) -> Self {
        Self::Audit(e)
    }
}

impl From<super::export::ExportError> for StoreError {
    fn from(e: super::export::ExportError) -> Self {
        Self::Export(e)
    }
}

pub struct Store {
    conn: Connection,
    /// 内存库时为 `None`。
    path: Option<DbPath>,
    seed: u64,
}

impl Store {
    // ---- 建库 ----

    fn configure(conn: &Connection) -> Result<(), StoreError> {
        // WAL：宠物进程与 CLI 可能同时打开这个库（CLI 写、宠物只读设置）。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// 直接给路径（等价于 `--db`）：允许创建与写入。
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_at(&DbPath {
            path: path.to_path_buf(),
            source: PathSource::CliArg,
        })
    }

    pub fn open_at(db: &DbPath) -> Result<Self, StoreError> {
        Self::open_with(db, std::env::var_os(ALLOW_DEFAULT_ENV).is_some())
    }

    /// `allow_default = false` 时，路径来自默认解析就拒绝写入（计划"风险"表最后一条：
    /// 元测试与探针绝不该落到用户真存档上）。
    pub fn open_with(db: &DbPath, allow_default: bool) -> Result<Self, StoreError> {
        if db.source.is_default() && !allow_default {
            return Err(StoreError::DefaultArchiveNotAllowed(
                db.path.display().to_string(),
            ));
        }
        if !db.path.is_absolute() {
            // 相对路径取决于 cwd，而 `opet pull` 可能被 systemd user service 起 ⇒ 明确拒绝。
            return Err(StoreError::Io(format!(
                "存档路径必须是绝对路径，收到 {:?}",
                db.path
            )));
        }
        let parent = db.path.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(parent)
            .map_err(|e| StoreError::Io(format!("{}: {e}", parent.display())))?;
        let conn = Connection::open(&db.path)?;
        Self::configure(&conn)?;
        migrate::migrate(&conn)?;
        let mut this = Self {
            conn,
            path: Some(db.clone()),
            seed: 0,
        };
        this.bootstrap()?;
        Ok(this)
    }

    /// 内存库：所有写入测试与模拟都走这里，绝不会落到用户存档。
    pub fn in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        migrate::migrate(&conn)?;
        let mut this = Self {
            conn,
            path: None,
            seed: 0,
        };
        this.bootstrap()?;
        Ok(this)
    }

    /// 建默认档案、播种 seed、开碎片账户。
    fn bootstrap(&mut self) -> Result<(), StoreError> {
        let now = clock::now_secs();
        self.conn.execute(
            "INSERT OR IGNORE INTO profiles(id, display_name, created_at) VALUES (?1, ?1, ?2)",
            params![crate::DEFAULT_PROFILE, now],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO shards(currency, amount) VALUES (?1, 0)",
            params![Currency::Shards.as_str()],
        )?;
        self.seed = match self.meta_get(SEED_KEY)? {
            Some(raw) => raw
                .trim()
                .parse::<u64>()
                .map_err(|_| StoreError::BadSeed(raw))?,
            None => {
                let s = random_seed().map_err(StoreError::NoRandomSource)?;
                self.meta_set(SEED_KEY, &s.to_string())?;
                s
            }
        };
        Ok(())
    }

    pub fn path(&self) -> Option<&DbPath> {
        self.path.as_ref()
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---- meta ----

    pub fn meta_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// 抽取根种子（首次播种来自系统随机源，之后**永不**自动变）。
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// 显式换 seed（`opet pull --seed` / 测试）。连带清掉账本与库存：
    /// 留着旧历史，链锚点就和新的 seed 对不上，`audit verify` 必然红。
    pub fn reseed(&mut self, seed: u64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM gacha_pull; DELETE FROM inventory; DELETE FROM shards;
             DELETE FROM meta WHERE key IN ('audit_head','audit_len');",
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('seed', ?1)",
            params![seed.to_string()],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO shards(currency, amount) VALUES (?1, 0)",
            params![Currency::Shards.as_str()],
        )?;
        tx.commit()?;
        self.seed = seed;
        Ok(())
    }

    // ---- banner：公示 == 代码 的运行时那一道 ----

    /// 写入或核对 banner 行。**已存在且任一数值不同 ⇒ 报错**，绝不"顺手更新"：
    /// 悄悄改掉公示值正是"公示变成假话"的那条路。
    pub fn ensure_banner(
        &self,
        p: &GachaParams,
        banner_id: &str,
        series: Option<&str>,
    ) -> Result<(), StoreError> {
        let wanted = BannerRow::from_params(p, Currency::CompanionTime);
        let stored: Option<BannerRow> = self
            .conn
            .query_row(
                &format!("SELECT {} FROM gacha_banner WHERE id = ?1", BANNER_COLS),
                params![banner_id],
                BannerRow::from_row,
            )
            .optional()?;
        if let Some(stored) = stored {
            if stored != wanted {
                return Err(StoreError::PublishedParamsDiffer {
                    banner_id: banner_id.into(),
                    stored: stored.describe(),
                    wanted: wanted.describe(),
                });
            }
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO gacha_banner(id, series, start_at, end_at, base_rate, soft_pity, hard_pity,
                    soft_step, batch_size, ten_pull_sr_floor, dup_shards, ssr_redeem_cost, currency)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                banner_id,
                series,
                clock::now_secs(),
                wanted.base_rate,
                wanted.soft_pity,
                wanted.hard_pity,
                wanted.soft_step,
                wanted.batch_size,
                wanted.ten_pull_sr_floor,
                wanted.dup_shards,
                wanted.ssr_redeem_cost,
                wanted.currency
            ],
        )?;
        Ok(())
    }

    /// 从库里读回一份 `GachaParams`。`audit verify` 用它，而不是"命令行说参数是什么"——
    /// 重放必须用**当年公示的那份参数**，用"现在的代码常量"重放等于什么都没验。
    pub fn banner_params(&self, banner_id: &str) -> Result<GachaParams, StoreError> {
        let row: Option<BannerRow> = self
            .conn
            .query_row(
                &format!("SELECT {} FROM gacha_banner WHERE id = ?1", BANNER_COLS),
                params![banner_id],
                BannerRow::from_row,
            )
            .optional()?;
        let row = row.ok_or_else(|| StoreError::NoBanner(banner_id.into()))?;
        row.to_params()
    }

    pub fn banner_currency(&self, banner_id: &str) -> Result<Currency, StoreError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT currency FROM gacha_banner WHERE id = ?1",
                params![banner_id],
                |r| r.get(0),
            )
            .optional()?;
        let raw = raw.ok_or_else(|| StoreError::NoBanner(banner_id.into()))?;
        Currency::parse(&raw).ok_or(StoreError::BadEnum {
            column: "gacha_banner.currency",
            value: raw,
        })
    }

    // ---- 目录落库 ----

    /// 把 catalog 的条目写成 `pets` / `accessories` 行（已存在则忽略）。返回 `(宠, 配件)`。
    pub fn install_catalog(&mut self, catalog: &Catalog) -> Result<(usize, usize), StoreError> {
        let now = clock::now_secs();
        let tx = self.conn.transaction()?;
        let mut pets = 0;
        let mut acc = 0;
        for item in &catalog.items {
            match item.kind {
                petcore::gacha::ItemKind::Pet => {
                    tx.execute(
                        "INSERT OR IGNORE INTO pets(id, name, rarity, atlas_path, manifest_path, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            item.id,
                            item.name,
                            item.rarity.as_str(),
                            item.manifest_path.as_ref().map(|p| sibling_sheet(p)),
                            item.manifest_path,
                            now
                        ],
                    )?;
                    pets += 1;
                }
                petcore::gacha::ItemKind::Accessory => {
                    tx.execute(
                        "INSERT OR IGNORE INTO accessories(id, name, slot, rarity, manifest_path)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            item.id,
                            item.name,
                            item.slot.map(|s| s.as_str()).unwrap_or("head"),
                            item.rarity.as_str(),
                            item.manifest_path
                        ],
                    )?;
                    acc += 1;
                }
            }
        }
        tx.commit()?;
        Ok((pets, acc))
    }

    // ---- 库存 / 碎片 ----

    /// 引擎要的"已拥有集合"。每次抽取前从这里重建 ⇒ 库存是唯一真相。
    pub fn owned(&self) -> Result<Owned, StoreError> {
        let mut stmt = self.conn.prepare("SELECT item_id FROM inventory")?;
        let ids = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(ids
            .collect::<Result<Vec<String>, _>>()?
            .into_iter()
            .collect())
    }

    pub fn item_count(&self, item_id: &str) -> Result<u64, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT count FROM inventory WHERE item_id = ?1",
                params![item_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as u64)
    }

    pub fn shards(&self, currency: Currency) -> Result<u64, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT amount FROM shards WHERE currency = ?1",
                params![currency.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as u64)
    }

    pub fn add_shards(&mut self, currency: Currency, amount: u64) -> Result<u64, StoreError> {
        self.conn.execute(
            "INSERT INTO shards(currency, amount) VALUES (?1, ?2)
             ON CONFLICT(currency) DO UPDATE SET amount = amount + excluded.amount",
            params![currency.as_str(), i64_sql(amount)],
        )?;
        self.shards(currency)
    }

    // ---- 账本 ----

    pub fn rows(&self, profile_id: &str, banner_id: &str) -> Result<Vec<PullRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, profile_id, banner_id, ts, rarity, item_kind, item_id,
                    pity_counter, batch_index, dice_used, is_duplicate, shards_gained,
                    ssr_prob, rng_state_hash, audit_prev_hash
             FROM gacha_pull WHERE profile_id = ?1 AND banner_id = ?2 ORDER BY seq",
        )?;
        let fetched = stmt.query_map(params![profile_id, banner_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, i64>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, String>(14)?,
            ))
        })?;
        fetched
            .map(|t| {
                let t = t?;
                Ok(PullRow {
                    seq: t.0,
                    profile_id: t.1,
                    banner_id: t.2,
                    ts: t.3,
                    rarity: Rarity::parse(&t.4).ok_or_else(|| StoreError::BadEnum {
                        column: "gacha_pull.rarity",
                        value: t.4.clone(),
                    })?,
                    item_kind: petcore::gacha::ItemKind::parse(&t.5).ok_or_else(|| {
                        StoreError::BadEnum {
                            column: "gacha_pull.item_kind",
                            value: t.5.clone(),
                        }
                    })?,
                    item_id: t.6,
                    pity_before: u32_sql(t.7, "pity_counter")?,
                    batch_index: u32_sql(t.8, "batch_index")?,
                    dice_used: u32_sql(t.9, "dice_used")?,
                    duplicate: t.10 != 0,
                    shards_gained: u32_sql(t.11, "shards_gained")?,
                    ssr_prob: u32_sql(t.12, "ssr_prob")?,
                    rng_state_hash: t.13,
                    prev_digest: t.14,
                })
            })
            .collect()
    }

    /// 本档案本 banner 的审计记录（`seq` 升序）。
    pub fn records(
        &self,
        profile_id: &str,
        banner_id: &str,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        self.rows(profile_id, banner_id)?
            .iter()
            .map(PullRow::to_record)
            .collect()
    }

    /// 下一次抽取的 pity 状态 + 该用的 seq。**从账本推导，不存表。**
    pub fn pity_state(
        &self,
        p: &GachaParams,
        profile_id: &str,
        banner_id: &str,
    ) -> Result<(PullState, u64), StoreError> {
        let rows = self.rows(profile_id, banner_id)?;
        let st = ledger::derive_state(p, &rows)?;
        let next_seq = rows.last().map(|r| r.seq + 1).unwrap_or(1) as u64;
        Ok((st, next_seq))
    }

    fn head(&self) -> Result<[u8; 32], StoreError> {
        match self.meta_get(AUDIT_HEAD_KEY)? {
            Some(s) => Ok(unhex(&s).ok_or(StoreError::BadHash(s))?),
            None => Ok(petcore::gacha::GENESIS),
        }
    }

    // ---- 抽取 ----

    /// 抽 `count` 次并落库。**这是唯一会写 `gacha_pull` 的路径。**
    ///
    /// 顺序：核对公示 → 推导状态 → 纯函数抽完 → 一个事务里写账本/库存/碎片/链尾。
    /// 骰子全在 `petcore::gacha` 里掷，这里只做 IO 与状态推进。
    pub fn pull(
        &mut self,
        p: &GachaParams,
        catalog: &Catalog,
        banner_id: &str,
        count: u32,
    ) -> Result<PullReport, StoreError> {
        self.pull_for(p, catalog, banner_id, crate::DEFAULT_PROFILE, count)
    }

    pub fn pull_for(
        &mut self,
        p: &GachaParams,
        catalog: &Catalog,
        banner_id: &str,
        profile_id: &str,
        count: u32,
    ) -> Result<PullReport, StoreError> {
        let started = Instant::now();
        self.ensure_banner(p, banner_id, Some(&catalog.id))?;
        self.single_ledger_guard(profile_id, banner_id)?;
        let engine = Engine::new(p, catalog)?;
        let (state, first_seq) = self.pity_state(p, profile_id, banner_id)?;
        let mut owned = self.owned()?;
        let (outcomes, _after) =
            engine.pull_batch(&state, &mut owned, self.seed, first_seq, count)?;

        let mut head = self.head()?;
        let now = clock::now_secs();
        let mut chain_len: u64 = self
            .meta_get(AUDIT_LEN_KEY)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let tx = self.conn.transaction()?;
        let mut counts = [0u32; 4];
        let mut shards_gained = 0u64;
        let mut duplicates = 0u32;
        for o in &outcomes {
            let row = PullRow::from_outcome(o, profile_id, banner_id, now, self.seed, head);
            head = row.to_record()?.digest();
            chain_len += 1;
            counts[o.rarity.index()] += 1;
            if o.duplicate {
                duplicates += 1;
            }
            shards_gained += u64::from(o.shards_gained);

            tx.execute(
                "INSERT INTO gacha_pull(seq, profile_id, banner_id, ts, rarity, item_kind, item_id,
                        pity_counter, batch_index, dice_used, is_duplicate, shards_gained,
                        ssr_prob, rng_state_hash, audit_prev_hash)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    row.seq,
                    row.profile_id,
                    row.banner_id,
                    row.ts,
                    row.rarity.as_str(),
                    row.item_kind.as_str(),
                    row.item_id,
                    row.pity_before,
                    row.batch_index,
                    row.dice_used,
                    row.duplicate as i64,
                    row.shards_gained,
                    row.ssr_prob,
                    row.rng_state_hash,
                    row.prev_digest
                ],
            )?;

            // 库存：首得与再得都 +1 count，重复另加 duplicates（它是碎片的来源账）。
            tx.execute(
                "INSERT INTO inventory(kind, item_id, count, duplicates, obtained_at, obtained_from)
                 VALUES (?1, ?2, 1, 0, ?3, ?4)
                 ON CONFLICT(kind, item_id) DO UPDATE SET
                   count = count + 1,
                   duplicates = duplicates + ?5",
                params![
                    row.item_kind.as_str(),
                    row.item_id,
                    now,
                    format!("gacha:{banner_id}#{}", row.seq),
                    i64::from(row.duplicate)
                ],
            )?;
            if row.shards_gained > 0 {
                tx.execute(
                    "INSERT INTO shards(currency, amount) VALUES (?1, ?2)
                     ON CONFLICT(currency) DO UPDATE SET amount = amount + excluded.amount",
                    params![Currency::Shards.as_str(), i64::from(row.shards_gained)],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![AUDIT_HEAD_KEY, petcore::gacha::hex(&head)],
        )?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![AUDIT_LEN_KEY, chain_len.to_string()],
        )?;
        tx.commit()?;

        let last_seq = outcomes
            .last()
            .map(|o| o.seq)
            .unwrap_or_else(|| first_seq.saturating_sub(1));
        Ok(PullReport {
            banner_id: banner_id.into(),
            profile_id: profile_id.into(),
            counts,
            ssr_count: counts[Rarity::SSR.index()],
            duplicates,
            shards_gained: shards_gained as u32,
            shards_after: self.shards(Currency::Shards)?,
            first_seq,
            last_seq,
            chain_len,
            head_hex: petcore::gacha::hex(&head),
            outcomes,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    /// v1 一库一档案一 banner：混了就明确拒绝，别把审计链的前提悄悄破坏掉。
    fn single_ledger_guard(&self, profile_id: &str, banner_id: &str) -> Result<(), StoreError> {
        let other: Option<String> = self
            .conn
            .query_row(
                "SELECT DISTINCT banner_id FROM gacha_pull
                 WHERE profile_id = ?1 AND banner_id <> ?2 LIMIT 1",
                params![profile_id, banner_id],
                |r| r.get(0),
            )
            .optional()?;
        match other {
            Some(other) => Err(StoreError::SingleLedgerRequired {
                banner_id: banner_id.into(),
                other,
            }),
            None => Ok(()),
        }
    }

    /// 碎片自选 SSR（§3.6 的 120 碎片）。**不掷骰**——"随机 ≠ 付费"在这里同样成立：
    /// 确定性兑换不是抽取，所以它既不进 `gacha_pull` 也不进审计链。
    pub fn redeem_ssr(
        &mut self,
        p: &GachaParams,
        catalog: &Catalog,
        item_id: &str,
    ) -> Result<u64, StoreError> {
        let item = catalog
            .find(item_id)
            .ok_or_else(|| StoreError::UnknownItem(item_id.into()))?;
        if item.rarity != Rarity::SSR {
            return Err(StoreError::UnknownItem(format!(
                "{item_id} 是 {}，自选兑换只收 SSR",
                item.rarity.as_str()
            )));
        }
        let need = u64::from(p.ssr_redeem_cost);
        let have = self.shards(Currency::Shards)?;
        if have < need {
            return Err(StoreError::NotEnoughShards { need, have });
        }
        let now = clock::now_secs();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE shards SET amount = amount - ?1 WHERE currency = ?2",
            params![need as i64, Currency::Shards.as_str()],
        )?;
        tx.execute(
            "INSERT INTO inventory(kind, item_id, count, duplicates, obtained_at, obtained_from)
             VALUES (?1, ?2, 1, 0, ?3, 'redeem:shards')
             ON CONFLICT(kind, item_id) DO UPDATE SET count = count + 1",
            params![item.kind.as_str(), item.id, now],
        )?;
        tx.commit()?;
        self.shards(Currency::Shards)
    }

    // ---- 审计 ----

    /// 全链校验 + 用库里的 seed 与**库里存的**公示参数重放。
    /// 这是 §6.1"审计重放脚本可校验任意记录"的完整口径。
    pub fn verify(
        &self,
        catalog: &Catalog,
        profile_id: &str,
        banner_id: &str,
    ) -> Result<ChainReport, StoreError> {
        let p = self.banner_params(banner_id)?;
        self.verify_with(&p, catalog, profile_id, banner_id)
    }

    pub fn verify_with(
        &self,
        p: &GachaParams,
        catalog: &Catalog,
        profile_id: &str,
        banner_id: &str,
    ) -> Result<ChainReport, StoreError> {
        let rows = self.rows(profile_id, banner_id)?;
        let stray = ledger::ledger_items_outside_catalog(&rows, catalog);
        if !stray.is_empty() {
            return Err(StoreError::LedgerOutsideCatalog(stray));
        }
        let records: Vec<AuditRecord> = rows
            .iter()
            .map(PullRow::to_record)
            .collect::<Result<_, StoreError>>()?;
        self.verify_records(p, catalog, &records)
    }

    /// 对**已经拿在手里的**记录做校验（导出文件走这条，满足"只吃导出、不吃数据库"）。
    pub fn verify_records(
        &self,
        p: &GachaParams,
        catalog: &Catalog,
        records: &[AuditRecord],
    ) -> Result<ChainReport, StoreError> {
        let stored_head = self.meta_get(AUDIT_HEAD_KEY)?;
        let report = match &stored_head {
            Some(h) => verify_chain_anchored(records, h)?,
            None => verify_chain(records)?,
        };
        verify_replay(self.seed, p, catalog, records)?;
        Ok(report)
    }

    /// 只验链（不需要 seed、不需要目录）：回答"库是不是被人删过行"。
    pub fn verify_chain_only(
        &self,
        profile_id: &str,
        banner_id: &str,
    ) -> Result<ChainReport, StoreError> {
        let records = self.records(profile_id, banner_id)?;
        let stored_head = self.meta_get(AUDIT_HEAD_KEY)?;
        Ok(match &stored_head {
            Some(h) => verify_chain_anchored(&records, h)?,
            None => verify_chain(&records)?,
        })
    }

    /// 导出 JSONL（一行一条）。**第三方只需这个文件 + seed + 公示参数**即可复核。
    pub fn export_jsonl(&self, profile_id: &str, banner_id: &str) -> Result<String, StoreError> {
        let mut out = String::new();
        for r in self.rows(profile_id, banner_id)? {
            let rec = r.to_record()?;
            out.push_str(&super::export::encode_one(&rec, r.ts)?);
        }
        Ok(out)
    }

    /// 从 JSONL 读回记录（`opet audit verify --file`：只吃导出文件）。
    pub fn records_from_jsonl(text: &str) -> Result<Vec<AuditRecord>, StoreError> {
        super::export::decode(text).map_err(StoreError::from)
    }
}

/// SQLite 存 INTEGER，但代码里这些列是 `u32`：越界就是库坏了，不许静默截断。
fn u32_sql(v: i64, column: &str) -> Result<u32, StoreError> {
    u32::try_from(v).map_err(|_| StoreError::Sqlite(format!("{column} 越界: {v}")))
}

fn i64_sql(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn sibling_sheet(manifest_path: &str) -> String {
    Path::new(manifest_path)
        .parent()
        .map(|p| p.join("sheet.webp").display().to_string())
        .unwrap_or_else(|| "sheet.webp".into())
}

/// 首次播种用的真随机。**失败就报错，绝不退化成 `time`**——
/// 拿墙钟当 seed 会让"同 seed 重放"变成"每次都不一样"，审计链当场作废。
fn random_seed() -> Result<u64, String> {
    let mut buf = [0u8; 8];
    match getrandom::fill(&mut buf) {
        Ok(()) => Ok(u64::from_be_bytes(buf)),
        Err(e) => Err(e.to_string()),
    }
}

/// `gacha_banner` 里凡是**进过骰子或折算**的列。顺序与 [`BannerRow`] 的字段一致。
const BANNER_COLS: &str = "base_rate, soft_pity, hard_pity, soft_step, batch_size, \
                           ten_pull_sr_floor, dup_shards, ssr_redeem_cost, currency";

/// 一张 banner 行的完整形状 ⇄ [`GachaParams`]。
///
/// 为什么要这个中间结构（而不是就地 `query_row` 拼元组）：`ensure_banner` 要拿"库里存的"
/// 与"代码要的"做**整行等值比较**，元组比到第 7 个数就已经看不出比的是什么了；
/// 而且 `Derived` 出来的 `PartialEq` 让漏比一个字段这件事在类型上就发生不了。
#[derive(Clone, Debug, PartialEq, Eq)]
struct BannerRow {
    base_rate: String,
    soft_pity: i64,
    hard_pity: i64,
    soft_step: i64,
    batch_size: i64,
    ten_pull_sr_floor: i64,
    dup_shards: String,
    ssr_redeem_cost: i64,
    currency: String,
}

impl BannerRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            base_rate: r.get(0)?,
            soft_pity: r.get(1)?,
            hard_pity: r.get(2)?,
            soft_step: r.get(3)?,
            batch_size: r.get(4)?,
            ten_pull_sr_floor: r.get(5)?,
            dup_shards: r.get(6)?,
            ssr_redeem_cost: r.get(7)?,
            currency: r.get(8)?,
        })
    }

    fn from_params(p: &GachaParams, currency: Currency) -> Self {
        Self {
            base_rate: p.rates.to_base_rate_json(),
            soft_pity: i64::from(p.soft_start),
            hard_pity: i64::from(p.hard_pity),
            soft_step: i64::from(p.soft_step),
            batch_size: i64::from(p.batch_size),
            ten_pull_sr_floor: p.ten_pull_sr_floor as i64,
            dup_shards: shards_to_text(&p.dup_shards),
            ssr_redeem_cost: i64::from(p.ssr_redeem_cost),
            currency: currency.as_str().to_string(),
        }
    }

    fn to_params(&self) -> Result<GachaParams, StoreError> {
        let rates = petcore::gacha::Rates::from_base_rate_json(&self.base_rate).map_err(|e| {
            StoreError::Sqlite(format!("base_rate {:?} 解析失败: {e}", self.base_rate))
        })?;
        let ten_pull_sr_floor = match self.ten_pull_sr_floor {
            0 => false,
            1 => true,
            other => {
                return Err(StoreError::BadEnum {
                    column: "ten_pull_sr_floor",
                    value: other.to_string(),
                });
            }
        };
        let p = GachaParams {
            rates,
            soft_start: u32_sql(self.soft_pity, "soft_pity")?,
            soft_step: u32_sql(self.soft_step, "soft_step")?,
            hard_pity: u32_sql(self.hard_pity, "hard_pity")?,
            ten_pull_sr_floor,
            batch_size: u32_sql(self.batch_size, "batch_size")?,
            dup_shards: shards_from_text(&self.dup_shards)?,
            ssr_redeem_cost: u32_sql(self.ssr_redeem_cost, "ssr_redeem_cost")?,
        };
        p.validate()
            .map_err(|e| StoreError::Sqlite(format!("banner 参数不合法: {e}")))?;
        Ok(p)
    }

    /// 报错时要能一眼看出**哪个数**不同（`{stored} != {wanted}` 整行打印）。
    fn describe(&self) -> String {
        format!(
            "{{{BANNER_COLS}: [{}, {}, {}, {}, {}, {}, {}, {}, {}]}}",
            self.base_rate,
            self.soft_pity,
            self.hard_pity,
            self.soft_step,
            self.batch_size,
            self.ten_pull_sr_floor,
            self.dup_shards,
            self.ssr_redeem_cost,
            self.currency
        )
    }
}

fn shards_to_text(s: &[u32; 4]) -> String {
    format!("[{}, {}, {}, {}]", s[0], s[1], s[2], s[3])
}

/// `dup_shards` 列（JSON 数组）→ `[u32; 4]`。手写而非引 serde：形状固定成一句话，
/// 而且"多一个数/少一个数"必须是错误，不是"截断或补零"。
fn shards_from_text(text: &str) -> Result<[u32; 4], StoreError> {
    let body = text.trim().trim_start_matches('[').trim_end_matches(']');
    let parts: Vec<&str> = body.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(StoreError::Sqlite(format!(
            "dup_shards 必须是 4 个数（N/R/SR/SSR），实得 {text:?}"
        )));
    }
    let mut out = [0u32; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<u32>()
            .map_err(|_| StoreError::Sqlite(format!("dup_shards[{i}] = {p:?} 不是非负整数")))?;
    }
    Ok(out)
}
