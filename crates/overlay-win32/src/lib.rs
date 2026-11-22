//! OPet 的 Windows 客户端（设计书 §3.7 / 里程碑 M5 第一批 = 宠物上屏最小闭环）。
//!
//! ## 这一块在整体里的位置
//!
//! `petcore` 给的是**图集格坐标**（[`petcore::PresentationIntent`]），像素、窗口、
//! 光标、DPI 全在客户端（D12）。本 crate 与 `overlay-wayland` 是**并列的两个消费者**，
//! 不互相依赖，共用的是 [`overlay_common`]（`atlas` 预乘 BGRA8 + `accounting` 五档省电口径）——
//! 那两个数一旦各家写一份，两平台报出的"这一轮省不省电"就不再是同一件事，而且没有任何东西
//! 会为此变红。
//!
//! ⚠️ **本 crate 不实现 [`petcore::render::OverlayBackend`]，而且这不是遗漏**（第 8 步接线时
//! 落实的判定，原先这里写的是"本 crate 是第一个真实现该 trait 的客户端"，那句话是错的）：
//! - `on_pointer(mpsc::Sender<PointerEvent>)` 要求指针事件**异步**投递，而本平台的命中判定
//!   必须**同步**回答（`WM_NCHITTEST` 的返回值就是答案）⇒ 那条签名结构上满足不了。
//! - trait 要求 `Self: Send`，而一个 `HWND` 属于建它的那条消息循环线程。
//! - `set_passthrough(bool)` 是**整窗**开关（设计书 §3.7 给 Win/mac 的那条近似），本平台有
//!   精确的一档（逐点 + alpha 阈值）⇒ 再挂 `WS_EX_TRANSPARENT` 严格更差，而且没有调用者
//!   （实测：整个仓库里 `set_passthrough` 除 `NullBackend` 自己的字段外**零消费者**，
//!   Wayland 客户端也没调）。
//!
//! 所以本平台走自己那条同步派发（[`window::Events`]），两平台共用的是 `overlay-common` 那两份
//! 数学。trait 与平台的这一格不匹配**登记给第 11 步的 §3.7 回写**（Wayland 侧绕开 trait 的
//! 那笔旧债是同一条线的另一端）。
//!
//! ## 模块切法：把 Linux 侧的证据最大化
//!
//! 交叉编译只能证"能编"，证不了"对"。所以能做成**在宿主上就跑**的数学，一律不放进
//! `#[cfg(windows)]`：
//!
//! | 模块 | 点名 OS 类型？ | 宿主上跑到什么程度 |
//! |---|---|---|
//! | [`cli`] | 否 | 全套单元测试（含 USAGE↔parser 双向平权） |
//! | [`stage`] | **否** —— 只碰 `Vec<u8>` | 预乘字节序、alpha 通道命中（阈值边界成对 + 通道错位的假阴性）、`backend_contract.rs` 那四个探针的移植、串格与拖影 |
//! | [`layout`] | 否 | 1.00 / 1.25 / 1.50 三档几何、留白"谁赢了"、px↔逻辑 两个方向的互逆性 |
//! | [`windx`] | 否（`lParam` 的打包与 UTF-16 都只碰 `isize` / `i32` / `Vec<u16>`；入口那个 `isize` 是 `LPARAM` 的本体，第 8 步实测：windows-sys 0.60 的 `LPARAM` **就是** `isize` 而不是指针） | 有符号 `GET_X_LPARAM`、NUL 终止符、零填充字段截断、坏码元 lossy 不 panic |
//! | [`summary`] | **只在两个采集函数里**（`HANDLE` / `FILETIME` / `PROCESS_MEMORY_COUNTERS_EX`，都不点名窗口类型） | **摘要正文全套测例**：三根 grep token、恒 0 档的 inline 标注、"没链进来"不许写成 `0`、占比分母 |
//! | `window` / `monitor` / `app` | **是** | 只到 `cargo check --target x86_64-pc-windows-gnu` |
//!
//! ⚠️ **坐标系这一格是本轮实测出来的平台差别，M6 会重新踩一遍如果我不写下来**：
//! Windows 只需要 px↔逻辑 换算的**一个方向**。命中判定读的是刚交给 `UpdateLayeredWindow`
//! 那块 buffer 的**物理**像素（[`stage::Stage::hit`] 直接收物理 px），拖拽要的是"**物理 →
//! 逻辑**"（[`layout::Frame::stage_to_local`]）；正方向"逻辑 → 物理"在本片
//! **没有生产调用方**，因此 `Frame::cell_to_stage` 立成 `#[cfg(test)]`，用途只有"与逆方向
//! 配成一对可往返验证的换算"。对照 Wayland：`app.rs:5331-5337` 的指针换算就是
//! `surface.pos + e.position`，而 `e.position` 本来就是逻辑 surface 坐标 ⇒ **那个平台
//! 从头到尾没有这一次乘除**。⇒ macOS 大概率与 Windows 同侧（`NSEvent` 给的是 point，
//! 而 point 在不同 API 上逻辑/物理混着），但**这条要 M6 自己量，不要照抄本段**。
//!
//! ⚠️ **规矩：`window.rs` 是本 crate 唯一点名 `HWND` / `UpdateLayeredWindow` 的文件。**
//! 同一条规矩预先写给 M6 的 `overlay-cocoa/src/ns.rs`。理由不是审美：OS 类型一旦散进
//! 业务模块，那些模块就再也拿不回宿主测试里跑，而"拿不回宿主"意味着下一个 bug 要等
//! 有人把它拷到 Windows 机器上才现形。
//!
//! 这一句的范围要读准，别扩大成"本 crate 只有一个文件碰 OS"：**它钉的是窗口与合成那一族**
//! （`HWND`/`UpdateLayeredWindow`/消息循环/`SetCapture`/DPI 上下文 ⇒ 全在 `window.rs`）。
//! `monitor.rs` 点的是 GDI 显示器结构（`HMONITOR`/`RECT`），`summary.rs` 的两个采集函数点的
//! 是进程计数（`HANDLE`/`FILETIME`）—— 两者都**不碰窗口**，且各自整个待在 `#[cfg(windows)]`
//! 里，所以"业务模块拿不回宿主"这条防线没有被削弱：`summary.rs` 的**格式化**部分照样在
//! Linux 上跑（上表那一行）。⇒ 判"有没有破规矩"的方法是问"这个 OS 类型是不是窗口/合成类的"，
//! 不是数文件名。
//!
//! ## 范围之外（别在这里找）
//!
//! 右键菜单、抽卡卡面、收藏册、配件、托盘、`opet ctl` 控制面 —— 用户 2026-11-21 第 2 条
//! 拍板把它们全排除在本片之外。它们的**命令行入口也没有**，而且不是忘了：[`cli`] 里那九条
//! `REFUSED` 逐条点名理由，绝不"能解析、没人读"（`--cell` 那一族假开关是本项目登记过的债）。

pub mod app;
pub mod cli;
pub mod layout;
pub mod stage;
pub mod summary;
pub mod windx;

#[cfg(windows)]
pub mod monitor;
#[cfg(windows)]
pub mod window;
