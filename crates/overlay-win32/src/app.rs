//! 运行参数与进程主体。
//!
//! ## 为什么 Windows 那一整块收进 `#[cfg(windows)] mod win`，而不是散在顶层
//!
//! 顶层一旦直接 `use crate::window::Client` / `use petcore::sim::Sim`，宿主构建就会报
//! **未使用导入**（`window` 那个模块在非 Windows 上根本不存在，而 `Sim` 在桩里没人碰）。
//! 本项目要求 `cargo clippy --all-targets` 零 warning ⇒ 只能整块收进一个带 cfg 的子模块，
//! 让"宿主看不见的代码"在类型系统层面也不存在。逐条 `#[cfg(windows)]` 是同一件事的更坏写法：
//! 它会漏，而漏了的症状是"Linux 上编不过了"，不是"这里写错了"。
//!
//! 宿主上剩下的只有 [`Options`] 与 [`run`] 的那一个桩，而桩必须是**会说真话**的那种：
//! 报错里带 [`Options::describe`]，这样"解析对了但没接线"与"参数打错了"在 stdout 上不同形。
//!
//! 为什么这两个桩要读遍 [`Options`] 的每个字段（见 [`Options::describe`]）：不读的话编译器
//! 会报 `field is never read`，那条警告是**真的**在说"参数一路解析对了、最后没人看"——
//! 本项目对这件事的形状有个名字，叫假开关（`--cell` 那一族）。让它先响在编译期。

use std::path::PathBuf;

/// 一次运行的全部输入（由 [`crate::cli`] 解析并做完所有语义拒绝后交过来）。
///
/// 与 Wayland 那份 `app::Options` **有意不同形**：少了 `no_cursor` / `ipc`（本片的范围，
/// 理由逐条在 [`crate::cli::REFUSED`]），
/// 而 [`Options::panel_top`] / [`Options::panel_bottom`] 从 `f32` 变成了 `Option<f32>`。
/// 那一条不是整理，是**语义换档**，见字段注释。
/// （`db` 曾经也在"少了"那一列，第二批 M4 起它转真了 —— 但形状仍与 Wayland 不同：
/// 那边不给 = 内存库 + DRYRUN 水印，这边不给 = **兜底档**，见 [`Options::db`]。
/// `allow_unowned_accessories` 在 M5e 转真，`allow_unowned_pets` 在 M7d 随换本体转真，
/// `lang` / `motion` 在 **M7e** 随配置链转真 ⇒ 这一族只剩 `ipc` 还欠着，而它欠的不是开关，
/// 是"Windows 侧的控制面传输怎么设计"那一整格。）
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// `pet.json` 路径；与 [`Options::test_rect`] 二选一（both / neither 由 `cli` 那一层拒掉）。
    pub manifest: Option<PathBuf>,
    /// 纯色矩形档：不读资产、不接内核，只验"窗口能否出现 + 逐点穿透是否成立"。
    ///
    /// ⚠️ **第 8 步接线时更正了这里原先那句话**（它当时写的是"它是
    /// `OverlayBackend::set_passthrough` 的真消费者"）：本平台**不实现**那条 trait，
    /// 这一档也就不消费 `set_passthrough`。理由不是遗漏，是平台形状：
    /// - `set_passthrough(bool)` 是**整窗**开关（设计书 §3.7 给 Win/mac 的那条近似），
    ///   而本平台有精确的那一条 —— `WM_NCHITTEST` 逐点同步回答（[`crate::window::Events::hit`]）。
    ///   有精确机制时再挂一个 `WS_EX_TRANSPARENT` 整窗开关，得到的只是"整块窗口忽男忽女地
    ///   吞或放点击"，比现状严格更差，而且没有任何调用者会用它。
    /// - 同一条 trait 还要求 `on_pointer(mpsc::Sender<PointerEvent>)` **异步**送指针事件，
    ///   而命中判定必须**当场**返回值（`WM_NCHITTEST` 的 `LRESULT` 就是答案）。异步队列满足不了
    ///   这个同步签名 —— 那一格要等 §3.7 那条"trait 与本平台不匹配"的回写来改设计，不是这一轮
    ///   硬塞一个实现。
    ///
    /// 所以这一档现在只答两个问题：窗口映射上了吗、剪影外点得穿吗。
    pub test_rect: bool,
    /// `--db` 的**原始字符串**（与 Wayland 同型：`Option<String>`，绝对性由 petstore 判，
    /// 不在解析层猜）。两个读取者，各办各的事：
    /// - 启动：[`petstore::paths::DbPath::resolve`] 第一档 ⇒ `Store::open_at` **开了即关**
    ///   —— 证明这套 schema 能在 Windows（交叉编译产物 + Wine 先验）上首次建库（M4 的冒烟）；
    /// - UI 那一刻：`ensure_gacha` → [`overlay_report::Gacha::connect`] 的**惰性真连接**，活着到退出，
    ///   余额、（M5e 起）换装读的那几行、（M6 起）抽取都走它 —— 启动那一发不代替它，那一发只证 schema。
    ///
    /// 不给 ⇒ 打印兜底档实解位置（`%LOCALAPPDATA%\opet\opet.db` 那一条链），**一个字节都不写**，
    /// UI 侧退成内存库 + DRYRUN 水印（Wayland 那侧同型）。
    ///
    /// ⚠️ 第二批 M5c 起 `App` 里有了 `gacha: Option<Gacha>`，但**仍然不是启动就持库**：
    /// 没开过菜单的 3 秒矩形档连一次库都不该连（硬约束 5 的事件驱动口径）。M5e 的启动恢复
    /// 是**第二个**惰性触发点（带 `--db` 的正常那一轮，第一轮就连），矩形档仍然不连 ——
    /// 判据在 [`App::restore_worn`] 那三条早退里，不在这里。
    pub db: Option<String>,
    /// 到点自毁的秒数；`None` = 一直跑（`--seconds 0`）。
    ///
    /// **缺省是 `Some(3.0)` 而不是 `None`**：live 桌面上手工敲起来的一只宠物不许赖着不走
    /// （Wayland 侧同一条规矩，边界见 `AGENTS.md`「live 会话的硬边界」）。
    pub seconds: Option<f32>,
    /// 显示器名（GDI 设备名，字面量形如 `\\.\DISPLAY1`，大小写不敏感）；`None` = 主屏。
    pub monitor: Option<String>,
    /// 顶部留白的**覆盖值**。
    ///
    /// ⚠️ 与 Wayland 那两个旗的语义不同，而且这个不同是平台给出来的：
    /// layer-shell **读不到** exclusive zone（`wl_output`/`xdg_output` 只给整块几何），
    /// 所以那边 `--panel-top N` 是在"量不到的 0"上**加**出 N；
    /// Win32 这边 `rcWork.top - rcMonitor.top` 直接就读得到任务栏留的高，
    /// 于是给值 = **覆盖**现量，不给 = 用现量。
    ///
    /// 一个 `Option` 就把这两种情况分开了；把它压成 `f32`（默认 0）会让"我要 0 留白"与
    /// "你去量"变成同一个数，而后者才是绝大多数情况 —— 那条旗也就退化成 `--cell` 那种
    /// 买了零东西的开关。启动行必须**打印谁赢了**。
    pub panel_top: Option<f32>,
    /// 底部留白的覆盖值，同 [`Options::panel_top`] 那条规则。
    ///
    /// 现量在这里**不够**：`SPI_GETWORKAREA` 只反映"占据工作区的停靠栏"，第三方 dock
    /// 常不占（设计书 §3.7 那条"底部遮挡要显式给"在 Windows 上同样成立）。
    /// 所以这一格是"现量打底、旗覆盖"，两个来源都真。
    pub panel_bottom: Option<f32>,
    /// 行为内核的种子。同种子必须逐位重放（硬约束 #8）。
    pub seed: u64,
    /// `--lang` 的**原始字符串**（M7-e 转真）。解释不在这里：合法性由
    /// [`crate::settings::validate_cli`] 判（`cli.rs` 据此报错退出），语义由
    /// [`overlay_common::settings`] 那条唯一的链判（`--lang` > `config.toml` > 环境变量 > 默认）。
    ///
    /// ⚠️ 类型是 `Option<String>` 而**不是** `Lang`：解析层一旦归一化成枚举，`None`（"让下一级
    /// 说话"）和`Some(zh)`（"我显式说了中文"）就合并成同一个值，config.toml 与环境变量两级
    /// 永久失效。原样交到底，是这条链能存在的前提。
    pub lang: Option<String>,
    /// `--motion` 的**原始字符串**，同 [`Options::lang`] 那两条理由（三档 full/reduce/off）。
    pub motion: Option<String>,
    /// `--allow-unowned-accessories`：把"这一件我在库存里有没有"那道闸门整个放开。
    ///
    /// 第二批 M5e 起它转真了（消费者 = [`overlay_common::dress::DressView::select`] 的
    /// `allow_all` 那一参，走 [`overlay_common::dress::dress_exempt_for`]）。
    /// 开 = 换装页把库里**扫到的每一件**都放出来（含从未抽到的），屏上那行 `*` 提示仍在；
    /// 不给 = 未验过的配件一律不进可穿集合（失败闭合）。
    ///
    /// ⚠️ 与 `dry_run` 的组合由 `dress_exempt_for` 一处判：**无 `--db` 时这一位不生效**
    /// （内存库里根本没有"拥有"这件事可查），不是"打开了但看不出来"。
    pub allow_unowned_accessories: bool,
    /// M7-d 转真：豁免收藏册里**宠物那一行**（未拥有也当场可换）。
    ///
    /// ⚠️ 它与 [`Options::allow_unowned_accessories`] **是两个开关而判据共用一条函数**
    /// （[`overlay_common::dress::dress_exempt_for`]）：拍板④ 说得很清楚 —— 配件那一档在帽子页
    /// 标题打了 `*`，而本体这一档**故意没有屏上记号**（那一页的列表里没有宠物，把记号合并过去
    /// 必然出现"标打了而没放行"）。所以两条不许合成一条。
    ///
    /// 无 `--db` 时它同样不生效（内存库没有"拥有"可查 ⇒ 一切照旧放行），判据仍在那一处。
    pub allow_unowned_pets: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            manifest: None,
            test_rect: false,
            db: None,
            // 与 Wayland 那份同值：不留"默认无限跑"这个选项给手工调用。
            seconds: Some(3.0),
            monitor: None,
            panel_top: None,
            panel_bottom: None,
            // 与 Wayland 那份同值（含"为什么是 24"那条注释，见 overlay-wayland/src/app.rs）
            // ⇒ 两平台裸跑看到的是同一齣戏。
            seed: 24,
            lang: None,
            motion: None,
            allow_unowned_accessories: false,
            allow_unowned_pets: false,
        }
    }
}

impl Options {
    /// 一行把十二个字段全打出来，供宿主桩与 Windows 的启动行共用。
    ///
    /// 它存在的第二个理由更硬：**这里读到每一个字段** ⇒ 谁加了第十三个字段而忘了在下面列出来，
    /// 只有 [`crate::cli`] 那条平权测试会红（`Options` 多一个字段而 `parse` 不产它 = 那个字段
    /// 恒 `None` = 屏上有东西没人能开）。结构体字面量在这里是**全字段**的，所以漏字段会在
    /// 编译期炸 —— 那是比测试更快的一道。
    pub fn describe(&self) -> String {
        let yes_no = |b: bool| if b { "是" } else { "否" };
        format!(
            "manifest={:?} 矩形档={} 存档={:?} 秒数={:?} 显示器={:?} 顶部留白={:?} 底部留白={:?} 种子={} 语言={:?} 动效={:?} 未拥有配件放行={} 未拥有本体放行={}",
            self.manifest,
            yes_no(self.test_rect),
            self.db,
            self.seconds,
            self.monitor,
            self.panel_top,
            self.panel_bottom,
            self.seed,
            self.lang,
            self.motion,
            yes_no(self.allow_unowned_accessories),
            yes_no(self.allow_unowned_pets),
        )
    }
}

/// 起一轮 Windows overlay。
#[cfg(windows)]
pub fn run(options: Options) -> Result<(), String> {
    win::run(options)
}

/// 宿主（Linux/macOS）上这坨代码只参与编译与测试，不参与运行。
///
/// ⚠️ 真话比"未实现"三个字有用：交叉编译出来的 `.exe` 会在 Windows 上走另一个桩，
/// 而**在这台机器上**跑 `cargo run -p overlay-win32` 的人需要知道的是"你脚下没有 Windows"，
/// 不是"这个功能坏了"。
#[cfg(not(windows))]
pub fn run(options: Options) -> Result<(), String> {
    Err(format!(
        "Windows overlay 只能跑在 Windows 上（当前构建目标不是 Windows）；已解析到的参数：{}",
        options.describe()
    ))
}

// ───────────────────────────── Windows 事件环 ─────────────────────────────

#[cfg(windows)]
mod win {
    use std::time::{Duration, Instant};

    use petcore::assets::{Manifest, default_manifest};
    use petcore::behavior::config::{Action, BehaviorConfig};
    use petcore::behavior::machine::Command;
    use petcore::geom::Vec2;
    use petcore::intent::{PresentationIntent, SURFACE_MARGIN};
    use petcore::sim::{Sim, TICK_HZ};
    use petcore::world::{Monitor, World, WorldEvent};

    use overlay_common::accounting::{Counters, Source, Tick, attribute_of, classify_tick};
    use overlay_common::atlas::{ALPHA_HIT_THRESHOLD, AtlasSet};
    use overlay_common::dress::{
        DressView, PickWear, dress_exempt_for, plan_pick_wear, plan_restore, split_body_row,
    };
    use overlay_common::menu::{self, CARD_H, CARD_W, MenuAnim, MenuItem, RectI};
    // 面板那一族只引命名空间与四个类型名：`PanelItem` 的各臂一律写成 `panel::PanelItem::X`
    // （与 Wayland 母本同形，也是为了分派臂上"这一格属于共用层"这件事在源码里可读）。
    use overlay_common::panel::{self, CollectionTab, PanelKind, PanelRow, RowRect};
    use overlay_common::{card, l10n::Lang, settings::Motion};

    use crate::dress::{
        Candidate, WornWrite, accessory_root, clear_equip, decode, discover_accessories,
        start_body_id, write_equip,
    };
    // `Gacha` 在 M7-b0 上提到 `overlay-report`（两家同一份，判据不许有第二个实现）⇒ 这里
    // 不再从 `crate::dress` 拿它。
    use crate::layout::{
        Frame, Insets, PhysMon, describe_inset, kernel_monitor, resolve_insets, to_phys,
    };
    use crate::settings;
    use crate::stage::{Stage, Worn};
    use crate::summary;
    use crate::window::{Client, DpiPath, Events, SurfaceKind, TimerPath, Wake, set_dpi_awareness};
    use overlay_report::{
        BodyCandidate, BodyChoice, Gacha, SwapBody, album_rows, db_body_row, discover_pets,
        pet_root, plan_body_override, plan_swap_body, published_rates_lines,
    };

    use super::Options;

    /// 补帧预算：与 Wayland 同一个数（`overlay-wayland/src/app.rs:97`）。
    ///
    /// 为什么要有上限：累积器一旦允许"落后多少就补多少"，一次 3 s 的卡顿会连着跑 180 个
    /// tick 并且每个都提交 —— 那既是硬约束 5 的 CPU 线直接爆掉，也是"宠物瞬移"的症状。
    const MAX_CATCHUP: u32 = 3;

    /// 双击折叠的时间窗与半径，**与 Wayland 逐字同值**（`app.rs:99-100` 的 320 ms / 24 逻辑 px）。
    ///
    /// 同值不是对齐好看：`Command::DoubleTap` 是确定性输入的一部分，而两平台必须从同一次
    /// 真实点击里折叠出同一条 `Command` 序列 —— 自 §3.5-j 起这条序列是**双击 = `Unpin` 后
    /// `DoubleTap`、单击按下 = `DragStart`**（见 `on_press` 的 dbl 臂），三枚指令的先后与
    /// 归属必须两平台逐字一致。半径是**逻辑**像素（内核坐标），不是物理像素。
    const DOUBLE_TAP_GAP: Duration = Duration::from_millis(320);
    const DOUBLE_TAP_RADIUS: f32 = 24.0;

    /// `MsgWaitForMultipleObjectsEx` 的等待上限。**它是保险丝，不是轮询周期**。
    ///
    /// 真正的 60 Hz 步进源是 `window.rs` 那条 waitable timer（`Wake::Timer`）。这一档存在的
    /// 唯一理由是"定时器哪天不来了"：`TimerPath::Fallback` 那条路上唤醒粒度是别人的进程
    /// 能改的系统定时器精度，而硬约束 5 说"事件驱动，禁止定时轮询"说的是**不许按表查状态**
    /// （库、compositor、显示器），不是"等待必须有超时"。写 `INFINITE` 的形状很具体：
    /// 定时器一停 = 宠物静止而进程还活着，读起来正好像"省电生效了"。
    const WAIT_CAP_MS: u32 = 1_000;

    /// 矩形档的固定落点（舞台内逻辑偏移），与 Wayland 的 `test_rect_frame` 同一个数。
    const RECT_AT: f32 = 200.0;

    /// 菜单面的全部业务状态（对齐 Wayland 的 `MenuState`，但**没有协议对象**）。
    ///
    /// 不在场的字段与在场的字段同样有意：没有 `frame_pending`/`pending_since`/看门狗
    /// （分层窗口是同步的、没有在飞帧回调 ⇒ 那一族在本平台结构上不可达，与 `classify_tick`
    /// 恒传 `frame_pending=false` 同一条理由）；没有 `configured`（窗口尺寸是自己定的，
    /// 没有"compositor 答你一个尺寸"这一步）。留下来的就三样：
    /// **命中表**（`items`）、**内容闸门**（`last_sig`/`force_draw`）、**屏上像素**（`canvas`）——
    /// 最后一样的双重身份是这一面的关键：它既是交给 `UpdateLayeredWindow` 的 buffer，
    /// 又是 `hit` 逐点读的那份 alpha ⇒ "屏上有什么才点得着什么"在本面与宠物面同一条数学。
    struct MenuState {
        items: Vec<menu::ItemRect>,
        page: menu::MenuPage,
        anim: MenuAnim,
        /// 开菜单那一刻读的余额（D14 的事件驱动口径：余额不是每 tick 查库）。
        balance: i64,
        dry_run: bool,
        motion: Motion,
        lang: Lang,
        /// **开发态豁免**记号（帽子页标题那个 `*`）。M5e 起它的右值就是 [`App::dress_exempt`]
        /// —— 与 Wayland 同一处点亮时机（开菜单那一刻）。M6 起卡面上屏，那个 `*` 真的画得出
        /// 来了（`card::Ctx::unowned_exempt` 在 M5c 就接对，M6 没有重画这一族）。
        dress_mark: bool,
        /// `Some(_)` = 上一帧上屏内容的签名。签名相等且没强制 ⇒ **一个 present 都不发**
        /// （静止菜单恒 0 提交是这块面的省电判据，`counters.menu_commits` 不随 tick 增长）。
        last_sig: Option<u64>,
        force_draw: bool,
        /// 左上角，**舞台局部逻辑**坐标 = `menu::placement` 的输出，打开后不再挪
        /// （与 Wayland 的 margin 同口径；DPI 变更也不重算摆位，只换像素尺寸）。
        at_logical: (i32, i32),
        /// 上一次绘制时的生效缩放。与 `App::scale` 不一致 ⇒ 换窗口尺寸 + 重建 canvas + 重画
        /// （Wayland 那侧的 `mirror_scale` 臂，本平台多一道物理尺寸的换档）。
        scale: f32,
        /// 物理窗口/buffer 尺寸（`CARD_W/H × scale`，与 `canvas`、窗口三者同生共死）。
        phys: (i32, i32),
        canvas: Vec<u8>,
    }

    impl MenuState {
        /// 建状态。**固定从盲盒页起**（照 Wayland：「右键就想要抽卡」），不记住上一次翻到哪 ——
        /// 真记住就成了一个既不进签名也不进持久化的运行中漂移源。
        fn new(
            at_logical: (i32, i32),
            scale: f32,
            balance: i64,
            dry_run: bool,
            motion: Motion,
            lang: Lang,
            dress_mark: bool,
        ) -> Self {
            let page = menu::MenuPage::Gacha;
            let phys = (
                to_phys(CARD_W as f32, scale).max(1),
                to_phys(CARD_H as f32, scale).max(1),
            );
            Self {
                items: menu::rects(page),
                page,
                anim: MenuAnim::Menu,
                balance,
                dry_run,
                motion,
                lang,
                dress_mark,
                last_sig: None,
                force_draw: true,
                at_logical,
                scale,
                phys,
                canvas: vec![0u8; (phys.0 * phys.1 * 4) as usize],
            }
        }

        /// 菜单局部**物理**像素 → 这一点有没有我们自己的不透明像素。
        ///
        /// 与宠物面同一个 `ALPHA_HIT_THRESHOLD`、同一句"屏上像素是唯一真值"：卡片像素的
        /// 圆角由 `card::paint` 清成透明 ⇒ 穿透在菜单上天然逐像素，不需要第二条规则。
        fn hit(&self, x: i32, y: i32) -> bool {
            let (w, h) = self.phys;
            if x < 0 || y < 0 || x >= w || y >= h {
                return false;
            }
            let idx = ((y * w + x) * 4 + 3) as usize;
            u32::from(self.canvas[idx]) > ALPHA_HIT_THRESHOLD
        }

        /// 菜单局部物理像素 → 局部**逻辑**坐标（`menu::press` 读的口径 = 命中表的口径）。
        ///
        /// ⚠️ 这条除法是 Windows 独有的一步：Wayland 的指针事件直接给逻辑坐标，本平台
        /// 窗口层交回来的是物理像素（`window.rs` 模块头"这一层不做任何缩放猜测"的另一半）。
        fn local_logical(&self, px: i32, py: i32) -> (f32, f32) {
            (px as f32 / self.scale, py as f32 / self.scale)
        }

        /// 推进一格动画。返回 `true` = 相位变了（像素必然不同）。照 Wayland 同名方法。
        fn step(&mut self) -> bool {
            if !self.anim.is_busy() {
                return false;
            }
            let next = menu::advance(&self.anim, self.motion);
            let changed = next != self.anim;
            self.anim = next;
            changed
        }

        /// 出一帧卡片。返回是否真的 present 上屏了（调用方据此记 `menu_commits`）。
        ///
        /// 与 Wayland `MenuState::draw` 的差别只有两处，且都是平台形状不是判据：
        /// 少了 `!configured || frame_pending` 的前置（这两档在本平台不存在），
        /// buffer 从 shm 换成自己那块 `canvas`（`card::paint` 自己 `fill(0)`，
        /// 圆角外保持透明 ⇒ 上面的 `hit` 读的就是这一次画的像素）。
        fn draw(&mut self, client: &mut Client, at: (i32, i32), error: &str) -> bool {
            let sig = menu::content_sig(
                &self.anim,
                self.balance,
                error,
                self.lang,
                self.page,
                self.motion,
            );
            if self.last_sig == Some(sig) && !self.force_draw {
                return false;
            }
            let (bw, bh) = self.phys;
            card::paint(
                &mut self.canvas,
                bw,
                bh,
                (bw * 4) as usize,
                &self.anim,
                card::Ctx {
                    balance: self.balance,
                    dry_run: self.dry_run,
                    error,
                    scale: self.scale,
                    motion: self.motion,
                    lang: self.lang,
                    page: self.page,
                    unowned_exempt: self.dress_mark,
                },
            );
            match client.present(SurfaceKind::Menu, at, &self.canvas) {
                Ok(()) => {
                    self.last_sig = Some(sig);
                    self.force_draw = false;
                    true
                }
                Err(e) => {
                    // 像素**没**上屏 ⇒ 不记 last_sig、留 `force_draw` 让下一轮重投
                    // （与宠物面 present 失败同一处置：静默跳过的症状是"菜单停在旧内容"，
                    // 而那与"内容本来就没变"在屏上同形）。
                    eprintln!("menu: present 失败: {e}");
                    false
                }
            }
        }
    }

    /// 面板面（M7b）= 第三块 surface 的业务状态，[`MenuState`] 的同形物。
    ///
    /// **去掉的字段与在场的字段同样有意**（判据本体在 Wayland 那份 `PanelState` 的模块头，
    /// 这里是同一套理由的第二份实现，不是第二个说法）：没有 `anim`（这一面没有开箱相位
    /// ⇒ `card::paint_panel` 压根不接 `&MenuAnim`）、没有 `page`（"翻页"是 [`PanelKind`] 里
    /// 那个 tab，而且只有收藏册有）、没有 `balance`（余额只留在菜单 footer 那一行：同一次
    /// 会话里同一个数印两处 = 两个事实源，而两处的刷新时刻还不一样）、**`motion` 有意不是
    /// 字段**（[`panel::settings_rows`] 已经把 `motion.code()` 烘进行文案，而行文案进签名
    /// ⇒ 再存一份平行字段就是"字段存在、签名不读它、画师也不读它"那一族漂移源）。
    ///
    /// 本平台还额外没有 Wayland 那八项协议/在飞帧字段（`layer`/`surface`/`viewport`/`pool`/
    /// `frame_pending`/`pending_since`/`active_region`/`configured`）：前四项在分层窗口这边
    /// 塌成 `Client` 注册表里的一扇窗 + 一块自己 `alloc` 的 `canvas`，后四项是"等 compositor
    /// 的帧回调回执"那一族，在同步 `UpdateLayeredWindow` 上**结构上不可达**（与 `classify_tick`
    /// 恒传 `frame_pending=false` 同一条理由）。所以这一面**没有看门狗**：`panel_watchdogs`
    /// 在本平台恒 0 是平台形状，不是"这一面没在跑"。
    struct PanelState {
        /// 现在哪一本（收藏册第几 tab / 设置）。它决定标题带写什么字、点标题带是什么动作
        /// ⇒ 既进签名，也被画师读到。
        kind: PanelKind,
        /// 这一面的内容。**唯一的一份**：签名比它、命中表由它派生、像素画它，三者同源。
        rows: Vec<PanelRow>,
        /// 命中表 = `panel::rects(kind, rows)`，与 `kind`/`rows` **同生同改** —— 写它们的
        /// 只有 [`PanelState::new`] 与 [`PanelState::set_lines`] 两处，两处都一次改齐三个字段。
        /// 分成两条路径改（"换 tab 只改 kind"、"换行只改 rows"）就是 `69fd158` 那一族：
        /// 点的是这一屏的行、画的是另一屏的字。
        items: Vec<RowRect>,
        /// 从 [`App::lang`] 抄来。与 [`MenuState::lang`] 同族：运行中可变 ⇒ 进签名、被画师读到。
        lang: Lang,
        /// 上一次 present 的内容签名 ⇒ 静止时**一个 present 都不发**（`panel_commits` 不随
        /// 轮次增长是这一面唯一的省电判据，而它与 `menu_commits` 必须分开读）。
        last_sig: Option<u64>,
        force_draw: bool,
        /// 左上角，**舞台局部逻辑**坐标 = `panel::placement` 的输出，打开后不再挪（同 [`MenuState`]）。
        at_logical: (i32, i32),
        /// 上一次绘制时的生效缩放；与 [`App::scale`] 不一致 ⇒ 换窗口尺寸 + 重建 canvas + 重画。
        scale: f32,
        /// 物理窗口/buffer 尺寸（`panel::CARD_W/H × scale`，与 `canvas`、窗口三者同生共死）。
        phys: (i32, i32),
        canvas: Vec<u8>,
    }

    impl PanelState {
        /// 建状态**并算好那一屏的一切**（行由调用方给：这一条不碰库、不读行，见
        /// [`App::open_panel`] 与 [`App::show_panel`] 的分工）。
        fn new(
            at_logical: (i32, i32),
            scale: f32,
            kind: PanelKind,
            rows: Vec<PanelRow>,
            lang: Lang,
        ) -> Self {
            let phys = (
                to_phys(panel::CARD_W as f32, scale).max(1),
                to_phys(panel::CARD_H as f32, scale).max(1),
            );
            Self {
                kind,
                items: panel::rects(kind, &rows),
                rows,
                lang,
                last_sig: None,
                force_draw: true,
                at_logical,
                scale,
                phys,
                canvas: vec![0u8; (phys.0 * phys.1 * 4) as usize],
            }
        }

        /// 面板局部**物理**像素 → 这一点有没有我们自己的不透明像素（与 [`MenuState::hit`]
        /// 同一个阈值、同一句"屏上像素是唯一真值"）。
        fn hit(&self, x: i32, y: i32) -> bool {
            let (w, h) = self.phys;
            if x < 0 || y < 0 || x >= w || y >= h {
                return false;
            }
            let idx = ((y * w + x) * 4 + 3) as usize;
            u32::from(self.canvas[idx]) > ALPHA_HIT_THRESHOLD
        }

        /// 局部物理像素 → 局部**逻辑**坐标（`panel::press` 读的是命中表口径 = 逻辑）。
        fn local_logical(&self, px: i32, py: i32) -> (f32, f32) {
            (px as f32 / self.scale, py as f32 / self.scale)
        }

        /// 原地换这一屏：`kind`、`rows`、`items`、`lang` **一次改齐**。
        ///
        /// 不置 `force_draw`：`kind`、`lang`、每行文本、错误行全在 [`panel::content_sig`] 里
        /// ⇒ 签名自己就认得出变了（Wayland `show_panel` 那条纪律原样，那里连"换 tab"和
        /// "从菜单进来"都合成这一条，就是为了不给 `items` 留第二条写路径）。
        fn set_lines(&mut self, kind: PanelKind, rows: Vec<PanelRow>, lang: Lang) {
            self.items = panel::rects(kind, &rows);
            self.kind = kind;
            self.rows = rows;
            self.lang = lang;
        }

        /// 出一帧面板。返回是否真的 present 上屏了（调用方据此记 `panel_commits`）。
        ///
        /// **没有 `step()`**：这一面没有任何随时间自己变的像素（相位表在菜单那一面），
        /// 所以"每轮都投一帧完全相同的 buffer"在这里连形状都不存在 —— 签名里没有随时间的量。
        fn draw(&mut self, client: &mut Client, at: (i32, i32), error: &str) -> bool {
            let sig = panel::content_sig(self.kind, self.lang, error, &self.rows);
            if self.last_sig == Some(sig) && !self.force_draw {
                return false;
            }
            let (kind, lang) = (self.kind, self.lang);
            let (bw, bh) = self.phys;
            // `scale` 先取：下面 `self.canvas` 是可变借用，同一个表达式里不能再借 self
            let scale = self.scale;
            card::paint_panel(
                &mut self.canvas,
                bw,
                bh,
                (bw * 4) as usize,
                card::PanelCtx {
                    kind,
                    lang,
                    rows: &self.rows,
                    error,
                    scale,
                },
            );
            match client.present(SurfaceKind::Panel, at, &self.canvas) {
                Ok(()) => {
                    self.last_sig = Some(sig);
                    self.force_draw = false;
                    true
                }
                Err(e) => {
                    // 与 [`MenuState::draw`] 同一处置：像素**没**上屏 ⇒ 不记 last_sig、
                    // 留 force_draw 让下一轮重投（静默跳过的症状"面板停在旧内容"与
                    // "内容本来没变"在屏上同形）。
                    eprintln!("panel: present 失败: {e}");
                    false
                }
            }
        }
    }

    /// [`App::pending_panel`] 的载荷。**只有两个变体**，因为它们要办的只有"窗口动不动"：
    /// 面板已经开着时的换行**不进这块旗**（那一步不需要 `Client`，回调里就地改完，见
    /// [`App::show_panel`]），所以 `Some(Show)` 的出现条件恰好是"面板此刻没开"
    /// ⇒ 旗与状态不会各持一份"这一面该显示什么"。
    enum PanelRequest {
        Show {
            kind: PanelKind,
            rows: Vec<PanelRow>,
        },
        Close,
    }

    /// 一轮运行的全部业务状态。
    ///
    /// ⚠️ **`Client` 不是这里的字段**，这一条不是风格而是 UB 边界：`Client::pump` 要的
    /// `&'static mut dyn Events` 只能由 `App` 的堆地址形成一次（线性借用，`pump` 返回时随
    /// `Slot::ev` 一起放掉）。如果 `Client` 在 `App` 里，那一轮就是
    /// `client.pump(&mut *app_raw)` —— **同一个对象上两个重叠的 `&mut`**，而它不会当场炸，
    /// 它会先写坏派发期间被读的字段。所以 `Client` 由 [`run`] 拥有，`App` 的方法一律按
    /// 参数收 `&mut Client`。
    ///
    /// 同一条纪律的第二个推论：[`Events::dpi_changed`] **只登记** `pending_scale`，
    /// 不在回调里重建 `Frame`/`Stage`/窗口 —— 派发发生在 `pump` 内部，那时环外没有任何
    /// `&mut Client` 能安全拿到。重算由事件环在 `pump` 返回之后做（[`App::apply_dpi`]）。
    struct App {
        opts: Options,
        /// 当前本体的清单。**M7d 起它是字段而不是启动局部变量**，因为换本体之后有两处要现读它：
        /// [`App::swap_body`] 的 `same_grid` 那道（比的是**身上这一只**的档，不是兜底那一只的）与
        /// 第 6 步重跑 `discover_accessories` 前要拿到新清单。
        ///
        /// `&'static` 不是风格：`Sim<'static>` 借的正是同一份（`load_manifest` 那里说明为什么必须
        /// leak），而 `BodyCandidate::leaked` 返的也是 `&'static` ⇒ 换本体 = 换一个 `'static`，
        /// 不需要重排任何生命周期。
        manifest: &'static Manifest,
        /// 本体的**全部像素**：N 张 sheet 的集合（DESIGN §6.1.27 裁决 1 = 行号全局连续、
        /// 分段只是存放）。取格只经 [`AtlasSet::blit`]，段号由 [`Manifest::locate`] 现算 ⇒
        /// 这里不可能出现"清单说 40 行而手里只有第 2 张"的半只宠物。
        atlas: Option<AtlasSet>,
        /// 行为内核。**它借的那份清单是 `Box::leak` 出来的 `&'static Manifest`**（`load_manifest`
        /// 那里说明为什么必须 leak），M7d 起换本体走 [`Sim::swap_manifest`] ⇒ 这份 `'static`
        /// 必须同时记在 [`App::manifest`]，否则下一次重跑配件发现时无处可取。
        sim: Option<Sim<'static>>,
        stage_buf: Stage,
        frame: Frame,
        /// 图集格尺寸，单独留着：换缩放时 `Frame` 要按它重建。
        ///
        /// M7d 之后 [`App::manifest`] 也存着，这一格**不是**它的第二个事实源：换本体那道闸门
        /// 保证了 `cell` 与 `manifest.grid` 永远同步（不同档的直接不换，见
        /// [`overlay_report::plan_swap_body`] 的 `same_grid`），所以这里读的永远是身上这一只的档。
        cell: (u32, u32),
        /// 全部物理显示器 + 舞台那台的名字。DPI 变更后要靠它们重算内核几何。
        mons: Vec<PhysMon>,
        stage_mon: PhysMon,
        insets: Insets,
        scale: f32,
        dpi_path: DpiPath,

        ticks: u64,
        counters: Counters,
        /// 当前这一批提交归属于谁（只有 `Source::Tick` 占五档）。
        source: Source,
        started: Instant,
        last_wake: Instant,
        deadline: Option<Instant>,
        acc: f32,
        force_draw: bool,
        drawn_cell: Option<(u32, u32)>,
        last_intent: Option<PresentationIntent>,
        /// 上一次交给 `UpdateLayeredWindow` / `SetWindowPos` 的位置（物理），用来判"只移动"。
        last_at: (i32, i32),

        // ── 指针（`Events` 的那几个回调是唯一写入点）──
        drag: bool,
        grab_offset: Vec2,
        last_press: Option<(Instant, Vec2)>,
        pointer_world: Vec2,
        pending_scale: Option<f32>,

        // ── UI（M5c 菜单面 + M7b 面板面）──
        /// 第一次开菜单才连（[`App::ensure_gacha`]），连上就活着到退出。
        gacha: Option<Gacha>,
        /// "最近一次失败"那一行（进 `content_sig`，屏上错误行的唯一来源；成功路径负责清空）。
        ui_error: Option<String>,
        /// 生效值 + 各自的**来源层**（M7e 从"两个裸值"换成这一格）。
        ///
        /// 为什么存整条 [`settings::Settings`] 而不是 `lang` / `motion` 两个字段：启动行、
        /// 退出摘要、还有 `--lang` 那一句"点了不会写回"都要说**这一档是从哪一层来的**，
        /// 而 `lang_from` 只有和 `lang` 同处一格才不会说谎（Wayland 母本同一条手艺）。
        /// 链本体在 `overlay-common`，这里存的只是它一次算完的结果 ⇒ 屏上永远只有一个真相，
        /// 不存在"变量一份、文件一份"（`AGENTS.md` 现场事实第 16 条那一族）。
        ///
        /// ⚠️ 菜单与面板里的 `m.lang` / `p.lang` 是**副本**，不是第二个事实源：那两个类型
        /// 要按"自己这一屏的像素"出图，而像素的语言必须与它被画那一刻生效的语言一致
        /// （`baked_lines_keep_the_language_they_were_pulled_in` 同族）。换档走
        /// [`App::apply_lang`]，它一次改齐三处（生效值、菜单副本、面板重算）。
        ui: settings::Settings,
        /// 演示态 = 无 `--db`（Wayland 同型：内存库 + 卡片右下角 DRYRUN 水印的判据）。
        dry_run: bool,

        // ── 换装（M5e）──
        /// 启动扫盘交出来的那张表（**元数据，一像素未解**；下标 = [`App::picked`] 的口径）。
        ///
        /// 空表有**三种**成因，全都不是错误：矩形档（一行资产都不读）、`%USERPROFILE%`/`$HOME`
        /// 都读不到（没有库）、库里每一件都没过门禁（跳过那几行点名了每一件）。
        candidates: Vec<Candidate>,
        /// 本体库（启动时扫过四道门禁、**一像素未解**的那几只）；空 = 没有第二只换得上去。
        ///
        /// 收藏册的宠物行点下去就是这张表里的下标（`PanelItem::Body`）。它必须不含像素，理由与
        /// [`Self::candidates`] 那一段逐字相同：解码一张 sheet 是 45 MiB，而"浏览一下库"不该付它。
        ///
        /// ⚠️ 与 `candidates` 是**两套下标空间**（拍板①）：`picked` 读前者，
        /// [`App::swap_body`] 读后者，共用一张表就是
        /// `the_album_routes_pets_to_the_body_index_space` 那条门禁要杀的形状。
        bodies: Vec<BodyCandidate>,
        /// 穿着中的那一件 = 第二张 sheet 的像素。`None` = 没穿。
        ///
        /// ⚠️ **只有 [`App::wear`] 写 `Some`、只有 [`App::take_worn`] 写 `None`**（红线 #16 的
        /// Windows 版，那条源级门禁的本体在 `overlay-wayland`，这一侧的等价物在
        /// `dress.rs` 的 `the_pixels_and_the_ledger_are_written_by_one_function_each`）。
        /// 理由在那边是实测出来的：把 `persist_worn` 单独删掉，全套测试照样绿，而用户看到的是
        /// "屏上戴着一顶而重启什么都没有"。
        worn: Option<Worn>,
        /// `candidates` 的下标 = "换一件"当前停在哪一格。
        ///
        /// 它是**下标**不是 id：可见集（[`DressView::visible`]）返回的也是下标，两者同一口径，
        /// 不会出现"选中的是哪件"有两个事实源。初值 0 与 Wayland 同（库里第一件）。
        picked: usize,
        /// 当前本体的身份（`--manifest` 那个目录名，退化成清单 `slug`；= [`Self::bodies`]
        /// 的 `id` 同一个键）。
        ///
        /// 两件事读它：`equipped` 的 `body` 行写它（[`App::persist_body`]）、收藏册那一行
        /// "是不是它"的 `same_id` 判据读它（[`App::swap_body`] 第 1 步）。
        /// ⚠️ 它**不上屏** —— 当前是哪只本体就是宠物自己那几个像素，再打一行字就是第二个事实源。
        body_id: String,
        /// 恢复上次穿着只跑**一次**的闩。`round` 环外读它（同 `pending_menu_toggle` 那条纪律：
        /// 它要 `&mut self` 并且必须在第一次 `present` **之前**生效，否则第一帧没帽子、第二帧才有）。
        restore_pending: bool,
        menu: Option<MenuState>,
        /// 面板面（M7b）。**与 `menu` 永不同时是 `Some`** —— 互斥的唯一守卫在
        /// [`App::apply_panel`]（落地 `Show` 那一刻先关菜单），理由照 Wayland 的 `panel`
        /// 模块头：同层两块以上的堆叠序没有定义，而这里还多一条 —— 两块面都读同一份库存，
        /// 叠着画就会出现"点的是上面那块的行、改的是下面那块的内容"。
        panel: Option<PanelState>,
        /// 面板的**延迟一拍**旗，与 [`App::pending_menu_toggle`] 同一条纪律：建窗/关窗要
        /// `&mut Client`，而 `Events` 回调发生在 `pump` 内部（那时 client 正被借用）。
        ///
        /// ⚠️ 一次 `pump` 里连点两下 ⇒ 后一次覆盖前一次（不是排队）。这在屏上是无损的：
        /// 这一面每轮最多 present 一次，只有最后一次的内容 ever 成为像素；代价只有一条，
        /// 就是"中间那一屏"不存在（`More` 连点两下 = 同一页加两次基数相同的 1）。写在这里
        /// 是为了下一次有人想把它改成 `Vec` 时知道现在让掉的是什么。
        pending_panel: Option<PanelRequest>,
        /// 右键 toggle 的**延迟一拍**旗：开窗/关窗要 `&mut Client`，而 `Events` 回调发生在
        /// `pump` 内部（那时 client 正被借用）。与 Wayland"按钮回调里直接建 surface"的
        /// 差别是平台形状：那边回调手里就有 compositor，这边没有。
        pending_menu_toggle: bool,
        /// `Close`/`Quit` 行同款的两面旗：点在 `press` 派发那一刻记下，环外收尾。
        pending_menu_close: bool,
        /// 退出请求（菜单"退出"行 = `--seconds` 之外的第二个退出路径，照 Wayland `quit_pet`）。
        /// `round` 读到它 ⇒ 关面、返回"该停了"，`drive` 走 `request_quit`，**摘要照打**。
        exit_requested: bool,
    }

    impl App {
        /// 启动：量台 → 按 1.00 建窗 → 读真 scale → 重建几何 → 装内核。
        ///
        /// 为什么中间要先开一次窗：`GetDpiForWindow` 只有在**窗口已经挂到显示器上**之后
        /// 才答得准（`window.rs` 里那一格顺序不能反），而窗口尺寸又要 scale ⇒ 圆环。
        /// 拆开它的不是猜一个 scale，而是**先按 1.00 建一扇看不见的窗**：`Client::open`
        /// 之后到第一次 `present` 之前，屏上没有东西（分层窗口的像素只在 ULW 那一刻出现），
        /// 所以那一档尺寸不会被人看见，也不需要"临时画一帧"。
        fn startup(opts: Options) -> Result<(App, Client), String> {
            // 设置只在**启动时读一次**（硬约束 5「事件驱动，禁止定时轮询」⇒ 不做 watcher）。
            // 顺序固定，与 Wayland 母本逐字同形：先说文件本身怎么样（坏语法要抢在生效值前面），
            // 再说生效值和它来自哪一层。`Config::read()` 里的 `cfg!(windows)` 挑的是
            // `%LOCALAPPDATA%` 那条三档阶梯（见 `crate::settings` 模块头）。
            let cfg = petstore::config::Config::read();
            cfg.announce();
            eprintln!("ui: 配置 {}", cfg.describe());
            // CLI 那两级开关此刻真的传进来了（`cli.rs` 已在解析期把坏值判掉）：这里给错值
            // = `Err` 退出，文件/环境变量给错值 = 出声后按链条往下走（两条路的理由在
            // `crate::settings` 模块头，判分在共用层）。
            let ui = settings::from_process(opts.lang.as_deref(), opts.motion.as_deref(), &cfg)?;
            ui.announce();

            // 把本轮生效的语言写回 config，于是"首轮取环境语言、此后取上次退出语言"两条同时
            // 成立：首轮没有 config ⇒ 走 env→默认，写回后重跑时 config 优先于 env。
            // `--lang` 例外：它是一次性覆盖，不该被锁进文件（否则下次不带开关也回不去环境语言）。
            // ⚠️ 本平台的环境语言一般是空的（原生 Windows 没有 `LANG`，而这里刻意不调
            // `GetUserDefaultUILanguage` —— 理由见 `crate::settings` 模块头），所以这一发在
            // 首轮写的就是**默认档** `en`：文件会因此被创建。这是与 Wayland 同一行为的后果，
            // 不是本平台另加的副作用（那条链的语义是"生效值落盘"，不是"用户显式设过才落盘"）。
            if ui.lang_from != settings::ValueSource::Cli {
                let _ = petstore::config::write_ui_key(petstore::config::KEY_LANG, ui.lang.code());
            }

            let started = Instant::now();
            // ⚠️ **先感知、后枚举**：awareness 生效之前进程是 DPI-unaware，
            // `GetMonitorInfoW` 交回的是**虚拟化**矩形（真机 2026-11-26：125% 的
            // 1920x1080 屏报成 1536x864 = 物理 ÷ scale），再除一次 scale 就是双重
            // 除法 ⇒ 舞台比屏幕小一整圈、脚不落地。`Client::open` 第一步也调它，
            // 但那一格只保证"建窗之前"—— 舞台算在建窗之前，所以这里也必须自己调。
            // 首调裁决、再调同值（`window::set_dpi_awareness` 记忆化）⇒ 两处读数一致。
            let _ = set_dpi_awareness();
            let mons = crate::monitor::enumerate()?;
            let stage = crate::layout::choose(&mons, opts.monitor.as_deref()).ok_or_else(|| {
                let found: Vec<&str> = mons.iter().map(|m| m.name.as_str()).collect();
                match &opts.monitor {
                    // 名字要给原文：这台机器上报出的是设备名（`\\.\DISPLAY1` 那一族），
                    // 与用户在“显示设置”里看到的显示器型号不是一回事，只说“没命中”没法自查。
                    Some(w) => format!(
                        "--monitor {w:?} 没有命中任何显示器（大小写不敏感整名匹配，要的是设备名）；\
                         这台机器报出的是 {found:?}"
                    ),
                    None => {
                        "一台显示器都没枚举到 ⇒ 没有可当舞台的一块（这不是参数问题）".to_string()
                    }
                }
            })?;
            let stage_name = stage.name.clone();
            let stage_mon = stage.clone();

            let mut manifest = load_manifest(&opts)?;
            // —— 起点本体（P5 = M7d）。本体库排在**解第一张 sheet 之前**扫一遍，因为库里那一行
            // `body` 可能改写"第一只本体是谁"，而那一天绝不该解两张 sheet（峰值 = 两只本体
            // ≈147 MB，它**仍落在 ≤200 MiB 门禁内 ⇒ 什么都不会红**，唯一的读数就是这段代码本身）。
            //
            // ⚠️ 这道 `pixels` 门槛与 [`load_atlas`] 里那两个"没有像素"的分支**是同一个条件**
            // （`--test-rect` / 没给 `--manifest`）：先算成布尔量，是为了打断
            // `discover_pets → load_atlas → discover_pets` 那条环（Wayland 母本 11-07 踩过）。
            let pixels = !opts.test_rect && opts.manifest.is_some();
            let mut bodies = match pet_root().filter(|_| pixels) {
                Some(root) => discover_pets(&root, &manifest.grid),
                // 本体没像素（矩形档 / 内置 fixture 那条路）⇒ 换了没东西可画；`%LOCALAPPDATA%`
                // 与 `$XDG_DATA_HOME`/`$HOME` 都读不到 ⇒ **没有库**。两种都是"收藏册的宠物行全部
                // 只读"，不是错误（这张表**不**参与"起不起来"的判断）。
                None => Vec::new(),
            };
            // —— 拍板③：库里那一行**赢**，`--manifest` 退化成"库里没有那一行时用什么"。
            // 老库（没有那一行）⇒ 这一整段是 `Keep(None)`，一个字都不打。
            let row = db_body_row(opts.db.as_deref());
            let pet_ids: Vec<String> = bodies.iter().map(|b| b.id.clone()).collect();
            // 起点本体的身份（`id` = 目录名 = 库存与 `equipped` 认的那个键）。
            let mut body_id = start_body_id(opts.manifest.as_deref(), &manifest.slug);
            // 覆盖成立时要连**目录**一起换：sheet 从它取。本平台没有 polygon 的消费者
            // （命中读的是叠完之后的像素），但 `discover_pets` 仍按三件套要求它 ⇒ 两家同一本账。
            let mut body_dir: Option<super::PathBuf> = None;
            match plan_body_override(row.as_deref(), &pet_ids, &body_id) {
                BodyChoice::Override(i) => {
                    let b = &mut bodies[i];
                    // 这一行排在 `manifest` 被改写**之前** ⇒ `{fallback}` 念到的还是兜底那只的 `id`。
                    eprintln!(
                        "body: 库里那一行 {slot}={id} ⇒ 起点本体从 {fallback} 换成这只（`--manifest` 退化成\
                         兜底；格子档与它相同 ⇒ 窗口尺寸 / buffer / DIB 一个都不必改）",
                        slot = overlay_common::dress::BODY_SLOT,
                        id = b.id,
                        fallback = body_id,
                    );
                    // 第一次用上它才 `Box::leak`（每只进程内只泄一份清单，几 KB）。
                    manifest = b.leaked();
                    body_dir = Some(b.dir.clone());
                    body_id = b.id.clone();
                }
                // 点名：库里明明有一行而这一轮不覆盖 ⇒ 必须说得出为什么（静默退回兜底 = 用户上次
                // 换的本体"自己没了"而没有任何一句话解释，那是本项目反复要杀的静默）。
                BodyChoice::Keep(Some(why)) => eprintln!(
                    "body: 库里那一行 {slot}={id} ⇒ 不覆盖起点本体：{why} ⇒ 这一轮用 `--manifest` 那只",
                    slot = overlay_common::dress::BODY_SLOT,
                    id = row.clone().unwrap_or_default(),
                ),
                BodyChoice::Keep(None) => {}
            }
            let atlas = load_atlas(&opts, manifest, body_dir.as_deref())?;
            let cell = (manifest.grid.w, manifest.grid.h);
            // 配件库在启动时**只读元数据**（清单 + `fits` 本体），一像素不解 ⇒ "穿上"唯一的
            // 失败面只剩"sheet 解不开"（Wayland 同一条前提，`VmHWM` 那一档的字面要求）。
            // 矩形档一行资产都不读，库里有什么都一样 ⇒ 空表，不是错误（`asset:` 那行已经说明
            // 这一档不读像素，这里不再补一行噪音）。
            let candidates = if opts.test_rect {
                Vec::new()
            } else {
                match accessory_root() {
                    Some(root) => discover_accessories(&root, manifest),
                    // 环境的两条腿（`%LOCALAPPDATA%` 与 `$XDG_DATA_HOME`/`$HOME`）都读不到
                    // ⇒ **没有库**。换装页照样画、照样点，"穿上"会打"库里没有可用配件"。
                    None => {
                        eprintln!(
                            "accessories: 读不出配件库的位置（`%LOCALAPPDATA%` / `$XDG_DATA_HOME` / \
                             `$HOME` 全空）⇒ 换装页「穿上」点了不会有效果"
                        );
                        Vec::new()
                    }
                }
            };
            let probe = Frame::new(1.0, cell.0, cell.1)?;
            let (pw, ph) = probe.surface_phys();
            let mut client = Client::open((stage.left, stage.top), (pw, ph))?;

            let scale = client
                .scale(SurfaceKind::Pet)
                .expect("刚 open 完，注册表里必然有本体面（缺它 = Client 被绕开构造了）");
            let dpi_path = client.dpi_path();
            let timer_path = client.timer_path();
            let frame = Frame::new(scale, cell.0, cell.1)?;
            let (bw, bh) = frame.surface_phys();
            if (bw, bh) != (pw, ph) {
                // 开机那一档按 1.00 建，真 scale 不是 1.00 ⇒ 立刻换一次 DIB 与窗口尺寸。
                // 这一步不许省：症状是"宠物在 150% 屏上只有三分之二大"，而没有任何一步会报错。
                client.resize(SurfaceKind::Pet, (bw, bh))?;
            }
            let stage_buf = Stage::new(frame);

            let insets = resolve_insets(&stage_mon, scale, opts.panel_top, opts.panel_bottom);
            let kmons: Vec<Monitor> = mons
                .iter()
                .map(|m| kernel_monitor(m, scale, &insets, m.name == stage_name))
                .collect();

            let sim = if opts.test_rect {
                None
            } else {
                let world = World::new(kmons.clone()).ok_or_else(|| {
                    format!(
                        "舞台几何退化，内核建不出世界（显示器 {kmons:?}）⇒ 拒起而不是起一只乱跑的宠物"
                    )
                })?;
                Some(Sim::new(
                    world,
                    manifest,
                    opts.seed,
                    BehaviorConfig::default(),
                ))
            };

            let deadline = opts.seconds.map(|s| started + Duration::from_secs_f32(s));

            println!("win: {}", opts.describe());
            // M4 `--db` 转真：启动这一发只证两件事 —— Windows 路径链（petstore::paths）实解成
            // 什么、schema 能否在此**首次建成**，建完即关。活到退出的连接在 UI 那一侧
            // （`ensure_gacha` 惰性连：M5e 的启动恢复与 M5c 的开菜单，谁先到谁连），两边互不替代。
            let dbp = petstore::DbPath::resolve(opts.db.as_deref().map(std::path::Path::new));
            if dbp.source.is_default() {
                // 兜底档 = Wayland 那条「不给 --db 绝不碰真存档」的 Windows 等价物：
                // 打印实解位置，一个字节都不写（`Store::open_at` 对 default 源本来也拒）。
                println!(
                    "win: 存档 = 兜底档（未给 --db；真存档的位置是 {} ← {}，本轮不写）",
                    dbp.path.display(),
                    dbp.source.as_str()
                );
            } else {
                let store = petstore::Store::open_at(&dbp)
                    .map_err(|e| format!("打开/创建存档 {} 失败：{e}", dbp.path.display()))?;
                drop(store);
                println!(
                    "win: 存档 = {} ← {}（schema 已建/验；常驻连接在第一轮恢复上次穿着、\
                     或开菜单那一刻惰性建立，两者哪个先到）",
                    dbp.path.display(),
                    dbp.source.as_str()
                );
            }
            println!(
                "win: 舞台 {stage_name} 物理 {}{} {}x{}（主屏={}），共 {} 台",
                stage.left,
                stage.top,
                stage.right - stage.left,
                stage.bottom - stage.top,
                stage.os_primary,
                mons.len(),
            );
            println!(
                "win: {}",
                describe_inset("顶部留白", &insets.top, insets.measured_top)
            );
            println!(
                "win: {}",
                describe_inset("底部留白", &insets.bottom, insets.measured_bottom)
            );
            println!(
                "win: scale {:.2}×（{}）｜窗口/buffer {bw}x{bh}｜逻辑 {}x{}",
                scale,
                dpi_path.as_str(),
                cell.0 + 2 * SURFACE_MARGIN as u32,
                cell.1 + 2 * SURFACE_MARGIN as u32,
            );
            match timer_path {
                TimerPath::HighResolution => println!("win: timer HIGH_RESOLUTION"),
                TimerPath::Fallback(e) => println!(
                    "win: timer 退路（错误码 {e:#X}）⇒ 唤醒粒度是系统定时器精度，\
                     累积器会补，但这一轮的 tick 抖动有第二个来源"
                ),
            }
            if !dpi_path.tracks_per_monitor() {
                println!(
                    "win: ⚠️ 这一档 DPI 感知**没有** WM_DPICHANGED ⇒ 拖到另一块不同缩放的屏上\
                     宠物会停在 {scale:.2}×，不会重算（不是 bug，是这台机器上这条路没有事件）"
                );
            }
            if let Some(m) = mons.iter().find(|m| m.name != stage_name) {
                println!(
                    "win: 另有显示器 {}（物理 {}x{}）；内核按**舞台那一台的 scale** 换算全部屏幕，\
                     跨 DPI 的多屏在这一片是近似（登记给 §3.7 回写）",
                    m.name,
                    m.right - m.left,
                    m.bottom - m.top
                );
            }

            let rect_at = (
                to_phys(stage.left as f32 / scale + RECT_AT, scale),
                to_phys(stage.top as f32 / scale + RECT_AT, scale),
            );
            let last_at = if opts.test_rect {
                // 矩形档先把窗口摆到固定落点，屏上看得见它在哪
                client.move_only(SurfaceKind::Pet, rect_at)?;
                rect_at
            } else {
                // 本体档**没有**任何"窗口当前在这里"的说法——开机那一站只是显示器原点，
                // 而那是 `Client::open` 的位置，不是宠物该在的位置。写 `rect_at` 在这种
                // 屏幕上（窗口尺寸 == 落点余量）会与真的第一格落点相等 ⇒ 那一格被判
                // `moved=false`，虽然结果仍对，但那一格的"移动"就从日志里消失了。
                // 用一个任何落点都不可能取到的值，第一格才诚实。
                (i32::MIN, i32::MIN)
            };

            // 演示态在**参数落定的这一刻**判一次（Wayland 同型：不给 `--db` 就是 DRYRUN），
            // 之后所有读取者（卡片水印、M5e 的换装过滤）都问这一个布尔，不在各自分支重算。
            let dry_run = opts.db.is_none();

            Ok((
                App {
                    manifest,
                    atlas,
                    sim,
                    stage_buf,
                    frame,
                    cell,
                    opts,
                    mons,
                    stage_mon,
                    insets,
                    scale,
                    dpi_path,
                    ticks: 0,
                    counters: Counters::default(),
                    source: Source::Replay,
                    started,
                    last_wake: started,
                    deadline,
                    acc: 0.0,
                    // 窗口此刻还没有任何像素（ULW 之前分层窗是空的），所以不需要"先画一帧占位"。
                    force_draw: true,
                    drawn_cell: None,
                    last_intent: None,
                    last_at,
                    drag: false,
                    grab_offset: Vec2::ZERO,
                    last_press: None,
                    pointer_world: Vec2::ZERO,
                    pending_scale: None,
                    gacha: None,
                    ui_error: None,
                    ui,
                    dry_run,
                    candidates,
                    bodies,
                    worn: None,
                    picked: 0,
                    body_id,
                    // 恒 `true`：`restore_worn` 自己有四条早退（演示态、矩形档、库里没那一行、
                    // 可见集为空），而"这一次到底要不要恢复"这件事**只有它能判**（要连库）。
                    // 在这里预先判 = 拿启动期的参数猜运行期的库，猜错的方向是"该恢复却没试"。
                    restore_pending: true,
                    menu: None,
                    panel: None,
                    pending_panel: None,
                    pending_menu_toggle: false,
                    pending_menu_close: false,
                    exit_requested: false,
                },
                client,
            ))
        }

        /// 事件环的一轮：退出旗 → DPI 变更 → UI 旗标收尾 → 定时累积 → 步进内核 → 出图 → 菜单刷新。
        ///
        /// 返回 `false` = 该停了（`--seconds` 到点，或菜单"退出"行置了 `exit_requested`）。
        ///
        /// ⚠️ **UI 操作全部延迟到这一层**（`pending_menu_toggle`/`pending_menu_close`）：
        /// 开/关窗要 `&mut Client`，而 `Events` 回调在 `pump` 内部执行，那时 client 正被借用
        /// （`App` 文档头那条线性借用纪律的同一条推论）。用户视觉上仍是"右键立刻出菜单"——
        /// `pump` 是被事件叫醒才返回的，返回后的第一件事就是这里。
        fn round(&mut self, client: &mut Client) -> Result<bool, String> {
            let now = Instant::now();
            if let Some(dl) = self.deadline {
                if now >= dl {
                    return Ok(false);
                }
            }
            if self.exit_requested {
                // 照 Wayland `quit_pet` 的两步顺序：**先关自己建的面，再走退出**。
                // "退出不依赖另一条不变量此刻成不成立"，所以这里显式 close 而不交给
                // `Client::drop`（那条是兜底，不是这条路径的负责人）。M7b 起"自己建的面"
                // 是两块 ⇒ 两条 close 都走，顺序无关（`Client::close` 幂等，且两块面互斥
                // 时另一条就是 `take()` 到 `None` 的空转）。
                self.close_menu(client);
                self.close_panel_now(client);
                return Ok(false);
            }
            if let Some(s) = self.pending_scale.take() {
                self.apply_dpi(client, s)?;
            }
            if std::mem::take(&mut self.pending_menu_toggle) {
                self.toggle_menu(client);
            }
            if std::mem::take(&mut self.pending_menu_close) {
                self.close_menu(client);
            }
            if self.pending_panel.is_some() {
                // 面板的建窗/关窗，以及"开面板必先关菜单"那一句，都落在这一格（回调里办不了，
                // 见 [`App::pending_panel`]）。排在两个菜单旗**之后**是有意的：同一轮里既翻了
                // 菜单又要开面板时，最终该在场的是面板；反过来排会让 `toggle_menu` 看到
                // "面板开着"再把它关掉 ⇒ 屏上两块面都没了，而日志会说两块都关过。
                self.apply_panel(client);
            }
            if std::mem::take(&mut self.restore_pending) {
                // 恢复上次穿着排在**步进之前**：它做的事是"往 `worn` 里放一张 sheet + 置
                // `force_draw`"，而第一格的 `present` 就在下面那个循环里 ⇒ 排在后面会让宠物
                // 先以"没戴帽子"的样子上一帧（16 ms，但那一帧是真的，而摘要里那一格会记成
                // 一次正常提交）。Wayland 同一条次序（`on_tick` 开头）。
                self.restore_worn();
            }

            // 与 Wayland `on_tick` 同一个式子：墙钟差夹住、按 1/TICK_HZ 累加、补帧有预算。
            let dt = now
                .duration_since(self.last_wake)
                .as_secs_f64()
                .clamp(0.0, 0.25) as f32;
            self.last_wake = now;
            self.acc += dt;
            let step = 1.0 / TICK_HZ;
            let mut budget = MAX_CATCHUP;
            // 这一段是**唯一**算"一个 tick"的地方 ⇒ 环外一律非归属档，免得哪个新加的调用点
            // 忘了标就静默双记（`overlay-common/src/accounting.rs` 头那段历史）。
            self.source = Source::Tick;
            while self.acc >= step && budget > 0 {
                self.acc -= step;
                budget -= 1;
                self.ticks += 1;
                if self.opts.test_rect {
                    self.rect_frame(client);
                } else if let Some(sim) = self.sim.as_mut() {
                    let (_, intent) = sim.step();
                    self.present(client, &intent)?;
                }
            }
            self.source = Source::Replay;
            if self.acc >= step {
                // 积压超过补帧预算 ⇒ 丢弃，不留着下一轮连环提交
                self.acc = 0.0;
            }
            // 菜单刷新排在 tick 循环**外**：它按"内容签名变了没有"决定提交，不按 tick 计数，
            // 而 `menu_commits` 本来就不并进五档（accounting.rs 那段"非 tick 补画"的同一族）。
            // 没开菜单时这一句就是一次 `is_none` 检查。
            self.update_menu(client);
            // 面板与菜单同一档：闸门是内容签名，不是轮次。没开面板时这一句就是一次 `is_none`。
            // 顺序在菜单之后而不是之前 —— 两块面互斥（`apply_panel` 已经把菜单关掉了），
            // 所以这一轮实际只有一块面有活干，谁先谁后不影响读数，只影响日志里两行的先后。
            self.update_panel(client);
            Ok(true)
        }

        /// 三级分派主入口（本平台版，语义与 `OverlayBackend::present` 对齐但不经过那条 trait，
        /// 理由见 [`Options::test_rect`]）。
        fn present(
            &mut self,
            client: &mut Client,
            intent: &PresentationIntent,
        ) -> Result<(), String> {
            // `key` 必须跟着行列号一起走：换装之后叠不叠这一件，问的是"这一件在**这一档状态**
            // 烘过像素吗"（`AccessoryManifest::covers`），而那个参数的真值是图集键。
            // 第一批这里只抄 `(row,col)`，因为那时 `worn` 恒 `None`、`covers` 没有读者。
            let sprite = intent.sprite.map(|s| ((s.row as u32, s.col as u32), s.key));
            let cell = sprite.map(|(c, _)| c);
            self.last_intent = Some(intent.clone());
            let (x, y, _, _) = self.frame.window_rect(&intent.surface);
            let at = (x, y);
            let moved = at != self.last_at;
            let new_cell = cell.is_some() && (cell != self.drawn_cell || self.force_draw);
            // `frame_pending` 传**常量 false**：分层窗口没有在飞 buffer 的回执，所以
            // `classify_tick` 的 `Deferred` 臂在本平台**结构上不可达**（摘要里那一档的
            // inline 标注就是为这一格写的）。传 true 才是 bug：那会把每一格都判成延后。
            let outcome = match classify_tick(new_cell, moved, false) {
                Tick::Move => match client.move_only(SurfaceKind::Pet, at) {
                    Ok(()) => {
                        self.last_at = at;
                        Tick::Move
                    }
                    Err(e) => {
                        // 位置没挪成而客户端以为挪了 ⇒ 下一格用 ULW 的 `dstPoint` 再摆一次
                        // （`UpdateLayeredWindow` 自带定位，这正是 Wayland 那条
                        // "补一个状态 commit 把新 margin 冲出去"的同类处置）。
                        eprintln!("move_only 失败: {e} ⇒ 下一 tick 强制重画以带回位置");
                        self.force_draw = true;
                        Tick::Fault
                    }
                },
                Tick::Idle => Tick::Idle,
                Tick::Deferred => unreachable!(
                    "classify_tick 只在 frame_pending 为真时产出 Deferred，而本平台恒传 false"
                ),
                Tick::Buffer => {
                    let painted = match (sprite, self.atlas.as_ref()) {
                        (Some(((row, col), key)), Some(set)) => {
                            // 本体 + （若穿着）配件 → 同一块 DIB。`worn` 是 `None` 时这一句与
                            // 第一批的 `paint` **逐像素等价**（`paint_composited` 就是它加上
                            // 一次 `covers` 判断与一趟 `blit_over`，M5b 那两条测例钉着等价）。
                            // 清单跟着集合一起给：`row` 是全局行号，段号只在 `locate` 那里换算。
                            let manifest = self.manifest;
                            self.stage_buf
                                .paint_composited(
                                    (set, manifest),
                                    self.worn.as_ref(),
                                    (row, col),
                                    key,
                                )
                                .and_then(|()| {
                                    client.present(SurfaceKind::Pet, at, self.stage_buf.bytes())
                                })
                        }
                        other => Err(format!(
                            "本体出图路径缺了格或图集（{other:?}）⇒ 这不是矩形档，是接线漂移"
                        )),
                    };
                    match painted {
                        Ok(()) => {
                            if self.drawn_cell.is_none() {
                                // 一次性"宠物在屏幕上的哪一块"：Wine/真机的自动化判卷与人工
                                // 右键都要读这一格（右键必须落在剪影的不透明像素上才有菜单），
                                // 而它是内核第一个落点，除了这一刻没有任何地方打印过。
                                let (bw, bh) = self.frame.surface_phys();
                                eprintln!(
                                    "win: 本体首帧 物理 at {},{} buffer {bw}x{bh}（右键落这格的剪影内即出菜单）",
                                    at.0, at.1
                                );
                            }
                            self.drawn_cell = cell;
                            self.force_draw = false;
                            self.last_at = at;
                            Tick::Buffer
                        }
                        Err(e) => {
                            // 失败 ⇒ 这格**没**上屏。留 `force_draw` 为真让下一 tick 重投，
                            // 而不是静默跳过：跳过的症状是"缺一段动画"，与"资产本来就少那一格"同形。
                            eprintln!("present 失败: {e}");
                            self.force_draw = true;
                            Tick::Fault
                        }
                    }
                }
                // `classify_tick` 不产出这三档（`classification_partitions_the_inputs` 钉着）
                Tick::Replay | Tick::Configure | Tick::Fault => {
                    unreachable!("这三档只在出图之后才可能成立")
                }
            };
            self.attribute(outcome);
            Ok(())
        }

        /// 矩形档的一格：整幅品红方块，margin 那圈全透。
        ///
        /// ⚠️ **这里与 Wayland 有一处有意的偏离**：Wayland 的 `test_rect_frame` 直接
        /// `counters.commits += 1`（`overlay-wayland/src/app.rs:4865`），于是矩形档那一轮的
        /// "五档合计 == tick"必然读不齐 —— 而摘要里那行 `核对:` 的正确用途恰恰是
        /// "有 tick 没归进任何一档"。本平台按正常归属走（画了 = `Buffer`，没画 = `Idle`），
        /// 让那一行在**两种**模式下都可读。这笔偏离登记给第 11 步的 §3.7 回写。
        fn rect_frame(&mut self, client: &mut Client) {
            if !self.force_draw {
                self.attribute(Tick::Idle);
                return;
            }
            self.stage_buf.paint_solid();
            match client.present(SurfaceKind::Pet, self.last_at, self.stage_buf.bytes()) {
                Ok(()) => {
                    self.force_draw = false;
                    self.attribute(Tick::Buffer);
                }
                Err(e) => {
                    eprintln!("present(矩形) 失败: {e}");
                    self.attribute(Tick::Fault);
                }
            }
        }

        /// 缩放变了（`WM_DPICHANGED` 只可能在 `DpiPath::PerMonitorV2` 上到达这里）。
        ///
        /// 顺序是**窗口先、缓冲后、画第三**：`Stage::resize` 换的是我们自己那块 `Vec`，
        /// `Client::resize` 换的是 `CreateDIBSection` 那张位图，而 `present` 要求两边字节数
        /// 逐像素相等（`window.rs` 里那条"不截断"检查）。反了不会崩，会报一句
        /// "src 长度不对"而让人去查图集。
        fn apply_dpi(&mut self, client: &mut Client, scale: f32) -> Result<(), String> {
            if !scale.is_finite() || scale <= 0.0 {
                return Err(format!(
                    "WM_DPICHANGED 给出的缩放 {scale} 不可用 ⇒ 不换几何"
                ));
            }
            let frame = Frame::new(scale, self.cell.0, self.cell.1)?;
            let (bw, bh) = frame.surface_phys();
            client.resize(SurfaceKind::Pet, (bw, bh))?;
            self.frame = frame;
            self.stage_buf.resize(frame);
            self.scale = scale;

            // 舞台几何跟着重算：留白的**现量**是物理像素，逻辑值随 scale 变（旗覆盖的那两档不变）。
            self.insets = resolve_insets(
                &self.stage_mon,
                scale,
                self.opts.panel_top,
                self.opts.panel_bottom,
            );
            let stage_name = self.stage_mon.name.clone();
            if let Some(sim) = self.sim.as_mut() {
                for m in &self.mons {
                    let km = kernel_monitor(m, scale, &self.insets, m.name == stage_name);
                    sim.apply_world_event(WorldEvent::MonitorGeometry {
                        monitor: km.name.clone(),
                        rect: km.rect,
                        scale: km.scale,
                        panel_bottom: km.panel_bottom,
                    });
                }
            }

            println!(
                "dpi: WM_DPICHANGED ⇒ 生效缩放 {:.2}×，窗口/buffer {bw}x{bh}；{}",
                scale,
                describe_inset("顶部留白", &self.insets.top, self.insets.measured_top)
            );
            self.drawn_cell = None;
            self.force_draw = true;
            // 重配置**不是** tick ⇒ 归属走 `Source::Configure`（那一档另计，不占五档）。
            self.source = Source::Configure;
            let last = self.last_intent.clone();
            match (last, self.opts.test_rect) {
                (Some(intent), false) => self.present(client, &intent)?,
                (None, false) => {
                    // 第一拍之前就来过 DPI 变更：没有 intent 可重画，留给下一个 tick 正常出图。
                }
                (_, true) => self.rect_frame(client),
            }
            self.source = Source::Replay;
            Ok(())
        }

        fn attribute(&mut self, outcome: Tick) {
            if let Some(hit) = attribute_of(self.source, outcome) {
                self.counters.bump(hit);
            }
        }

        /// 窗口内物理像素 → 世界逻辑坐标（与 Wayland 的 `surface.pos + e.position` 同义，
        /// 只是本平台拿到的是物理像素，必须先除一次 scale —— 见 `lib.rs` 坐标系那一格）。
        fn to_world(&self, px: i32, py: i32) -> Vec2 {
            let (lx, ly) = self.frame.stage_to_local(px, py);
            let origin = self
                .last_intent
                .as_ref()
                .map(|i| i.surface.pos)
                .unwrap_or(Vec2::ZERO);
            Vec2::new(origin.x + lx, origin.y + ly)
        }

        fn on_press(&mut self, world: Vec2) {
            self.pointer_world = world;
            let now = Instant::now();
            let dbl = match self.last_press {
                Some((t, p)) => {
                    now - t <= DOUBLE_TAP_GAP
                        && ((p.x - self.pointer_world.x).powi(2)
                            + (p.y - self.pointer_world.y).powi(2))
                        .sqrt()
                            < DOUBLE_TAP_RADIUS
                }
                None => false,
            };
            // **每一次**按下都刷新 `last_press`（照 Wayland）：三连击在两平台折叠出的
            // 双击次数才相同。只在判定成双击时刷新 = 第三次永远不再算双击，那是本平台独有的行为。
            self.last_press = Some((now, self.pointer_world));
            let feet = self
                .sim
                .as_ref()
                .map(|s| s.body.pos)
                .unwrap_or(self.pointer_world);
            self.grab_offset =
                Vec2::new(self.pointer_world.x - feet.x, self.pointer_world.y - feet.y);
            // 双击不发起抓取（§3.5-j，与 Wayland 同文同序）：否则"解钉→跑开→同手被抓回→
            // 松手又钉回"，双击永远解不掉。抽卡反馈只发 DoubleTap（另一处），所以它不解钉。
            self.drag = !dbl;
            if let Some(sim) = self.sim.as_mut() {
                if dbl {
                    sim.send(Command::Unpin);
                    sim.send(Command::DoubleTap);
                } else {
                    sim.send(Command::DragStart);
                }
            }
        }

        /// 幂等：**不在拖拽中就当没事发生**。
        ///
        /// 这条不是防御性的。`ReleaseCapture()` 自己就会发一条 `WM_CAPTURECHANGED`，
        /// 所以"正常拖完"也会走到 [`Events::capture_lost`] 一次；窗口层虽然已经用
        /// `Window::captured` 那面旗把"我们自己放的"挡了一道，**两面都要**——少了这一面，
        /// 那多出来的一次事件会真的往内核写一条 `DragEnd`。
        fn on_release(&mut self, world: Vec2) {
            self.pointer_world = world;
            if !self.drag {
                return;
            }
            self.drag = false;
            if let Some(sim) = self.sim.as_mut() {
                sim.send(Command::DragEnd);
            }
        }

        // ───────────────────────── 菜单面（M5c）─────────────────────────

        /// 宠物那面的**舞台局部逻辑**矩形 = `menu::placement` 的输入。
        ///
        /// ⚠️ 这里多做一次 Wayland 那边不存在的换算：那边的 margin 本来就是逻辑坐标，
        /// 本平台手里只有上一次交给 `UpdateLayeredWindow` 的**物理**位置（`last_at`），
        /// 而 `placement` 与命中表全在逻辑口径上 ⇒ 不除 scale 就是拿物理像素喂逻辑函数，
        /// 症状在 150% 屏上是"菜单永远离宠物一段不该有的距离"。
        fn pet_logical_rect(&self) -> RectI {
            let lx = ((self.last_at.0 - self.stage_mon.left) as f32 / self.scale).round();
            let ly = ((self.last_at.1 - self.stage_mon.top) as f32 / self.scale).round();
            RectI::new(
                lx as i32,
                ly as i32,
                (self.cell.0 + 2 * SURFACE_MARGIN as u32) as i32,
                (self.cell.1 + 2 * SURFACE_MARGIN as u32) as i32,
            )
        }

        /// 舞台的**逻辑**宽高 = `placement` 的可用区。
        ///
        /// 与 Wayland 的 `output_available` 差一个语义层级：那边读的是扣掉 reserved 的可用区，
        /// 这边的 `stage_mon` 是整块监视器物理矩形（任务栏留白只进了内核舞台，没进屏幕坐标系）。
        /// clamp 因此比 Wayland 松任务栏那一档 ⇒ 菜单最坏情况压住任务栏上沿，**绝不出屏**。
        fn stage_logical(&self) -> (i32, i32) {
            (
                ((self.stage_mon.right - self.stage_mon.left) as f32 / self.scale).round() as i32,
                ((self.stage_mon.bottom - self.stage_mon.top) as f32 / self.scale).round() as i32,
            )
        }

        /// 菜单当前该摆的**物理屏幕**位置（`at_logical` × scale + 舞台原点）。
        ///
        /// 每轮现算而不是存死：DPI 变更后逻辑位置不变、物理位置必变（Wayland 那侧 margin
        /// 是逻辑的、由 compositor 重乘，这侧要我们自己乘）。
        fn menu_at(&self, m: &MenuState) -> (i32, i32) {
            (
                self.stage_mon.left + to_phys(m.at_logical.0 as f32, self.scale),
                self.stage_mon.top + to_phys(m.at_logical.1 as f32, self.scale),
            )
        }

        /// 右键翻转：**菜单开着 ⇒ 关菜单，面板开着 ⇒ 关面板，都没开 ⇒ 开菜单**。
        ///
        /// M7b 补上第三臂（原来那句"面板要到 M7 才存在，在那块面存在之前这一支不是被省略、
        /// 是不存在"到这一批为止）。三路而不是"一个 toggle 旗管两块面"：右键的语义是
        /// "把我此刻看着的那块关掉"，而两块面同屏不可能（互斥），所以"此刻是哪块"有唯一答案。
        fn toggle_menu(&mut self, client: &mut Client) {
            if self.opts.test_rect {
                eprintln!(
                    "menu: 矩形档不接菜单 ⇒ 这一档只验窗口映射与逐点穿透（没有内核，也就没有可操作的宠物）"
                );
                return;
            }
            if self.menu.is_some() {
                self.close_menu(client);
            } else if self.panel.is_some() {
                // 就地关（这一条手里就有 client，不必绕旗）。`Events::right_press` 那条臂走
                // 的才是旗 —— 两条路径汇到同一个 [`App::close_panel_now`]，"面板关了"只有一个写法。
                self.close_panel_now(client);
            } else {
                self.open_menu(client);
            }
        }

        /// 开菜单 = 摆位（纯函数）→ 连库读余额（第一次才真连）→ 建窗 → 存状态。
        ///
        /// 与 Wayland `open_menu` 同序、同豁免点亮时机；差异只在"建 surface + set_margin +
        /// commit"这一族协议动作塌成一次 `Client::spawn`，而**首帧不在这里画** ——
        /// `force_draw=true` 交给同一轮末尾的 `update_menu`（分层窗口在第一次 ULW 之前
        /// 屏上本来就什么都没有，与宠物开机同理）。
        fn open_menu(&mut self, client: &mut Client) {
            let pet = self.pet_logical_rect();
            let (aw, ah) = self.stage_logical();
            let at_logical = menu::placement(pet, RectI::new(0, 0, aw, ah));
            // "打开菜单"就是一个事件：连库（第一次才真连）+ 查一次余额。
            // 连不上也照样把菜单开出来 —— 错误行会说话，而打不开库绝不是宠物的死因。
            let balance = if self.ensure_gacha() {
                self.gacha_balance()
            } else {
                0
            };
            let m = MenuState::new(
                at_logical,
                self.scale,
                balance,
                self.dry_run,
                self.ui.motion,
                self.ui.lang,
                self.dress_exempt(),
            );
            let at = self.menu_at(&m);
            if let Err(e) = client.spawn(SurfaceKind::Menu, at, m.phys) {
                eprintln!("menu: 建窗失败 ⇒ 不开（{e}）");
                self.ui_error = Some(format!("菜单建窗失败: {e}"));
                return;
            }
            eprintln!(
                "menu: 打开于逻辑 {},{}（物理 at {},{}｜宠物逻辑 {},{} {}x{}｜舞台逻辑 {aw}x{ah}）",
                m.at_logical.0, m.at_logical.1, at.0, at.1, pet.x, pet.y, pet.w, pet.h
            );
            self.menu = Some(m);
        }

        /// 幂等：**没开过就当没事发生**（`Client::close` 同口径，两边都不许 double-close 报错）。
        ///
        /// 关闭打一行**累计提交数**：这一批的 Wine 判卷读的是日志流（一次开合之间有多少帧），
        /// 这一行就是那条读数的当场锚点。Wayland 那侧靠屏上开关 + 摘要，不需要这行。
        fn close_menu(&mut self, client: &mut Client) {
            if self.menu.take().is_some() {
                if let Err(e) = client.close(SurfaceKind::Menu) {
                    eprintln!("menu: 关窗失败: {e}");
                    return;
                }
                eprintln!("menu: 关闭（本轮累计 {} 提交）", self.counters.menu_commits);
            }
        }

        /// 每轮一遍：镜像 scale → 推进相位 → 按签名决定要不要提交。
        ///
        /// 静止的菜单**一个 present 都不发**（`counters.menu_commits` 必须不随轮次增长）。
        /// Wayland 的看门狗臂在这里**没有对应物**：那块面等的是 compositor 的帧回调回执，
        /// 本平台 `UpdateLayeredWindow` 是同步的，"在飞帧"这个状态结构上不存在。
        fn update_menu(&mut self, client: &mut Client) {
            let Some(mut m) = self.menu.take() else {
                return;
            };
            let error = self.error_line();
            if m.scale != self.scale {
                // scale 变了：窗口与缓冲都要换尺寸，次序照 `apply_dpi` 那条"窗口先、缓冲后、
                // 画第三"—— `present` 要求 src 字节数与窗口尺寸逐像素相等。
                m.scale = self.scale;
                m.phys = (
                    to_phys(CARD_W as f32, self.scale).max(1),
                    to_phys(CARD_H as f32, self.scale).max(1),
                );
                if let Err(e) = client.resize(SurfaceKind::Menu, m.phys) {
                    eprintln!("menu: 换缩放后 resize 失败: {e} ⇒ 这块旧尺寸的菜单不可用，关掉");
                    self.menu = Some(m);
                    self.close_menu(client);
                    return;
                }
                m.canvas = vec![0u8; (m.phys.0 * m.phys.1 * 4) as usize];
                m.force_draw = true;
            }
            m.step();
            let at = self.menu_at(&m);
            if m.draw(client, at, &error) {
                self.counters.menu_commits += 1;
            }
            self.menu = Some(m);
        }

        // ───────────────────────── 面板面（M7b）─────────────────────────

        /// 面板该摆的**物理屏幕**位置（[`App::menu_at`] 的同形物，理由原样：DPI 变更后
        /// 逻辑位置不变、物理位置必变，所以每轮现算而不存死）。
        fn panel_at(&self, p: &PanelState) -> (i32, i32) {
            (
                self.stage_mon.left + to_phys(p.at_logical.0 as f32, self.scale),
                self.stage_mon.top + to_phys(p.at_logical.1 as f32, self.scale),
            )
        }

        /// 把 [`App::pending_panel`] 落地。**这是本平台唯一会 spawn/close 面板的地方**
        /// （另一处 `close_panel_now` 的调用点在 `toggle_menu` 与退出臂，都走同一个函数）。
        ///
        /// 互斥那一句就落在这里：`Show` 先关菜单，再决定"换行还是建窗"。不在 `show_panel`
        /// 里关（那条要能被回调安全调用，而回调手里没有 client），也不在 `dispatch_menu` 里关
        /// （那样"谁关菜单"就有两个答案）。
        fn apply_panel(&mut self, client: &mut Client) {
            let Some(req) = self.pending_panel.take() else {
                return;
            };
            match req {
                PanelRequest::Close => self.close_panel_now(client),
                PanelRequest::Show { kind, rows } => {
                    if self.menu.is_some() {
                        self.close_menu(client);
                    }
                    if let Some(p) = self.panel.as_mut() {
                        // `show_panel` 只在"没开"时置这块旗，所以正常路径到不了这里。它存在的
                        // 理由是"面板此刻该显示什么"仍然只有 `set_lines` 一个写法 ——
                        // 与其在这里 panic 或重建窗口，不如换行。
                        p.set_lines(kind, rows, self.ui.lang);
                        return;
                    }
                    let pet = self.pet_logical_rect();
                    let (aw, ah) = self.stage_logical();
                    let at_logical = panel::placement(pet, RectI::new(0, 0, aw, ah));
                    let p = PanelState::new(at_logical, self.scale, kind, rows, self.ui.lang);
                    let at = self.panel_at(&p);
                    if let Err(e) = client.spawn(SurfaceKind::Panel, at, p.phys) {
                        eprintln!("panel: 建窗失败 ⇒ 不开（{e}）");
                        self.ui_error = Some(format!("面板建窗失败: {e}"));
                        return;
                    }
                    eprintln!(
                        "panel: 打开于逻辑 {},{}（物理 at {},{}｜{kind:?}｜宠物逻辑 {},{} {}x{}｜舞台逻辑 {aw}x{ah}）",
                        p.at_logical.0, p.at_logical.1, at.0, at.1, pet.x, pet.y, pet.w, pet.h
                    );
                    self.panel = Some(p);
                }
            }
        }

        /// 开面板 = 现算行 → [`App::show_panel`]。收藏册那三行**不走这里**，走
        /// [`App::collection_panel`]（同一条路还要把同一批字打进 stderr）。
        ///
        /// 这一条不碰库，所以它只可能因为"库还没连上"而失败于 Owned/Missing/Recent 那三本
        /// ⇒ 失败只让错误行说话（[`App::gacha_bailed`]），面板**不**带着空行开出来。
        fn open_panel(&mut self, kind: PanelKind) {
            match self.panel_rows(kind) {
                Ok(rows) => self.show_panel(kind, rows),
                Err(msg) => self.gacha_bailed(msg),
            }
        }

        /// 让面板显示这一组行：**开着就原地换行 + 换命中表，没开就记旗**（建窗要到 `round`）。
        ///
        /// 两个动作合成一条不是省事，也不是平台偷懒：「点标题带换 tab」与「从菜单进来」要办的
        /// 是同一件事（这一面此刻该画什么），分成两条路径就会出现其中一条**忘了同步 `items`**
        /// —— 而 `items` 是点击的唯一依据（`69fd158` 那一族"命中表与画面分家"）。
        fn show_panel(&mut self, kind: PanelKind, rows: Vec<PanelRow>) {
            if let Some(p) = self.panel.as_mut() {
                p.set_lines(kind, rows, self.ui.lang);
                return;
            }
            self.pending_panel = Some(PanelRequest::Show { kind, rows });
        }

        /// 回调侧的"关掉这块面" = 记旗（`Events` 回调发生在 `pump` 内部，那时动不了窗口）。
        /// 幂等：没开过就当没事发生（`Client::close` 同口径）。
        fn close_panel(&mut self) {
            if self.panel.is_some() {
                self.pending_panel = Some(PanelRequest::Close);
            }
        }

        /// 真的关窗。**关闭打一行累计提交数**：这一批的 Wine 判卷读的是日志流，
        /// "静止恒 1 提交"这条判据要靠这一行落地（Wayland 那侧靠摘要，不需要这行）。
        fn close_panel_now(&mut self, client: &mut Client) {
            if self.panel.take().is_some() {
                if let Err(e) = client.close(SurfaceKind::Panel) {
                    eprintln!("panel: 关窗失败: {e}");
                    return;
                }
                eprintln!(
                    "panel: 关闭（本轮累计 {} 提交）",
                    self.counters.panel_commits
                );
            }
        }

        /// 每轮一遍：镜像 scale → 按签名决定要不要 present。
        ///
        /// **没有 `step()`**（这一面没有任何随时间自己变的像素）、**没有看门狗**（同步 ULW
        /// 没有"在飞帧"这个状态）⇒ 静止恒 1 提交是它唯一的省电判据，而它必须与
        /// `menu_commits` 分开读（两块面共用池子的话，这个数就不再说明任何事）。
        fn update_panel(&mut self, client: &mut Client) {
            let Some(mut p) = self.panel.take() else {
                return;
            };
            let error = self.error_line();
            if p.scale != self.scale {
                // 次序照 `update_menu` 与 `apply_dpi` 那条"窗口先、缓冲后、画第三"：
                // `present` 要求 src 字节数与窗口尺寸逐像素相等。
                p.scale = self.scale;
                p.phys = (
                    to_phys(panel::CARD_W as f32, self.scale).max(1),
                    to_phys(panel::CARD_H as f32, self.scale).max(1),
                );
                if let Err(e) = client.resize(SurfaceKind::Panel, p.phys) {
                    eprintln!("panel: 换缩放后 resize 失败: {e} ⇒ 这块旧尺寸的面板不可用，关掉");
                    self.panel = Some(p);
                    self.close_panel_now(client);
                    return;
                }
                p.canvas = vec![0u8; (p.phys.0 * p.phys.1 * 4) as usize];
                p.force_draw = true;
            }
            let at = self.panel_at(&p);
            if p.draw(client, at, &error) {
                self.counters.panel_commits += 1;
            }
            self.panel = Some(p);
        }

        /// 面板**唯一**的行生产者。
        ///
        /// ⚠️ **不许在这里调 [`App::ensure_gacha`]**：连不上库那件事 `ensure_gacha` →
        /// [`App::gacha_bailed`] 已经出过一次声，这里再返回 `Err` 让调用方处理一遍，就是
        /// **同一次失败打两遍**（两条 stderr、错误行被覆写两次，而用户看不出哪条是真的）。
        /// 所以这条只问"已经连上了没有"，连不连是调用方的责任。
        ///
        /// 设置页不碰库 ⇒ 它永远 `Ok`；`Err` 只可能来自收藏册那三个 tab，而那一路唯一的使用者
        /// 是 [`App::open_panel`] 与 [`App::refresh_panel_rows`]，那里库必然已经连着。
        ///
        /// M7c 起 Owned 那一臂岔给 [`App::album_screen`]（逐件可点 + 分页），M7d 起宠物那一行
        /// 也真了（`Body(i)` = [`App::swap_body`]）。两家同形，判据同一个 [`Gacha::album_entries`]。
        fn panel_rows(&mut self, kind: PanelKind) -> Result<Vec<PanelRow>, String> {
            let lang = self.ui.lang;
            match kind {
                PanelKind::Settings => Ok(panel::settings_rows(lang, self.ui.motion)),
                PanelKind::Collection {
                    tab: CollectionTab::Owned,
                    page,
                } => self.album_screen(page).map(|(_kind, rows)| rows),
                PanelKind::Collection { tab, .. } => {
                    let g = self
                        .gacha
                        .as_ref()
                        .ok_or_else(|| "库还没连上（收藏册读不到东西）".to_string())?;
                    Ok(panel::collection_rows(
                        lang,
                        &g.tab_lines(tab, lang, self.scale)?,
                    ))
                }
            }
        }

        /// 「已获得」那一本的**一整屏** = 逐件行 + 汇总那五行 → 分页（Wayland 母本的同形物）。
        ///
        /// 为什么返回 `kind` 而不是只返回行：页码归一（末页回卷）只由 [`panel::album_page`] 做，
        /// 而存进 [`PanelState`] 的那一格必须就是归一后的页码 —— 存原始请求值的话，
        /// [`App::dispatch_panel`] 的 `kind.page() + 1` 会在回卷之后继续往上加，页码从此漂掉，
        /// 屏上却永远只有那几页（"两个事实源"的又一形状）。
        ///
        /// ⚠️ 汇总那五行是**追加**在逐件行之后，不是被替换掉：`have/all` 与碎片余额只有那一行
        /// 说得了，而逐件行回答的是"哪一件、能不能点"。代价（明写）= 汇总被挤到后面的页。
        ///
        /// 库存这一趟读了两遍（[`App::dress_view`] 内部一遍、这里一遍）：让 `DressView` 把
        /// `Owned` 带出来会搅浑"豁免态压根不该读库"那条形状，而这一屏要的是**报表**。两遍读的
        /// 是同一次点击之内的同一个 store ⇒ 同一个事实问了两次，不是两个事实源；代价 = 两次
        /// sqlite 查询，而它们不在 tick 环上（§4 禁的是定时轮询）。
        fn album_screen(&mut self, page: usize) -> Result<(PanelKind, Vec<PanelRow>), String> {
            if self.gacha.is_none() {
                return Err("库还没连上（收藏册读不到东西）".to_string());
            }
            // 名字要先取：下面 `dress_view()` 可变借用 self，不能同时留 `candidates` 的引用。
            let ids: Vec<String> = self.candidates.iter().map(|c| c.id.clone()).collect();
            // 本体那一张表**同形、同一条理由**（P5）：`pet_view()` 也要可变借用 self。
            let pet_ids: Vec<String> = self.bodies.iter().map(|b| b.id.clone()).collect();
            // 闸门只有一个所有者 = `dress_view`（这一条不许自己实现"许不许穿"）。
            let view = self.dress_view();
            // 本体那道闸门是它的同形兄弟（同一个 [`DressView::select`]，只是喂的表不同）⇒ 收藏册
            // 里"这件能不能点"与"这只能不能换"问的是**同一个判据**，不是两套答案。
            let pet_view = self.pet_view();
            let (lang, scale) = (self.ui.lang, self.scale);
            let g = self
                .gacha
                .as_ref()
                .ok_or_else(|| "库还没连上（收藏册读不到东西）".to_string())?;
            let owned = g.store.owned().map_err(|e| format!("读库存失败: {e}"))?;
            let mut rows = album_rows(
                &g.album_entries(
                    &owned,
                    &ids,
                    &view.visible,
                    &pet_ids,
                    &pet_view.visible,
                    lang,
                )?,
                scale,
            );
            rows.extend(
                g.collection_lines(lang)?
                    .into_iter()
                    .map(|text| panel::PanelRow {
                        item: panel::PanelItem::Info,
                        text,
                    }),
            );
            let screen = panel::album_page(lang, &rows, page);
            Ok((
                PanelKind::Collection {
                    tab: CollectionTab::Owned,
                    page: screen.page,
                },
                screen.rows,
            ))
        }

        /// 收藏册三本与"面板里点标题带"共用的入口：读行 → 打 stderr → 上屏。
        ///
        /// 一条而不是两条的理由是 §4 那条口径：**屏上那几行就是日志里那几行**。分两条就会出现
        /// "日志打了 5 行而屏上截断成 +N 更多"这种两边都自称正确的东西。
        fn collection_panel(&mut self, tab: CollectionTab, page: usize) {
            if tab == CollectionTab::Owned {
                self.collection_album(page);
                return;
            }
            self.gacha_panel(
                "collection",
                PanelKind::Collection { tab, page },
                move |g, l, s| g.tab_lines(tab, l, s),
            );
        }

        /// 「已获得」那一本（逐件 + 分页）的入口 = [`App::collection_panel`] 的 Owned 分支。
        ///
        /// `page` 是**请求的**页码，可以越界（末页点一下就是下一页）；归一在
        /// [`panel::album_page`] 里做，而这里把它归一后的结果原样交给 [`App::show_panel_rows`]
        /// —— 传请求值上屏就是"页码漂掉"那条形状，见 [`App::album_screen`] 那条注释。
        fn collection_album(&mut self, page: usize) {
            if !self.ensure_gacha() {
                return;
            }
            let screen = self.album_screen(page);
            self.show_panel_rows("collection", screen);
        }

        /// 「连库 → 读 → 上屏」的唯一路径：先 `ensure_gacha()`（惰性连库那条红线，D14）、
        /// 失败只写错误行（[`App::gacha_bailed`]，不 panic、不置退出，硬约束 3 同族）、
        /// 成功就把错误行清空（否则上一条失败的字一直挂着）。
        ///
        /// `scale` 在**这里**取而不是各分支自己取：它决定"一行的字装不装得下"，而这一条是
        /// 三屏唯一的出口 ⇒ 读屏那一步拿到的缩放与画那块面用的是同一个数。
        fn gacha_panel(
            &mut self,
            prefix: &str,
            kind: PanelKind,
            report: impl FnOnce(&Gacha, Lang, f32) -> Result<Vec<String>, String>,
        ) {
            if !self.ensure_gacha() {
                return;
            }
            let (lang, scale) = (self.ui.lang, self.scale);
            let lines = match self.gacha.as_ref() {
                Some(g) => report(g, lang, scale),
                None => return,
            };
            self.show_panel_lines(prefix, kind, lines);
        }

        /// 面板那两半的后半（行已经算好 → 打日志 → 上屏）的**行版**。
        ///
        /// ⚠️ 逐行打的时候**跳过 [`panel::PanelItem::Close`]**：那一格是"关闭"这个动作，不是
        /// 一行内容，而既有那几屏的生产者最多给 5 行 ⇒ 加进 `rows` 的只有它 ⇒ 不打它就等于
        /// 旧路径（只打日志的年代）的 stderr 文本**一个字节都不变**。
        fn show_panel_rows(
            &mut self,
            prefix: &str,
            screen: Result<(PanelKind, Vec<PanelRow>), String>,
        ) {
            match screen {
                Ok((kind, rows)) => {
                    self.ui_error = None;
                    for r in &rows {
                        if r.item != panel::PanelItem::Close {
                            eprintln!("{prefix}: {}", r.text);
                        }
                    }
                    self.show_panel(kind, rows);
                }
                Err(msg) => self.gacha_bailed(msg),
            }
        }

        /// `Vec<String>` 那一版的适配器 = "一串字" → "全是只读行" → [`App::show_panel_rows`]。
        ///
        /// 留着它只有一个理由：「还缺」/「最近」/概率/复核这几屏的行**本来就没有可点的**，
        /// 强行套进行结构会让每条报表先变成 `PanelRow` 再变回字。M7c 的收藏册不走这条。
        fn show_panel_lines(
            &mut self,
            prefix: &str,
            kind: PanelKind,
            lines: Result<Vec<String>, String>,
        ) {
            let lang = self.ui.lang;
            self.show_panel_rows(
                prefix,
                lines.map(|v| (kind, panel::collection_rows(lang, &v))),
            );
        }

        /// 面板开着的行**跟着 `App::lang`/`App::motion` 重算**（点语言行、点动画行之后调）。
        ///
        /// 这里不置 `force_draw`：`lang` 与 `motion.code()` 都烘进行文案里，而行文案与 `lang`
        /// 都进签名 ⇒ 闸门自己认得出（把会变的东西交给闸门去认，而不是靠调用方在每个改它的
        /// 地方补一句"记得重画"）。重算按**当前的 `kind`**：从「概率」/「复核」借走的那一屏
        /// 换回来的是默认五行。
        fn refresh_panel_rows(&mut self) {
            let Some(kind) = self.panel.as_ref().map(|p| p.kind) else {
                return;
            };
            if let Ok(rows) = self.panel_rows(kind) {
                self.show_panel(kind, rows);
            }
        }

        /// 公示概率那五行：见 [`overlay_report::published_rates_lines`]（不碰库）。
        fn rates_lines(&self) -> Result<Vec<String>, String> {
            published_rates_lines(self.ui.lang)
        }

        /// 点"动画"那一行：`full → reduce → off → full`，**本进程立刻生效 + 当场写回**（M7e），
        /// 与 [`App::cycle_lang`] 同一条路（`[ui] motion` 是 `write_ui_key` 认的两个键之一）。
        ///
        /// ⚠️ **不给菜单置 `force_draw`，也不是漏了**：`motion` 故意**不在** `menu::content_sig`
        /// 里，而 `menu_pixels_ignore_the_motion_tier` 那条门禁断言的是"三档下菜单像素逐字节相同"
        /// —— 这一行改的是**卡片动画**（burst 时长），不是此刻这张菜单。补那一句就是在
        /// "静止菜单恒 1 提交"那块门面上重造 `69fd158`。这里只把 `MenuState::motion` 跟上新值：
        /// 它是 `menu::after_pull` 的输入（下一次抽取的 burst 时长），不是"此刻这张菜单要重画"的理由。
        ///
        /// 面板反过来：设置面**确实要重算行**，因为 `settings_rows` 把 `motion.code()` 烘进那一行
        /// 的正文，而正文进签名 ⇒ [`App::refresh_panel_rows`] 换完行，下一 tick 闸门自己放行。
        fn cycle_motion(&mut self) {
            let next = self.ui.motion.next();
            let from_cli = self.ui.motion_from == settings::ValueSource::Cli;
            self.ui.motion = next;
            self.ui.motion_from = settings::ValueSource::MenuClick;
            if let Some(m) = self.menu.as_mut() {
                m.motion = next;
            }
            self.refresh_panel_rows();
            eprintln!(
                "ui: 抽卡动画 → {}（下一次抽取的 burst 才跟着变，本体动画不受它影响）",
                next.code()
            );
            self.persist_ui_key(petstore::config::KEY_MOTION, next.code(), from_cli);
        }

        /// 面板被点：命中哪一格、那一格办哪件事（Wayland `on_panel_press` 的本平台版）。
        ///
        /// 分派**只走 [`panel::press`]**（命中 + `actionable` 把关，`Info` 在命中侧就被滤掉），
        /// 把关放在命中侧而不是各分支里的理由那条已经付过学费（门禁
        /// `an_info_row_is_hit_but_never_activates_...` 在共用层，两家共用）。所以这里的
        /// `Info` 臂**不可达**，写出来是为了以后加一种行而忘了决定它点不点得着时，
        /// `match` 会红而不是静默默认成"不可点"。
        ///
        /// 哪些格真、哪些格欠（M7d 口径：**这一面上没有欠的行**，逐臂写明去向）：
        /// * `Header`（收藏册换 tab / 设置面回默认五行，也就是「概率」「复核」借走
        ///   这一面时唯一的回程）、`More`（翻页；Owned 那一本真的产这一格，见
        ///   [`App::album_screen`]）、`Wear(i)` = [`App::pick_and_wear`]、
        ///   `Body(i)` = [`App::swap_body`]（M7d 的 P5）、`Lang`、`Motion`、
        ///   `Rates`、`Verify`、`Close`。
        ///
        /// ⚠️ 两套下标**刻意不合并**：`Wear` 数 [`App::candidates`]（磁盘配件），`Body` 数
        ///   [`App::bodies`]（本体库）—— 一个数字两个事实源就是
        ///   `the_album_routes_pets_to_the_body_index_space` 要拦的形状。
        ///
        /// ⚠️ 这两臂都**只能在回调里做纯业务**（像素、清单、账）：动窗口要 `&mut Client`，
        ///   而派发发生在 `pump` 内部 ⇒ 面板留在原地、行由 [`App::refresh_panel_rows`] 重算，
        ///   那条函数走的仍是 `show_panel` 的"记旗"那一支。
        fn dispatch_panel(&mut self, item: panel::PanelItem) {
            let Some(kind) = self.panel.as_ref().map(|p| p.kind) else {
                return;
            };
            match item {
                panel::PanelItem::Info => {}
                panel::PanelItem::Header => match kind.cycle().tab() {
                    Some(tab) => self.collection_panel(tab, 0),
                    None => self.open_panel(PanelKind::Settings),
                },
                panel::PanelItem::More => match kind.tab() {
                    Some(tab) => self.collection_panel(tab, kind.page() + 1),
                    None => self.open_panel(PanelKind::Settings),
                },
                panel::PanelItem::Wear(i) => self.pick_and_wear(i),
                panel::PanelItem::Body(i) => self.swap_body(i),
                panel::PanelItem::Lang => self.cycle_lang(),
                panel::PanelItem::Motion => self.cycle_motion(),
                panel::PanelItem::Rates => {
                    let lines = self.rates_lines();
                    self.show_panel_lines("rates", PanelKind::Settings, lines);
                }
                panel::PanelItem::Verify => {
                    self.gacha_panel("verify", PanelKind::Settings, |g, l, s| {
                        g.verify_lines(l, s)
                    });
                }
                panel::PanelItem::Close => self.close_panel(),
            }
        }

        /// 第一次用到才连库，之后复用同一条连接。返回"库是否可用"，调用方据此决定余额读不读。
        ///
        /// 失败**只写错误行**：菜单照样开得出、宠物照样跑（硬约束 3 同族 —— UI 的故障不许
        /// 波及宠物，更不许波及桌面）。
        ///
        /// ⚠️ 这是**惰性连接**：启动那发 `open_at` 只证 schema（M4 冒烟），它的成败不代替这里 ——
        /// 余额与 M6 抽取、M5e 换装读的都是这一条活到退出的连接。
        fn ensure_gacha(&mut self) -> bool {
            if self.gacha.is_some() {
                return true;
            }
            match Gacha::connect(self.opts.db.as_deref()) {
                Ok(g) => {
                    eprintln!(
                        "gacha: 库已就绪（{}）",
                        self.opts.db.as_deref().unwrap_or("内存库：不写盘")
                    );
                    self.gacha = Some(g);
                    self.ui_error = None;
                    true
                }
                Err(msg) => {
                    self.gacha_bailed(msg);
                    false
                }
            }
        }

        /// 抽取层的出声处：stderr + 菜单共用的错误行。**不 panic、不置 `exit`**。
        fn gacha_bailed(&mut self, msg: String) {
            eprintln!("gacha: {msg}");
            self.ui_error = Some(msg);
        }

        /// UI 侧（非抽取）的出声处，与 [`App::gacha_bailed`] 同一条纹路：stderr + 那一行错误。
        /// 分两个函数只为了**前缀不说谎**：配置写回的失败打成 `gacha: …` 会把排查的人支到
        /// 数据库那边去。
        fn ui_bailed(&mut self, msg: String) {
            eprintln!("ui: {msg}");
            self.ui_error = Some(msg);
        }

        /// 点设置行 = **当场写回 `config.toml`**（M7e 转真；与 Wayland 母本同一条路）。
        ///
        /// 本平台要额外记住的只有**那一条链**：`write_ui_key` 与 `Config::read()` 共用
        /// `petstore::config::config_path()` 那三档阶梯（`%LOCALAPPDATA%` → `%APPDATA%` →
        /// `%USERPROFILE%`），所以"写回哪一个文件"这句话**只能由 writer 返回的那个 `PathBuf`**
        /// 说，不在 `App` 里重算一遍 —— 两处各算一次早晚漂移成两个文件，而用户会改哪一个全看运气。
        ///
        /// · **成功**只念 writer 返回的路径；消音那一条错误行是既有约定（`ensure_gacha`、
        ///   `show_panel_lines`、`do_pull` 三处各自清空它）⇒ 这一行的语义是"最近一次失败"，
        ///   不是"某个一直没修好的东西"；
        /// · **失败**要把"本进程已经改了"一起说清 —— 只报"失败"会让人以为什么都没变，而屏上
        ///   那一行此刻写的就是新值；
        /// · **优先关系不因写回而改变**：`--lang`/`--motion` 高于文件（§4.6 那条链），从命令行
        ///   启动的那次要明说"不带参数重启才看得见"，不然用户会以为点了等于没生效。
        fn persist_ui_key(&mut self, key: &str, value: &str, from_cli: bool) {
            match petstore::config::write_ui_key(key, value) {
                Ok(path) => {
                    self.ui_error = None;
                    eprintln!(
                        "ui: {key} = \"{value}\" 已写回 {}{}",
                        path.display(),
                        if from_cli {
                            "（本次启动的值来自命令行参数，它高于文件 ⇒ 不带参数重启才读得到这一档）"
                        } else {
                            "（下次启动读这一档）"
                        }
                    );
                }
                Err(msg) => self.ui_bailed(format!(
                    "写回 {key} 失败：{msg} ⇒ 本次会话仍然生效，下次启动不会"
                )),
            }
        }

        fn gacha_balance(&self) -> i64 {
            self.gacha.as_ref().map(|g| g.balance()).unwrap_or(0)
        }

        /// 点"抽一次 / 十连"（M6）。**同步抽完**：Wayland 那侧实测单抽 0.61 ms、十连 0.85 ms，
        /// 都远在 16.6 ms 的 tick 预算里，而引一条线程只会多出"结果落在哪个 tick"这一族新竞态
        /// （D14 原文）。
        ///
        /// 三个落点，缺一个就是一句谎话：
        /// · `m.anim` —— 卡面从哪一格起。[`menu::after_pull`] 是共用层的纯函数，`--motion off`
        ///   那一档**根本不进 `Burst`**（不是"在 Burst 里画 0 帧"），理由写在那儿自己的注释里；
        /// · `m.balance` —— 抽完的余额。余额只在"开菜单"与"抽完"各刷一次，**不是每 tick 查库**
        ///   （硬约束 5 的事件驱动口径）；
        /// · `Command::DoubleTap` —— 宠物本体那 3 帧 react 叠加动画。复用既有指令而不新增
        ///   `Command`：碰 `machine.rs` 就是威胁状态指纹与 §3.3 那张迁移表。它附带"着地且允许
        ///   run 则起跑"的副作用，那是 M3-b 明写接受的代价，不是这里的 bug。
        fn do_pull(&mut self, count: u32) {
            if !self.ensure_gacha() {
                return;
            }
            let dry_run = self.dry_run;
            // 语言与缩放都在这一刻取：文案与截断预算属于**这张卡**，之后切语言/换显示器
            // 不许重排已经挂着的卡（§7-R10，与 Wayland 同一句）。
            let lang = self.ui.lang;
            let scale = self.menu.as_ref().map(|m| m.scale).unwrap_or(self.scale);
            let pulled = match self.gacha.as_mut() {
                Some(g) => g.pull(count, dry_run, lang, scale),
                None => return,
            };
            let (lines, best, balance) = match pulled {
                Ok(v) => {
                    self.ui_error = None;
                    v
                }
                // 库被别的进程锁住、公示与库里对不上…… 都只出声，不停宠物（硬约束 3）。
                Err(msg) => {
                    self.gacha_bailed(msg);
                    return;
                }
            };
            if let Some(m) = self.menu.as_mut() {
                m.anim = menu::after_pull(best, lines, self.ui.motion);
                m.balance = balance;
            }
            if let Some(sim) = self.sim.as_mut() {
                sim.send(Command::DoubleTap);
            }
        }

        /// 卡片错误行：菜单签名的输入之一。`None` = 空串（屏上那一格不留白也不假装有字）。
        fn error_line(&self) -> String {
            self.ui_error.clone().unwrap_or_default()
        }

        /// 这一轮**开发态豁免**是否成立 = `--allow-unowned-accessories` 且真的有 `--db`。
        ///
        /// 判据函数与 Wayland **共用**（[`dress_exempt_for`]），所以"给了旗却没给库"那一格
        /// （演示态）在两平台必然同一个答案：放行（库存那道无从可查）但**不**点帽子页那个 `*`。
        /// 放一处而不摊进 `open_menu` / `dress_view` 两处：它是两个启动参数的函数，
        /// 两处各写一遍就会出现"标打了而没放行"或反过来。
        fn dress_exempt(&self) -> bool {
            dress_exempt_for(self.opts.allow_unowned_accessories, self.dry_run)
        }

        // ───────────────────────── 换装（M5e）─────────────────────────

        /// 换装页"这一件许不许穿"的**库存那道**（P2）。逐字照 Wayland `App::dress_view`，
        /// 只少了它那句"名字要先取"的注释底下的同一件事。
        ///
        /// **顺序 = 先磁盘后库存**：[`App::candidates`] 已经过完磁盘那两道，这一道只问
        /// "库存里有没有这一件"。反过来做（从库存出发再查磁盘）得到同一批件，但"这件不匹配本体"
        /// 与"你没有这件"会混成一句 —— 那是两句不同的话，都要能打到。
        ///
        /// 只在点击回调与启动恢复那一刻调（硬约束 5 禁止定时轮询；D14"余额是事件驱动的"同形状）。
        ///
        /// ⚠️ **失败关闭**：给了 `--db` 而库连不上/读不动 ⇒ 可见集为**空**、不放行。
        fn dress_view(&mut self) -> DressView {
            // 名字要先取：下面 `ensure_gacha()` 可变借用 self，不能同时留 `candidates` 的引用。
            let ids: Vec<String> = self.candidates.iter().map(|c| c.id.clone()).collect();
            if self.dress_exempt() || self.dry_run {
                // 两档放行合成一次调用，但**屏上记号不同**：豁免态点亮帽子页那个 `*`（判据 =
                // [`App::dress_exempt`]），演示态不点 —— 右端已有「演示」标，同一件事给两个记号
                // 就是两个事实源（Wayland 那侧 2026-11-06 的拍板，本平台照抄）。
                return DressView::select(true, None, &ids);
            }
            if !self.ensure_gacha() {
                // 连不上库那件事 `ensure_gacha` → `gacha_bailed` 已经出过一次声，这里只负责不出声地
                // 关死，不再重复播报。
                return DressView::select(false, None, &ids);
            }
            let owned = match self.gacha.as_ref().map(|g| g.store.owned()) {
                Some(Ok(o)) => Some(o),
                Some(Err(e)) => {
                    eprintln!(
                        "ui: 换装 ⇒ 读库存失败（{e}）⇒ 未验过的配件一律不放行（库里有什么这轮读不出）"
                    );
                    None
                }
                // 走到这里 `ensure_gacha()` 刚返回真，`gacha` 理论上是 `Some` ⇒ 这一支不该存在。
                // 留着而不是 `expect`：不可达分支不该赌一次 panic（硬约束 3）。
                None => None,
            };
            DressView::select(false, owned.as_ref(), &ids)
        }

        /// 本体这一档的**开发态豁免** = `--allow-unowned-pets` 且真的有 `--db`。
        ///
        /// ⚠️ 它与 [`App::dress_exempt`] **共用同一个判据函数**（[`dress_exempt_for`]），输入是
        /// 两个不同的开关。合用一根的代价在 Wayland 那边算过：配件的屏上记号是帽子页标题的 `*`，
        /// 而那一页的列表里**根本没有宠物** ⇒ 两根合并必然出现"标打了而没放行"。所以本体这一档
        /// **没有屏上记号**（拍板④），只有这一处判据 + 启动/换上去那两行 stderr。
        fn pet_exempt(&self) -> bool {
            dress_exempt_for(self.opts.allow_unowned_pets, self.dry_run)
        }

        /// 收藏册宠物行的**库存那道** = [`App::dress_view`] 的同形兄弟，喂的是 [`App::bodies`] 的
        /// id。判定共用 [`DressView::select`]（三档输入那一层），所以"未拥有的件不出现"与"未拥有的
        /// 本体点不动"从头是**同一判据**，不会有两套答案。
        ///
        /// ⚠️ **失败关闭**：给了 `--db` 而库存读不出 ⇒ 可见集为空。宁可这一轮换不了本体而 stderr
        /// 有一行解释，也不要"没验过拥有就换成那只"。
        ///
        /// ⚠️ 只在点击回调里调（收藏册那一行点下去、以及 [`App::album_screen`] 给不给下标那一刻），
        /// 不轮询。与 [`App::dress_view`] 各自开一次库不是两个事实源：库是事实源，两次读的是同一张表。
        fn pet_view(&mut self) -> DressView {
            // 名字要先取：下面 `ensure_gacha()` 可变借用 self，不能同时留 `bodies` 的引用。
            let ids: Vec<String> = self.bodies.iter().map(|b| b.id.clone()).collect();
            if self.pet_exempt() || self.dry_run {
                return DressView::select(true, None, &ids);
            }
            if !self.ensure_gacha() {
                return DressView::select(false, None, &ids);
            }
            let owned = match self.gacha.as_ref().map(|g| g.store.owned()) {
                Some(Ok(o)) => Some(o),
                Some(Err(e)) => {
                    eprintln!(
                        "ui: 换本体 ⇒ 读库存失败（{e}）⇒ 未验过的本体一律不放行（库里有什么这轮读不出）"
                    );
                    None
                }
                // 走到这里 `ensure_gacha()` 刚返回真 ⇒ 这一支理论上不可达，留着而不是 `expect`
                // （配件那侧同一条理由，写在 [`App::dress_view`]：不可达分支不该赌一次 panic）。
                None => None,
            };
            DressView::select(false, owned.as_ref(), &ids)
        }

        /// 点"穿上"。三条早退 + 一次装载，逐字照 Wayland `App::equip`（差别只有最后那行的措辞：
        /// 本平台**没有"换穿透轮廓"这一步**，命中跟着像素走，所以那一格写的是"重画当前格"）。
        fn equip(&mut self) {
            let view = self.dress_view();
            if self.worn.is_some() {
                eprintln!("ui: 穿上 ⇒ 已经穿着（穿的就是当前选中这件），不动");
                return;
            }
            if self.candidates.is_empty() {
                eprintln!("ui: 穿上 ⇒ 库里没有可用配件 ⇒ 换装页点了不会有效果");
                return;
            }
            // P2 那道：**没验过拥有就不解像素**（不是"解完再把结果丢掉"，那要白付一次解码与几 MiB）。
            if !view.visible.contains(&self.picked) {
                eprintln!(
                    "ui: 穿上 ⇒ 选中 {} 但这一轮不许穿（库里 {} 件 · 可穿 {} 件 · 因未拥有出局 {} 件）\
                     ⇒ 停在未装备态；开发态要放行就加 --allow-unowned-accessories（屏上会打 `*`）",
                    self.candidates[self.picked].slug,
                    self.candidates.len(),
                    view.visible.len(),
                    view.withheld
                );
                return;
            }
            match self.wear(self.picked) {
                Ok(slug) => eprintln!("ui: 穿上 {slug} ⇒ 重画当前格（命中跟着像素走）"),
                Err(e) => eprintln!("ui: 穿上失败 ⇒ {e}（停在未装备态）"),
            }
        }

        /// 点"脱下"：丢掉配件那两张字节，下一格只画本体。
        ///
        /// Wayland 那条"轮廓换回本体那份"在这里**不是一个动作**：本平台的命中读的就是叠完之后的
        /// 那份像素（`stage.rs` 模块头），所以像素回到本体 = 轮廓自动回到本体，结构上没有
        /// "帽子没了而那块还点得到"这种中间态可以忘记处理。
        fn unequip(&mut self) {
            let Some((slug, bytes)) = self.unwear() else {
                eprintln!("ui: 脱下 ⇒ 本来就没穿，不动");
                return;
            };
            eprintln!(
                "ui: 脱下 {slug} ⇒ 配件 sheet（{:.3} MiB 解码）已释放，下一格起只剩本体",
                bytes as f64 / (1024.0 * 1024.0)
            );
        }

        /// 点"换一件"：在 [`App::candidates`] 里把下标往前挪一格，**穿着时立刻换过去**。
        ///
        /// **没穿东西时屏上一个像素都不变**，所以这里**不置 `force_draw`、也不进 `content_sig`**
        /// （见 [`App::picked`] 那条注释）。反馈只有 stderr：选中那件叫什么 = 用户内容，
        /// 子集字体盖不住 ⇒ 宁可不打字也不在卡片上画 `?`。
        ///
        /// ⚠️ 穿着时**先释放再装载**，两个理由原样继承 Wayland：
        /// * 只改下标会把"选中的"和"屏上的"分成两个事实，而用户拿它们对不上时没有任何一行读数
        ///   能告诉我是哪一步没跟上。
        /// * 顺序 = [`App::take_worn`] 在前、`decode` 在后 ⇒ 峰值始终是一件。那边 2026-11-03 真机
        ///   `VmHWM 148,076 kB`（本体 45 + 旧 45 + 新 45）量的就是反过来做的代价，而本平台的
        ///   RSS 档线同样是**高水位**（`K32GetProcessMemoryInfo` 的 `PeakWorkingSetSize`）⇒
        ///   同一条算式在这里同样成立，`take_worn` 也因此只带回 `(slug, bytes)` 而不是那个对象。
        fn cycle_accessory(&mut self) {
            let view = self.dress_view();
            if self.candidates.is_empty() {
                eprintln!(
                    "ui: 换一件 ⇒ 库里没有可用配件（上面 `accessories:` 那几行点名了每一件为什么出局）"
                );
                return;
            }
            if view.visible.is_empty() {
                eprintln!(
                    "ui: 换一件 ⇒ 库里有 {} 件而库存说这一件都不拥有 ⇒ 换装页不出现任何一件\
                     （要开发态放行就加 `--allow-unowned-accessories`，那条会在屏上打 `*`）",
                    self.candidates.len()
                );
                return;
            }
            // 循环序 = `visible` 的升序（= 目录序）。`picked` 不在其中是**合法状态**而不是 bug：
            // 它上一次被选中时也许还在豁免态里，而现在库存那道把它划掉了 ⇒ 落到第一个可见格。
            let pos = view.visible.iter().position(|&i| i == self.picked);
            self.picked = match pos {
                Some(p) => view.visible[(p + 1) % view.visible.len()],
                None => view.visible[0],
            };
            let shown = view
                .visible
                .iter()
                .position(|&i| i == self.picked)
                .expect("刚赋的值必然来自这张表")
                + 1;
            let total = view.visible.len();
            let slug = self.candidates[self.picked].slug.clone();
            let withheld = view.withheld;
            let note = if withheld == 0 {
                String::new()
            } else {
                format!(" · 另有 {withheld} 件因未拥有不出现")
            };
            if self.worn.is_none() {
                eprintln!(
                    "ui: 换一件 ⇒ 选中 {slug}（第 {shown}/{total} 件）{note}；本来就没穿，屏上不变"
                );
                return;
            }
            // `take_worn` 只带回打印要用的两个数 ⇒ 旧那张 sheet 在此刻已经不在内存里。
            let (freed_slug, freed_bytes) = self.take_worn().expect("上面查过 worn.is_some");
            eprintln!(
                "ui: 换一件 ⇒ 释放 {freed_slug}（{:.3} MiB）、装载 {slug}",
                freed_bytes as f64 / (1024.0 * 1024.0)
            );
            match self.wear(self.picked) {
                Ok(worn) => {
                    eprintln!("ui: 换一件 ⇒ 已穿上 {worn}（第 {shown}/{total} 件）{note}");
                }
                Err(e) => eprintln!("ui: 换一件 ⇒ 选中 {slug} 但装载失败：{e}（停在未装备态）"),
            }
        }

        /// 收藏册点名一件 ⇒ **直接穿上**（P4 = Wayland 那边用户 11-07 拍板②，本平台同判词）。
        /// 第 4 个 [`Self::wear`] 调用点，也是唯一一个"下标来自屏上一行"的。
        ///
        /// 为什么不借道 [`Self::equip`]（这条反直觉的理由在母本已经付过学费，这里整段照抄）：
        /// `equip` 的第一条出口是"已经穿着 ⇒ 当场返回"，而它成立靠的是**"穿着的就是 `picked`
        /// 那一件"这条不变量**。收藏册要点的是**任意一格**，那条不变量在这一条路上还没建立就
        /// 要求它 ⇒ 借道 `equip` 的话，"身上是 A、点的是 B"会读成"已经穿着"⇒ **静默不动 +
        /// 一句谎话**。所以顺序是：
        ///
        /// 1. `dress_view()` —— 所有权闸**照旧只有一个所有者**，这一条不自己实现"许不许穿"；
        /// 2. 判 [`overlay_common::dress::plan_pick_wear`]（**比 `picked` 必须在这里，任何写入之前**）；
        /// 3. `SelectAndWear` 时**先** [`Self::take_worn`] 释放身上那件，**再**写 `picked`、再 [`Self::wear`]。
        ///
        /// 3 里"先释放"是 M1 内存档的字面要求（配件 sheet 每件 45.000 MiB，11-23 的 Wine 那轮
        /// 实测过），而"释放排在写 `picked` 之前"是为了让 2 的判据读到的还是**用户点之前**那个世界。
        /// 用 `take_worn` 不用 [`Self::unwear`]：后者会先把库里那一格 DELETE 再写回来，白写一次
        /// 且留一个"库里没有"的可见窗口。
        ///
        /// **面板不关**：[`panel::placement`] 优先右/左/上/下不压宠物 ⇒ 换的像素看得见，而关掉会
        /// 连 `page` 一起丢（用户翻页翻到第 3 页点一件，屏跳回第一页 = 白翻）。这一条也不重画
        /// 面板：本轮面板的行**一个字没变**（选中了什么不上屏 = 用户 11-07 拍板③）。
        fn pick_and_wear(&mut self, i: usize) {
            let view = self.dress_view();
            match plan_pick_wear(
                view.visible.contains(&i),
                self.worn.is_some(),
                self.picked,
                i,
            ) {
                PickWear::Refused => {
                    // 屏上这一行是 `Wear(i)` 才被点到的，而 `Wear(i)` 是产行的那一刻按同一道闸
                    // 给的下标 ⇒ 走到这里只能是**两次点击之间**世界变了（库被另一进程改了 / 磁盘
                    // 产物被删了）。不赌 panic，也不"顺手穿上"：失败关闭。
                    eprintln!(
                        "ui: 收藏册 ⇒ 点名第 {i} 格而这一轮不许穿（可穿 {} 件 · 因未拥有出局 {} 件）⇒ 不动",
                        view.visible.len(),
                        view.withheld
                    );
                }
                PickWear::AlreadyWorn => {
                    eprintln!("ui: 收藏册 ⇒ 身上已经是第 {i} 格这一件，不解像素、不重投帧");
                }
                PickWear::SelectAndWear => {
                    if let Some((slug, bytes)) = self.take_worn() {
                        eprintln!(
                            "ui: 收藏册 ⇒ 先释放 {slug}（{:.3} MiB 解码）⇒ 峰值始终是一件",
                            bytes as f64 / (1024.0 * 1024.0)
                        );
                    }
                    self.picked = i;
                    match self.wear(i) {
                        Ok(slug) => eprintln!("ui: 收藏册 ⇒ 穿上 {slug}（点名自第 {i} 格）"),
                        Err(e) => {
                            eprintln!("ui: 收藏册 ⇒ 点名第 {i} 格而装载失败：{e}（停在未装备态）")
                        }
                    }
                }
            }
        }

        /// 换成另一只本体（P5 = 收藏册里点宠物行那一件事）。**唯一入口**。
        ///
        /// Wayland 母本（`overlay-wayland/src/app.rs` 的 `swap_body`）的九步在这里是**八步**：
        /// 少的只有第 3 步"换穿透轮廓"。本平台没有 `Regions` 消费者（命中读的是叠完之后的
        /// canvas alpha，见 `stage.rs` 模块头）⇒ "新清单 + 旧轮廓"这个半换形状在本平台
        /// **没有对应的数据结构**，不是省了一步。其余每一步的位置与理由逐字继承，包括
        /// "判定全在写入之前"和"账排最后"。
        ///
        /// ⚠️ **顺序就是它的全部安全性**，两条最承重的：
        /// * 像素只有一个写点：同段形走 [`AtlasSet::reload_segments`]（内部先逐张读头再解，任何一张
        ///   对不上都在**碰像素之前**退出），不同段形走 [`AtlasSet::load`]。两条都是"几何自检排在
        ///   第一笔像素写之前" ⇒ 半换形状构造不出来。
        /// * 第 2 步（先释放帽子）排在第 3 步之前不是顺手：反过来就瞬间有"旧本体 + 新本体 + 帽"
        ///   在场，而 ≈147 MB **仍落在 ≤200 MiB 门禁内 ⇒ 什么都不会红**。所以这条防线的读数是
        ///   两处：验收量一次 `VmHWM`，以及源级门禁数 `set.reload_segments` = 1 / `AtlasSet::load` = 2
        ///   （needle **不带**左括号：这条门禁数的是 `code.matches(…)`，注释与代码同张纸，写了括号
        ///   就是把自己数进去）+ 次序（`take_worn` 排在取像素之前、`fits` 排在 `reload_segments` 之前）加"整只重解那一臂
        ///   必须打「**峰值两只本体**」"（`dress.rs` 的 `the_pixels_and_the_ledger_are_written_by_one_function_each`）。
        ///
        /// 非目标（写在这里以免被读宽）：**不做**"换回默认本体"这一档 ⇒ `body` 行永远只有 UPSERT、
        /// 没有 DELETE（换本体是"换到某一只"，不是"脱下"；屏幕上任何时刻都有一只本体）。
        fn swap_body(&mut self, i: usize) {
            // —— 第 1 步：四道判完，**任何写入之前**（P4 在 [`Self::pick_and_wear`] 立的那条不变量）。
            let view = self.pet_view();
            let gate = view.visible.contains(&i);
            let in_range = i < self.bodies.len();
            let same_grid = self
                .bodies
                .get(i)
                .is_some_and(|b| b.grid == self.manifest.grid);
            let same_id = self.bodies.get(i).is_some_and(|b| b.id == self.body_id);
            // 名字取在判定之前，只为让拒绝那一行点得出名（用户内容 ⇒ 只进 stderr，不上屏）。
            let name = self
                .bodies
                .get(i)
                .map(|b| b.id.clone())
                .unwrap_or_else(|| format!("第 {i} 格"));
            match plan_swap_body(in_range, gate, same_id, same_grid) {
                SwapBody::Refused(why) => {
                    eprintln!("ui: 换本体 ⇒ 拒绝 {name}：{why} ⇒ 本体、帽子、`equipped` 一律不动");
                    return;
                }
                SwapBody::Same => {
                    eprintln!("ui: 换本体 ⇒ 身上已经是 {name} ⇒ 不解像素、不重投帧、账也不写");
                    return;
                }
                SwapBody::Switch => {}
            }
            // 四道都过 ⇒ 这一格必然在库里。仍用 `get`：取不到就多打一行后返回，不赌一次 panic
            // （硬约束 3：宠物崩溃不能影响桌面）。
            // ⚠️ 取的是**目录**不是某张 sheet：文件名归清单（[`Manifest::sheet_segments`]），这里
            // 拼死一个名字就是第二个事实源（旧形状 = `b.sheet()` 写回 `sheet.webp`）。
            let Some((pet_slug, dir)) = self.bodies.get(i).map(|b| (b.slug.clone(), b.dir.clone()))
            else {
                eprintln!("ui: 换本体 ⇒ 判定放行而 {name} 这一格取不到 ⇒ 不动（判据与库对不上）");
                return;
            };

            // —— 第 2 步：先释放帽子。身份要在这一步**之前**取全：`Worn` 只带清单（`slug`/`slot`），
            // 而"这一件是谁"（`id` = 目录名 = 库存与 `equipped` 认的那个键）记在 [`Candidate`] 那一侧，
            // 第 5 步重建候选表之后就查不到了。读 `candidates[picked]` 依赖的是那条既有不变量
            // "**穿着 ⇒ 穿的就是当前选中这件**"（四个装载点共同维持，见 [`Self::pick_and_wear`]
            // 那条注释）；`id` 取不到时第 6 步不猜，直接按"出局"处理。
            struct WornHat {
                id: Option<String>,
                slot: String,
                slug: String,
            }
            let worn_hat = self.worn.as_ref().map(|w| WornHat {
                id: self.candidates.get(self.picked).map(|c| c.id.clone()),
                slot: w.manifest.slot.to_string(),
                slug: w.manifest.slug.clone(),
            });
            if let Some((slug, bytes)) = self.take_worn() {
                eprintln!(
                    "ui: 换本体 ⇒ 先释放帽子 {slug}（{:.3} MiB 解码）⇒ 峰值始终是一只本体 + 一顶帽",
                    bytes as f64 / (1024.0 * 1024.0)
                );
            }

            // —— 第 3 步：解新本体的像素（母本的第 4 步）。这一步之后就没有可回头的失败了。
            // 清单**先取**（[`BodyCandidate::leaked`] 只泄几 KB 元数据、一像素不碰 ⇒ 排在"可回头"这侧
            // 安全），因为段表只能从清单读；**赋给 `self.manifest` 仍是第 4 步那唯一一处**（那条赋值
            // 由形状门禁数着，"新清单 + 旧像素"必须只有一个落点）。
            let m = self.bodies[i].leaked();
            match self.atlas.as_mut() {
                Some(set) => {
                    // 两条路，按**段形是否相同**分岔（`fits` 是那条判据），与 Wayland 同形：
                    // * 相同 ⇒ 逐段原地换 ⇒ 峰值 = 一只本体，与单张时代同一个数。
                    // * 不同（单张 ↔ 分段、或 2+2 ↔ 1+3）⇒ 原地接不住，只能整只 `load`。⚠️ 那一刻
                    //   内存里是**两只**本体（≈147 MB），它落在 ≤200 MiB 门禁内 ⇒ 没有任何测例会为它
                    //   变红，所以那一行日志是它唯一的读数；在册产物切法相同，正常轮不走到这里。
                    let swapped: Result<(), String> = if set.fits(m) {
                        set.reload_segments(&dir, m).map_err(|e| e.to_string())
                    } else {
                        match AtlasSet::load(&dir, m) {
                            Ok(fresh) => {
                                eprintln!(
                                    "ui: 换本体 ⇒ {name} 的 sheet 切法与当前本体不同（{} 段 → {} 段）\
                                     ⇒ 整只重解，**峰值两只本体**",
                                    set.len(),
                                    fresh.len(),
                                );
                                *set = fresh;
                                Ok(())
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    };
                    if let Err(e) = swapped {
                        eprintln!(
                            "ui: 换本体 ⇒ {name} 的 sheet 解不开（{e}）⇒ **本体没换**，屏上仍是 {cur}\
                             （帽子已在第 2 步释放，库里那一行还在：点「穿上」就回来，重启也会回来）",
                            cur = self.body_id
                        );
                        // ⚠️ 这里**必须**补一次重投：`take_worn` 只丢字节、不置 `force_draw`
                        // （它的注释明写"由调用方决定用户要不要看见"），而此刻屏上留着的是**已经不在
                        // 内存的那顶帽子**的像素 ⇒ 不重画就是"像素里有帽子而库里说没穿"那格点不到的
                        // 状态，且它要留到下一次格子真变才被洗掉。
                        self.force_draw = true;
                        return;
                    }
                }
                None => {
                    // 走到这里意味着 `bodies` 非空而 `atlas` 是 `None` —— 启动里那两个条件是
                    // 同一个（`pixels`），所以这是"以后有人绕过 `startup` 构造 App"的形状。
                    // 报一行、不动，而不是 `unwrap`（硬约束 3）。
                    eprintln!("ui: 换本体 ⇒ 这一轮没有图集像素（`--test-rect` 那条路）⇒ 本体不动");
                    return;
                }
            }

            // —— 第 4 步：提交元数据。**唯一一处** `self.manifest` 赋值（形状门禁数它）：清单的
            // `&'static` 在第 3 步就取好了（`leaked` 只泄元数据），像素先落、清单后落 ⇒ 中间任何
            // 失败分支上"本体没换"那句话都是真的。
            //
            // `cell` **不动**：第 1 步的 `same_grid` 已经保证新清单的格子档与身上这一只逐字节相同
            // （不同档在 `plan_swap_body` 就被拒了），所以 `Frame` / 窗口 / buffer 一个都不必重建。
            self.manifest = m;
            self.body_id = name.clone();
            if let Some(sim) = self.sim.as_mut() {
                sim.swap_manifest(m);
            }

            // —— 第 5 步：按**新本体**重跑配件发现 ⇒ `fits` 不含新 `slug` 的那几件当场出局
            // （`AccessoryManifest::validate_against` 比的就是 `body.slug`），出局的每一件走既有的
            // skip-and-log 点名 ⇒ 这几行就是 J4a/J4b 的唯一读数。
            // ⚠️ 库位置是 `accessory_root()`，**不是**本体库那个根：Wayland 那边 11-07 实测错过
            // 这一次（`accessories: 库 …/pets_p5 ⇒ 0 件可用`）⇒ **换完本体之后帽子当场掉、且这一轮
            // 再也穿不上任何配件**，而第 6 步的按 id 重认永远查不到幸存者。那是一条 headless 读不到
            // 的接线错，所以判据落在 `dress.rs` 那条门禁：`swap_body` 的函数体里连注释都不许写
            // 本体库那个函数名。
            self.candidates = accessory_root()
                .map(|root| discover_accessories(&root, m))
                .unwrap_or_default();

            // —— 第 6 步：**按 id 重认**身上那顶（不按位置、不按旧下标）。
            let survivor = worn_hat
                .as_ref()
                .and_then(|hat| hat.id.as_ref())
                .and_then(|old_id| self.candidates.iter().position(|c| &c.id == old_id));
            match (&worn_hat, survivor) {
                (None, _) => {}
                (Some(hat), Some(j)) => {
                    self.picked = j;
                    // 这里**不重跑库存那道**（与 [`Self::equip`] 不同），是选的：这一件本来就穿着，
                    // 重新装上不是"批准一个未验过的请求"，而重跑会把"库这一刻读不出"那档变成**当场脱
                    // 帽** —— 那等于把一次 IO 故障当成一条判定。
                    match self.wear(j) {
                        Ok(slug) => eprintln!(
                            "ui: 换本体 ⇒ 换成 {name}；帽子 {slug} 在新本体上仍然匹配 ⇒ 重新穿上（候选第 {} 格）",
                            j + 1
                        ),
                        Err(e) => eprintln!(
                            "ui: 换本体 ⇒ 换成 {name}，而帽子 {} 重新装载失败（{e}）⇒ 停在未装备态\
                             （库里那一行还在：{slot}={id}）",
                            hat.slug,
                            id = hat.id.clone().unwrap_or_default(),
                            slot = hat.slot
                        ),
                    }
                }
                (Some(hat), None) => {
                    let why = match &hat.id {
                        Some(id) => format!(
                            "{id} 不在新本体的候选里（{name} 的 `slug` = {pet_slug}，`fits` 不含它，或它的产物门禁在这只上过不了）"
                        ),
                        None => format!("释放前没能确定这一件是谁（{pet_slug}）⇒ 不猜"),
                    };
                    eprintln!(
                        "ui: 换本体 ⇒ 换成 {name}；{why} ⇒ 当场脱帽，并删掉库里 {slot}={id} 那一行",
                        slot = hat.slot,
                        id = hat.id.clone().unwrap_or_default()
                    );
                    // 用 `persist_unworn`（只删账）而不是 [`Self::unwear`]：像素在第 2 步就释放了，
                    // `unwear` 此刻读 `worn` 是 `None` ⇒ 它会一声不响地什么都不做，而**库里那一行必须
                    // 删掉** —— 留着就是"重启给一只戴不上的宠物戴帽子"（屏上 A、记忆里 B 那一族）。
                    self.persist_unworn(&hat.slot);
                    // `picked` 不该停在一件已经不存在的格上（[`Self::cycle_accessory`] 那条同形理由：
                    // 那正是"点了没反应"的假按钮形状）。候选表整个换了 ⇒ 重问一次**配件**那一侧的
                    // 可见集；这一刻多开一次库不算两个事实源（同一次点击、同一张表、事件驱动）。
                    let fresh = self.dress_view();
                    self.picked = fresh.visible.first().copied().unwrap_or(0);
                }
            }

            // —— 第 6′ 步：面板行要按**新的**候选表重算。收藏册里配件那一格的载荷是 `candidates` 的
            // 下标，而上一步整张表重建过 ⇒ 不刷新就是"行上写着 A、点下去穿的是 B"（两个事实源），
            // 而它**不进 `content_sig`**（下标故意不在签名里）⇒ 光靠签名自己认不出来。
            self.refresh_panel_rows();

            // —— 第 7 步：**本轮最承重的一行**。四份 @192 产物的行表逐字节相同 ⇒ 内核下一次给出的
            // `{key,row,col}` 与换之前**完全相同**，而 [`Self::round`] 里 `cell != drawn_cell
            // || force_draw` 那道门永远不开 ⇒ 删掉这两行的后果是一次**屏上静默不变**的换本体：像素与
            // 清单都真的换了，而屏幕继续投旧格子（那边变异测试的实测形状 = 五百多条全绿而真机失败，
            // 只有截图抓得到 ⇒ 所以这两行进了源级门禁的读数，不能只留注释）。
            // `drawn_cell = None` 排在前：它保证"下一格无条件算作新格"，`force_draw` 保证"就算格子
            // 没变也投一帧"。先例 = [`Self::apply_dpi`] 与 [`Self::equip`] 那条"这正是 `force_draw`
            // 的合法领地 = 签名与 `drawn_cell` 都看不见的那些像素"。
            self.drawn_cell = None;
            self.force_draw = true;

            // —— 第 8 步：**账排最后**（先像素后账，[`Self::persist_worn`] 同一条口径）。
            self.persist_body(&name);
            eprintln!(
                "ui: 换本体 ⇒ 当前本体 = {name}（`equipped` 的 {slot} 行）",
                slot = overlay_common::dress::BODY_SLOT
            );
        }

        /// 把"当前本体是这只"记进库的 `body` 行。三档结论与 [`Self::persist_worn`] 完全同一套
        /// （共用 [`write_equip]`），差别只在**槽位**和那句"重启会回到哪一只"。
        ///
        /// 拆成单独一条而不是把 `persist_worn` 改造成带槽位参数：后者的 `index` 是 `candidates` 的
        /// 下标，而本体那套下标是 `bodies` 的（两个下标空间，收藏册按 `PanelItem::Body` /
        /// `PanelItem::Wear` 两臂路由）。共用一个函数就要共用一个下标空间，那正是
        /// `the_album_routes_pets_to_the_body_index_space` 那条门禁要杀的形状。
        ///
        /// ⚠️ 只有 UPSERT、没有 DELETE：见 [`Self::swap_body`] 那条"非目标"。
        fn persist_body(&mut self, id: &str) {
            match write_equip(
                &mut self.gacha,
                self.dry_run,
                overlay_common::dress::BODY_SLOT,
                id,
            ) {
                Ok(WornWrite::Written) => {}
                Ok(WornWrite::DemoSkipped) => eprintln!(
                    "ui: 演示态 ⇒ 本体没记进 equipped（无 `--db` = 内存库，重启必然回到 `--manifest` 那只；要留住就加 `--db`）"
                ),
                Ok(WornWrite::NoStore) => eprintln!(
                    "ui: 屏上已换而 equipped 没记上 ⇒ 连不上库 ⇒ 重启会回到 `--manifest` 那只"
                ),
                Err(e) => {
                    eprintln!("ui: 屏上已换而 equipped 没记上（{e}）⇒ 重启会回到 `--manifest` 那只")
                }
            }
        }

        /// **唯一**能把配件放上身的那一步：解码 → 像素上身 → 记进 `equipped`。
        ///
        /// Wayland 那一份多一件"把 `regions` 换成并集"，本平台没有那个对象 ⇒ 这里是四行。
        /// 但**为什么这四行必须在一个函数里**是同一条实测结论（那边变异删掉 `persist_worn`
        /// 全套绿）：`App` 要 Windows 才构造得起来，"屏上戴着一顶而库里没记这笔"在自动化里
        /// 没有读数 ⇒ 只能靠"绕不过去"来防。`dress.rs` 那条源级门禁数着这里写了什么。
        ///
        /// ⚠️ **调用方如果原本穿着，必须先 [`App::take_worn`]**（[`App::cycle_accessory`] 那条
        /// 算过的峰值）。顺序（先像素后账）也是选的：反过来会把库里留一行屏上没有的帽子。
        fn wear(&mut self, index: usize) -> Result<String, String> {
            let worn = decode(&self.candidates[index])?;
            let slug = worn.manifest.slug.clone();
            self.worn = Some(worn);
            self.force_draw = true;
            self.persist_worn(index);
            Ok(slug)
        }

        /// 释放穿着的配件 = 丢掉那张 sheet。返回 `(slug, decoded_bytes)` 只给人看的那两行用。
        ///
        /// **不打印、不置 `force_draw`** ⇒ 由调用方决定用户要不要看见这一次。返回 `None` =
        /// 本来就没穿。本平台这里没有"换回哪份轮廓"的第二件事 ⇒ 单独一个函数的理由只剩
        /// "取走 = 释放"这一个动作不许被拆成"先 `clone` 再 `take`"（那会把峰值推回两件）。
        fn take_worn(&mut self) -> Option<(String, usize)> {
            let worn = self.worn.take()?;
            Some((worn.manifest.slug.clone(), worn.atlas.decoded_bytes()))
        }

        /// 脱下 = 释放像素 + 把那一格从库里删掉，**一个动作**（与 [`App::wear`] 对称）。
        ///
        /// 合起来的理由同 Wayland：把 `persist_unworn` 单独删掉，测试全绿，而用户看到的是
        /// "点了脱下，重启帽子自己回来"。返回 `None` = 本来就没穿。
        ///
        /// ⚠️ [`App::cycle_accessory`] **故意不走这里**，走裸的 [`App::take_worn`]：换一件是
        /// "卸下 A 立刻穿上 B"，中间先 DELETE 再写回会留一个可见的"库里没有"窗口，而后面那次
        /// `wear` 的 upsert 直接把该格改成 B，那才是要的语义。
        fn unwear(&mut self) -> Option<(String, usize)> {
            // 槽位要在丢掉之前取：`take_worn` 只带回打印要用的那两个数，而删那一行要的正是槽位。
            let slot = self.worn.as_ref().map(|w| w.manifest.slot.to_string())?;
            let (slug, bytes) = self.take_worn()?;
            self.force_draw = true;
            self.persist_unworn(&slot);
            Some((slug, bytes))
        }

        /// 把"屏上现在穿着第 `index` 件"记进库。**只在像素已经装成功之后调**。
        fn persist_worn(&mut self, index: usize) {
            let slot = self.candidates[index].manifest.slot.to_string();
            let id = self.candidates[index].id.clone();
            match write_equip(&mut self.gacha, self.dry_run, &slot, &id) {
                Ok(WornWrite::Written) => {}
                Ok(WornWrite::DemoSkipped) => eprintln!(
                    "ui: 演示态 ⇒ 这一次不记进 equipped（无 `--db` = 内存库，重启必然什么都不穿；要留住就加 `--db`）"
                ),
                Ok(WornWrite::NoStore) => {
                    eprintln!("ui: 屏上已穿而 equipped 没记上 ⇒ 连不上库 ⇒ 重启不会恢复");
                }
                Err(e) => eprintln!("ui: 屏上已穿而 equipped 没记上（{e}）⇒ 重启不会恢复"),
            }
        }

        /// 把那一格从库里删掉。同 [`App::persist_worn`] 的三档与同一句必须说出来的理由。
        fn persist_unworn(&mut self, slot: &str) {
            match clear_equip(&mut self.gacha, self.dry_run, slot) {
                Ok(WornWrite::Written) => {}
                Ok(WornWrite::DemoSkipped) => {
                    eprintln!("ui: 演示态 ⇒ 库里那一格没得删（这一轮从头就没往库里写过）")
                }
                Ok(WornWrite::NoStore) => {
                    eprintln!("ui: 屏上已脱而 equipped 没删 ⇒ 连不上库 ⇒ 重启还会穿上");
                }
                Err(e) => eprintln!("ui: 屏上已脱而 equipped 没删（{e}）⇒ 重启还会穿上"),
            }
        }

        /// 启动时读一次 `equipped` ⇒ 恢复上次穿着（P3 = J3 的全部）。**只跑一次**（闩 =
        /// [`App::restore_pending`]，在 [`App::round`] 环外放下）。
        ///
        /// 三条早退各有其门，都**不出声**（它们不是"失败了"，是"这一轮压根没有这件事"）：
        /// 演示态不读库、矩形档（一行资产都不读 ⇒ 与启动时 `discover_accessories` 同一个条件）、
        /// 库里没记过。
        ///
        /// 判定本身在 [`plan_restore`]（四道、可红、宿主可测）；这里只做三件它做不了的事：
        /// 连库、**重跑最后一道（解码）**、把结果接到 `worn`/`picked` 上。那一道为什么只能在这里：
        /// 启动扫盘那一轮一像素未解，而"文件在而 sheet 坏了"这种形状只有穿的那一刻才看得见 ——
        /// 报一行、停在未穿，绝不为一次恢复把宠物带崩（硬约束 3）。
        fn restore_worn(&mut self) {
            if self.dry_run || self.opts.test_rect {
                return;
            }
            let ids: Vec<String> = self.candidates.iter().map(|c| c.id.clone()).collect();
            let slots: Vec<String> = self
                .candidates
                .iter()
                .map(|c| c.manifest.slot.to_string())
                .collect();
            if !self.ensure_gacha() {
                eprintln!(
                    "ui: 恢复上次穿着 ⇒ 连不上存档，这一轮一件都不恢复（连不上的原因上面已说过一次）"
                );
                return;
            }
            let rows = match self
                .gacha
                .as_ref()
                .map(|g| g.store.equipped(petstore::DEFAULT_PROFILE))
            {
                Some(Ok(rows)) => rows,
                Some(Err(e)) => {
                    eprintln!("ui: 恢复上次穿着 ⇒ 读 equipped 失败（{e}）⇒ 这一轮一件都不恢复");
                    return;
                }
                None => return,
            };
            if rows.is_empty() {
                // 没记过 ⇒ 一行都不打。启动噪音要留给"出事了"，而"上次没穿"是常态。
                return;
            }
            // —— **先把本体那一行剥掉**，否则 [`plan_restore`] 第 1 道会对着一条**根本不是配件**的
            // 账打"磁盘上找不到这一件"，而那句是假话（共用层那条函数的注释算过整件事）。
            let (body, accessories) = split_body_row(&rows);
            if let Some(id) = body.as_deref().filter(|id| *id != self.body_id) {
                // M7d 起这一行**只在"覆盖没成立"时打**：起点本体早在 [`App::startup`] 那一轮按
                // [`plan_body_override`] 换过了（换成功 ⇒ `body_id` 已经等于库里那一行，这一支不进）。
                // 所以此刻说的是一件真事：那一只**这一轮没装上**，而说得出没装上的三种成因。
                eprintln!(
                    "body: 库里那一行 {slot}={id}，而本轮起点本体是 {cur} ⇒ 那一只这一轮没装上\
                     （扫盘没交出它：目录不存在 / 产物坏了 / 格子档不同）",
                    slot = overlay_common::dress::BODY_SLOT,
                    cur = self.body_id,
                );
            }
            // 可见集与换装页共用同一次判定（`dress_view`）⇒ 不会有"帽子页里不出现而启动却穿上了"。
            let view = self.dress_view();
            let plan = plan_restore(&accessories, &ids, &slots, &view.visible);
            for note in &plan.notes {
                eprintln!("ui: 恢复上次穿着 ⇒ {note}");
            }
            let Some(i) = plan.wear else {
                return;
            };
            let slot = slots[i].clone();
            let id = ids[i].clone();
            self.picked = i;
            match self.wear(i) {
                Ok(_) => eprintln!(
                    "equipped: {slot}={id} ⇒ 恢复上次穿着（磁盘两道 + 库存一道 + 解码全过，像素已叠在当前格上）"
                ),
                Err(e) => eprintln!(
                    "equipped: {slot}={id} 解不开（{e}）⇒ 最后一道没过，这一轮不穿（停在未装备态）"
                ),
            }
        }

        // ───────────────────────── 菜单点击分派 ─────────────────────────

        /// 菜单行分派（Wayland `on_menu_press` 的 Windows 版）。
        ///
        /// 命中与相位把关**不在这函数里** —— 那是 `menu::press` 的职责（调用方 `Events::press`
        /// 那臂只把命中结果递过来）。把关放命中侧的原因原样继承（结果卡相位不许点着底下的行，
        /// 门禁 `the_card_phase_opens_no_row_to_the_pointer` 是共用的绘制层测的）。
        ///
        /// 哪些行真（M7b 之后**这一面上没有欠的行**，逐臂写明去向）：
        /// * `Close`/`Page`/`Lang`、动作页四行 + `Pause`（`Command::Go`）、`Quit`、
        ///   `Equip`/`Unequip`/`NextOutfit`（M5e 换装，走 [`App::wear`] 那一族）、
        ///   `Pull1`/`Pull10`（M6 抽卡与结果卡面）；
        /// * `Owned`/`Missing`/`Recent`/`MotionTier`/`Rates`/`Verify` = M7b 面板入口。
        ///   ⚠️ 这六行**只是入口**：功能本体在 [`App::dispatch_panel`]，而菜单这侧不复制任何
        ///   一条报表逻辑（收藏册三屏的字由 [`App::gacha_panel`] 一次算完、同时进 stderr 与像素）。
        ///
        /// 曾经挂在这一臂上的那句"欠的行**只出声 stderr，不在屏上发明新字**"仍然成立，
        /// 只是 M7b 之后它没有使用者了 —— 屏上每一个格子背后都有一件真会办的事。
        fn dispatch_menu(&mut self, item: MenuItem) {
            match item {
                MenuItem::Close => self.pending_menu_close = true,
                MenuItem::Page => self.cycle_page(),
                MenuItem::Lang => self.cycle_lang(),
                MenuItem::Quit => {
                    self.pending_menu_close = true;
                    self.exit_requested = true;
                    eprintln!("ui: 收到退出请求（菜单退出行）⇒ 收尾并打退出摘要");
                }
                MenuItem::Walk => self.send_action(Action::Walk),
                MenuItem::Run => self.send_action(Action::Run),
                MenuItem::Jump => self.send_action(Action::Jump),
                MenuItem::Climb => self.send_action(Action::Climb),
                MenuItem::Pause => self.send_action(Action::Sleep),
                MenuItem::Pull1 => self.do_pull(1),
                MenuItem::Pull10 => self.do_pull(10),
                MenuItem::Equip => self.equip(),
                MenuItem::Unequip => self.unequip(),
                MenuItem::NextOutfit => self.cycle_accessory(),
                MenuItem::Owned => self.collection_panel(CollectionTab::Owned, 0),
                MenuItem::Missing => self.collection_panel(CollectionTab::Missing, 0),
                // 「最近」的条数 = 一屏装得下的行数（`panel::MAX_ROWS - 1`，判据在
                // `overlay_report::Gacha::tab_lines`，两家同一个数）。
                MenuItem::Recent => self.collection_panel(CollectionTab::Recent, 0),
                // ── 设置页：菜单那三行是**入口**，功能在面板上办 ──
                // `MenuItem::Settings` 不存在（也不该有 —— 加一个变体要动 `rows()`、命中表、
                // 四个 locale 的文案与字体子集），所以"动画"那一档的循环职责交给面板里的
                // `PanelItem::Motion` 行，菜单这一行只负责把那一面开出来。
                MenuItem::MotionTier => self.open_panel(PanelKind::Settings),
                // 公示概率不碰库（见 [`App::rates_lines`]），所以这一臂不过 `gacha_panel`。
                MenuItem::Rates => {
                    let lines = self.rates_lines();
                    self.show_panel_lines("rates", PanelKind::Settings, lines);
                }
                MenuItem::Verify => {
                    self.gacha_panel("verify", PanelKind::Settings, |g, l, s| {
                        g.verify_lines(l, s)
                    });
                }
            }
        }

        /// 点标题条：翻页 + **当场重建命中表**（`items` 与 `page` 必须一起改，理由照 Wayland：
        /// 分开改就是"点的是这一页的行、画的是那一页的字"）。不加 `force_draw` ——
        /// 页进 `content_sig`，下一轮闸门自己放行。
        fn cycle_page(&mut self) {
            let Some(m) = self.menu.as_mut() else {
                return;
            };
            let next = m.page.next();
            m.page = next;
            m.items = menu::rects(next);
        }

        /// 点语言行：`zh → en → ja → ko` 循环，**本进程立刻生效 + 当场写回**（M7e）。
        ///
        /// 四步联动的次序与 Wayland 母本逐字一致（见 [`App::apply_lang`] 的理由）：本平台**没有**
        /// 第二条调用者（§3.12 的 `lang.set` 套接字还没实现，那一臂归第三批），但这一层拆分成
        /// 函数不是省事 —— 本平台只有这一处会动 `ui.lang`，写在一起让"只有这一处"可被 grep 数出来。
        fn cycle_lang(&mut self) {
            let next = self.ui.lang.next();
            self.apply_lang(next, settings::ValueSource::MenuClick);
        }

        /// **一档新语言被应用 + 写回的唯一落点**（本平台只有 [`App::cycle_lang`] 一个调用者；
        /// Wayland 那边还有 §3.12 `lang.set` 的套接字臂共用这一处）。
        ///
        /// 写回不再"提示你自己去写"：[`petstore::config::write_ui_key`] 走 `toml_edit` 保 decor +
        /// 落盘前后各判一次，所以两处一起改（`App.ui.lang` 是真相源、`MenuState.lang` 是 draw 用的
        /// 副本）—— **全项目只有这一处写它**。
        ///
        /// 第三处 = **开着的面板要把行重算**（[`App::refresh_panel_rows`]）：面板那一屏每一行的字
        /// 都从 `lang` 来，不换就是"切了语言而收藏册还写着中文"。这里**不给面板置 `force_draw`**：
        /// `lang` 与每一行文本都已经在 `panel::content_sig` 里，闸门自己认得出。
        ///
        /// 写回排在最后，是因为 [`App::persist_ui_key`] 是这一族里唯一会**改错误行**的一步，
        /// 而错误行也在签名里 —— 它对"这次到底落没落地"有最后发言权。
        fn apply_lang(&mut self, next: Lang, source: settings::ValueSource) {
            // 写回之前先记下这一档原来是从哪层来的：命令行高于文件（§4.6），所以从 `--lang`
            // 启动的那一次点了也不会下次生效，这句话必须说，不然是"点了没反应"的假故障。
            let from_cli = self.ui.lang_from == settings::ValueSource::Cli;
            self.ui.lang = next;
            self.ui.lang_from = source;
            if let Some(m) = self.menu.as_mut() {
                m.lang = next;
                m.force_draw = true;
            }
            self.refresh_panel_rows();
            eprintln!("ui: 语言 → {} ({})", next.self_name(), next.code());
            self.persist_ui_key(petstore::config::KEY_LANG, next.code(), from_cli);
        }

        /// 动作页四行 + 暂停行 ⇒ 一条 `Command::Go`，**不在客户端预判内核会不会响应**
        /// （照 Wayland `send_action`：预过滤要求绘图读内核状态，而那是本项目禁止的假按钮反形；
        /// 内核本来就把这些全吸收了 —— 下坠丢 `Go`、睡着先 `Wakeup`）。
        fn send_action(&mut self, action: Action) {
            if let Some(sim) = self.sim.as_mut() {
                sim.send(Command::Go(action));
            }
        }

        /// 退出摘要。**任何一条退出路径都要走到这里**（`taskkill` 掉的轮次没有摘要 = 不是证据）。
        fn report(&self) {
            let mem = summary::collect_memory();
            let cpu = summary::collect_cpu_secs();
            let state_name = self
                .sim
                .as_ref()
                .map(|sim| format!("{:?}", sim.machine.state()));
            // `Run::kernel` 那一格的原文是"state 的 Debug"，而 `State` 只有 Debug 可用
            // （`label()` 在 `EdgeFrom` 上，不在 `State` 上）⇒ 名字先落到一个活过 `lines()`
            // 调用的 `String`，避免把 `Run` 从 `Copy` 改成非 `Copy`。
            let kernel = match (&state_name, self.sim.as_ref()) {
                (Some(n), Some(sim)) => Some((n.as_str(), sim.fingerprint())),
                _ => None,
            };
            // 生效值那一行同理（M7e）：正文 = [`Settings::summary`](crate::settings::Settings::summary)
            // 的返回值，而它是**算出来的** `String` ⇒ 先落一个活过 `lines()` 的绑定，
            // 好让 `Run` 保持 `Copy`。这一行与摘要清单里那行 `ui: 已接/未接` **共用 `ui:` 前缀**
            // （读它要带更长的前缀，`summary.rs` 里那条计数断言钉着）。
            let ui_line = self.ui.summary();
            let run = summary::Run {
                ticks: self.ticks,
                wall_secs: self.started.elapsed().as_secs_f64(),
                counters: &self.counters,
                surface_logical: (
                    self.cell.0 + 2 * SURFACE_MARGIN as u32,
                    self.cell.1 + 2 * SURFACE_MARGIN as u32,
                ),
                buffer_physical: self.frame.surface_phys(),
                scale: self.scale,
                dpi_path: self.dpi_path.as_str(),
                kernel,
                mem: mem.as_ref().ok().copied(),
                mem_reason: mem.as_ref().err().map(String::as_str),
                cpu_secs: cpu.as_ref().ok().copied(),
                cpu_reason: cpu.as_ref().err().map(String::as_str),
                menu_open: self.menu.is_some(),
                panel_open: self.panel.is_some(),
                font: summary::Font::collect(),
                ui_effective: &ui_line,
            };
            println!("windows overlay 摘要：");
            for line in summary::lines(&run) {
                println!("{line}");
            }
        }
    }

    impl Events for App {
        fn hit(&self, kind: SurfaceKind, x: i32, y: i32) -> bool {
            match kind {
                SurfaceKind::Pet => {
                    // 拖拽期间一律算命中 —— 这是 Wayland 那条"把 input region 临时撑成整块
                    // surface"在本平台的同形做法。不这么写的症状与那边逐字相同：指针甩出剪影后
                    // `HTTRANSPARENT` 把点击让给底下的窗口，`Release` 从此不归我们，内核卡在 `Dragged`。
                    self.drag || self.stage_buf.hit(x, y)
                }
                SurfaceKind::Menu => {
                    // 菜单没有"拖拽豁免"那一臂（它不参与拖拽），命中读的就是上一帧交出去的
                    // 那份 canvas ⇒ 圆角外点得穿，与宠物面同一套 alpha 数学、同一个阈值。
                    self.menu.as_ref().is_some_and(|m| m.hit(x, y))
                }
                // 面板与菜单同一套数学：命中读的就是上一轮交出去的那份 canvas ⇒ 圆角外点得穿。
                SurfaceKind::Panel => self.panel.as_ref().is_some_and(|p| p.hit(x, y)),
                // 结果卡**不是一块独立面**（M6 拍板：它与菜单同面不同相位，`MenuAnim::Burst/
                // Reveal` 画在同一张卡片上）⇒ 这一格在 Windows 上结构性不可达，`false` 是实话。
                SurfaceKind::Card => false,
            }
        }

        fn press(&mut self, kind: SurfaceKind, x: i32, y: i32) {
            match kind {
                SurfaceKind::Pet => self.on_press(self.to_world(x, y)),
                SurfaceKind::Menu => {
                    // 命中 + 相位把关走 `menu::press`（唯一许可分派的入口，理由见
                    // [`App::dispatch_menu`]）；坐标先物理→逻辑 —— 命中表是逻辑口径。
                    let item = self
                        .menu
                        .as_ref()
                        .and_then(|m| menu::press(&m.items, m.local_logical(x, y), &m.anim));
                    if let Some(item) = item {
                        self.dispatch_menu(item);
                    }
                }
                // 面板：与菜单同一条纪律 —— 命中 + `actionable` 把关只走 `panel::press`
                // （唯一许可分派入口），坐标先物理→逻辑（命中表是逻辑口径）。
                SurfaceKind::Panel => {
                    let item = self
                        .panel
                        .as_ref()
                        .and_then(|p| panel::press(&p.items, p.local_logical(x, y)));
                    if let Some(item) = item {
                        self.dispatch_panel(item);
                    }
                }
                // 结果卡与菜单同面不同相位（见 `hit` 那一臂）⇒ 这块面上没有独立的点击可派发。
                SurfaceKind::Card => {}
            }
        }

        fn release(&mut self, kind: SurfaceKind, x: i32, y: i32) {
            if kind.is_pet() {
                self.on_release(self.to_world(x, y));
            }
        }

        fn right_press(&mut self, kind: SurfaceKind) {
            // 三臂 = 三种"此刻看着哪块面"：右键宠物/菜单 ⇒ 菜单 toggle（D14"关闭 = 再右键"）；
            // 右键面板 ⇒ 关面板。**不许把面板并进上面那一臂**：`toggle_menu` 的第一句查的是
            // `self.menu`，面板开着而菜单没开时它会去**开菜单**，正好把互斥那条破掉
            // （症状 = 右键面板弹出菜单，看起来像"关不掉"）。
            // **这一臂就是 M5a 那份退出证据**：Wine 上右键宠物菜单出现，读的是这里，
            // 而 `window.rs` 那侧只保证 `WM_RBUTTONDOWN` 会走到这。
            match kind {
                SurfaceKind::Pet | SurfaceKind::Menu => self.pending_menu_toggle = true,
                SurfaceKind::Panel => self.close_panel(),
                SurfaceKind::Card => {}
            }
        }

        fn motion(&mut self, kind: SurfaceKind, x: i32, y: i32) {
            if !kind.is_pet() {
                return;
            }
            let world = self.to_world(x, y);
            self.pointer_world = world;
            if self.drag {
                if let Some(sim) = self.sim.as_mut() {
                    let to = Vec2::new(
                        self.pointer_world.x - self.grab_offset.x,
                        self.pointer_world.y - self.grab_offset.y,
                    );
                    sim.send(Command::DragMove { to });
                }
            }
        }

        fn capture_lost(&mut self, kind: SurfaceKind) {
            if kind.is_pet() && self.drag {
                eprintln!("捕获被收回 ⇒ 提前结束拖拽（没等到 Release）");
                // 位置未知：按"最后已知指针位置"收尾，与 Wayland 的 Leave 分支同一个处置。
                let world = self.pointer_world;
                self.on_release(world);
            }
        }

        fn dpi_changed(&mut self, kind: SurfaceKind, scale: f32) {
            // 几何重算目前只有本体面一条路（`apply_dpi`）；其余面 M5 起各自吃新 scale。
            if kind.is_pet() {
                self.pending_scale = Some(scale);
            }
        }
    }

    /// `Box::leak` 那一笔的镜像说明见 [`App::manifest`]。
    fn load_manifest(opts: &Options) -> Result<&'static Manifest, String> {
        let m = match opts.manifest.as_deref() {
            None => default_manifest(),
            Some(p) => {
                let s = p.to_str().ok_or("清单路径不是 UTF-8")?;
                Manifest::load(s).map_err(|e| format!("{s} 加载失败: {e}"))?
            }
        };
        Ok(Box::leak(Box::new(m)))
    }

    /// 解**起点本体**的全部像素。`body_dir` = `plan_body_override` 判定要覆盖时那一只的
    /// 目录；`None` = 用 `--manifest` 那个目录的父级（= 老行为）。
    ///
    /// ⚠️ 本平台**没有 polygon 的消费者**（命中读的是叠完之后的 canvas alpha，见 `stage.rs`），
    /// 所以这里只读清单点名的那些 sheet。这不影响 [`discover_pets`] 仍按三件套要求本体 —— 两家同一本账，
    /// 差别只在"谁在什么时候读哪一份"，不在"产物必须长什么样"。
    ///
    /// ⚠️ 文件名**只能来自清单**（`AtlasSet::load` 读 `sheet_segments`）：这里写死一个 `sheet.webp`
    /// 就是第二个"像素从哪来"的事实源，症状是清单改了名而屏上还在读旧那张、一错都不报（§6.1.27 裁决 6）。
    fn load_atlas(
        opts: &Options,
        manifest: &Manifest,
        body_dir: Option<&std::path::Path>,
    ) -> Result<Option<AtlasSet>, String> {
        if opts.test_rect {
            return Ok(None);
        }
        let path = opts
            .manifest
            .as_deref()
            .ok_or("--manifest 缺失而矩形档也没开（cli 应当已经拒掉这一组合）")?;
        let owned_dir;
        let dir = match body_dir {
            Some(d) => d,
            None => {
                owned_dir = path
                    .parent()
                    .ok_or_else(|| format!("{} 没有父目录 ⇒ 找不到像素", path.display()))?
                    .to_path_buf();
                &owned_dir
            }
        };
        let set = AtlasSet::load(dir, manifest).map_err(|e| format!("{}: {e}", dir.display()))?;
        // 逐张一行：一张满 sheet 是 45 MiB 那一档，"两张里哪一张贵"必须看得见（D16 的输入）。
        // 文案与 Wayland 的 `load_asset` 逐字同 ⇒ Wine 与真机那两份日志能用同一条 grep 读。
        for (index, (file, sheet)) in set.segments().enumerate() {
            eprintln!(
                "asset: 第 {} 张 {file} {}x{} {} KB → 解码 {} ms / {:.3} MiB",
                index + 1,
                sheet.width,
                sheet.height,
                sheet.encoded_bytes / 1024,
                sheet.decode_ms,
                sheet.decoded_bytes() as f64 / (1024.0 * 1024.0),
            );
        }
        eprintln!(
            "asset: 合计 {} 张 · 解码 {:.3} MiB（{} 列 × {} 行，格 {}x{}）",
            set.len(),
            set.decoded_bytes() as f64 / (1024.0 * 1024.0),
            manifest.grid.cols,
            manifest.grid.rows,
            manifest.grid.w,
            manifest.grid.h,
        );
        Ok(Some(set))
    }

    pub(super) fn run(options: Options) -> Result<(), String> {
        let (app, mut client) = App::startup(options)?;
        // `App` 放在**稳定的堆位置**并且整轮不移出：`pump` 要的 `'static` 借用是从这个
        // 地址形成的，而派发期间窗口回调读的就是它。
        let raw = Box::into_raw(Box::new(app));
        let outcome = drive(&mut client, raw);
        // SAFETY: `drive` 返回后没有任何 `&mut App` 还活着（`pump` 在返回时把 `Slot::ev`
        // 清回 `None`），而这块内存除了这里没有第二个所有者。
        let app = unsafe { Box::from_raw(raw) };
        app.report();
        drop(app);
        outcome
    }

    /// 事件环本体。
    ///
    /// 每轮三拍，顺序**不可重排**：
    /// 1. 从裸指针重新取一次借用，做完业务（DPI 重算 + 内核步进 + 出图）；
    /// 2. 把同一块对象作为 `&'static mut dyn Events` **交出去**给 `pump`（这一句之后，
    ///    在 `pump` 返回之前，任何别处的 `&App` / `&mut App` 都是 UB —— 这就是为什么
    ///    借用要每轮重新形成而不是存成字段：存下来会同时存在两份，而类型系统拦不住）；
    /// 3. `pump` 返回 = 句柄已被消耗（`Slot::ev` 清回 `None`）⇒ 重新取借用继续。
    ///
    /// ⚠️ 第 2 拍那一份是**一次性**的：编译期实测，把 `let ev = unsafe { &mut *raw }` 存进
    /// 变量后再借用同一个对象，报的是 `E0502 … type annotation requires that *app is
    /// borrowed for 'static`。`pump` 消耗句柄不是不便，是那一条纪律的类型写法。
    fn drive(client: &mut Client, raw: *mut App) -> Result<(), String> {
        loop {
            // SAFETY: 与 `pump` 期间那份不重叠（上面第 2 拍的注释），且 `raw` 由 `run` 保证非空、
            // 指向一块直到 `Box::from_raw` 之前都不会被释放的 `App`。
            let keep_going = unsafe { &mut *raw }.round(client)?;
            if !keep_going {
                client.request_quit();
                return Ok(());
            }
            let ev: &'static mut dyn Events = unsafe { &mut *raw };
            match client.pump(ev, WAIT_CAP_MS) {
                // `pump` 已经把 `Wake::Failed` 翻成 `Err` 并带上 `GetLastError` 与 API 名
                // （`window.rs:772-783`），所以正常路径从下面那一支走。这一支留着**不是**
                // 装饰：那条翻译一旦被改动，"等待失败"塌成一次普通唤醒 = 无限睡，
                // 正是 `Wake::Failed` 自己那句注释点名的病。
                Ok(Wake::Failed(code)) => {
                    if client.quit() {
                        return Ok(());
                    }
                    return Err(format!(
                        "等待返回 Failed 而窗口层没翻成错误（GetLastError={code:#X}）\
                         ⇒ 结束这一轮而不是无限睡"
                    ));
                }
                Err(e) => {
                    // `pump` 内部已经抽过一遍队列（`WM_QUIT` 可能就在里面），所以这里
                    // 先看是否已经被判退，判退了就走正常收尾而不是丢一句错误。
                    if client.quit() {
                        return Ok(());
                    }
                    // **原文带走**：换成一句泛指等于把那一步的证据（哪个 API、什么错误码）扔掉。
                    return Err(format!("{e} ⇒ 结束这一轮而不是无限睡"));
                }
                Ok(_) => {}
            }
            if client.quit() {
                return Ok(());
            }
        }
    }
}
