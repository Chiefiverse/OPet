//! 绘制层：菜单 / 结果卡 / 背包面板 / 词表 / 文字 / 字体兜底。
//!
//! 这六个模块 2026-11-22 从 `overlay-wayland` 整搬上来（M5 第二批拍板③"上提共用，零分叉"，
//! 先例 = `atlas`/`region`/`accounting`）。搬的判据不是"看起来通用"，而是实测：六个文件头部
//! 自带的约束就是"零 wayland 类型"，全部外部依赖只有 `petcore` 与 `ab_glyph`（纯 Rust），
//! 互引全走 `crate::` 根路径 —— 于是这里用 lib.rs 的再导出把 `crate::menu::…` 这类既有
//! 路径**一个都不用改**。
//!
//! ⚠️ 反面理由与 `atlas` 同族：这一层若被复制成两份，两份会在某一天静默分叉，而分叉的症状
//! 画在屏上（菜单行距、卡面相位、词表键），不会报在编译器上。两家客户端**只许画同一份**。
//!
//! ## 各模块
//!
//! - [`menu`] —— 右键菜单的几何与命中（`rects/hit_test/press/placement/content_sig`）与叠层动画表。
//! - [`card`] —— 抽卡结果卡面：相位机 + 像素（`Ctx.motion` 三档在这里落地）。
//! - [`panel`] —— 背包/收藏/设置面板；与菜单互斥（"两块面永不同时在场"）。
//! - [`l10n`] —— 词表 `Key` × `Lang`，以及"当前 locale 会画到屏上的字符串"的出口。
//! - [`text`] —— 文字层：入库 CFF 子集栅格化（`FONT_BYTES` = `include_bytes!`，资产在
//!   `crates/overlay-common/assets/fonts/`）。
//! - [`font`] —— `text` 之前的旧字体兜底路径，保留是因为它仍被引用。
//! - [`settings`] —— `Motion`/`Lang` 的档位、来源标签与**生效值优先级链本体**
//!   （`--lang` > 文件 > `LC_ALL`/`LANG` > 默认）。2026-11-24（M7）整条从 `overlay-wayland`
//!   提上来，覆盖 2026-11-22 拍板③那句"解析链留在各家客户端"；链的输入是本模块的
//!   [`settings::FileUi`]（两个字符串）而**不是** `petstore::config::UiKeys` —— 各家客户端
//!   只留"读哪条路径、读哪个环境变量"那一层搬运。
//!
//! 资产门禁在 `tests/font_assets.rs`（随资产一起搬上来：`include_bytes!("../assets/…")`
//! 的相对深度没变）。

pub mod card;
pub mod font;
pub mod l10n;
pub mod menu;
pub mod panel;
pub mod settings;
pub mod text;
