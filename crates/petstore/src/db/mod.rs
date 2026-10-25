//! SQLite 存档层。[`Store`] 是本 crate 唯一持有 `Connection` 的类型。

mod export;
pub mod migrate;
mod store;

pub use export::{EXPORT_SCHEMA, ExportError, ExportRecord, decode, encode, encode_one};
pub use migrate::{KEY_VERSION, MIGRATIONS, SCHEMA_V1, SCHEMA_VERSION, current_version};
pub use store::{ALLOW_DEFAULT_ENV, AUDIT_HEAD_KEY, AUDIT_LEN_KEY, SEED_KEY, Store, StoreError};
