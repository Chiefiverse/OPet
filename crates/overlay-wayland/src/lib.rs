//! OPet 的 Wayland 适配层（设计书 §3.7）。
//!
//! 契约来自 [`petcore::render::OverlayBackend`]：**内核只给图集格坐标，不经手像素**，
//! 所以解码 sheet、预乘 alpha、写 shm buffer、把 alpha 区域翻成 `wl_region` 都在这里做。

pub mod app;
pub mod audio;
pub mod cli;
pub mod ipc;
pub mod probe;
pub mod settings;

/// 图集解码与 polygon 轮廓已上移到 [`overlay_common`]（与 Windows 客户端共用一份实现），
/// 这里**再导出**是为了 `crate::atlas::…` / `overlay_wayland::region::…` 这些既有路径
/// （`app.rs`、`text.rs`、`tests/real_asset.rs`、`tests/accessory_product.rs` 都在用）
/// 一个都不用改。删掉这两行 = 那四处一起改，风险不在编译期而在"某一处漏了、悄悄换成别的实现"。
///
/// 2026-11-22（M5 第二批拍板③）同一手法办了第二件事：绘制层六模块（`card/font/l10n/
/// menu/panel/text`）整搬进 `overlay_common::ui`，`crate::menu::…` 这类路径靠下面的再导出
/// 原样成立；`settings` 只有值类型与预检上提，优先级链仍在本 crate。
pub use overlay_common::{atlas, card, font, l10n, menu, panel, region, text};
