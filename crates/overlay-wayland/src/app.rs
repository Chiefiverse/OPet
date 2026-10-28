//! 运行环：60 Hz 定时的内核 tick + Wayland 事件 + 三级提交分派（设计书 §3.7、§4）。
//!
//! 这里是**客户端唯一使用墙钟的地方**（`petcore` 内禁止，硬约束 8）。定时用 calloop 的
//! `Timer`，Wayland socket 用 `calloop-wayland-source`，两者共用一个事件循环。
//!
//! 三级分派（省电契约，`sim.rs:135-137` 的语义落到协议上）：
//! 1. 图集格变了 ⇒ 新 buffer + `damage_buffer` + `set_input_region` + `commit`；
//! 2. 只有位置变了 ⇒ 只改 layer margin + `commit`（不附新 buffer）；
//! 3. 都没变 ⇒ 什么都不发。

use std::sync::Arc;
use std::time::{Duration, Instant};

use petcore::assets::Manifest;
use petcore::behavior::config::BehaviorConfig;
use petcore::behavior::machine::Command;
use petcore::gacha::{Catalog, Currency, GachaParams, Rarity};
use petcore::geom::{Rect, Vec2};
use petcore::intent::{PresentationIntent, SURFACE_MARGIN};
use petcore::sim::{Sim, TICK_HZ};
use petcore::world::{Monitor, World, WorldEvent};
use petstore::Store;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::delegate_compositor;
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::delegate_output;
use smithay_client_toolkit::delegate_pointer;
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::delegate_seat;
use smithay_client_toolkit::delegate_shm;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopSignal};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
// `macro_export` 的宏在 crate 根，不在 `registry` 模块里
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerEvent as SeatPointerEvent, PointerEventKind, PointerHandler, ThemeSpec,
    ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure, SurfaceKind,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};

use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_output;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_seat;
use wayland_client::protocol::wl_shm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};

use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::atlas::Atlas;
use crate::card;
use crate::l10n::Lang;
use crate::menu::{self, CARD_H, CARD_W, ItemRect, MenuAnim, RectI};
use crate::region::{CellRect, Regions};
use crate::settings::{self, Motion};
use crate::text;

/// `wp_fractional_scale` 的 scale 单位是 1/120（180 = 1.5×，本机 eDP-1 实测值）。
const SCALE_UNIT: u32 = 120;
/// 单轮事件循环最多补几个 tick（合盖/高负载回来后追帧会一次性提交一串 buffer）。
const MAX_CATCHUP: u32 = 3;
// 按键码只在 `menu::BTN_*` 一处定义（外部 ABI，两处就会漂移）。
const DOUBLE_TAP_GAP: Duration = Duration::from_millis(320);
const DOUBLE_TAP_RADIUS: f32 = 24.0;

#[derive(Clone, Debug)]
pub struct Options {
    /// `pet.json` 路径；`None` = 内核内置默认清单（M1 fixture：有元数据、无像素）
    pub manifest: Option<String>,
    /// 自动退出秒数（在 live 会话上必须给，别让宠物赖在桌面上）
    pub seconds: Option<f32>,
    /// 舞台显示器名；`None` = 第一个
    pub monitor: Option<String>,
    /// 底边被面板占掉的高度（逻辑像素）；地面 = 舞台底边往上这么多。
    pub panel_bottom: f32,
    /// 顶边被面板占掉的高度（逻辑像素）：layer-shell 的 margin 是**从可用区边缘**量的，
    /// 顶部有 exclusive zone 的栏会把整块 surface 整体下推 ⇒ 不给这个值，宠物的脚就掉到
    /// 屏幕外（本机实测 200 → 渲染在 220）。协议读不到栏高，只能由启动参数给。
    pub panel_top: f32,
    pub seed: u64,
    /// 抽取存档的 sqlite 路径（`--db`）。`None` = **内存库**：不建文件、不写盘，菜单与
    /// 结果卡一律打 `DRYRUN` 标。显式给路径才写真存档，与 `opet pull` 的默认存档门禁同族。
    pub db: Option<String>,
    /// 不接内核、不读资产，画一块纯色矩形（M4 第 3 步：只验 surface 能否映射 + 穿透）
    pub test_rect: bool,
    /// 不设光标（主题加载失败时的退路，代价是"鼠标在宠物上隐形"）
    pub no_cursor: bool,
    /// `--lang zh|en|ja|ko` 的**原始字符串**。这里不解释值：解释只在
    /// [`settings::Settings::resolve`] 一处（`Lang::parse`/`Motion::parse` + 优先级链），
    /// 免得 CLI 和配置文件各写一套"什么算合法"。
    pub lang: Option<String>,
    /// `--motion full|reduce|off`，同上存原始字符串
    pub motion: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            manifest: None,
            seconds: Some(30.0),
            monitor: None,
            panel_bottom: 0.0,
            panel_top: 0.0,
            seed: 1,
            db: None,
            test_rect: false,
            no_cursor: false,
            lang: None,
            motion: None,
        }
    }
}

/// 省电口径的计数（§4 的三个档位 + 延后 + 失败）。
///
/// **五档互斥且齐**：`commits + moves + idle + deferred + buffer_errors == ticks`（落地见
/// [`Tick`] 与 [`Counters::bump`]）。补投与菜单那块 surface 的计数**不并进五档**（理由见各自
/// 字段注释）。这条恒等式是判据④的硬核对项，而它只在互斥时读得出来：`延后`/`buffer 错误`
/// 那两条分支原先除了记自己那一档还走 `finish_move` 再记一次 `moves` 或 `idle`，帧回调的补投
/// 又记进 `commits`。2026-10-28 真机轮 4 第一次让 `延后` 非零（=2），五档当场比 tick 多 3
/// （= 补投 2 + 双记 1，那 1 就是"延后且同时挪 margin"，`仅移动` 因此是 5618 而非 5617）。
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    /// 提交新 buffer（图集格真的变了）
    pub commits: u64,
    /// 只移 surface（margin 变了，没附新 buffer）
    pub moves: u64,
    /// 整 tick 无事可做（不换格、不延后、不出错）
    pub idle: u64,
    /// 内容变了但帧回调还没回来 ⇒ 本次不提交（丢帧可接受，绝不阻塞）
    pub deferred: u64,
    /// `create_buffer` / attach 失败次数
    pub buffer_errors: u64,
    /// 帧回调里补投的 buffer 数。**不并进五档** —— 它由帧回调驱动、不对应任何 tick，
    /// 混进 `commits` 就让"五档 == tick"永久读不出来（补投几次就超几次）。
    ///
    /// **读法（2026-10-28 实测校正，原先这里写的"M 应当等于 N"是错的判据）**：
    /// `延后 N / 补投 M` 只要求 **M ≤ N**，`M < N` 是**设计使然**而非丢格 ——
    /// 延后那一档只置一个布尔 `force_draw`（"绝不排队"），所以**连续几个延后的 tick 会
    /// 塌缩成回调里的一次补投**（补的是最后那个 intent，旧的已被更新的一帧取代）；另一条
    /// 让 M 变小的路径是看门狗先强制解锁 ⇒ 那一格由**下一个 tick** 按 `Buffer` 画上，
    /// 不经过补投。⇒ 真要警惕的形状是 **M > N**（补投比延后还多 = 有一格被投了两遍），
    /// 而"延后 > 0 但 M 与看门狗全 0"意味着通道锁死没解开。
    /// 首轮非零实测 = 2026-10-28 `--motion off` 那轮（用户在运行中改了屏幕缩放）：
    /// `延后 3 / 补投 2 / 帧回调看门狗 3`，五档仍 `== tick 总数` ⇒ 这条 P0/P1 路径
    /// 第一次有真机证据（前十轮全是 0，那 0 是"没跑到"）。
    pub replay_commits: u64,
    /// `configure` 重配置触发的 buffer 数。同 `replay_commits`：**不并进五档**（不由 tick 驱动）。
    /// 2026-10-28 轮 3 首次撞到（5 次重配置 + 4 次看门狗），五档当场比 tick 多 5。
    pub configure_commits: u64,
    /// 帧回调迟迟没回来 ⇒ 看门狗强制解锁（不解锁会永远不再附新 buffer，宠物视觉上冻住）
    pub watchdogs: u64,
    /// 菜单那块 surface 的提交数。**不并进上面三档** —— §4 的省电口径是宠物的，
    /// 第二块 surface 混进去就会让"宠物三档不劣化"这条门禁变成读不出来的混合数。
    pub menu_commits: u64,
    pub menu_watchdogs: u64,
}

impl Counters {
    /// 记这一档：**恰好动一个字段** —— 五档互斥这条恒等式就是靠它落地，别在外面手写 `+=`。
    fn bump(&mut self, tick: Tick) {
        match tick {
            Tick::Buffer => self.commits += 1,
            Tick::Replay => self.replay_commits += 1,
            Tick::Configure => self.configure_commits += 1,
            Tick::Move => self.moves += 1,
            Tick::Idle => self.idle += 1,
            Tick::Deferred => self.deferred += 1,
            Tick::Fault => self.buffer_errors += 1,
        }
    }

    /// 五档加总：应当逐轮等于 `App.ticks`（`Replay` 与菜单两块计数都在之外）。
    fn tick_accounted(&self) -> u64 {
        self.commits + self.moves + self.idle + self.deferred + self.buffer_errors
    }
}

/// 一次 `present` 是被谁驱动的。**只有 `Tick` 归进五档**（`Counters::tick_accounted`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    /// 60 Hz 定时的一个 tick（`on_tick` 的补帧循环里）
    Tick,
    /// 帧回调回来后的补投：把延后的那一格补上
    Replay,
    /// 合成器又发了一次 `configure` ⇒ 客户端主动重画一帧
    Configure,
}

/// 一个 tick 归哪一档。档位与"发什么协议"是两条轴：`Deferred`/`Fault` 在 margin 动了时照样发
/// 一个不附 buffer 的状态 commit，但那不改变归属（历史上正是这里双记让五档超了 tick）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tick {
    /// 附了新 buffer
    Buffer,
    /// 帧回调补投的 buffer ⇒ 不对应任何 tick
    Replay,
    /// `configure` 重配置触发的 buffer ⇒ 不对应任何 tick
    Configure,
    /// 只发一个不附 buffer 的状态 commit（margin 变了）
    Move,
    /// 什么都没发
    Idle,
    /// 内容要换格，但在途 buffer 没回来 ⇒ 本次不提交
    Deferred,
    /// 出图失败（`create_buffer` / attach）
    Fault,
}

/// 逐档遍历用（只服务测试；生产路径按具体档位分支，不需要这张表）。
#[cfg(test)]
const ALL_TICKS: [Tick; 7] = [
    Tick::Buffer,
    Tick::Replay,
    Tick::Configure,
    Tick::Move,
    Tick::Idle,
    Tick::Deferred,
    Tick::Fault,
];

/// `present` 可能产出的**归档结果**（`Replay`/`Configure` 不是结果、是来源，见 `ALL_TICKS`）。
#[cfg(test)]
const ALL_OUTCOMES: [Tick; 5] = [
    Tick::Buffer,
    Tick::Move,
    Tick::Idle,
    Tick::Deferred,
    Tick::Fault,
];

impl Tick {
    /// 这一档要不要单独发一个"只挪位置"的状态 commit。`Buffer` 已经把它带在同一次 commit 里。
    fn flushes_margin(self) -> bool {
        matches!(self, Tick::Move | Tick::Deferred | Tick::Fault)
    }
}

/// 纯判定：这一 tick 归哪一档（副作用由 `App::present` 按返回值执行）。
///
/// `new_cell` = 内核给了一个和已画不同的格，**或** `force_draw` 要求补画；`frame_pending` =
/// 有 buffer 在途等帧回调。出图成不成、以及是不是补投，要等真画了才知道，故 `Fault`/`Replay`
/// 不由这里产出。
fn classify_tick(new_cell: bool, moved: bool, frame_pending: bool) -> Tick {
    if !new_cell {
        if moved { Tick::Move } else { Tick::Idle }
    } else if frame_pending {
        Tick::Deferred
    } else {
        Tick::Buffer
    }
}

/// 归属规则（纯函数）：非 tick 来源里唯有"真发了一格 buffer"才占一个**另计**档
/// （`replay_commits` / `configure_commits`），其余（挪 margin、无事、延后、出错）协议动作照做
/// 但不占归属 —— 一次重配置什么都改不了，可它照样会走一遍 `present`。
///
/// 这条规则一天内断过两次（补投混进 `commits`、重配置混进 `idle`），所以做成表驱动、逐组合测。
fn attribute_of(source: Source, outcome: Tick) -> Option<Tick> {
    match source {
        Source::Tick => Some(outcome),
        Source::Replay => (outcome == Tick::Buffer).then_some(Tick::Replay),
        Source::Configure => (outcome == Tick::Buffer).then_some(Tick::Configure),
    }
}

/// 客户端侧的呈现器。`present` 的语义与 [`petcore::render::OverlayBackend`] 一致，但所有权方向
/// 相反：M4 竖切片是客户端进程内驱动内核，`OverlayBackend` 的契约实现留给
/// "守护进程 spawn overlay 子进程"那一版（那时 `present` 由 IPC 线程调用）。
pub struct App {
    conn: Connection,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: Arc<CompositorState>,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    /// 留着不只为不丢引用：`None` 时整数 scale 走 `wl_output` 那条路
    fractional: Option<WpFractionalScaleV1>,
    /// 建第二块 layer surface（右键菜单）要用，`run()` 里的局部量活不到回调里
    layer_shell: Arc<LayerShell>,
    /// 菜单复用宠物的 `scale_num`，但每块 surface 要自己的 `wp_viewport` ⇒ 这个对象得活着
    viewporter: Option<WpViewporter>,
    /// 右键菜单那块 surface；`None` = 没开
    menu: Option<MenuState>,
    pointer: Option<ThemedPointer>,
    raw_pointer: Option<WlPointer>,
    qh: Option<QueueHandle<App>>,

    manifest: &'static Manifest,
    atlas: Option<Atlas>,
    regions: Option<Regions>,

    sim: Option<Sim<'static>>,
    stage: Option<String>,
    opts: Options,

    /// surface 的缩放，1/120 单位（`wp_fractional_scale` 的口径）；没有该协议时由
    /// `wl_output` 的整数 scale 换算。本机 180 = 1.5×。
    scale_num: u32,
    /// 逻辑 surface 尺寸（= grid + 2×margin），与内核一致
    logical: (i32, i32),
    last_intent: Option<PresentationIntent>,
    drawn_cell: Option<(u32, u32)>,
    /// 上一次设置的 margin（output 局部，逻辑像素）
    margin: (i32, i32),
    /// 当前 input region 是整块 surface（拖拽态）
    region_full: bool,
    active_region: Option<WlRegion>,
    /// 帧回调在途 ⇒ 不再附新 buffer
    frame_pending: bool,
    /// `frame_pending` 的起点，用来跑看门狗（合成器只给**可见**的 surface 发帧回调，
    /// 一次丢失就能把提交永久锁死）
    pending_since: Option<Instant>,
    /// 合成器实际把 surface 放到了哪台 output（`wl_surface.enter` 告知）。margin 换算
    /// 必须以**它**为基准，用"第一个 output"会在双屏时整体偏掉一个屏幕宽。
    on_output: Option<WlOutput>,
    /// 必须重画一帧（首帧、configure、scale 变化、拖拽结束）
    force_draw: bool,
    /// 本次 `present` 的**来源**：只有 `Tick` 才算"一个 tick 的归属"。默认 `Replay` 是故意的
    /// —— 新增调用点忘了标就落进非 tick 档（五档仍然齐），反过来忘标就静默双记（2026-10-28
    /// 一天内漏了两次：帧回调补投、`configure` 重配置）。
    source: Source,

    counters: Counters,
    ticks: u64,
    acc: f32,
    last_wake: Instant,
    deadline: Option<Instant>,
    started: Instant,
    exit: bool,
    configured: bool,

    drag: bool,
    /// 按下时指针相对脚锚点的偏移（保持"抓在哪就挂在哪"）
    grab_offset: Vec2,
    last_press: Option<(Instant, Vec2)>,
    /// 世界坐标下的指针位置
    pointer_world: Vec2,
    /// `--db` 没给 ⇒ 菜单与结果卡一律打 DRYRUN 标，且**不产生任何文件**。
    /// 从 `Options.db` 一次定死（每进程恒定，所以 `content_sig` 不含它也够精确）。
    dry_run: bool,
    /// 生效的 UI 设置（`--lang`/`--motion` > config.toml > `LC_ALL`/`LANG` > 默认，链条
    /// 只在 [`settings`] 一处解释）。整份存下来的理由不是省事：**退出摘要要能复述"这一场
    /// 会话到底用了哪一档、它是从哪一级来的"**（验收 §6-4），而只存值就把来源丢了。
    /// `MenuState.motion` 是从这里抄的副本（同 `dry_run` 的手法：`draw()` 里先
    /// `menu.as_mut()` 再读 `self.ui` 会撞借用冲突；下一步的语言行同理）。
    ui: settings::Settings,
    /// `config.toml` 的路径（**不存在也留着**）。语言行点下去时要打印"想固定就写哪儿"，
    /// 而本轮不回写文件（自写的平坦 reader 保不住用户注释 ⇒ 读改写会把文件刷平）。
    /// 存在 `App` 而不是现算：路径回退规则只在 `petstore::config` 一处，这里只复述结果。
    ui_config_path: String,
    /// 抽取层，**lazy**：第一次右键才连库。rusqlite + WAL 一打开就把页缓存和语句缓存
    /// 挂进进程，而常驻宠物的 VmHWM 门禁（M4 基线 15.7 MB）不该为"也许根本不抽"付这笔。
    gacha: Option<Gacha>,
    /// 连不上库 / 抽取失败的唯一出声处：画在菜单错误行上，**不 panic、不置 `exit`**
    /// （硬约束 3 同族 —— UI 的故障不许波及宠物，更不许波及桌面）。
    gacha_error: Option<String>,
}

/// 库与目录成对出现（`connect` 一起建、一起失败），所以没有"有库没目录"的中间态。
///
/// 这一层**不含任何 wayland 类型**（和 `menu.rs`/`card.rs` 同族约束）：好处不是整洁，
/// 而是"落库这条路"能在 headless 里被测 —— 抽取的入口是鼠标，而我绝不注入点击。
struct Gacha {
    store: Store,
    catalog: Catalog,
}

impl Gacha {
    /// 连库 + 装目录。`None` = 内存库（一条文件都不产生，UI 全程打 DRYRUN 标）。
    ///
    /// 不做 canonicalize：它要求文件已存在，第一次运行必然失败，反而把"能建库"这条正路
    /// 堵死。绝对路径的要求由 `Store::open` 自己把关（相对路径取决于 cwd，而宠物可能被
    /// systemd user service 起），口径与 CLI 一致。
    fn connect(db: Option<&str>) -> Result<Self, String> {
        let mut store = match db {
            None => Store::in_memory().map_err(|e| format!("内存库建不起来: {e}"))?,
            Some(p) => {
                Store::open(std::path::Path::new(p)).map_err(|e| format!("打不开存档 {p}: {e}"))?
            }
        };
        let catalog = Catalog::default_v1().map_err(|e| format!("公示目录读不出来: {e}"))?;
        store
            .install_catalog(&catalog)
            .map_err(|e| format!("装目录失败: {e}"))?;
        Ok(Self { store, catalog })
    }

    /// 碎片余额。`u64 → i64` 只在天文数字上饱和 —— 宁可显示封顶，也不许把余额读成负数。
    fn balance(&self) -> i64 {
        self.store
            .shards(Currency::Shards)
            .ok()
            .map(|v| i64::try_from(v).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    /// 抽 `count` 次并落库，返回菜单要的三样：结果卡行、最高档、抽完的余额。
    /// 错误只是一条给人看的字（进菜单错误行），调用方不许因为它而停环。
    ///
    /// `lang`/`scale` 都在**这一刻**烤进字符串：文案由 `l10n` 按语言选，截断按当前缩放
    /// 换算的像素预算算（§4.5）。⇒ 之后切语言/换显示器都不会重刷已经挂着的卡，这是语义
    /// 而不是缺陷（"当前显示的卡永远与生成它时的语言一致"，§7-R10）。
    fn pull(
        &mut self,
        count: u32,
        dry_run: bool,
        lang: Lang,
        scale: f32,
    ) -> Result<(Vec<String>, Rarity, i64), String> {
        let report = self
            .store
            .pull_for(
                &GachaParams::V1,
                &self.catalog,
                petstore::DEFAULT_BANNER,
                petstore::DEFAULT_PROFILE,
                count,
            )
            .map_err(|e| format!("PULL {count}: {e}"))?;
        // 显示名 = catalog 里的**中文**名（`蓝帽`），查不到回退 `item_id` ⇒ **绝不留白**。
        // 四 locale 一律显示目录中文，英文界面配中文物品名是明写的取舍（§7-R12：给目录加
        // `name_en` 要动 `petcore::gacha::catalog` 的 `deny_unknown_fields` schema，本轮不做）。
        let items: Vec<(String, Rarity, bool)> = report
            .outcomes
            .iter()
            .map(|o| {
                let name = self
                    .catalog
                    .find(&o.item_id)
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| o.item_id.clone());
                (name, o.rarity, o.duplicate)
            })
            .collect();
        let lines = menu::report_lines(
            &report.counts,
            &items,
            u64::from(report.shards_gained),
            lang,
            scale,
        );
        let best = menu::best_rarity(&report.counts).unwrap_or(Rarity::N);
        let balance = self.balance();
        eprintln!(
            "gacha: 抽 {count} ⇒ N={} R={} SR={} SSR={} 碎片 +{} 余额 {balance}（{}）",
            report.counts[0],
            report.counts[1],
            report.counts[2],
            report.counts[3],
            report.shards_gained,
            if dry_run { "DRYRUN" } else { "已落库" }
        );
        Ok((lines, best, balance))
    }
}

pub fn run(opts: Options) -> Result<(), String> {
    // 设置只在**启动时读一次**（设计书 §4 禁止定时轮询 ⇒ 不做 watcher）。顺序固定：
    // 先说文件本身怎么样（坏语法要抢在生效值前面），再说生效值和它来自哪一层。
    let cfg = petstore::config::Config::read();
    cfg.announce();
    eprintln!("ui: 配置 {}", cfg.describe());
    // CLI 那两级开关此刻真的传进来了：`--lang`/`--motion` 给错值 = 报错退出（不是静默变默认），
    // 文件/环境变量给错值 = 出声后跳过那一级的 notice（两条路的区别与理由见 `settings.rs` 模块头）。
    let settings =
        settings::Settings::from_process(opts.lang.as_deref(), opts.motion.as_deref(), &cfg)?;
    settings.announce();

    let conn = Connection::connect_to_env().map_err(|e| format!("连不上 Wayland: {e}"))?;
    let (globals, event_queue) =
        registry_queue_init::<App>(&conn).map_err(|e| format!("registry 初始化失败: {e}"))?;
    let qh = event_queue.handle();

    let compositor = Arc::new(
        CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor 不可用: {e}"))?,
    );
    let layer_shell = Arc::new(
        LayerShell::bind(&globals, &qh).map_err(|e| format!("zwlr_layer_shell_v1 不可用: {e}"))?,
    );
    // `ShmHandler` 要 `&mut Shm`，所以这层不能包 `Arc`
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm 不可用: {e}"))?;

    let manifest = load_manifest(&opts)?;
    let (atlas, regions) = load_asset(&opts, manifest)?;
    let logical = (
        manifest.grid.w + (SURFACE_MARGIN as u32) * 2,
        manifest.grid.h + (SURFACE_MARGIN as u32) * 2,
    );
    let logical = (logical.0 as i32, logical.1 as i32);

    let viewporter: Option<WpViewporter> = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
    let frac_mgr: Option<WpFractionalScaleManagerV1> = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
        .ok();

    let surface = compositor.create_surface(&qh);
    // output 尽量在建 surface 时就给准（`--monitor` 指定且已枚举到 ⇒ 合成器不会把我们
    // 放到别的屏，margin 坐标系也不会和内核舞台错位）。此刻通常还枚举不到 output
    // （首轮 dispatch 之后才有 `wl_output` 事件），给不出就传 None 让合成器选，
    // 再由 `surface_enter` 把真实那台记进 `on_output`。
    let output_state = OutputState::new(&globals, &qh);
    let target_output = opts.monitor.as_ref().and_then(|want| {
        output_state
            .outputs()
            .find(|o| output_state.info(o).and_then(|i| i.name).as_deref() == Some(want.as_str()))
    });
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface.clone(),
        Layer::Overlay,
        Some("opet"),
        target_output.as_ref(),
    );
    // 位置全靠 margin（锚定左上角），穿透靠 input region，键盘一律不参与。
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(0);
    layer.set_size(logical.0 as u32, logical.1 as u32);
    layer.set_margin(0, 0, 0, 0);

    // HiDPI：buffer 出物理像素（`wp_fractional_scale` 给 1/120 单位的真 scale），
    // viewport 把 surface 逻辑尺寸钉回 grid+margin ⇒ 合成器侧 1:1 采样。
    // 没有 viewporter 时退回 `set_buffer_scale`（只能整数，且与 viewport 互斥）。
    let viewport = viewporter
        .as_ref()
        .map(|v| v.get_viewport(&surface, &qh, ()));
    if let Some(vp) = &viewport {
        vp.set_destination(logical.0, logical.1);
    }
    let fractional = frac_mgr
        .as_ref()
        .map(|m| m.get_fractional_scale(&surface, &qh, ()));
    layer.commit();

    // 池按 2× 预留（128 逻辑 ⇒ 256 物理 = 256 KiB × 3 槽），更大尺寸由 `SlotPool` 自己扩。
    let phys = (logical.0 * 2, logical.1 * 2);
    let initial = phys.0 as usize * phys.1 as usize * 4 * 3;
    let pool = SlotPool::new(initial, &shm).map_err(|e| format!("shm 池建不起来: {e}"))?;

    // `--db` 没给 ⇒ 演示模式（不建文件、UI 打 DRYRUN 标）。在 `opts` 被搬进 App 之前取出来。
    let dry_run = opts.db.is_none();
    let mut app = App {
        conn,
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state,
        compositor,
        shm,
        pool,
        layer,
        viewport,
        fractional,
        layer_shell,
        viewporter,
        menu: None,
        pointer: None,
        raw_pointer: None,
        qh: Some(qh),
        manifest,
        atlas,
        regions,
        sim: None,
        stage: opts.monitor.clone(),
        opts,
        scale_num: SCALE_UNIT,
        logical,
        last_intent: None,
        drawn_cell: None,
        margin: (0, 0),
        region_full: false,
        active_region: None,
        frame_pending: false,
        pending_since: None,
        on_output: None,
        force_draw: true,
        source: Source::Replay,
        counters: Counters::default(),
        ticks: 0,
        acc: 0.0,
        last_wake: Instant::now(),
        deadline: None,
        started: Instant::now(),
        exit: false,
        configured: false,
        drag: false,
        grab_offset: Vec2::ZERO,
        dry_run,
        ui: settings,
        ui_config_path: cfg.path.display().to_string(),
        gacha: None,
        gacha_error: None,
        last_press: None,
        pointer_world: Vec2::ZERO,
    };
    let mut evl: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = evl.handle();
    let signal: LoopSignal = evl.get_signal();
    let period = Duration::from_secs_f64(1.0 / TICK_HZ as f64);
    handle
        .insert_source(Timer::from_duration(period), move |_at, _meta, app| {
            app.on_tick(&signal);
            TimeoutAction::ToDuration(period)
        })
        .map_err(|e| format!("定时器装不上: {e:?}"))?;
    WaylandSource::new(app.conn.clone(), event_queue)
        .insert(handle)
        .map_err(|e| format!("wayland source 装不上: {e:?}"))?;

    evl.run(None, &mut app, |_app| {})
        .map_err(|e| format!("事件循环崩了: {e}"))?;

    app.report();
    Ok(())
}

// ───────────────────────────── 资产与清单 ─────────────────────────────

/// `Box::leak`：内核的 `Sim<'m>` 借一个 `&'m Manifest`，而 `App` 必须是 `'static`
/// （`QueueHandle<App>` 的要求）。清单是进程级只读的几 KB，故意不回收。
fn load_manifest(opts: &Options) -> Result<&'static Manifest, String> {
    let m = match opts.manifest.as_deref() {
        None => petcore::assets::default_manifest(),
        Some(p) => Manifest::load(p).map_err(|e| format!("清单 {p} 加载失败: {e}"))?,
    };
    Ok(Box::leak(Box::new(m)))
}

fn load_asset(
    opts: &Options,
    manifest: &Manifest,
) -> Result<(Option<Atlas>, Option<Regions>), String> {
    if opts.test_rect {
        return Ok((None, None));
    }
    let Some(path) = opts.manifest.as_deref() else {
        // 内置 fixture 只有元数据、没有像素，画不出任何东西
        return Ok((None, None));
    };
    let dir = std::path::Path::new(path)
        .parent()
        .ok_or_else(|| format!("{path} 没有父目录"))?;
    let sheet = dir.join("sheet.webp");
    let atlas = Atlas::load(
        sheet.to_str().ok_or("sheet 路径不是 UTF-8")?,
        manifest.grid.cols as u32,
        manifest.grid.rows as u32,
        manifest.grid.w,
        manifest.grid.h,
    )
    .map_err(|e| format!("{}: {e}", sheet.display()))?;
    let polygon = dir.join("polygon.json");
    let regions = Regions::load(
        polygon.to_str().ok_or("polygon 路径不是 UTF-8")?,
        manifest.grid.w,
        manifest.grid.h,
        SURFACE_MARGIN,
    )
    .map_err(|e| format!("{}: {e}", polygon.display()))?;
    eprintln!(
        "asset: sheet {}x{} {} KB → 解码 {} ms / {:.3} MiB；区域 {} 格",
        atlas.width,
        atlas.height,
        atlas.encoded_bytes / 1024,
        atlas.decode_ms,
        atlas.decoded_bytes() as f64 / (1024.0 * 1024.0),
        regions.frame_count(),
    );
    Ok((Some(atlas), Some(regions)))
}

// ───────────────────────────── 几何换算 ─────────────────────────────

/// **生效**的缩放（1/120 单位）。`wp_viewport` 缺席时只能用整数档：`wl_surface
/// .set_buffer_scale` 只吃整数，而分数档（1.5×）若照样出 1.5× 的 buffer 却不带
/// `buffer_scale`，合成器会按 1× 把它当成逻辑尺寸 ⇒ 宠物整体大 1.5 倍且坐标错位。
/// 所以这里向下取整（1.5 ⇒ 1.0），宁可略糊也不让尺寸/位置双错。
///
/// 自由函数而不是 `App` 的方法：菜单那块 surface 拿不到 `&App`（`App` 正被可变借用），
/// 但**必须**用同一套口径 —— 两处各写一份"1.5 还是 2"迟早漂移，而漂移的表现是菜单发糊
/// 或错位，只有肉眼能看出来。
fn eff_units(scale_num: u32, has_viewport: bool) -> u32 {
    if has_viewport {
        scale_num
    } else {
        (scale_num / SCALE_UNIT).max(1) * SCALE_UNIT
    }
}

fn scale_of(scale_num: u32, has_viewport: bool) -> f32 {
    eff_units(scale_num, has_viewport).max(SCALE_UNIT) as f32 / SCALE_UNIT as f32
}

/// 逻辑像素 → 物理像素（四舍五入；margin 与格子各自取整，保证像素与区域边界自洽）。
fn to_phys_of(logical: f32, scale_num: u32, has_viewport: bool) -> i32 {
    (logical * scale_of(scale_num, has_viewport)).round() as i32
}

/// viewport 缺席时才退回 `set_buffer_scale`（它只能整数）；有 viewport 时
/// `buffer_scale` 必须是 1，否则和 `wp_viewport` 一起用就是协议错误。
fn buffer_scale_of(scale_num: u32, has_viewport: bool) -> Option<i32> {
    let s = scale_of(scale_num, has_viewport);
    if !has_viewport && s >= 2.0 {
        Some(s as i32)
    } else {
        None
    }
}

impl App {
    fn scale_f(&self) -> f32 {
        scale_of(self.scale_num, self.viewport.is_some())
    }

    fn to_phys(&self, logical: f32) -> i32 {
        to_phys_of(logical, self.scale_num, self.viewport.is_some())
    }

    /// 附了一个新 buffer ⇒ 开始等它的帧回调（同时给看门狗上表）。
    fn arm_frame_wait(&mut self) {
        self.frame_pending = true;
        self.pending_since = Some(Instant::now());
    }

    /// 帧回调回来 / configure / attach 失败 ⇒ 提交通道重新可用。
    fn clear_frame_wait(&mut self) {
        self.frame_pending = false;
        self.pending_since = None;
    }

    fn buffer_scale(&self) -> Option<i32> {
        buffer_scale_of(self.scale_num, self.viewport.is_some())
    }

    /// 我们这块 surface **实际所在**的 output：优先用 `wl_surface.enter` 报的那台，其次
    /// 才按 `--monitor` 的名字找，最后退回第一台。margin 与 `Monitor.rect` 的换算全走这里。
    fn pick_output(&self) -> Option<WlOutput> {
        if let Some(o) = self.on_output.as_ref() {
            if self.output_state.info(o).is_some() {
                return Some(o.clone());
            }
        }
        let mut first = None;
        for o in self.output_state.outputs() {
            let Some(info) = self.output_state.info(&o) else {
                continue;
            };
            if self
                .opts
                .monitor
                .as_deref()
                .is_some_and(|w| info.name.as_deref() == Some(w))
            {
                return Some(o);
            }
            if first.is_none() {
                first = Some(o);
            }
        }
        first
    }

    fn output_origin(&self) -> (i32, i32) {
        self.pick_output()
            .and_then(|o| self.output_state.info(&o))
            .and_then(|i| i.logical_position)
            .unwrap_or((0, 0))
    }

    /// 内核坐标（全局虚拟桌面，y 向下）→ layer margin（**所锚定 output 的可用区**局部坐标）。
    ///
    /// 两处换算，少一处都会错位：
    /// 1. 减去 output 原点（多屏时不减会偏掉一整个 monitor 宽度）；
    /// 2. 减去 `panel_top`：layer-shell 规定 margin 从**可用区**边缘量起，顶部栏的
    ///    exclusive zone 会把 surface 整体下推（本机实测请求 200 ⇒ 渲染 220），不补回来
    ///    宠物的脚就掉到屏幕外。
    fn world_to_margin(&self, surface_pos: Vec2) -> (i32, i32) {
        let (ox, oy) = self.output_origin();
        (
            (surface_pos.x - ox as f32).round() as i32,
            (surface_pos.y - oy as f32 - self.opts.panel_top).round() as i32,
        )
    }

    /// 把 output 列表对齐进内核的 `World`（热插拔走 `WorldEvent`，不做轮询）。
    fn sync_world(&mut self) {
        let infos: Vec<_> = self
            .output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .collect();
        if infos.is_empty() {
            return;
        }
        // 舞台 = 用户 `--monitor` 指定的那台；没指定就跟着**surface 实际所在**的那台
        // （`surface_enter` 报的 `on_output`），最后退回第一个。
        let chosen = self
            .opts
            .monitor
            .clone()
            .or_else(|| {
                self.pick_output()
                    .and_then(|o| self.output_state.info(&o))
                    .and_then(|i| i.name.clone())
            })
            .or_else(|| infos.first().and_then(|i| i.name.clone()))
            .unwrap_or_default();
        // 只有 `wp_fractional_scale` 缺席时才拿 `wl_output` 的整数 scale 出图：本机真值
        // 1.5×，而 wl_output 只能报 2 ⇒ 用它会被合成器再缩 0.75×，边缘发糊。
        // 有该协议时以 `preferred_scale` 为准（第一帧前通常已到）。
        if self.fractional.is_none() {
            let want = infos
                .iter()
                .find(|i| i.name.as_deref() == Some(chosen.as_str()))
                .or(infos.first())
                .map(|i| i.scale_factor.max(1) as u32 * SCALE_UNIT)
                .unwrap_or(SCALE_UNIT);
            if self.scale_num != want {
                self.scale_num = want;
                self.redraw_for_scale();
            }
        }
        if self.opts.test_rect {
            // 纯色矩形模式不接内核 ⇒ 不该有 Sim，但 scale 仍要按 output 出 buffer
            self.stage = Some(chosen);
            return;
        }
        let monitors: Vec<Monitor> = infos
            .iter()
            .map(|i| {
                let pos = i.logical_position.unwrap_or((0, 0));
                let size = i.logical_size.unwrap_or((0, 0));
                // 舞台 = **可见**那块：顶部被栏占掉 panel_top，底边由内核自己的
                // `panel_bottom`（`Monitor::ground()`）处理，这里不重复减。
                let top = self.opts.panel_top.min(size.1.max(0) as f32);
                Monitor {
                    name: i.name.clone().unwrap_or_default(),
                    rect: Rect::new(
                        pos.0 as f32,
                        pos.1 as f32 + top,
                        size.0 as f32,
                        (size.1 as f32 - top).max(1.0),
                    ),
                    // 舞台那台给**生效**缩放（`wp_fractional_scale` 的真值，本机 1.5）；
                    // 其余台只有 `wl_output` 的整数近似可用（1.5× 会被报成 2）。内核现在
                    // 不读这个字段，但别把错的整数近似当事实传给下游。
                    scale: if i.name.as_deref() == Some(chosen.as_str()) {
                        self.scale_f()
                    } else {
                        i.scale_factor.max(1) as f32
                    },
                    primary: i.name.as_deref() == Some(chosen.as_str()),
                    panel_bottom: self.opts.panel_bottom,
                }
            })
            .collect();

        if self.sim.is_none() {
            let Some(world) = World::new(monitors.clone()) else {
                return;
            };
            self.stage = Some(chosen.clone());
            let sim = Sim::new(
                world,
                self.manifest,
                self.opts.seed,
                BehaviorConfig::default(),
            );
            self.sim = Some(sim);
            eprintln!(
                "world: 舞台 {}（{} 台显示器，逻辑几何取自 wl_output；舞台 {:?}，panel_top {} / panel_bottom {}）",
                chosen,
                monitors.len(),
                monitors
                    .iter()
                    .find(|m| m.name == chosen)
                    .map(|m| { (m.rect.pos.x, m.rect.pos.y, m.rect.size.x, m.rect.size.y,) }),
                self.opts.panel_top,
                self.opts.panel_bottom,
            );
        } else if let Some(sim) = self.sim.as_mut() {
            let known: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
            for m in monitors {
                sim.apply_world_event(WorldEvent::MonitorGeometry {
                    monitor: m.name.clone(),
                    rect: m.rect,
                    scale: m.scale,
                    panel_bottom: m.panel_bottom,
                });
            }
            let stale: Vec<String> = sim
                .world
                .monitors
                .iter()
                .map(|m| m.name.clone())
                .filter(|n| !known.contains(n))
                .collect();
            for n in stale {
                sim.apply_world_event(WorldEvent::MonitorRemoved { monitor: n });
            }
        }
    }
}

// ───────────────────────────── 菜单 surface（M3-b）─────────────────────────────

/// 右键宠物开出的第二块 layer surface。
///
/// **为什么什么都要自己一套**：同层两块 surface 若共用 `SlotPool`/`frame_pending`，宠物那
/// 三档提交计数会被菜单污染（§4 的省电门禁就没法读），而且一块 surface 的帧回调没回来会把
/// 另一块也锁死。所以 pool、在途标记、看门狗、`wl_region` 全部分开，共享的只有 `qh`、
/// `wl_compositor`、`wl_shm`、layer-shell、viewporter 和**宠物的 `scale_num`**（不建第二个
/// `wp_fractional_scale`：那块对象的回调只会多一个漂移源）。
struct MenuState {
    layer: LayerSurface,
    /// `frame` / 指针事件按**对象身份**分流，这块要能被认出来
    surface: WlSurface,
    viewport: Option<WpViewport>,
    pool: SlotPool,
    /// 宠物 scale 的镜像（开菜单时取一次，之后每 tick 同步）
    scale_num: u32,
    logical: (i32, i32),
    /// 布局表只算一次：`hit_test` 与 draw 共用同一份矩形
    items: Vec<ItemRect>,
    anim: MenuAnim,
    /// 余额行。**只在事件上刷新**（开菜单时、每次抽取后），不每 tick 查库：
    /// §4 的省电契约明写"事件驱动，禁止定时轮询"，而 60 Hz 一条 `SELECT` 正是轮询。
    /// 代价是菜单开着时外部（CLI）的变动不会立刻反映 —— v1 没有 IPC，接受。
    balance: i64,
    /// 从 `App` 抄来的演示标（每进程恒定，不进 `content_sig` 也精确）
    dry_run: bool,
    /// 从 `App.ui.motion` 抄来的动画强度档（同 `dry_run`：每进程恒定 ⇒ 不作为字段进签名，
    /// 但要**传给** `content_sig` 去算"此刻画的是哪一帧"；`step`/`draw` 每格也都要读它，
    /// 否则 `--motion` 就是个只打印不生效的假开关）。
    motion: Motion,
    /// 从 `App.ui.lang` 抄来的当前语言。**必须进 `content_sig`**（与 `motion` 不同族：
    /// 语言现在能在运行中被点着换，不进签名就出现"点了但卡片没换字"）。抄副本而不是
    /// 每处借 `self.lang`：`draw()` 里 `self.pool`/`self.anim` 已经可变借用着 self。
    lang: Lang,
    /// 上一次 commit 的内容签名 ⇒ 静止时"什么都不发"
    last_sig: Option<u64>,
    frame_pending: bool,
    pending_since: Option<Instant>,
    active_region: Option<WlRegion>,
    configured: bool,
    force_draw: bool,
}

impl MenuState {
    fn has_viewport(&self) -> bool {
        self.viewport.is_some()
    }

    fn scale_f(&self) -> f32 {
        scale_of(self.scale_num, self.has_viewport())
    }

    fn to_phys(&self, logical: f32) -> i32 {
        to_phys_of(logical, self.scale_num, self.has_viewport())
    }

    fn buffer_scale(&self) -> Option<i32> {
        buffer_scale_of(self.scale_num, self.has_viewport())
    }

    /// 宠物 scale 变了要跟着重画（菜单自己不发 `preferred_scale`，只能镜像）。
    fn mirror_scale(&mut self, scale_num: u32) {
        if self.scale_num != scale_num {
            self.scale_num = scale_num;
            self.force_draw = true;
        }
    }

    /// 推进一格动画。返回 `true` = 相位变了（像素必然不同）。
    fn step(&mut self) -> bool {
        if !self.anim.is_busy() {
            return false;
        }
        let next = menu::advance(&self.anim, self.motion);
        let changed = next != self.anim;
        self.anim = next;
        changed
    }

    fn arm(&mut self) {
        self.frame_pending = true;
        self.pending_since = Some(Instant::now());
    }

    fn clear_wait(&mut self) {
        self.frame_pending = false;
        self.pending_since = None;
    }

    /// 看门狗同构（合成器只对可见 surface 发帧回调）；返回是否触发。
    fn tick_watchdog(&mut self, now: Instant) -> bool {
        let overdue = self
            .pending_since
            .is_some_and(|t| now.duration_since(t) > Duration::from_secs_f64(3.0 / TICK_HZ as f64));
        if overdue {
            self.clear_wait();
            self.force_draw = true;
        }
        overdue
    }

    /// 收回协议对象。顺序：region → viewport → layer_surface → wl_surface
    /// （`zwlr_layer_surface_v1.destroy` 之后再碰它就是协议错误）。
    fn teardown(self) {
        if let Some(r) = &self.active_region {
            r.destroy();
        }
        if let Some(v) = &self.viewport {
            v.destroy();
        }
        if let SurfaceKind::Wlr(l) = self.layer.kind() {
            l.destroy();
        }
        self.surface.destroy();
    }

    /// 出一帧卡片。返回是否真的 commit 了。
    fn draw(&mut self, compositor: &CompositorState, qh: &QueueHandle<App>, error: &str) -> bool {
        if !self.configured || self.frame_pending {
            return false;
        }
        let sig = menu::content_sig(&self.anim, self.balance, error, self.lang, self.motion);
        if self.last_sig == Some(sig) && !self.force_draw {
            return false;
        }
        let dry_run = self.dry_run;
        let balance = self.balance;
        let motion = self.motion;
        let lang = self.lang;
        let bw = self.to_phys(self.logical.0 as f32).max(1);
        let bh = self.to_phys(self.logical.1 as f32).max(1);
        let stride = bw * 4;
        // `scale_f` 先取：下面 `self.pool` 是可变借用，同一个表达式里不能再不可变借 self
        let scale = self.scale_f();
        let created = self
            .pool
            .create_buffer(bw, bh, stride, wl_shm::Format::Argb8888);
        let (buffer, canvas) = match created {
            Ok(v) => v,
            Err(e) => {
                eprintln!("菜单 create_buffer 失败: {e:?}");
                return false;
            }
        };
        // 像素全在 `card.rs`（纯函数，headless 能出图给人看）。这里的 `self.anim` 已经
        // 由 `step()` 推进过，签名相同 ⇒ 上面就早退了，静止的菜单一帧都不投。
        card::paint(
            &mut canvas[..],
            bw,
            bh,
            stride as usize,
            &self.anim,
            card::Ctx {
                balance,
                dry_run,
                error,
                scale,
                motion,
                lang,
            },
        );
        // 句柄克隆（不是对象克隆）：下面的 `self.arm()` 要可变借 self，不能同时留引用
        let surface = self.surface.clone();
        if let Some(s) = self.buffer_scale() {
            surface.set_buffer_scale(s);
        }
        surface.damage_buffer(0, 0, bw, bh);
        self.arm();
        surface.frame(qh, surface.clone());
        if buffer.attach_to(&surface).is_err() {
            self.clear_wait();
            return false;
        }
        // 菜单整块都该吃掉点击（它就是来接收点击的），region = 卡片矩形，**逻辑**坐标
        let region = compositor.wl_compositor().create_region(qh, ());
        region.add(0, 0, self.logical.0, self.logical.1);
        surface.set_input_region(Some(&region));
        if let Some(old) = self.active_region.replace(region) {
            old.destroy();
        }
        self.layer.commit();
        self.last_sig = Some(sig);
        self.force_draw = false;
        true
    }
}

impl App {
    /// 可用区尺寸（output 逻辑尺寸减掉顶栏）：菜单 clamp 的边界必须和 margin 同口径，
    /// 否则菜单会"合规地"贴进顶栏底下。
    fn output_available(&self) -> (i32, i32) {
        let info = self
            .pick_output()
            .and_then(|o| self.output_state.info(&o))
            .or_else(|| {
                self.output_state
                    .outputs()
                    .find_map(|o| self.output_state.info(&o))
            });
        let (w, h) = info
            .and_then(|i| i.logical_size)
            .unwrap_or((CARD_W.max(self.logical.0), CARD_H.max(self.logical.1)));
        (
            w.max(self.logical.0),
            (h - self.opts.panel_top.round() as i32).max(self.logical.1),
        )
    }

    fn toggle_menu(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
        } else {
            self.open_menu();
        }
    }

    fn open_menu(&mut self) {
        let qh = self.qh();
        // 几何先算完（下面的建 surface 全是不可变借用，别和 `self.menu =` 的可变借用打架）
        let (mx, my) = self.margin;
        let pet = RectI::new(mx, my, self.logical.0, self.logical.1);
        let (aw, ah) = self.output_available();
        let at = menu::placement(pet, RectI::new(0, 0, aw, ah));
        let scale_num = self.scale_num;
        let has_viewport = self.viewporter.is_some();
        let target = self.on_output.clone();
        let logical = (CARD_W, CARD_H);
        let phys = (
            to_phys_of(logical.0 as f32, scale_num, has_viewport).max(1),
            to_phys_of(logical.1 as f32, scale_num, has_viewport).max(1),
        );
        // "打开菜单"就是一个事件：连库（第一次才真连）+ 查一次余额。
        // 连不上也照样把菜单开出来 —— 错误行会说话，而打不开库绝不是宠物的死因。
        let balance = if self.ensure_gacha() {
            self.gacha_balance()
        } else {
            0
        };
        let dry_run = self.dry_run;
        let motion = self.ui.motion;
        let lang = self.ui.lang;

        let surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(
            &qh,
            surface.clone(),
            Layer::Overlay,
            // 独立 namespace：`hyprctl layers` 要能分别认出两块，同层堆叠序不稳定 ⇒
            // 摆位靠 `placement` 保证矩形不相交
            Some("opet-menu"),
            target.as_ref(),
        );
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        layer.set_size(logical.0 as u32, logical.1 as u32);
        // margin 顺序 = top,right,bottom,left
        layer.set_margin(at.1, 0, 0, at.0);
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, &qh, ()));
        if let Some(vp) = &viewport {
            vp.set_destination(logical.0, logical.1);
        }
        layer.commit();

        let initial = (phys.0 * phys.1 * 4 * 2) as usize;
        let pool = match SlotPool::new(initial, &self.shm) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("菜单 shm 池建不起来: {e:?}");
                if let Some(v) = &viewport {
                    v.destroy();
                }
                surface.destroy();
                return;
            }
        };
        self.menu = Some(MenuState {
            layer,
            surface,
            viewport,
            pool,
            scale_num,
            logical,
            items: menu::rects(),
            anim: MenuAnim::Menu,
            balance,
            dry_run,
            motion,
            lang,
            last_sig: None,
            frame_pending: false,
            pending_since: None,
            active_region: None,
            configured: false,
            force_draw: true,
        });
        eprintln!(
            "menu: 打开于 margin {}x{}（宠物 {mx},{my} {}x{}，可用区 {aw}x{ah}）",
            at.0, at.1, self.logical.0, self.logical.1
        );
    }

    fn close_menu(&mut self) {
        if let Some(m) = self.menu.take() {
            m.teardown();
        }
    }

    /// 每 tick 走一遍：同步 scale、跑看门狗、推进相位、按签名决定要不要提交。
    /// 静止的菜单**一次 commit 都不发**（`counters.menu_commits` 必须不随 tick 增长）。
    fn update_menu(&mut self, now: Instant) {
        if self.menu.is_none() {
            return;
        }
        let scale_num = self.scale_num;
        let error = self.menu_error_line();
        let mut m = self.menu.take().expect("上面查过 is_some");
        m.mirror_scale(scale_num);
        if m.tick_watchdog(now) {
            self.counters.menu_watchdogs += 1;
            eprintln!("警告：菜单帧回调超时 ⇒ 强制解锁提交");
        }
        m.step();
        let qh = self.qh();
        if m.draw(&self.compositor, &qh, &error) {
            self.counters.menu_commits += 1;
        }
        self.menu = Some(m);
    }

    /// 菜单被点：命中哪一项。`at` 是菜单 surface 的**逻辑**局部坐标。
    ///
    /// 分派**只走 [`menu::press`]**（命中 + 相位把关），不走 [`menu::hit_test`]：这块 surface
    /// 是同一张 240×150，开箱那段时间（`Burst`/`Reveal`）整屏由 `card::paint` 重画，三个行项
    /// 与语言行**一个都没画出来**，而静态命中表照样答得出 ⇒ 曾经出现"点结果卡的位置就执行
    /// 那一行的功能"（用户 2026-10-28 报）。把关放在命中侧而不是各 `match` 分支里，是为了
    /// **以后新增菜单项不会忘记问相位**（门禁 = `the_card_phase_opens_no_row_to_the_pointer`）。
    fn on_menu_press(&mut self, at: (f32, f32)) {
        let Some(item) = self
            .menu
            .as_ref()
            .and_then(|m| menu::press(&m.items, at, &m.anim))
        else {
            return;
        };
        match item {
            menu::MenuItem::Close => self.close_menu(),
            menu::MenuItem::Pull1 => self.do_pull(1),
            menu::MenuItem::Pull10 => self.do_pull(10),
            // 语言行的相位把关现在也在 `press` 里了。留一条这里特有的口径：已挂着的卡**保持它
            // 生成那一刻的语言**（§7-R10：这不是 bug，是"当前显示的卡永远与生成它时的语言一致"），
            // 所以"切完语言卡片跟着换字"只在下一张卡上成立。
            menu::MenuItem::Lang => self.cycle_lang(),
        }
    }

    /// 点语言行：zh→en→ja→ko→zh，**只改本进程，不回写文件**。
    ///
    /// 不写回的理由不是省事：本轮的 reader 是自写的平坦 `key = "value"` 解析，读改写会把
    /// 用户的注释整段刷平 ⇒ **丢注释比"重启回默认"更坏**（§7-R15）。所以切换时把持久化
    /// 写法打在 stderr，让人自己决定要不要写。两处一起改（`App.ui.lang` 是真相源、
    /// `MenuState.lang` 是 draw 用的副本）—— 全项目只有这一处写它。
    fn cycle_lang(&mut self) {
        let next = self.ui.lang.next();
        self.ui.lang = next;
        self.ui.lang_from = settings::ValueSource::MenuClick;
        if let Some(m) = self.menu.as_mut() {
            m.lang = next;
            m.force_draw = true;
        }
        eprintln!(
            "ui: 语言 → {} ({})；本轮会话生效，想固定请在 {} 写 [ui] lang = \"{}\"",
            next.self_name(),
            next.code(),
            self.ui_config_path,
            next.code()
        );
    }

    /// 第一次用到才连库，之后复用同一条连接。返回"库是否可用"，调用方据此决定余额读不读。
    ///
    /// 失败**只写错误行**：菜单照样开得出、宠物照样跑（硬约束 3 同族 —— UI 的故障不许
    /// 波及宠物，更不许波及桌面）。
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
                self.gacha_error = None;
                true
            }
            Err(msg) => {
                self.gacha_bailed(msg);
                false
            }
        }
    }

    /// 抽取层唯一的出声处：stderr + 菜单错误行。**不 panic、不置 `exit`**。
    fn gacha_bailed(&mut self, msg: String) {
        eprintln!("gacha: {msg}");
        self.gacha_error = Some(msg);
    }

    fn gacha_balance(&self) -> i64 {
        self.gacha.as_ref().map(|g| g.balance()).unwrap_or(0)
    }

    /// 点"抽一次/十连"。同步抽完（单抽实测 0.61 ms、十连 0.85 ms，都在 16.6 ms 的 tick
    /// 预算里），不值得为它引入一条线程和它的竞态。
    ///
    /// 宠物本体复用 `Command::DoubleTap`（3 帧 react 叠加动画）：M3-b 不加新 Command，
    /// 碰 `machine.rs` 就是威胁状态指纹与 §3.3 的迁移表。DoubleTap 附带"着地且允许 run
    /// 则起跑"的副作用，这是计划里明写接受的代价。
    fn do_pull(&mut self, count: u32) {
        if !self.ensure_gacha() {
            return;
        }
        let dry_run = self.dry_run;
        // 语言与缩放都在这一刻取：结果卡的文案与截断预算属于**这张卡**，之后用户切语言
        // 或换显示器都不会把已经烤好的字符串重排（`baked_lines_keep_the_language_they_were_pulled_in`）。
        let lang = self.ui.lang;
        let scale = self.menu.as_ref().map(|m| m.scale_f()).unwrap_or(1.0);
        let pulled = match self.gacha.as_mut() {
            Some(g) => g.pull(count, dry_run, lang, scale),
            None => return,
        };
        let (lines, best, balance) = match pulled {
            Ok(v) => {
                self.gacha_error = None;
                v
            }
            // 库被别的进程锁住、公示与库里对不上…… 都只出声，不停宠物
            Err(msg) => {
                self.gacha_bailed(msg);
                return;
            }
        };
        if let Some(m) = self.menu.as_mut() {
            // 从哪一格开始由 `menu::after_pull` 定（`off` 档根本不进 `Burst`）。分派写成
            // 纯函数是为了让它有自动化覆盖 —— 这条写在 `App` 里就只有真机看得出来。
            m.anim = menu::after_pull(best, lines, self.ui.motion);
            m.balance = balance;
        }
        if let Some(sim) = self.sim.as_mut() {
            sim.send(Command::DoubleTap);
        }
    }

    /// 菜单的错误行（连不上库、抽取失败都靠它出声）。
    fn menu_error_line(&self) -> String {
        self.gacha_error.clone().unwrap_or_default()
    }
}

// ───────────────────────────── 定时与呈现 ─────────────────────────────

impl App {
    fn on_tick(&mut self, signal: &LoopSignal) {
        let now = Instant::now();
        // 合成器关掉了我们的 layer（锁屏/它自己收回 surface）⇒ 立刻停环。
        // 不停的话 `--seconds 0` 会朝着一个已死的 layer surface 以 60 Hz 提交到天荒地老。
        if self.exit {
            signal.stop();
            return;
        }
        match self.deadline {
            Some(dl) if now >= dl => {
                signal.stop();
                return;
            }
            None => {
                if let Some(secs) = self.opts.seconds {
                    self.deadline = Some(self.started + Duration::from_secs_f32(secs));
                }
            }
            _ => {}
        }

        if self.sim.is_none() && !self.opts.test_rect {
            self.sync_world();
        }

        // 看门狗：合成器只对**可见**的 surface 发帧回调（未映射/被隐藏/output 关掉），
        // 丢一次就把提交通道永久锁死、宠物视觉冻在原地。超 3 个周期强制解锁并补画。
        if self.frame_pending
            && self.pending_since.is_some_and(|t| {
                now.duration_since(t) > Duration::from_secs_f64(3.0 / TICK_HZ as f64)
            })
        {
            self.frame_pending = false;
            self.pending_since = None;
            self.force_draw = true;
            self.counters.watchdogs += 1;
            eprintln!(
                "警告：帧回调超时（>{:.0}ms）⇒ 强制解锁提交（第 {} 次）",
                3000.0 / TICK_HZ as f64,
                self.counters.watchdogs
            );
        }

        // 菜单自己走一遍（独立 pool / 独立看门狗）。放在宠物 `!configured` 早退之前：
        // 宠物的映射状态和菜单该不该动帧毫无关系。
        self.update_menu(now);

        // 第一次 configure 之前 surface 还没映射，`present` 会直接早退 ⇒ 这期间推进内核就等于
        // **把"内容变了"的 intent 扔掉**（宠物可能一直不显形）。所以这里干脆不推进。
        if !self.configured {
            self.last_wake = now;
            self.acc = 0.0;
            return;
        }

        let dt = now
            .duration_since(self.last_wake)
            .as_secs_f64()
            .clamp(0.0, 0.25) as f32;
        self.last_wake = now;
        self.acc += dt;
        let step = 1.0 / TICK_HZ;
        let mut budget = MAX_CATCHUP;
        // 这一段是**唯一**算"一个 tick"的地方 ⇒ 环外一律 `Source::Replay`（默认非归属档），
        // 免得哪个新加的调用点忘了标就静默双记（见 `Source` 的注释）。
        self.source = Source::Tick;
        while self.acc >= step && budget > 0 {
            self.acc -= step;
            budget -= 1;
            self.ticks += 1;
            if self.opts.test_rect {
                if self.force_draw {
                    self.test_rect_frame();
                }
            } else if let Some(sim) = self.sim.as_mut() {
                let (_, intent) = sim.step();
                self.present(&intent);
            }
        }
        // 环外默认"不占归属"（帧回调与 configure 各自还会显式标一次）
        self.source = Source::Replay;
        if self.acc >= step {
            // 积压超过补帧预算 ⇒ 丢弃，不留着下一轮连环提交
            self.acc = 0.0;
        }
    }

    /// 三级分派主入口（等价于 `OverlayBackend::present`）。
    ///
    /// 三条轴分得很清楚，别再缠在一起：
    /// - **归档**（[`classify_tick`]）：这一趟算 `Buffer`/`Move`/`Idle`/`Deferred`/`Fault` 哪一档；
    /// - **副作用**：发不发 buffer、发不发状态 commit。延后/出错在 margin 动了时照样发状态 commit，
    ///   但不改变归档；
    /// - **归属**（[`Self::attribute`]）：**只有 `Source::Tick` 才占五档**。帧回调补投和 `configure`
    ///   重配置都不是 tick —— 补投那一笔混进 `commits` 就"补投几次超几次"，重配置那几笔混进
    ///   `idle`/`moves` 同理（2026-10-28 轮 3 实测：5 次重配置 ⇒ 五档当场多 5）。
    pub fn present(&mut self, intent: &PresentationIntent) {
        if !self.configured {
            return;
        }
        self.last_intent = Some(intent.clone());
        let cell = intent.sprite.map(|s| (s.row as u32, s.col as u32));
        let moved = self.apply_margin(intent.surface.pos);
        let new_cell = cell.is_some() && (cell != self.drawn_cell || self.force_draw);

        let outcome = match classify_tick(new_cell, moved, self.frame_pending) {
            Tick::Move => {
                self.layer.commit();
                Tick::Move
            }
            Tick::Idle => Tick::Idle,
            Tick::Deferred => {
                // 一个在途 buffer 都没回来 ⇒ 延后（帧回调里补），绝不排队。
                // ⚠️ 必须同时置 `force_draw`：帧回调只在 force_draw 为真时才重放 `last_intent`，
                // 漏掉这步 = **这一格永久丢失**（`Sleep` 放完最后一格、一次性动画的尾帧
                // 只会发一次 ⇒ 挂在那一格不再变）。
                self.force_draw = true;
                self.flush_margin_if(Tick::Deferred, moved);
                Tick::Deferred
            }
            Tick::Buffer => {
                // 走这条 ⇒ 附新 buffer，margin 的改动同一次 commit 生效
                if matches!(cell, Some((r, c)) if self.draw_cell(r, c)) {
                    self.drawn_cell = cell;
                    self.force_draw = false;
                    Tick::Buffer
                } else {
                    // 出图失败 ⇒ 这趟没有带 buffer 的 commit，但 `apply_margin` 已经把新
                    // margin 排进待提交状态。补一个状态 commit 把它冲出去，否则合成器停在旧位置、
                    // 而客户端以为已经挪了（下一趟 `moved=false` ⇒ 再也不发）。
                    self.flush_margin_if(Tick::Fault, moved);
                    Tick::Fault
                }
            }
            // `classify_tick` 不产出这三档（`classification_partitions_the_inputs` 钉着）
            Tick::Replay | Tick::Configure | Tick::Fault => {
                unreachable!("这三档只在出图之后才可能成立")
            }
        };
        self.attribute(outcome);
    }

    /// 这一档若"只挪位置"也要把 margin 冲出去（`Buffer` 已把它带在同一次 commit 里，故不发）。
    fn flush_margin_if(&mut self, tick: Tick, moved: bool) {
        if tick.flushes_margin() && moved {
            self.layer.commit();
        }
    }

    /// 归属：交给纯函数 [`attribute_of`]，这里只负责记档。
    fn attribute(&mut self, outcome: Tick) {
        if let Some(hit) = attribute_of(self.source, outcome) {
            self.counters.bump(hit);
        }
    }

    /// 只改 margin（**不** commit —— 由调用方决定这次 commit 附不带 buffer）。
    ///
    /// layer-shell 的位置只能靠 margin，而 margin 要 `commit` 才生效；漏掉这一步，
    /// 宠物在"只移动不换帧"的档位（Idle 5 fps）会卡在原地。
    fn apply_margin(&mut self, surface_pos: Vec2) -> bool {
        let want = self.world_to_margin(surface_pos);
        if self.margin == want {
            return false;
        }
        self.margin = want;
        // anchor = TOP|LEFT ⇒ 只用 top/left
        self.layer.set_margin(want.1, 0, 0, want.0);
        true
    }

    /// 画一格并提交。返回是否真的发出了一次 commit。
    fn draw_cell(&mut self, row: u32, col: u32) -> bool {
        let (pw, ph, pm) = self.cell_geometry();
        let (bw, bh) = (pw + pm * 2, ph + pm * 2);
        let stride = (bw * 4) as usize;
        let off = (pm * stride as i32 + pm * 4) as usize;
        let rects = self.target_region(row, col);

        let created = self
            .pool
            .create_buffer(bw, bh, stride as i32, wl_shm::Format::Argb8888);
        let (buffer, canvas) = match created {
            Ok(v) => v,
            Err(e) => {
                eprintln!("create_buffer 失败: {e:?}");
                return false;
            }
        };
        // 这段里只能做不相交字段的可读访问（`self.pool` 正被可变借用）
        {
            canvas.fill(0);
            let tail = off.min(canvas.len());
            let take = &mut canvas[tail..];
            if let Some(atlas) = self.atlas.as_ref()
                && let Err(e) = atlas.blit(row, col, take, pw as u32, ph as u32, stride)
            {
                eprintln!("blit ({row},{col}) 失败: {e}");
                return false;
            }
        }

        let surface = self.layer.wl_surface().clone();
        if let Some(s) = self.buffer_scale() {
            surface.set_buffer_scale(s);
        }
        surface.damage_buffer(0, 0, bw, bh);
        self.arm_frame_wait();
        let qh = self.qh();
        surface.frame(&qh, surface.clone());
        if buffer.attach_to(&surface).is_err() {
            self.clear_frame_wait();
            return false;
        }
        self.set_input_region(&rects);
        self.layer.commit();
        true
    }

    /// 该帧该用的穿透矩形：拖拽中 = 整块 surface，否则 = 该格的精确轮廓。
    fn target_region(&self, row: u32, col: u32) -> Vec<CellRect> {
        if self.drag {
            if let Some(r) = self.regions.as_ref() {
                return r.full_surface();
            }
            return vec![CellRect::new(0, 0, self.logical.0, self.logical.1)];
        }
        self.region_rects(row, col)
    }

    /// 该格的穿透矩形；资产里没有这格 ⇒ 退化成整格（含 margin 之外）。
    fn region_rects(&self, row: u32, col: u32) -> Vec<CellRect> {
        if let Some(r) = self.regions.as_ref().and_then(|r| r.rects(row, col)) {
            return r.to_vec();
        }
        let m = SURFACE_MARGIN as i32;
        vec![CellRect::new(
            m,
            m,
            self.manifest.grid.w as i32,
            self.manifest.grid.h as i32,
        )]
    }

    /// (物理格宽, 物理格高, 物理边距)
    fn cell_geometry(&self) -> (i32, i32, i32) {
        (
            self.to_phys(self.manifest.grid.w as f32),
            self.to_phys(self.manifest.grid.h as f32),
            self.to_phys(SURFACE_MARGIN),
        )
    }

    /// 换 `wl_region`。旧的在"新 commit 之后"销毁：那时合成器已经复制了它的内容。
    fn set_input_region(&mut self, rects: &[CellRect]) {
        let qh = self.qh();
        let region = self.compositor.wl_compositor().create_region(&qh, ());
        for r in rects {
            if !r.is_empty() {
                region.add(r.x, r.y, r.w, r.h);
            }
        }
        self.layer.wl_surface().set_input_region(Some(&region));
        let full = rects.len() == 1 && rects[0].w >= self.logical.0 && rects[0].h >= self.logical.1;
        self.region_full = full;
        if let Some(old) = self.active_region.replace(region) {
            old.destroy();
        }
    }

    /// 纯色矩形路径（M4 第 3 步）：整格不透明品红 + margin 透明，区域 = 整格。
    fn test_rect_frame(&mut self) {
        if !self.configured || self.frame_pending {
            return;
        }
        let (pw, ph, pm) = self.cell_geometry();
        let (bw, bh) = (pw + pm * 2, ph + pm * 2);
        let stride = (bw * 4) as usize;
        let created = self
            .pool
            .create_buffer(bw, bh, stride as i32, wl_shm::Format::Argb8888);
        let (buffer, canvas) = match created {
            Ok(v) => v,
            Err(e) => {
                eprintln!("create_buffer 失败: {e:?}");
                self.counters.buffer_errors += 1;
                return;
            }
        };
        {
            // 预乘品红 (R=255,G=0,B=255,A=255) 在 BGRA 内存序下 = 小端 u32 0xFFFF00FF
            canvas.fill(0);
            for y in 0..ph {
                let base = ((y + pm) as usize) * stride + (pm as usize) * 4;
                for x in 0..pw {
                    let i = base + (x as usize) * 4;
                    canvas[i..i + 4].copy_from_slice(&0xFFFF_00FFu32.to_le_bytes());
                }
            }
        }
        let surface = self.layer.wl_surface().clone();
        if let Some(s) = self.buffer_scale() {
            surface.set_buffer_scale(s);
        }
        surface.damage_buffer(0, 0, bw, bh);
        self.arm_frame_wait();
        let qh = self.qh();
        surface.frame(&qh, surface.clone());
        if buffer.attach_to(&surface).is_err() {
            self.clear_frame_wait();
            self.counters.buffer_errors += 1;
            return;
        }
        // 区域用**逻辑**坐标（`wl_region` 与 buffer 像素差一个 scale）
        let m = SURFACE_MARGIN as i32;
        let rects = vec![CellRect::new(
            m,
            m,
            self.manifest.grid.w as i32,
            self.manifest.grid.h as i32,
        )];
        self.set_input_region(&rects);
        self.layer.commit();
        self.counters.commits += 1;
        self.force_draw = false;
        // 固定落点：舞台内 (200,200) 逻辑像素，方便肉眼确认
        if self.margin == (0, 0) {
            self.apply_margin(Vec2::new(
                self.output_origin().0 as f32 + 200.0,
                self.output_origin().1 as f32 + 200.0,
            ));
            self.layer.commit();
        }
    }

    fn qh(&self) -> QueueHandle<App> {
        self.qh.clone().expect("App 构造后 qh 一定有")
    }

    /// scale 变了：buffer 物理尺寸要重算，下一帧重画（池会自动扩容）。
    fn redraw_for_scale(&mut self) {
        self.drawn_cell = None;
        self.force_draw = true;
    }

    /// 退出前的门禁读数（RSS/CPU 直接读 `/proc`，比 btop 可脚本化）。
    fn report(&self) {
        // 生效值与来源在**退出摘要**里再打一遍：验收要按 300 s 那一轮读数判"我配的到底
        // 生效没有"，而启动那一行早就滚出屏幕了。
        println!("  {}", self.ui.summary());
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        let t = (self.counters.commits + self.counters.moves + self.counters.idle).max(1) as f64;
        println!(
            "overlay: {} tick / {:.1}s 墙钟；提交 {}（{:.1}%）、仅移动 {}（{:.1}%）、无事 {}（{:.1}%）、延后 {}、buffer 错误 {}、帧回调看门狗 {}",
            self.ticks,
            secs,
            self.counters.commits,
            100.0 * self.counters.commits as f64 / t,
            self.counters.moves,
            100.0 * self.counters.moves as f64 / t,
            self.counters.idle,
            100.0 * self.counters.idle as f64 / t,
            self.counters.deferred,
            self.counters.buffer_errors,
            self.counters.watchdogs,
        );
        // 判据④的硬核对项直接打成结论，别让人再手加一遍（历史上"三档相加 == tick 总数"是在
        // `延后` 恒为 0 的前提下才成立的，而那条前提没人写在读数旁边）。
        let five = self.counters.tick_accounted();
        println!(
            "  核对: 五档合计 {} vs tick {} ⇒ {}｜非 tick 补画: 帧回调补投 {} / configure {}",
            five,
            self.ticks,
            if five == self.ticks {
                "OK（互斥且齐）"
            } else {
                "不一致：有 tick 没归进任何一档，或某档重复计数"
            },
            self.counters.replay_commits,
            self.counters.configure_commits,
        );
        println!(
            "  surface 逻辑 {}x{} ⇒ buffer {}x{}（scale {:.2}×，viewport {}）",
            self.logical.0,
            self.logical.1,
            self.to_phys(self.logical.0 as f32),
            self.to_phys(self.logical.1 as f32),
            self.scale_f(),
            if self.viewport.is_some() {
                "有"
            } else {
                "无，退 set_buffer_scale"
            },
        );
        println!(
            "  菜单: {}（{} 提交 / {} 看门狗，静止时应当恒 0 增长）",
            if self.menu.is_some() {
                "开着"
            } else {
                "没开"
            },
            self.counters.menu_commits,
            self.counters.menu_watchdogs,
        );
        if let Some(sim) = self.sim.as_ref() {
            println!(
                "  内核: state={:?} 指纹={:016x}",
                sim.machine.state(),
                sim.fingerprint()
            );
        }
        // 字体这层的内存代价必须是**实测值**而不是"应该不多"：VmHWM 门禁（19 MB）的余量
        // 只有 ≈580 KB，而子集 + 字形缓存是本轮新加的两块（§7-R4）。缓存 arena 在启动时
        // 一次性 `resize`，所以这里读的数与峰值是同一个数，不存在"这次绿下次炸"。
        let (hits, made, uncached) = text::cache_stats();
        println!(
            "  字体: face={} 资产 {} KiB / 字形缓存 {} B（上限 {} B，命中 {hits}，栅格化 {made}，未缓存 {uncached}）",
            if text::face().is_some() {
                "真字体"
            } else {
                "降级 5×7 点阵"
            },
            text::FONT_BYTES.len() / 1024,
            text::cache_bytes(),
            text::CACHE_MAX_BYTES,
        );
        println!("  {}", proc_self_summary());
    }
}

/// `/proc/self/status` 的 VmHWM/VmRSS + `/proc/self/stat` 的 utime+stime。
pub fn proc_self_summary() -> String {
    let mut out = String::new();
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        for line in s.lines() {
            if line.starts_with("VmHWM:") || line.starts_with("VmRSS:") {
                out.push_str(line.trim());
                out.push_str("  ");
            }
        }
    }
    if let Ok(s) = std::fs::read_to_string("/proc/self/stat")
        && let Some((_, rest)) = s.rsplit_once(") ")
    {
        // comm 可能含空格，故从最后一个 ") " 之后重新数：f[0]=state(3)，utime=字段14⇒f[11]
        let f: Vec<&str> = rest.split_whitespace().collect();
        if let (Some(ut), Some(st)) = (f.get(11), f.get(12)) {
            let secs =
                (ut.parse::<f64>().unwrap_or(0.0) + st.parse::<f64>().unwrap_or(0.0)) / 100.0;
            out.push_str(&format!("CPU 时间 {secs:.2}s"));
        }
    }
    out
}

// ───────────────────────────── wayland 事件 ─────────────────────────────

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        new_factor: i32,
    ) {
        // 有 `wp_fractional_scale` 时以它的 `preferred_scale` 为准（整数是 1.5→2 的向上取整，会糊）
        if self.fractional.is_none() {
            let want = new_factor.max(1) as u32 * SCALE_UNIT;
            if self.scale_num != want {
                self.scale_num = want;
                self.redraw_for_scale();
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, s: &WlSurface, _t: u32) {
        // 按**对象身份**分流：宠物 / 菜单 / 别人的（光标 surface 也走这条路）。
        // 不分流的话，菜单的帧回调会把宠物的 `frame_pending` 解开（反之亦然），
        // 两块 surface 互相"替对方"放行 ⇒ 提交计数和延后补投全都读不出真相。
        if self.menu.as_ref().is_some_and(|m| &m.surface == s) {
            if let Some(mut m) = self.menu.take() {
                m.clear_wait();
                if m.force_draw {
                    let qh = self.qh();
                    let e = self.menu_error_line();
                    if m.draw(&self.compositor, &qh, &e) {
                        self.counters.menu_commits += 1;
                    }
                }
                self.menu = Some(m);
            }
            return;
        }
        if s != self.layer.wl_surface() {
            return;
        }
        self.clear_frame_wait();
        if !self.force_draw {
            return;
        }
        // 补投由帧回调驱动、不对应任何 tick ⇒ 这一趟的 commit 归 `replay_commits`，
        // 否则"五档 == tick"永远差一个补投数（2026-10-28 轮 4 实测超 3 = 补投 2 + 双记 1）。
        self.source = Source::Replay;
        if self.opts.test_rect {
            self.test_rect_frame();
        } else if let Some(i) = self.last_intent.clone() {
            self.present(&i);
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        s: &WlSurface,
        o: &wl_output::WlOutput,
    ) {
        // 只认宠物那块：光标 surface 和菜单也会报 enter，拿它们写 `on_output`
        // 等于"谁最后进屏谁定义舞台"，热插拔时会飘。
        if s != self.layer.wl_surface() {
            return;
        }
        // layer surface 建的时候 output 传的是 None ⇒ **由合成器决定它落在哪台**。
        // margin 是相对那台的锚边算的，所以必须用它上报的这台，不能猜第一个。
        self.on_output = Some(o.clone());
        self.sync_world();
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        s: &WlSurface,
        o: &wl_output::WlOutput,
    ) {
        if s == self.layer.wl_surface() && self.on_output.as_ref() == Some(o) {
            self.on_output = None;
        }
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: wl_output::WlOutput) {
        self.sync_world();
    }

    fn update_output(&mut self, _c: &Connection, _q: &QueueHandle<Self>, _o: wl_output::WlOutput) {
        self.sync_world();
    }

    fn output_destroyed(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        _o: wl_output::WlOutput,
    ) {
        self.sync_world();
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _c: &Connection, _q: &QueueHandle<Self>, l: &LayerSurface) {
        // **只有宠物那块被关才停整个环**（R3）。合成器收回菜单 surface（我们主动 destroy、
        // 或者它自己把这块回收了）时，若照旧无条件 `exit = true`，"关个菜单"就会把宠物
        // 一起带走 —— 而这条在 `--seconds 0` 的场景里表现为"宠物自己消失了"。
        if self.menu.as_ref().is_some_and(|m| &m.layer == l) {
            self.menu = None;
            eprintln!("menu: 合成器收回菜单 surface（宠物继续跑）");
            return;
        }
        if &self.layer == l {
            self.exit = true;
        }
    }

    fn configure(
        &mut self,
        _c: &Connection,
        _q: &QueueHandle<Self>,
        l: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if self.menu.as_ref().is_some_and(|m| &m.layer == l) {
            if let Some(mut m) = self.menu.take() {
                m.configured = true;
                m.force_draw = true;
                m.clear_wait();
                let qh = self.qh();
                let e = self.menu_error_line();
                if m.draw(&self.compositor, &qh, &e) {
                    self.counters.menu_commits += 1;
                }
                self.menu = Some(m);
            }
            return;
        }
        if l != &self.layer {
            return;
        }
        if !self.configured {
            // scale 此刻通常还没到（`preferred_scale` 在第一次 dispatch 之后），所以不打这里
            eprintln!(
                "configure: 合成器建议 {}x{}（我们坚持逻辑 {}x{}）",
                configure.new_size.0, configure.new_size.1, self.logical.0, self.logical.1,
            );
        }
        self.configured = true;
        self.force_draw = true;
        self.clear_frame_wait();
        // 合成器又发了一次 `configure`（重配置、改 anchor、reserved 变动都可能）⇒ 主动补画一帧。
        // 这**不是 tick**：不标来源就会多记一笔 `idle`/`moves`（2026-10-28 轮 3 实测 5 次 ⇒ 五档超 5）。
        self.source = Source::Configure;
        if self.opts.test_rect {
            self.test_rect_frame();
        } else if let Some(i) = self.last_intent.clone() {
            self.present(&i);
        }
        self.source = Source::Replay;
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer || self.pointer.is_some() {
            return;
        }
        if self.opts.no_cursor {
            // 不载主题光标 ⇒ 鼠标进到宠物上方可能隐形；只用于验证穿透
            self.raw_pointer = self.seat_state.get_pointer(qh, &seat).ok();
            eprintln!("--no-cursor：宠物上方的光标可能隐形");
            return;
        }
        let cursor_surface = self.compositor.create_surface(qh);
        match self.seat_state.get_pointer_with_theme(
            qh,
            &seat,
            self.shm.wl_shm(),
            cursor_surface,
            ThemeSpec::System,
        ) {
            Ok(p) => self.pointer = Some(p),
            Err(e) => {
                eprintln!("主题光标加载失败（{e:?}），退回到隐形光标");
                self.opts.no_cursor = true;
                self.raw_pointer = self.seat_state.get_pointer(qh, &seat).ok();
            }
        }
    }

    fn remove_capability(
        &mut self,
        conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.pointer = None;
            self.raw_pointer = None;
            // 指针能力没了（热插拔 / 别的客户端 grab / 锁屏）同样等不到 Release ⇒ 结束拖拽
            if self.drag {
                self.on_release(conn);
            }
            // **菜单连带丢掉**：它整块 input region 都吃点击，而没有指针就再也点不到
            // CLOSE —— 留着一块永久吞点击的不透明方块，正是 M4 那条 P0 的同一个形状。
            self.close_menu();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[SeatPointerEvent],
    ) {
        for e in events {
            // 三路分流：菜单 / 宠物 / 别的 surface（光标那块的事件也在这里被丢掉）
            let on_menu = self.menu.as_ref().is_some_and(|m| m.surface == e.surface);
            if !on_menu && e.surface != *self.layer.wl_surface() {
                continue;
            }
            if on_menu {
                let local = (e.position.0 as f32, e.position.1 as f32);
                match e.kind {
                    PointerEventKind::Enter { .. } => self.set_cursor(conn),
                    PointerEventKind::Press { button, .. } => {
                        if menu::menu_press_activates(button) {
                            self.on_menu_press(local);
                        }
                    }
                    // 菜单被点完可能当场关闭（CLOSE 项）⇒ Leave/Release 拿不到那块 surface，
                    // 这里只用来收尾光标，不参与状态机。
                    PointerEventKind::Motion { .. } | PointerEventKind::Leave { .. } => {}
                    PointerEventKind::Release { .. } | PointerEventKind::Axis { .. } => {}
                }
                continue;
            }
            let local = Vec2::new(e.position.0 as f32, e.position.1 as f32);
            let origin = self
                .last_intent
                .as_ref()
                .map(|i| i.surface.pos)
                .unwrap_or(Vec2::ZERO);
            self.pointer_world = Vec2::new(origin.x + local.x, origin.y + local.y);
            match e.kind {
                PointerEventKind::Enter { .. } => self.set_cursor(conn),
                PointerEventKind::Motion { .. } => {
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
                PointerEventKind::Press { button, .. } => {
                    // 左键 = 拖拽/双击（原逻辑），右键 = 开/关菜单，中键 = 忽略。
                    // 右键**不进** `last_press` 计时，否则"右键两下"会被认成双击宠物。
                    match menu::pet_press_disposition(button) {
                        menu::PetPressAction::StartDragOrDoubleTap => self.on_press(conn),
                        menu::PetPressAction::ToggleMenu => self.toggle_menu(),
                        menu::PetPressAction::Ignore => {}
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == menu::BTN_LEFT {
                        self.on_release(conn);
                    }
                }
                PointerEventKind::Axis { .. } => {}
                // **离开宠物 = 拖拽的失败路径**（P0）：合成器可能因为锁屏、VT 切换、
                // 别的客户端 grab 而取消指针，此时 `Release` 永远不会来。不在这儿收尾，
                // `drag` 就永久为真 ⇒ 每帧都把 input region 撑成整块 128×128 实心矩形，
                // 一直吞掉那块桌面的点击 —— 正是"绝不抢输入"的硬红线。
                PointerEventKind::Leave { .. } => {
                    if self.drag {
                        eprintln!("指针离开 ⇒ 提前结束拖拽（没等到 Release）");
                        self.on_release(conn);
                    }
                }
            }
        }
    }
}

impl App {
    fn set_cursor(&self, conn: &Connection) {
        if let Some(p) = self.pointer.as_ref() {
            let icon = if self.drag {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Default
            };
            if let Err(e) = p.set_cursor(conn, icon) {
                // 不致命：只是光标隐形
                eprintln!("set_cursor 失败: {e:?}");
            }
        }
    }

    fn on_press(&mut self, conn: &Connection) {
        let now = Instant::now();
        let dbl = match self.last_press {
            Some((t, p)) => {
                now - t <= DOUBLE_TAP_GAP
                    && ((p.x - self.pointer_world.x).powi(2) + (p.y - self.pointer_world.y).powi(2))
                        .sqrt()
                        < DOUBLE_TAP_RADIUS
            }
            None => false,
        };
        self.last_press = Some((now, self.pointer_world));
        let feet = self
            .sim
            .as_ref()
            .map(|s| s.body.pos)
            .unwrap_or(self.pointer_world);
        self.grab_offset = Vec2::new(self.pointer_world.x - feet.x, self.pointer_world.y - feet.y);
        self.drag = true;
        if let Some(sim) = self.sim.as_mut() {
            if dbl {
                sim.send(Command::DoubleTap);
            }
            sim.send(Command::DragStart);
        }
        self.set_cursor(conn);
        // 拖拽期把区域撑满整块 surface：指针滑出剪影会丢 focus，Release 就永远不来
        if !self.region_full {
            let rects = match self.regions.as_ref() {
                Some(r) => r.full_surface(),
                None => vec![CellRect::new(0, 0, self.logical.0, self.logical.1)],
            };
            self.set_input_region(&rects);
            self.layer.commit();
        }
    }

    fn on_release(&mut self, conn: &Connection) {
        if !self.drag {
            return;
        }
        self.drag = false;
        if let Some(sim) = self.sim.as_mut() {
            sim.send(Command::DragEnd);
        }
        self.set_cursor(conn);
        // 区域回到当前帧的精确轮廓（下一格提交也会更新，这里不等）
        if self.region_full {
            let cell = self.drawn_cell.unwrap_or((0, 0));
            let rects = self.region_rects(cell.0, cell.1);
            self.set_input_region(&rects);
            self.layer.commit();
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, (), App> for App {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: <WpFractionalScaleV1 as Proxy>::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && scale > 0
            && state.scale_num != scale
        {
            state.scale_num = scale;
            eprintln!(
                "scale: {:.2}×（wp_fractional_scale）⇒ buffer {}x{}",
                state.scale_f(),
                state.to_phys(state.logical.0 as f32),
                state.to_phys(state.logical.1 as f32)
            );
            state.redraw_for_scale();
        }
    }
}

delegate_noop!(App: WpViewporter);
delegate_noop!(App: WpViewport);
delegate_noop!(App: WpFractionalScaleManagerV1);
delegate_noop!(App: WlRegion);

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);

// ───────────────────────────── 抽取层测试 ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // 只在测试里用：生产路径的文案都经 `card.rs`/`menu.rs` 取，`app.rs` 顶部不引 `Key`
    use crate::l10n::Key;

    /// 抽取层的入口是鼠标，而**我绝不注入点击**（三项人工门禁只有用户本人能判）。
    /// 所以这一层的验收必须在 headless 里跑完：`Gacha` 不含任何 wayland 类型，
    /// 下面测的就是"菜单按下去之后真正执行的那段代码"，不是它的替身。
    #[test]
    fn memory_store_pulls_and_bakes_the_locale_into_the_report() {
        let mut g = Gacha::connect(None).expect("内存库该建得起来");
        assert!(
            g.store.path().is_none(),
            "内存库必须连路径都没有 ⇒ 一条文件都不会产生"
        );
        let before = g.balance();
        let (lines, best, balance) = g.pull(10, true, Lang::Zh, 1.5).expect("十连该成功");
        assert!(
            lines[0].starts_with("10 抽") && lines[0].contains(best.as_str()),
            "默认档的标题行要同时说出抽了几次和最高档：{lines:?}"
        );
        // 结果行里的名字必须是**目录里的中文名**，不是 `item_id`（本轮改的就是这一处）。
        let named = lines
            .iter()
            .skip(1)
            .filter(|l| l.starts_with(['N', 'R', 'S']))
            .filter(|l| !l.is_ascii())
            .count();
        assert!(
            named > 0,
            "结果行里没有中文物品名 ⇒ catalog 查找没接上，卡片还在打 slug：{lines:?}"
        );
        assert!(
            balance >= before,
            "抽取只加碎片（v1 免费软通货，D2），余额倒退说明扣了不该扣的钱"
        );
        // 演示标**不在这些行里**（它由 `card.rs` 的标题行徽标承担）：这里钉住的是
        // "内存库这条路确实一个字节都没写盘"，像素侧的证据在 `dryrun_stamp_changes_the_header_pixels`。
        assert!(
            !lines.iter().any(|l| l.contains("DRYRUN")),
            "演示标跑进结果行了 ⇒ 卡片上会有两处，而只有一处是签收过的"
        );
    }

    #[test]
    fn every_locale_pull_line_is_sayable_with_the_bundled_font() {
        // 抽一次真东西，逐 locale 过一遍：这条抓的是"某个 locale 的模板里有一个没进子集的字"
        // —— `ui_strings` 测的是**函数能产出的**串，而这里测的是**真抽取产出的**串（物品名
        // 来自目录，不在 `ui_strings` 里）。缺字在画布上只会变成一个 `?`，只有这里能红。
        let mut g = Gacha::connect(None).expect("内存库该建得起来");
        for l in Lang::ALL {
            let (lines, _best, _b) = g.pull(10, true, l, 1.5).expect("十连该成功");
            for line in &lines {
                if let Some(c) = crate::text::missing(line) {
                    panic!("{l:?} 的结果行「{line}」里的 {c:?} 不在字体子集里");
                }
            }
        }
    }

    #[test]
    fn real_archive_is_written_and_replays_the_chain() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("opet.db");
        let mut g = Gacha::connect(path.to_str()).expect("--db 指向可写路径就该建库");
        // 这次传 `en`：既验"落库这条路"，也验 `lang` 参数真的走到底（默认档在上一条测过了）。
        let (lines, _best, balance) = g.pull(10, false, Lang::En, 1.5).expect("十连该成功");
        assert!(
            lines[0].starts_with("10 pulled"),
            "英文标题行没按 locale 走（`lang` 参数没接上）：{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("DRYRUN") || l.contains(Lang::En.t(Key::DryRun))),
            "真落库不许打演示标（否则用户不知道哪次真的写进了存档）"
        );
        // 换一条连接重开：只有磁盘上真有东西，才认得这 10 条、链才验得过
        let reopened = Store::open(&path).expect("重开存档");
        let catalog = Catalog::default_v1().unwrap();
        let rep = reopened
            .verify(
                &catalog,
                petstore::DEFAULT_PROFILE,
                petstore::DEFAULT_BANNER,
            )
            .expect("账本该自洽");
        assert_eq!(rep.len, 10, "落库条数");
        assert_eq!(
            balance,
            i64::try_from(reopened.shards(Currency::Shards).unwrap()).unwrap(),
            "菜单显示的余额必须就是库里那个数"
        );
    }

    #[test]
    fn a_relative_path_is_refused_before_anything_is_written() {
        // 相对路径取决于 cwd，而宠物可能被 systemd user service 起 ⇒ 只能拒绝，不能猜。
        let Err(err) = Gacha::connect(Some("opet_m3b_relative.db")) else {
            panic!("相对路径该被拒绝");
        };
        assert!(err.contains("绝对路径"), "错误要读得懂: {err}");
        assert!(
            !std::path::Path::new("opet_m3b_relative.db").exists(),
            "拒绝就该一个字都没写"
        );
    }

    #[test]
    fn an_unopenable_store_only_produces_a_line_of_text() {
        // 父路径是个普通文件 ⇒ `create_dir_all` 必炸。这条测的是**失败的形状**：
        // 返回 Err（调用方转成菜单错误行），而不是 panic（硬约束 3）。
        let dir = tempfile::tempdir().expect("临时目录");
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").expect("占位文件");
        let bad = blocker.join("opet.db");
        let Err(err) = Gacha::connect(bad.to_str()) else {
            panic!("该建不起来");
        };
        assert!(err.starts_with("打不开存档"), "错误要带上是哪个路径: {err}");
    }

    /// 判据④"五档相加 == tick 总数"的结构前提：一档只动一个字段。
    /// 真机上这条第一次露馅是 2026-10-28 轮 4（`延后 2` ⇒ 五档比 tick 多 3 = 补投 2 + 双记 1），
    /// 而那条路径撞不撞得上全看合成器时序 ⇒ 没有 headless 覆盖就随时可能悄悄退回去。
    #[test]
    fn a_tick_bumps_exactly_one_counter() {
        for tick in ALL_TICKS {
            let mut c = Counters::default();
            c.bump(tick);
            let touched = [
                c.commits,
                c.moves,
                c.idle,
                c.deferred,
                c.buffer_errors,
                c.replay_commits,
                c.configure_commits,
            ];
            assert_eq!(
                touched.iter().filter(|&&n| n != 0).count(),
                1,
                "{tick:?} 动了不止一个字段: {touched:?}"
            );
            assert_eq!(
                c.tick_accounted() == 1,
                matches!(
                    tick,
                    Tick::Buffer | Tick::Move | Tick::Idle | Tick::Deferred | Tick::Fault
                ),
                "{tick:?} 算不算一个 tick 与五档加总不一致: {touched:?}"
            );
        }
    }

    /// 归属表逐组合钉死：非 tick 来源只有在真发 buffer 时才占档，且占的是**另计**那一档。
    /// 这条断了就是判据④读不出真相（2026-10-28 一天内断过两次：补投进 `commits`、重配置进 `idle`）。
    #[test]
    fn attribution_is_a_table_over_source_and_outcome() {
        for source in [Source::Tick, Source::Replay, Source::Configure] {
            for outcome in ALL_OUTCOMES {
                let hit = attribute_of(source, outcome);
                let mut c = Counters::default();
                if let Some(hit) = hit {
                    c.bump(hit);
                }
                match source {
                    Source::Tick => {
                        assert_eq!(hit, Some(outcome), "{outcome:?} 必须原样占档");
                        assert_eq!(c.tick_accounted(), 1, "{outcome:?} 没占上一档");
                    }
                    non_tick => {
                        let want = match non_tick {
                            Source::Configure => Some(Tick::Configure),
                            _ => Some(Tick::Replay),
                        };
                        assert_eq!(
                            hit,
                            if outcome == Tick::Buffer { want } else { None },
                            "{outcome:?} 在 {non_tick:?} 下判错了另计档"
                        );
                        // 另计档一律不碰五档
                        assert_eq!(c.tick_accounted(), 0, "{outcome:?} 抢了一个 tick 的归属");
                    }
                }
            }
        }
    }

    /// 回归钉：延后那一 tick 即使同时挪了 margin，协议上照发状态 commit，但**不双记**。
    #[test]
    fn a_deferred_tick_flushes_the_margin_without_counting_a_move() {
        assert_eq!(classify_tick(true, true, true), Tick::Deferred);
        assert!(
            Tick::Deferred.flushes_margin(),
            "延后时 margin 动了必须把状态 commit 冲出去，否则合成器停在旧位置"
        );
        let mut c = Counters::default();
        c.bump(Tick::Deferred);
        assert_eq!((c.moves, c.idle, c.deferred), (0, 0, 1));

        // 出错同理（这条分支也会发状态 commit）
        assert!(Tick::Fault.flushes_margin());
        let mut c = Counters::default();
        c.bump(Tick::Fault);
        assert_eq!((c.moves, c.idle, c.buffer_errors), (0, 0, 1));
    }

    /// 归档全覆盖：8 种输入组合各自只落一档，且 `Move`/`Idle` 不可能吃掉一个"该换格"的 tick。
    #[test]
    fn classification_partitions_the_inputs() {
        for new_cell in [false, true] {
            for moved in [false, true] {
                for frame_pending in [false, true] {
                    let tick = classify_tick(new_cell, moved, frame_pending);
                    // `Fault`/`Replay` 要等真出图的结果，不归这里判
                    assert!(
                        matches!(
                            tick,
                            Tick::Buffer | Tick::Move | Tick::Idle | Tick::Deferred
                        ),
                        "{new_cell}/{moved}/{frame_pending} 判出了 {tick:?}"
                    );
                    assert_eq!(
                        matches!(tick, Tick::Move | Tick::Idle),
                        !new_cell,
                        "{new_cell}/{moved}/{frame_pending} ⇒ {tick:?}：不换格才会落进这两档"
                    );
                    assert_eq!(tick == Tick::Move, !new_cell && moved);
                    assert_eq!(tick == Tick::Idle, !new_cell && !moved);
                    assert_eq!(tick == Tick::Deferred, new_cell && frame_pending);
                    assert_eq!(tick == Tick::Buffer, new_cell && !frame_pending);
                }
            }
        }
    }
}
