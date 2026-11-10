//! 运行环：60 Hz 定时的内核 tick + Wayland 事件 + 三级提交分派（设计书 §3.7、§4）。
//!
//! 这里是**客户端唯一使用墙钟的地方**（`petcore` 内禁止，硬约束 8）。定时用 calloop 的
//! `Timer`，Wayland socket 用 `calloop-wayland-source`，两者共用一个事件循环。
//!
//! 三级分派（省电契约，`sim.rs:135-137` 的语义落到协议上）：
//! 1. 图集格变了 ⇒ 新 buffer + `damage_buffer` + `set_input_region` + `commit`；
//! 2. 只有位置变了 ⇒ 只改 layer margin + `commit`（不附新 buffer）；
//! 3. 都没变 ⇒ 什么都不发。
//!
//! 装备了配件（换装页"穿上"，D16 路线 B）时，第 1 档画的那一格是**本体先覆盖、配件后
//! source-over**（`blit_cell`），而 `set_input_region` 用的是装备那一刻算好一次的**并集**
//! （[`Regions::merged_with`]）。⇒ 分档判据与每档提交次数一个字都没变，变的只是那一格的内容与
//! 那块 region 的矩形集合。配件的**选择**没有命令行入口，只有屏上那三行（用户原话
//! "配件不是应该由菜单选择吗，这也不合理"）；库里有什么在启动时扫出来并打进 stderr。

use std::sync::Arc;
use std::time::{Duration, Instant};

use petcore::assets::{AccessoryManifest, Grid, Manifest};
use petcore::behavior::AssetKey;
use petcore::behavior::config::{Action, BehaviorConfig};
use petcore::behavior::machine::Command;
use petcore::gacha::{
    Catalog, Currency, DEFAULT_RATES_JSON, GachaParams, ItemKind, Owned, Rarity, RatesFile, Slot,
};
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
use crate::l10n::{self, Key, Lang};
use crate::menu::{self, CARD_H, CARD_W, ItemRect, MenuAnim, RectI};
use crate::panel::{self, CollectionTab, PanelKind};
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
    /// `--allow-unowned-accessories`：**开发态豁免** —— 磁盘上有而库存里没有的那几件，
    /// 照样进换装页、照样能穿。默认关（用户 2026-11-05 第 3 问 = 玩家不能穿未拥有）。
    ///
    /// ⚠️ 它**只在给了 `--db` 时有意义**：不给 `--db` = 内存库 ⇒ `owned()` 恒空 = 演示态，
    /// 那一档由 [`Options::db`] 那条既有的 DRYRUN 决定负责放行与打标（屏上右端那个「演示」），
    /// 不是这条开关的作用域。启动时把这句话打进 stderr，**别让它变成静默空转的假开关**。
    pub allow_unowned_accessories: bool,
    /// `--allow-unowned-pets`：**开发态豁免** —— 库存里没有的那几只本体，收藏册里照样点得动。
    ///
    /// ⚠️ 故意**不**复用 [`Options::allow_unowned_accessories`]：那条开关的屏上记号是帽子页标题
    /// 那个 `*`，而换装页的列表里从头就没有宠物 ⇒ 一条开关自称管两类就会出现"标打了而没放行"。
    /// 本体侧因此**没有**任何屏上记号（拍板④），判据只在 [`App::pet_exempt`] 一处；与配件那条
    /// 一样，不给 `--db` 时不生效（演示态本来就放行），那一档由启动时一行 stderr 说清。
    pub allow_unowned_pets: bool,
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
            allow_unowned_accessories: false,
            allow_unowned_pets: false,
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
    /// 面板（第三块 surface，收藏册 / 设置）的提交数。同 `menu_commits`：**不并进上面三档**。
    ///
    /// ⚠️ 这条字段的存在理由与 `menu_commits` 不是一句"再来一块面就再来一个数"：面板是
    /// 「**开着一动不动**」的那类面 ⇒ 它唯一的省电判据是**静止恒 1 提交**（`69fd158` 那一族），
    /// 而这个数必须和菜单的分开读，否则"菜单翻页 +1"与"面板静止不涨"混成一个数，
    /// 两边都读不出来。
    pub panel_commits: u64,
    pub panel_watchdogs: u64,
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

    /// 五档加总：应当逐轮等于 `App.ticks`（`Replay` 与**菜单、面板两块面的计数都在之外**）。
    ///
    /// 这一句从"两块面"变成"三块面"不是补字：面板的提交同样**不走 [`Self::bump`]**（它由
    /// `update_panel` 记进 `panel_commits`）。并进来这条恒等式当场就断 —— 历史上"五档比
    /// tick 多 5"就是 `configure` 那一格被并进去导致的。
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

/// 配件库里的一件**候选**（= 目录里那份 `accessory.json` 加上它同目录的 `polygon.json`）。
///
/// 这里刻意**没有图集像素**，只有元数据与轮廓：内存档 M1 说"配件那份 sheet 不在常驻预算里"，
/// 而启动时把库里每一件都解码就是把 M1 当场变成"全部常驻"。⇒ 一次启动读进内存的只有
/// 几 KB 的清单与轮廓表，像素归 [`Equipped`]（穿了才存在，脱了就没）。
///
/// `merged` 是**启动时就算好**的并集轮廓，不是穿上那一刻才算：四道门禁里除解码之外的一切
/// （网格/行表/fps/loop 一致、每格有轮廓、轮廓几何能并）都是纯元数据比对，放在启动做
/// ⇒ [`App::equip`] 的唯一失败模式只剩"sheet 解不开"。把一件坏配件的判定推迟到用户
/// 点下那一行，等于让屏上一个能点而没有反馈的控件（本项目对假按钮的一贯立场）。
#[derive(Debug)]
struct Candidate {
    /// **库存那一侧的身份** = 库目录名。P2 的 `owned()` 过滤、P1 的 INV 门禁、`inventory.item_id`
    /// 认的都是这一个（`AssetDir` 的 `id` 也是目录名，`source.rs`）⇒ 三条路必须同一个键。
    id: String,
    /// 清单里写的 slug（`accessory.json` 的 `slug`，也是 `fits` 比的那个名字）。
    ///
    /// ⚠️ 它与 [`Self::id`] **可以不相等**（生产者 `pack.py --accessory` 让两者相等，手工塞进库的
    /// 未必），而不相等就是"屏上这件叫 A、库存记的是 B"。 [`discover_accessories`] 为此专门打一行。
    slug: String,
    /// 三件套所在目录（`sheet.webp` 与 `accessory.json` 同目录 = 生产者 `pack.py --accessory` 的约定）。
    dir: std::path::PathBuf,
    manifest: AccessoryManifest,
    /// 本体轮廓 ∪ 这件配件的轮廓 ⇒ 穿上 = 把 [`App::regions`] 换成它（一次性的事实，不重算）。
    merged: Regions,
}

/// 豁免那一句的**纯**版本 = [`App::dress_exempt`] 的函数体。
///
/// 单拎出来只为一件事：`--allow-unowned-accessories` 而**没有** `--db` 那一格（演示态）是本轮
/// 用户拍板专门区分过的（"照 DRYRUN 放行 + 打「演示」标"⇒ 放行但**不**点帽子页那个 `*`），
/// 而 `App` 要连 compositor 才构造得起来 ⇒ 留在方法里这条判定就没有任何自动化读数，
/// 只能靠"起一次进程看有没有星号"。拆成自由函数 ⇒ 四格真值表逐格钉住。
fn dress_exempt_for(allow_unowned: bool, dry_run: bool) -> bool {
    allow_unowned && !dry_run
}

/// [`App::dress_view`] 的结果 = 库存那道跑完之后，换装页手里那张表。
///
/// 为什么不是一个 `Vec<Candidate>` 而是**下标**：[`App::picked`] 存的就是 `candidates` 的下标，
/// 而"选中哪一件"与"这一件许不许穿"是两件事 —— 复制一份过滤后的表就会出现第二个"当前选中的
/// 是哪件"（本项目反复出现的那一族）。留下标 ⇒ `picked` 仍是唯一下标，`visible` 只是它能不能落
/// 在这几格上。
struct DressView {
    /// 通过库存那道判定的 [`App::candidates`] 下标，**升序**（`picked` 的循环序 = 目录序）。
    visible: Vec<usize>,
    /// 因为"库存里没有这一件"而出局的数量 ⇒ 只进 stderr 那一句，不上屏（用户内容不进子集字体）。
    withheld: usize,
}

impl DressView {
    /// 库存那道的**纯**判定 = "这些件叫什么" + "库存说拥有哪些" ⇒ 可见下标与出局数。
    ///
    /// 拆这一层的唯一理由 = 让这条判据能被自动化打到红。外层 [`App::dress_view`] 要连库、要扫盘，
    /// 整条只有真机跑得起来 ⇒ "未拥有的件不出现"就只能靠人眼看屏幕，而本项目对门禁的一贯要求是
    /// "能证明它会红"（`assert_no_paid_randomness`、`--inv` 那两轮红读数同族）。纯函数拿走三档
    /// 输入 ⇒ 三档各有一条独立读数，真机那一轮只剩"接线对不对"，不再承担"逻辑对不对"。
    ///
    /// - `allow_all` = 这一轮**根本没有**库存那道（开发态豁免 ∪ 演示态；两者屏上记号不同，
    ///   但可见集是同一个，所以在这里合成一个布尔）。
    /// - `owned = None` = 那道**没读过**（连不上库 / 读失败）⇒ 失败关闭，一件都不给。
    ///   这一档必须与 `Some(空集)` 分开：后者是"读到了，答案是都没有"，前者的"都没有"没有任何
    ///   事实支撑过 —— 拿它当后者就是"没验过也当验过"（§9.2 第 20 条要杀的静默形状）。
    ///
    /// `ids` 传的是**目录名**（[`Candidate::id`]）而不是清单里的 `slug`：库存那一侧的键从头到尾
    /// 是目录名，而 `slug` 是打包者填的字符串，两者可以不相等（启动时那行 ⚠️ 报的就是这个）。
    fn select(allow_all: bool, owned: Option<&Owned>, ids: &[String]) -> Self {
        let visible: Vec<usize> = match (allow_all, owned) {
            // 这一轮没有库存那道 ⇒ 全可见（`ids` 只贡献长度，不参与判定）
            (true, _) => (0..ids.len()).collect(),
            // 那道没读过 ⇒ 全不可见
            (false, None) => Vec::new(),
            // 读过了 ⇒ 逐件问"库存里有没有这个名字"
            (false, Some(owned)) => ids
                .iter()
                .enumerate()
                .filter(|(_, id)| owned.contains(*id))
                .map(|(i, _)| i)
                .collect(),
        };
        let withheld = ids.len() - visible.len();
        Self { visible, withheld }
    }
}

/// 穿着这件事**落不落库**的唯一判据（P3）。两个动作共用一条理由：屏上穿的与库里记着的
/// 必须是同一件事，否则重启回到另一顶帽子，而"重启后还穿着"（J3）就成了一句注释。
///
/// `enum` 而不是 `bool`：这一处有三个**互斥**的结论，而两个布尔量能表示 4 种、其中 2 种
/// 是没人能解释的（本项目"能表示就迟早会被表示错"那一族）。
enum WornWrite {
    /// 真写进库了（`--db` 且连得上）。
    Written,
    /// **演示态一个字都不写**，也不报错 —— 但调用方必须打一行，否则用户会以为记下了。
    DemoSkipped,
    /// 给了 `--db` 而这一轮连不上库 ⇒ 没地方可写。错误只是一行字（硬约束 3）。
    NoStore,
}

/// 穿上 = 把"这一格现在穿这件"写进 `equipped`。**自由函数而不是 `App` 的方法**：
/// `App` 要连 compositor 才构造得起来，而"演示态不落库"这一句（FLOW §9 P3 明写要在测试里）
/// 只需要一个 `Option<Gacha>` ⇒ 拆出来它才打得红。
///
/// `dry_run` 排在最前且**不调** `ensure_gacha`：演示态连"顺手把内存库那一行写上"都不该做，
/// 因为那行永远不可能在重启后还在，而屏上没有这件事。
fn write_equip(
    gacha: &mut Option<Gacha>,
    dry_run: bool,
    slot: &str,
    item_id: &str,
) -> Result<WornWrite, String> {
    if dry_run {
        return Ok(WornWrite::DemoSkipped);
    }
    let Some(g) = gacha.as_mut() else {
        return Ok(WornWrite::NoStore);
    };
    g.store
        .equip(petstore::DEFAULT_PROFILE, slot, item_id)
        .map_err(|e| format!("equipped 写入失败: {e}"))?;
    Ok(WornWrite::Written)
}

/// 脱下 = 删掉那一格。与 [`write_equip`] 同一条演示态判据，同拆法。
fn clear_equip(gacha: &mut Option<Gacha>, dry_run: bool, slot: &str) -> Result<WornWrite, String> {
    if dry_run {
        return Ok(WornWrite::DemoSkipped);
    }
    let Some(g) = gacha.as_mut() else {
        return Ok(WornWrite::NoStore);
    };
    g.store
        .unequip(petstore::DEFAULT_PROFILE, slot)
        .map_err(|e| format!("equipped 删除失败: {e}"))?;
    Ok(WornWrite::Written)
}

/// `equipped.slot` 里代表**本体**那一行的取值（P5 = 拍板③ 的那个接口槽）。
///
/// 为什么是一个裸字符串而不是 `Slot` 枚举的成员：`Slot` 是**配件的挂点**（`head`/`face`/…六值，
/// `accessory.json` 的 `slot` 字段就归它），而本体不挂在任何挂点上 —— 把 `body` 塞进那个枚举会让
/// "一件配件的槽位是 body"这种组合变得**可以表示**，而它是没有意义的（`schema.sql:65` 那个
/// `slot` 列**故意没有 CHECK**，正是为这件事留的接口槽；petstore 的
/// `the_slot_column_accepts_body_by_design` 钉着"写得进去"）。
///
/// ⚠️ 全仓库只有 [`App::persist_body`] 写它、只有启动那一道（[`db_body_row`]）读它。
const BODY_SLOT: &str = "body";

/// 启动那一步读库里的 `body` 行（拍板③ 唯一的读数）。
///
/// **只在 `--db` 给了的时候**开库，读完这个 `Store` 就随函数返回一起 drop ⇒ 无 `--db` 那一轮
/// 一个文件字节都不碰（与"演示态一律放行、也不记本体这一笔"同一条口径）。
///
/// 代价明写：带 `--db` 的这一轮库会被开**两次**（这里一次只读、[`App::ensure_gacha`] 一次）。
/// 选它而不是选"半个初始化的 `App`"：`App` 的构造点排在 surface 之后，而起点本体必须排在
/// surface **之前**（`logical` / `set_size` / `set_destination` / `SlotPool` 都从清单的格子算），
/// 让本体去等 surface 就得把清单字段换成 `Option` 并给每一处补一份"还没有清单"的形状 ——
/// 那比一次 sqlite 打开大得多。**以后若有第三个读取者，先问它能不能并入 `ensure_gacha`。**
///
/// 三条出口合成一个 `Option`：**没有那一行**与**读失败**都是 `None`（= 老库、今天的行为逐字节
/// 不变），而读失败**必须**先点名一行 ⇒ 静默退化成兜底就是假门禁（`--cell` 假开关那一族）。
///
/// ⚠️ 它读的是 [`Store::equipped_slot`] 那一条 SQL（`WHERE profile_id = ?1 AND slot = ?2`），
/// 而 [`split_body_row`] 读的是 [`Store::equipped`] 的整表 ⇒ 同一张表两个读法。它们对
/// "多于一条 `body` 行"给同一个答案（两条都按 `ORDER BY slot, item_id` 取首条），所以这不是
/// 两个事实源；合成一条读法的代价是"启动多开一次库"或"恢复侧先跑起来才能拿到本体"那条循环。
fn db_body_row(db: Option<&str>) -> Option<String> {
    let path = db?;
    match Store::open(std::path::Path::new(path))
        .and_then(|s| s.equipped_slot(petstore::DEFAULT_PROFILE, BODY_SLOT))
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!(
                "body: 读 `equipped` 的 {BODY_SLOT} 行失败（{e}）⇒ 这一轮用 `--manifest` 那只（兜底）"
            );
            None
        }
    }
}

/// [`plan_body_override`] 的结论。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BodyChoice {
    /// 覆盖成立 = 起点本体换成**启动扫盘交出来的表**里那一格（调用方拿它去 `bodies[i]`）。
    ///
    /// ⚠️ 判据来自发现表 ⇒ **覆盖天然同档**（[`check_body`] 第 ② 道已经按 `--manifest` 的
    /// `Grid` 过滤过）。所以"启动允许跨档"这一句今天是**协议层面的陈述**（surface 尚未建，
    /// 改尺寸合法），不是这条路径的读数：跨档的那一只在这里落到 `Keep(理由)` 而屏上从来
    /// 不需要改尺寸。真要支持跨档，改的是发现那道过滤（把它做成启动/运行时两套），不是这里。
    Override(usize),
    /// 不覆盖。`Keep(None)` = 库里压根没有那一行（老库、以及一切没换过本体的库）⇒ **一个字
    /// 都不打**，因为那是常态而启动噪音要留给"出事了"。`Keep(Some(理由))` 必须点名。
    Keep(Option<&'static str>),
}

/// "库里那一行能不能当起点本体"的**纯**判定（拆出来的唯一理由 = 让 J3 那条读数在 headless
/// 里也量得到：外层要开库、要解码、要 compositor，这里三样都不需要）。
///
/// 只认**发现表里有的**那一只，而且只按 `id` = 目录名认（库存与 `equipped` 记账用的就是它）。
/// 那句 `Keep` 的文案要说清"为什么不覆盖"有两个不同的原因（目录不存在 / 档不同被扫盘挡掉），
/// 而**不**把它们合并成"读不到"：这一行报的是"你上次换的那只现在这只机器上装不上"，
/// 用户下一步该去找产物，不是去查数据库。
///
/// ⚠️ `start_id` 那一条是 11-07 真机 E 轮实测出来的：库里那一行写的就是 `--manifest` 已经给的
/// 那一只时，旧版无条件走 `Override` ⇒ stderr 打出「起点本体从 cat_shiro 换成这只」而**什么也没
/// 换**。拍板③ 的原话是"**覆盖发生时必须打一行点名**"，读到的那一行不等于覆盖发生 ⇒ 这一臂今天
/// 走 `Keep(None)`（静默 = 常态），而它的读数是下面那条真值表里的第 ③⑤ 两格（后者是"发现表为空
/// 而起点正是它"那一面 —— 判据必须排在 `position()` 之前，否则那台机器上会平白多一行假点名）。
fn plan_body_override(row: Option<&str>, pet_ids: &[String], start_id: &str) -> BodyChoice {
    let Some(id) = row else {
        return BodyChoice::Keep(None);
    };
    // **先问"是不是本来就在身上"**，再问它在不在发现表里：那一行就是起点那只时这一轮什么都没
    // 换，而"没有本体库 / `--test-rect`"的机器上发现表是空的 ⇒ 把这条判据放在 `position()`
    // 之后会得到一句假点名（"上次换的那只装不上"）—— 它装得上，它就是身上这只。
    if id == start_id {
        return BodyChoice::Keep(None);
    }
    match pet_ids.iter().position(|p| p == id) {
        Some(i) => BodyChoice::Override(i),
        None => BodyChoice::Keep(Some(
            "库里那一行指的本体不在启动扫盘交出来的表里（目录不存在、清单或产物坏了，或格子档与 `--manifest` 那只不同）",
        )),
    }
}

/// 把 `equipped` 读回的整表分成"本体那一行"与"配件那几行"（P5 给启动恢复补的那道剥壳）。
///
/// 为什么**必须**先剥：`Store::equipped` 的 `ORDER BY slot, item_id` 里 `body` 排在**所有**配件
/// 槽位之前（字母序），于是那一行是整表的第一条，而 [`plan_restore`] 的第 1 道只问"配件表里有没有
/// 这个 id" ⇒ 它必然走到那句 `磁盘上找不到这一件（已删除，或五道门禁没过）`。**坏的不是行为，是
/// 那句话**，而这两件事在本项目里从来不是同一个优先级：
/// ① 它说的是一件**根本不是配件**的账 ⇒ 用户下一次就去查配件库、查 `pack.py`，而真正的下一件事
///    是"上次换的那只本体在这台机器上装不上"（这句话 [`db_body_row`] 那一段早就该说了）；
/// ② 每一轮启动都打这一行 ⇒ 一句永久性的假话，而"启动噪音要留给出事了"是 [`App::restore_worn`]
///    那条既定裁定。
///
/// ⚠️ 明写**不构成**危害的那一条，免得后人把它当成更强的理由：本体行**不会**把帽子挤掉。
/// `plan_restore` 取的是"第一条过了四道的行"，而本体行在第 1 道（或第 3 道 `slot != 清单声明的
/// 槽位`，因为 `Slot` 六值里没有 `body`）就被 `continue` 掉了，一次都不占 `wear`。⇒ 这条剥壳
/// 的全部意义是**不说假话**，不是"少剥就丢装备"。
///
/// 返回 `(本体的 id, 只剩配件的行)`。多于一条 `body` 行 = 手工行或旧版本写的 ⇒ **取首条**，
/// 与 [`db_body_row`] 经 [`Store::equipped_slot`] 给的答案一致（同一条 `ORDER BY`）。
fn split_body_row(rows: &[(String, String)]) -> (Option<String>, Vec<(String, String)>) {
    let mut body: Option<String> = None;
    let mut accessories = Vec::new();
    for (slot, item_id) in rows {
        if slot == BODY_SLOT {
            if body.is_none() {
                body = Some(item_id.clone());
            }
        } else {
            accessories.push((slot.clone(), item_id.clone()));
        }
    }
    (body, accessories)
}

/// [`plan_restore`] 的结论 = 要穿的那一格（`None` = 一件都不穿）+ 每一行的处置。
struct RestorePlan {
    wear: Option<usize>,
    /// 给人看的行，**只进 stderr 不上屏**（用户内容不进子集字体，同 `DressView::withheld`）。
    /// 每一条都点名"哪一格、哪一件、为什么"。
    notes: Vec<String>,
}

/// 启动恢复那一道的**纯**判定（P3 = FLOW §5 那张表的读侧）。
///
/// 拆出来同 [`DressView::select`] 一个理由：外层要连库、要解码、要 compositor，只有这里这三样
/// 都不需要 ⇒ "手工往 `equipped` 塞一行未拥有的 id 必须出局并报一句"（J2 的后半）才有读数。
///
/// 四道各管一件事，顺序 = 便宜在前：
/// 1. **磁盘上找不到这一件** —— 已删除，或五道门禁没过（[`discover_accessories`] 把它跳掉了）。
///    ⚠️ 这一条把"恢复时重跑五道"整个接住了：恢复能穿的**只能是 discovery 活着交出来的那件**，
///    所以清单坏了 / 轮廓缺格 / 不 `fits` 本体的那几道，在恢复这一刻都由同一条打到。单独再跑
///    一遍五道只会多出第二套答案。第五道（解码）不在这里 —— 它是像素，只能穿的那一刻验。
/// 2. **库存没有这一件** ⇒ 出局并报（这一条拦的正是手工行；可见集 `visible` 来自
///    [`DressView::select`] ⇒ 换装页"许不许穿"与恢复"穿不穿"从头就是同一道判据，不会有
///    "换装页看不见而启动却穿上了"那种分裂）。
/// 3. **这一行记的槽位与清单声明不符** ⇒ 手工行的第二种形状，不穿。槽位的真值在清单
///    （`Slot` 六值），而 `equipped.slot` 没有 CHECK（FLOW §5 第 2 条），所以只能这样对。
/// 4. **多于一件**：内存档 M1 只容一件 ⇒ 取第一件，其余只报告、不穿，而**库里那几行原样留着**
///    （删掉等于把用户没让我删的东西丢了；哪天容得下多件，它们自己会恢复）。
///    "第一件"可重放的前提 = 行序由 `Store::equipped` 的 `ORDER BY slot, item_id` 给。
fn plan_restore(
    rows: &[(String, String)],
    ids: &[String],
    slots: &[String],
    visible: &[usize],
) -> RestorePlan {
    let mut wear: Option<usize> = None;
    let mut notes: Vec<String> = Vec::new();
    for (slot, item_id) in rows {
        let at = |i: usize| format!("slot={slot} item={item_id} ⇒ {}", ids[i]);
        let Some(i) = ids.iter().position(|id| id == item_id) else {
            notes.push(format!(
                "{slot}={item_id} 现在装不上：磁盘上找不到这一件（已删除，或五道门禁没过）"
            ));
            continue;
        };
        if slots[i] != *slot {
            notes.push(format!(
                "{} 而清单声明的是 {slots_i} ⇒ 这一行是手工塞的，不穿",
                at(i),
                slots_i = slots[i]
            ));
            continue;
        }
        if !visible.contains(&i) {
            notes.push(format!("{} 出局：库存里没有这一件 ⇒ 未验过拥有不穿", at(i)));
            continue;
        }
        if wear.is_some() {
            notes.push(format!(
                "{} 不恢复：内存档 M1 一次只容一件（库里这一行留着，以后容得下多件会自动穿）",
                at(i)
            ));
            continue;
        }
        wear = Some(i);
    }
    RestorePlan { wear, notes }
}

/// 装备中的一件配件（D16 路线 B = **运行时逐格叠加**）。内存档 **M1 = 一次一件**，
/// 所以"有没有穿"就是这一个 `Option`：穿了 ⇒ 它的 sheet 在内存里；脱了 ⇒ 整个丢掉。
///
/// 两个字段必须**同时存在**才画得对（清单说这一档烘没烘过、图集供像素），所以挤在一个结构里
/// 而不是铺成 `App` 上的两个 `Option` —— 后者能表示"有清单没像素"这种一画就错的组合。
///
/// ⚠️ 这里**没有 `regions`**：配件的穿透矩形在装备那一刻就已经并进 [`App::regions`] 了
/// （[`Candidate::merged`]，它在启动时由 [`Regions::merged_with`] 算好）。并集是一次性的事实，
/// 不是每次出图、也不是每次装卸都要重算的东西。
/// ⚠️ 这里也**没有 `dir`**：像素已经在这里了，而回源目录只有卸下重穿时才有用 ⇒ 那是
/// [`Candidate`] 的职责，混进来就会有两个"这件配件从哪来"。
struct Equipped {
    manifest: AccessoryManifest,
    atlas: Atlas,
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
    /// 收藏册 / 设置那块 surface；`None` = 没开。
    ///
    /// **与菜单互斥**（开面板即关菜单，反之亦然），所以「同时两块开着」这个形状 structurally
    /// 不可能 —— 这不是偷懒：同层三块 surface 的堆叠序在合成器侧不稳定，而 `placement` 的两两
    /// 不相交检查会变成三块面的算术，收益只有"少切一块面"。
    panel: Option<PanelState>,
    pointer: Option<ThemedPointer>,
    raw_pointer: Option<WlPointer>,
    qh: Option<QueueHandle<App>>,

    manifest: &'static Manifest,
    atlas: Option<Atlas>,
    /// **本体自己那份**穿透轮廓，永远不含配件 ⇒ 脱装的复原就是把它换回 [`Self::regions`]。
    /// 存两份而不是"脱装时重读 `polygon.json`"：重读会失败（文件可能已被搬走），而失败发生在
    /// 用户刚点完"脱下"的那一刻 —— 一份几 KB 的表换来一条不可能失败的复原路径。
    body_regions: Option<Regions>,
    /// **已经并好配件轮廓**的那份区域表（没穿配件时就是本体那份，逐位不变）。
    regions: Option<Regions>,
    /// 配件库（启动时扫过四道门禁、**一像素未解**的那几件）；空 = 库里没一件能用。
    ///
    /// 换装页的"换一件"就是在这个表里挪下标 ⇒ 它必须**不含像素**，否则"浏览一下库"
    /// 会把 M1 那条"一次一件"的档吃光。
    candidates: Vec<Candidate>,
    /// 本体库（启动时扫过四道门禁、**一像素未解**的那几只）；空 = 没有第二只换得上去。
    ///
    /// 收藏册的宠物行点下去就是这张表里的下标（`PanelItem::Body`）。它必须不含像素，理由与
    /// [`Self::candidates`] 那一段逐字相同：解码一张 sheet 是 45 MiB，而"浏览一下库"不该付它。
    bodies: Vec<BodyCandidate>,
    /// 当前本体的身份（= 库目录名那一侧，与 [`Self::bodies`] 的 `id` 同一个键）。
    ///
    /// 两件事读它：`equipped` 的 `body` 行写它、收藏册那一行"是不是它"的 `same_id` 判据读它。
    /// ⚠️ 它**不上屏** —— 当前是哪只本体就是宠物自己那几个像素，再打一行字就是第二个事实源。
    body_id: String,
    /// [`Self::candidates`] 里当前选中的下标。**只在换装页被读到（画面上没有它）**，
    /// 所以它不进 `menu::content_sig`：没穿东西时点"换一件"，屏上一个像素都没变，
    /// 签名跟着变就是白投一帧（`69fd158` 那个形状的反面用法）。
    picked: usize,
    /// 装备中的配件；`None` = 不叠，出图路径与没有配件时逐字节相同。
    worn: Option<Equipped>,

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
    /// 抽取层，**lazy**：第一次右键才连库。rusqlite + WAL 一打开就把页缓存和语句缓存
    /// 挂进进程，而常驻宠物的 VmHWM 门禁（M4 基线 15.7 MB）不该为"也许根本不抽"付这笔。
    gacha: Option<Gacha>,
    /// 出错出声处（连不上库 / 读库存失败 / 抽取失败 / **写回 `config.toml` 失败**）：画在菜单
    /// 与面板共用的错误行上，**不 panic、不置 `exit`**（硬约束 3 同族 —— UI 的故障不许波及宠物，
    /// 更不许波及桌面）。字段不叫 `gacha_error` 了：拍板④ 之后它管的不止抽取，名字跟着事实走。
    ui_error: Option<String>,
    /// **恢复上次穿着只跑一次**的那道闩（P3）。
    ///
    /// 立成字段而不是"在 `run()` 里直接读库"，有两个理由：① FLOW §5 要的时刻是
    /// **`configured` 之后**（surface 已映射，恢复出来的像素才有人接），而 `run()` 里那句
    /// `discover_accessories()` 还在 configure 之前；② 这条绝不能落进 tick 环里——第二次
    /// "恢复"等于把用户刚脱掉的帽子又扣回去，而那正是 §4 禁轮询要防的形状。
    /// ⚠️ 它同时是"演示态一个字都不读库"的那道门：闩在 [`Self::restore_worn`] 里先放下，
    /// 条件都写在函数里，别在这里预算。
    restore_pending: bool,
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

    /// 「已获得」那一屏的行：每档各拿到几种 / 该档共几种，最后一行是总数 + 重复 + 余额。
    ///
    /// **纯读**：不掷骰、不花钱、不改库 —— 这是它能进菜单而 `redeem_ssr`（花掉 120 碎片、
    /// 不可回退，而这块 surface 没有对话框，D14）不能进的分界。
    ///
    /// 分母取 **catalog** 而不是库存行数 ⇒ "还缺多少"这件事在这一行就读得出来（`n/total`），
    /// 而库存自己只能回答"有什么"。重复数按 `count − 1` 累加：第一件不算重复，哪怕它躺在
    /// 库存里 —— 这是 §3.6 的折算口径（第 2 件起才折碎片），不是我在省一次查询。
    ///
    /// ⚠️ **每一串字都只用「字体子集里已有的字」**（`l10n` 标签 + ASCII + catalog 名）：
    /// 这一批的字现在**同时**是屏上那一行和 stderr 那一行（[`App::gacha_panel`] 一条路径出两边），
    /// 而 32 px 档的字号竞技场只剩 ≈7 个字形。代价**明写而不是藏起来**：原先那句
    /// 「共 3/30 · 重复 5 · 碎片 120」的中文散文没了，读者要认 `DUP` / `Shards` 这类标签；
    /// 换来的是"屏幕上读到的与日志里打到的是同一串字"，一个口径。
    fn collection_lines(&self, lang: Lang) -> Result<Vec<String>, String> {
        let owned = self.store.owned().map_err(|e| format!("读库存失败: {e}"))?;
        let mut lines = Vec::with_capacity(Rarity::ALL.len() + 1);
        let mut dup = 0u64;
        let mut kinds = 0usize;
        let mut total = 0usize;
        for r in Rarity::ALL {
            let mut have = 0usize;
            let mut all = 0usize;
            for item in self.catalog.items_of(r) {
                all += 1;
                if owned.contains(&item.id) {
                    have += 1;
                    let n = self
                        .store
                        .item_count(&item.id)
                        .map_err(|e| format!("读 {} 的件数失败: {e}", item.id))?;
                    dup += n.saturating_sub(1);
                }
            }
            kinds += have;
            total += all;
            lines.push(format!("{} {have}/{all}", r.as_str()));
        }
        lines.push(format!(
            "{kinds}/{total} {} {dup} {} {}",
            lang.t(Key::Dup),
            lang.t(Key::Shards),
            self.balance()
        ));
        Ok(lines)
    }

    /// 「已获得」那一本的**逐件**行（P4 = 收藏册点名 → 直接穿上）。
    ///
    /// ⚠️ **遍历的是 `catalog ∩ owned`（稀有度序 + catalog 内序），不是 `candidates`** —— 后者
    /// 只有磁盘上真的有产物那几件（裸开发树 30 件里只有 1 件），拿它当列表而汇总行读的是
    /// `catalog ∩ owned`，屏上就是"汇总说 12 件而列表只有 1 行"，两个事实自己打自己。
    /// 同一个集合还有一条好处：名字一律来自 catalog，所以不需要 `recent_lines` 那种"catalog
    /// 不认就退化成 id"的分支（那一条读的是账本，账本可以记着目录里没有的 id）。
    ///
    /// 三档**可点/只读**由 [`AlbumEntry::target`] 一格表示，而它的判据是别人给的（不是这里自己算的）：
    /// `ids` = [`App::candidates`] 的目录名表，`visible` = [`App::dress_view`] 跑完库存那道之后
    /// 的可见下标集。⇒ "抽到但磁盘没产物""磁盘有但库存不认""本体不匹配"三种形状都落在
    /// `target = None`，屏上那一行**没有强调条**（画师读的正是可点性，见 [`card::paint_panel`]），
    /// 用户看得见地能分辨，而我不用为这三种形状各造一个新字形或新标记。
    ///
    /// ⚠️ P5 把"宠物永远只读"这一条**换成了第二个判据源**：`pet_ids` / `pet_visible` 是
    /// [`App::bodies`] 的目录名表与 [`App::pet_view`] 的可见集，形状与上面那一对**完全同构**（同一
    /// 个 [`DressView::select`]，只是喂的表不同）。所以本体现在有三档路由：配件格 ⇒
    /// [`AlbumTarget::Accessory`]、宠物格 ⇒ [`AlbumTarget::Body`]、两表都查不到 ⇒
    /// [`AlbumTarget::None`]。**"没有强调条"这一档对三类原因仍然只有一句实现**（未拥有 / 磁盘没
    /// 产物 / 跨档出局），跨档那一只在 [`check_body`] 第 ② 道就被扫盘挡掉 ⇒ 它压根不在
    /// `pet_ids` 里 ⇒ 落到同一支 `None`（= 只读，而不是"点了被拒"，拍板② 的验收形状）。
    ///
    /// `dup` 的折算口径与 [`Self::collection_lines`] **同一个**（`item_count − 1`：第一件不算
    /// 重复），否则逐件行加起来与汇总那一行对不上，就成了"两本账"。
    ///
    /// `owned` 由调用方给（[`App::album_screen`] 一次点击只读一遍库存 ⇒ 逐件那几张行与
    /// 汇总行问的是**同一份**答案，不是"各问了一遍然后祈祷它们相同"）。
    fn album_entries(
        &self,
        owned: &Owned,
        ids: &[String],
        visible: &[usize],
        pet_ids: &[String],
        pet_visible: &[usize],
    ) -> Result<Vec<AlbumEntry>, String> {
        let mut out = Vec::new();
        for r in Rarity::ALL {
            for item in self.catalog.items_of(r) {
                if !owned.contains(&item.id) {
                    continue;
                }
                let kind_pet = item.kind == ItemKind::Pet;
                // **两张表各查各的**，而且"查得到 + 过得了那道闸"两个条件都要成立。
                // 反查一次而不是把下标带进来看起来像"顺手"，其实是要紧的：**id 是唯一入口**，
                // 下标在这里是查出来的结果 ⇒ catalog 的顺序与磁盘的顺序谁变了都不影响路由。
                let target = if kind_pet {
                    pet_ids
                        .iter()
                        .position(|c| c == &item.id)
                        .filter(|i| pet_visible.contains(i))
                        .map_or(AlbumTarget::None, AlbumTarget::Body)
                } else {
                    ids.iter()
                        .position(|c| c == &item.id)
                        .filter(|i| visible.contains(i))
                        .map_or(AlbumTarget::None, AlbumTarget::Accessory)
                };
                let n = self
                    .store
                    .item_count(&item.id)
                    .map_err(|e| format!("读 {} 的件数失败: {e}", item.id))?;
                out.push(AlbumEntry {
                    target,
                    name: item.name.clone(),
                    rarity: item.rarity,
                    slot: item.slot,
                    kind_pet,
                    dup: n.saturating_sub(1),
                });
            }
        }
        Ok(out)
    }

    /// 「还缺」那一屏的行：catalog 里还没拿到的**名字**，按档分组，**每档最多一行**。
    ///
    /// 名字来自 catalog ⇒ 这些字**已经在字体子集里**（`everything_the_ui_can_render_is_in_the_
    /// codes_list` 那条门禁把 catalog 也纳进去了），所以这一屏是换装页那条"配件名盖不住"的反面：
    /// 它真的能上卡片。
    ///
    /// ⚠️ **宽度是量出来的，不是猜的**（这条是这一批新加的，而它本来就该在产行侧）：一行的预算
    /// = [`panel::ROW_TEXT_W`]（只读行让位后的矩形）经 [`menu::budget_for`] 换算，装不下的名字
    /// 折进尾标 [`l10n::line_more`]（`+N 更多`，四语言都已经在子集里 ⇒ 零新字形）。
    /// **不靠 `card` 的 `fit` 兜底**：那一层截出来的是一个 `…`，读者会以为"缺的就这些"，
    /// 而真正缺的还有 9 件 —— 一行只读报表把数量说错，和把金额说错是同一条红线。
    /// 精确计数另有一行（`have/all`），这一屏给的是"缺哪些名字"的线索 + 几个没列出。
    ///
    /// ⚠️ **"放不下"必须问 [`menu::fit_to`] 本身，不能拿 [`menu::line_width`] 自己算**（这条
    /// 是新门禁实测逼出来的，别再改回去）：`fit_to` 决定截点用的是**逐 char +1 px 的逻辑上界**
    /// （`menu::advance_bound_log`，为了"三档截在同一个字上"可证），`line_width` 量的是**实际**
    /// 画宽 ⇒ 按后者产行、按前者截行 = 生产者和排版各有各的预算，实测在 1× 少一个字，
    /// 产出的行被排版截成 `…`，正好落进上面那条红线。问同一个函数 ⇒ 这一屏的结论**按构造**
    /// 就是屏幕上会被原样画出来的那行。
    fn missing_lines(&self, lang: Lang, scale: f32) -> Result<Vec<String>, String> {
        let owned = self.store.owned().map_err(|e| format!("读库存失败: {e}"))?;
        let budget = menu::budget_for(panel::ROW_TEXT_W, scale);
        let fits = |s: &str| menu::fit_to(s, budget, scale) == s;
        let mut lines = Vec::new();
        for r in Rarity::ALL {
            let names: Vec<&str> = self
                .catalog
                .items_of(r)
                .filter(|i| !owned.contains(&i.id))
                .map(|i| i.name.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            let all = format!("{} {}", r.as_str(), names.join(" "));
            if fits(&all) {
                lines.push(all);
                continue;
            }
            // 从"少折一个"往下减，第一个 `fits` 的就收 —— 名字位数最多影响上界里一个 char，
            // 而这里量的是**成品整行**，所以不存在"列完才发现尾标挤不下"那种改判。
            let mut line = format!("{} {}", r.as_str(), l10n::line_more(lang, names.len()));
            for k in (1..names.len()).rev() {
                let cand = format!(
                    "{} {} {}",
                    r.as_str(),
                    names[..k].join(" "),
                    l10n::line_more(lang, names.len() - k)
                );
                if fits(&cand) {
                    line = cand;
                    break;
                }
            }
            lines.push(line);
        }
        if lines.is_empty() {
            lines.push(format!("0 {}", lang.t(Key::Missing)));
        }
        Ok(lines)
    }

    /// 「最近」那一屏的行：账本最后 `n` 次抽取。**只读 `gacha_pull`，一个字都不写**。
    ///
    /// ⚠️ **不印时间**：`ts` 是 unix 秒，而这个 crate 没有任何日期格式化依赖（引一个只为把
    /// 一串数字变好读，代价是 `Cargo.lock` 与 D8 那条许可证核对）。印 `seq` 而不是 `ts`：
    /// 序号是账本自己的顺序、可重放、可和 `audit verify` 的 `len` 对上，而墙钟秒数在屏幕上
    /// 也读不出"几点"。
    ///
    /// 一次全表读（`rows` 没有 LIMIT）换来的是"这一条路径与 `audit verify` 走的是同一个
    /// 查询" ⇒ 少一个口径。点击驱动、不是每 tick 轮询（§4 那条禁止）。
    fn recent_lines(&self, lang: Lang, n: usize) -> Result<Vec<String>, String> {
        let rows = self
            .store
            .rows(petstore::DEFAULT_PROFILE, petstore::DEFAULT_BANNER)
            .map_err(|e| format!("读账本失败: {e}"))?;
        if rows.is_empty() {
            return Ok(vec![format!("0 {}", lang.t(Key::Recent))]);
        }
        let from = rows.len().saturating_sub(n);
        Ok(rows[from..]
            .iter()
            .map(|r| {
                let name = self
                    .catalog
                    .find(&r.item_id)
                    .map(|i| i.name.as_str())
                    .unwrap_or(r.item_id.as_str());
                let dup_tag = if r.duplicate {
                    format!(" {}", lang.t(Key::Dup))
                } else {
                    String::new()
                };
                format!(
                    "#{seq} {rar} {name}{dup_tag} +{shards}",
                    seq = r.seq,
                    rar = r.rarity.as_str(),
                    shards = r.shards_gained,
                )
            })
            .collect())
    }

    /// 「复核」那一屏的两行：审计链全链校验 + 用**库里的** seed 与公示参数重放。
    ///
    /// `Store::verify` 只读 ⇒ 这一行不需要确认步骤（同 [`Self::collection_lines`] 那条分界）。
    /// 报红的时候**只报错**，不报"通过了一半"：一条链要么自洽要么不自洽，中间态不存在。
    ///
    /// 档位计数那行借 [`l10n::line_tiers`]：它与结果卡用的是同一个生产者 ⇒ 同一批字，
    /// 而 `line_tiers` 只列非零档 ⇒ 空账本那行是光秃秃一个 `OK`，这是事实而不是排版事故。
    ///
    /// ⚠️ **链头在这块屏上只能是前缀，而前缀必须自己说它是前缀**（这条同样是新门禁逼出来的：
    /// 原先那行 `复核 {len} {64 位十六进制}` 实测在 1× 被 `fit_to` 截成
    /// `复核 10 d2427b64ac70b454da557…` —— 一个被悄悄切短的哈希比不印哈希更坏，读者会拿它去
    /// 和 CLI 的输出逐字比，而"少了 39 位"这件事一行像素门禁都看不见）。所以这里
    /// **先数出装得下的位数、把它写进正文**（`{k}/64`，纯 ASCII ⇒ 零新字形），再按同一口径
    /// 用 [`menu::fit_to`] 挑最长的那一版。完整的 64 位不在屏上也不在日志里：权威出处是
    /// `opet audit verify --db … --account …`（它打的就是同一个 `head_hex`）。
    /// k=0（行窄到一位都装不下）就整个字段不印 ⇒ 那一行只剩 `复核 {len}`，是少说，不是说错。
    fn verify_lines(&self, lang: Lang, scale: f32) -> Result<Vec<String>, String> {
        let report = self
            .store
            .verify(
                &self.catalog,
                petstore::DEFAULT_PROFILE,
                petstore::DEFAULT_BANNER,
            )
            .map_err(|e| format!("复核不过: {e}"))?;
        let budget = menu::budget_for(panel::ROW_TEXT_W, scale);
        let fits = |s: &str| menu::fit_to(s, budget, scale) == s;
        let head = report.head_hex();
        let base = format!("{} {}", lang.t(Key::Verify), report.len);
        let mut shown = String::new();
        for k in (1..=head.len()).rev() {
            let cand = format!("{base} {k}/{} {}", head.len(), &head[..k]);
            if fits(&cand) {
                shown = cand;
                break;
            }
        }
        if shown.is_empty() {
            shown = base;
        }
        Ok(vec![
            shown,
            format!("OK {}", l10n::line_tiers(&report.counts)),
        ])
    }

    /// 收藏册三个 tab 的**唯一**行生产者：菜单那三行、面板里点标题带换 tab、切语言后重建行
    /// 三条路都从这里过 ⇒ 不存在"屏上是这一个 tab 的字、stderr 打的是另一个"。
    ///
    /// `Recent` 的条数 = [`panel::MAX_ROWS`] 减一（一屏正好装下，见
    /// [`panel::collection_rows`] 那条"正解在调用方"的注释）；给多了不报错而是截断补
    /// 「+N 更多」，那条分支是保险，不是主路径。
    ///
    /// `scale` 只有 `Missing` 那一支读（[`Gacha::missing_lines`] 按宽度折名字），另外两支
    /// 的行与缩放无关。它和 [`Gacha::pull`] 的 `scale` 是同一个口径：**生产行的那一刻就知道
    /// 自己要上哪块屏**，而不是画的时候再猜。
    fn tab_lines(&self, tab: CollectionTab, lang: Lang, scale: f32) -> Result<Vec<String>, String> {
        match tab {
            CollectionTab::Owned => self.collection_lines(lang),
            CollectionTab::Missing => self.missing_lines(lang, scale),
            CollectionTab::Recent => self.recent_lines(lang, panel::MAX_ROWS - 1),
        }
    }
}

/// 逐件收藏册里**一件**要说的事（还没变成能画的行）。
///
/// 它刻意不是 `CatalogItem` 的引用：一件"装包来的池外件"（catalog 不认的 id）也要能进这张表，
/// 而那时只有 id 字符串可说。
///
/// 一行点下去要去**哪张表**（P5 = 这一面第一次同时出现两套下标空间）。
///
/// 为什么不写成 `Option<usize>` 加一个"这是宠物还是配件"的布尔：那样能表示
/// `{ index: Some(3), kind: Accessory }` 而 `candidates[3]` 其实是另一件东西 —— 也就是把"这个数字
/// 属于哪张表"交给**旁表**去说，而旁表要跨 `show_panel` / `refresh_panel_rows` / `album_screen`
/// 保持对齐（[`panel::PanelItem::Wear`] 那条注释立的同一条纪律，本项目为它付过学费）。判别式让
/// "哪张表"和"下标"**必须一起写**，写错一张表就是编译错误而不是运行时的一次错穿。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlbumTarget {
    /// [`App::candidates`] 的下标 ⇒ 点它走 [`App::pick_and_wear`]。
    Accessory(usize),
    /// [`App::bodies`] 的下标 ⇒ 点它走 [`App::swap_body`]。⚠️ 与上面那个数字**不是同一个空间**。
    Body(usize),
    /// 只读行：未拥有 / 磁盘没产物 / 不 `fits` 本体 / 跨档在本体侧出局。
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AlbumEntry {
    /// 这一行**此刻**点得动吗、点下去去哪张表。`None` = 只读行，屏上没有强调条（画师读的可点性
    /// 正是这一格，见 [`card::paint_panel`]）。
    target: AlbumTarget,
    /// catalog 里的名字；catalog 不认这个 id 时退化成 id 本身（ASCII）—— 与
    /// [`Gacha::recent_lines`] **同一个既有口径**，不是新洞。
    name: String,
    rarity: Rarity,
    slot: Option<Slot>,
    /// `kind == Pet` ⇒ 行尾打一个 ASCII `pet` 标。
    ///
    /// ⚠️ 它**不参与路由**（路由看 [`AlbumEntry::target`]）：P4 那一轮它同时是"为什么这一行永远
    /// 不给下标"的理由（那时 `wear()` 解的是配件三件套，传一个宠物 id 进去在类型上合法而屏上什么
    /// 都没有），P5 之后宠物有了自己的下标空间，这一格就只剩**排版**职责（行尾那个 `pet` 字）。
    /// 留着一个不参与判定的布尔而它恰好和判别式同真是巧合不是约束 ⇒ 所以这里的注释要说清它不管什么。
    kind_pet: bool,
    /// 重复数 = `item_count − 1`，与 [`Gacha::collection_lines`] 同一个折算口径。
    dup: u64,
}

/// 逐件那一本的行文本 = **一行一件**，宽度纪律立在产行的这一刻（[`Gacha::missing_lines`]
/// 那条同族理由：必须问 [`menu::fit_to`] 本身，不许拿 [`menu::line_width`] 自己算）。
///
/// ⚠️ **预算按可点/只读分两档**：可点行让出左端那条强调条 ⇒ [`panel::ROW_LABEL_W`]（209），
/// 只读行是 [`panel::ROW_TEXT_W`]（216）。**用错一档就是"生产者和排版各有各的账"**：按 216
/// 裁出来的可点行送到 `menu::label_rect` 手里就差出一个汉字，被排版截成 `…`，而"这件叫什么"
/// 是用户点它的唯一依据。
///
/// 装不下时**按 `dup` → `slot` 的顺序丢词**，最后只剩 `{RAR} {name}`，**绝不让 `card` 截成
/// `…`**：丢掉的两个词都是"能从别处再读到"的信息（重复数在汇总那一行、挂点在换装页），
/// 而一个 `…` 说的是"这件的名字我不知道"，那是假信息。
/// 极端情况（名字本身就超预算）无词可丢 ⇒ 只能交给排版，那一条由
/// `every_panel_row_is_sayable_and_never_truncated` 用**真 catalog 真三档缩放**当场量红，
/// 不是靠这句注释保证。
///
/// 这里**不需要 `lang`**：token 全是 ASCII（`Rarity::as_str` / `Slot::as_str` /
/// `ItemKind::as_str` / `x{dup}`）加上本来就在子集里的 catalog 名 ⇒ 逐件那几张行**与语言无关**，
/// 而"签名里有、函数体不读"是本项目反复抓的形状（见 [`panel::collection_rows`] 那条同一纪律）。
fn album_rows(entries: &[AlbumEntry], scale: f32) -> Vec<panel::PanelRow> {
    entries
        .iter()
        .map(|e| {
            let actionable = !matches!(e.target, AlbumTarget::None);
            let width = if actionable {
                panel::ROW_LABEL_W
            } else {
                panel::ROW_TEXT_W
            };
            let budget = menu::budget_for(width, scale);
            let slot = e.slot.map(Slot::as_str);
            let dup = if e.dup > 0 {
                Some(format!("x{}", e.dup))
            } else {
                None
            };
            // 从"信息最全"往下退：① 全量 ② 丢重复数 ③ 再丢挂点 ④ 只剩档名 + 名字。
            let render = |with_slot: bool, with_dup: bool| {
                let mut s = String::from(e.rarity.as_str());
                if with_slot {
                    if let Some(sl) = slot {
                        s.push(' ');
                        s.push_str(sl);
                    }
                }
                s.push(' ');
                s.push_str(&e.name);
                if e.kind_pet {
                    s.push(' ');
                    s.push_str(ItemKind::Pet.as_str());
                }
                if with_dup {
                    if let Some(d) = &dup {
                        s.push(' ');
                        s.push_str(d);
                    }
                }
                s
            };
            let ladder = [
                render(true, true),
                render(true, false),
                render(false, false),
            ];
            let mut text = ladder[ladder.len() - 1].clone();
            for cand in &ladder {
                if menu::fit_to(cand, budget, scale) == *cand {
                    text = cand.clone();
                    break;
                }
            }
            panel::PanelRow {
                item: match e.target {
                    AlbumTarget::Accessory(i) => panel::PanelItem::Wear(i),
                    AlbumTarget::Body(i) => panel::PanelItem::Body(i),
                    AlbumTarget::None => panel::PanelItem::Info,
                },
                text,
            }
        })
        .collect()
}

/// 「点中一件」要判的三档结论（**纯**版本，方法论同 [`DressView::select`]：`App` 在没有
/// compositor 的机器上构造不起来 ⇒ 把"要判的那件事"改写成写得出来、也测得出来的形状）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickWear {
    /// 库存那道没过（未拥有 / 库读不出 / 这件不匹配本体 / 下标已失效）⇒ 一个字都不动。
    Refused,
    /// 点的**就是**身上这一件 ⇒ 不解像素、不重投帧（点了什么都没发生是**对的**，但它必须说话）。
    AlreadyWorn,
    /// 先释放身上那件（若有任何一件），再装载这一件。
    SelectAndWear,
}

/// [`PickWear`] 的判定本体。拆出来的唯一理由 = 让"先比后写"这件事有一条 headless 读数。
///
/// ⚠️ `AlreadyWorn` 的比较**必须**用写之前的 `picked` —— [`App::pick_and_wear`] 里那句
/// "先比再写"的次序就是这条函数存在的理由：一旦先写 `picked` 再比，`picked == want` 恒真，
/// 最常见的那一次点击（换一件别的）会读成"已经穿着这一件"⇒ 静默不动 + 一句谎话。
fn plan_pick_wear(gate_passes: bool, worn_some: bool, picked: usize, want: usize) -> PickWear {
    if !gate_passes {
        return PickWear::Refused;
    }
    if worn_some && picked == want {
        return PickWear::AlreadyWorn;
    }
    PickWear::SelectAndWear
}

/// 「点中一只宠物」要判的四档结论（**纯**版本；方法论与 [`PickWear`] 同一条：`App` 在没有
/// compositor 的机器上构造不起来 ⇒ 判定必须先改写成写得出来、也测得出来的形状）。
///
/// 比配件那一族多一档 `Same` 而少一档 `AlreadyWorn` 的写法差别不是审美：**"身上已经是这只"
/// 是一个不需要"穿着"这个前提的状态**（本体永远在，`worn` 那格才可能是 `None`），所以它的判据
/// 是 `id` 而不是下标 —— 下标是`Body` 那套（[`App::bodies`]），而"当前是哪只"记在
/// [`App::body_id`]。用下标比就会有两个"当前本体是谁"。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SwapBody {
    /// 四道都过 ⇒ 走 [`App::swap_body`] 那九步。
    Switch,
    /// 点的就是当前这只 ⇒ 不解像素、不重投帧、**账也不写**（写一次 `body` 行等于给"没发生的
    /// 那次换"留一条重启读数）。
    Same,
    /// 某一道没过 ⇒ 一个字都不动。带**那道的名字**，因为三道的文案是不同的事实：
    /// "这一格根本没有这只"、"档不同所以运行时换不了"、"库存没验过拥有"是三句不同的回答。
    Refused(&'static str),
}

/// [`SwapBody`] 的判定本体，四道的**顺序 = 便宜的、以及"错了会更贵"的在前**。
///
/// ⚠️ `same_grid` 排在库存那道**之前**，理由不是"更便宜"而是"更贵的那个错"：`atlas.reload`
/// 自检的是"新 sheet 的像素尺寸 == `cols×cell_w, rows×cell_h`"，而那三个数仍是**旧**本体的
/// （[`App::atlas`] 只在构造时由清单算出，`reload` 不动它们）。所以在档不同的时候，失败关闭
/// 的形状是"白付一次读盘 + 一次解码尝试 + 一句几何错误"，而**赌赢**（两份 WebP 尺寸恰好相同）
/// 的形状是"新像素配旧行表 ⇒ 屏上是一堆没人能解释的格子"。后者正是本项目反复要杀的静默。
///
/// 这一道的现实读数在扫盘那一步（[`check_body`] 第 ② 道 ⇒ 跨档的那只压根进不了
/// [`App::bodies`]，收藏册里是只读行、点不到）。留在这里是**第二道保险**：唯一合法走到它的
/// 调用者是"以后有人绕过发现直接往 `bodies` 里塞一只"，而那件事的读数只能来自单测。
///
/// `same_id` 排在最后（与 [`plan_pick_wear`] 把 `AlreadyWorn` 排在 `Refused` 之后同一个理由）：
/// 三道门禁里任何一道没过都不该给出"已经是这一只"这句**听起来像成功**的话 —— 那会把"我为什么
/// 点不动它"回答成一个不需要回答的问题。
fn plan_swap_body(in_range: bool, gate_passes: bool, same_id: bool, same_grid: bool) -> SwapBody {
    if !in_range {
        return SwapBody::Refused("这一格不在本体库里（启动扫盘之后库被改过？）");
    }
    if !same_grid {
        return SwapBody::Refused(
            "格子档与当前本体不同 ⇒ 运行时不动 surface 尺寸 / 目标尺寸 / shm 池，跨档只能重启",
        );
    }
    if !gate_passes {
        return SwapBody::Refused(
            "库存那道没过（未拥有，或给了 --db 而这一轮读不出库存）⇒ 没验过拥有不换",
        );
    }
    if same_id {
        return SwapBody::Same;
    }
    SwapBody::Switch
}

/// 「概率」那一屏要上屏的**五行整**：一行图例 + 四档各一行。
///
/// ⚠️ **这条路不碰库**，而且是故意的：它读的是编译期就嵌进二进制的 `rates.json`
/// （`DEFAULT_RATES_JSON` = `RATES_ASSET` 那份，与 `catalog rates --out` 写的是同一个产物），
/// 回答的是"承诺了什么"而不是"这次抽到了什么"。所以它**不需要** `ensure_gacha()` ——
/// 让它去连库就等于"内存库的玩家读不到公示"，而公示对每个人都一样。
///
/// `Published::check` 是硬约束 #2（"概率必须与公示一致"）的守门人：文件与代码不一致时
/// 这一行报**不一致**，而不是把那份已经在说谎的文件念给用户听。宁可不答，不可答错。
///
/// **五行 = `panel::MAX_ROWS - 1`**，一格余量都不留：这一面第六格固定是 `Close`，多一行
/// 就得靠 `panel::collection_rows` 那条截断补「+N 更多」，而"你还没看到的第四档概率"被
/// 截成一句话是这一屏最坏的失败形状。
///
/// 两处口径是明写的取舍（不是遗漏）：
/// - **两位小数**（`{:.2}`）而不是 `percent_str` 的四位：`SSR 60.0000>57.0147` 一定超 240 px
///   那一行的排版预算（§4 那次量化）。四位那一份没消失，它就在 `rates.json` 里。
/// - 原先那行"公示口径 method=/视界=/format="**不再上屏也不进日志**：五行装不下它，而点
///   「概率」要问的是"某一档多大概率"，不是"这份公示用什么口径推的"。
///
/// 写成**自由函数**（`Gacha` 与 `App` 都不碰）是为了让面板那两条门禁能直接吃到它：
/// 这一批第一次把「概率」的字送进像素，而"每一行都画得出来而且不破版"这件事的入口是鼠标，
/// 我绝不注入点击 ⇒ 它必须在 headless 里可调用。挂在 `App` 上就等于把自动化让给了真机。
fn published_rates_lines(lang: Lang) -> Result<Vec<String>, String> {
    let published = RatesFile::read(DEFAULT_RATES_JSON)?;
    let effective = published.check(&GachaParams::V1)?;
    // 百万分位 → 百分数。这里**只有**显示用的格式化碰浮点：`petcore` 里那份
    // `percent_str` 走整数是为了公示字符串逐字符可复现（硬约束 #8），而这一行不进任何
    // 重放契约，`{:.2}` 换来的排版余量才是这一屏要付的代价。
    let pct = |w: u32| w as f64 / 10_000.0;
    let mut lines = vec![format!("{} b>c", lang.t(Key::Rates))];
    for r in Rarity::ALL {
        lines.push(format!(
            "{} {:.2}>{:.2}",
            r.as_str(),
            pct(published.base.weight(r)),
            pct(effective.weight(r))
        ));
    }
    Ok(lines)
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

    let mut manifest = load_manifest(&opts)?;
    // 本体库排在**第一次解码之前**扫一遍，因为库里那一行 `body` 可能改写"第一只本体是谁"，
    // 而那一天绝不该解两张 sheet（峰值 = 两只本体 ≈147 MB，它落在 ≤200 MiB 门禁内 ⇒ 什么都
    // 不会红，唯一的读数就是这段代码本身）。
    //
    // ⚠️ 这道 `pixels` 门槛与 [`load_asset`] 里那两个"没有像素"的分支**是同一个条件**
    // （`--test-rect` / 没给 `--manifest`）：原先这张表是在 `body_regions` 出来之后按"本体有没有
    // 像素"算的，换成这个布尔量是为了打断 `discover_pets → load_asset → discover_pets` 那条环。
    let pixels = !opts.test_rect && opts.manifest.is_some();
    let mut bodies = match pet_root().filter(|_| pixels) {
        Some(root) => discover_pets(&root, &manifest.grid),
        // 本体没像素（`--test-rect` / 内置 fixture 不读盘的那条路）⇒ 换了没东西可画；
        // `$HOME` 与 `$XDG_DATA_HOME` 都读不到 ⇒ **没有库**。两种都是"收藏册的宠物行全部只读"，
        // 不是错误（这张表**不**参与"起不起来"的判断）。
        None => Vec::new(),
    };
    // —— 拍板③：库里的 `body` 行**赢**，`--manifest` 退化成"库里没有那一行时用什么"。
    // 老库（没有那一行）⇒ 这一整段是 `Keep(None)`，一个字都不打，行为与 P4 逐字节相同。
    let row = db_body_row(opts.db.as_deref());
    let pet_ids: Vec<String> = bodies.iter().map(|b| b.id.clone()).collect();
    // 起点本体的身份（`id` = 目录名 = 库存与 `equipped` 认的那个键，见 [`BodyCandidate::id`]）。
    let mut body_id = start_body_id(opts.manifest.as_deref(), &manifest.slug);
    // 覆盖成立时要连**目录**一起换：sheet 与 polygon 是从清单所在目录取的。
    let mut body_dir: Option<std::path::PathBuf> = None;
    match plan_body_override(row.as_deref(), &pet_ids, &body_id) {
        BodyChoice::Override(i) => {
            let b = &mut bodies[i];
            // 这一行排在 `manifest` 被改写**之前** ⇒ `{fallback}` 念到的还是兜底那只的 `id`。
            eprintln!(
                "body: 库里那一行 {slot}={id} ⇒ 起点本体从 {fallback} 换成这只（`--manifest` 退化成\
                 兜底；格子档与它相同 ⇒ surface 尺寸 / 目标尺寸 / shm 池一个都不必改）",
                slot = BODY_SLOT,
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
            slot = BODY_SLOT,
            id = row.clone().unwrap_or_default(),
        ),
        BodyChoice::Keep(None) => {}
    }
    let (atlas, body_regions) = load_asset(&opts, manifest, body_dir.as_deref())?;
    // 配件库在启动时**只读元数据**（清单 + 轮廓 + 四道门禁里除解码之外的全部），一像素不解，
    // 于是 `regions` 起手就是本体那份 —— 出图路径上没有"这份区域含不含配件"这个问题。
    //
    // ⚠️ 本体那份是**借出去**的（`as_ref()`）：[`Regions::merged_with`] 新造对象，而 discovery
    // 全程只读它 ⇒ 零拷贝、"未穿的真值"那份自然原样。
    let candidates = match (accessory_root(), &body_regions) {
        (Some(root), Some(body)) => discover_accessories(&root, manifest, body),
        // 本体没像素（`--test-rect` / 内置 fixture 不读盘的那条路）⇒ 没有可叠之物，库里有什么
        // 都一样；`asset:` 那行已经说明了这轮不读像素，这里不再补一行噪音。
        // `$HOME` 与 `$XDG_DATA_HOME` 都读不到 ⇒ **没有库**，同上：换件表空，不是错误。
        _ => Vec::new(),
    };
    let regions = body_regions.clone();
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

    // 池按 2× 预留（@192：224 逻辑 ⇒ 448 物理 = 784 KiB × 3 槽），更大尺寸由 `SlotPool` 自己扩。
    let phys = (logical.0 * 2, logical.1 * 2);
    let initial = phys.0 as usize * phys.1 as usize * 4 * 3;
    let pool = SlotPool::new(initial, &shm).map_err(|e| format!("shm 池建不起来: {e}"))?;

    // `--db` 没给 ⇒ 演示模式（不建文件、UI 打 DRYRUN 标）。在 `opts` 被搬进 App 之前取出来。
    let dry_run = opts.db.is_none();
    // 豁免开关没配 `--db` = 一条永远不会点亮的作用域（演示态本来就放行，未拥有的概念在内存库里
    // 不存在）。**不报错、也不静默**：静默就是 `--cell` 那个假开关的形状，而它当时只打印了一句
    // 就继续写产物 —— 区别在于这里没有产物可写，唯一能做的就是把这句话说清。
    if opts.allow_unowned_accessories && dry_run {
        eprintln!(
            "ui: --allow-unowned-accessories 没起作用也不需要 ⇒ 无 --db = 演示态，库存恒空、一律放行（屏上右端那个「演示」标就是这件事的记号）"
        );
    }
    // 本体那条豁免的兄弟行（拍板④ = 两条开关分开，所以**两句分开**）。屏上记号这一档**没有**：
    // 收藏册没有属于"本体"的那一页可以打标，而"当前是哪只"就是宠物自己那几个像素。
    if opts.allow_unowned_pets && dry_run {
        eprintln!(
            "ui: --allow-unowned-pets 没起作用也不需要 ⇒ 无 --db = 演示态，库存恒空、一律放行（本体侧不打屏上记号，见 --help）"
        );
    }
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
        panel: None,
        pointer: None,
        raw_pointer: None,
        qh: Some(qh),
        manifest,
        atlas,
        body_regions,
        regions,
        candidates,
        bodies,
        body_id,
        picked: 0,
        worn: None,
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
        gacha: None,
        ui_error: None,
        restore_pending: true,
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

/// 解**起点本体**那一张 sheet（本轮唯一一次本体解码）。
///
/// `body_dir` = 库里那一行 `body` 覆盖出来的目录（[`plan_body_override`] 那一臂）。给出来时
/// sheet 与 polygon 都从**它**取，而不是从 `--manifest` 的父目录取 ⇒ 覆盖成立的那一轮
/// 从头到尾只解**一只**本体的像素。`None` = 没有覆盖（老库、无 `--db`）⇒ 逐字节是 P4 的行为。
fn load_asset(
    opts: &Options,
    manifest: &Manifest,
    body_dir: Option<&std::path::Path>,
) -> Result<(Option<Atlas>, Option<Regions>), String> {
    if opts.test_rect {
        return Ok((None, None));
    }
    let Some(path) = opts.manifest.as_deref() else {
        // 内置 fixture 只有元数据、没有像素，画不出任何东西
        return Ok((None, None));
    };
    let owned_dir;
    let dir = match body_dir {
        Some(d) => d,
        None => {
            owned_dir = std::path::Path::new(path)
                .parent()
                .ok_or_else(|| format!("{path} 没有父目录"))?
                .to_path_buf();
            &owned_dir
        }
    };
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

/// 配件库在哪 —— **只由环境决定，命令行没有第二个入口**（用户 2026-11-02 的原话"配件不是应该由
/// 菜单选择吗，这也不合理" ⇒ `--accessory` 整个删掉，穿哪件是屏上的活）。
///
/// 规则本身**不在这里**：它在 [`petstore::paths::AssetRoot`]，与本体侧 `OPET_PETS` 那一根、
/// 与安装器写入的那一根同源。以前这里是本仓库第二份"三级解析"的实现，而 §9.2 第 20 条
/// 那个"两块面互不相认"正是这么长出来的 ⇒ 现在只剩一份，这里只负责把它变成 `Option`。
///
/// 返回 `Option` 而不是直接 `PathBuf`：`$HOME` 都读不到（裸 `env -i` 的 CI）时**没有库**，
/// 不去猜一个根目录下的 `/.local/share`。那一档下换装页三行照样画出来、照样点得到，
/// 只是"穿上"打一行"库里没有可用配件"——**没有东西可穿不等于没有这个功能**。
fn accessory_root() -> Option<std::path::PathBuf> {
    petstore::paths::AssetRoot::resolve(petcore::gacha::ItemKind::Accessory).map(|r| r.path)
}

/// [`discover_accessories`] 的内层：一个目录 → 一件候选，或一句点名它错在哪的说明。
///
/// 单列成函数有两个理由：① `dir` 在这里只被**借走**（`&Path`），调用方随后还要拿它打"跳过谁"
/// 那行；写成闭包就得 move 进去，错误分支反而读不到名字。② `tests/accessory_product.rs` 要照
/// 同一套顺序走**真产物**，收一个 `&Path` 比收一整个库目录更好摆。
fn check_accessory(
    dir: &std::path::Path,
    body: &Manifest,
    body_regions: &Regions,
) -> Result<Candidate, String> {
    let path = dir.join("accessory.json");
    let name = path.to_str().ok_or("配件清单路径不是 UTF-8")?;
    let acc = AccessoryManifest::load(name).map_err(|e| format!("清单 {name} 加载失败: {e}"))?;
    acc.validate_against(body)
        .map_err(|e| format!("拒绝配件 {}（{e}）", acc.slug))?;
    let polygon = dir.join("polygon.json");
    let acc_regions = Regions::load(
        polygon.to_str().ok_or("配件 polygon 路径不是 UTF-8")?,
        acc.grid.w,
        acc.grid.h,
        SURFACE_MARGIN,
    )
    .map_err(|e| format!("{}: {e}", polygon.display()))?;
    // 配件**会被画到的每一格**都必须有自己的轮廓条目。缺格的后果不是报错，而是"那一格帽子照样
    // 上屏、而 `wl_region` 的并集里没有它" ⇒ 那块像素点不到（三条真机人工门禁守的正是这件事）。
    let cells = acc.drawn_cells();
    if let Some(missing) = acc_regions.first_missing(&cells) {
        return Err(format!(
            "配件 {} 的 polygon.json 没有格 ({},{})：清单声明了 {} 格而轮廓只有 {} 格 \
             ⇒ 那一格会画出点不到的像素",
            acc.slug,
            missing.0,
            missing.1,
            cells.len(),
            acc_regions.frame_count()
        ));
    }
    let merged = body_regions
        .merged_with(&acc_regions)
        .map_err(|e| format!("拒绝配件 {}: {e}", acc.slug))?;
    Ok(Candidate {
        // 身份 = **目录名**，不是 slug：库存的 `item_id`、P1 的 INV 门禁、`AssetDir::locate` 认的
        // 都是这一级（安装器落盘时也是按 `<id>/` 建目录）。slug 是给人看的名字。
        id: dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} 的目录名不是合法 UTF-8 ⇒ 无法与库存对齐", dir.display()))?
            .to_string(),
        slug: acc.slug.clone(),
        dir: dir.to_path_buf(),
        manifest: acc,
        merged,
    })
}

/// 启动时扫一遍配件库 ⇒ 换装页能选的那张表（**元数据 + 并集轮廓，一像素未解**）。
///
/// 四道门禁与旧的显式路径版同一套、同一顺序（便宜的在前），只把**第五道 = 解码**挪进
/// [`App::equip`]。这条不是洁癖：`VmHWM` 是**高水位**，启动时"每件解码一次再丢掉"与"全部常驻"
/// 给出同一个峰值（实测 59,164 kB，越过了用户签的未装备配件那条 57,344 kB）。
///
/// 顺序不是审美：`validate_against` 是纯元数据比对，而 sheet 解码是几十毫秒和几 MiB。把它放在
/// 门禁之前，等于让"格子尺寸对不上"这种一行就能判出来的错先付一次装载代价 —— 而那条错恰恰是
/// D16 路线 B 唯一真正防的东西（"偏移恒为 0"一旦被违反，屏上只是帽子画歪，永远不会报错）。
///
/// ⚠️ **这里一个 `Err` 都不出**，和显式路径那版最大的语义区别就在这里：库里一个坏文件的后果是
/// "少一件可穿"，不是"宠物起不来"（硬约束 3）。每条跳过打一行**点名是哪件、错在哪一道**的 stderr
/// ⇒ 少了什么用户看得见，而不是在屏上找一个不存在的行。
///
/// 生产者 = `tools/pack.py --accessory`（产 `accessory.json` + `sheet.webp` + `polygon.json`
/// 三件同目录）。这几道门禁在**真产物**上的证据 = `tests/accessory_product.rs`，它照本函数的
/// 顺序走一遍；那条 ⑤′ 把"帽子画出来了而点不到"钉成了像素与轮廓同空同满。
fn discover_accessories(
    root: &std::path::Path,
    body: &Manifest,
    body_regions: &Regions,
) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!(
                "accessories: 库 {} 打不开（{e}）⇒ 换装页「穿上」点了不会有效果",
                root.display()
            );
            return found;
        }
    };
    // 目录名参与排序：`read_dir` 的顺序由文件系统给，同一台机器上两次启动可能不同 ⇒
    // 换装页的"换一件"循环会把用户从一个配件直接跳到另一个不相干的配件。
    let mut dirs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("accessory.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        // —— 逐件把四道门禁走完；任一道的错都只让**这一件**出局，并点名它是哪一件。
        match check_accessory(&dir, body, body_regions) {
            Ok(cand) => {
                // 两个名字不一致 ⇒ 库存那本账（认目录名）与屏上那件（叫 slug）会对不上，而
                // "为什么这件我抽到了却不出现"是问不出来的。生产者不会产出这种包，手工塞的会，
                // 所以这一行的受众就是"今天这台机器"那个形状（§9.2 第 20 条）。
                if cand.id != cand.slug {
                    eprintln!(
                        "accessories:   ⚠ 目录名 {} 与清单 slug {} 不一致 ⇒ 库存按 {} 记账、屏上叫 {}",
                        cand.id, cand.slug, cand.id, cand.slug
                    );
                }
                // **覆盖哪些状态**必须打出来，而且缺的时候要把缺的点名。这条行是本轮唯一的
                // "为什么有些动作看不到帽子"的出口：清单只声明 `idle` 是一件**合法**的配件
                // （`covers()` 为假 ⇒ `blit_cell` 按设计不叠），而它上屏后的形状与"叠加坏了"
                // 一模一样，只有这一行分得开两者。
                let all = AssetKey::ALL;
                let have: Vec<&str> = all
                    .iter()
                    .filter(|k| cand.manifest.covers(**k))
                    .map(|k| k.as_str())
                    .collect();
                if have.len() == all.len() {
                    eprintln!(
                        "accessories:   {} 叠 {:?} 档 · 覆盖 {}/{} 状态 · 轮廓 {} 格 · sheet 未解（穿上才读）",
                        cand.slug,
                        cand.manifest.slot,
                        have.len(),
                        all.len(),
                        cand.merged.frame_count()
                    );
                } else {
                    let miss: Vec<&str> = all
                        .iter()
                        .filter(|k| !cand.manifest.covers(**k))
                        .map(|k| k.as_str())
                        .collect();
                    eprintln!(
                        "accessories:   {} 叠 {:?} 档 · 覆盖 {}/{} 状态 ⇒ **只有这 {} 档会看到它**（缺 {}）\
                         · 轮廓 {} 格 · sheet 未解（穿上才读）",
                        cand.slug,
                        cand.manifest.slot,
                        have.len(),
                        all.len(),
                        have.len(),
                        miss.join(","),
                        cand.merged.frame_count()
                    );
                }
                found.push(cand);
            }
            Err(err) => eprintln!(
                "accessories: 跳过 {}（{}）⇒ 这一件不进换装页，其余不受影响",
                dir.display(),
                err
            ),
        }
    }
    eprintln!(
        "accessories: 库 {} ⇒ {} 件可用{}",
        root.display(),
        found.len(),
        if found.is_empty() {
            "（「穿上」不会有效果；把 `pack.py --accessory` 的三件套放进这里）"
        } else {
            ""
        }
    );
    found
}

/// 本体库在哪 —— **只由环境决定**，与 [`accessory_root`] 是同一根规则的两个分支
/// （`petstore::paths::AssetRoot`：`OPET_PETS` → `$XDG_DATA_HOME/opet/pets` →
/// `$HOME/.local/share/opet/pets`）。**没有 `--pets` 命令行入口**：`--accessory` 已被用户删过
/// 的那条先例（"配件不是应该由菜单选择吗"），本体侧同理，库位置是装机事实不是用户意图。
fn pet_root() -> Option<std::path::PathBuf> {
    petstore::paths::AssetRoot::resolve(ItemKind::Pet).map(|r| r.path)
}

/// 收藏册里一只**换得上去**的本体 = 一个通过全部发现门禁的目录（**一像素未解**）。
///
/// 与 [`Candidate`] 同族，但多一个 `leaked`：`Sim<'m>` 与 [`App::manifest`] 的类型是
/// `&'static Manifest`，而这张表是**自己拥有**清单的 `Vec` ⇒ 借用活不到那一刻。
/// `load_manifest`（`:1390`）那边是直接 `Box::leak` 一份，这里照同一条策略，但**延到第一次
/// 真要换上它时才泄**并通过 [`BodyCandidate::leaked`] 缓存 ⇒ 一份进程内每只不同的宠物只泄一份
/// （几 KB），不是每次换都泄一份。
/// （不可行的另一条路：把清单存进一个 self-referential arena —— `QueueHandle<App>` 要求
/// `App: 'static`，任何"挂在 `App` 里的借用"都构造不出来。）
struct BodyCandidate {
    /// 库存那一侧的身份 = **库目录名**（与 [`Candidate::id`] 同一条既有裁定：`inventory.item_id`、
    /// P1 的 INV 门禁、`AssetDir::locate`、`equipped` 的 `body` 行认的都是这一个）。
    id: String,
    /// 清单里的 `slug` = `fits` 比的那个名字。⚠️ 它与 [`Self::id`] **可以不相等**，见
    /// [`discover_pets`] 那行 ⚠（今天四份 @192 产物里三份的 slug 写着 `cat_shiro` 就是这一格）。
    slug: String,
    dir: std::path::PathBuf,
    grid: Grid,
    manifest: Manifest,
    /// 本体自己的穿透轮廓（发现第 ④ 道解的，见 [`check_body`]）。
    ///
    /// **存下来而不是换的时候再解一次**：`App::swap_body` 的第 3 步于是没有任何 IO、不可能失败，
    /// "新清单 + 旧轮廓"这个半换形状就构造不出来。代价是每只几 KB，相对一张 sheet 的 45 MiB 是零。
    regions: Regions,
    leaked: Option<&'static Manifest>,
}

impl BodyCandidate {
    /// 第一次真要换上它时才 `Box::leak`，之后返缓存（字段说明里那三条理由）。
    fn leaked(&mut self) -> &'static Manifest {
        match self.leaked {
            Some(m) => m,
            None => {
                let m: &'static Manifest = Box::leak(Box::new(self.manifest.clone()));
                self.leaked = Some(m);
                m
            }
        }
    }

    /// 那只本体的 sheet 路径（`Atlas::reload` 的唯一输入，**在**门禁之内**不在**扫盘里读）。
    fn sheet(&self) -> std::path::PathBuf {
        self.dir.join("sheet.webp")
    }
}

/// [`discover_pets`] 的内层：一个目录 → 一只可换的本体，或一句点名它错在哪的说明。
///
/// 门禁按**便宜**排序（清单 → 档 → 存在性 → 轮廓），一像素都不解。与 [`check_accessory`]
/// 同族，**两处故意不同**：
///
/// ② **格子档整块比较**（`Grid: PartialEq` 逐字段含 `cols/rows/w/h`），判据不是只比 `w/h`：
///    `rows` 决定解码缓冲大小与 polygon 面，`cols` 决定寻址。跨档在**扫盘这一步**出局 ⇒
///    收藏册那一行是只读、没有强调条（= P4 立好的形状，不是新发明）。为什么不在点击时报错：
///    `layer.set_size` / `viewport.set_destination` / `SlotPool` 那三处只在 `run()` 里发生一次，
///    运行时换档要走一遍 configure 往返；**启动允许跨档（surface 还没建），运行时不允许** ——
///    这条不对称是协议限制不是数据模型限制。
///
/// ④ **没有**配件侧那道 `first_missing` 逐格轮廓覆盖检查。本体的 `region_rects` 有设计好的
///    "整格兜底"，缺一格只是"点击面积稍大"；配件缺格是"帽子画在 region 外"。在这里照抄过来
///    就是又一条**假门禁**（它会拒绝一堆画得对的本体）。
fn check_body(dir: &std::path::Path, cur: &Grid) -> Result<BodyCandidate, String> {
    let path = dir.join("pet.json");
    let name = path.to_str().ok_or("本体清单路径不是 UTF-8")?;
    // ① 清单本身。`Manifest::validate` 已覆盖 format / slug / `cols==8` / 格像素 / sheet 像素 /
    //    `idle` 必在 / 状态名 / rows 越界 / fps / `anchor.feet` 在框内 / `outfit_anchors` /
    //    `input_polygon` / facing ⇒ 这里不重复任何一道。
    let manifest = Manifest::load(name).map_err(|e| format!("清单 {name} 加载失败: {e}"))?;
    // ② 格子档（拍板② 的落点）。
    if manifest.grid != *cur {
        let (a, b) = (cur, &manifest.grid);
        return Err(format!(
            "拒绝本体 {}：格子档不同（当前 {}×{}、{} 列 × {} 行；这一份 {}×{}、{} 列 × {} 行）\
             ⇒ 运行时换本体不动 surface 尺寸，跨档只能重启",
            manifest.slug, a.w, a.h, a.cols, a.rows, b.w, b.h, b.cols, b.rows
        ));
    }
    // ③ 两件伴生产物**存在**（不读内容 ⇒ 把 `discovery_never_touches_the_sheet…` 那条扩到宠物）。
    for what in ["sheet.webp", "polygon.json"] {
        if !dir.join(what).is_file() {
            return Err(format!("拒绝本体 {}：目录里没有 {what}", manifest.slug));
        }
    }
    // ④ 轮廓解得开（`Regions::load` 自己会交叉核对 polygon 的格尺寸 vs 清单）。
    let polygon = dir.join("polygon.json");
    let regions = Regions::load(
        polygon.to_str().ok_or("本体 polygon 路径不是 UTF-8")?,
        manifest.grid.w,
        manifest.grid.h,
        SURFACE_MARGIN,
    )
    .map_err(|e| format!("{}: {e}", polygon.display()))?;
    Ok(BodyCandidate {
        // 身份 = **目录名**，与配件侧同一条裁定（理由见 [`Candidate::id`] 那段）。
        id: dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} 的目录名不是合法 UTF-8 ⇒ 无法与库存对齐", dir.display()))?
            .to_string(),
        slug: manifest.slug.clone(),
        dir: dir.to_path_buf(),
        grid: manifest.grid,
        regions,
        leaked: None,
        manifest,
    })
}

/// 启动时扫一遍本体库 ⇒ 收藏册里**点得动**的那些宠物行（元数据 + 轮廓，一像素未解）。
///
/// 与 [`discover_accessories`] 同一条最要紧的语义：**这里一个 `Err` 都不出**。库里一只坏本体的
/// 后果是"少一只可换"，不是"宠物起不来"（硬约束 3），每条跳过打一行点名它错在哪道的 stderr。
///
/// ⚠️ 起点本体**不要求**在这张表里：`--manifest` 仍是硬要求（`cli.rs:80` 不动），它退化成
/// "库里没那一行时用什么"（拍板③）。所以这张表空 ⇒ 收藏册的宠物行全部只读，那是**正确行为**。
fn discover_pets(root: &std::path::Path, cur: &Grid) -> Vec<BodyCandidate> {
    let mut found: Vec<BodyCandidate> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!(
                "pets: 库 {} 打不开（{e}）⇒ 收藏册里的宠物行点了不会有效果",
                root.display()
            );
            return found;
        }
    };
    // 排序理由与配件侧逐字相同：`read_dir` 的顺序由文件系统给，两次启动可能不同。
    let mut dirs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("pet.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        match check_body(&dir, cur) {
            Ok(cand) => {
                // 与配件侧同一行 ⚠（`:1557`）：不一致就是"库存按 A 记账、屏上叫 B"，而
                // "为什么这只我抽到了却不让我换"问不出来。**拒绝文案点的是 `fits` 或档，不是这个。**
                if cand.id != cand.slug {
                    eprintln!(
                        "pets:   ⚠ 目录名 {} 与清单 slug {} 不一致 ⇒ 库存按 {} 记账、屏上叫 {}",
                        cand.id, cand.slug, cand.id, cand.slug
                    );
                }
                eprintln!(
                    "pets:   {}（{}）换得上去 · {}×{} {}列×{}行 · 轮廓 {} 格 · sheet 未解（点上才读）",
                    cand.id,
                    cand.slug,
                    cand.grid.w,
                    cand.grid.h,
                    cand.grid.cols,
                    cand.grid.rows,
                    cand.regions.frame_count()
                );
                found.push(cand);
            }
            Err(err) => eprintln!(
                "pets: 跳过 {}（{}）⇒ 这一只在收藏册里是只读行，其余不受影响",
                dir.display(),
                err
            ),
        }
    }
    eprintln!(
        "pets: 库 {} ⇒ {} 只可换{}",
        root.display(),
        found.len(),
        if found.is_empty() {
            "（宠物行全部只读；把 `tools/pack.py` 的三件套放进这里）"
        } else {
            ""
        }
    );
    found
}

/// 起点本体的身份 = `--manifest` 那个路径的**目录名**（`id = 目录名`那条既有裁定，与收藏册
/// 那一行同认一个 id ⇒ `same_id` 判据才有读数）。内置 fixture 不读盘 ⇒ 没有目录名可取，
/// 退化成清单 `slug`，而且**在这里**说清，不留到屏上去猜。
fn start_body_id(manifest_path: Option<&str>, slug: &str) -> String {
    manifest_path
        .and_then(|p| std::path::Path::new(p).parent())
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .map(String::from)
        .unwrap_or_else(|| slug.to_string())
}

/// **到这一步才解码**第二张 sheet ⇒ [`App::equip`] 是内存档 M1 唯一的执行点。
///
/// 只剩一道可能失败 = sheet 读不开。轮廓并集已经在 [`discover_accessories`] 算过并存的
/// [`Candidate::merged`] 里，所以"点了穿上而屏上没帽子"与"点了穿上而帽子点不到"这两种形状
/// 在这一层都构造不出来（后者正是上面那条 `first_missing` 守的东西）。
fn decode(cand: &Candidate) -> Result<Equipped, String> {
    let sheet = cand.dir.join("sheet.webp");
    let acc = &cand.manifest;
    let atlas = Atlas::load(
        sheet.to_str().ok_or("配件 sheet 路径不是 UTF-8")?,
        acc.grid.cols as u32,
        acc.grid.rows as u32,
        acc.grid.w,
        acc.grid.h,
    )
    .map_err(|e| format!("{}: {e}", sheet.display()))?;
    eprintln!(
        "accessory: 穿上 {}（{}x{} {} KB → 解码 {} ms / {:.3} MiB）",
        acc.slug,
        atlas.width,
        atlas.height,
        atlas.encoded_bytes / 1024,
        atlas.decode_ms,
        atlas.decoded_bytes() as f64 / (1024.0 * 1024.0),
    );
    Ok(Equipped {
        manifest: acc.clone(),
        atlas,
    })
}

/// 把"这一格该画什么"从 wayland 的 buffer 里剥出来，写进调用方给的字节切片。
///
/// 为什么是自由函数：它**不碰任何 wayland 类型**（`dst` 就是 `&mut [u8]`），而它做的两个决定
/// —— "没烘过这档状态就不叠"与"叠加失败 ⇒ 整格作废" —— 恰恰是 headless 唯一能验的那部分。
/// 留在 `draw_cell` 里，这两条就只能靠"代码写在那儿"，而本项目两条 P0 全是这种形状炸出来的。
///
/// `body` / `worn` 都是 `Option`：`--test-rect` 那一档两份图集都没有（画纯色矩形），
/// 没装备配件时只有本体。顺序恒为"本体先覆盖、配件后 source-over"，反过来帽子会变透明底。
///
/// 配件那支的 `covers(key)` 那道门是**数据决定的可见性**：一份只声明 `idle` 的配件，在
/// `walk` 那 4 行就是不叠。这不是 bug 也不是"没画出来"，所以它必须在启动时被读出来 ——
/// 见 [`discover_accessories`] 那行 `覆盖 N/9 状态`。
// 参数表 = `Atlas::blit` 的几何六项 + 两个来源 + 一个 `AssetKey`；把它压成结构体只是把
// lint 的门槛挪个地方，而 `cell` 与 `dst_wh` 同为 `(u32, u32)` 反而多出一个可静默对调的形状。
#[allow(clippy::too_many_arguments)]
fn blit_cell(
    dst: &mut [u8],
    body: Option<&Atlas>,
    worn: Option<&Equipped>,
    cell: (u32, u32),
    key: AssetKey,
    cell_w: u32,
    cell_h: u32,
    stride: usize,
) -> Result<(), String> {
    if let Some(atlas) = body {
        atlas
            .blit(cell.0, cell.1, dst, cell_w, cell_h, stride)
            .map_err(|e| format!("blit ({},{}) 失败: {e}", cell.0, cell.1))?;
    }
    if let Some(acc) = worn.filter(|a| a.manifest.covers(key)) {
        acc.atlas
            .blit_over(cell.0, cell.1, dst, cell_w, cell_h, stride)
            .map_err(|e| format!("配件叠加 ({},{}) 失败: {e}", cell.0, cell.1))?;
    }
    Ok(())
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
    /// 当前页的布局表：`hit_test` 与 draw 共用同一份矩形。**"只算一次"现在是"每翻一页算一次"**
    /// —— 分页把这里变成了一件有状态的东西：`items` 必须与 `page` 同步，否则命中表是这一页、
    /// 画出来是那一页（这正是 `69fd158` 那条"签名与下游读的字段不配对"的第三种形状）。
    /// 唯一改 `page` 的地方是 `on_menu_press` 的 `Page` 臂，它同时重建 `items`。
    items: Vec<ItemRect>,
    /// 现在显示哪一页。**必须进 `content_sig` 且必须被 `card::Ctx` 读到**（两件事缺一半都不行，
    /// 理由逐条见 `menu::content_sig` 与 `card::Ctx::page` 上那两段注释）。
    page: menu::MenuPage,
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
    /// 从 `App` 抄来的**开发态豁免**标（`--allow-unowned-accessories` 且真有 `--db`）。与
    /// `dry_run` 同族：每进程恒定 ⇒ 不进 `content_sig`，但帽子页标题必须读得到它，否则屏上就
    /// 少一个"这一轮穿的是没抽到的东西"的记号 = 假门禁（`--cell` 假开关那一族）。
    ///
    /// ⚠️ 演示态（无 `--db`）**不点这个**：那一档右端已有「演示」标，同一件事给两个记号就是
    /// 两个事实源。点亮与否由 [`App::dress_exempt`] 一处给结论，别在这里重算条件。
    dress_mark: bool,
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
        let dry_run = self.dry_run;
        let balance = self.balance;
        let motion = self.motion;
        let lang = self.lang;
        let page = self.page;
        // 与 `dry_run` 同族：每进程恒定 ⇒ 不进签名，但必须在绘制前取出来（`self.pool` 那步是
        // 可变借用，同一个表达式里不能再不可变借 `self`）。
        let unowned_exempt = self.dress_mark;
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
                page,
                unowned_exempt,
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

// ───────────────────────────── 面板 surface（裁定④）─────────────────────────────

/// 点收藏册 / 设置那一类"要看内容、要能操作"的菜单项时开出的**第三块** layer surface。
///
/// **为什么要有第三块而不是把菜单撑大**：菜单那张 240×150 的卡片是**几何证明**，不是审美
/// 选择 —— `ROW_STEP 30 × 3 + HEAD_H 22 + PAD 8 = 116`，footer 从 124 起 ⇒ 只剩 8 px，第四行
/// 物理放不下（设计书 §6.1.6）。收藏册一屏要 5 行内容 + Close，硬塞进菜单就是在改那道证明。
/// 所以菜单保持六页三行，面板另开一面 **240×214**（同宽 ⇒ `line_budget_px`/`fit`/`MAX_LINE_PX`
/// 逐字复用，只有高度是新的）。
///
/// **为什么它与菜单互斥**：同层两块以上 surface 的堆叠序在协议里**没有定义**（`hyprctl layers`
/// 认得出 namespace 而认不出谁压谁），而菜单 + 面板 + 宠物三块同屏会读不出"我点的是哪一面"。
/// 互斥换来的另一件事更重要：面板不会盖住宠物导致那三条 input-region 人工门禁判不了。
///
/// **为什么它自己一套 pool/在途标记/看门狗/region**：与 [`MenuState`] 同一条理由，而这里
/// 更硬 —— 面板的 `content_sig` 与菜单的完全不同族（没有相位、没有余额），共用池子就等于
/// 两块面互相"替对方"放行帧回调，`菜单: N 提交` 与 `面板: N 提交` 两个读数同时作废。
///
/// 与 [`MenuState`] 相比**去掉的四个字段各自的理由**（写出来是为了下次有人想加回来时
/// 知道那不是漏了）：
/// - **`anim`**：面板没有开箱相位 ⇒ 没有 burst、没有白闪、没有 `step()`。`card::paint_panel`
///   因此压根不接 `&MenuAnim`。
/// - **`page`**：面板的"翻页"是 [`panel::PanelKind`] 里的 tab，而且**只有收藏册有**。溢出
///   不靠翻页解决而是靠截断 + 一行「+N 更多」（[`panel::collection_rows`]），因为页码要进
///   标题带、标题带要进命中表、命中表要跟着变 —— 那是菜单分页那一整族复杂度，而这里
///   一屏装得下 5 行数据，剩下的本来就不该在 214 px 里翻。
/// - **`balance`**：余额**只留在菜单 footer 那一行**。同一张桌面上同一个数印两处 = 两个
///   事实源，而"事件驱动、不轮询"那条纪律下这两处刷新的时刻还不一样（菜单在开菜单/抽卡后，
///   面板在开面板/换 tab 后）⇒ 会出现"面板上的余额是旧的"这种读起来像 bug 的东西。
/// - **`dry_run` / `motion`**：`dry_run` 是演示标，面板上不印它。**`motion` 有意不是字段**：
///   `panel::settings_rows` 已经把 `motion.code()` 烘进行文案里，而**行文案进签名** ⇒ 再存
///   一份平行字段就是"字段存在、签名不读它、画师也不读它"那一族漂移源（`69fd158` 的形状）。
struct PanelState {
    layer: LayerSurface,
    /// `frame` / 指针事件按**对象身份**分流（第四路），这块要能被认出来
    surface: WlSurface,
    viewport: Option<WpViewport>,
    pool: SlotPool,
    /// 宠物 scale 的镜像；面板自己不发 `preferred_scale`，只能镜像（同 [`MenuState`]）
    scale_num: u32,
    logical: (i32, i32),
    /// 现在哪一本（收藏册第几 tab / 设置）。它决定**标题带那串字**（[`panel::header_text`]）
    /// 与**标题带那一格点下去是什么**（收藏册换 tab / 设置重载默认行，见
    /// [`App::on_panel_press`]），所以**必须进签名**。
    kind: panel::PanelKind,
    /// 这一面的内容。**唯一的一份**：签名比它、命中表由它派生、像素画它，三者同源。
    rows: Vec<panel::PanelRow>,
    /// 命中表 = `panel::rects(kind, rows)`，与 `rows` **同生同改**（[`update_panel`]、
    /// [`on_panel_press`] 里换 tab 的那一支是唯一改 `kind` 的地方，它同时重建这两个）。
    items: Vec<panel::RowRect>,
    /// 从 `App.ui.lang` 抄来的当前语言。与菜单同族：**运行中可变**（面板里那一行点下去
    /// 就换）⇒ 进签名，且必须被画师读到。
    lang: Lang,
    /// 上一次 commit 的内容签名 ⇒ 静止时"什么都不发"
    last_sig: Option<u64>,
    frame_pending: bool,
    pending_since: Option<Instant>,
    active_region: Option<WlRegion>,
    configured: bool,
    force_draw: bool,
}

impl PanelState {
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

    /// 宠物 scale 变了要跟着重画（面板自己不发 `preferred_scale`，只能镜像）。
    fn mirror_scale(&mut self, scale_num: u32) {
        if self.scale_num != scale_num {
            self.scale_num = scale_num;
            self.force_draw = true;
        }
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

    /// 收回协议对象。顺序与 [`MenuState::teardown`] 逐字一致：region → viewport →
    /// layer_surface → wl_surface（`zwlr_layer_surface_v1.destroy` 之后再碰它就是协议错误）。
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

    /// 出一帧面板。返回是否真的 commit 了。
    ///
    /// **没有 `step()`**：这一面没有任何随时间自己变的像素（相位表在菜单那一面），所以
    /// "每 tick 都投一帧完全相同的 buffer"在这里连形状都不存在 —— 签名里没有任何随时间的量。
    fn draw(&mut self, compositor: &CompositorState, qh: &QueueHandle<App>, error: &str) -> bool {
        if !self.configured || self.frame_pending {
            return false;
        }
        let sig = panel::content_sig(self.kind, self.lang, error, &self.rows);
        if self.last_sig == Some(sig) && !self.force_draw {
            return false;
        }
        let kind = self.kind;
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
                eprintln!("面板 create_buffer 失败: {e:?}");
                return false;
            }
        };
        // `rows` 走引用过去（`PanelCtx::rows` 是 `&'a [PanelRow]`）：这里必须**不**先把
        // `self.rows` 拷一份出来，否则"签名比的那串字"与"画的那串字"就真是两份了。
        card::paint_panel(
            &mut canvas[..],
            bw,
            bh,
            stride as usize,
            card::PanelCtx {
                kind,
                lang,
                rows: &self.rows,
                error,
                scale,
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
        // 与菜单同理：整块都该吃掉点击（它就是来接收点击的），region = 卡片矩形，**逻辑**坐标
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
        self.output_available_for(CARD_W, CARD_H)
    }

    /// [`output_available`](Self::output_available) 的按尺寸版：把"这块卡片"当成参数而不是
    /// 写死菜单的 240×150。
    ///
    /// **为什么必须 generalize 而不是给面板另写一份**：面板比菜单高 64 px（214 vs 150），
    /// 而这里那两条 `max` 是**把卡片塞进可用区**的唯一防线 —— 复制一份就会有两处 clamp
    /// 口径，改一处漏一处时面板会"合规地"贴出屏外，而那只在真机上看得出来。
    /// `card_w`/`card_h` 同时也喂给兜底的 `unwrap_or`：读不到 output 几何时（第一 tick
    /// 之前）兜的底必须按**这一块**的尺寸，不是按菜单的。
    fn output_available_for(&self, card_w: i32, card_h: i32) -> (i32, i32) {
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
            .unwrap_or((card_w.max(self.logical.0), card_h.max(self.logical.1)));
        (
            w.max(self.logical.0).max(card_w),
            (h - self.opts.panel_top.round() as i32)
                .max(self.logical.1)
                .max(card_h),
        )
    }

    /// 右键 = 开/关"当前开着的那一面"。三块面里宠物自己不是面，所以这里只有两种状态：
    /// 菜单开着 ⇒ 关菜单；**面板开着 ⇒ 关面板**（不是"关掉面板再顺手把菜单开出来"——
    /// 那会让"再右键一次就关"这条既有语义变成"右键三次换两面"）。都没有 ⇒ 开菜单。
    ///
    /// 这条三分支同时是**互斥那条不变量的唯一守卫**：[`App::open_panel_lines`] 会先关菜单，
    /// 而这里保证 `open_menu` 永远不会在面板开着时被叫到（否则菜单不带关面板 ⇒ 两块面同屏）。
    fn toggle_menu(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
        } else if self.panel.is_some() {
            self.close_panel();
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
        // 豁免标记在**开菜单**这一刻就点亮，不等用户点某一行：它是两个启动参数的函数，
        // 不读库存 ⇒ 不需要 `dress_view` 那次查询。要读库的是那道过滤，不是这个记号。
        // 晚一步点亮就会出现"翻页翻到帽子页看不到 `*`，点一下换一件才冒出来"——
        // 而那正是"屏上读不出这轮是豁免态"的假门禁形状。
        let dress_mark = self.dress_exempt();
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
        // 右键就想要抽卡 ⇒ 菜单**固定从盲盒页起**，不记住上一次翻到哪。真记住的话就成了
        // 一个运行中状态，而它既不进签名也不进任何持久化 = 一个没人负责的漂移源。
        let page = menu::MenuPage::Gacha;
        self.menu = Some(MenuState {
            layer,
            surface,
            viewport,
            pool,
            scale_num,
            logical,
            items: menu::rects(page),
            page,
            anim: MenuAnim::Menu,
            balance,
            dry_run,
            motion,
            lang,
            dress_mark,
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

    // ───────────────────────── 面板（第三块 surface）─────────────────────────

    /// 面板**唯一**的行生产者。
    ///
    /// ⚠️ **不许在这里调 [`Self::ensure_gacha`]**：连不上库那件事 `ensure_gacha` →
    /// [`Self::gacha_bailed`] 已经出过一次声，这里再返回 `Err` 让调用方处理一遍，就是
    /// **同一次失败打两遍**（两条 stderr、错误行被覆写两次，而用户看不出哪条是真的）。
    /// 所以这条只问"已经连上了没有"，连不连是调用方的责任。
    ///
    /// 设置页不碰库 ⇒ 它永远 `Ok`；`Err` 只可能来自收藏册那三个 tab，而那一路唯一的使用者
    /// 是 [`Self::refresh_panel_rows`]（面板开着时换语言/换动画档），那里库必然已经连着。
    ///
    /// ⚠️ 「已获得」那一本**忽略传进来的 `page` 之外的一切**：它不走 [`Gacha::tab_lines`]，
    /// 因为它的行不是"一串字"而是"一串带下标的可点条目"，分页与归一都在 [`Self::album_screen`]
    /// 里。另两个 tab 保持 [`panel::collection_rows`]（非分页生产者的保险，见那条的注释）而
    /// **读不到 `page`** —— `PanelItem::More` 只可能由 [`panel::album_page`] 产在 Owned 那一本，
    /// 所以"传了 page 而没人读"在这一支是结构上不可达的，不是口子。
    fn panel_rows(&mut self, kind: PanelKind) -> Result<Vec<panel::PanelRow>, String> {
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
                    &g.tab_lines(tab, lang, self.scale_f())?,
                ))
            }
        }
    }

    /// 「已获得」那一本的**一整屏** = 逐件行 + 汇总那五行 → 分页。返回**归一后的** `kind` 与行。
    ///
    /// 为什么返回 `kind` 而不是只返回行：页码归一（末页回卷）只由 [`panel::album_page`] 做，
    /// 而存进 [`PanelState`] 的那一格必须就是归一后的页码 —— 存原始请求值的话，
    /// `on_panel_press` 的 `kind.page() + 1` 会在回卷之后继续往上加，页码从此漂掉，
    /// 屏上却永远只有那几页（"两个事实源"的又一形状）。
    ///
    /// ⚠️ 汇总那五行是**追加**在逐件行之后，不是被替换掉：`have/all` 与碎片余额只有那一行
    /// 说得了，而逐件行回答的是"哪一件、能不能点"。代价（明写）= 汇总被挤到后面的页，
    /// 首屏第一眼是逐件列表；要反过来就一行改（把汇总放到 `rows` 头部）。
    ///
    /// 库存这一趟读了三遍（[`Self::dress_view`] 内部一遍、[`Self::pet_view`] 内部一遍、这里一遍）。
    /// 为什么不合成一遍：
    /// 让 `DressView` 把 `Owned` 带出来会搅浑"豁免态压根不该读库"那条形状，而这一屏要的
    /// 是**报表**（豁免态照样得知道有什么）。三遍读的是同一个 store、同一次点击之内 ⇒ 是
    /// 同一个事实问了三次，不是三个事实源；代价 = 三次 sqlite 查询，而它们不在 tick 环上
    /// （§4 禁的是定时轮询）。
    fn album_screen(&mut self, page: usize) -> Result<(PanelKind, Vec<panel::PanelRow>), String> {
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
        let (lang, scale) = (self.ui.lang, self.scale_f());
        let g = self
            .gacha
            .as_ref()
            .ok_or_else(|| "库还没连上（收藏册读不到东西）".to_string())?;
        let owned = g.store.owned().map_err(|e| format!("读库存失败: {e}"))?;
        let mut rows = album_rows(
            &g.album_entries(&owned, &ids, &view.visible, &pet_ids, &pet_view.visible)?,
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

    /// 开面板 = **先关菜单**（两块面永不同时在场）+ 按 [`Self::panel_rows`] 现算一行。
    ///
    /// 收藏册那三行**不走这里**，走 [`Self::collection_panel`] —— 同一条路还要把同一批字
    /// 打进 stderr，而这里不碰库。
    fn open_panel(&mut self, kind: PanelKind) {
        match self.panel_rows(kind) {
            Ok(rows) => self.show_panel(kind, rows),
            Err(msg) => self.gacha_bailed(msg),
        }
    }

    /// 让面板显示这一组行：**没开就建那块面，开着就原地换行 + 换命中表**。
    ///
    /// 两个动作合成一条不是省事：「点标题带换 tab」与「从菜单进来」要办的是**同一件事**
    /// （这一面此刻该画什么），分成两条路径就会出现其中一条**忘了同步 `items`** —— 而
    /// `items` 是点击的唯一依据（`69fd158` 那一族"命中表与画面分家"的形状）。
    ///
    /// 原地换行**不置 `force_draw`**：`kind`、`lang`、每行文本、错误行全在
    /// [`panel::content_sig`] 里 ⇒ 签名自己就认得出变了（`cycle_page` 那条注释立的纪律）。
    fn show_panel(&mut self, kind: PanelKind, rows: Vec<panel::PanelRow>) {
        if let Some(p) = self.panel.as_mut() {
            let items = panel::rects(kind, &rows);
            p.kind = kind;
            p.lang = self.ui.lang;
            p.rows = rows;
            p.items = items;
            return;
        }
        self.open_panel_lines(kind, rows);
    }

    /// 建那块面（[`App::open_menu`] 的同形物）。行由调用方给 ⇒ 这一条**不碰库、不读行**，
    /// 只做协议：算摆位、建 surface/layer/viewport/shm 池，然后交给 [`PanelState`]。
    fn open_panel_lines(&mut self, kind: PanelKind, rows: Vec<panel::PanelRow>) {
        // 互斥：菜单与面板永不同时在场（同层两块以上的堆叠序协议里没定义，见 `panel` 模块头）。
        self.close_menu();
        let qh = self.qh();
        // 几何先算完（下面建 surface 全是不可变借用，别和 `self.panel =` 的可变借用打架）
        let (mx, my) = self.margin;
        let pet = RectI::new(mx, my, self.logical.0, self.logical.1);
        let (aw, ah) = self.output_available_for(panel::CARD_W, panel::CARD_H);
        let at = panel::placement(pet, RectI::new(0, 0, aw, ah));
        let scale_num = self.scale_num;
        let has_viewport = self.viewporter.is_some();
        let target = self.on_output.clone();
        let logical = (panel::CARD_W, panel::CARD_H);
        let phys = (
            to_phys_of(logical.0 as f32, scale_num, has_viewport).max(1),
            to_phys_of(logical.1 as f32, scale_num, has_viewport).max(1),
        );
        let lang = self.ui.lang;
        // 命中表与行**同源**：都从这一份 `rows` 派生（`MenuState.items` 那条"每翻一页算一次"
        // 的同一条约定，而面板一页里换 tab 也算一次）。
        let items = panel::rects(kind, &rows);

        let surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(
            &qh,
            surface.clone(),
            Layer::Overlay,
            // 独立 namespace：`hyprctl layers` 要能分别认出三块
            Some("opet-panel"),
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
                eprintln!("面板 shm 池建不起来: {e:?}");
                if let Some(v) = &viewport {
                    v.destroy();
                }
                surface.destroy();
                return;
            }
        };
        self.panel = Some(PanelState {
            layer,
            surface,
            viewport,
            pool,
            scale_num,
            logical,
            kind,
            rows,
            items,
            lang,
            last_sig: None,
            frame_pending: false,
            pending_since: None,
            active_region: None,
            configured: false,
            force_draw: true,
        });
        eprintln!(
            "panel: 打开于 margin {}x{}（{kind:?}，宠物 {mx},{my} {}x{}，可用区 {aw}x{ah}）",
            at.0, at.1, self.logical.0, self.logical.1
        );
    }

    fn close_panel(&mut self) {
        if let Some(p) = self.panel.take() {
            p.teardown();
        }
    }

    /// 每 tick 走一遍：同步 scale、跑看门狗、按签名决定要不要提交。
    /// **没有 `step()`** —— 这一面没有任何随时间自己变的像素，所以"停留期恒 0 提交"
    /// （`counters.panel_commits` 不随 tick 增长）是它唯一的省电判据。
    ///
    /// 放在宠物 `!configured` 早退**之前**（同 [`Self::update_menu`]）：宠物的映射状态和
    /// 面板该不该动帧毫无关系。
    fn update_panel(&mut self, now: Instant) {
        if self.panel.is_none() {
            return;
        }
        let scale_num = self.scale_num;
        let error = self.error_line();
        let mut p = self.panel.take().expect("上面查过 is_some");
        p.mirror_scale(scale_num);
        if p.tick_watchdog(now) {
            self.counters.panel_watchdogs += 1;
            eprintln!("警告：面板帧回调超时 ⇒ 强制解锁提交");
        }
        let qh = self.qh();
        if p.draw(&self.compositor, &qh, &error) {
            self.counters.panel_commits += 1;
        }
        self.panel = Some(p);
    }

    /// 收藏册那三行**与面板里点标题带**共用的入口：读行 → 打 stderr → 上屏。
    ///
    /// 一条而不是两条的理由是 §4 那条口径：**屏上那几行就是日志里那几行**。分两条就会出现
    /// "日志打了 5 行而屏上截断成 +N 更多"这种两边都自称正确的东西。
    ///
    /// ⚠️ 「已获得」那一本**从这里岔出去**走 [`Self::collection_album`]：它的行不是"一串字"
    /// 而是"一串带 candidates 下标的可点条目"，`Vec<String>` 那个中间层装不下它（装得下也要
    /// 再造一张"第几行 → 第几件"的旁表 = `69fd158` 那一族）。两条路的**出声编配**是同一套
    /// （先 `ensure_gacha`、失败只写错误行、成功清错误行、日志 == 屏上）。
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

    /// 「已获得」那一本（逐件 + 分页）的入口 = [`Self::collection_panel`] 的 Owned 分支。
    ///
    /// `page` 是**请求的**页码，可以越界（末页点一下就是下一页）；归一在
    /// [`panel::album_page`] 里做，而这里把它归一后的结果原样交给 [`Self::show_panel_rows`]
    /// —— 传请求值上屏就是"页码漂掉"那条形状，见 [`Self::album_screen`] 那条注释。
    fn collection_album(&mut self, page: usize) {
        if !self.ensure_gacha() {
            return;
        }
        let screen = self.album_screen(page);
        self.show_panel_rows("collection", screen);
    }

    /// 收藏册 / 复核那几行"连库 → 读 → 上屏"的唯一路径。
    ///
    /// 与上一批的 `gacha_report` 的差别只有一件，而那是这一批的核心：那些行不再**只**进
    /// stderr，它们现在**就是像素**。所以原先挂在这一条上那句
    /// ⚠「不置 `force_draw`：这些行进的是 stderr，一行像素都不动」**作废** ——
    /// 行文本进 [`panel::content_sig`]，闸门认得出，不需要调用方补"记得重画"。
    ///
    /// 三件事仍由这一条保证：**先** `ensure_gacha()`（lazy 连库那条红线，D14）、失败只写
    /// 错误行（[`Self::gacha_bailed`]，不 panic 不置 `exit`，硬约束 3 同族）、成功就把错误行
    /// 清空（否则上一条失败的字一直挂着，用户会以为它还没修）。
    ///
    /// `scale` 在**这里**取（不是各分支自己取）：它决定"一行的字装不装得下"，而这一条路径是
    /// 收藏册三屏唯一的出口 ⇒ 读屏那一步拿到的缩放与画那块面用的是同一个数。
    fn gacha_panel(
        &mut self,
        prefix: &str,
        kind: PanelKind,
        report: impl FnOnce(&Gacha, Lang, f32) -> Result<Vec<String>, String>,
    ) {
        if !self.ensure_gacha() {
            return;
        }
        let lang = self.ui.lang;
        let scale = self.scale_f();
        let lines = match self.gacha.as_ref() {
            Some(g) => report(g, lang, scale),
            None => return,
        };
        self.show_panel_lines(prefix, kind, lines);
    }

    /// 面板那两半的后半（行已经算好 → 打日志 → 上屏）的**行版**。
    ///
    /// ⚠️ 逐行打的时候**跳过 [`panel::PanelItem::Close`]**：那一格是"关闭"这个动作，不是一行
    /// 内容，而既有那几屏（`Missing`/`Recent`/概率/复核）的生产者最多给 5 行 ⇒ 加进 `rows` 的
    /// 只有它 ⇒ 不打它就等于**旧路径的 stderr 文本一个字节都不变**。这条不是洁癖：日志与屏上
    /// 同字是 §4 的口径，而"关闭"从来没进过日志，现在也不该进。
    ///
    /// `Err` 只走 [`Self::gacha_bailed`]（stderr + 错误行），**不关面板**：点了"收藏"而什么都没
    /// 发生，比读不到东西更糟。
    fn show_panel_rows(
        &mut self,
        prefix: &str,
        screen: Result<(PanelKind, Vec<panel::PanelRow>), String>,
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

    /// `Vec<String>` 那一版的适配器 = "一串字" → "全是只读行" → [`Self::show_panel_rows`]。
    ///
    /// 留着它只有一个理由：**「还缺」/「最近」/概率/复核这几屏的行本来就没有可点的**，
    /// 把它们强行套进行结构会让每条报表都先变成 `PanelRow` 再变回字（日志侧多一次拷贝，
    /// 而读的人看不出这两件事有什么区别）。
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

    /// 面板开着的行**跟着 `App.ui` 重算**（点语言行 / 点动画行之后调）。
    ///
    /// `lang` 与 `motion.code()` 都烘进行文案里，而行文案与 `lang` 都进签名 ⇒ **这里不置
    /// `force_draw`**（`cycle_page` 那条纪律：把会变的东西交给闸门去认，而不是靠调用方在
    /// 每个改它的地方补一句"记得重画"）。
    ///
    /// 重算按**当前的 `kind`**：从「概率」/「复核」借走的那一屏换回来的是默认五行 ——
    /// 而 `Lang`/`Motion` 两格本来就只存在于默认五行上，借走的那一屏除了标题带和 `Close`
    /// 全是不可点的 `Info`，所以这里永远读不到"借走的内容 + 新语言"那种组合。
    fn refresh_panel_rows(&mut self) {
        let Some(kind) = self.panel.as_ref().map(|p| p.kind) else {
            return;
        };
        if let Ok(rows) = self.panel_rows(kind) {
            self.show_panel(kind, rows);
        }
    }

    /// 面板被点：命中哪一格、那一格办哪件事。`at` 是面板 surface 的**逻辑**局部坐标。
    ///
    /// 分派**只走 [`panel::press`]**（命中 + `actionable` 把关），与菜单同一条"把关放在命中侧
    /// 而不是各 `match` 分支里"的纪律（门禁 = `an_info_row_is_hit_but_never_activates_...`）。
    /// 面板没有"忙"相位 ⇒ `press` 只滤 `Info`，这里那臂因此**不可达**，写出来是为了以后
    /// 加一种行而忘了决定它点不点得着时，`match` 会红而不是静默默认成"不可点"。
    fn on_panel_press(&mut self, at: (f32, f32)) {
        let Some(item) = self.panel.as_ref().and_then(|p| panel::press(&p.items, at)) else {
            return;
        };
        let kind = self.panel.as_ref().expect("上面查过 is_some").kind;
        match item {
            panel::PanelItem::Info => {}
            // 标题带：收藏册换 tab；设置页 `cycle()` 返回自身 ⇒ 这一臂的含义是
            // **重载这一面的默认五行**，也就是「概率」/「复核」借走这一面时唯一的回程。
            panel::PanelItem::Header => match kind.cycle().tab() {
                // `cycle()` 已经把 `page` 归零（换一本当然从第一页起），这里传 0 是**同一个
                // 意思写两遍**：`tab` 是从归一后的 `kind` 上取的，页码不是。
                Some(tab) => self.collection_panel(tab, 0),
                None => self.open_panel(PanelKind::Settings),
            },
            // 页码控件 ⇒ 翻这一本的第二页（末页回卷由 [`panel::album_page`] 归一）。
            //
            // ⚠️ 这一格**只可能出现在「已获得」**（另两本与设置页都不产 `PanelItem::More`），
            // 而这里不写 `unreachable!`：这一面被「概率」/「复核」借走过，`kind` 与行**分家**
            // 的窗口是存在的，命中一张旧表而办事办到新的一屏，比赌一次 panic 好（硬约束 3）。
            panel::PanelItem::More => match kind.tab() {
                Some(tab) => self.collection_panel(tab, kind.page() + 1),
                None => self.open_panel(PanelKind::Settings),
            },
            // 逐件那一行 ⇒ **直接穿上**（用户 11-07 拍板②）。走 [`Self::pick_and_wear`]，
            // **不走 [`Self::equip`]**：后者的早退读的是 `picked`，而"先写 picked 还是先比
            // picked"就是这一条的全部安全性所在（见那条上的顺序注释）。
            panel::PanelItem::Wear(i) => self.pick_and_wear(i),
            // 宠物那一行 ⇒ **当场换成那只**（P5，用户 11-07 拍板①"当场换"）。
            //
            // ⚠️ 这一臂与上面那臂**数的是两套下标**：`Wear` 数 [`App::candidates`]（磁盘配件），
            // `Body` 数 [`App::bodies`]（磁盘本体）。把它们分开的地方是 [`album_rows`] 产行的
            // 那一刻（`AlbumTarget` 的三臂），不是这里靠"看当前是哪一屏"猜 —— 一个数字两个
            // 事实源就是门禁 `the_album_routes_pets_to_the_body_index_space` 要拦的形状。
            panel::PanelItem::Body(i) => self.swap_body(i),
            panel::PanelItem::Lang => self.cycle_lang(),
            panel::PanelItem::Motion => self.cycle_motion(),
            // 公示概率不碰库（见 [`Self::rates_lines`]），所以这一臂不过 `gacha_panel`。
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

    /// 每 tick 走一遍：同步 scale、跑看门狗、推进相位、按签名决定要不要提交。
    /// 静止的菜单**一次 commit 都不发**（`counters.menu_commits` 必须不随 tick 增长）。
    fn update_menu(&mut self, now: Instant) {
        if self.menu.is_none() {
            return;
        }
        let scale_num = self.scale_num;
        let error = self.error_line();
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
    ///
    /// ⚠️ 这一条注释在分页之前是**已经写了但只执行了四分之一**的规矩（当时只有 `Lang` 那一路
    /// 问了 `is_busy`）。分页把命中表加到 5 张（多了标题条）而这个函数**一行把关都不用加** ——
    /// 这就是"放在命中侧"买到的东西。
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
            menu::MenuItem::Page => self.cycle_page(),
            menu::MenuItem::Quit => self.quit_pet(),
            menu::MenuItem::Walk => self.send_action(Action::Walk),
            menu::MenuItem::Run => self.send_action(Action::Run),
            menu::MenuItem::Jump => self.send_action(Action::Jump),
            menu::MenuItem::Climb => self.send_action(Action::Climb),
            menu::MenuItem::Pause => self.send_action(Action::Sleep),
            menu::MenuItem::Equip => self.equip(),
            menu::MenuItem::Unequip => self.unequip(),
            menu::MenuItem::NextOutfit => self.cycle_accessory(),
            // ── 收藏册页：三行各自**开出面板的那一个 tab**（全是只读，不掷骰、不花钱、不改库）──
            // 这一批起它们不再"只进 stderr"：点下去 = 第三块面上看得见那几行，而 stderr 打的
            // 是同一批字（`Gacha::tab_lines` 是唯一的生产者，两边都从它过）。
            menu::MenuItem::Owned => self.collection_panel(CollectionTab::Owned, 0),
            menu::MenuItem::Missing => self.collection_panel(CollectionTab::Missing, 0),
            // 条数 = 一屏装得下的行数（`panel::MAX_ROWS - 1`，见 [`Gacha::tab_lines`]）。
            // 不是"最近十次 = 一次十连"那种口径：那个说法属于只打日志的年代，而现在这一屏
            // 第六格是 `Close`，多一条就得靠截断补「+N 更多」，那是保险而不是主路径。
            menu::MenuItem::Recent => self.collection_panel(CollectionTab::Recent, 0),
            // ── 设置页：菜单那三行是**入口**，功能在面板上办 ──
            // `MenuItem::Settings` 不存在（也不该有 —— 加一个变体要动 `rows()`、命中表、
            // l10n 四个 locale、字体重裁），所以"动画"那一行的循环职责交给面板里的
            // `Motion` 行，菜单这行改成把那一面开出来。
            menu::MenuItem::MotionTier => self.open_panel(PanelKind::Settings),
            menu::MenuItem::Rates => {
                let lines = self.rates_lines();
                self.show_panel_lines("rates", PanelKind::Settings, lines);
            }
            menu::MenuItem::Verify => {
                self.gacha_panel("verify", PanelKind::Settings, |g, l, s| {
                    g.verify_lines(l, s)
                });
            }
        }
    }

    /// 点标题条：翻到下一页，并**当场重建命中表**。
    ///
    /// `items` 与 `page` 必须一起改：`items` 是点击的唯一依据而 `page` 是绘制的唯一依据，
    /// 分开改就会出现"点的是这一页的行、画的是那一页的字"。这里不加 `force_draw` ——
    /// **页进 `menu::content_sig`**（`69fd158` 那条教训的正用：把会变的东西交给闸门去认，
    /// 而不是靠调用方在每个改它的地方补一句"记得重画"）。`force_draw` 的合法领地只有签名
    /// 看不见的那些像素（scale 镜像、看门狗、configure）。
    fn cycle_page(&mut self) {
        let Some(m) = self.menu.as_mut() else {
            return;
        };
        let next = m.page.next();
        m.page = next;
        m.items = menu::rects(next);
    }

    /// 点"退出"：**这是 `exit` 的第二个写入点**（第一个是 `--seconds` 到点）。
    ///
    /// 两步的顺序是有意的：先 `close_menu()` 与 `close_panel()`（`teardown()` 已按 region →
    /// viewport → layer → surface 的次序收回，两块面都不会活得比决定久），再置 `exit`。
    /// 下一个 `on_tick` 读到它就 `signal.stop()`，而 `report()` 在事件循环**之后** ⇒
    /// **退出摘要照打**（`菜单: N 提交`、`面板: N 提交`、五档核对行、VmHWM/CPU 一条不少）。
    /// 抽取在点击那一刻就同步落库 ⇒ 不需要额外拆库。
    ///
    /// ⚠️ `close_panel()` 在**当前**的可达路径上是空转（`Quit` 只有菜单那一面能点到，而菜单
    /// 一开面板必然已经关了）。留着它不是防御，是因为"退出要把自己建过的每一块面都收回"
    /// 这件事**不该依赖另一条不变量此刻成不成立** —— 互斥那条以后若被推翻，这里必须仍然对。
    ///
    /// **不做确认步骤**：这块 surface 没有对话框、也没有 click-outside（D14），"确认"会变成
    /// 又一个页状态 + 又一批假按钮风险；而"退出"不在首页而在第三页的最后一行，误触至少要三次
    /// 点击（翻两页 + 点行），而翻页点错是免费的（六页循环，最坏多按五次就回到原位）。误触退出的代价
    /// = 重启一次，零数据损失。
    fn quit_pet(&mut self) {
        self.close_menu();
        self.close_panel();
        self.exit = true;
        eprintln!("ui: 菜单请求退出 ⇒ 收尾并打退出摘要");
    }

    /// 动作页四行 + 暂停那一行 ⇒ 一条 `Command::Go`，**不在客户端预判内核会不会响应**。
    ///
    /// 为什么不预过滤（为什么不灰掉"现在点没用"的行）：那要求每次绘制都读 `sim.state`/`world`
    /// ⇒ **内核几何变成绘图的输入** ⇒ "落地了吗？""这动作开着吗？"每个判据都要变成签名的一个
    /// 字段，而"灰掉的行"正是本项目禁止的假按钮形状反过来 —— 画一个客户端读不到的、而且
    /// 在绘制与点击之间内核自己就能改掉的状态。内核本来就把这些全吸收了：下坠中 `Go(Walk)`
    /// 被 `handle_go` 丢掉；睡着时任何别的动作先 `goto(Idle, Wakeup)`（⇒ **"点动作页任意一行
    /// = 唤醒它"是内核既有语义，不是权宜**）；睡着再点"暂停"无副作用。这和 CLI 的形状一致
    /// （CLI 是盲发的），也是 §3.3"指令目标不预剪枝"那条原则换到 UI 面。
    ///
    /// 连带后果，**明写而不是藏起来**：宠物正在下坠时点"走路"就是什么都不发生。真机门禁里
    /// 有这一条，要用户判它可不可接受（计划 §验证 第 3 条）。
    fn send_action(&mut self, action: Action) {
        if let Some(sim) = self.sim.as_mut() {
            sim.send(Command::Go(action));
        }
    }

    /// 这一轮**开发态豁免**是否成立 = `--allow-unowned-accessories` 且真的有 `--db`。
    ///
    /// 单独一处，是因为它同时是两件事的判据：**未拥有的件进不进可见集**（[`Self::dress_view`]）
    /// 与**帽子页标题打不打那个 `*`**。两处各写一遍条件，就会出现"标打了而没放行"或反过来
    /// —— 而那正是"提示不是判据"登记过的那一族。
    fn dress_exempt(&self) -> bool {
        dress_exempt_for(self.opts.allow_unowned_accessories, self.dry_run)
    }

    /// 换装页"这一件许不许穿"的**库存那道**（P2 = FLOW §8 第一条的落地）。
    ///
    /// **顺序 = 先磁盘后库存**，这不是审美：[`Self::candidates`] 已经过完五道磁盘门禁，这一道
    /// 只问"库存里有没有这一件"。反过来做（从库存出发再查磁盘）得到同一批件，但"这件不匹配本体"
    /// 与"你没有这件"会混成一句 —— 那是两句不同的话，都要能打到（FLOW §8 明写的理由）。
    ///
    /// 只在点击回调与开菜单那一刻调（§4 禁止定时轮询；D14 那条"余额是事件驱动的"同形状）。
    ///
    /// ⚠️ **失败关闭**：给了 `--db` 而库连不上/读不动 ⇒ 可见集为**空**、不放行。宁可这一轮穿不上
    /// 而 stderr 有一行解释，也不要"没验过拥有就穿上"—— 后者正是 §9.2 第 20 条要杀掉的那类静默。
    ///
    /// 这一层只剩"把三档输入问出来"（豁免？演示？库读不读得到？），判定本身在
    /// [`DressView::select`] —— 拆开的理由写在那条上。
    fn dress_view(&mut self) -> DressView {
        // 名字要先取：下面 `ensure_gacha()` 可变借用 self，不能同时留 `candidates` 的引用。
        let ids: Vec<String> = self.candidates.iter().map(|c| c.id.clone()).collect();
        if self.dress_exempt() || self.dry_run {
            // 两档放行合成一次调用，但**屏上记号不同**：豁免态点亮帽子页那个 `*`（判据 =
            // [`App::dress_exempt`]），演示态不点 —— 右端已有「演示」标，同一件事给两个记号
            // 就是两个事实源（用户 2026-11-06 拍板"照 DRYRUN 放行 + 打「演示」标"）。
            return DressView::select(true, None, &ids);
        }
        if !self.ensure_gacha() {
            // 连不上库那件事 `ensure_gacha` → `gacha_bailed` 已经出过一次声，这里只负责不出声地
            // 关死，不再重复播报（同 `dress_panel` 那条"错误只说一次"的分界）。
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
            // 留着而不是 `expect`：它对应的是"那道没读过"，语义上就该关死，不值得为一条不可达
            // 分支赌一次 panic（硬约束 3：宠物不能因为一行判据消失）。
            None => None,
        };
        DressView::select(false, owned.as_ref(), &ids)
    }

    /// 这一轮**本体侧开发态豁免**是否成立 = `--allow-unowned-pets` 且真的有 `--db`。
    ///
    /// 判据函数与配件侧**共用**（[`dress_exempt_for`]），而**开关字段分开**（拍板④）：配件那条
    /// 的屏上记号是帽子页标题的 `*`，那一页的列表里根本没有宠物 ⇒ 两条合并必然出现"标打了而没
    /// 放行"。所以本体这一档**没有屏上记号**，只有这一处判据 + 启动那行 stderr。
    fn pet_exempt(&self) -> bool {
        dress_exempt_for(self.opts.allow_unowned_pets, self.dry_run)
    }

    /// 收藏册宠物行的**库存那道** = [`Self::dress_view`] 的同形兄弟，喂的是 [`Self::bodies`] 的
    /// id。判定共用 [`DressView::select`]（三档输入那一层），所以"未拥有的件不出现"与"未拥有的
    /// 本体点不动"从头是**同一判据**，不会有两套答案。
    ///
    /// 与配件那侧一样**失败关闭**：给了 `--db` 而库存读不出 ⇒ 可见集为空。宁可这一轮换不了本体
    /// 而 stderr 有一行解释，也不要"没验过拥有就换成那只"—— 后者是 §9.2 第 20 条要杀的静默。
    ///
    /// ⚠️ 只在点击回调里调（收藏册那一行点下去、以及 `album_rows` 给不给下标那一刻），不轮询。
    /// 与 [`Self::dress_view`] 各自开一次库不是两个事实源：库是事实源，两次读的是同一张表。
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
            // （配件那侧同一条理由，写在 [`Self::dress_view`]：不可达分支不该赌一次 panic）。
            None => None,
        };
        DressView::select(false, owned.as_ref(), &ids)
    }

    /// 点"换一件"：在 [`Self::candidates`] 里把下标往前挪一格，**穿着时立刻换过去**。
    ///
    /// 页导航不在这条行里做（`Dress.next() == Gacha`，标题条就是唯一翻页控件）⇒ 这一行只管
    /// "这一页要办的两件事"里的第一件，而不是既换件又换页。
    ///
    /// **没穿东西时屏上一个像素都不变**，所以这里**不置 `force_draw`、也不进 `content_sig`**
    /// （见 [`Self::picked`] 那条注释）。反馈只有 stderr：选中那件叫什么 = 用户内容，子集字体
    /// 盖不住 ⇒ 宁可不打字也不在卡片上画 `?`。
    ///
    /// ⚠️ 穿着时**先释放再装载**（不是"只改下标，等用户再点一次穿上"），两个理由：
    /// * 只改下标会把"选中的"和"屏上的"分成两个事实，而用户拿它们对不上时**没有任何一行读数
    ///   能告诉我是哪一步没跟上** —— 这正是本项目反复出现的"两个事实源"形状。
    /// * 顺序 = [`take_worn`] 在前、`decode` 在后 ⇒ 峰值始终是一件（M1 那一档的字面要求；
    ///   反过来先解再丢会瞬间有两份 45 MiB 解码在场，而 `VmHWM` 记的正是那一瞬间）。
    ///   ⚠️ 光有顺序**不够**：`take_worn` 原先把那个 [`Equipped`] 交回给这里当局部变量，而 Rust
    ///   在块尾才 drop ⇒ "先释放"是注释上的而内存里没发生，"换一件"照样撞出三份 sheet。
    ///   2026-11-03 真机 `VmHWM 148,076 kB` = 本体 45 + 旧 45 + 新 45 + 底子，就是这条撞出来的
    ///   ⇒ 现在 `take_worn` 只返回 `(slug, bytes)`，旧那张在本函数返回时就没了。
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
        // 循环序 = `visible` 的升序（= 目录序）。`picked` 不在其中是**合法状态**而不是 bug：它
        // 上一次被选中时也许还在豁免态里，而现在库存那道把它划掉了 ⇒ 落到第一个可见格，
        // 绝不把下标停在一件穿不上的东西上（那正是"点了没反应"的假按钮形状）。
        let pos = view.visible.iter().position(|&i| i == self.picked);
        self.picked = match pos {
            Some(p) => view.visible[(p + 1) % view.visible.len()],
            None => view.visible[0],
        };
        // 报的是"可见表里的第几格"，不是"库里的第几件"：用户拿这行去对屏上循环，对的就是这一张表。
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

    /// 收藏册点名一件 ⇒ **直接穿上**（P4 = 用户 11-07 拍板②）。第 4 个 [`Self::wear`] 调用点，
    /// 也是唯一一个"下标来自屏上一行"的。
    ///
    /// 为什么不借道 [`Self::equip`]（这点很反直觉，值得看完）：`equip` 的第一条出口是
    /// "已经穿着 ⇒ 当场返回"，而它成立靠的是**"穿着的就是 `picked` 那一件"这条不变量**
    /// （原来只有 [`Self::cycle_accessory`] 会改 `picked`，它改下标时总是连像素一起换）。
    /// 收藏册要点的是**任意一格**，那条不变量在这一条路上还没建立就要求它 ⇒ 借道 `equip` 的
    /// 话，"身上是 A、点的是 B"会读成"已经穿着"⇒ **静默不动 + 一句谎话**。所以顺序是：
    ///
    /// 1. `dress_view()` —— 所有权闸**照旧只有一个所有者**，这一条不自己实现"许不许穿"；
    /// 2. 判 `plan_pick_wear`（**比 `picked` 必须在这里，任何写入之前**）；
    /// 3. `SelectAndWear` 时**先** [`Self::take_worn`] 释放身上那件，**再**写 `picked`、再 [`Self::wear`]。
    ///
    /// 3 里"先释放"是 M1 内存档的字面要求（[`Self::take_worn`] 那条算过 135 MiB 的峰值），
    /// 而"释放排在写 `picked` 之前"是为了让 2 的判据读到的还是**用户点之前**的那个世界。
    /// 用 `take_worn` 不用 [`Self::unwear`]：后者会先把库里那一格 DELETE 再写回来，白写一次
    /// 且留一个"库里没有"的可见窗口（[`Self::cycle_accessory`] 同一处理由）。
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
    /// ⚠️ **顺序就是它的全部安全性**，所以九步一步不许换位置，理由各自写在行上。整体论证：
    /// * 第 4 步（`atlas.reload`）是**不可回头的那一条线**之前唯一的写点，而它自己的三条自检
    ///   全排在它第一笔像素写之前（见 [`Atlas::reload`]）⇒ "新图集 + 旧清单"这个半换形状
    ///   **构造不出来**：3 之后的步骤全是赋值（`regions` 早在扫盘时就解好，见
    ///   `BodyCandidate::regions` 那条字段注释，所以这一步没有 IO、不可能失败）。
    /// * 5–9 里唯一还能失败的 IO 是第 7 步那次 `wear()` 的解码；它的失败形状 = 本体新、帽子无、
    ///   点名一行 ⇒ **自洽**（屏上没有帽子而库里有那一行，重启会在新本体上重新穿上它 —— 而它
    ///   过得了门禁，因为第 6 步刚刚按这只新本体重跑过一遍）。
    /// * calloop 单线程 ⇒ `swap_body` 内部不可能发生 commit，所以即便存在瞬时不一致也不可观测。
    ///   它并不存在：见上一条。
    ///
    /// **峰值 = 一只本体 + 一顶帽**（D16 那条 M1 档）。第 2 步排在第 4 步之前不是顺手：
    /// 反过来就瞬间有"旧本体 45 + 新本体 45 + 帽 45"在场，而 ≈147 MB **仍落在 ≤200 MiB 门禁内
    /// ⇒ 什么都不会红**（`148,076 kB` 那一族就是这么漏掉的；那条线 2026-11-10 前是 150 MiB）。
    /// 所以这条防线的读数是两处：
    /// 验收量一次 `VmHWM`，以及源级门禁数 `atlas.reload(` 恰好 1 次 + `take_worn` 排在它前面。
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
        // （硬约束 3：宠物不能因为一行下标消失）。
        let Some((pet_slug, new_regions, sheet)) = self
            .bodies
            .get(i)
            .map(|b| (b.slug.clone(), b.regions.clone(), b.sheet()))
        else {
            eprintln!("ui: 换本体 ⇒ 判定放行而 {name} 这一格取不到 ⇒ 不动（判据与库对不上）");
            return;
        };
        let Some(sheet_str) = sheet.to_str().map(|s| s.to_string()) else {
            eprintln!("ui: 换本体 ⇒ {name} 的 sheet 路径不是 UTF-8 ⇒ 本体不动");
            return;
        };

        // —— 第 2 步：先释放帽子。身份要在这一步**之前**取全：[`Equipped`] 只带清单（`slug`/
        // `slot`），而"这一件是谁"（`id` = 目录名 = 库存与 `equipped` 认的那个键）记在
        // [`Candidate`] 那一侧，第 6 步重建候选表之后就查不到了。读 `candidates[picked]` 依赖的是
        // 那条既有不变量"**穿着 ⇒ 穿的就是当前选中这件**"（四个装载点共同维持，见
        // [`Self::equip`] 那条注释）；`id` 取不到时第 7 步不猜，直接按"出局"处理（宁可多删一行
        // 并报出原因，也不要"给一只戴不上的宠物留着帽子"）。
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

        // —— 第 3 步：新本体的轮廓**已经在 [`BodyCandidate`] 里**（扫盘第 ④ 道解的那份）⇒ 这一步
        // 没有 IO。把"到换的时候再读一次 `polygon.json`"排除掉不是省几微秒，而是让"新清单 +
        // 旧轮廓"这个半换形状**没有对应的代码**：一个不可能失败的步骤不需要错误分支。
        // —— 第 4 步：解新 sheet。这一步之后就没有可回头的失败了。
        match self.atlas.as_mut() {
            Some(atlas) => {
                if let Err(e) = atlas.reload(&sheet_str) {
                    eprintln!(
                        "ui: 换本体 ⇒ {name} 的 sheet 解不开（{e}）⇒ **本体没换**，屏上仍是 {cur}\
                         （帽子已在第 2 步释放，库里那一行还在：点「穿上」就回来，重启也会回来）",
                        cur = self.body_id
                    );
                    // ⚠️ 这里**必须**补一次重投：`take_worn` 只换轮廓、不置 `force_draw`（它的
                    // 注释明写"由调用方决定用户要不要看见"），而此刻屏上留着的是**已经不在内存的
                    // 那顶帽子**的像素 ⇒ 不重画就是"轮廓说是本体、像素里还有帽子"那格点不到的
                    // 状态，且它要留到下一次格子真变才被洗掉。
                    self.force_draw = true;
                    return;
                }
            }
            None => {
                // 走到这里意味着 `bodies` 非空而 `atlas` 是 `None` —— `run()` 里那两个条件是
                // 同一个（`body_regions`），所以这是"以后有人绕过 `run()` 构造 App"的形状。
                // 报一行、不动，而不是 `unwrap`（硬约束 3）。
                eprintln!("ui: 换本体 ⇒ 这一轮没有图集像素（`--test-rect` 那条路）⇒ 本体不动");
                return;
            }
        }

        // —— 第 5 步：提交元数据。清单的 `&'static` 在这里才泄（第一次用上它，见
        // [`BodyCandidate::leaked`]）⇒ 一份进程内每只不同的宠物只泄几 KB，不是每次换都泄。
        let m = self.bodies[i].leaked();
        self.manifest = m;
        self.body_regions = Some(new_regions.clone());
        self.regions = Some(new_regions.clone());
        self.body_id = name.clone();
        if let Some(sim) = self.sim.as_mut() {
            sim.swap_manifest(m);
        }

        // —— 第 6 步：按**新本体**重跑配件发现 ⇒ `fits` 不含新 `slug` 的那几件当场出局
        // （`AccessoryManifest::validate_against` 比的就是 `body.slug`），出局的每一件走既有的
        // skip-and-log 点名 ⇒ 这几行就是 J4a/J4b 的唯一读数。
        // ⚠️ 库位置是 `accessory_root()`，**不是**本体库那个根：11-07 交互主轮实测错过这一次，那
        // 一轮把配件库扫成了本体库那个目录（`accessories: 库 …/pets_p5 ⇒ 0 件可用`）⇒ **换完本体
        // 之后帽子当场掉、且这一轮再也穿不上任何配件**，而第 7 步的按 id 重认永远查不到幸存者。
        // 那是一条 headless 读不到的接线错（`App` 要 compositor 才构造得起来），所以判据落在下面
        // 那条 textual 门禁：`swap_body` 的函数体里连注释都不许写那个函数名。
        self.candidates = accessory_root()
            .map(|root| discover_accessories(&root, m, &new_regions))
            .unwrap_or_default();

        // —— 第 7 步：**按 id 重认**身上那顶（不按位置、不按旧下标）。
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

        // —— 第 7′ 步：面板行要按**新的**候选表重算。收藏册里配件那一格的载荷是 `candidates` 的
        // 下标，而第 6 步整张表重建过 ⇒ 不刷新就是"行上写着 A、点下去穿的是 B"（两个事实源），
        // 而它**不进 `content_sig`**（下标故意不在签名里）⇒ 光靠签名自己认不出来。
        self.refresh_panel_rows();

        // —— 第 8 步：**本轮最承重的一行**。四份 @192 产物的行表逐字节相同 ⇒ 内核下一次给出的
        // `{key,row,col}` 与换之前**完全相同**，而 [`Self::present`] 那道门（`cell != drawn_cell
        // || force_draw`）永远不开 ⇒ 删掉这两行的后果是一次**屏上静默不变**的换本体：像素与清单
        // 都真的换了，而屏幕继续投旧格子（变异 M3 的实测形状 = 545 条全绿而 J1 真机失败，只有
        // 截图抓得到 ⇒ 所以这两行进了源级门禁的读数，不能只留注释）。
        // `drawn_cell = None` 排在前：它保证"下一格无条件算作新格"，`force_draw` 保证"就算格子
        // 没变也投一帧"。先例 = [`Self::redraw_for_scale`] 与 [`Self::equip`] 那条"这正是
        // `force_draw` 的合法领地 = 签名与 `drawn_cell` 都看不见的那些像素"。
        self.drawn_cell = None;
        self.force_draw = true;

        // —— 第 9 步：**账排最后**（先像素后账，[`Self::persist_worn`] 同一条口径）。
        self.persist_body(&name);
        eprintln!(
            "ui: 换本体 ⇒ 当前本体 = {name}（`equipped` 的 {slot} 行）",
            slot = BODY_SLOT
        );
    }

    /// 把"当前本体是这只"记进库的 `body` 行。三档结论与 [`Self::persist_worn`] 完全同一套
    /// （共用 [`write_equip`]），差别只在**槽位**和那句"重启会回到哪一只"。
    ///
    /// 拆成单独一条而不是把 `persist_worn` 改造成带槽位参数：后者的 `index` 是 `candidates` 的
    /// 下标，而本体那套下标是 `bodies` 的（拍板① 之后仍然分开的两个下标空间，收藏册按
    /// `PanelItem::Body` / `PanelItem::Wear` 两臂路由）。共用一个函数就要共用一个下标空间，那正是
    /// `the_album_routes_pets_to_the_body_index_space` 那条门禁要杀的形状。
    ///
    /// ⚠️ 只有 UPSERT、没有 DELETE：见 [`Self::swap_body`] 那条"非目标"。
    fn persist_body(&mut self, id: &str) {
        match write_equip(&mut self.gacha, self.dry_run, BODY_SLOT, id) {
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

    /// 点"穿上"：把当前选中的那件**到这一步才解码**（内存档 M1 = 一次一件 ⇒ 装备 = 装载、
    /// 脱装 = 释放，配件那份 sheet 不在常驻预算里）。
    ///
    /// 三条出口都必须是"什么都不做"而不是"做一遍再把结果丢掉"，而只有第一条由本函数自己判：
    /// * **已经穿着** ⇒ 当场返回，不重解码。重复点同一行要再付一次解码的几十毫秒与几 MiB，
    ///   还会把同一格像素再投一帧（签名看不见变化而照样提交 = `69fd158` 那个形状）。
    ///   这里**不需要比"是不是同一件"**：[`cycle_accessory`] 与 [`pick_and_wear`] 改 `picked`
    ///   时都是"当场就装载"（后者连"比了再写"的顺序都写在那条里）⇒ "穿着"永远意味着
    ///   "穿的就是当前选中这件"，这是那**两条**共同维持的不变量。**以后任何"只改下标不装像素"
    ///   的新路径都会当场把这句话变成假话**，而 `equip` 的第一条出口就会静默吞掉一次点击。
    /// * **库里没有可用配件**（库不存在 / 空 / 每一件都过不了门禁）⇒ 一行日志。
    ///   **不预先把这行画成灰的**：那要求客户端在绘制时读"扫到了几件"，而灰掉的行正是本项目
    ///   禁止的假按钮形状反过来（[`send_action`] 那条同族理由，用户已判"照发、接受不响应"）。
    /// * **sheet 解不开** ⇒ 报错并停在未装备态。**这不是新策略**：同一份坏文件在
    ///   [`discover_accessories`] 里是"少一件可穿"，而此刻用户已经坐在桌面前 ⇒ 一行 stderr 比
    ///   杀掉宠物好（硬约束 3）。四道门禁都在启动走过 ⇒ 这一档剩下的唯一失败面就是读像素。
    ///
    /// ⚠️ 装成之后**必须** `force_draw`：换装改的是**宠物那块 surface** 的像素，而换的往往是
    /// **同一个图集格**（帽子不改变内核给出的 `{key,row,col}`）⇒ 光靠"格子变了"那道门永远不开。
    /// 这正是 `force_draw` 的合法领地 = 签名与 `drawn_cell` 都看不见的那些像素。
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
        // 三个形状都落在这一支：库存说没这一件、给了 `--db` 而库读不出（失败关闭）、
        // 以及 `picked` 停在一件已出局的格上。
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
            Ok(slug) => eprintln!("ui: 穿上 {slug} ⇒ 换穿透轮廓并重画当前格"),
            Err(e) => eprintln!("ui: 穿上失败 ⇒ {e}（停在未装备态）"),
        }
    }

    /// 点"脱下"：丢掉配件，把穿透轮廓换回 [`Self::body_regions`] 那一份。
    ///
    /// 复原是**换对象**而不是"再算一遍"：并集是一次性的事实，重算要求配件那份 `Regions` 一直在
    /// 内存里 —— 而 M1 档的整个前提就是脱装要把配件的东西释放掉。这条路径不读盘、不分配，
    /// ⇒ **结构上不可能失败**，所以没有"点了脱下而屏上还有帽子"这一档。
    /// 本来就没穿 ⇒ 只打一行，同样不置 `force_draw`（不置就是"静止不发协议"那条老账，不是遗漏）。
    fn unequip(&mut self) {
        let Some((slug, bytes)) = self.unwear() else {
            eprintln!("ui: 脱下 ⇒ 本来就没穿，不动");
            return;
        };
        eprintln!(
            "ui: 脱下 {slug} ⇒ 配件 sheet（{:.3} MiB 解码）已释放，轮廓回到本体那份",
            bytes as f64 / (1024.0 * 1024.0)
        );
    }

    /// 脱下 = 释放像素 + 把那一格从库里删掉，**一个动作**（与 [`Self::wear`] 对称）。
    ///
    /// 合起来的理由和 `wear` 那条一模一样：变异实测把原来 `unequip` 末尾那句
    /// `persist_unworn` 单独删掉，全套测试照样绿，而用户看到的是"点了脱下，重启帽子自己回来"
    /// —— 恰好是本项目反复在抓的"屏上 A、记忆里 B"。返回 `None` = 本来就没穿。
    ///
    /// ⚠️ [`Self::cycle_accessory`] **故意不走这里**，走裸的 [`Self::take_worn`]：换一件是
    /// "卸下 A 立刻穿上 B"，中间那一步先把库里那一格删掉再写回来会留一个可见的"库里没有"窗口，
    /// 而且白写一次；它后面那次 `wear` 的 upsert 直接把该格改成 B，那才是要的语义。
    fn unwear(&mut self) -> Option<(String, usize)> {
        // 槽位要在丢掉之前取：[`take_worn`] 只带回打印要用的那两个数（它的注释写了理由），
        // 而"把这一格从库里删掉"要的正是那个槽位。这一行同时是"本来就没穿"那档的守卫。
        let slot = self.worn.as_ref().map(|w| w.manifest.slot.to_string())?;
        let (slug, bytes) = self.take_worn()?;
        self.force_draw = true;
        self.persist_unworn(&slot);
        Some((slug, bytes))
    }

    /// 释放穿着的配件 = 丢掉 [`Equipped`]（那张 sheet 从此不在内存）+ 把轮廓换回本体那份。
    ///
    /// **不打印、不置 `force_draw`** ⇒ 由调用方决定用户要不要看见这一次（"脱下"要打一行，
    /// "换一件"随后自己会装载，把两件事写成两行才读得懂）。返回 `None` = 本来就没穿。
    ///
    /// 单独一个函数只有一个理由：`take` 与"换回哪份轮廓"必须是一个动作。分成两处写就有一个
    /// 中间形状 = "像素没了而 `regions` 还是并集" ⇒ 屏上没帽子而那块区域点得到，而那条
    /// **没有任何门禁看得见**（`wl_region` 的并集多几格从来不算错，只有"少几格"会被
    /// `first_missing` 抓到）。
    ///
    /// ⚠️ **返回 `(slug, decoded_bytes)` 而不是那个 [`Equipped]**，这是 M1 档最要紧的一条形状：
    /// 交出对象会让调用方把旧 sheet 一路活到它自己那次 `decode()` 之后（Rust 在块尾 drop，
    /// 不在最后一次使用处），而 [`cycle_accessory`] 正是"先卸下再装载" ⇒ 峰值变成
    /// **本体 45 + 旧 45 + 新 45 = 135 MiB**，2026-11-03 真机 `VmHWM 148,076 kB` 量的就是它，
    /// 越的是用户为 M1 签的 `≤ 100 MiB`。只把**打印要用的那两个数**带出来，图集就在本函数
    /// 返回的那一刻被释放 ⇒ 内存里任何时刻最多两张 sheet。⇒ 这条不该靠调用方记得 `drop`。
    fn take_worn(&mut self) -> Option<(String, usize)> {
        let worn = self.worn.take()?;
        self.regions = self.body_regions.clone();
        Some((worn.manifest.slug.clone(), worn.atlas.decoded_bytes()))
    }

    /// **唯一**能把配件放上身的那一步：解码 → 像素与轮廓上身 → 记进 `equipped`。
    ///
    /// 为什么这三件必须在一个函数里（本轮变异实测逼出来的）：原先几个装载点各自写"四行赋值 +
    /// 一句 `persist_worn`"，而把 `equip` 里那句删掉，**全套 526 条测试全绿** —— `App` 在没有
    /// compositor 的机器上构造不出来 ⇒ "屏上戴着一顶而库里没记这笔"这个本项目反复出现的形状，
    /// 在 headless 里**根本没有读数**。合成一个动作之后，"改了像素而忘了记账"不再是写得出来的
    /// 代码；要绕过它就得先删掉这个函数，而那必然在其他调用点上报错。
    ///
    /// ⚠️ **调用点现在是五个**：[`Self::equip`]（换装页点"穿上"）、[`Self::cycle_accessory`]
    /// （"换一件"）、[`Self::restore_worn`]（启动恢复）、[`Self::pick_and_wear`]（收藏册点名）、
    /// [`Self::swap_body`]（换本体后**旧帽子还在新可见集里**的那一次重穿）。
    /// 数量本身由 `pixels_and_the_ledger_are_written_by_one_function_only` 逐个数着 ⇒ 加下一个
    /// 不会静默，而**第四个与第五个都是被不变量逼出来的**，不是重复：`equip` 的早退读 `picked`，
    /// 而收藏册要在"点 B 而身上是 A"时真的换过去（完整理由写在 [`Self::pick_and_wear`] 那条上）；
    /// `swap_body` 那一次则是"下标整套换了一套房"之后唯一能把那顶帽重新放上身的手法（它排在
    /// `take_worn` 之后，所以内存里任何时刻仍最多两张 sheet）。
    /// **以后再加装载/卸下路径，必须走 `wear()`/`unwear()`**，且必须问清"它能不能借道已有的那几个"。
    ///
    /// ⚠️ **调用方如果原本穿着，必须先 [`Self::take_worn`]**：这里不做，是因为 [`cycle_accessory`]
    /// 要在两件之间插一行"释放了多少 MiB"，而那行要在旧 sheet 还在的时候取它的字节数
    /// （[`Self::pick_and_wear`] 同理）。忘了 take 的代价就是 `take_worn` 那条注释算过的 135 MiB 峰值。
    ///
    /// 顺序（先像素后账）是选的：反过来的话"写库成功而解码失败"会在库里留一行屏上没有的帽子
    /// ⇒ 下次启动凭空多一顶。宁可"这次没留住"并且由 [`Self::persist_worn`] 说出来。
    ///
    /// 返回 `slug` 只给人看的那几行用（各处措辞自带上下文：第几件 / 释放了多少 / 恢复 / 点名）。
    fn wear(&mut self, index: usize) -> Result<String, String> {
        let eq = decode(&self.candidates[index])?;
        let slug = eq.manifest.slug.clone();
        let merged = self.candidates[index].merged.clone();
        self.worn = Some(eq);
        self.regions = Some(merged);
        self.force_draw = true;
        self.persist_worn(index);
        Ok(slug)
    }

    /// 把"屏上现在穿着第 `index` 件"记进库。**只在像素已经装成功之后调**（三档结论里
    /// 只有 `Written` 是不多话的 ⇒ 这一行与那句"穿上"是一件事的两半，不是两句日志）。
    ///
    /// 顺序（先像素后账）是选的：反过来的话"写库成功而解码失败"会在库里留一行屏上没有的帽子
    /// ⇒ 下次启动凭空多一顶。宁可"这次没留住"并且说出来。
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

    /// 把那一格从库里删掉。同 [`Self::persist_worn`] 的三档与同一句"屏上没了而库里还记着"
    /// 必须说出来的理由（脱下没留住 = 下次启动帽子自己回来，用户会以为脱下没生效）。
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
    /// [`Self::restore_pending`]，在 [`Self::on_tick`] 里放下）。
    ///
    /// 三条早退各有其门，都**不出声**（它们不是"失败了"，是"这一轮压根没有这件事"）：
    /// 演示态不读库（FLOW §9 P3 明写要测的那条）、本体没像素（`--test-rect` / 内置 fixture
    /// 不读盘那条路，与启动时 `discover_accessories()` 的 `match` 同一个条件）。
    ///
    /// 判定本身在 [`plan_restore`]（四道、可红）；这里只做三件它做不了的事：连库、
    /// **重跑第五道（解码）**、把结果接到 `worn`/`regions`/`picked` 上。第五道为什么只能在这里：
    /// 启动扫盘那一轮一像素未解（M1 的前提），而"文件在而 sheet 坏了"这种形状只有穿的那一刻
    /// 才看得见 —— 报一行、停在未穿，绝不为一次恢复把宠物带崩（硬约束 3）。
    fn restore_worn(&mut self) {
        if self.dry_run || self.body_regions.is_none() {
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
        // —— **先把本体那一行剥掉**（P5）。`body` 按字母序排在所有配件槽位**之前** ⇒ 它是整表的
        // 第一条，而 [`plan_restore`] 四道里第 1 道只问"配件表里有没有这个 id"、第 3 道比"槽位 vs
        // 清单声明"，本体行必然撞上其中一道，于是 stderr 打出的是一句**说的是配件的假话**（它压根
        // 不是配件）而每一轮启动都打。坏的不是行为（那一行 `continue` 掉了，一次都不占 `wear`，帽子
        // 照旧回来），是那句话 + 那句永久噪音 —— 详见 [`split_body_row`] 那段。
        // 剥下来的那一行**这里不办事**：起点本体早在 surface 建起来之前就由 [`db_body_row`] 定完了
        // （那一臂自己点名），所以此刻唯一的活是"库里记的本体与屏上这只不一致"时补一行名 ——
        // 那种情况只可能是 `Keep(Some(理由))` 那一条（或 `db_body_row` 读失败），而它必须说得出
        // 来，否则"上次换的那只自己没了"就成了一句没人说过的话。
        let (body, accessories) = split_body_row(&rows);
        if let Some(id) = body.as_deref().filter(|id| *id != self.body_id) {
            eprintln!(
                "body: 库里那一行 {slot}={id}，而本轮起点本体是 {cur} ⇒ 那一只这一轮没装上\
                 （扫盘没交出它：目录不存在 / 产物坏了 / 格子档不同）",
                slot = BODY_SLOT,
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
            Ok(_) => {
                eprintln!("equipped: {slot}={id} ⇒ 恢复上次穿着（五道全过，像素已叠在当前格上）");
            }
            Err(e) => eprintln!(
                "equipped: {slot}={id} 解不开（{e}）⇒ 第五道没过，这一轮不穿（停在未装备态）"
            ),
        }
    }

    /// 点语言行：zh→en→ja→ko→zh，**本进程立刻生效 + 当场写回 `config.toml`**（拍板④）。
    ///
    /// 原先这一条是"只改本进程、不回写"，理由是 §7-R15：那一轮的 reader 是自写的平坦
    /// `key = "value"` 解析，读改写会把用户的注释整段刷平 ⇒ **丢注释比"重启回默认"更坏**。
    /// 那个理由现在被 `petstore::config::write_ui_key` 换掉了（`toml_edit` 保 decor +
    /// 落盘前后各判一次），所以这一行从"提示你自己去写"变成"真的写"。两处一起改
    /// （`App.ui.lang` 是真相源、`MenuState.lang` 是 draw 用的副本）—— 全项目只有这一处写它。
    ///
    /// 第三处 = **开着的面板要把行重算**（[`Self::refresh_panel_rows`]）：面板那一屏每一行的字
    /// 都从 `lang` 来，不换就是"切了语言而收藏册还写着中文"。这里**不置 `force_draw`**：
    /// `lang` 与每行文本都在 `panel::content_sig` 里，闸门自己认得出。
    ///
    /// ⚠️ 这里**不给面板置 `force_draw`**（第三处那段的同一条例外）：`lang` 与重建出来的每一行
    /// 文本都已经在 `panel::content_sig` 里，下一 tick 闸门自己放行。写回排在最后，是因为
    /// [`Self::persist_ui_key`] 是这一里唯一会**改错误行**的一步，而错误行也在签名里 ——
    /// 它对"这次到底落没落地"有最后发言权。
    fn cycle_lang(&mut self) {
        let next = self.ui.lang.next();
        // 写回之前先记下这一档原来是从哪层来的：命令行高于文件（§4.6），所以从 `--lang`
        // 启动的那一次点了也不会下次生效，这句话必须说，不然是"点了没反应"的假故障。
        let from_cli = self.ui.lang_from == settings::ValueSource::Cli;
        self.ui.lang = next;
        self.ui.lang_from = settings::ValueSource::MenuClick;
        if let Some(m) = self.menu.as_mut() {
            m.lang = next;
            m.force_draw = true;
        }
        self.refresh_panel_rows();
        eprintln!("ui: 语言 → {} ({})", next.self_name(), next.code());
        self.persist_ui_key(petstore::config::KEY_LANG, next.code(), from_cli);
    }

    /// 点"动画"那一行：full → reduce → off → full，**本进程立刻生效 + 当场写回**，
    /// 与 [`Self::cycle_lang`] 同一条路（`[ui] motion` 是 `write_ui_key` 认的两个键之一）。
    ///
    /// ⚠️ **这里不给菜单置 `force_draw`，也不是漏了**：`motion` 故意**不在** `menu::content_sig`
    /// 里，而 `menu_pixels_ignore_the_motion_tier` 那条门禁断言的是"三档下菜单像素逐字节相同"
    /// —— 这一行改的是**卡片动画**（burst 时长、SSR 白闪），不是此刻这张菜单。加一句
    /// `force_draw` 就是在唯一有"静止菜单 = 1 提交"预算的门面上重造 `69fd158` 那个形状：
    /// 每 tick 重投一帧像素完全相同的 buffer。改动**下一次抽取才看得见**。
    ///
    /// 面板反过来：设置面**确实要重算行**，因为 `settings_rows` 把 `motion.code()` 烘进了那一行
    /// 的正文，而正文进签名 ⇒ [`Self::refresh_panel_rows`] 换完行，下一 tick 闸门自己放行。
    /// 菜单那一行只是入口，它自己的字不含档位。
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

    /// 点菜单/面板上那个「概率」格子：见 [`published_rates_lines`]（不碰库、五行整）。
    fn rates_lines(&self) -> Result<Vec<String>, String> {
        published_rates_lines(self.ui.lang)
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
                self.ui_error = None;
                true
            }
            Err(msg) => {
                self.gacha_bailed(msg);
                false
            }
        }
    }

    /// 抽取层的出声处：stderr + 菜单/面板共用的错误行。**不 panic、不置 `exit`**。
    fn gacha_bailed(&mut self, msg: String) {
        eprintln!("gacha: {msg}");
        self.ui_error = Some(msg);
    }

    /// UI 侧（非抽取）的出声处，与 [`Self::gacha_bailed`] 同一条纹路：stderr + 那一行错误。
    /// 分两个函数只为了**前缀不说谎**：配置写回的失败打成 `gacha: …` 会把排查的人支到
    /// 数据库那边去。
    fn ui_bailed(&mut self, msg: String) {
        eprintln!("ui: {msg}");
        self.ui_error = Some(msg);
    }

    /// 点设置行 = **当场写回 `config.toml`**（拍板④：「窗口中显示相关的内容，并实现功能」）。
    ///
    /// 写法只有 [`petstore::config::write_ui_key`] 一处（读改写走 `toml_edit` 保 decor，落盘前
    /// 与落盘后各用**我们自己的** reader 判一遍，禁止 writer 拿自己的 parser 验自己 —— 五条规则
    /// 与"为什么不能拿平坦 reader 反推整份文件"都写在那里）。这里只管三件事：
    /// · **成功**只念 writer 返回的那个路径，不在 `App` 里再算一遍路径（两处各算一次早晚
    ///   漂移成两个文件，而用户会改哪一个全看运气）；消音那一条错误行是**既有约定**
    ///   （`ensure_gacha` 连库成功、`show_panel_lines` 读行成功、`do_pull` 抽取成功，三处各自
    ///   清空这一行）⇒ 这一行的语义是"最近一次失败"，不是"某个一直没修好的东西"；
    /// · **失败**要把"本进程已经改了"一起说清 —— 只报"失败"会让人以为什么都没变，而屏上
    ///   那一行此刻写的就是新值；
    /// · **优先关系不因写回而改变**：`--lang`/`--motion` 高于文件（§4.6 那条链），所以从
    ///   命令行启动的那次要明说"不带参数重启才看得见"，不然用户会以为点了等于没生效。
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
                self.ui_error = None;
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

    /// 错误行（连不上库、读库存失败、抽取失败、**写回 `config.toml` 失败**都靠它出声）。
    /// **菜单与面板共用这一条通道**：写它的只有两处 —— 抽取侧
    /// [`Self::gacha_bailed`]、UI 侧 [`Self::ui_bailed`]，而两块面都把它画进自己的标题带右段，
    /// 且**都把它混进内容签名**（`menu::content_sig` / `panel::content_sig`）。
    fn error_line(&self) -> String {
        self.ui_error.clone().unwrap_or_default()
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

        // 每一块附属面自己走一遍（**各自的 pool、各自的看门狗、各自的内容签名**）。放在宠物
        // `!configured` 早退之前：宠物的映射状态和菜单/面板该不该动帧毫无关系。
        self.update_menu(now);
        self.update_panel(now);

        // 第一次 configure 之前 surface 还没映射，`present` 会直接早退 ⇒ 这期间推进内核就等于
        // **把"内容变了"的 intent 扔掉**（宠物可能一直不显形）。所以这里干脆不推进。
        if !self.configured {
            self.last_wake = now;
            self.acc = 0.0;
            return;
        }

        // 恢复上次穿着 = **这一刻之后跑一次**（P3；FLOW §5 那张表要求的时刻就是"configured 之后、
        // discover 之后"）。排在早退之后有两个理由：surface 还没映射时 `force_draw` 没人接；
        // 而排在 tick 环之前是为了让恢复出来的那一格走**正常那一条**出图路径，不是另开一次提交。
        if self.restore_pending {
            self.restore_pending = false;
            self.restore_worn();
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
                // 走这条 ⇒ 附新 buffer，margin 的改动同一次 commit 生效。
                // 配件要按**状态**决定叠不叠（没烘过那一档就不叠），而状态只在 `intent.sprite`
                // 里 ⇒ 与 `(row, col)` 从同一个 `Option` 里一起取，编译器就看不出"有格没状态"
                // 那种运行时不存在、类型上活着的组合。
                let paint = intent.sprite.map(|s| ((s.row as u32, s.col as u32), s.key));
                if matches!(paint, Some(((r, c), k)) if self.draw_cell(r, c, k)) {
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
    ///
    /// `key` 一路传到 [`blit_cell`]，在那儿只用来问一件事：装备中的配件**为这一档状态烘过像素吗**
    /// （[`AccessoryManifest::covers`]）。没烘过 ⇒ 一帧都不叠。这不是"保守起见不画"：一件只在
    /// `idle` 烘过的帽子，在 `run` 里继续拿 `idle` 那一格的像素往本体上叠，画的就是**别的东西**，
    /// 而屏上看不出来。
    fn draw_cell(&mut self, row: u32, col: u32, key: AssetKey) -> bool {
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
            // 失败 ⇒ **整趟作废**（不 commit）。配件叠加失败时本体已经画对了，但那顶帽子只盖了
            // 一半，而"半顶帽子"在屏上和"帽子歪了"是同一种看不出根因的错；宁可得罪一次五档计数
            // （`Tick::Fault` 有自己的档位，且会把新 margin 冲出去）也不把它画出来。
            if let Err(e) = blit_cell(
                take,
                self.atlas.as_ref(),
                self.worn.as_ref(),
                (row, col),
                key,
                pw as u32,
                ph as u32,
                stride,
            ) {
                eprintln!("{e}");
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
        // 面板单列一行，**不并进宠物三档也不并进菜单**：这三块面的省电判据是三个不同的问题
        // （宠物 = 五档 vs tick；菜单 = 翻页才 +1；面板 = 开着一动不动而恒 0 增长）。混成一个
        // 数就读不出任何一个。互斥 ⇒ 这两行最多一行是"开着"，同时开着就是 bug 的现场。
        println!(
            "  面板: {}（{} 提交 / {} 看门狗，静止时应当恒 0 增长）",
            if self.panel.is_some() {
                "开着"
            } else {
                "没开"
            },
            self.counters.panel_commits,
            self.counters.panel_watchdogs,
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
        // 按**对象身份**分流：宠物 / 菜单 / 面板 / 别人的（光标 surface 也走这条路）。
        // 不分流的话，菜单的帧回调会把宠物的 `frame_pending` 解开（反之亦然），
        // 三块 surface 互相"替对方"放行 ⇒ 提交计数和延后补投全都读不出真相。
        if self.menu.as_ref().is_some_and(|m| &m.surface == s) {
            if let Some(mut m) = self.menu.take() {
                m.clear_wait();
                if m.force_draw {
                    let qh = self.qh();
                    let e = self.error_line();
                    if m.draw(&self.compositor, &qh, &e) {
                        self.counters.menu_commits += 1;
                    }
                }
                self.menu = Some(m);
            }
            return;
        }
        if self.panel.as_ref().is_some_and(|p| &p.surface == s) {
            if let Some(mut p) = self.panel.take() {
                p.clear_wait();
                if p.force_draw {
                    let qh = self.qh();
                    let e = self.error_line();
                    if p.draw(&self.compositor, &qh, &e) {
                        self.counters.panel_commits += 1;
                    }
                }
                self.panel = Some(p);
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
        // 只认宠物那块：光标 surface、菜单、面板也会报 enter，拿它们写 `on_output`
        // 等于"谁最后进屏谁定义舞台"，热插拔时会飘。附属面**故意不进这条路**（它们的
        // output 由 `open_panel_lines`/`open_menu` 建 surface 时按 `on_output` 定，改了舞台
        // 就等下一次点击重建那块面，没有"跟着宠物搬"这回事）。
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
        // 面板同一条：那块面被收回只说明"这一屏没了"，不说明"宠物该退出"。`take()` 而不是
        // `teardown()`：合成器已经关掉它，再走一遍 destroy 只是把已经失效的对象再问一次。
        if self.panel.as_ref().is_some_and(|p| &p.layer == l) {
            self.panel = None;
            eprintln!("panel: 合成器收回面板 surface（宠物继续跑）");
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
                let e = self.error_line();
                if m.draw(&self.compositor, &qh, &e) {
                    self.counters.menu_commits += 1;
                }
                self.menu = Some(m);
            }
            return;
        }
        if self.panel.as_ref().is_some_and(|p| &p.layer == l) {
            if let Some(mut p) = self.panel.take() {
                p.configured = true;
                p.force_draw = true;
                p.clear_wait();
                let qh = self.qh();
                let e = self.error_line();
                if p.draw(&self.compositor, &qh, &e) {
                    self.counters.panel_commits += 1;
                }
                self.panel = Some(p);
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
            // **两块附属面连带丢掉**：它们整块 input region 都吃点击，而没有指针就再也点不到
            // CLOSE —— 留着一块永久吞点击的不透明方块，正是 M4 那条 P0 的同一个形状。
            // 面板在这一批成为第三块面，所以它**必须和菜单同进退**，不能只补菜单那一半。
            self.close_menu();
            self.close_panel();
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
            // 四路分流：菜单 / 面板 / 宠物 / 别的 surface（光标那块的事件也在这里被丢掉）
            let on_menu = self.menu.as_ref().is_some_and(|m| m.surface == e.surface);
            let on_panel = self.panel.as_ref().is_some_and(|p| p.surface == e.surface);
            if !on_menu && !on_panel && e.surface != *self.layer.wl_surface() {
                continue;
            }
            if on_menu || on_panel {
                // 两块附属面**共用这一条臂**，因为它们对指针的语义完全相同：只用左键、
                // 只用 Press、其余事件只为光标服务（`menu_press_activates` 是那条"右键落进
                // 菜单不激活任何东西"的红线所在，面板照抄 —— 右键在面板上什么都不做，
                // 关面板要按 CLOSE 那一行，与关菜单同一个口径）。
                let local = (e.position.0 as f32, e.position.1 as f32);
                match e.kind {
                    PointerEventKind::Enter { .. } => self.set_cursor(conn),
                    PointerEventKind::Press { button, .. } => {
                        if menu::menu_press_activates(button) {
                            if on_menu {
                                self.on_menu_press(local);
                            } else {
                                self.on_panel_press(local);
                            }
                        }
                    }
                    // 面被点完可能当场关闭（CLOSE 项）⇒ Leave/Release 拿不到那块 surface，
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

    /// P2 的三条读数 = [`DressView::select`] 的三档输入，各打各的。
    ///
    /// 为什么这三条值得单独存在，而不是"真机点一下看看"：整条 `App::dress_view` 要连库要扫盘，
    /// 只有真机跑得起来 ⇒ "未拥有的件不出现"这一句就变成只有人眼能验。拆出纯函数后，判定这部分
    /// 由下面三条钉住，屏上那个 `*` 由 `card::tests::dress_page_star_marks_only_the_dress_page`
    /// 钉住，两边都能红。
    #[test]
    fn unowned_accessory_never_appears_while_the_owned_one_does() {
        let ids: Vec<String> = ["hat_a", "hat_b", "hat_c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let owned: Owned = ["hat_b"].into_iter().map(String::from).collect();
        let v = DressView::select(false, Some(&owned), &ids);
        // 可见的是**下标**而不是克隆 ⇒ `picked` 仍是"选中哪件"的唯一事实
        assert_eq!(v.visible, vec![1], "只有库存里那一件能出现");
        assert_eq!(v.withheld, 2, "出局数要说得出漏了几件（stderr 那一句读它）");
    }

    #[test]
    fn exempt_or_demo_makes_nothing_withheld() {
        let ids: Vec<String> = ["hat_a", "hat_b"].iter().map(|s| s.to_string()).collect();
        let v = DressView::select(true, None, &ids);
        assert_eq!(v.visible, vec![0, 1]);
        assert_eq!(
            v.withheld, 0,
            "放行这一档不许报出局数（那会把两句不同的话混成一句）"
        );
    }

    /// **失败关闭**那一档（`owned = None` = 库存这道**没读过**：连不上库 / 读失败）。
    ///
    /// 光断"什么都不给"抓不到两种坏实现：① 把 `None` 合并成"读过而答案是空集"（那 `--db` 打错
    /// 路径与"确实没抽到东西"就在屏上分不开了）；② 更坏的**永远返回空**。所以这条真正起作用的是
    /// 后半 —— **库存里有名字时必须一件不少地放行**，它证明这道门是读出来的，不是常量。
    #[test]
    fn unreadable_store_closes_the_door_while_a_real_set_opens_it() {
        let ids: Vec<String> = ["hat_a", "hat_b"].iter().map(|s| s.to_string()).collect();
        let closed = DressView::select(false, None, &ids);
        assert!(closed.visible.is_empty(), "没验过拥有就一件都不给");
        assert_eq!(closed.withheld, 2);
        let all: Owned = ["hat_a", "hat_b"].iter().map(|s| s.to_string()).collect();
        let open = DressView::select(false, Some(&all), &ids);
        assert_eq!(
            open.visible,
            vec![0, 1],
            "库存说都有 ⇒ 这道门必须真的放行，否则上面那些断言全在测一个常量"
        );
        assert_eq!(open.withheld, 0);
    }

    /// 豁免真值表四格（[`dress_exempt_for`]）。
    ///
    /// 第三格 = 用户 2026-11-06 专门拍板的那一格：给了 `--allow-unowned-accessories` 而**没给**
    /// `--db` ⇒ 放行（照 DRYRUN 那套），但帽子页**不**点 `*`（右端已有「演示」标，同一件事不给
    /// 两个记号）。这一格以前只有"起进程看屏幕"能验，而 `App` 离开 compositor 构造不起来。
    #[test]
    fn exemption_needs_the_flag_and_a_real_store() {
        assert!(
            dress_exempt_for(true, false),
            "开关 + `--db` ⇒ 这才是豁免态"
        );
        assert!(
            !dress_exempt_for(false, false),
            "没给开关，普通运行轮不许豁免"
        );
        assert!(
            !dress_exempt_for(true, true),
            "演示态放行但**不点星**：屏上已有「演示」标，两个记号 = 两个事实源"
        );
        assert!(
            !dress_exempt_for(false, true),
            "演示态没给开关也是放行（DRYRUN 那一套），但同样不许点星"
        );
    }

    /// **演示态一个字都不落库**（FLOW §9 P3 明写"这条要写进测试，防顺手修好"）。
    ///
    /// 这条要拿**一只真好用的内存库**来测，而不是拿 `None`：`None` 那一档证不了"演示态"起了作用
    /// （它连得上库而库里没写，才算挡住了那次写）。三档一次测完，顺序就是危险程度。
    #[test]
    fn demo_mode_writes_nothing_while_a_real_store_keeps_the_row() {
        let mut gacha = Some(Gacha::connect(None).expect("内存库该建得起来"));
        let read = |g: &Option<Gacha>| {
            g.as_ref()
                .unwrap()
                .store
                .equipped(petstore::DEFAULT_PROFILE)
        };
        // ① 演示态：库就在手边，也不许写
        assert!(
            matches!(
                write_equip(&mut gacha, true, "head", "cap").unwrap(),
                WornWrite::DemoSkipped
            ),
            "演示态要的是 DemoSkipped 这个**结论**，不是 Ok(())（调用方按结论决定打哪一行）"
        );
        assert!(
            read(&gacha).unwrap().is_empty(),
            "无 `--db` 那一轮写进内存库 = 对用户说「记下了」而它必然不还在"
        );
        // ② 同一只库、同一个动作，只是这轮不是演示态 ⇒ 必须真写得进去
        assert!(matches!(
            write_equip(&mut gacha, false, "head", "cap").unwrap(),
            WornWrite::Written
        ));
        assert_eq!(
            read(&gacha).unwrap(),
            vec![("head".to_string(), "cap".to_string())],
            "①②合起来才证明拦下来的是 `dry_run` 这个条件本身"
        );
        // ③ 连不上库（`None`）不许 panic，也不许编出个"写好了"的结论
        let mut none = None;
        assert!(matches!(
            write_equip(&mut none, false, "head", "cap").unwrap(),
            WornWrite::NoStore
        ));
    }

    /// **UI 那两个写函数写的，就是下一个进程要打开的那只文件**（P3 唯一一条走完的写链）。
    ///
    /// 为什么单独立一条：上面几条测 `write_equip`/`clear_equip` 用的是**内存库**，而
    /// `petstore/tests/equipped_roundtrip.rs` 测的是 **`Store` 本体** —— 两边都绿而中间没接上，
    /// 是一个真实存在的形状（本项目"两个事实源互不相认"那一族，§9.2 第 20 条就是这么登记的）。
    /// 会两两不一致的三件事至少有四个：UI 挑的 profile、UI 打开的路径、恢复时读的那只库、
    /// 恢复时读的那张表。⇒ 这里走 UI 的那两个函数 → **落盘** → 换一个 `Gacha` 重新打开同一路径
    /// → 从 `plan_restore` 的输入端读回来，一条链走完，末尾还带一次正面控制。
    #[test]
    fn the_ui_writes_the_very_file_the_next_process_opens() {
        let dir = tempfile::tempdir().expect("临时目录该建得起来");
        let path = dir.path().join("opet.db");
        let path_str = path.to_str().expect("临时路径该是 UTF-8");

        // 进程 A：穿上 → 换一顶 → 脱下 → 再穿上。净效果只该剩最后一笔。
        let mut a = Some(Gacha::connect(Some(path_str)).expect("带 `--db` 那条路该连得上文件库"));
        for id in ["cap", "beanie"] {
            assert!(matches!(
                write_equip(&mut a, false, "head", id).unwrap(),
                WornWrite::Written
            ));
        }
        assert!(matches!(
            clear_equip(&mut a, false, "head").unwrap(),
            WornWrite::Written
        ));
        assert!(matches!(
            write_equip(&mut a, false, "head", "cap").unwrap(),
            WornWrite::Written
        ));
        drop(a);

        // 进程 B：同一路径重新打开 ⇒ 读到的必须是最后那一笔，而不是空表
        let b = Some(Gacha::connect(Some(path_str)).expect("第二个进程该开得了同一只文件"));
        let rows = b
            .as_ref()
            .unwrap()
            .store
            .equipped(petstore::DEFAULT_PROFILE)
            .expect("读 equipped 不该失败");
        assert_eq!(
            rows,
            vec![("head".to_string(), "cap".to_string())],
            "写了四轮而文件里没有净效果 ⇒ 那些笔只活在进程 A 的连接上（J3 必然红）"
        );
        // 正面控制：读回来的这一行必须真能被恢复穿上，而不是"库里对而穿不上"。
        let ids: Vec<String> = ["cap".into()].to_vec();
        let slots: Vec<String> = ["head".into()].to_vec();
        let plan = plan_restore(&rows, &ids, &slots, &[0]);
        assert_eq!(
            plan.wear,
            Some(0),
            "文件里那一行必须能走回 `wear`：{:?}",
            plan.notes
        );
    }

    /// 脱下 = 删那一格。同上一条的三档，方向反过来（"删不掉"的后果是重启帽子自己回来）。
    #[test]
    fn unworn_clears_the_row_and_demo_has_nothing_to_clear() {
        let mut gacha = Some(Gacha::connect(None).expect("内存库该建得起来"));
        write_equip(&mut gacha, false, "head", "cap").unwrap();
        assert!(matches!(
            clear_equip(&mut gacha, true, "head").unwrap(),
            WornWrite::DemoSkipped
        ));
        assert!(
            !gacha
                .as_ref()
                .unwrap()
                .store
                .equipped(petstore::DEFAULT_PROFILE)
                .unwrap()
                .is_empty(),
            "演示态的「脱下」也不许动库：屏上没了而库里还记着，是**另一种**谎，不是同一种"
        );
        assert!(matches!(
            clear_equip(&mut gacha, false, "head").unwrap(),
            WornWrite::Written
        ));
        assert!(
            gacha
                .as_ref()
                .unwrap()
                .store
                .equipped(petstore::DEFAULT_PROFILE)
                .unwrap()
                .is_empty()
        );
        let mut none = None;
        assert!(matches!(
            clear_equip(&mut none, false, "head").unwrap(),
            WornWrite::NoStore
        ));
    }

    /// 手工往 `equipped` 塞一行 ⇒ **报一句并出局**，不许静默穿上（J2 的后半）。
    ///
    /// 两种手工形状都测：未拥有的那件（磁盘有、库存没有）与槽位不符的那行（清单说 `neck`
    /// 而库里写 `head`）。只测前者会留下后者：后者更阴 —— 像素照解照画，帽子挂错部位，
    /// 而屏幕上没有任何一处会说"这一行是我自己塞的"。
    #[test]
    fn a_hand_made_equipped_row_is_refused_and_named() {
        let ids: Vec<String> = ["hat_a", "hat_b"]
            .iter()
            .copied()
            .map(String::from)
            .collect();
        let slots: Vec<String> = ["head".into(), "head".into()].to_vec();
        // 库存只有 hat_a ⇒ visible 只有 0（这一档直接沿用 P2 那次判定的输出）
        let unowned = plan_restore(
            &[("head".to_string(), "hat_b".to_string())],
            &ids,
            &slots,
            &[0],
        );
        assert!(unowned.wear.is_none(), "未拥有的那一件绝不被恢复穿上");
        assert_eq!(unowned.notes.len(), 1);
        assert!(
            unowned.notes[0].contains("hat_b") && unowned.notes[0].contains("库存里没有这一件"),
            "出局要点名是哪一件、错在哪一道：{:?}",
            unowned.notes
        );
        // 槽位不符：库里说 head，清单说 neck ⇒ 这一行不是这套代码写出来的
        let crossed = plan_restore(
            &[("head".to_string(), "hat_a".to_string())],
            &ids,
            &["neck".into(), "head".into()],
            &[0, 1],
        );
        assert!(crossed.wear.is_none());
        assert!(
            crossed.notes[0].contains("手工"),
            "要说清这是手工行，不然用户会去查库存而那儿根本没毛病：{:?}",
            crossed.notes
        );
        // 正向对照：同一批输入把槽位改对 ⇒ 必须真的穿上那一格。
        // 没有这一句，上面四条断言全可能是在测一个"永远返回 None"的函数。
        let ok = plan_restore(
            &[("neck".to_string(), "hat_a".to_string())],
            &ids,
            &["neck".into(), "head".into()],
            &[0, 1],
        );
        assert_eq!(ok.wear, Some(0), "四道全过就该穿上（门禁不能只会说不）");
        assert!(ok.notes.is_empty());
    }

    /// 上次穿的帽子文件没了（被删、或五道门禁没过而被 discovery 跳掉）⇒ 点名，不静默。
    ///
    /// 这条**不需要**恢复代码自己再跑一遍五道：库里那一行要能穿，前提是它出现在 `ids` 里，
    /// 而 `ids` 只由 [`discover_accessories`] 活着交出来 ⇒ "不在磁盘上/不过门禁"这两句是同一条。
    #[test]
    fn a_vanished_item_is_named_instead_of_silently_not_worn() {
        let ids: Vec<String> = ["hat_a".into()].to_vec();
        let slots: Vec<String> = ["head".into()].to_vec();
        let plan = plan_restore(
            &[("head".to_string(), "gone_hat".to_string())],
            &ids,
            &slots,
            &[0],
        );
        assert!(plan.wear.is_none());
        assert!(
            plan.notes[0].contains("gone_hat") && plan.notes[0].contains("找不到这一件"),
            "「上次穿的 X 现在装不上」要带上 X 叫什么：{:?}",
            plan.notes
        );
    }

    /// 库里记着好几格 ⇒ 只回第一件（M1 = 一次一件），其余**只报告、不删**。
    ///
    /// "第一件"能重放的前提是行序由库给（`Store::equipped` 的 `ORDER BY slot, item_id`）⇒
    /// 这一条同时是那次排序的消费方：排序坏了这里就是随机一顶。
    #[test]
    fn only_the_first_of_several_rows_comes_back_and_the_ledger_stays_intact() {
        let ids: Vec<String> = ["back_pack".into(), "cap".into()].to_vec();
        let slots: Vec<String> = ["back".into(), "head".into()].to_vec();
        let visible: Vec<usize> = vec![0, 1];
        let plan = plan_restore(
            &[
                ("back".to_string(), "back_pack".to_string()),
                ("head".to_string(), "cap".to_string()),
            ],
            &ids,
            &slots,
            &visible,
        );
        assert_eq!(
            plan.wear,
            Some(0),
            "取行序第一件（back 排在 head 前，因为那个 ORDER BY）"
        );
        assert_eq!(plan.notes.len(), 1);
        assert!(
            plan.notes[0].contains("M1") && plan.notes[0].contains("留着"),
            "要说「这一件留在库里没删」，删掉等于丢用户没让我丢的东西：{:?}",
            plan.notes
        );
    }

    /// **"改了像素"与"记进 equipped"在源码里必须是同一个动作**（P3 的接线门禁）。
    ///
    /// 这条不是洁癖，是变异实测逼出来的：本轮把 `equip` 里那句 `persist_worn` 删掉，
    /// **全套 526 条全绿**（变异 G）。原因很简单 —— `App` 在没有 compositor 的机器上构造不出来，
    /// 所以"屏上戴着一顶而库里没记这笔"这个本项目反复出现的形状，在 headless 里**没有读数**。
    /// 修法不是再补三句"记得调用"，那是同一个洞换地方：把三处装载合成 [`App::wear`] 一个动作
    /// （解码 → 上身 → 记账），这条门禁再钉住"只有一个落点"这件事。⇒ 以后要红，得有人往
    /// `wear` 外面写 `self.worn = Some(` 或删掉 `wear` 里那句记账，两种都在此点名。
    ///
    /// 与 `cli.rs` 的 `every_flag_advertised_in_usage_exists` 同族（那条查 `--help` 与解析器是否
    /// 各说一套）。**读源码文本的门禁天生脆**，所以这里只数三件最小的事，且每一条都带
    /// "为什么是这一条"的断言文本；重构改名会红，那是**要人来看一眼**的红，不是误报。
    #[test]
    fn pixels_and_the_ledger_are_written_by_one_function_only() {
        let src = include_str!("app.rs");
        // 只数**测试模块之外**的代码：这条门禁自己的源码里就写着它数的那些字面量，
        // 不切掉的话它数到的是 5 而不是 1（本轮实测），而那是一个永远不红的假门禁的反面 ——
        // 一个永远绿的读数。
        let code = src
            .split("\nmod tests {")
            .next()
            .expect("本文件有 `mod tests`，切不出来说明结构变了");

        // ① 把配件放上身的赋值**全仓库只许有一处**，而那一处必须在 `wear` 的函数体里。
        let body = |name: &str| -> String {
            let at = format!("fn {name}(");
            let start = code
                .find(&at)
                .unwrap_or_else(|| panic!("找不到 `{at}` ⇒ 这条门禁白跑"));
            let rest = &code[start..];
            let end = rest[at.len()..]
                .find("\n    fn ")
                .or_else(|| rest[at.len()..].find("\n    }"))
                .map(|d| at.len() + d)
                .unwrap_or(rest.len());
            rest[..end].to_string()
        };
        let wears = body("wear");
        assert!(
            code.matches("self.worn = Some(").count() == 1,
            "把配件放上身写了 {} 处 ⇒ 有一处绕过了 `wear`，而它不会记 equipped（本轮变异 G 的形状）",
            code.matches("self.worn = Some(").count()
        );
        assert!(
            wears.contains("self.worn = Some("),
            "唯一那处上身必须在 `wear` 里，不然它就不在记账的那条路上"
        );

        // ② `wear` 自己必须带那句记账（① 只保证"别处没有"，不保证"这里有"）。
        assert!(
            wears.contains("persist_worn"),
            "`wear` 里没有 `persist_worn` ⇒ 四处装载全都不记账，而 headless 测不出来"
        );

        // ③ 五个装载点都真的走 `wear`（少一个就是"那条路又自己写了一遍像素"）。
        //
        // ⚠️ **这个数从 3 变成 4 是 P4 那一格（收藏册点名）逼出来的，不是顺手加的**：那一条
        // **不能借道 [`App::equip`]**，因为 `equip` 的早退读的是 `picked`，而收藏册要在
        // "身上是 A、点的是 B"时真的换过去（完整理由在 `pick_and_wear` 那条注释上）。
        // ⚠️ **从 4 变成 5 是 P5（换本体）那一格逼出来的，同一条理由**：换完本体后 `candidates`
        // 是**新可见集**，旧帽子如果还在里面，它的下标已经不是身上那格的旧下标 ⇒ 只能按 id 重认
        // 一次再走 `wear`。在这条路上自己写那四行赋值 = 屏上戴着帽子而 `equipped` 记的是上一只
        // 本体的那一件（= 变异 G 那一族，只是换了主语）。
        // 第六个调用点如果将来出现，红的是这一条，而那正是要人看一眼"它能不能复用前五个"的时刻。
        assert_eq!(
            code.matches("self.wear(").count(),
            5,
            "`wear` 应当被穿上 / 换一件 / 启动恢复 / 收藏册点名 / 换本体后重穿幸存那件 五处共用，\
             实际调用点 {} 个",
            code.matches("self.wear(").count()
        );
        // ④ 新那条路只有一个入口，别让它被复制一份到某个 `match` 臂里（复制的那份会自己
        //    决定"要不要先释放"，而那是这一整条设计的全部安全性所在）。
        assert_eq!(
            code.matches("self.pick_and_wear(").count(),
            1,
            "`pick_and_wear` 只许被 `on_panel_press` 的 `Wear` 那一臂调用，实际 {} 处",
            code.matches("self.pick_and_wear(").count()
        );

        // ⑤ **`pick_and_wear` 内部的次序**就是这一整条设计的安全性所在，而 headless 构造不出
        //    `App` ⇒ 这件事除了读文本没有别的读数（①②③④ 同一族，也是 P3 变异 G 那次的教训）。
        //    次序 = 判定（读**旧** `picked`）→ 释放身上那件 → 写 `picked` → 装载：
        //    · 颠倒"判定/写" ⇒ 最常见的那一次点击（身上 A、点 B）被读成"已经穿着这一件"
        //      ⇒ 静默不动 + 一句谎话；
        //    · 删掉"释放" ⇒ 两份 45 MiB 同时在内存，正是历史上 `VmHWM 148,076 kB` 那次越线的成因。
        //    两次变异抽查各打这一条，红了就说明这条门禁有读数。
        let pick = body("pick_and_wear");
        let at = |needle: &str| {
            pick.find(needle)
                .unwrap_or_else(|| panic!("`pick_and_wear` 里没有 `{needle}` ⇒ 那条次序被拆了"))
        };
        assert!(
            at("plan_pick_wear(") < at("self.picked = i"),
            "先写 `picked` 再判定 ⇒ `AlreadyWorn` 恒真，换一件那次点击会被静默吞掉"
        );
        assert!(
            at("self.take_worn()") < at("self.picked = i"),
            "释放排在写 `picked` 之后 ⇒ 判定读到的已经不是用户点之前的世界"
        );
        assert!(
            at("self.picked = i") < at("self.wear(i)"),
            "装载排在写 `picked` 之前 ⇒ `wear` 装的是上一件，而屏上下标已经换"
        );

        // ⑥ **P5：换本体这一族的同一件事，换了四个主语**。四条计数各钉一个"唯一落点"，
        //    第五段（下面 ⑦）钉的是那九步的**次序**。全部理由同一个：`App` 在没有 compositor
        //    的机器上构造不起来 ⇒ "屏上是那只而记忆里是另一只"、"屏上一个像素没动而日志说换了"
        //    这两类形状在 headless 里除了读文本没有别的读数（①–⑤ 与 P3 变异 G 同一族）。
        assert_eq!(
            code.matches("self.swap_body(").count(),
            1,
            "`swap_body` 只许被 `on_panel_press` 的 `Body` 那一臂调用，实际 {} 处 ⇒ 多出来的那份\
             会自己决定九步的次序",
            code.matches("self.swap_body(").count()
        );
        assert_eq!(
            code.matches("self.persist_body(").count(),
            1,
            "`equipped` 的 `body` 行只许在 `swap_body` 末尾写一次（先像素后账），实际 {} 处",
            code.matches("self.persist_body(").count()
        );
        assert_eq!(
            code.matches("self.manifest = ").count(),
            1,
            "本体清单只有一个写入点（`swap_body` 第 5 步）⇒ 第二处赋值就是\"清单是新的而 atlas \
             是旧的\"那个半换状态的另一半（实际 {} 处）",
            code.matches("self.manifest = ").count()
        );
        // ⚠️ needle 带 `&`：`atlas.reload(` 这个字面量在 [`Atlas::reload`] 那条**文档注释**里
        // 也出现过（它把这条门禁的口径写在注释上），不带 `&` 会数到 2 而那是文档不是代码。
        assert_eq!(
            code.matches("atlas.reload(&").count(),
            1,
            "原地重载只许有一处 ⇒ 写成 load-then-swap（先 `Atlas::load` 再换掉）峰值就是**两只\
             本体 ≈147 MB，而它落在 ≤200 MiB 门禁内 ⇒ 什么都不会红**，只有这一条 + 验收那一次 \
             `VmHWM` 能抓（实际 {} 处）",
            code.matches("atlas.reload(&").count()
        );

        // ⑦ **九步的次序**（本轮最承重的四行了）。`reload` 是不可回头的那一条线，所以：
        //    判定与"卸下旧帽"排在它**前**（失败时屏上还是原来那只、账一个字不动），
        //    记账、`drawn_cell = None`、`candidates` 重跑排在它**后**。
        //    · 删掉 `drawn_cell = None` = **本轮的变异 G**：四份产物的行表逐字节相同 ⇒
        //      `cell != drawn_cell` 那条永不开 ⇒ 545 条全绿而真机 J1 失败（屏上静默不变）。
        //      ⇒ 这一行**只有**下面这条 textual 读数，真机上唯一的读数是一张截图。
        let swap = body("swap_body");
        let said = |needle: &str| {
            swap.find(needle)
                .unwrap_or_else(|| panic!("`swap_body` 里没有 `{needle}` ⇒ 那九步被拆了"))
        };
        assert!(
            said("plan_swap_body(") < said("atlas.reload(&"),
            "判定排在解像素之后 ⇒ 被拒的那一次已经改过图集，而日志说的是\"没换\""
        );
        assert!(
            said("self.take_worn()") < said("atlas.reload(&"),
            "旧帽没先卸下就去解新本体 ⇒ 峰值 = 本体 + 旧帽 + 新本体（M1 档那条 148,076 kB）"
        );
        assert!(
            said("atlas.reload(&") < said("self.persist_body("),
            "账排在像素之前 ⇒ 解码失败那一轮库里记着一只屏上没有的本体"
        );
        assert!(
            said("atlas.reload(&") < said("self.drawn_cell = None"),
            "`drawn_cell` 的复位没排在 `reload` 之后 ⇒ 换本体是一次屏上静默不变（变异 G 的 P5 版）"
        );
        assert!(
            said("discover_accessories(") < said("let fresh = self.dress_view()"),
            "`picked` 在 `candidates` 重跑之前被改写 ⇒ 拿旧下标去索引新表，点下去是另一件"
        );

        // ⑥′ **第 6 步那个库位置**：11-07 交互主轮实测抓到的一刀，而它**不在**上面任何一条的射程里
        // （次序全对、只是根取错了）。`swap_body` 当时写的是 `pet_root()` ⇒ 配件库被扫成本体库那个
        // 目录，读数 `accessories: 库 …/pets_p5 ⇒ 0 件可用` ⇒ 换完本体帽子当场掉、这一轮再也穿不上
        // 任何配件，而第 7 步按 id 重认幸存者永远查不到。变异（把 `accessory_root` 换回 `pet_root`）
        // 在 headless 里 545 条全绿 ⇒ 这一族的唯一防线是这两行 + 一张截图。
        assert!(
            !swap.contains("pet_root("),
            "`swap_body` 里出现了 `pet_root(` ⇒ 重扫配件用的是本体库的根，换完本体配件全灭"
        );

        // ⑧ **启动恢复那条路上，剥壳必须排在判定之前**（变异 M6 唯一的读数）。
        //
        // `a_body_row_is_not_an_accessory_note` 测的是 `split_body_row` 与 `plan_restore` 这两个
        // **纯函数**，而 M6 那一刀（把 `restore_worn` 里的 `&accessories` 换回 `&rows`）**实测
        // 204 条全绿** ⇒ 那一族的洞原样复发：判定对、接线错，headless 里没人读到接线。这一族
        // 从 P3 的变异 G 起就是同一个形状，而防线从来只有一种 = 读源码文本。
        let rw = body("restore_worn");
        let said_rw = |needle: &str| {
            rw.find(needle)
                .unwrap_or_else(|| panic!("`restore_worn` 里没有 `{needle}` ⇒ 剥壳那一步被拆了"))
        };
        assert!(
            said_rw("split_body_row(") < said_rw("plan_restore(&accessories"),
            "`plan_restore` 收到的不是剥过壳那张表 ⇒ 库里有 `body` 行时每轮启动多一句假话\
             （「磁盘上找不到这一件」说的是本体）"
        );
        assert!(
            !rw.contains("plan_restore(&rows"),
            "`plan_restore` 直接吃了整表 ⇒ 同上一条，只是这次是文本形状变了而次序还在"
        );

        // ⑨ **两道格子档门禁都必须是整块 `Grid` 比较**（变异 M5 的两次读数）。
        //
        // `plan_swap_body_partitions_the_four_outcomes` 那张真值表**挡不住这一刀**：`same_grid`
        // 在那里是一个传进来的 `bool`，把生产它的那一句放松成只比 `w/h`，真值表照旧全绿。
        // ⇒ 这一族的读数只有两处文本：扫盘那一道（`check_body`）与运行时第二道保险
        // （`swap_body` 里算 `same_grid` 那一句）。放松任何一处的后果都不是"报错"而是
        // **换上一只像素错位的新本体**（`rows` 决定解码缓冲长度，`cols` 决定寻址），
        // 而那在屏上是一只能看的宠物 —— 只有这两条 + `atlas.reload` 的长度自检能红。
        assert_eq!(
            code.matches("manifest.grid != *cur").count(),
            1,
            "扫盘那道档门禁不是整块 `Grid` 比较了（实际 {} 处）⇒ 判据放松成 `w/h` 的那一天，\
             行表不同的本体会被放进来",
            code.matches("manifest.grid != *cur").count()
        );
        assert_eq!(
            code.matches("b.grid == self.manifest.grid").count(),
            1,
            "运行时第二道保险必须是整块比较；放松成 `w/h` 只比尺寸不比行数/列数 ⇒\
             一只 8 行的本体被换进 4 行的缓冲（实际 {} 处）",
            code.matches("b.grid == self.manifest.grid").count()
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

    /// 一串面板上的字**画得出来**吗（每个码位都在字体子集里）。缺字在画布上是一个方框或
    /// 一个 `?`，像素门禁全绿而玩家读到的是断字 —— 判据① 那一族。
    fn assert_sayable(text: &str, lang: Lang, where_: &str) {
        if let Some(c) = crate::text::missing(text) {
            panic!("{lang:?} 的{where_}「{text}」里的 {c:?} 不在字体子集里 ⇒ 屏上是一个方框");
        }
    }

    /// 面板六屏（设置默认 + 收藏册三 tab + 被「概率」/「复核」借走的 setting 面两屏）×
    /// 四 locale × **两档缩放**：每一串字都要①画得出来、②一个字节都不被截。
    ///
    /// 为什么这条是这一批新加的，而 `panel.rs` 那 11 条不算覆盖：面板的行**从这一批起才是
    /// 像素**（上一批它们只进 stderr，截不断、缺不缺字都没人负责），而它的入口是鼠标 ——
    /// 我绝不注入点击 ⇒ "点下去那一屏破不破版"在真机上永远只有用户本人撞得到。没有这条，
    ///  ja/ko 的面板正文就是**从来没被任何人看过**的东西。
    ///
    /// 行一律来自**真生产者**（`Gacha::tab_lines` / `Gacha::verify_lines` /
    /// [`published_rates_lines`] / `panel::settings_rows`），不手写样本：手写的行永远比真的行短。
    /// 预算算法与 `card::paint_panel` **逐字同构**：可点的行让出强调条（`menu::label_rect`）、
    /// 只读行占满行底（`menu::row_text`），换算只走 `menu::budget_for`；标题带走
    /// `menu::line_budget_px`。两边不同构的话这条绿而屏上截字。
    ///
    /// ⚠️ **错误行不在这条的管辖内**：它是 `petstore` 的原文（§7-R9 不译），长就得长，
    /// `card::paint_panel` 对它**允许截断**（预算让位给标题带那一句是明写的取舍）。
    #[test]
    fn every_panel_row_is_sayable_and_never_truncated() {
        let mut g = Gacha::connect(None).expect("内存库该建得起来");
        // 真抽十连：让「已获得」有名字行、「还缺」有缺项、「最近」装满五行、「复核」有链头。
        g.pull(10, true, Lang::Zh, 1.5).expect("十连该成功");
        let mut screens = 0usize;
        // 收藏册逐件那本的页数**由生产者算**（不是写死一个算式）：条目一多，写死的算式就变成
        // 猜硬币，而这条门禁的意义恰恰是"没人能悄悄少量一屏"。
        let mut album_screens = 0usize;
        // ⚠️ `scale` 在**外层**：产行的那一刻读它（`missing_lines` 按宽度折名字），画的那一刻读
        // 同一个数。把这两层拆开选就等于"用 1× 折出来的行去判 1.5× 的预算"，那是两个口径。
        for scale in [1.0f32, 1.5] {
            for lang in Lang::ALL {
                let mut sets: Vec<(PanelKind, Vec<panel::PanelRow>)> = vec![(
                    PanelKind::Settings,
                    panel::settings_rows(lang, Motion::Full),
                )];
                for tab in CollectionTab::ALL {
                    let lines = g
                        .tab_lines(tab, lang, scale)
                        .expect("收藏册三屏都该读得出来");
                    sets.push((
                        PanelKind::Collection { tab, page: 0 },
                        panel::collection_rows(lang, &lines),
                    ));
                }
                // 「已获得」那一本的**逐件 + 分页**：每一页都是一屏，都得量。
                //
                // 可点性按**最坏情况**给 —— `ids` 交全部 catalog 配件名、`visible` 交全部下标
                // ⇒ 配件行全是 `Wear`，走的是那条**更窄**的预算（[`panel::ROW_LABEL_W`] 209 而不是
                // 只读行的 216）。量宽的那档而屏上画窄的那档，就是这条门禁最没用的形状。
                //
                // ⚠️ 上面那条 `tab_lines(Owned)` 那一屏**留着**：它现在不再是一整屏（Owned 走
                // 分页那条路），但它的五行汇总**就是**下面 `entries` 尾部那五行 ⇒ 留它是把
                // "报表那五行"单独钉住，而分页那一段钉的是"逐件那几张行 + 页码控件"。
                let ids: Vec<String> = g.catalog.accessories().map(|i| i.id.clone()).collect();
                let visible: Vec<usize> = (0..ids.len()).collect();
                // 本体那一侧同一条**最坏情况**纪律（P5）：`pet_ids` 交全部 catalog 宠物、
                // `pet_visible` 交全部下标 ⇒ 宠物行全是 `Body`，走的也是那条窄预算。
                let pet_ids: Vec<String> = g.catalog.pets().map(|i| i.id.clone()).collect();
                let pet_visible: Vec<usize> = (0..pet_ids.len()).collect();
                let owned = g.store.owned().expect("刚抽完十连的内存库该读得出库存");
                let entries = g
                    .album_entries(&owned, &ids, &visible, &pet_ids, &pet_visible)
                    .expect("逐件收藏册的行该读得出来");
                let mut content = album_rows(&entries, scale);
                content.extend(
                    g.collection_lines(lang)
                        .expect("汇总那五行该读得出来")
                        .into_iter()
                        .map(|text| panel::PanelRow {
                            item: panel::PanelItem::Info,
                            text,
                        }),
                );
                let first = panel::album_page(lang, &content, 0);
                album_screens += first.pages;
                let mut shown = 0usize;
                for p in 0..first.pages {
                    let screen = panel::album_page(lang, &content, p);
                    shown += screen
                        .rows
                        .iter()
                        .filter(|r| {
                            r.item != panel::PanelItem::More && r.item != panel::PanelItem::Close
                        })
                        .count();
                    sets.push((
                        PanelKind::Collection {
                            tab: CollectionTab::Owned,
                            page: screen.page,
                        },
                        screen.rows,
                    ));
                }
                // **覆盖**也要点名：分页分错了会把同一件画两遍而把另一件永远藏起来，
                // 那种 bug 在"每屏都 ≤ 六行、每行都画得出来"里长得完全正确。
                assert_eq!(
                    shown,
                    content.len(),
                    "{lang:?} 在 {scale}× 下翻了 {} 页只画到 {shown} 条，而逐件收藏册有 {} 条 ⇒ 有件永远见不到",
                    first.pages,
                    content.len()
                );
                // 「概率」/「复核」把设置面**借走**的那两屏：`kind` 仍是 `Settings` ⇒ 标题带
                // 还是「设置」，而行是别的内容。这两屏是这一批新出现的形状，没人量过。
                let rates = published_rates_lines(lang).expect("公示读得出来");
                sets.push((PanelKind::Settings, panel::collection_rows(lang, &rates)));
                let verify = g
                    .verify_lines(lang, scale)
                    .expect("刚抽完十连的内存账本该自洽");
                sets.push((PanelKind::Settings, panel::collection_rows(lang, &verify)));

                for (kind, rows) in &sets {
                    screens += 1;
                    assert!(
                        !rows.is_empty(),
                        "{lang:?} 的 {kind:?} 一行都没有 ⇒ 生产者出字的地方根本没被量到"
                    );
                    assert!(
                        rows.len() <= panel::MAX_ROWS,
                        "{lang:?} 的 {kind:?} 有 {} 行 > 一屏的 {}",
                        rows.len(),
                        panel::MAX_ROWS
                    );
                    let head = panel::header_text(*kind, lang);
                    assert_sayable(&head, lang, "标题带");
                    assert_eq!(
                        menu::fit_to(&head, menu::line_budget_px(scale), scale),
                        head,
                        "{lang:?} 的标题带「{head}」在 {scale}× 下被截 ⇒ 那一屏读不出自己在哪一本第几页"
                    );
                    for (r, row) in panel::layout(rows).iter().zip(rows) {
                        let label = if r.item.actionable() {
                            menu::label_rect(r.as_rect())
                        } else {
                            menu::row_text(r.as_rect())
                        };
                        assert_sayable(&row.text, lang, "行");
                        let budget = menu::budget_for(label.w, scale);
                        assert_eq!(
                            menu::fit_to(&row.text, budget, scale),
                            row.text,
                            "{lang:?} 的 {kind:?}/{:?} 行「{}」在 {scale}× 下被截（预算 {budget}）",
                            r.item,
                            row.text
                        );
                    }
                }
            }
        }
        // 屏数要点名：少一个生产者这条照样绿（"没量到"和"量过了且对"在两行断言里长得一样）。
        // ⚠️ 这里**不写死**收藏册那一本的页数（`album_screens` 是逐轮累加的真实页数）⇒
        // 加一件能穿的配件、抽到新的一档，页数会变而这条跟着变；写死就是一个"内容一多就
        // 变成猜硬币"的门禁。
        let combos = 2 * Lang::ALL.len();
        assert_eq!(
            screens,
            combos * (panel::CollectionTab::ALL.len() + 3) + album_screens,
            "应当量满 两档缩放 × 四 locale × (收藏册三屏 + 设置默认 + 概率 + 复核) 再叠加逐件册的每一页（每轮 {} 页）",
            album_screens / combos
        );
        assert_eq!(
            album_screens % combos,
            0,
            "每轮 (缩放 × locale) 的页数应当一致（不一致 = 分页读了语言或缩放，而它不该读）"
        );
    }

    // ==================== P4 收藏册点名 ====================
    //
    // 这四条共同钉住本轮唯一的新形状：**一行 = 一件，格子里的下标 = 换装页 cycle 的那套下标**。
    // 之所以全是纯函数级的断言：`App` 在没有 compositor 的机器上构造不起来（P2/P3 同一理由，
    // 方法论也是同一套 —— 见 [`DressView::select`] 与 [`plan_pick_wear`] 的注释）。

    /// P4 的 fixture：**一份把稀有度序打乱的合成目录**。
    ///
    /// ⚠️ 目录里的数组顺序**故意**不是"同档相邻"（`hood_night` 那件 R 夹在 N 前面）：
    /// [`Gacha::album_entries`] 按 `Rarity::ALL` × 档内序遍历 ⇒ 条目顺序由**档**决定，而
    /// 行上的载荷 `Wear(i)` 里的 `i` 必须是 `candidates` 的下标。fixture 里**每一件**的
    /// "目录枚举序号"与"`ids` 下标"都不同 ⇒ 谁把那句 `ids.iter().position(..)` 顺手写成
    /// 枚举计数器，下面第一条断言会当场点名是哪一件（而"两个序号恰好相等"的 fixture 永远
    /// 看不出来 —— 那是本项目反复抓的"测试测的是一个常量"的形状）。
    fn album_fixture() -> Gacha {
        let real = Catalog::default_v1().expect("默认目录该读得动");
        let pick = |id: &str| {
            real.items
                .iter()
                .find(|i| i.id == id)
                .unwrap_or_else(|| panic!("fixture 要的 {id} 不在默认目录里"))
                .clone()
        };
        let mut g = Gacha::connect(None).expect("内存库该建得起来");
        g.catalog = Catalog {
            format: real.format,
            id: real.id.clone(),
            banner_id: real.banner_id.clone(),
            items: vec![
                pick("hood_night"), // R · 夜兜
                pick("bell_gold"),  // N · 金铃
                pick("spacecat"),   // N · 太空猫（宠物：下标在**本体那一张表**里，不在配件表里）
                pick("cap_blue"),   // N · 蓝帽
                pick("hat_straw"),  // SSR · 草帽（"抽到了而磁盘没产物"那一件）
            ],
        };
        g
    }

    fn owned_of(ids: &[&str]) -> Owned {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// `candidates`（= 磁盘上真有产物、且匹配本体的那几件）的下标表，顺序与目录遍历序无关。
    fn album_ids() -> Vec<String> {
        ["cap_blue", "hood_night", "spacecat", "bell_gold"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// **载荷下标 = 换装页那套下标**（这条是 P4 唯一"点错件"级别的事故）。
    ///
    /// 抄 [`unowned_accessory_never_appears_while_the_owned_one_does`] 那批 `hat_*` fixture
    /// 的写法：`visible` 不是手填的常量，而是**换装页自己那个纯函数**（[`DressView::select`]）
    /// 跑出来的 ⇒ 两边共用同一个下标空间这件事由构造说话。反查用 catalog 的 `name`，因为
    /// [`AlbumEntry`] 刻意不带 id（"catalog 不认的 id 也要能进这张表"那一条）。
    ///
    /// ⚠️ **P5 让这一条变成两问而不是一问**：本体走 `Body` 那套下标（`bodies`），配件走
    /// `Wear` 那套（`candidates`），而 fixture 里 `spacecat` **同时**在两张表里 ⇒ 同一个数字
    /// `1` 在这两块面上指的是两件不同的东西。所以这里两条反查分开写，一条不许替另一条说话
    /// （"两套下标互不冒充"那条独立门禁 = [`the_album_routes_pets_to_the_body_index_space`]）。
    #[test]
    fn the_row_payload_is_the_same_index_the_dress_page_cycles() {
        let g = album_fixture();
        let ids = album_ids();
        let owned = owned_of(&["bell_gold", "spacecat", "cap_blue", "hood_night"]);
        let visible = DressView::select(false, Some(&owned), &ids).visible;
        // 本体表刻意与配件表**错位**：`spacecat` 在配件表是下标 2，在本体表是下标 1，
        // 而 `dinosaur` 没抽到 ⇒ 它被同一道闸门挡在外面（于是 `pet_visible` 只有 `[1]`，
        // 拿它当 `[0..len]` 的那点侥幸也一并被测掉）。
        let pet_ids: Vec<String> = ["dinosaur", "spacecat"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pet_view = DressView::select(false, Some(&owned), &pet_ids);
        let entries = g
            .album_entries(&owned, &ids, &visible, &pet_ids, &pet_view.visible)
            .expect("逐件条目该读得出来");

        // 条目序 = 档序（N 三件按档内序 → R），而 `hat_straw` 不在 `owned` 里 ⇒ 根本不出现。
        // 那只猫这一轮**给下标了**，但那是 `Body` 那套下标里的 1（= `pet_ids` 里的 `spacecat`）。
        use AlbumTarget::{Accessory, Body, None as Inert};
        let want = [
            ("金铃", Accessory(3)),
            ("太空猫", Body(1)),
            ("蓝帽", Accessory(0)),
            ("夜兜", Accessory(1)),
        ];
        assert_eq!(entries.len(), want.len(), "条目数不等于 catalog ∩ owned");
        for (e, (name, t)) in entries.iter().zip(want) {
            assert_eq!(e.name, name, "条目顺序漂了");
            assert_eq!(e.target, t, "{name} 的载荷下标不对");
        }
        assert!(
            entries.iter().all(|e| e.name != "草帽"),
            "未拥有的件不该进逐件册（它出现在汇总那一行就够了）"
        );

        // **真正的那条**：每个载荷都能拿它去**它那一张表**里反查出这一件自己的 id。
        // 写成"等于目录里的枚举序号"的话，这行永远验不出任何东西。
        for e in &entries {
            let id = g
                .catalog
                .items
                .iter()
                .find(|it| it.name == e.name)
                .map(|it| it.id.clone())
                .unwrap_or_else(|| panic!("{} 不在 fixture 目录里", e.name));
            match e.target {
                Accessory(i) => {
                    assert_eq!(
                        ids[i], id,
                        "Wear({i}) 说的不是 {id} 在 candidates 里的位置 ⇒ 点这行会穿错件"
                    );
                    // 载荷必然在闸门放行集里（`pick_and_wear` 的那道 `visible.contains` 恒真，
                    // 而它**必须**仍然查一遍 —— 屏上挂着的行可能已经比这一轮的记忆旧一次）。
                    assert!(
                        visible.contains(&i),
                        "给了下标却没让它过闸 = 屏上给了一个必然被拒的按钮"
                    );
                }
                Body(i) => {
                    assert_eq!(
                        pet_ids[i], id,
                        "Body({i}) 说的不是 {id} 在 bodies 里的位置 ⇒ 点这行会换成另一只"
                    );
                    assert!(
                        pet_view.visible.contains(&i),
                        "给了本体下标却没让它过库存那道 = 屏上给了一个必然被拒的按钮"
                    );
                }
                Inert => panic!("{id} 这一行既没配件下标也没本体下标 ⇒ 上面 want 里给了它下标"),
            }
        }
        // 反面对照（P5 把它**翻过来了**）：配件闸门放行了下标 2（那只猫在配件表里占的那一格）
        // 而**没有任何配件行**认领它 ⇒ 同一个 id 在两张表里时，路由走本体那一套，
        // 绝不因为"它在 `candidates` 里也有位置"就产出一条 `Wear(2)`。
        let inert: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|i| {
                !entries
                    .iter()
                    .any(|e| matches!(e.target, Accessory(j) if j == *i))
            })
            .collect();
        assert_eq!(
            inert,
            vec![2],
            "宠物之外还有格子被放行了却没行认领 ⇒ 那件点不到（而 2 那一格**本该**空着：它是本体，\
             行在那一套下标里）"
        );
    }

    /// [`plan_pick_wear`] 的**全划分**：8 格真值表每一格都落进三档之一，三档各被至少一格打到，
    /// 而"先比后写"那条次序就住在这张表里。
    ///
    /// 抄 [`exemption_needs_the_flag_and_a_real_store`] 那种"四格真值表逐格点名"的写法。
    /// 两条最容易写反的格子单独钉：② `worn=false` 时下标相等**不是**"已经穿着它"（那是"没穿
    /// 任何东西而选中格恰好指着它"⇒ 必须真装）；④ 如果 [`App::pick_and_wear`] 先写 `picked`
    /// 再比，这一格（最常见的那一次"换一件别的"）就变成 `AlreadyWorn` ⇒ 静默不动 + 一句谎话。
    #[test]
    fn plan_pick_wear_partitions_the_three_outcomes() {
        let mut refused = 0;
        let mut already = 0;
        let mut wear = 0;
        for gate in [false, true] {
            for worn in [false, true] {
                for (picked, want) in [(3usize, 3usize), (3, 4)] {
                    match plan_pick_wear(gate, worn, picked, want) {
                        PickWear::Refused => refused += 1,
                        PickWear::AlreadyWorn => already += 1,
                        PickWear::SelectAndWear => wear += 1,
                    }
                }
            }
        }
        assert_eq!(refused + already + wear, 8, "八格真值表有格子没落进三档");
        assert_eq!(
            (refused, already, wear),
            (4, 1, 3),
            "三档的分布变了 ⇒ 有人动了判定次序或把某档合并了"
        );

        // ① 闸门没过 ⇒ 无论身上有什么、下标等不等，一律不动。这是"失败关闭"那一侧。
        for worn in [false, true] {
            for (picked, want) in [(3usize, 3usize), (3, 4)] {
                assert_eq!(
                    plan_pick_wear(false, worn, picked, want),
                    PickWear::Refused,
                    "库存没过还去装载 = 换装页那道闸在这里被绕过"
                );
            }
        }
        // ② 身上没东西 ⇒ 哪怕选中的就是它，也得真装一次（否则"第一次点收藏册"什么都不发生）。
        assert_eq!(
            plan_pick_wear(true, false, 3, 3),
            PickWear::SelectAndWear,
            "未装备态点同一格被读成'已经穿着它'"
        );
        // ③ 穿着的就是它 ⇒ 不解像素、不重投帧，但必须说话。
        assert_eq!(plan_pick_wear(true, true, 3, 3), PickWear::AlreadyWorn);
        // ④ 穿着别的那件 ⇒ 换。
        assert_eq!(plan_pick_wear(true, true, 3, 4), PickWear::SelectAndWear);
    }

    /// [`plan_swap_body`] 的**全划分**：16 格（四道输入）每一格都落进三档之一，而**换本体那九步
    /// 只有一格能打**，剩下的 15 格要么"已经是这只"、要么带出**那一道自己的名字**。
    ///
    /// 抄 [`plan_pick_wear_partitions_the_three_outcomes`] 的写法（那张 8 格 / 三档输入，这张
    /// 16 格 / 四档输入）。三条最要紧的格子单独钉：
    /// ① **任何一道没过都不许给 `Same`**：那句"身上已经是这只"听起来像成功，把它排在门禁之前
    ///    就是把"我为什么点不动它"回答成一个不需要回答的问题（配件那侧的 `AlreadyWorn` 同一条
    ///    理由、同一次实现选择，见 [`plan_pick_wear`] ④ 那一格）。
    /// ② `same_grid = false` 那一族**先于**库存那道被点名 ⇒ 拒绝说的是档、不是库存，因为
    ///    [`crate::atlas::Atlas::reload`] 自检读回的三个数仍是**旧**本体的（完整理由写在
    ///    [`plan_swap_body`] 那条注释上；扫盘那一步才是拍板② 的屏上形状，这里只是第二道保险）。
    /// ③ 三道拒绝的文案**两两不同**（14 格只该有三句不同的话）⇒ "没反应"这一格屏上长得一样，
    ///    只有 stderr 那一句分得开三种原因，这是"失败关闭必须点名"那条纪律的最低读数。
    #[test]
    fn plan_swap_body_partitions_the_four_outcomes() {
        let mut switched = 0;
        let mut same = 0;
        let mut refused = 0;
        let mut reasons: Vec<&'static str> = Vec::new();
        for in_range in [false, true] {
            for gate in [false, true] {
                for same_id in [false, true] {
                    for same_grid in [false, true] {
                        let got = plan_swap_body(in_range, gate, same_id, same_grid);
                        let cell = format!(
                            "in_range={in_range} gate={gate} same_id={same_id} same_grid={same_grid}"
                        );
                        // 期望值按**判定的顺序**算，不按"哪几道过了"算 ⇒ 次序被改就是这里红。
                        if !in_range || !same_grid || !gate {
                            let SwapBody::Refused(why) = got else {
                                panic!("{cell} ⇒ 门禁没过却给出 {got:?}（那就是放行）");
                            };
                            let needle = if !in_range {
                                "本体库里"
                            } else if !same_grid {
                                "格子档"
                            } else {
                                "库存"
                            };
                            assert!(
                                why.contains(needle),
                                "{cell} ⇒ 该点「{needle}」那一道的名，实际给的是「{why}」"
                            );
                            refused += 1;
                            reasons.push(why);
                        } else if same_id {
                            assert_eq!(got, SwapBody::Same, "{cell} ⇒ 三道全过而点的就是当前这只");
                            same += 1;
                        } else {
                            assert_eq!(got, SwapBody::Switch, "{cell} ⇒ 四道全过而点的不是这只");
                            switched += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(
            switched + same + refused,
            16,
            "四道输入的真值表有格子没落进三档"
        );
        // `Switch` 与 `Same` **各只有一格**（三道门禁必须全过，只剩 `same_id` 分这两档）。
        assert_eq!(
            (switched, same, refused),
            (1, 1, 14),
            "三档分布变了 ⇒ 有人动了判定次序，或者把某一道门禁合并进了另一道"
        );
        assert_eq!(
            reasons.iter().filter(|w| w.contains("本体库里")).count(),
            8,
            "「这一格没有」那一族该占 16 格里的一半（另外三道全不参与）"
        );
        // ③ 三道文案两两不同（14 格拒绝只该有三句不同的话）。
        let distinct: std::collections::BTreeSet<&str> = reasons.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            3,
            "14 格拒绝只给出 {} 句不同的话 ⇒ 有三道被合并成了同一句",
            distinct.len()
        );
    }

    /// **一个数字，两张表**（P5 唯一"点错本体"级别的事故）。
    ///
    /// 收藏册那一屏上，配件行走 [`panel::PanelItem::Wear`]（载荷 = [`App::candidates`] 的下标），
    /// 宠物行走 [`panel::PanelItem::Body`]（载荷 = [`App::bodies`] 的下标）。这两套下标**住在同一
    /// 个 `usize` 空间里**，所以同屏出现 `Wear(1)` 与 `Body(1)` 是完全正常的，而它们必须指着两件
    /// 不同的东西 —— 反过来说：谁把 `album_entries` 里那两次 `position()` 合并成一次（"反正都是
    /// 查 id"），屏上就会有一行点下去换成一只猫、另一行点下去戴上夜兜，而**这两次都自洽**，
    /// headless 里除了这一条没有别的读数。
    ///
    /// 抄 [`the_row_payload_is_the_same_index_the_dress_page_cycles`] 的 fixture 与"按 catalog 的
    /// `name` 反查"的手法（[`AlbumEntry`] 刻意不带 id），差别是这一条**同时**拿两套下标去各自的
    /// 表里反查，并且**明确禁止**拿它去对面那张表里反查（那两条 `NOT` 断言才是这条的名字）。
    #[test]
    fn the_album_routes_pets_to_the_body_index_space() {
        let g = album_fixture();
        let ids = album_ids();
        let owned = owned_of(&["bell_gold", "spacecat", "cap_blue", "hood_night"]);
        let visible = DressView::select(false, Some(&owned), &ids).visible;
        // `dinosaur` 没抽到 ⇒ 被同一道闸挡在 `pet_visible` 外面，于是 `spacecat` 在本体
        // 那一张表里落在**下标 1** —— 而 1 在配件那张表里是 `hood_night`（夜兜）。
        let pet_ids: Vec<String> = ["dinosaur", "spacecat"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pet_view = DressView::select(false, Some(&owned), &pet_ids);
        assert_eq!(
            pet_view.visible,
            vec![1],
            "fixture 的前提变了：未拥有的本体被放行了 ⇒ 下面那两条对照就没意义了"
        );

        let entries = g
            .album_entries(&owned, &ids, &visible, &pet_ids, &pet_view.visible)
            .expect("逐件条目该读得出来");
        let rows = album_rows(&entries, 1.0);
        assert_eq!(rows.len(), entries.len(), "一行一件这条断了");

        // **同屏两格同一个数字**：`Wear(1)` 与 `Body(1)` 都在，且各自的文本说的是各自那件。
        let wear = rows
            .iter()
            .find(|r| r.item == panel::PanelItem::Wear(1))
            .expect("夜兜该是 Wear(1)");
        let body = rows
            .iter()
            .find(|r| r.item == panel::PanelItem::Body(1))
            .expect("太空猫该是 Body(1)");
        assert!(
            wear.text.contains("夜兜"),
            "Wear(1) 那行没写夜兜：{:?}",
            wear.text
        );
        assert!(
            body.text.contains("太空猫"),
            "Body(1) 那行没写太空猫：{:?}",
            body.text
        );
        // 这两条 `NOT` 才是这条门禁的名字：**拿错表**的两种写法各打一条。
        assert!(
            !wear.text.contains("太空猫"),
            "Wear(1) 那行读到了本体那张表的东西 ⇒ 两套下标混用了（配件那臂去查了 pet_ids）"
        );
        assert!(
            !body.text.contains("夜兜"),
            "Body(1) 那行读到了配件那张表的东西 ⇒ 两套下标混用了（本体那臂去查了 candidates）"
        );

        // 可点性这一格**必须一样**：宠物不再是"永远只读"（那是 P4 的形状），而画师读的正是
        // 可点性 ⇒ 两行都有强调条，用户看不出"这是换一件衣服"和"这是换一只本体"的区别，
        // 那件事由 `pet` 那个 ASCII 标与本体自己那几个像素承担（不上屏新字形）。
        for r in [&wear.item, &body.item] {
            assert!(
                r.actionable(),
                "{r:?} 给了下标却不 actionable ⇒ 假按钮的反面：真按钮点不到"
            );
        }
        // 反向对照：那只没抽到的本体**不产行**（它压根不在 fixture 目录里），而目录里那只
        // 若被 `pet_visible` 挡在外面就退化成只读 —— 后者由
        // `an_unowned_or_undiskable_entry_stays_inert` 正面钉住，这里只留一句名。
        assert!(
            rows.iter().all(|r| r.item != panel::PanelItem::Body(0)),
            "`dinosaur` 没抽到却有一行指着本体第 0 格 ⇒ 库存那道在本体侧空转"
        );
    }

    /// [`plan_body_override`] 的五格：**只有两格**能让库里那一行赢，而那一只必须**在启动扫盘
    /// 交出来的表里**且**不是 `--manifest` 那一只**。
    ///
    /// 抄 [`plan_restore`] 那两条"手工行必须出局并报一句"的写法（同一族判据：库里记着一个
    /// 这轮装不上的东西 ⇒ 不办，但必须点名）。要紧的是三件事各被钉住一次：
    /// ① `Keep(None)`（老库、没有那一行）**不带理由** ⇒ 启动一个字都不打，那条"行为逐字节
    ///   不变"的判据（拍板③ 的后半）就是这一格；
    /// ② 有行而不覆盖 ⇒ 理由必须**非空**（静默退回 `--manifest` 就是假门禁：用户上次换的本体
    ///   自己没了而没有任何一句话）；
    /// ③ 库里那一行**就是**起点那只 ⇒ 也是 `Keep(None)` 且无理由 —— 它不是"覆盖"（那一只本来
    ///   就在身上），给它打一行点名就是把"什么都没变"说成"我替你换了一只"，与 ① 同族。
    #[test]
    fn plan_body_override_wins_only_when_the_row_names_a_discovered_pet() {
        let pet_ids: Vec<String> = ["spacecat", "tiger", "dinosaur"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let start = "spacecat";

        // ① 没有那一行 ⇒ 不覆盖，而且**没有理由**（那是常态，不是事故）。
        match plan_body_override(None, &pet_ids, start) {
            BodyChoice::Keep(None) => {}
            other => panic!("库里没有 body 行却给出 {other:?} ⇒ 老库的行为变了"),
        }

        // ② 那一行指的就是表里的第 1 格 ⇒ 覆盖，而且**赢**（`--manifest` 那一只让位）。
        //    下标必须能反查回同一个 id ⇒ 与 `the_album_routes_pets_to_the_body_index_space`
        //    同一条"载荷下标 = 那张表自己的位置"的纪律。
        match plan_body_override(Some("tiger"), &pet_ids, start) {
            BodyChoice::Override(i) => {
                assert_eq!(
                    pet_ids[i], "tiger",
                    "Override({i}) 说的不是 tiger 在发现表里的位置 ⇒ 起点本体换成另一只"
                );
            }
            other => panic!("库里那一行点的是发现表里有的本体却给出 {other:?} ⇒ 拍板③ 没生效"),
        }

        // ③ 那一行指的就是**起点那一只**（用户上次没换，或换了又换回来）⇒ 不覆盖，也**没有
        //    理由**。这一格与 ① 是同一族：这一轮没有发生"覆盖" ⇒ 一行点都不该打；给它点一行
        //    就是把"什么都没变"说成"我替你换了一只"（`--manifest` 那一只本来就是它）。
        //    ⚠️ 这一格的存在意味着 `id == start_id` 的判据**必须**在 `position()` 之前问：
        //    它指的是表里有的东西，走 ④ 那一支会得到一句假话。
        match plan_body_override(Some("spacecat"), &pet_ids, start) {
            BodyChoice::Keep(None) => {}
            other => panic!("库里那一行就是起点那只却给出 {other:?} ⇒ 启动凭空多一行点名"),
        }

        // ④ 有行而表里没有这一只（目录不存在 / 产物坏了 / **格子档不同被扫盘挡掉**）⇒ 不覆盖，
        //    且必须点出为什么。这三族合并成一句是选过的：它们在启动这一刻**没有可区分的动作**
        //    （都不覆盖、都照旧用 `--manifest`），而真正区分它们的那一行在 [`check_body`] /
        //    [`discover_pets`] 的 skip-and-log 里，那里才是点名的地方。
        for ghost in ["monkey", ""] {
            match plan_body_override(Some(ghost), &pet_ids, start) {
                BodyChoice::Keep(Some(why)) => assert!(
                    why.contains("不在启动扫盘"),
                    "拒绝覆盖却没说清拦在哪一道：{why}"
                ),
                other => panic!("库里那一行指 {ghost:?} 不在表里却给出 {other:?}"),
            }
        }
        // ⑤ 表为空（没有本体库 / `--test-rect`）⇒ 任何一行都不许覆盖 —— 这一格单独钉，因为
        //    `position()` 在空表上恒 `None`，写成 `bodies.first()` 之类的"顺手"就恒真了。
        assert!(matches!(
            plan_body_override(Some("tiger"), &[], start),
            BodyChoice::Keep(Some(_))
        ));
        // ⑤ 的另一面：表为空而那一行**恰好就是起点那只**（`--manifest` 与库里记的是同一只，
        //    而产物没烘出来 ⇒ 起点照旧是它）⇒ 无理由。少了这一格，`id == start_id` 那一支放
        //    在 `position()` 之后就会在"没有本体库"的机器上打一行假点名。
        assert!(matches!(
            plan_body_override(Some("spacecat"), &[], start),
            BodyChoice::Keep(None)
        ));
    }

    /// **`body` 那一行不是配件的账**（P5 给启动恢复补的那道剥壳，J3/M6 的读数）。
    ///
    /// 这条不测"剥得对"，测的是**不剥会说出哪句假话** —— 那句在 headless 里除了这里没有别的读数，
    /// 而它是真机上会被当成"功能没生效"的误导：`磁盘上找不到这一件（已删除，或五道门禁没过）`
    /// 说的是一行**根本不是配件**的账，而且**每一轮启动都打**。
    ///
    /// ⚠️ 明写这条**不**测的那一件：不剥**不会**丢帽子。[`plan_restore`] 取的是"第一条过了四道
    /// 的行"，而本体行在第 1 道就被 `continue` 掉，一次都不占 `wear`。下面那两条 `wear` 断言因此
    /// 是**同一件事的两面**（剥与不剥都穿上那一件）—— 它守的是"剥壳别把配件行也剥掉"，不是"少剥
    /// 就丢装备"。这条剥壳的全部意义是**不说假话**。
    #[test]
    fn a_body_row_is_not_an_accessory_note() {
        let rows = vec![
            ("body".to_string(), "tiger".to_string()),
            ("head".to_string(), "cap_blue".to_string()),
        ];
        let ids: Vec<String> = ["cap_blue"].iter().map(|s| s.to_string()).collect();
        let slots: Vec<String> = ["head"].iter().map(|s| s.to_string()).collect();
        let visible = vec![0usize];

        // 剥壳本身：`(本体 id, 只剩配件的行)`，两条都不许丢。
        let (body, accessories) = split_body_row(&rows);
        assert_eq!(body.as_deref(), Some("tiger"), "body 那一行没被认出来");
        assert_eq!(
            accessories,
            vec![("head".to_string(), "cap_blue".to_string())],
            "剥壳把配件那一行也剥掉了 ⇒ 帽子从此不会恢复"
        );

        // ✅ 剥完之后：帽子是帽子、本体是本体，两句噪音都不该有。
        let plan = plan_restore(&accessories, &ids, &slots, &visible);
        assert_eq!(plan.wear, Some(0), "配件行被 body 行的存在影响到了");
        assert!(
            plan.notes.is_empty(),
            "剥干净了却还有说明：{:?}",
            plan.notes
        );

        // ❌ 不剥（这是 M6 那一刀）：那句假话必须**当场**打得出来，否则这条门禁就是在念注释。
        let bad = plan_restore(&rows, &ids, &slots, &visible);
        assert_eq!(
            bad.wear,
            Some(0),
            "不剥反而丢了装备 ⇒ `plan_restore` 第 4 条那条取首条的规则变了，上面那段\n\
             「本体行不会把帽子挤掉」的论证作废，这条得重新想（这是本轮唯一能读到它变没变的地方）"
        );
        assert!(
            bad.notes
                .iter()
                .any(|n| n.contains("磁盘上找不到这一件") && n.contains("tiger")),
            "不剥却没有那句假话：{:?}",
            bad.notes
        );

        // 只有 body 行（上次只换了本体、没戴帽子）⇒ 配件侧**一行说明都不许有**。
        let only_body = vec![("body".to_string(), "tiger".to_string())];
        let (b, acc) = split_body_row(&only_body);
        assert_eq!(b.as_deref(), Some("tiger"));
        let plan = plan_restore(&acc, &ids, &slots, &visible);
        assert_eq!(plan.wear, None, "只剩本体行却还要穿一件");
        assert!(
            plan.notes.is_empty(),
            "只剩本体行却给出了配件说明：{:?}",
            plan.notes
        );
    }

    /// 三种"点不了"的形状都**留在屏上**，只是没有强调条；唯一被点的那一件才有载荷。
    ///
    /// 抄 [`an_info_row_is_hit_but_never_activates_while_the_row_below_does`]（`panel.rs`）
    /// 那条"逐格列举可点性"的写法，但这一条量的是**生产者侧**：`Wear`/`Body`/`Info` 这一格是
    /// [`Gacha::album_entries`] 定的，而 `panel::press` 只是照它办事。合在一起才是
    /// "抽到 ≠ 能穿"这件事第一次在屏上读得出来（§9.2 第 20 条那一族的最后一块）。
    #[test]
    fn an_unowned_or_undiskable_entry_stays_inert() {
        let g = album_fixture();
        // 磁盘只有两件有产物（裸开发树 30 件只有 1 件 ⇒ **这才是常态**），而库存里躺着四件。
        let ids: Vec<String> = ["cap_blue", "hood_night"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let owned = owned_of(&["cap_blue", "hood_night", "hat_straw", "spacecat"]);
        let visible = DressView::select(false, Some(&owned), &ids).visible;
        // ⚠️ 本体表**交空** = 这一轮磁盘上没有一只本体（裸开发树的真实读数：`$OPET_PETS` 没指、
        // `$XDG_DATA_HOME/opet/pets` 不存在 ⇒ [`discover_pets`] 返空）。所以那只猫在这里是
        // "第四种点不了"的形状：抽到了、名字照说、但它不在 `bodies` 里 ⇒ `Info`。
        // P5 之后宠物行**并非永远只读**，那一条由 [`the_row_payload_is_the_same_index_the_dress_page_cycles`]
        // 与 [`the_album_routes_pets_to_the_body_index_space`] 正面钉住。
        let pet_ids: Vec<String> = Vec::new();
        let pet_visible: Vec<usize> = Vec::new();
        let entries = g
            .album_entries(&owned, &ids, &visible, &pet_ids, &pet_visible)
            .expect("逐件条目该读得出来");
        assert_eq!(
            entries.len(),
            4,
            "列表读的是 catalog ∩ owned ⇒ 四件都该在，哪怕磁盘上只有两件"
        );

        let rows = album_rows(&entries, 1.0);
        assert_eq!(rows.len(), entries.len(), "一行一件这条断了");
        let row_of = |name: &str| {
            rows.iter()
                .find(|r| r.text.contains(name))
                .unwrap_or_else(|| panic!("屏上没有{name}那一行"))
        };
        // ✅ 有产物 + 过闸 ⇒ 可点，且载荷就是它在 `ids` 里的位置。
        assert_eq!(row_of("蓝帽").item, panel::PanelItem::Wear(0));
        assert_eq!(row_of("夜兜").item, panel::PanelItem::Wear(1));
        // ⚠️ 抽到了而磁盘没产物 ⇒ **只读行**，而名字照说（"消失了"和"不能穿"是两件事）。
        let straw = row_of("草帽");
        assert_eq!(straw.item, panel::PanelItem::Info);
        assert_eq!(
            straw.text,
            format!(
                "{} {} {}",
                Rarity::SSR.as_str(),
                Slot::Head.as_str(),
                "草帽"
            ),
            "无产物的那一件把档名/挂点/名字丢了 ⇒ 这行读不出自己是什么"
        );
        // ⚠️ 宠物（本体表为空）⇒ 只读 + 行尾 ASCII `pet` 标（FLOW §6③ 那句"明写而不是灰掉不显示"）。
        let pet = row_of("太空猫");
        assert_eq!(pet.item, panel::PanelItem::Info);
        assert!(
            pet.text.ends_with(ItemKind::Pet.as_str()),
            "宠物那一行没打 `pet` 标：{:?}",
            pet.text
        );
        assert_eq!(
            pet.text,
            format!("N {} {}", "太空猫", ItemKind::Pet.as_str()),
            "挂点为 `None` 的那一格该整个消失，而不是留一个空格"
        );

        // **可点格子数必须正好等于"磁盘有产物又过闸"那两件**：多一个就是假按钮（D14 判死过的
        // 形状），少一个就是那一件永远点不到。
        assert_eq!(
            rows.iter().filter(|r| r.item.actionable()).count(),
            2,
            "可点行与 `ids ∩ visible` 不对账"
        );
    }

    /// 逐件那一行的**宽度纪律**：能放的词一个不丢，放不下的按 `dup` → `slot` 的顺序丢，
    /// 而**永远不许**留下一个 `…`（除了"名字本身就超预算"那一格，那一格明确交给排版）。
    ///
    /// 抄 [`every_panel_row_is_sayable_and_never_truncated`] 那套"预算与排版逐字同构"的量法
    /// （可点行问 [`menu::label_rect`]、只读行问 [`menu::row_text`]），但条目是**合成的**：
    /// 真 catalog 的名字最短 2 个字，量不到这一族的下界。三档缩放 = `card.rs` 那三条
    /// （`[1.0, 1.5, 2.0]`），四 locale 走的是**分页之后**那一屏（`More` 那一行才是本地化的）。
    #[test]
    fn album_rows_say_every_entry_without_truncation() {
        // ① 两档预算必须是**两个不同的数**：把它们"顺手统一"成 216，下面那条合成阶梯会
        //    少一档而真机屏上多一个 `…`（这条断言就是防止那句"反正差 7 px"）。
        for scale in [1.0f32, 1.5, 2.0] {
            assert!(
                menu::budget_for(panel::ROW_LABEL_W, scale)
                    < menu::budget_for(panel::ROW_TEXT_W, scale),
                "{scale}× 下可点行的预算不比只读行窄 ⇒ 强调条没人让位"
            );
        }

        // ② 合成阶梯：名字长度四档，正好各打中"丢 dup / 丢 slot / 无词可丢"里的一档。
        //    期望值由本条自己按 `render` 的同一条顺序算（参照物，不是重新实现控制流）。
        let rar = Rarity::N;
        let neck = Slot::Neck;
        let slot = Some(neck);
        for scale in [1.0f32, 1.5, 2.0] {
            let mut branch = [0usize; 4];
            for name in [
                "蓝帽".to_string(),
                "蓝帽".repeat(6),
                "蓝帽".repeat(12),
                "蓝帽".repeat(40),
            ] {
                let e = AlbumEntry {
                    target: AlbumTarget::Accessory(0),
                    name: name.clone(),
                    rarity: rar,
                    slot,
                    kind_pet: false,
                    dup: 3,
                };
                let row = &album_rows(&[e], scale)[0];
                let budget = menu::budget_for(panel::ROW_LABEL_W, scale);
                let fits = |s: &str| menu::fit_to(s, budget, scale) == s;
                let say = |with_slot: bool, with_dup: bool| {
                    let mut s = rar.as_str().to_string();
                    if with_slot {
                        s.push(' ');
                        s.push_str(neck.as_str());
                    }
                    s.push(' ');
                    s.push_str(&name);
                    if with_dup {
                        s.push_str(" x3");
                    }
                    s
                };
                let (all, no_dup, bare) = (say(true, true), say(true, false), say(false, false));
                if fits(&all) {
                    assert_eq!(row.text, all, "{scale}× 全量放得下却丢了词");
                    branch[0] += 1;
                } else if fits(&no_dup) {
                    assert_eq!(row.text, no_dup, "{scale}× 该丢 dup 而不是丢挂点");
                    branch[1] += 1;
                } else if fits(&bare) {
                    assert_eq!(row.text, bare, "{scale}× dup 与挂点都该丢");
                    branch[2] += 1;
                } else {
                    assert_eq!(row.text, bare, "{scale}× 无词可丢时只剩档名 + 名字");
                    branch[3] += 1;
                }
                assert_sayable(&row.text, Lang::Zh, "逐件行");
            }
            assert!(
                branch[0] > 0 && branch[2] + branch[3] > 0,
                "{scale}× 的阶梯只走了一档 ⇒ 这几条名字长度没覆盖到下界：{branch:?}"
            );
        }

        // ③ **分页之后**整屏量一遍（`More` 那一行是本地化的 ⇒ 四 locale 各有不同的宽度）：
        //    每一行都问排版自己的账，且没有一行被截。条目取"最坏情况"= 全部可点（窄预算），
        //    而 `kind_pet` 那一档可点**只能是 `Body`** —— 造一个"宠物挂着配件下标"的形状
        //    就是在测一条生产者发不出来的行（P5 之前 `wear: Some(i)` 兼 `kind_pet` 正是那种形状）。
        let entries: Vec<AlbumEntry> = (0..14)
            .map(|i| AlbumEntry {
                target: if i % 7 == 3 {
                    AlbumTarget::Body(i)
                } else {
                    AlbumTarget::Accessory(i)
                },
                name: "蓝帽".repeat(1 + i % 4),
                rarity: Rarity::ALL[i % 4],
                // ⚠️ 取模的数由 `Slot::ALL::len()` 给，不写死：写死的那个余数窗口会让新加的挂点
                // 永远进不了这张"最坏情况"表，而这一屏的量就是靠它兜住的（v2 加 `feet` 时
                // 原先这里写的是 5 ⇒ 第六值一次都没被排过版）。
                slot: Slot::ALL.get(i % Slot::ALL.len()).copied(),
                kind_pet: i % 7 == 3,
                dup: i as u64,
            })
            .collect();
        for scale in [1.0f32, 1.5, 2.0] {
            let content = album_rows(&entries, scale);
            for lang in Lang::ALL {
                let pages = panel::album_page(lang, &content, 0).pages;
                for p in 0..pages {
                    let screen = panel::album_page(lang, &content, p);
                    assert!(
                        screen.rows.len() <= panel::MAX_ROWS,
                        "{lang:?} {scale}× 第 {p} 页有 {} 行",
                        screen.rows.len()
                    );
                    for (r, row) in panel::layout(&screen.rows).iter().zip(&screen.rows) {
                        let label = if r.item.actionable() {
                            menu::label_rect(r.as_rect())
                        } else {
                            menu::row_text(r.as_rect())
                        };
                        assert_sayable(&row.text, lang, "逐件行/页码行");
                        assert_eq!(
                            menu::fit_to(&row.text, menu::budget_for(label.w, scale), scale),
                            row.text,
                            "{lang:?} {scale}× 第 {p} 页 {:?}「{}」被排版截 ⇒ 生产者算错了预算",
                            r.item,
                            row.text
                        );
                    }
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

    // ─────────────── 配件叠加（D16 路线 B）：装备门禁 + 逐格出图 ───────────────

    /// 测试资产用 **16×16** 的格子：一张 sheet 只有 128×64 px，`encode_lossless_rgba` 现烘一份
    /// 是毫秒级。寻址、并集、`covers` 这些判据跟格子尺寸无关，用 192 只会让每轮测试多解 1.5 MB。
    const T_CELL: u32 = 16;
    const T_COLS: u32 = 8;
    const T_ROWS: u32 = 4;
    /// 本体：`idle` 占行 0–1、`run` 占行 2–3 ⇒ 配件**没烘过** `run` 是这份 fixture 的常态。
    const BODY_JSON: &str = r#"{"format":2,"slug":"tiny","sheet":"sheet.webp","grid":{"cols":8,"rows":4,"w":16,"h":16},"states":{"idle":{"rows":[0,1],"fps":8,"loop":true},"run":{"rows":[2,3],"fps":16,"loop":true}},"anchor":{"feet":[8,15]}}"#;
    /// 帽子只覆盖 `idle`（两行）⇒ `drawn_cells()` = 行 0/1 × 8 列 = **16 格**。
    const HAT_JSON: &str = r#"{"format":2,"slug":"cap","display_name":"蓝帽","slot":"head","fits":["tiny"],"sheet":"sheet.webp","grid":{"cols":8,"rows":4,"w":16,"h":16},"states":{"idle":{"rows":[0,1],"fps":8,"loop":true}}}"#;
    /// 本体每格一条满高 span ⇒ 一格一个矩形；帽子两格不相邻的短 span ⇒ 一格两个矩形。
    /// "两个矩形"是刻意的：拼接等于并集这条论证，只有两边矩形数不同时才测得出来。
    const BODY_SPANS: &[[u32; 3]] = &[[0, 0, 15]];
    const HAT_SPANS: &[[u32; 3]] = &[[0, 0, 0], [4, 4, 4]];

    /// `polygon.json` 的一条帧记录。测试里这两个 helper 到处传这张表，具名比裸元组好读。
    #[derive(Clone, Copy)]
    struct Frame {
        cell: (u32, u32),
        spans: &'static [[u32; 3]],
    }

    fn cells_with(rows: u32, spans: &'static [[u32; 3]], skip: &[(u32, u32)]) -> Vec<Frame> {
        (0..rows)
            .flat_map(move |r| {
                (0..T_COLS).map(move |c| Frame {
                    cell: (r, c),
                    spans,
                })
            })
            .filter(move |f| !skip.contains(&f.cell))
            .collect()
    }

    fn polygon_json(cell: u32, frames: &[Frame]) -> String {
        let parts: Vec<String> = frames
            .iter()
            .map(|f| {
                let (r, c) = f.cell;
                format!(
                    r#"{{"row":{r},"col":{c},"spans":{}}}"#,
                    serde_json::to_string(f.spans).expect("span 表该编得出去")
                )
            })
            .collect();
        format!(
            r#"{{"cell":{{"w":{cell},"h":{cell}}},"frames":[{}]}}"#,
            parts.join(",")
        )
    }

    /// 每格填一色（`at(row, col)`），行数由参数给 ⇒ 能造"清单说 4 行而像素只有 1 行"那种坏图集。
    fn sheet_bytes(rows: u32, at: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let (w, h) = (T_COLS * T_CELL, rows * T_CELL);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for r in 0..rows {
            for c in 0..T_COLS {
                let color = at(r, c);
                for y in 0..T_CELL {
                    let lo = ((r * T_CELL + y) * w * 4 + c * T_CELL * 4) as usize;
                    let take = (T_CELL * 4) as usize;
                    for p in px[lo..lo + take].chunks_exact_mut(4) {
                        p.copy_from_slice(&color);
                    }
                }
            }
        }
        crate::atlas::encode_lossless_rgba(&px, w, h).expect("lossless webp 该编得出来")
    }

    fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建目录");
        }
        std::fs::write(&path, bytes).expect("写测试资产");
        path
    }

    /// 本体区域从盘上读（`Regions::load` 只吃路径 ⇒ 没有"内存里造一份"的旁路，这正好）。
    fn body_regions(dir: &std::path::Path, skip: &[(u32, u32)]) -> Regions {
        let json = polygon_json(T_CELL, &cells_with(T_ROWS, BODY_SPANS, skip));
        let path = write(dir, "polygon.json", json.as_bytes());
        Regions::load(
            path.to_str().expect("UTF-8"),
            T_CELL,
            T_CELL,
            SURFACE_MARGIN,
        )
        .expect("本体轮廓该读得进来")
    }

    /// 把帽子那三样（清单 / 像素 / 轮廓）按测试需要落进 `dir/hat/`（= 库里的一件）。
    /// 缺哪一样就不写哪一样 ⇒ "被拒绝时一个字节都没读"这种判据才有得断言。
    ///
    /// **不再返回清单路径**：`--accessory` 删掉以后，生产代码里没有任何地方拿着"一份清单的路径"，
    /// 只有"一个库目录"。测试照调用方能看到的东西写，否则它会一直测一个已经不存在的外形。
    fn hat_in(dir: &std::path::Path, hat_json: &str, polygon: Option<&str>, rows: u32) {
        write(dir, "hat/accessory.json", hat_json.as_bytes());
        if let Some(p) = polygon {
            write(dir, "hat/polygon.json", p.as_bytes());
        }
        if rows > 0 {
            let bytes = sheet_bytes(rows, |r, c| {
                if (r, c) == (0, 0) {
                    [0, 255, 0, 128] // 半透绿：source-over 之后三个源像素都区分得出来
                } else {
                    [255, 0, 0, 255]
                }
            });
            write(dir, "hat/sheet.webp", &bytes);
        }
    }

    /// 一套全过的库：本体清单 + 本体 32 格轮廓 + 库里一件帽子（16 格齐全）。
    /// 返回的是**库目录**，因为那是 `discover_accessories` 收的东西。
    fn kit(
        dir: &std::path::Path,
        body_skip: &[(u32, u32)],
        hat_json: &str,
        hat_skip: &[(u32, u32)],
    ) -> (Manifest, Regions) {
        let body = Manifest::parse(BODY_JSON).expect("本体清单该合法");
        let regions = body_regions(dir, body_skip);
        let polygon = polygon_json(T_CELL, &cells_with(2, HAT_SPANS, hat_skip));
        hat_in(dir, hat_json, Some(&polygon), T_ROWS);
        (body, regions)
    }

    /// 库里只有这一件、且全过 ⇒ 拿到那唯一的候选。八条配件测试里有六条要它，形状 =
    /// "过了门禁的那件"而不是"过了门禁的那些"，所以这里直接断言只有一件。
    fn sole(dir: &std::path::Path, body: &Manifest, regions: &Regions) -> Candidate {
        let found = discover_accessories(dir, body, regions);
        assert_eq!(found.len(), 1, "这套资产该恰好产出一件候选，实际 {found:?}");
        found.into_iter().next().expect("上面查过 len == 1")
    }

    /// 空库 ⇒ 零候选，而**本体轮廓逐位不变**。这条是"没穿配件时出图路径与从来没有配件时
    /// 逐字节相同"那句承诺的入口（三条真机人工门禁吃的就是那份穿透矩形）。
    ///
    /// 三个方向都要断：① 库目录不存在 ⇒ 空表（不是错误 —— 硬约束 3，起不来比没帽子严重）；
    /// ② 库存在但空 ⇒ 空表；③ 库里有目录而**没有** `accessory.json` ⇒ 也不算一件。
    /// 第 ③ 档最容易被写错：拿 `read_dir` 的每一项直接当候选，那么任何一个别的目录（比如
    /// 用户手放的 `README/`）都会变成换装页里一行点了必然失败的配件。
    #[test]
    fn an_empty_library_yields_no_candidates_and_leaves_the_regions_untouched() {
        let dir = tempfile::tempdir().expect("临时目录");
        let body = Manifest::parse(BODY_JSON).expect("本体清单该合法");
        let given = body_regions(dir.path(), &[]);
        let want: Vec<Vec<CellRect>> = (0..T_ROWS)
            .flat_map(|r| (0..T_COLS).map(move |c| (r, c)))
            .map(|(r, c)| given.rects(r, c).expect("每格都该有").to_vec())
            .collect();

        let missing = dir.path().join("nope");
        assert!(
            discover_accessories(&missing, &body, &given).is_empty(),
            "库不存在 ⇒ 应当是「没有可穿」，不是错误、更不是 panic"
        );
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).expect("建空库");
        assert!(
            discover_accessories(&empty, &body, &given).is_empty(),
            "空目录 ⇒ 零候选"
        );
        // 一个没有清单的子目录：它**不是**一件配件，也不该进候选表。
        std::fs::create_dir(dir.path().join("notes")).expect("建无关目录");
        assert!(
            discover_accessories(dir.path(), &body, &given).is_empty(),
            "没有 accessory.json 的目录不该被当成一件配件"
        );

        let again: Vec<Vec<CellRect>> = (0..T_ROWS)
            .flat_map(|r| (0..T_COLS).map(move |c| (r, c)))
            .map(|(r, c)| given.rects(r, c).expect("每格都该有").to_vec())
            .collect();
        assert_eq!(again, want, "扫库这条路动了本体的穿透区域 ⇒ 那条承诺是假的");
        assert_eq!(again.iter().map(Vec::len).sum::<usize>(), 32);
    }

    /// **启动这条路根本不打开 `sheet.webp`** —— 库里把像素文件删掉，候选照样产出。
    ///
    /// 这条断的不是"健壮性"而是**内存档 M1 的字面含义**：`VmHWM` 是高水位，"每件解码一次再丢掉"
    /// 与"全部常驻"给出同一个峰值（实测 59,164 kB > 用户签的未装备配件 57,344 kB）。所以
    /// "扫库时解不解码"不是一个实现细节，而是一条能被这一行抓住的判据：哪天有人把
    /// [`Atlas::load`] 挪回 [`check_accessory`]，这份没有像素的库会立刻让它红。
    #[test]
    fn discovery_never_touches_the_sheet_so_a_pixel_less_library_still_lists_it() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (body, regions) = kit(dir.path(), &[], HAT_JSON, &[]);
        std::fs::remove_file(dir.path().join("hat/sheet.webp")).expect("删掉像素");
        let cand = sole(dir.path(), &body, &regions);
        assert_eq!(cand.slug, "cap");
        assert_eq!(
            cand.merged.frame_count(),
            32,
            "候选只带轮廓，轮廓该已经并好"
        );

        // 而"穿上"才是唯一可能读不到像素的地方 ⇒ 同一条候选到 `decode` 手里必须报**那个路径**。
        let Err(err) = decode(&cand) else {
            panic!("没有 sheet.webp 而 decode 还成功 ⇒ 它读的不是候选那份目录");
        };
        assert!(
            err.contains("hat/sheet.webp"),
            "错误必须点名是哪个文件读不开，否则真机上只能靠猜: {err}"
        );
    }

    /// **门禁顺序 + 一坏不害众**：库里放两件，一件格子漂移、一件全过 ⇒ 漂移的那件出局而
    /// 另一件照常可选，且报的错点名的是那条几何后果。
    ///
    /// 这里干脆**不写**漂移那件的 `sheet.webp` ⇒ 一旦哪天有人把 `validate_against` 挪到解码之后，
    /// 报的就会是 Io 错而不是这句"偏移不再是 0"，测试当场红。
    #[test]
    fn a_drifted_hat_is_skipped_in_favor_of_the_one_that_passes() {
        let dir = tempfile::tempdir().expect("临时目录");
        let body = Manifest::parse(BODY_JSON).expect("本体清单该合法");
        let regions = body_regions(dir.path(), &[]);
        let drifted = HAT_JSON.replace(r#""w":16,"h":16"#, r#""w":32,"h":32"#);
        assert_ne!(drifted, HAT_JSON, "替换没生效 ⇒ 这条测试什么也没测");
        hat_in(dir.path(), &drifted, None, 0);

        // 同一道门禁单独问出来时，错误里带的必须是"偏移不再是 0"（`discover_accessories` 只把它
        // 打进 stderr，而 stderr 在测试里断不到 —— 所以断言放在这一层，扫描那层只断"少一件"）。
        let Err(err) = check_accessory(&dir.path().join("hat"), &body, &regions) else {
            panic!("格子 32 对 16 的帽子该被拒绝");
        };
        assert!(
            err.contains("偏移不再是 0"),
            "错误要点名那条几何后果，而不是笼统的 schema 不符: {err}"
        );
        assert!(
            !dir.path().join("hat/sheet.webp").exists(),
            "这份资产根本没有配件像素 ⇒ 报出的错不可能来自解码"
        );

        // 换成一份好的：库 ⇒ 1 件，坏的那件被跳过而没有连带杀掉好的那件（硬约束 3）。
        let good = dir.path().join("good");
        write(&good, "accessory.json", HAT_JSON.as_bytes());
        write(
            &good,
            "polygon.json",
            polygon_json(T_CELL, &cells_with(2, HAT_SPANS, &[])).as_bytes(),
        );
        let found = discover_accessories(dir.path(), &body, &regions);
        assert_eq!(found.len(), 1, "坏的一件不该连带淘汰好的一件");
        assert_eq!(found[0].slug, "cap");
    }

    /// 配件**声明了但轮廓里没有**的格子 ⇒ 拒绝，而且错误里要带那个坐标。
    /// 少了坐标就变成"启动失败但没人知道差哪格"，那是§9.2 第 17 条那次账的典型形状。
    ///
    /// 现在这条路发生在**扫库**里，所以它的后果从"宠物起不来"换成"这一件不进换装页"（硬约束 3）。
    /// 断言还是放在 `check_accessory` 这一层：`discover_accessories` 只把它打进 stderr，而 stderr
    /// 在测试里断不到。
    #[test]
    fn an_accessory_cell_without_contour_is_refused_and_named() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (body, regions) = kit(dir.path(), &[], HAT_JSON, &[(1, 5)]);
        let Err(err) = check_accessory(&dir.path().join("hat"), &body, &regions) else {
            panic!("帽子缺 (1,5) 的轮廓 ⇒ 那一格会画出点不到的像素");
        };
        assert!(err.contains("(1,5)"), "错误要点名那一格: {err}");
        assert!(
            err.contains("点不到"),
            "错误要说清后果，否则下一个人只会把它当 schema 笔误: {err}"
        );
    }

    /// 反方向：**本体缺格而配件有**⇒ `merged_with` 拒绝。放行的话那一格不再是"缺格 ⇒ 退化成整格"，
    /// 于是画出来的本体像素点不到 —— 三条真机人工门禁守的正是这件事。
    #[test]
    fn a_cell_the_body_lacks_is_refused_rather_than_merged() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (body, regions) = kit(dir.path(), &[(1, 2)], HAT_JSON, &[]);
        let Err(err) = check_accessory(&dir.path().join("hat"), &body, &regions) else {
            panic!("本体缺 (1,2) 而帽子有 ⇒ 并进来会取消那次整格退化");
        };
        assert!(err.contains("(1,2)"), "错误要点名那一格: {err}");
    }

    /// 把一只本体那三样按测试需要落进 `dir/<name>/`（= 本体库里的一只）。
    ///
    /// 与 [`hat_in`] 同一条设计：**缺哪样就不写哪样**，"被拒绝时一个字节都没读"这种判据才断得出来。
    /// `rows == 0` ⇒ 只写清单（`check_body` 的第 ③ 道就是冲它设的）；`polygon = None` ⇒ 不写轮廓。
    fn pet_in(dir: &std::path::Path, name: &str, pet_json: &str, polygon: Option<&str>, rows: u32) {
        write(dir, &format!("{name}/pet.json"), pet_json.as_bytes());
        if let Some(p) = polygon {
            write(dir, &format!("{name}/polygon.json"), p.as_bytes());
        }
        if rows > 0 {
            write(
                dir,
                &format!("{name}/sheet.webp"),
                &sheet_bytes(rows, |_, _| [255, 0, 0, 255]),
            );
        }
    }

    /// 一套全过的本体库：库里两只同档 + 一只跨档 + 一只缺产物。
    /// 返回 `(当前档, 库目录)` —— `cur` 就是启动那份本体的 `grid`，与 `check_body` 收的同源。
    fn pet_kit(dir: &std::path::Path) -> (Grid, Vec<String>) {
        let cur = Manifest::parse(BODY_JSON).expect("本体清单该合法").grid;
        let poly = polygon_json(T_CELL, &cells_with(T_ROWS, BODY_SPANS, &[]));
        // ① 全过，而且目录名 == slug ⇒ 没有 ⚠ 那行
        pet_in(dir, "tiny", BODY_JSON, Some(&poly), T_ROWS);
        // ② 全过，但目录名 != slug ⇒ 走那行 ⚠（今天三份 @192 产物就是这个形状）
        let renamed = BODY_JSON.replace(r#""slug":"tiny""#, r#""slug":"cat_shiro""#);
        assert_ne!(renamed, BODY_JSON, "替换没生效 ⇒ 这条测试什么也没测");
        pet_in(dir, "tiny_second", &renamed, Some(&poly), T_ROWS);
        // ③ 跨档：32 对 16，而且**一件产物都不写** ⇒ 报出来的错不可能来自解码
        let wide = BODY_JSON.replace(r#""w":16,"h":16"#, r#""w":32,"h":32"#);
        assert_ne!(wide, BODY_JSON, "替换没生效 ⇒ 这条测试什么也没测");
        pet_in(dir, "wide", &wide, None, 0);
        // ④ 同档而缺 polygon/sheet ⇒ 第 ③ 道那句
        pet_in(dir, "bare", BODY_JSON, None, 0);
        // ⑤ **只有行数不同**（`w/h` 与 `cols` 全同）：这一只守的是那条门禁"逐字段"的那一半 ——
        // `rows` 决定解码缓冲长度与 polygon 面，把它当成"档"只比 `w/h` 的写法会放它进来，
        // 而放进来之后 `Atlas::reload` 的长度自检才炸 ⇒ 屏上是**一只像素错位的新本体**。
        let tall = BODY_JSON.replace(r#""rows":4"#, r#""rows":8"#);
        assert_ne!(tall, BODY_JSON, "替换没生效 ⇒ 这条测试什么也没测");
        let tall_poly = polygon_json(T_CELL, &cells_with(8, BODY_SPANS, &[]));
        pet_in(dir, "tall", &tall, Some(&tall_poly), 8);
        (cur, vec!["tiny".into(), "tiny_second".into()])
    }

    /// **格子档不同 ⇒ 在扫盘那一步就出局**（拍板②），而且另一只同档的照常活着（一坏不害众）。
    ///
    /// 三条读数缺一不可：① 跨档那只在 `discover_pets` 的结果里**根本不存在**（⇒ 收藏册那一行
    /// 是只读、没有强调条，而不是"点了没反应"）；② `check_body` 单独问时错误里必须带
    /// "格子档不同"这五个字和两边的**四个数**（`cols/rows/w/h`），否则真机上只能靠猜是哪一档；
    /// ③ 那份资产**没有** `sheet.webp` ⇒ 报出的错不可能来自解码（`Atlas::load` 一旦被人挪进
    /// 发现这一步，这条立刻红）。
    ///
    /// ⚠️ 这里断的是 `check_body`，不是 `plan_swap_body` 的 `same_grid` 那臂 —— 后者是**第二道
    /// 保险**，其唯一合法走到它的调用者是"有人绕过发现直接往 `bodies` 里塞"，所以它的读数在
    /// `plan_swap_body_partitions_the_four_outcomes` 那张真值表里，不在这里。
    #[test]
    fn a_different_grid_rivet_is_refused_by_name_at_discovery() {
        let tmp = tempfile::tempdir().expect("临时目录");
        let (cur, want_ids) = pet_kit(tmp.path());

        let found = discover_pets(tmp.path(), &cur);
        let ids: Vec<String> = found.iter().map(|b| b.id.clone()).collect();
        assert_eq!(
            ids, want_ids,
            "跨档与缺产物的两只必须出局，而活着的两只一只都不能少"
        );
        assert!(
            !ids.iter().any(|id| id == "wide"),
            "跨档那只进了表 ⇒ 收藏册会给一行点了必然失败的可点行"
        );

        // ② 单独问那道门禁：文案点名"格子档不同"，并带上四个数。
        let Err(err) = check_body(&tmp.path().join("wide"), &cur) else {
            panic!("32 对 16 的本体该被拒绝");
        };
        assert!(err.contains("格子档不同"), "错误要点名那一族: {err}");
        for token in ["16", "32", "8", "4"] {
            assert!(
                err.contains(token),
                "错误要带当前档与那份的数（缺 {token}）: {err}"
            );
        }
        assert!(
            !tmp.path().join("wide/sheet.webp").exists(),
            "这份资产根本没有本体像素 ⇒ 报出的错不可能来自解码"
        );

        // ④ 同档而缺 polygon ⇒ 另一句名，而且**不是**"格子档不同"（两族不能混成一谈）。
        let Err(err) = check_body(&tmp.path().join("bare"), &cur) else {
            panic!("缺 polygon.json 的本体该被拒绝");
        };
        assert!(
            err.contains("sheet.webp"),
            "缺产物要点名缺的是哪一件: {err}"
        );
        assert!(
            !err.contains("格子档"),
            "同档的错不该报成跨档（那样第二道保险就没法区分了）: {err}"
        );

        // ⑤ **只有行数不同** ⇒ 同一条门禁必须同样挡得住：判据是整块 `Grid` 比较，不是 `w/h`。
        // 放松成只比 `w/h`（变异 M5）在这一只上立刻放行，而它带的是 8 行的 sheet 与 polygon ⇒
        // 屏上会是一只像素错位的新本体，而 `Atlas::reload` 那道长度自检才是唯一还亮着的灯。
        assert!(
            !ids.iter().any(|id| id == "tall"),
            "只有行数不同的本体进了表 ⇒ 格子档门禁退化成尺寸门禁（变异 M5）: {ids:?}"
        );
        let Err(err) = check_body(&tmp.path().join("tall"), &cur) else {
            panic!("只有 `rows` 不同（4 对 8）的本体该被同一条门禁拒绝");
        };
        assert!(
            err.contains("格子档不同") && err.contains("4 行") && err.contains("8 行"),
            "错误要说出**行数**是哪一个数不同: {err}"
        );
    }

    /// 扫本体库**一像素不解**（把配件侧那条 `discovery_never_touches_the_sheet…` 扩到宠物）。
    ///
    /// 判据选的是"**内容坏而存在**"而不是"删掉文件"：删掉的话读到的是第 ③ 道的存在性检查；
    /// 而这里 `sheet.webp` 在、内容是三个字节，发现**只会**看它在不在 ⇒ 它照样进表。
    /// 哪天有人把解码挪进这一步，这份坏字节会当场让它红。
    ///
    /// ⚠️ 这份坏 sheet **之后**会怎样（`Atlas::reload` 失败关闭、原像素不动）已有专门的
    /// `atlas::tests::reload_keeps_the_old_pixels_when_the_new_sheet_is_broken`，这里不重复。
    /// 两条合起来才是完整的一条路：**扫得到**（不预付费）+ **换不上时不半换**。
    #[test]
    fn a_broken_pet_sheet_bytes_still_list_the_body_because_discovery_never_decodes() {
        let tmp = tempfile::tempdir().expect("临时目录");
        let cur = Manifest::parse(BODY_JSON).expect("本体清单该合法").grid;
        let poly = polygon_json(T_CELL, &cells_with(T_ROWS, BODY_SPANS, &[]));
        pet_in(tmp.path(), "tiny", BODY_JSON, Some(&poly), 0);
        write(tmp.path(), "tiny/sheet.webp", b"not a webp");

        let found = discover_pets(tmp.path(), &cur);
        assert_eq!(found.len(), 1, "sheet 内容不参与发现，否则这条就是假的");
        assert_eq!(found[0].id, "tiny");
        assert_eq!(found[0].sheet(), tmp.path().join("tiny/sheet.webp"));
        assert_eq!(
            found[0].regions.frame_count(),
            (T_ROWS * T_COLS) as usize,
            "候选只带轮廓 ⇒ 轮廓该已经解好"
        );
    }

    /// 全过那一趟：并集**不新增格**（配件格是本体格的子集）、拼接就是并集、本体那份没被改。
    ///
    /// 分两段走 = 真实的两段（扫库只到候选，穿上才解码）。这条因此同时是"M1 那两步各自的产物"
    /// 的证据：`merged` 属于候选、`slug` 在两边都得是同一件。
    #[test]
    fn the_merged_region_is_the_union_and_the_body_regions_stay_alone() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (body, regions) = kit(dir.path(), &[], HAT_JSON, &[]);
        let cand = sole(dir.path(), &body, &regions);
        let equipped = decode(&cand).expect("这套资产四道门禁全过又有像素 ⇒ 该穿得上");
        assert_eq!(equipped.manifest.slug, "cap");
        let merged = &cand.merged;
        assert_eq!(merged.frame_count(), 32, "并集不许凭空多出格子");

        // 一格一个 + 一格两个 = 拼接后三个：这条断的就是"直接拼接即并集"（`wl_region` 只吃矩形）。
        let rects = merged.rects(0, 0).expect("这格有轮廓");
        assert_eq!(rects.len(), 3, "本体 1 个 ++ 帽子 2 个: {rects:?}");
        // 借走而不是搬走的那份，调用完了还在手里，而且**还是 1 个** ⇒ 并集只长在返回的新对象上。
        // 这条比"从盘上重读"强：盘上那份从来不会被改（谁都没开写句柄），而这一份是真正递进
        // 函数去的那一个。
        assert_eq!(
            regions.rects(0, 0).map(<[CellRect]>::len),
            Some(1),
            "递进去的本体轮廓被并集改写了 ⇒ `merged_with` 不再是新造对象"
        );

        // 配件**没烘过**的状态（run = 行 2/3）轮廓里本来就没有 ⇒ 那些格逐位保持本体。
        for col in 0..T_COLS {
            assert_eq!(
                merged.rects(2, col).map(<[CellRect]>::len),
                Some(1),
                "行 2 帽子根本不画 ⇒ 穿透矩形不该多出来"
            );
        }
    }

    /// `covers` 那道闸门真的接在出图上：帽子只在 `idle` 烘过，`run` 里一帧都不许叠。
    /// 三个方向都断（叠了 / 没叠 / 叠成"帽子盖掉本体"），否则写反一次仍然全绿。
    #[test]
    fn blit_cell_skips_the_overlay_for_a_state_the_hat_never_drew() {
        let dir = tempfile::tempdir().expect("临时目录");
        let body_bytes = sheet_bytes(T_ROWS, |_, _| [0, 0, 255, 255]);
        let body_path = write(dir.path(), "body.webp", &body_bytes);
        let body_atlas = Atlas::load(
            body_path.to_str().expect("UTF-8"),
            T_COLS,
            T_ROWS,
            T_CELL,
            T_CELL,
        )
        .expect("本体图集");
        let hat_bytes = sheet_bytes(T_ROWS, |r, c| {
            if (r, c) == (0, 0) {
                [0, 255, 0, 128]
            } else {
                [255, 0, 0, 255]
            }
        });
        let hat_path = write(dir.path(), "hat.webp", &hat_bytes);
        let hat_atlas = Atlas::load(
            hat_path.to_str().expect("UTF-8"),
            T_COLS,
            T_ROWS,
            T_CELL,
            T_CELL,
        )
        .expect("配件图集");
        let hat = Equipped {
            manifest: AccessoryManifest::parse(HAT_JSON).expect("帽子清单该合法"),
            atlas: hat_atlas,
        };

        let n = (T_CELL * T_CELL) as usize;
        let stride = (T_CELL * 4) as usize;
        let mut plain = vec![0u8; n * 4];
        body_atlas
            .blit(0, 0, &mut plain, T_CELL, T_CELL, stride)
            .expect("本体那一格该画得出来");
        let mut covered = vec![0u8; n * 4];
        hat.atlas
            .blit(0, 0, &mut covered, T_CELL, T_CELL, stride)
            .expect("帽子那一格该画得出来");

        let mut idle = vec![0u8; n * 4];
        blit_cell(
            &mut idle,
            Some(&body_atlas),
            Some(&hat),
            (0, 0),
            AssetKey::Idle,
            T_CELL,
            T_CELL,
            stride,
        )
        .expect("idle 该叠上");
        let mut run = vec![0u8; n * 4];
        blit_cell(
            &mut run,
            Some(&body_atlas),
            Some(&hat),
            (0, 0),
            AssetKey::Run,
            T_CELL,
            T_CELL,
            stride,
        )
        .expect("run 也该出图（只是不叠）");

        for px in 0..n {
            let p = px * 4;
            let want: Vec<u8> = crate::atlas::composite_over(
                [covered[p], covered[p + 1], covered[p + 2], covered[p + 3]],
                [plain[p], plain[p + 1], plain[p + 2], plain[p + 3]],
            )
            .to_vec();
            assert_eq!(
                idle[p..p + 4],
                want[..],
                "第 {px} 个像素不是 source-over 的结果"
            );
            assert_eq!(run[p..p + 4], plain[p..p + 4], "run 里帽子漏出来了");
            assert_ne!(
                idle[p..p + 4],
                covered[p..p + 4],
                "叠 = 整格拷贝帽子 ⇒ blit 用错了"
            );
        }
    }

    /// 两边的失败**分得开**：本体检不到格 vs 配件图集比清单短。错误里必须带着是哪一侧，
    /// 否则真机上"帽子没显示"和"宠物没了"是同一条日志，只能靠猜。
    #[test]
    fn blit_cell_names_the_side_that_failed() {
        let dir = tempfile::tempdir().expect("临时目录");
        let short = write(
            dir.path(),
            "short.webp",
            &sheet_bytes(1, |_, _| [0, 0, 0, 0]),
        );
        let short_atlas = Atlas::load(short.to_str().expect("UTF-8"), T_COLS, 1, T_CELL, T_CELL)
            .expect("1 行的图集");
        let full = write(
            dir.path(),
            "full.webp",
            &sheet_bytes(T_ROWS, |_, _| [0, 0, 0, 0]),
        );
        let full_atlas = Atlas::load(
            full.to_str().expect("UTF-8"),
            T_COLS,
            T_ROWS,
            T_CELL,
            T_CELL,
        )
        .expect("4 行的图集");
        let hat = Equipped {
            manifest: AccessoryManifest::parse(HAT_JSON).expect("帽子清单该合法"),
            // 清单说 4 行而像素只有 1 行 = 不同批产物的形状（`covers(idle)` 仍为真）
            atlas: short_atlas,
        };
        let stride = (T_CELL * 4) as usize;
        let mut dst = vec![0u8; (T_CELL * T_CELL * 4) as usize];

        let Err(err) = blit_cell(
            &mut dst,
            Some(&full_atlas),
            Some(&hat),
            (3, 0),
            AssetKey::Idle,
            T_CELL,
            T_CELL,
            stride,
        ) else {
            panic!("配件图集只有 1 行，而清单说 4 行 ⇒ 行 3 必须失败");
        };
        assert!(
            err.starts_with("配件叠加"),
            "要指明失败的是配件那一侧: {err}"
        );
        assert!(err.contains("图集只有 1 行"), "要带上图集自己的说法: {err}");

        let Err(err) = blit_cell(
            &mut dst,
            Some(
                &Atlas::load(short.to_str().expect("UTF-8"), T_COLS, 1, T_CELL, T_CELL)
                    .expect("再Load一次短图集"),
            ),
            None,
            (3, 0),
            AssetKey::Idle,
            T_CELL,
            T_CELL,
            stride,
        ) else {
            panic!("本体检不到行 3 ⇒ 必须失败");
        };
        assert!(err.starts_with("blit"), "本体侧不该被写成配件的错: {err}");
    }
}
