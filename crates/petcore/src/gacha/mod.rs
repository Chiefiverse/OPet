//! 盲盒子系统（设计书 §3.6）：**纯函数、全整数、无 IO、无墙钟**。
//!
//! 分层与 `PresentationIntent` 同理（§3.1）：引擎只做 `(参数, 目录, 状态, seed, seq) → 结果`，
//! 落库与时间戳归 `crates/petstore`。这样"同种子逐位可重放"（硬约束 #8）由本模块的测试守住，
//! 第三方审计只需 `seed + seq + 公示参数` 即可重掷骰子（§3.6"可验证公平"）。
//!
//! ```
//! use petcore::gacha::{Catalog, Engine, GachaParams, PullState};
//!
//! let params = GachaParams::V1;
//! let catalog = Catalog::default_v1().unwrap();
//! let engine = Engine::new(&params, &catalog).unwrap();
//! let mut owned = Default::default();
//! let (outcome, _next) = engine
//!     .pull(&PullState::fresh(), &mut owned, 1, 1)
//!     .unwrap();
//! assert!(owned.contains(&outcome.item_id));
//! ```

pub mod audit;
pub mod catalog;
pub mod currency;
pub mod disclose;
pub mod engine;
pub mod params;
pub mod pity;
pub mod rarity;
pub mod sim;
pub mod stream;

pub use audit::{
    AuditError, AuditRecord, ChainReport, DIGEST_LEN, GENESIS, REC_FORMAT, hex, rng_state_hash,
    unhex, verify_chain, verify_chain_anchored, verify_replay,
};
pub use catalog::{CATALOG_FORMAT_V1, Catalog, CatalogError, CatalogItem, ItemKind, Slot};
pub use currency::Currency;
pub use disclose::{
    DEFAULT_RATES_JSON, EFFECTIVE_METHOD, Published, RATES_ASSET, RATES_FORMAT, RatesFile,
    build as build_rates_file, percent_str,
};
pub use engine::{Engine, GachaError, Owned, PullOutcome, batch_has_sr_plus};
pub use params::{GachaParams, ParamsError, Rates, SCALE};
pub use pity::{PullState, RarityRoll, roll_rarity, soft_bonus};
pub use rarity::Rarity;
pub use sim::{
    BASE_SIGMA, BaseCheck, CHI2_CRITICAL_MILLIONTHS, EFFECTIVE_HORIZON, Fit, ItemRate,
    MAX_DEV_MILLIONTHS, Observed, effective_rates, expected_counts, item_rates, simulate,
};
pub use stream::{PullStream, derive_seed};
