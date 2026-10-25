//! OPet 存档层（`petstore`）——**全项目唯一**碰磁盘和墙钟的地方。
//!
//! 分层的理由和 `PresentationIntent` 是同一条（设计书 §3.1，M3 计划 P3）：
//! [`petcore::gacha`] 是 `(参数, 目录, 状态, seed, seq) → 结果` 的纯函数，
//! "同种子逐位可重放"（硬约束 #8）由它自己的测试守着；本 crate 负责
//! 把那些结果**落成账本**、提供时间戳、持有 pity 状态，并对外给出可独立校验的审计导出。
//!
//! 于是有三条边界，破坏任何一条都会让 M3 的门禁失去意义：
//! 1. 本 crate **不实现任何抽样**。想要"顺手在这里再掷一次骰" ⇒ 先改 `petcore::gacha`，
//!    否则公示概率与代码概率不再是同一件事。
//! 2. 墙钟只从 [`clock::now_secs`] 进来（`gacha_pull.ts`、`inventory.obtained_at`、
//!    `session_log.ts` 三列）。将来要接 monotonic 或可注入的假时钟，只有这一个改动点。
//! 3. pity 状态**不建表**，从 `gacha_pull` 账本推导（[`ledger::derive_state`]）⇒
//!    少一个"状态表和账本对不上"的机会，而且推导结果天然与审计一致。
//!
//! ```no_run
//! use petstore::Store;
//! use petcore::gacha::{Catalog, GachaParams};
//!
//! // 内存库：例子绝不会写到用户真存档上（`Store::open` 要求绝对路径，且默认路径需显式放行）。
//! let mut store = Store::in_memory()?;
//! let catalog = Catalog::default_v1().unwrap();
//! let report = store.pull(&GachaParams::V1, &catalog, "cat_series", 10)?;
//! println!("SSR {} 件，本次碎片 +{}", report.ssr_count, report.shards_gained);
//! # Ok::<(), petstore::StoreError>(())
//! ```

pub mod clock;
pub mod db;
pub mod ledger;
pub mod licenses;
pub mod paths;

pub use db::{Store, StoreError};
pub use ledger::{PullReport, derive_state};
pub use licenses::{LicenseService, LicenseState, NoopLicenseService};
pub use paths::DbPath;

/// 默认账号 / 默认 banner（v1 单档案，§10.2 的 `--account local`）。
pub const DEFAULT_PROFILE: &str = "local";
pub const DEFAULT_BANNER: &str = "cat_series";
