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
/// 与 Wayland 那份 `app::Options` **有意不同形**：少了 `db` / `lang` / `motion` / `no_cursor`
/// / `ipc` / `allow_unowned_*`（本片的范围，理由逐条在 [`crate::cli::REFUSED`]），
/// 而 [`Options::panel_top`] / [`Options::panel_bottom`] 从 `f32` 变成了 `Option<f32>`。
/// 那一条不是整理，是**语义换档**，见字段注释。
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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            manifest: None,
            test_rect: false,
            // 与 Wayland 那份同值：不留"默认无限跑"这个选项给手工调用。
            seconds: Some(3.0),
            monitor: None,
            panel_top: None,
            panel_bottom: None,
            seed: 1,
        }
    }
}

impl Options {
    /// 一行把七个字段全打出来，供宿主桩与 Windows 的启动行共用。
    ///
    /// 它存在的第二个理由更硬：**这里读到每一个字段** ⇒ 谁加了第八个字段而忘了在下面列出来，
    /// 只有 [`crate::cli`] 那条平权测试会红（`Options` 多一个字段而 `parse` 不产它 = 那个字段
    /// 恒 `None` = 屏上有东西没人能开）。结构体字面量在这里是**全字段**的，所以漏字段会在
    /// 编译期炸 —— 那是比测试更快的一道。
    pub fn describe(&self) -> String {
        let yes_no = |b: bool| if b { "是" } else { "否" };
        format!(
            "manifest={:?} 矩形档={} 秒数={:?} 显示器={:?} 顶部留白={:?} 底部留白={:?} 种子={}",
            self.manifest,
            yes_no(self.test_rect),
            self.seconds,
            self.monitor,
            self.panel_top,
            self.panel_bottom,
            self.seed,
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
    use petcore::behavior::BehaviorConfig;
    use petcore::behavior::machine::Command;
    use petcore::geom::Vec2;
    use petcore::intent::{PresentationIntent, SURFACE_MARGIN};
    use petcore::sim::{Sim, TICK_HZ};
    use petcore::world::{Monitor, World, WorldEvent};

    use overlay_common::accounting::{Counters, Source, Tick, attribute_of, classify_tick};
    use overlay_common::atlas::Atlas;

    use crate::layout::{
        Frame, Insets, PhysMon, describe_inset, kernel_monitor, resolve_insets, to_phys,
    };
    use crate::stage::Stage;
    use crate::summary;
    use crate::window::{Client, DpiPath, Events, TimerPath, Wake};

    use super::Options;

    /// 补帧预算：与 Wayland 同一个数（`overlay-wayland/src/app.rs:97`）。
    ///
    /// 为什么要有上限：累积器一旦允许"落后多少就补多少"，一次 3 s 的卡顿会连着跑 180 个
    /// tick 并且每个都提交 —— 那既是硬约束 5 的 CPU 线直接爆掉，也是"宠物瞬移"的症状。
    const MAX_CATCHUP: u32 = 3;

    /// 双击折叠的时间窗与半径，**与 Wayland 逐字同值**（`app.rs:99-100` 的 320 ms / 24 逻辑 px）。
    ///
    /// 同值不是对齐好看：`Command::DoubleTap` 是确定性输入的一部分，而两平台必须从同一次
    /// 真实点击里折叠出同一条 `Command` 序列。半径是**逻辑**像素（内核坐标），不是物理像素。
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

    /// 一轮运行的全部业务状态。
    ///
    /// ⚠️ **`Client` 不是这里的字段**，这一条不是风格而是 UB 边界：`Client::pump` 要的
    /// `&'static mut dyn Events` 只能由 `App` 的堆地址形成一次（线性借用，`pump` 返回时随
    /// `Window::ev` 一起放掉）。如果 `Client` 在 `App` 里，那一轮就是
    /// `client.pump(&mut *app_raw)` —— **同一个对象上两个重叠的 `&mut`**，而它不会当场炸，
    /// 它会先写坏派发期间被读的字段。所以 `Client` 由 [`run`] 拥有，`App` 的方法一律按
    /// 参数收 `&mut Client`。
    ///
    /// 同一条纪律的第二个推论：[`Events::dpi_changed`] **只登记** `pending_scale`，
    /// 不在回调里重建 `Frame`/`Stage`/窗口 —— 派发发生在 `pump` 内部，那时环外没有任何
    /// `&mut Client` 能安全拿到。重算由事件环在 `pump` 返回之后做（[`App::apply_dpi`]）。
    struct App {
        opts: Options,
        atlas: Option<Atlas>,
        /// 行为内核。**它借的那份清单是 `Box::leak` 出来的 `&'static Manifest`**（`load_manifest`
        /// 那里说明为什么必须 leak），所以这里不需要再存一份 `&Manifest` —— 存了也没人读
        /// （`cell` 那一格是它唯一被读过的信息，已经在启动时抄成 `cell`）。
        sim: Option<Sim<'static>>,
        stage_buf: Stage,
        frame: Frame,
        /// 图集格尺寸，单独留着：换缩放时 `Frame` 要按它重建（清单此刻已经借不出来了）。
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
            let started = Instant::now();
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

            let manifest = load_manifest(&opts)?;
            let atlas = load_atlas(&opts, manifest)?;
            let cell = (manifest.grid.w, manifest.grid.h);

            let probe = Frame::new(1.0, cell.0, cell.1)?;
            let (pw, ph) = probe.surface_phys();
            let mut client = Client::open((stage.left, stage.top), (pw, ph))?;

            let scale = client.scale();
            let dpi_path = client.dpi_path();
            let timer_path = client.timer_path();
            let frame = Frame::new(scale, cell.0, cell.1)?;
            let (bw, bh) = frame.surface_phys();
            if (bw, bh) != (pw, ph) {
                // 开机那一档按 1.00 建，真 scale 不是 1.00 ⇒ 立刻换一次 DIB 与窗口尺寸。
                // 这一步不许省：症状是"宠物在 150% 屏上只有三分之二大"，而没有任何一步会报错。
                client.resize((bw, bh))?;
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
                client.move_only(rect_at)?;
                rect_at
            } else {
                // 本体档**没有**任何"窗口当前在这里"的说法——开机那一站只是显示器原点，
                // 而那是 `Client::open` 的位置，不是宠物该在的位置。写 `rect_at` 在这种
                // 屏幕上（窗口尺寸 == 落点余量）会与真的第一格落点相等 ⇒ 那一格被判
                // `moved=false`，虽然结果仍对，但那一格的"移动"就从日志里消失了。
                // 用一个任何落点都不可能取到的值，第一格才诚实。
                (i32::MIN, i32::MIN)
            };

            Ok((
                App {
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
                },
                client,
            ))
        }

        /// 事件环的一轮：DPI 变更 → 定时累积 → 步进内核 → 出图。
        ///
        /// 返回 `false` = 该停了（`--seconds` 到点）。
        fn round(&mut self, client: &mut Client) -> Result<bool, String> {
            let now = Instant::now();
            if let Some(dl) = self.deadline {
                if now >= dl {
                    return Ok(false);
                }
            }
            if let Some(s) = self.pending_scale.take() {
                self.apply_dpi(client, s)?;
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
            Ok(true)
        }

        /// 三级分派主入口（本平台版，语义与 `OverlayBackend::present` 对齐但不经过那条 trait，
        /// 理由见 [`Options::test_rect`]）。
        fn present(
            &mut self,
            client: &mut Client,
            intent: &PresentationIntent,
        ) -> Result<(), String> {
            let cell = intent.sprite.map(|s| (s.row as u32, s.col as u32));
            self.last_intent = Some(intent.clone());
            let (x, y, _, _) = self.frame.window_rect(&intent.surface);
            let at = (x, y);
            let moved = at != self.last_at;
            let new_cell = cell.is_some() && (cell != self.drawn_cell || self.force_draw);
            // `frame_pending` 传**常量 false**：分层窗口没有在飞 buffer 的回执，所以
            // `classify_tick` 的 `Deferred` 臂在本平台**结构上不可达**（摘要里那一档的
            // inline 标注就是为这一格写的）。传 true 才是 bug：那会把每一格都判成延后。
            let outcome = match classify_tick(new_cell, moved, false) {
                Tick::Move => match client.move_only(at) {
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
                    let painted = match (cell, self.atlas.as_ref()) {
                        (Some((row, col)), Some(atlas)) => self
                            .stage_buf
                            .paint(atlas, row, col)
                            .and_then(|()| client.present(at, self.stage_buf.bytes())),
                        other => Err(format!(
                            "本体出图路径缺了格或图集（{other:?}）⇒ 这不是矩形档，是接线漂移"
                        )),
                    };
                    match painted {
                        Ok(()) => {
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
            match client.present(self.last_at, self.stage_buf.bytes()) {
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
            client.resize((bw, bh))?;
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
            self.drag = true;
            if let Some(sim) = self.sim.as_mut() {
                // 顺序照 Wayland：`DoubleTap` 先、`DragStart` 后。同一次点击里"被叫了一下"
                // 与"被抓住"是两件事，而内核按到达顺序处理指令。
                if dbl {
                    sim.send(Command::DoubleTap);
                }
                sim.send(Command::DragStart);
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
            };
            println!("windows overlay 摘要：");
            for line in summary::lines(&run) {
                println!("{line}");
            }
        }
    }

    impl Events for App {
        fn hit(&self, x: i32, y: i32) -> bool {
            // 拖拽期间一律算命中 —— 这是 Wayland 那条"把 input region 临时撑成整块
            // surface"在本平台的同形做法。不这么写的症状与那边逐字相同：指针甩出剪影后
            // `HTTRANSPARENT` 把点击让给底下的窗口，`Release` 从此不归我们，内核卡在 `Dragged`。
            self.drag || self.stage_buf.hit(x, y)
        }

        fn press(&mut self, x: i32, y: i32) {
            self.on_press(self.to_world(x, y));
        }

        fn release(&mut self, x: i32, y: i32) {
            self.on_release(self.to_world(x, y));
        }

        fn motion(&mut self, x: i32, y: i32) {
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

        fn capture_lost(&mut self) {
            if self.drag {
                eprintln!("捕获被收回 ⇒ 提前结束拖拽（没等到 Release）");
                // 位置未知：按"最后已知指针位置"收尾，与 Wayland 的 Leave 分支同一个处置。
                let world = self.pointer_world;
                self.on_release(world);
            }
        }

        fn dpi_changed(&mut self, scale: f32) {
            self.pending_scale = Some(scale);
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

    fn load_atlas(opts: &Options, manifest: &Manifest) -> Result<Option<Atlas>, String> {
        if opts.test_rect {
            return Ok(None);
        }
        let path = opts
            .manifest
            .as_deref()
            .ok_or("--manifest 缺失而矩形档也没开（cli 应当已经拒掉这一组合）")?;
        let sheet = path
            .parent()
            .ok_or_else(|| format!("{} 没有父目录 ⇒ 找不到 sheet.webp", path.display()))?
            .join("sheet.webp");
        let sheet_str = sheet.to_str().ok_or("sheet 路径不是 UTF-8")?;
        let atlas = Atlas::load(
            sheet_str,
            manifest.grid.cols as u32,
            manifest.grid.rows as u32,
            manifest.grid.w,
            manifest.grid.h,
        )
        .map_err(|e| format!("{sheet_str}: {e}"))?;
        eprintln!(
            "asset: sheet {}x{} {} KB → 解码 {} ms / {:.3} MiB（{} 列 × {} 行，格 {}x{}）",
            atlas.width,
            atlas.height,
            atlas.encoded_bytes / 1024,
            atlas.decode_ms,
            atlas.decoded_bytes() as f64 / (1024.0 * 1024.0),
            manifest.grid.cols,
            manifest.grid.rows,
            manifest.grid.w,
            manifest.grid.h,
        );
        Ok(Some(atlas))
    }

    pub(super) fn run(options: Options) -> Result<(), String> {
        let (app, mut client) = App::startup(options)?;
        // `App` 放在**稳定的堆位置**并且整轮不移出：`pump` 要的 `'static` 借用是从这个
        // 地址形成的，而派发期间窗口回调读的就是它。
        let raw = Box::into_raw(Box::new(app));
        let outcome = drive(&mut client, raw);
        // SAFETY: `drive` 返回后没有任何 `&mut App` 还活着（`pump` 在返回时把 `Window::ev`
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
    /// 3. `pump` 返回 = 句柄已被消耗（`Window::ev` 清回 `None`）⇒ 重新取借用继续。
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
