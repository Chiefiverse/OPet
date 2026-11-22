//! 逻辑坐标 ↔ 物理像素，以及留白（任务栏让出来的那条边）的两个来源。
//!
//! ⚠️ **本文件一个 `windows-sys` 类型都不出现**，所以它在 Linux 宿主上能编能测。这不是洁癖：
//! M5 第一批的分工是"最吓人的两处数学不等上 Windows 就该红"，而窗口句柄一旦混进来，
//! 这两处就只能挂 `#[ignore]` —— 本项目对 `#[ignore]` 的读法是"这条没有读数"，不是"这条通过了"
//! （`AGENTS.md`「无显示环境也必须能跑 CI」那一节把 `SKIP` 和 `ok` 分开写，同一条理由）。
//!
//! ## 与 Wayland 侧的那一处不同，和它为什么不是漂移
//!
//! [`to_phys`] 与 `overlay-wayland` 的 `to_phys_of`（`app.rs:2098`）是**同一条式子**
//! `(logical * scale).round()`，两处各自取整的也是同三样（margin / 格宽 / 格高）。
//! 但 Wayland 那边多一层 `eff_units`：`wp_viewport` 缺席时分数档必须向下取整到整数，
//! 因为 `wl_surface.set_buffer_scale` 只吃整数（M4 踩坑第 2 条）。
//! **Win32 没有这个约束** —— `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` 之后
//! DPI 就是 `GetDpiForWindow` 给的那个整数，物理 = 逻辑 × dpi/96 直接成立。
//! 所以这里**刻意不搬** `eff_units`：搬过来会造出一个在本平台永远走"降级"那一支的分支，
//! 而那是 `--cell` 那一族（买了零东西的开关）。

use petcore::geom::Rect;
use petcore::intent::SURFACE_MARGIN;
use petcore::world::Monitor;

/// 逻辑像素 → 物理像素（四舍五入）。
///
/// 负数也走 `round()`：`(-0.5).round() == -1`（远离零），这与"屏幕坐标可以为负"（主屏左边的
/// 副屏）一致，而 `as i32` 的截断会把 `-0.6` 变成 `0` ⇒ 窗口右挪一像素且没人说得清为什么。
pub fn to_phys(logical: f32, scale: f32) -> i32 {
    (logical * scale).round() as i32
}

/// 一块覆盖层窗口的一次布局：显示器缩放 + 图集格尺寸。
///
/// `cell_w/h` 取自清单的 `grid.w/h`（@192 那一档 = 192），而 `SURFACE_MARGIN` 是内核
/// 给 surface 留的那圈余量（`petcore::intent:9`，16 逻辑 px）—— 三个数在这里是全部的几何输入。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// 生效缩放 = `GetDpiForWindow(hwnd) / 96.0`，**必须 > 0 且有限**（见 [`Frame::new`]）。
    pub scale: f32,
    pub cell_w: u32,
    pub cell_h: u32,
}

impl Frame {
    /// 拒绝 `scale <= 0` 与非有限值。
    ///
    /// 为什么这不是防御性的多余检查：`GetDpiForWindow` 在窗口还没挂到任何显示器时会返回 0，
    /// 而 0 缩放给出的缓冲是 **0 字节** ⇒ `Atlas::blit` 在 `dst_w == 0` 那一支直接 `Ok(())`
    /// （`atlas.rs:277`，它把"零尺寸目标"当合法的空任务）。两个各自正确的行为接起来 =
    /// **一只存在、每帧都在画、而屏上什么都没有的宠物**，退出摘要五档计数全绿。
    /// 这正好是本项目登记过两次的"沉默降级"形状（尾巴不摆 / 缺角色只打一行日志），
    /// 所以它在构造点就要不出手，而不是等眼睛。
    pub fn new(scale: f32, cell_w: u32, cell_h: u32) -> Result<Self, String> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(format!(
                "缩放 {scale} 不可用：必须是有限正数（GetDpiForWindow 返回 0 = 窗口尚未挂到显示器）"
            ));
        }
        if cell_w == 0 || cell_h == 0 {
            return Err(format!(
                "格子尺寸 {cell_w}x{cell_h} 不可用：清单的 grid.w/h 必须非零"
            ));
        }
        Ok(Self {
            scale,
            cell_w,
            cell_h,
        })
    }

    /// 那圈余量的物理宽度。**各自取整**与 Wayland 的 `cell_geometry()`（`app.rs:4786`）同形，
    /// 于是"格矩形"与"缓冲尺寸"这两件事在两个平台上由同一个规则给出。
    pub fn margin_phys(&self) -> i32 {
        to_phys(SURFACE_MARGIN, self.scale)
    }

    pub fn cell_phys(&self) -> (i32, i32) {
        (
            to_phys(self.cell_w as f32, self.scale),
            to_phys(self.cell_h as f32, self.scale),
        )
    }

    /// 窗口 = 缓冲 = 物理像素的宽高：`格 + 2 × margin`。
    ///
    /// ⚠️ 这是**权威**的那个数（DIB 与 `UpdateLayeredWindow` 的 `SIZE` 都读它），
    /// 不是 `to_phys(surface.size)`。后者在 `--test-rect` 那一档之外应当与它相等，
    /// 而"应当"要有测试，见 `the_kernel_surface_rect_agrees_at_three_scales`。
    pub fn surface_phys(&self) -> (i32, i32) {
        let (pw, ph) = self.cell_phys();
        let m = self.margin_phys();
        (pw + m * 2, ph + m * 2)
    }

    /// DIB 每行字节数（`CreateDIBSection` 的 `biWidth * 4`，与 [`Self::surface_phys`] 同源）。
    pub fn stride(&self) -> usize {
        (self.surface_phys().0 * 4) as usize
    }

    /// 格左上角在缓冲里的字节偏移 = `margin 行 × 跨距 + margin 列 × 4`。
    ///
    /// 与 `app.rs:4708` 那个 `off` 逐项相同。这里是**私有字段的唯一出口**之外的另一件事：
    /// `Stage` 拿它切出"只给 `Atlas::blit` 的那一段"。
    pub fn cell_offset_bytes(&self) -> usize {
        let m = self.margin_phys() as usize;
        m * self.stride() + m * 4
    }

    pub fn buffer_bytes(&self) -> usize {
        let (w, h) = self.surface_phys();
        (w * h * 4) as usize
    }

    /// 格内逻辑坐标 → 窗口物理像素。**[`Self::stage_to_local`] 的逆**（差半颗物理像素，
    /// 原因与那条测例的容差同一个：这一支要量化，那一支不量化）。
    ///
    /// 输入那一套坐标的出处 = 清单的 `anchor.feet`（[`petcore::assets::Manifest::feet_anchor`]）
    /// 与内核的 `hit_test_world` 换算结果：**原点在该格的左上角，向右向下为正，单位是逻辑 px**。
    /// 加 margin 而不是乘完再加 ⇒ 与 [`Self::cell_offset_bytes`] 同一口径，画与点必落在同一像素。
    ///
    /// ⚠️ **`#[cfg(test)]` + `pub(crate)`，不是对外 API**，因为本片**没有任何生产调用方**，
    /// 而这件事是实测的而不是"我没找到"：Windows 的命中判定读的是刚交给 `UpdateLayeredWindow`
    /// 那块 buffer 的**物理**像素（`crate::stage::Stage::hit` 直接收物理 px，不需要换算），
    /// 拖拽要的又是**反方向**（`stage_to_local`）。Wayland 那侧两个方向都不需要 ——
    /// `app.rs:5331-5337` 的指针换算就是 `surface.pos + e.position`，而 `e.position` 本来
    /// 是**逻辑** surface 坐标 ⇒ 那个平台从头到尾没有 px↔逻辑 这一次乘除。
    /// 所以它剩下的唯一用途是：**与 `stage_to_local` 配成一对可证可逆的换算**，让那条不变式
    /// 能被往返测试钉住（`the_two_directions_are_one_inverse_pair`）。
    /// 明写 `cfg(test)` 而不是留着当"以后可能用"的公开面：一个从没被生产调用过的 `pub` 函数
    /// 与一个假开关的差别**只在于有没有人说清它是谁的**。真来了生产调用方，摘掉这一行就是全部改动。
    #[cfg(test)]
    pub(crate) fn cell_to_stage(&self, x: f32, y: f32) -> (i32, i32) {
        let m = self.margin_phys();
        (m + to_phys(x, self.scale), m + to_phys(y, self.scale))
    }

    /// 窗口物理像素 → surface 内的逻辑坐标（原点 = surface 左上角，即 margin 的**外侧**）。
    ///
    /// 第 8 步的拖拽走这一支：`WM_MOUSEMOVE`/`WM_LBUTTON*` 的 `lParam` 是**窗口相对物理 px**
    ///（进程是 PER_MONITOR_AWARE_V2 ⇒ OS 不做任何 DPI 虚拟化，解包见 `crate::windx` 那三条
    /// 有符号测试），减掉 margin、除以 scale 才回到内核说话的那套逻辑坐标，再加上
    /// `intent.surface.pos` 才是世界坐标 —— 与 Wayland 的 `surface.pos + local` 同一条链，
    /// 只是那边少一次除法（它的 `local` 已经是逻辑的）。
    ///
    /// **不做整数化**：返回的是 `f32`，因为内核位置本来就是浮点；把它钉成整数 px 会让
    /// 1.25/1.5× 屏上的拖拽"粘格子"（每移 1 物理 px 只走 0.8 或 0.667 逻辑 px）。
    /// 也正因为它不量化、而 `cell_to_stage` 量化，**两个方向的往返不可能逐位相等**
    /// （1.25 档 `3.0 → 4 → 3.2`）⇒ 那条测例断的是 `≤ 0.5/scale`（半颗物理像素），
    /// 并在 scale 1.0 那一档另开一条严格相等的。不变式要钉的是"画与点不会指到不同的格"，
    /// 不是"浮点双向无损"。
    pub fn stage_to_local(&self, px: i32, py: i32) -> (f32, f32) {
        let m = self.margin_phys();
        ((px - m) as f32 / self.scale, (py - m) as f32 / self.scale)
    }

    /// 内核给的 surface 矩形（世界逻辑坐标）→ `SetWindowPos` 要的 `(x, y, w, h)` 物理像素。
    ///
    /// 尺寸一律取 [`Self::surface_phys`] 而**不**用 `rect.size`：窗口尺寸与 DIB 尺寸必须逐像素
    /// 相等，否则 `UpdateLayeredWindow` 会自己插值（`ULW_ALPHA` 双线性 = 糊边，M4 那条
    /// "拒绝插值"的理由在 Windows 上更长，因为 ULW 连"有没有缩放"都不告诉你）。
    pub fn window_rect(&self, surface: &Rect) -> (i32, i32, i32, i32) {
        let (w, h) = self.surface_phys();
        (
            to_phys(surface.pos.x, self.scale),
            to_phys(surface.pos.y, self.scale),
            w,
            h,
        )
    }
}

/// 一条留白是谁给的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsetSource {
    /// `SPI_GETWORKAREA` / `GetMonitorInfoW` 的现量（`rcWork.top - rcMonitor.top` 那一档）。
    Measured,
    /// `--panel-top` / `--panel-bottom` 的覆盖值。
    Override,
}

/// 一条边上的留白：值 + 谁赢了。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inset {
    pub px: f32,
    pub source: InsetSource,
}

/// 旗**覆盖**现量，不给才现量。
///
/// ⚠️ 这一条语义与 Wayland 侧**不同**，而且是平台给的：layer-shell 读不到 exclusive zone
/// （`wl_output`/`xdg_output` 只报整块几何），所以那边 `--panel-top N` 是在"量不到的 0"上
/// **加**出 N（M4 踩坑第 4 条）。Win32 这边 `rcWork` 直接就读得到任务栏留的高，
/// 于是"加"会变成"任务栏 40 + 旗 40 = 80"这种没人要的东西。
///
/// 正因如此 `Options::panel_top` 是 `Option<f32>` 而不是 `f32`：把 `Some(0.0)` 与 `None`
/// 压成同一个数，那条旗就退化成"只有 nonzero 才有意义"的半残开关（`--cell` 一族）。
/// `Some(0.0)` 在这里是真话："我知道任务栏高 40，我就要贴顶"。
pub fn resolve_inset(flag: Option<f32>, measured: f32) -> Inset {
    match flag {
        Some(px) => Inset {
            px,
            source: InsetSource::Override,
        },
        None => Inset {
            px: measured,
            source: InsetSource::Measured,
        },
    }
}

/// 启动行要说清"谁赢了"，而且要连**没赢的那个值**一起说。
///
/// 这不是文案偏好：`AGENTS.md` 现场事实那一族里，"降级只打一行日志"与"日志里读不出谁生效"
/// 是同一类事故的两次成因。`panel_top 40（现量 40）` 与 `panel_top 40（旗覆盖，现量 0）`
/// 是两个不同的事实，前者是"量到了"，后者是"用户点名要 0 而系统说没有任务栏"。
pub fn describe_inset(which: &str, inset: &Inset, measured: f32) -> String {
    match inset.source {
        InsetSource::Measured => format!("{which} {}（现量）", inset.px),
        InsetSource::Override => {
            format!("{which} {}（旗覆盖，现量 {measured}）", inset.px)
        }
    }
}

// ───────────────────── 物理显示器 → 内核那块舞台 ─────────────────────

/// 一台显示器的**物理**几何，形状 = `GetMonitorInfoW` 那两个 `RECT` 的摊平。
///
/// 为什么在 `layout.rs` 而不是 `monitor.rs`：`rcWork.top - rcMonitor.top` ÷ scale 之后
/// 减去谁、夹不夹负、`rect.h` 最少留多少 —— 这几条是本片唯一的舞台数学，而它们必须在
/// Linux 上跑到（同文件开头那条理由）。`monitor.rs` 因此只剩"把 GDI 的结构填进这个类型"，
/// 那一半除了上真机没有别的判据，也就没有可省的测试。
///
/// ⚠️ `work_left` / `work_right` **刻意不在这张表上**：内核舞台只补顶、底两条边
/// （Wayland 侧 `sync_world` 也是 `rect.y += top` 一处，左右那两档在 `hyprctl` 里
/// 同样是 `reserved: 0 26 0 0` 的 0）。竖排任务栏在 Windows 上确实会留左右 ——
/// 那一格是**未接**，不是"读了但没用到"，登记在第 11 步的回写里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysMon {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    /// `rcWork.top`（物理）。
    pub work_top: i32,
    /// `rcWork.bottom`（物理）。
    pub work_bottom: i32,
    /// OS 说的那台主屏（`MONITORINFOF_PRIMARY`）。
    ///
    /// ⚠️ 这**不是** `petcore::world::Monitor::primary` 的语义 —— 内核那个字段读作
    /// "舞台那台"（`world.rs:146` 拿它 `position()` 选 stage），与 OS 主屏无关。
    /// 两个名字 alike、含义不同 = 一条等着被接错的线，所以这里把它单独留着，
    /// 只用于"取不到窗口 DPI 时该按哪台算 scale"这一件事。
    pub os_primary: bool,
}

impl PhysMon {
    /// 任务栏在这一侧留的物理像素。**负数夹到 0**。
    ///
    /// 会负不是理论：远程桌面/虚拟显示驱动报的 `rcWork` 可以**超出** `rcMonitor`
    /// （实测形状 = 某些 RDP 会话里工作区被报成整块屏再往下延），那时"减一下"
    /// 得到负留白 ⇒ `rect.y` 往**上**跑、`rect.h` 往**大**长 ⇒ 宠物有一半站在屏外，
    /// 而它自己走不回来（`landing()` 只拦从上方穿过地面）。
    pub fn inset_top_phys(&self) -> i32 {
        (self.work_top - self.top).max(0)
    }

    pub fn inset_bottom_phys(&self) -> i32 {
        (self.bottom - self.work_bottom).max(0)
    }
}

/// 两条边的留白，连同"现量原本是多少"。
///
/// `measured_*` 之所以要留在这张表上而不是算完就丢：启动行必须说清**谁赢了**
/// （[`describe_inset`]），而旗覆盖时"没赢的那个数"是唯一能证明现量真的跑过的证据。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Insets {
    pub top: Inset,
    pub bottom: Inset,
    /// 逻辑像素，舞台那台的现量。
    pub measured_top: f32,
    pub measured_bottom: f32,
}

/// 物理留白 → 逻辑 → 与旗做"覆盖"判定。**只对舞台那台现量**，其余台沿用同一档。
///
/// 沿用（而不是各台各量）与 Wayland 侧逐字同口径，代价明写：多屏且任务栏在副屏
/// 那一侧时，副屏的舞台顶边会按主屏的留白算。这一格从未实测（用户的机器是单屏），
/// 属 M7/多屏活。
pub fn resolve_insets(
    stage: &PhysMon,
    scale: f32,
    flag_top: Option<f32>,
    flag_bottom: Option<f32>,
) -> Insets {
    let s = safe_scale(scale);
    let measured_top = stage.inset_top_phys() as f32 / s;
    let measured_bottom = stage.inset_bottom_phys() as f32 / s;
    Insets {
        top: resolve_inset(flag_top, measured_top),
        bottom: resolve_inset(flag_bottom, measured_bottom),
        measured_top,
        measured_bottom,
    }
}

/// `scale` 必须是正的有限数；不是就当 1.0。
///
/// 为什么不 panic 也不报错：这个数来自 `GetDpiForWindow / 96`，而 0（驱动返回怪值）
/// 走 panic 就是"宠物起不来"，硬约束 3 不许；走"报错退出"同样把一次显示驱动的怪值
/// 变成整个功能不可用。1.0 是**取不到 DPI 时那条回退路径本来就该给的值**，
/// 于是这一夹不是猜，而是与回退同一个口径；调用方另外会把走了哪条路径打出来。
fn safe_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Win32 的"100% 缩放"那个基准 DPI。
///
/// 微软把 96 写死在文档里（"96 DPI = 100%"），而 `GetDpiForWindow` 给的是绝对值不是比例，
/// 所以这一层除法是本片唯一能把 DPI 换成 `Frame::scale` 的地方 —— 留一个常量而不是散在各处
/// 写 96.0，是因为 120 那一档（125%）与 168（175%）都必须是**同一个基准**除出来的。
pub const BASE_DPI: f32 = 96.0;

/// `GetDpiForWindow` / `WM_DPICHANGED` 那个整数 DPI → 生效缩放。
///
/// `dpi == 0` 是那两次调用在窗口尚未挂到显示器时的返回，它**不等于** 0% 缩放：
/// 走 [`safe_scale`] 给 1.00，与 [`crate::DpiPath::Assume100`] 那条回退同一个口径，
/// 而"取到 0"这件事由调用方打印，不由这个函数决定。
pub fn scale_from_dpi(dpi: u32) -> f32 {
    safe_scale(dpi as f32 / BASE_DPI)
}

/// 一台物理显示器 + 生效缩放 + 那两条留白 → 内核要的 [`Monitor`]。
///
/// 三条与 Wayland 侧**必须**一致的形状，逐条钉在下面的测例里：
/// - `rect` 是**可见**那块：顶边减掉 `top.px`，**底边不减**（底归 `panel_bottom`，
///   由 `Monitor::ground_y()` 自己扣 ⇒ 这里再扣就是重复减一次，M4 踩坑第 4 条同一族）；
/// - 高度下限 1.0：一台被报成 0 高的显示器不能让内核拿到退化矩形；
/// - `primary` 填"是不是舞台"，不是 OS 主屏（见 [`PhysMon::os_primary`]）。
///
/// 与 Wayland 的那一处真差别：那边几何本来就是**逻辑**的（`wl_output` 给 logical），
/// 这里进来的是**物理**像素 ⇒ 每次除法都是本片新增的一层，而 `Frame::stage_to_local`
/// 那个逆方向是它的另一面（`lib.rs` 开头那格坐标系）。
pub fn kernel_monitor(m: &PhysMon, scale: f32, insets: &Insets, is_stage: bool) -> Monitor {
    let s = safe_scale(scale);
    let w = (m.right - m.left) as f32 / s;
    let h = (m.bottom - m.top) as f32 / s;
    Monitor {
        name: m.name.clone(),
        rect: Rect::new(
            m.left as f32 / s,
            m.top as f32 / s + insets.top.px,
            w,
            (h - insets.top.px).max(1.0),
        ),
        scale: s,
        primary: is_stage,
        panel_bottom: insets.bottom.px,
    }
}

/// 挑舞台那台：`--monitor` 指名字 > OS 主屏 > 枚举到的第一台。
///
/// 这一段留在宿主侧而不是 `monitor.rs`，因为它的输入只是一张 [`PhysMon`] 表 ——
/// 而"用户点了 `\\\\.\\DISPLAY2` 结果宠物上了主屏"这一族错，在 Windows 真机上
/// 每次复现都要一次远程桌面。宿主上它是三行断言。
///
/// 三条规则各自的理由：
/// - **大小写不敏感**：`szDevice` 给的是 `\\.\DISPLAY1`（全大写），而用户在命令行里
///   打 `display1` 是完全自然的。两边都不是"错的那个"，所以由我们来折叠。
///   ⚠️ 只折叠 ASCII 大小写语义（`eq_ignore_ascii_case`）而**不做** Unicode 归一：
///   设备名由系统给、不是用户文件名，`ẛ` 那一类在这里没有来源。
/// - **点名点不到 ⇒ 返回 `None` 让调用方报错**，绝不回退到主屏。理由与
///   `--panel-top` 那格同源：静默换台 = 用户以为宠物在第二屏、它却在第一屏，
///   而屏幕上没有任何一行说过这件事（本项目"提示不是判据"那一族的反面 =
///   **该报错的时候必须报错**）。
/// - **没有 OS 主屏时取第一台**：`MONITORINFOF_PRIMARY` 在多屏拔线/远程桌面的
///   某一瞬间可以一个都不带（Windows 那时仍会枚举出一台）。空表才 `None`。
pub fn choose<'a>(mons: &'a [PhysMon], wanted: Option<&str>) -> Option<&'a PhysMon> {
    if let Some(w) = wanted {
        return mons.iter().find(|m| m.name.eq_ignore_ascii_case(w));
    }
    mons.iter().find(|m| m.os_primary).or_else(|| mons.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用：一台 1920×1080 @1.0、任务栏在顶 48 底 0 的显示器。
    fn mon(left: i32, top: i32, right: i32, bottom: i32) -> PhysMon {
        PhysMon {
            name: r"\\.\DISPLAY1".into(),
            left,
            top,
            right,
            bottom,
            work_top: top + 48,
            work_bottom: bottom,
            os_primary: true,
        }
    }

    /// @192 那一档的真格子（清单 `grid.w/h`），margin 用内核那个 16。
    const CELL: u32 = 192;

    fn frame(scale: f32) -> Frame {
        Frame::new(scale, CELL, CELL).expect("测试里的缩放总是合法的")
    }

    /// 三档缩放的**全部**几何读数一次列清。这张表就是第 8 步 DIB 尺寸的唯一出处。
    #[test]
    fn three_scales_have_their_geometry_pinned() {
        let rows = [(1.0f32, 192i32, 16i32), (1.25, 240, 20), (1.5, 288, 24)];
        for &(scale, want_cell, want_margin) in &rows {
            let f = frame(scale);
            assert_eq!(
                (f.cell_phys().0, f.margin_phys()),
                (want_cell, want_margin),
                "scale {scale}"
            );
            assert_eq!(
                f.surface_phys(),
                (want_cell + want_margin * 2, want_cell + want_margin * 2),
                "scale {scale} 的窗口尺寸"
            );
            assert_eq!(
                f.stride(),
                (want_cell as usize + want_margin as usize * 2) * 4
            );
            assert_eq!(f.buffer_bytes(), f.stride() * f.surface_phys().1 as usize);
        }
    }

    /// 内核的 surface 矩形必须与 [`Frame::surface_phys`] 相等 —— 不等就是"窗口比宠物大/小"，
    /// 而 ULW 会**静默插值**补掉那一格（M4 拒绝插值的理由在 Windows 上更硬）。
    #[test]
    fn the_kernel_surface_rect_agrees_at_three_scales() {
        for scale in [1.0f32, 1.25, 1.5] {
            let f = frame(scale);
            let (cw, ch) = (f.cell_w as f32, f.cell_h as f32);
            // sim.rs:219-233 那个构造：pos = 锚点 - feet - margin，size = 格 + 2×margin
            let surface = Rect::new(
                500.0 - cw / 2.0 - SURFACE_MARGIN,
                700.0 - ch - SURFACE_MARGIN,
                cw + SURFACE_MARGIN * 2.0,
                ch + SURFACE_MARGIN * 2.0,
            );
            let (w, h) = f.surface_phys();
            assert_eq!(
                (
                    to_phys(surface.size.x, scale),
                    to_phys(surface.size.y, scale)
                ),
                (w, h),
                "scale {scale}：内核说的 surface 尺寸与 DIB 尺寸不一致"
            );
        }
    }

    /// 0 / 负 / NaN / Infinity 缩放必须在构造点就被拒 —— 它们给出的是一只
    /// "每帧都在画、屏上什么都没有"的宠物，而五档计数全绿。
    #[test]
    fn a_useless_scale_is_refused_at_construction() {
        for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
            let e = Frame::new(bad, CELL, CELL).expect_err(&format!("{bad} 必须被拒"));
            assert!(e.contains("缩放"), "{bad} ⇒ {e}");
        }
        // 格子为 0 同样（清单能读出 0 吗？能：`grid` 是 `HashMap` 来的 u32，无下限校验）。
        assert!(Frame::new(1.0, 0, CELL).is_err());
        assert!(Frame::new(1.0, CELL, 0).is_err());
    }

    /// 负坐标不许被截断：主屏左边的副屏给的就是负 x，而 `as i32` 会把 `-0.6` 变成 `0`。
    #[test]
    fn negative_coordinates_round_away_from_zero() {
        assert_eq!(to_phys(-0.6, 1.0), -1);
        assert_eq!(to_phys(-1.2, 1.25), -2); // -1.5 → 远离零 = -2
        assert_eq!(to_phys(-20.0, 1.5), -30);
    }

    /// `cell_to_stage` 与 `cell_offset_bytes` 必须指同一颗像素。
    ///
    /// 判据写法：格内逻辑 `(0, 0)` 得到的 stage 坐标，其字节下标必须恰好等于
    /// `cell_offset_bytes()`。这两处一旦分叉，症状是"点得到画不出"或"画出来点不动"，
    /// 而它们在屏上都只是"差了一小圈"。
    #[test]
    fn the_paint_offset_and_the_hit_offset_are_the_one_number() {
        for scale in [1.0f32, 1.25, 1.5] {
            let f = frame(scale);
            let (x, y) = f.cell_to_stage(0.0, 0.0);
            let m = f.margin_phys();
            assert_eq!(
                (x, y),
                (m, m),
                "scale {scale}：格左上角应落在 margin 之内侧"
            );
            assert_eq!(
                (y as usize) * f.stride() + (x as usize) * 4,
                f.cell_offset_bytes(),
                "scale {scale}：画与点用的不是同一颗像素"
            );
        }
    }

    /// **两个方向必须互逆到"半颗物理像素"以内**，且逆的那一支必须能接住 `cell_offset_bytes`
    /// 解出来的那颗像素。
    ///
    /// 这条是上面那条与 [`Frame::stage_to_local`] 之间真正的加固：单向测只钉住"画的那一侧"，
    /// 而第 8 步的拖拽读的是另一侧。两侧各写各的换算 ⇒ 症状是"拖起来偏手"，
    /// 而那在 1.0 缩放下**一个像素都不差**，只有 1.25/1.5 才现形（本项目"假开关"那一族
    /// 的反面：一个只在非默认档成立的换算）。
    ///
    /// ⚠️ **断言强度第一次写错，记在这儿以免第二次写错**：我第一版要求整数逻辑 px
    /// **逐位回来**（容差 `1e-4`），在 1.25 档就红了 —— `(3.0 → to_phys=4 → 3.2)`。
    /// 那不是换算有病，是**正方向本来就要量化**（`to_phys` 的 `.round()`），
    /// 所以"逐位可逆"是一个**永远不可能成立**的要求，而反方向不量化（返回 `f32`）。
    /// 正确的不变式是 **`|误差| ≤ 0.5 / scale`**：往返最多差半颗物理像素，
    /// 也就是"点得到的那格与画出来的那格不会不是同一格"。
    /// 唯一能逐位相等的档是 scale 1.0（`to_phys` 在那儿是恒等），所以那一档单独**严格**断言，
    /// 免得这条测试整体退化成"放宽阈值直到绿"（本项目对假红的处置是修仪器，反过来也一样：
    /// **对假绿的答案是另开一条更严的**）。
    #[test]
    fn the_two_directions_are_one_inverse_pair() {
        let points = [
            (0.0f32, 0.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (3.0, 7.0),
            (96.0, 192.0),
        ];
        for scale in [1.0f32, 1.25, 1.5] {
            let f = frame(scale);
            let tol = 0.5 / scale + 1e-5;
            for (lx, ly) in points {
                let (px, py) = f.cell_to_stage(lx, ly);
                let (bx, by) = f.stage_to_local(px, py);
                if scale == 1.0 {
                    assert_eq!(
                        (bx, by),
                        (lx, ly),
                        "scale 1.0 下 to_phys 是恒等 ⇒ 往返必须逐位相等，实得 ({bx},{by})"
                    );
                } else {
                    assert!(
                        (bx - lx).abs() <= tol && (by - ly).abs() <= tol,
                        "scale {scale}: 逻辑 ({lx},{ly}) → 物理 ({px},{py}) → 逻辑 ({bx},{by}) \
                         超出半颗物理像素（容差 {tol}）⇒ 画与点已分叉"
                    );
                }
            }
            // ② 画的那一侧：把 cell_offset_bytes 反解成 (x, y)，它必须正好是格的左上角。
            // 这一条**必须是严格相等**：格原点由两条独立公式给出（margin 与 偏移字节），
            // 半像素容差在这里会把"差一整颗 margin"读成通过。
            let off = f.cell_offset_bytes();
            let stride = f.stride();
            let (px, py) = ((off % stride) / 4, off / stride);
            let (lx, ly) = f.stage_to_local(px as i32, py as i32);
            assert!(
                lx == 0.0 && ly == 0.0,
                "scale {scale}: 写入偏移解出 ({px},{py}) 却不是格原点 ⇒ 画与点已分叉 ({lx},{ly})"
            );
        }
    }

    /// 脚锚在格内贴底（`anchor.feet = [w/2, h]` 的默认口径）⇒ 换算后必须仍在缓冲内，
    /// 且**下一行**（格外的 margin 那圈）也在缓冲内 —— 那圈是全透明的，第 8 步的命中判定读它。
    #[test]
    fn the_feet_anchor_lands_inside_the_buffer_with_margin_to_spare() {
        for scale in [1.0f32, 1.25, 1.5] {
            let f = frame(scale);
            let (x, y) = f.cell_to_stage(CELL as f32 / 2.0, CELL as f32);
            let (w, h) = f.surface_phys();
            assert!(x >= 0 && x < w, "scale {scale}: x {x} 越出宽 {w}");
            assert!(
                y + 1 < h,
                "scale {scale}: 脚底 y {y} 的下一行已越出缓冲（高 {h}）"
            );
        }
    }

    /// 留白：旗覆盖现量，`Some(0.0)` 与 `None` **必须**给出不同来源。
    #[test]
    fn an_explicit_zero_inset_is_not_the_same_as_not_asking() {
        // 不给 ⇒ 用现量
        let m = resolve_inset(None, 40.0);
        assert_eq!(m.px, 40.0);
        assert_eq!(m.source, InsetSource::Measured);
        // 给 0 ⇒ 0，而且来源要说"旗覆盖"（那才是"用户知道有 40 还要贴顶"）
        let z = resolve_inset(Some(0.0), 40.0);
        assert_eq!(z.px, 0.0);
        assert_eq!(z.source, InsetSource::Override);
        // 给值 ⇒ 覆盖
        assert_eq!(resolve_inset(Some(7.5), 40.0).px, 7.5);
    }

    /// 启动行必须点名"谁赢了"，还要把没赢那个数一起打出来。
    ///
    /// 这条断的是**字符串**，因为第 10 步真机回报单要读它：用户抄回的一行
    /// `panel_top …` 是唯一能远程判断"DPI 那格走没走对"的读数。
    #[test]
    fn the_startup_line_names_the_winner_and_the_loser() {
        let measured = resolve_inset(None, 40.0);
        let s = describe_inset("panel_top", &measured, 40.0);
        assert!(s.contains("现量"), "{s}");
        assert!(!s.contains("覆盖"), "{s}");

        let over = resolve_inset(Some(0.0), 40.0);
        let s = describe_inset("panel_top", &over, 40.0);
        assert!(s.contains("旗覆盖"), "{s}");
        assert!(s.contains("40"), "{s} 里必须还能看到被覆盖掉的那个现量");
        assert!(s.starts_with("panel_top 0"), "{s}");
    }

    /// [`choose`] 的三条规则，逐条钉。
    ///
    /// 这函数在 Windows 真机上错一次的代价 = 一次远程桌面，而它的输入只是一张表
    /// ⇒ 宿主上写全是赚的。三条里最要紧的是**中间那条**（点不到就 `None`）：
    /// 静默回退到主屏是"宠物跑去了你没看的那块屏"的同一种形状。
    #[test]
    fn the_stage_monitor_is_chosen_by_a_three_step_ladder() {
        let mut a = mon(0, 0, 1920, 1080);
        a.name = r"\\.\DISPLAY1".into();
        let mut b = mon(-1920, 0, 0, 1080);
        b.name = r"\\.\DISPLAY2".into();
        b.os_primary = false;
        let both = vec![a.clone(), b.clone()];

        // ① 点名：命中第二台，且**大小写不敏感**
        assert_eq!(
            choose(&both, Some(r"\\.\DISPLAY2")).map(|m| &m.name),
            Some(&b.name)
        );
        assert_eq!(
            choose(&both, Some(r"\\.\display2")).map(|m| &m.name),
            Some(&b.name),
            "设备名由系统全大写给，用户在命令行里打小写不算错 ⇒ 不许miss"
        );

        // ② 点不到 ⇒ None（不许回退到主屏）
        assert_eq!(choose(&both, Some(r"\\.\DISPLAY3")), None);

        // ③ 不点名 ⇒ OS 主屏
        assert_eq!(choose(&both, None).map(|m| &m.name), Some(&a.name));

        // ④ 一台都不带主屏旗 ⇒ 第一台；空表 ⇒ None
        let mut no_primary = both.clone();
        for m in &mut no_primary {
            m.os_primary = false;
        }
        assert_eq!(choose(&no_primary, None).map(|m| &m.name), Some(&a.name));
        assert_eq!(choose(&[], None), None);
        assert_eq!(choose(&[], Some(r"\\.\DISPLAY1")), None);

        // 反面对照：如果 ② 写成"点不到就 `first()`"，下面这两行必须能看出差别
        //（否则②那条断言在"无论如何都返回第一台"的实现下也绿 = 验空集）
        let fallback = choose(&both, Some(r"\\.\DISPLAY3"))
            .or(both.first())
            .unwrap();
        assert_ne!(fallback.name, b.name, "②的对照失效：回退恰好等于正确答案");
    }

    /// 整数 DPI → 缩放这一颗除法，档位表 + 那个 0。
    ///
    /// 为什么单独钉而不是并进 [`three_scales_have_their_geometry_pinned`]：那张表**直接写死
    /// 1.0/1.25/1.5 三个浮点**，所以它验的是"给定缩放后的几何"，完全没验**缩放是怎么来的**。
    /// 而 Windows 给的是绝对 DPI ⇒ 基准写错一次（比如拿 120 当基准）在宿主上一行代码都不会红，
    /// 症状却是真机上整只宠物小 25%、脚悬在半空。
    /// `0` 那一档是本片唯一能把 `GetDpiForWindow` 的"窗口尚未挂到显示器"接住的地方，
    /// 它必须给 **1.00**（与 `window::DpiPath::Assume100` 同口径），不许给 0.0 ——
    /// 0 缩放会造出 0 字节缓冲，而 `Atlas::blit` 把零尺寸目标当合法空任务（`Frame::new`
    /// 那条注释里登记的"存在、每帧都画、屏上什么都没有"）。
    #[test]
    fn the_dpi_to_scale_division_uses_ninety_six_and_survives_zero() {
        for (dpi, scale) in [
            (96u32, 1.0f32),
            (120, 1.25),
            (144, 1.5),
            (168, 1.75),
            (192, 2.0),
        ] {
            assert_eq!(
                scale_from_dpi(dpi),
                scale,
                "{dpi} DPI 在微软的口径里就是 {scale} 倍 ⇒ 基准不是 96 的话这里必然偏"
            );
            // 这颗除法必须能喂给 Frame，且 Frame 不许把它拒了（拒了 = 起不来）
            let f = Frame::new(scale_from_dpi(dpi), CELL, CELL).expect("每一档都该是合法缩放");
            assert_eq!(f.scale, scale);
        }

        // 反面对照：基准若写成 120，96 DPI 会读到 0.8 而不是 1.0 ⇒ 这行证明上面那张表
        // 真的在指"除以 96"，而不是"给一个正数就绿"。
        assert_ne!(scale_from_dpi(96), 96.0 / 120.0);

        // 0 ⇒ 1.00（不是 0.0，也不是 panic / Err）
        assert_eq!(scale_from_dpi(0), 1.0);
        assert!(Frame::new(scale_from_dpi(0), CELL, CELL).is_ok());
    }
}
