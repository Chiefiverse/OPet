//! OPet 的 Wayland 适配层（设计书 §3.7）。
//!
//! 契约来自 [`petcore::render::OverlayBackend`]：**内核只给图集格坐标，不经手像素**，
//! 所以解码 sheet、预乘 alpha、写 shm buffer、把 alpha 区域翻成 `wl_region` 都在这里做。

pub mod app;
pub mod card;
pub mod cli;
pub mod font;
pub mod ipc;
pub mod l10n;
pub mod menu;
pub mod panel;
pub mod probe;
pub mod settings;
pub mod text;

/// 图集解码与 polygon 轮廓已上移到 [`overlay_common`]（与 Windows 客户端共用一份实现），
/// 这里**再导出**是为了 `crate::atlas::…` / `overlay_wayland::region::…` 这些既有路径
/// （`app.rs`、`text.rs`、`tests/real_asset.rs`、`tests/accessory_product.rs` 都在用）
/// 一个都不用改。删掉这两行 = 那四处一起改，风险不在编译期而在"某一处漏了、悄悄换成别的实现"。
pub use overlay_common::{atlas, region};
