//! 各平台 overlay 客户端共用的那一层（设计书 §3.7）。
//!
//! ## 为什么这一层存在，以及它为什么必须零平台依赖
//!
//! [`petcore::render::OverlayBackend`] 的契约是"**内核只给图集格坐标，不经手像素**"（D12）。
//! 于是每个平台客户端各自要做三件同样的事：**把 `sheet.webp` 解码成能直接交给窗口系统的字节
//! 布局**、**按格取像素**、**回答"这一格的哪些像素吃点击"**。这三件事的算法与窗口系统无关，
//! 只有"把它贴上去"那一步有关 —— 所以算法在这里，贴法在各家的 crate 里。
//!
//! 反面理由更硬：这一层若被复制成两份，两份会在**某一天静默分叉**，而分叉的症状画在屏上
//! （边缘多一圈黑、某几格点不着），不会报在编译器上。本项目对"同一件事两个事实源"已经付过
//! 多次学费（`assets/pets/*_192` 那份"同一产物两个目录"、抽卡池与配件库互不相认那一格）。
//!
//! ## 依赖面（由 `tests/no_platform_deps.rs` 钉，不靠自觉）
//!
//! 只有 `petcore` / `serde` / `serde_json` / `image-webp` / `ab_glyph`（纯 Rust 字形栅格，
//! M5 第二批随绘制层进来；外加只在测例里用的 `tempfile` 与 `sha2`（字体资产门禁钉摘要）。
//! 没有 `wayland-*`、没有 `smithay-*`、没有 `calloop`、
//! 没有 `windows-sys`、没有 `objc2`，也没有 `std::os::unix`。⇒ 任何一个平台的客户端都能依赖它，
//! 包括 `x86_64-pc-windows-gnu` 与将来的 macOS 客户端。
//!
//! ⚠️ 上面那份清单的**唯一事实源是测试里的 `ALLOWED`**（`the_census_is_not_empty` 逐字比对
//! 依赖全集），不是这一段话 —— 所以改了 `Cargo.toml` 而没改测试会红，而这一段话漂了不会。
//! 这一段是给人读的，那一条断言是守着的。
//!
//! ## 五个模块各自被谁用（不是"全都用"）
//!
//! - [`atlas`] —— **每个平台都用**。产出的字节布局是**预乘 BGRA8**（小端 = B,G,R,A），
//!   这同时是 Wayland `wl_shm` 的 `Argb8888` 要的、和 Win32 `UpdateLayeredWindow` 带
//!   `AC_SRC_ALPHA` 要的 ⇒ 两个平台都**零转换**接走（这不是巧合：两边要的都是"预乘 32 位小端"）。
//! - [`accounting`] —— **每个平台都用**，用的是同一份的**同一组恒等式**。那句
//!   `核对: 五档合计 N vs tick M` 是跨平台唯一可比的省电读数；两家各写一份 `Counters`
//!   的话，两边报的数不再是同一件事，而且没有任何东西会为此变红。
//! - [`region`] —— **目前只有 Wayland 侧用**。`wl_region` 只接受矩形（协议限制，不是偏好），
//!   所以精确轮廓必须在客户端从 `pack.py` 产的 span 表还原成矩形列表。Windows 侧不需要它：
//!   穿透靠 `WM_NCHITTEST` 当场读**刚交出去那块 buffer 的 alpha 字节**，读的是屏上像素本身，
//!   而 `polygon.json` 是那份像素的预计算近似（同一阈值 `atlas::ALPHA_HIT_THRESHOLD`）。
//!   留着它是因为另一个消费者真的需要它，不是为了"看起来对称"。
//! - [`ui`] —— 2026-11-22（M5 第二批拍板③）起是**菜单/卡面/面板/词表/文字**的唯一实现，
//!   六个模块自 `overlay-wayland` 整搬而来，搬的判据 = 它们头部自带的"零 wayland 类型"约束
//!   与实测依赖面（只有 `petcore` + `ab_glyph`）。`overlay-wayland` 与 `overlay-win32`
//!   画的是**同一份数学**，分叉在结构上不可能。
//! - [`dress`] —— 2026-11-23（M5e）起是**换装那几条判据**的唯一实现（"这一件许不许穿""这句
//!   该不该打""上次穿的这一轮还穿不穿"），与 [`accounting`] 的 `classify_tick` 是同一条先例：
//!   纯决策上提，IO 与像素留在各家。落库那一步（`write_equip`/`clear_equip`）**没有**搬 ——
//!   它要 `petstore`，而这一层的立身之本是不碰任何带状态的层；代价（"演示态不写"有两份实现）
//!   明写在 `dress.rs` 头部。

pub mod accounting;
pub mod action_map;
pub mod atlas;
pub mod audio;
pub mod dress;
pub mod region;
pub mod ui;

/// 再导出：搬进来的六个模块内部互引全走 `crate::menu::…` 这类**根路径**，把它们钉在 crate
/// 根就等于**一个引用都不用改**（与 [`atlas`]/[`region`] 上提时 wayland 侧的同款手法是一对
/// 镜像）。`settings` 也在其中：各家客户端自己的 `settings.rs` 只保留优先级链。
pub use ui::{card, font, l10n, menu, panel, settings, text};
