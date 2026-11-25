//! 报表与库扫描层：两家客户端共用的那一半（M7-b 前置上提，2026-11-24）。
//!
//! ## 为什么是**新 crate**，而不是塞进 `overlay-common` 或 `petstore`
//!
//! 2026-11-22 的 M5 第二批拍板③ 写的是"绘制层上提、零逻辑分叉"，同一轮把**生效值优先级链**
//! 留在各家客户端、把报表生产者留在了 `overlay-wayland/src/app.rs`。第二批把 Windows 侧推进到
//! 菜单 + 换装 + 抽卡卡面之后，M7 的面板（收藏册 / 设置 / 概率 / 复核）要上第二块屏，那一族
//! 生产者于是必须同时服务两家。三条路各有什么问题：
//!
//! - **塞进 `overlay-common`**：这一层要读库存（`Store::owned` / `item_count` / `rows` /
//!   `verify` / `pull_for`）⇒ 必然链 `petstore` ⇒ 必然链 `rusqlite/bundled` ⇒ 破
//!   `overlay-common/Cargo.toml` 顶上那句立身之本（"零显示服务器"之外再加一条"零 IO"），
//!   也被 `tests/no_platform_deps.rs` 的 ALLOWED 逐字相等那条测例当场拦下。
//! - **塞进 `petstore`**：那边是纯数据访问层，而这里每一行都要问 `menu::fit_to`"这行字在
//!   1× / 1.5× / 2× 装不装得下"，还要问 `l10n` 取标签 ⇒ 排版与词表会倒灌进存储层。
//! - **各留一份副本**（第二批之前的事实）：Windows 那份 `Gacha::pull` 已经与 Wayland 那份
//!   逐字对齐过一次，代价写在 `overlay-win32/src/dress.rs` 的注释里（"同一份语义，逐条对齐"）。
//!   对齐是**人做的事**，而分叉的症状先出现在屏上、不会出现在编译器上 —— 这条正是
//!   `overlay-common/src/ui/mod.rs` 顶上那段反面理由。
//!
//! 所以：`overlay-report` = `petcore` + `petstore` + `overlay-common` 三者的**下游**，
//! 零平台绑定（`cargo clippy -p overlay-report --target x86_64-pc-windows-gnu` 必须绿），
//! 不含任何 surface / 像素 / 墙钟 / `SystemTime`。方向单向，谁都不许反过来依赖它之外的两家客户端。
//!
//! ## 各模块
//!
//! - [`gacha`] —— [`gacha::Gacha`]：连库 + 装目录 + 抽取，以及收藏册三本（已获得 / 还缺 / 最近）
//!   与复核那一屏的**文本生产者**。行宽预算在这里立，不在画的时候猜。
//! - [`album`] —— 逐件收藏册的一条记录与其路由（`AlbumTarget` = 这一行点下去去哪张表），
//!   以及把记录变成 `panel::PanelRow` 的那支函数。
//! - [`bodies`] —— 本体库的启动扫盘四道门禁（[`bodies::check_body`]）与"点中一只宠物"的
//!   纯判定 [`bodies::plan_swap_body`]。
//! - [`rates`] —— 「概率」那一屏的五行，**不碰库**（读的是编译期嵌进二进制的 `rates.json`）。
//!
//! ## 这里**没有**什么（故意的）
//!
//! 像素（图集解码 / 叠配件 / blit）、surface 与 input region、`config.toml` 的读写、事件环、
//! 以及"哪块屏、放在哪个角"—— 全部留在各家客户端。本层只回答两件事：**库存里有什么**，
//! 以及**那一行字该怎么说**。

pub mod album;
pub mod bodies;
pub mod gacha;
pub mod rates;

pub use album::{AlbumEntry, AlbumTarget, album_rows};
pub use bodies::{
    BodyCandidate, BodyChoice, SwapBody, check_body, db_body_row, discover_pets, pet_root,
    plan_body_override, plan_swap_body,
};
pub use gacha::Gacha;
pub use rates::published_rates_lines;
