//! 窗口层：本 crate **唯一**点名窗口/合成那一族 OS 类型的文件（`lib.rs` 那条规矩）。
//!
//! 规矩的判据不是"文件数"而是**类别**：`HWND` / `UpdateLayeredWindow` / 消息循环 /
//! `SetCapture` / DPI 上下文全在这里，业务侧（`app.rs`）只经 [`Events`] 这一个 trait 与它们相连。
//! 缝的这一边全是 `i32` / `f32` / `&[u8]` ⇒ "命中判定读什么像素""捕获丢了算不算拖拽结束"
//! 这些问题在业务侧有名字、能被读、能在宿主上测；而 `GWLP_USERDATA` 那个裸指针的形状
//! 被封死在一个文件里，只能靠交叉编译证 —— 这正是本项目对"只能交叉编译证的"那一族的处置。
//!
//! ## 穿透有两层，我们两层都要
//!
//! - 第一层是**平台给的**：`ULW_ALPHA` 的 layered window，合成器自己做逐像素命中 ⇒
//!   alpha 为 0 的那一片根本不会给我们发消息。Wayland 那侧的等价物是 `wl_region`。
//! - 第二层才是 [`WM_NCHITTEST`]：它管的是"alpha **不为** 0、但低于我们那条阈值"的那一圈
//!   半透明边缘（羽化、抗锯齿）。平台那一层看不见它们（任何 alpha > 0 都算命中），
//!   所以由我们答 `HTTRANSPARENT`。
//!
//! ⇒ 缺一层的症状不同：少第一层 = 整个矩形吃掉点击；少第二层 = 宠物轮廓外多一圈
//! "点得着但看不见"。这也解释了 [`Events::hit`] 为什么用 `stage::ALPHA_HIT_THRESHOLD`
//! 而**不是** `alpha != 0`：它答的正是平台答不了的那一档，两者必须同一条阈值，
//! 否则"看得见没有"与"点得着没有"会是两个答案。
//!
//! ## 另外两件本平台独有的事
//!
//! 2. **没有"在飞 buffer"。** `UpdateLayeredWindow` 同步返回，没有回执也没有 frame callback
//!    ⇒ 省电五档里的延后那一档在本平台**结构上恒 0**（`classify_tick` 的 `frame_pending`
//!    恒传 `false`），摘要里必须带 inline 说明，不能让 `延后 0` 被读成"通过了一次延后测量"
//!    （理由见 `summary.rs` 模块头）。
//! 3. **DPI 是窗口属性，不是输出属性。** 缩放会在运行中被换掉（宠物被拖过屏边界），
//!    [`WM_DPICHANGED`] 是**必须处理**的消息而不是可选的：不处理 = 窗口停在旧物理尺寸而内容按
//!    新 scale 画 ⇒ 宠物被裁一角。
//!
//! ## 多面（M5 第二批 M3）：一进程 N 扇窗，一个泵、一个派发目标
//!
//! 注册表在 [`Client`]（`wins: Vec<Box<Window>>`），消息路由**不新增机制**：`drain` 照旧
//! NULL hwnd 全域抽，`DispatchMessageW` 按各自的 `GWLP_USERDATA` 落回 [`wnd_proc`]，
//! 窗口从自己的 [`SurfaceKind`] 知道"我是哪一面"。三件事随之多面化，每件都有硬理由：
//!
//! - **一泵一线**：`MsgWaitForMultipleObjectsEx` + 一个 waitable timer 保持唯一。事件环
//!   拆成两份的话，"每 tick 内核一步"就变成"每 tick 至多一步、看哪个泵先到" —— 确定性
//!   （硬约束 #8）第一个没。
//! - **共享 [`Slot`]**：`ev` 与 `quit` 从 `Window` 字段挪进 [`Client`] 独占的一块 `Box<Slot>`。
//!   这不是整理：`&'static mut dyn Events` 是线性借用，**同一个借用不可能存进两份**，
//!   多面共存时唯一合法的形状就是共享一块槽。
//! - **逐面命中策略**：[`Events::hit`] 收 `kind`。宠物面读宠物 buffer 的 alpha；菜单/卡/面板
//!   面读各自面的。透明处答 `HTTRANSPARENT` ⇒ 点击**落到桌面**而不是落到我们另一扇窗
//!   （Windows 的命中测试自上而下，穿透一层就出一层）。
//!
//! ⚠️ 已知偏离（登记给 §6.1.26，与"滞回未实现"同族）：`HTTRANSPARENT` 不投递任何事件 ⇒
//! Wayland 式"点外面关菜单"在本平台**没有免费实现**。面的关闭只有：菜单 CLOSE 行、右键宠物
//! toggle、卡面点掉/超时。

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, SIZE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP,
    HDC, HGDIOBJ, ReleaseDC, ScreenToClient, SelectObject, ValidateRect,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateWaitableTimerExW,
    CreateWaitableTimerW, SetWaitableTimer, TIMER_ALL_ACCESS,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetWindowLongPtrW, HTCLIENT, HTTRANSPARENT, IDC_ARROW, IsWindowVisible, LoadCursorW,
    MA_NOACTIVATE, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW,
    PostQuitMessage, QS_ALLINPUT, RegisterClassExW, SW_SHOWNA, SetProcessDPIAware,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
    WM_CAPTURECHANGED, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
    WS_POPUP,
};

use crate::layout::scale_from_dpi;
use crate::windx::{lparam_x, lparam_y, wide};

/// 窗口类名。
///
/// ⚠️ **纯 ASCII 且带项目前缀**，不是风格问题：Windows 的窗口类是**进程内全局**的一张表，
/// 撞了的形状不是报错而是"atom 已存在 ⇒ `RegisterClassExW` 返回旧 atom ⇒ 我们的窗口挂的是
/// **别人的** WNDPROC"，从此我们的消息一条都不认。那种症状要等这台机器上跑起另一家同名类的
/// 进程才现形 ⇒ 用名字挡掉比调试便宜。
///
/// M3 起**一个类服务所有面**：菜单/卡/面板挂同一个类、同一个 `wnd_proc`，分流靠窗口自己的
/// [`SurfaceKind`]（`GWLP_USERDATA` 已经能把我们带到它）。类名不改、不增 —— 改名没有任何收益，
/// 还把"注销旧类"引进来（那是 [`Client`] 的 `Drop` 里明写不去做的假开关）。
const CLASS: &str = "opet-pet";

/// 进程内**只许注册一次**窗口类的那道闸。
///
/// M3 之前 `create` 每进程只走一次，这道闸是隐式的；M3 起 `spawn` 让第二次 `create`
/// 成为常规路径 ⇒ 不设闸的话第二面直接撞 `ERROR_CLASS_ALREADY_EXISTS`
/// （Wine 尖峰实测 `GetLastError=0x582`，症状 = 整个进程起不来）。
/// 旗放在"注册成功之后"才置真：失败的那一趟不烧闸，下一次 `create` 会再试一次注册。
static CLASS_REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 定时线的自重复周期（毫秒）。
///
/// ⚠️ **这个数不是 tick 频率，也不许被当成 tick 频率。** 真正的步进由 `app.rs` 那条
/// "墙钟差 ÷ 1/60"的累积器算（与 Wayland 侧同一个式子、同一个内核常量）⇒ 16 ms = 62.5 Hz
/// 的唤醒比 60 Hz 略快，多出来的那些轮次累积器给 0 tick，行为一步都不多走。
///
/// 为什么不做成"每次醒来重设 due"：那会把唤醒时刻变成"上一轮派发了多久"的函数
/// = 新增一条抖动源，而 `lPeriod > 0` 这个周期性档存在的理由恰好就是躲开它。
const TIMER_MS: i32 = 16;

/// 第一次到点的延迟，100 ns 单位（= 16.6667 ms）。**负值 = 相对时间**（见 [`Clock::arm`]）。
const TIMER_DUE_100NS: i64 = -166_667;

/// 缩放是从哪条路拿到的。**必须打印**，不许静默回退（硬坑清单第 1 条：
/// 已登记的降级 ≠ 静默的降级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpiPath {
    /// `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` 成功 ⇒ 每窗 DPI + `WM_DPICHANGED`。
    PerMonitorV2,
    /// 上一步失败 ⇒ 退回进程级那一档。这一档**没有** `WM_DPICHANGED`：拖到另一块不同缩放的
    /// 屏上不会重算，宠物尺寸停在开机那一档。
    ProcessAware,
    /// 两条都失败：只能按 1.00 算。
    Assume100,
}

impl DpiPath {
    /// 摘要里那一格（`summary::Run::dpi_path` 收 `&str`，为的是**格式化**能在宿主上测）。
    pub fn as_str(self) -> &'static str {
        match self {
            DpiPath::PerMonitorV2 => "PER_MONITOR_AWARE_V2",
            DpiPath::ProcessAware => "SetProcessDPIAware 退路（拖屏不会重算缩放）",
            DpiPath::Assume100 => "两条路都失败 ⇒ 按 1.00 算",
        }
    }

    /// 这一档到底有没有 `WM_DPICHANGED`。启动行用它把"以后跨屏会不会重算"说在前面，
    /// 而不是等用户发现宠物停在旧尺寸上。
    pub fn tracks_per_monitor(self) -> bool {
        matches!(self, DpiPath::PerMonitorV2)
    }
}

/// 高精度唤醒源走了哪条路。与 DPI 同一条纪律：退路要说出来，而错误码要留着。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerPath {
    /// `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` 成功。
    HighResolution,
    /// 高精度档创建失败 ⇒ 退回普通 `CreateWaitableTimerW`，括号里是**那一次失败**的错误码。
    ///
    /// "系统没有这个 flag"（Win10 1607 之前）是这一档的**已知主因**，但不是唯一因
    /// （参数错、句柄表满也会给 null）⇒ 带码打出来，别把已知主因当本次结论。
    /// 这一档的实际唤醒粒度约 1–15 ms，行为不变（累积器补），但"这一轮 tick 为什么这么抖"
    /// 从此有了第二个原因，所以必须上启动行。
    Fallback(u32),
}

impl TimerPath {
    pub fn as_str(self) -> &'static str {
        match self {
            TimerPath::HighResolution => "HIGH_RESOLUTION",
            TimerPath::Fallback(_) => "普通 timer（高精度档不可用 ⇒ 唤醒粒度粗）",
        }
    }

    /// 退路那一档的错误码；成功路径给 `None`。
    pub fn error_code(self) -> Option<u32> {
        match self {
            TimerPath::Fallback(e) => Some(e),
            TimerPath::HighResolution => None,
        }
    }
}

/// 一面窗口的身份：注册表（[`Client`] 的 `wins`）与 [`Events`] 的每一次回调都靠它分流。
///
/// ⚠️ 每一面**至多一扇**（`spawn` 拒重）：菜单开两次不是"叠两层菜单"，是两个事实源在回答
/// 同一个 `hit(Menu,…)` —— 那正是本项目对"同一件事两份"付过学费的形状。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    /// 宠物本体。第一批的唯一一面；拖拽、双击、`WM_DESTROY` 落全局退出旗都只由它。
    Pet,
    /// 右键菜单（第三批前不实现"点外面关闭"，见模块头那条偏离）。
    Menu,
    /// 抽卡结果卡。
    Card,
    /// 背包 / 收藏 / 设置面板。
    Panel,
}

impl SurfaceKind {
    /// 窗口标题。**纯 ASCII 且带项目面名**，与 `CLASS` 同一条取向，但这里的读者不同：
    /// Wine/真机上用窗口列表（`xdotool search` / Alt-Tab 之外的枚举工具）核对"到底映射了几扇、
    /// 哪扇是哪面"时，标题是**唯一**当场可读的身份 —— 四扇全叫 `OPet` 时两窗尖峰就没法判卷。
    pub const fn title(self) -> &'static str {
        match self {
            Self::Pet => "OPet",
            Self::Menu => "OPet-menu",
            Self::Card => "OPet-card",
            Self::Panel => "OPet-panel",
        }
    }

    /// 只有本体面的销毁会让整个进程收尾（[`Client`] 的 `Drop` 负责其余各面）。
    pub const fn is_pet(self) -> bool {
        matches!(self, Self::Pet)
    }
}

/// 一个进程**共享**的派发槽：`Box` 在 [`Client`] 手里，各扇 [`Window`] 只存它的裸地址。
///
/// `ev` 字段的两大理由原样继承第一批（记在一个 `isize` 的格子里装不下 16 B 的 fat pointer ⇒
/// 存"指向稳定堆位置的指针"；`Option` 让六处派发点拿到的要么没有、要么是一个合法借用，
/// 没有第三种"判过但判错"）。**为什么现在从 `Window` 字段挪到这里**：`&'static mut dyn Events`
/// 是线性借用，同一个借用不可能同时存进两个 `Window` ⇒ 多面共存时共享一块槽不是整理，
/// 是唯一编译得过的形状。派发目标本来就只有一个（`App`）—— 两面各借一份才是假的。
///
/// 为什么那个借用是 `&'static mut` 而不是普通 `&mut`：这一格是本文件里**唯一由编译器管的
/// 寿命**，而它管的恰好是最容易写错的一条 —— 派发期间的借用不许活得比 `pump` 长。
/// 另一版写法是被实测否证掉的：字段存 `*mut dyn Events`、参数收 `&mut dyn Events` 时，
/// `&raw mut *ev` 与 `ev as *mut dyn Events` **两支都报** `lifetime may not live long
/// enough`（裸胖指针的 trait object 界默认 `'static`，而 rustc 现在明确拒绝用裸指针转换
/// 延长寿命：rust-lang/rust#141402）⇒ 想在这一格把关，唯一不洗白寿命的写法是调用方真的
/// 给一个 `'static`。代价落在 `app.rs`：**业务对象必须 `Box::leak`**，与 Wayland 侧
/// "`App` 必须是 `'static`（`QueueHandle<App>` 的要求）"（`overlay-wayland/src/app.rs:1562`）
/// 是同一个形状的同一条处置 ⇒ 一个进程级、活到退出的对象，漏一份不是债。
struct Slot {
    /// **平时是 `None`**，只在 [`Client::pump`] 的调用期间是 `Some`（有效期 = 该函数文档）。
    ev: Option<&'static mut dyn Events>,
    /// 全局退出旗：`--seconds` 到点、本体面 `WM_DESTROY`、外部 `WM_QUIT` 三条路都落这一格。
    quit: bool,
}

impl Slot {
    fn new() -> Self {
        Slot {
            ev: None,
            quit: false,
        }
    }
}

/// 窗口层向业务层要的全部能力，也是这一层与业务层之间**唯一**的缝。
///
/// 坐标一律是**那一面窗口内物理像素**（不是逻辑、不是屏幕；`WM_NCHITTEST` 的屏幕坐标在
/// 回调里已经 `ScreenToClient` 过）。理由见 `lib.rs` 坐标系那一格：命中读的就是刚交给
/// `UpdateLayeredWindow` 那块缓冲的物理字节，而拖拽要的"物理 → 逻辑"换算在 `app.rs` 做
/// （`Frame::stage_to_local`）⇒ 这一层不做任何缩放猜测。
///
/// M3 起每个回调带 [`SurfaceKind`]：派发目标只有一个，但"哪一扇窗上的哪一点"必须能问出来，
/// 否则第二面接进来的那一天就是一天重写。
pub trait Events {
    /// 这一面的这一点有没有"我们自己的像素"（决定 `HTCLIENT` vs `HTTRANSPARENT`）。见模块头：
    /// 它答的是平台答不了的那一档，所以阈值与 `Stage::hit` 必须同一条。
    fn hit(&self, kind: SurfaceKind, x: i32, y: i32) -> bool;
    /// 左键按下。
    ///
    /// ⚠️ [`WM_LBUTTONDBLCLK`] 也送到这里、**且只送这一次**（不另给双击回调）：
    /// 双击判定在 `app.rs` 用与 Wayland 同一条 320 ms / 24 px 规则自己做。
    /// 为什么不用系统那份：系统的"双击"读的是**它自己那两格**用户设置（时限 + 距离），
    /// 而行为内核的 `Command::DoubleTap` 是确定性的一部分 ⇒ 同一份种子在两平台必须折叠出
    /// 同一条 `Command` 序列。拿系统判定 = 引入一条"用户的面板设置能改内核输入"的边，
    /// 而那条边在"同种子逐位重放"这条硬约束上**不存在**，不是"暂时没测到"。
    fn press(&mut self, kind: SurfaceKind, x: i32, y: i32);
    /// 右键按下（M5 起；第二批的"右键 = 开/关菜单"走的正是这一格）。
    ///
    /// 为什么不并进 [`Events::press`] 加一个按钮参数：左键那一格背着**双击折叠**与
    /// **拖拽捕获**两条语义（见其文档），右键一条都不参与 —— 合进同一个入口就得在业务侧
    /// 处处 `match button`，而"漏判一个按钮"的症状是把拖拽逻辑套到菜单开关上。
    /// 两条消息在平台层面本来就是两个码（`WM_LBUTTON*` / `WM_RBUTTON*`），缝也跟着分。
    ///
    /// 默认实现 = **不理右键**。这是除本体面外每一面的正解（菜单面自己只认左键，
    /// 与 Wayland 侧 `menu_press_activates` 左键独占同一条）；本体面覆盖它做 toggle。
    /// 坐标**不进这一格**：它唯一的读取者（宠物面的开关菜单）不读点在哪 —— 递一个没人读的
    /// 参数就是假开关换成了假参数。
    fn right_press(&mut self, _kind: SurfaceKind) {}
    fn release(&mut self, kind: SurfaceKind, x: i32, y: i32);
    fn motion(&mut self, kind: SurfaceKind, x: i32, y: i32);
    /// 捕获被收回 —— Wayland 那条 P0（锁屏 / 切 VT / 别人 grab ⇒ 收不到 `Release` ⇒
    /// 内核永久卡在 `Dragged`）在本平台的同形事件，处置也一样：**当场按抬起收尾**。
    ///
    /// 这一档**不是**可选加固：`SetCapture` 之后系统随时能基于安全原因收回捕获
    /// （Ctrl-Alt-Del 是文档写明的一条路）。
    ///
    /// ⚠️ **正常拖完也会走到这里一次**：`ReleaseCapture()` 自己就发
    /// [`WM_CAPTURECHANGED`]。所以业务侧的 `capture_lost` 必须是**幂等**的（"不在拖拽中"
    /// 就当没事发生），实现里靠 [`Window::captured`] 那面旗把"我们自己放的"挡在转达之前 ——
    /// 两面都要，缺一面的症状不同：少旗 = 每次拖完多一次"提前结束"（宠物松手那刻抖一下）；
    /// 少幂等 = 那一多出来的事件会真的写进内核。
    fn capture_lost(&mut self, kind: SurfaceKind);
    /// 缩放换了（只有 [`DpiPath::PerMonitorV2`] 那一档会走到这里）。
    /// 业务侧要按新 scale 重算那一面的 `Frame` 尺寸、请求一次 [`Client::resize`] 并重画。
    fn dpi_changed(&mut self, kind: SurfaceKind, scale: f32);
}

/// 一次唤醒的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// 定时线到点。
    Timer,
    /// 消息先到（用户操作、或系统给我们的窗口发了什么）。
    Message,
    /// 等的这段时间到点而两样都没来。
    Timeout,
    /// 等待函数自己失败 ⇒ 必须报出来，**不许**塌进 `Timeout` 继续睡。
    Failed(u32),
}

/// 一层窗口 + 它的 DIB + 捕获状态。M3 起 `kind` 与共享的 [`Slot`] 也是它的一部分：
/// 窗口自己说得出"我是哪一面"，而派发目标全进程只有一个。
pub struct Window {
    hwnd: HWND,
    /// 我是哪一面。`wnd_proc` 里所有分流读这一格（它是 `Copy` 的枚举，没有漂移空间）。
    kind: SurfaceKind,
    /// 共享派发槽的裸地址（[`Client`] 用 `Box` 独占那块内存）。
    ///
    /// ⚠️ 有效期纪律与 [`Window::install_userdata`] 逐字同条：`Slot` 进堆一次、永不移动、
    /// 最后一个消费者（窗口回调）先被 [`Client`] 拆干。**这一格不另设判空**：`Client` 之外
    /// 没有构造 `Window` 的路径，而 `Client::drop` 的次序保证任何能被回调看到的 `Window`
    /// 都带着活槽（判空设在 `ev: Option` 那一层，见 [`events`]）。
    slot: *mut Slot,
    /// 与屏幕兼容的内存 DC，DIB 挂在它上面。
    mem_dc: HDC,
    dib: HBITMAP,
    /// 选进 `mem_dc` 时换下来的那一张（新建 DC 自带的占位位图）。
    /// ⚠️ 位图**还选在 DC 上时 `DeleteObject` 会失败**而失败没有可见症状（只返回 0）
    /// ⇒ 换位图必须先把它选回去。漏这一步的形状是"每次换 DPI 漏一张位图"，而位图吃的是
    /// GDI 对象池（进程默认上限 10,000）⇒ 耗尽时崩在**别人的** GDI 调用里。
    old_bmp: HGDIOBJ,
    /// DIB 的像素基址。⚠️ 有效期归 `dib` ⇒ [`Window::resize`] 之后必须重取（那里就重取了）。
    bits: *mut c_void,
    w: i32,
    h: i32,
    scale: f32,
    dpi_path: DpiPath,
    /// 现在是不是我们握着鼠标捕获。见 [`Events::capture_lost`] 那条警告。
    /// **每扇窗自己的**：捕获本来就归 hwnd，多面共存时这一点没有共享的余地。
    captured: bool,
}

unsafe impl Send for Window {}

impl Window {
    /// 注册类 + 建窗 + 造第一张 DIB。
    ///
    /// 样式那几项，每一项"少了它"是一个不同的症状：
    /// - [`WS_EX_LAYERED`] —— 没有它 `UpdateLayeredWindow` 直接返回失败，而失败码说不清为什么。
    /// - [`WS_EX_TOPMOST`] —— 宠物要在所有窗口之上。
    /// - [`WS_EX_NOACTIVATE`] —— 点宠物不许抢走用户正在打字的那个窗口的焦点。
    ///   ⚠️ 它与下面 `WM_MOUSEACTIVATE → MA_NOACTIVATE` 是**两条不同的路**：样式管"能不能被
    ///   激活"，消息管"这一次点击要不要吃掉激活"。只给样式在部分 Windows 版本上仍会激活
    ///   （文档明写），所以两条都上。少了它们的症状 = "点一下宠物，用户的编辑器丢了焦点"，
    ///   在本项目算事故而不是瑕疵。
    /// - [`WS_POPUP`] —— 无边框。有边框 = 宠物头顶多一条标题栏。
    /// - **不给** `WS_EX_TOOLWINDOW` 是有意的：给了它 alt-tab 里看不到我们，而 ToolWindow 在
    ///   部分第三方窗口管理器上会被跳过层管理 ⇒ 与 `WS_EX_TOPMOST` 打架。本片按"稳定浮在最上"
    ///   取舍，并把这一格登记成**未验**（不是"读到了但没用"，是"没这条路径"）。
    ///
    /// **不给** `CS_HREDRAW | CS_VREDRAW` 同样是有意的：那两个标志让系统在尺寸变化时发
    /// `WM_PAINT` 并要求整窗重绘，而我们的"重绘"是 `UpdateLayeredWindow`，它**不是**
    /// `WM_PAINT` 的响应。留着只会多一轮没人应答的 `WM_PAINT`（默认处理是 `ValidateRect`
    /// ⇒ 一次空提交，而"空提交"在省电计数上是要算一笔的）。
    /// `CS_OWNDC` 要的恰好相反：一张**长期存在**的 DC，`resize` 换位图时不用重建 DC。
    /// 注册类 + 建窗 + 造第一张 DIB。**只能由 [`Client`] 调用**（`open` 的第一扇与 `spawn`）：
    /// 它要的 `slot` 裸地址必须指向一块 Client 永久拥有的 `Box<Slot>`（见 [`Window::slot`]）。
    /// 可见性本身就是这条纪律的写法 —— `Slot` 是私有类型，构造器不能比它更公开。
    fn create(
        kind: SurfaceKind,
        at: (i32, i32),
        size: (i32, i32),
        dpi_path: DpiPath,
        slot: *mut Slot,
    ) -> Result<Self, String> {
        let (w, h) = size;
        if w <= 0 || h <= 0 {
            return Err(format!(
                "窗口尺寸 {w}x{h} 不合法 ⇒ DIB 造不出来（负高度是 top-down 的记号，不是尺寸本身）"
            ));
        }

        let class_w = wide(CLASS);
        let title_w = wide(kind.title());
        let hinst = unsafe { GetModuleHandleW(core::ptr::null()) };
        if !CLASS_REGISTERED.load(std::sync::atomic::Ordering::Relaxed) {
            // 类光标 = 箭头。⚠️ **一处已登记的偏离**：Wayland 侧拖拽时会换抓取手光标，本平台没有
            // （本片验收面上只有"拖得动/拖不动"，光标形状不在上面）。换抓取手要 `WM_SETCURSOR`
            // + `IDC_SIZEALL`，属下一轮，不是这一轮的遗漏。
            let hcur = unsafe { LoadCursorW(core::ptr::null_mut(), IDC_ARROW) };
            if hcur.is_null() {
                return Err(format!(
                    "LoadCursorW(IDC_ARROW) 失败（GetLastError={:#X}）⇒ 指针移到窗口上会隐形",
                    unsafe { GetLastError() }
                ));
            }
            let wc = WNDCLASSEXW {
                cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst,
                hCursor: hcur,
                lpszClassName: class_w.as_ptr(),
                ..Default::default()
            };
            if unsafe { RegisterClassExW(&wc) } == 0 {
                return Err(format!(
                    "RegisterClassExW({CLASS:?}) 失败（GetLastError={:#X}）⇒ 通常是类名撞了已注册的类",
                    unsafe { GetLastError() }
                ));
            }
            CLASS_REGISTERED.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class_w.as_ptr(),
                title_w.as_ptr(),
                WS_POPUP,
                at.0,
                at.1,
                w,
                h,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                hinst,
                core::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(format!(
                "CreateWindowExW {w}x{h} @ {},{} 失败（GetLastError={:#X}）",
                at.0,
                at.1,
                unsafe { GetLastError() }
            ));
        }

        // 屏幕 DC 只借来问"要兼容什么"，拿到内存 DC 就还掉。
        // ⚠️ `ReleaseDC` 必须跟上：屏幕 DC 是共享资源，漏一次就少一个别人能用的槽。
        let screen = unsafe { GetDC(core::ptr::null_mut()) };
        let mem_dc = unsafe { CreateCompatibleDC(screen) };
        unsafe { ReleaseDC(core::ptr::null_mut(), screen) };
        if mem_dc.is_null() {
            unsafe { DestroyWindow(hwnd) };
            return Err(format!(
                "CreateCompatibleDC(屏幕) 失败（GetLastError={:#X}）⇒ 没有内存 DC 就造不出 DIB",
                unsafe { GetLastError() }
            ));
        }

        let mut win = Window {
            hwnd,
            kind,
            slot,
            mem_dc,
            dib: core::ptr::null_mut(),
            old_bmp: core::ptr::null_mut(),
            bits: core::ptr::null_mut(),
            w: 0,
            h: 0,
            scale: 1.0,
            dpi_path,
            captured: false,
        };
        // 建窗**之后**才问得到"这个窗口"的 DPI —— 那正是 PER_MONITOR_AWARE_V2 那一档的整个卖点。
        // ⚠️ 顺序不能反（`GetDpiForWindow` 对不存在的窗口返回 0），而这里是唯一一个
        // "窗口已经在了、业务还没开始"的时刻。
        win.scale = win.read_scale();
        win.make_dib(w, h)?;
        // 文档说分层窗口"在第一次 `UpdateLayeredWindow` 之后才变可见"（Layered Windows 一节），
        // 所以**按文档这里不需要** `ShowWindow`。仍然显式 show 一次，两条理由：
        // ① 那条"ULW 自带可见"的行为在真机上从没被看过一眼，而它错了的症状是"屏上什么都没有"
        //    —— 最贵的失败形状（进程活着、计数在涨、宠物不存在）。这一行把它换成一条会被读到的日志。
        // ② 2026-11-22 实测：Wine 里不 show 就**根本不映射窗口**（补上之后 `hyprctl clients`
        //    立刻出现 `OPet 128x128`），本机那台冒烟仪器全靠这一行才存在。
        // `SW_SHOWNA` 而不是 `SW_SHOW`：显示但**不激活**，与 `WS_EX_NOACTIVATE` 同一条取向
        // （硬约束 3 的邻域：点宠物不许抢走用户正在打字的窗口）。
        unsafe { ShowWindow(hwnd, SW_SHOWNA) };
        if unsafe { IsWindowVisible(hwnd) } == 0 {
            unsafe { DestroyWindow(hwnd) };
            return Err(
                "ShowWindow(SW_SHOWNA) 之后 IsWindowVisible 仍为 0 ⇒ 窗口映射不上，屏上不会有任何东西"
                    .into(),
            );
        }
        Ok(win)
    }

    /// 这个窗口自己的缩放。
    ///
    /// `GetDpiForWindow` 在 [`DpiPath::ProcessAware`] / [`DpiPath::Assume100`] 两档上**照样返回
    /// 一个数**（进程级 DPI），那不是我们猜的 1.0 ⇒ 三档都读，只有函数返回 0（= 失败）才落回
    /// 1.0 并把这句话打到 stderr：静默按 100% 算的形状，与"这台机器就是 100%"逐字相同。
    fn read_scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            eprintln!(
                "dpi: GetDpiForWindow 返回 0（GetLastError={:#X}）⇒ 按 1.00 算，\
                 这一轮的宠物尺寸可能与屏幕实际缩放不符",
                unsafe { GetLastError() }
            );
            return 1.0;
        }
        scale_from_dpi(dpi)
    }

    /// 造（或换）一张 top-down 32bpp DIB，并把像素基址记下来。
    fn make_dib(&mut self, w: i32, h: i32) -> Result<(), String> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // ⚠️ **负高度 = top-down**（行 0 落在缓冲区顶部），这是本函数唯一真正要紧的一行。
                // 给正值 = bottom-up，那么 `Stage` 里"行 0 在 y=0"那条约定就会上下颠倒：
                // 宠物**翻过来**、而**所有门禁全绿**（字节数、尺寸、alpha 阈值、命中率全不变）。
                // Wayland 侧同一条约定由 shm 布局免费给；这里必须显式写。
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default(); 1],
        };
        let mut bits: *mut c_void = core::ptr::null_mut();
        let dib = unsafe {
            CreateDIBSection(
                self.mem_dc,
                &raw const bmi,
                DIB_RGB_COLORS,
                &raw mut bits,
                core::ptr::null_mut(),
                0,
            )
        };
        if dib.is_null() || bits.is_null() {
            return Err(format!(
                "CreateDIBSection {w}x{h} 失败（GetLastError={:#X}，dib={dib:?}，bits={bits:?}）⇒ \
                 没有可写的像素缓冲",
                unsafe { GetLastError() }
            ));
        }

        // 旧位图先选回 DC，再删新位图 —— 顺序反了 `DeleteObject` 会失败（见字段注释）。
        if !self.dib.is_null() {
            unsafe {
                SelectObject(self.mem_dc, self.old_bmp);
                DeleteObject(self.dib);
            }
        }
        let old = unsafe { SelectObject(self.mem_dc, dib) };

        self.dib = dib;
        self.old_bmp = old;
        self.bits = bits;
        self.w = w;
        self.h = h;
        Ok(())
    }

    /// 把 `src`（预乘 BGRA8）拷进 DIB，再一次 `UpdateLayeredWindow` 上屏。
    ///
    /// `src.len()` 必须**恰好**等于 `w × h × 4`：不等就报错而不是截断，因为截断的后果是
    /// "窗口右下角保持上一帧"，而那种残影在验收时会被读成平台病（Wayland 侧 `atlas.rs`
    /// 那条 buffer 越界检查是同一条纪律）。
    ///
    /// **不做任何缩放**：`srcRect` 从 (0,0) 起、`psize` = 整个 DIB，与目标 1:1 是有意的。
    /// 放大由 `Atlas::blit` 的最近邻完成，不交给 `UpdateLayeredWindow` —— 两条理由：
    /// (a) 两平台必须出同一张图，让系统插值就等于把像素交给一个我们读不到也测不到的实现；
    /// (b) ULW 的双线性在 alpha 边缘上会造出我们那条"黑描边"门禁抓不到的半透明晕，
    /// 而那一圈晕恰好是模块头"穿透第二层"的输入 ⇒ 它不只是好看问题。
    pub fn present(&mut self, at: (i32, i32), src: &[u8]) -> Result<(), String> {
        let need = (self.w as usize)
            .checked_mul(self.h as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| format!("present 拒绝：{}x{} 的缓冲字节数溢出", self.w, self.h))?;
        if src.len() != need {
            return Err(format!(
                "present 拒绝：src={} B 而窗口缓冲要 {need} B（{}x{}）⇒ 不拷不完整的像素",
                src.len(),
                self.w,
                self.h
            ));
        }
        if self.bits.is_null() {
            return Err("present 拒绝：没有 DIB 基址 ⇒ 无处可拷".into());
        }
        // SAFETY: `need` 上面逐条核过（两侧字节数相等、乘法不溢出）；`bits` 由 `make_dib` 给、
        // 在 `dib` 存活期内有效；两个区域不同源（`src` 是 `Stage` 的 `Vec`，DIB 是系统给的映射）。
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), self.bits as *mut u8, need) };

        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            // `SourceConstantAlpha = 255` 让**逐像素** alpha 全权生效。这一格给小于 255 是
            // "整只宠物变半透明"那条完全不同的路（全局淡出），本片没有那个需求 ⇒ 写死并说明，
            // 不留成可配置的数（`--cell` 那一族假开关）。
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let dst = POINT { x: at.0, y: at.1 };
        let size = SIZE {
            cx: self.w,
            cy: self.h,
        };
        let src_pt = POINT { x: 0, y: 0 };
        let ok = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                core::ptr::null_mut(),
                &raw const dst,
                &raw const size,
                self.mem_dc,
                &raw const src_pt,
                0,
                &raw const blend,
                ULW_ALPHA,
            )
        };
        if ok == 0 {
            return Err(format!(
                "UpdateLayeredWindow(ULW_ALPHA) 失败（GetLastError={:#X}）⇒ 屏上停在上一帧",
                unsafe { GetLastError() }
            ));
        }
        Ok(())
    }

    /// 只移动、不改像素。`hdcSrc = NULL` 时文档给了**四条**配套约束，缺一条件的失败码是
    /// `ERROR_NOT_SUPPORTED`（0x1F）而不是任何带解释的码：
    /// `psize` 必须空、`hdcDst` 必须空、`pblend` 应当空、**`dwFlags` 应当是 0**。
    ///
    /// ⚠️ 最后那条原本漏了（注释里当时只数到前三个），2026-11-22 Win11 真机第一轮
    /// `--test-rect` 当场踩中：这一支传 `ULW_ALPHA` ⇒ `UpdateLayeredWindow` 直接失败。
    /// 影响面不止矩形档 —— 本体档每个 `Tick::Move` 都走这里，失败后由 `app.rs` 的
    /// `force_draw` 兜底改成"每次移动都重发整块 buffer"，于是**省电五档与 CPU 门禁在那一轮
    /// 全都不可信**（屏上看得到宠物，所以不会有人怀疑）。⇒ `dwFlags` 这一格不是可选整洁。
    ///
    /// 为什么不用 `SetWindowPos` 做仅移动：那条会让**底下的窗口重绘**，而 ULW 的文档明写
    /// 不会（"the windows underneath a layered window do not need to be repainted"）。宠物是
    /// 每 tick 都在动的常驻层，拿重绘换一次调用参数变简单，是拿别人的帧预算付自己的账。
    ///
    /// 对照 Wayland：那边仅移动 = margin 变化 + 一次**不带 buffer**的 commit。
    /// ⚠️ 失败被吞掉的后果 = 宠物停在原地而计数照加，那正是 Wayland 侧 `buffer_errors`
    /// 那一档要防的形状 ⇒ 这里返回 `Result`，由调用方计入 `Counters::buffer_errors`。
    pub fn move_only(&mut self, at: (i32, i32)) -> Result<(), String> {
        let dst = POINT { x: at.0, y: at.1 };
        let ok = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                core::ptr::null_mut(),
                &raw const dst,
                core::ptr::null(),
                core::ptr::null_mut(),
                core::ptr::null(),
                0,
                core::ptr::null(),
                // `hdcSrc` 为空 ⇒ 这一格必须是 0（文档写明 "If hdcSrc is NULL, dwFlags should
                // be zero"）。给 `ULW_ALPHA` 的失败是 0x1F，见上面那条警告。
                0,
            )
        };
        if ok == 0 {
            return Err(format!(
                "UpdateLayeredWindow(仅移动) 失败（GetLastError={:#X}）",
                unsafe { GetLastError() }
            ));
        }
        Ok(())
    }

    /// 换物理尺寸 = 换 DIB。**尺寸没变就一个字都不做**：`WM_DPICHANGED` 会连发多条，
    /// 而换一次 DIB 的代价是"整帧重画 + 一次 GDI 对象池进出"。
    ///
    /// ⚠️ 换完必须紧跟一次 [`Self::present`]：`resize` 只换缓冲，不碰屏上那张。
    pub fn resize(&mut self, size: (i32, i32)) -> Result<(), String> {
        let (w, h) = size;
        if (w, h) == (self.w, self.h) {
            return Ok(());
        }
        self.make_dib(w, h)
    }

    pub fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn dpi_path(&self) -> DpiPath {
        self.dpi_path
    }

    pub fn kind(&self) -> SurfaceKind {
        self.kind
    }

    /// 把 `self` 的地址交给窗口回调。**只能由 [`Client::open`] / [`Client::spawn`] 调用，
    /// 且每扇一次**。
    ///
    /// ⚠️ 前置条件 = 调用方把 `Window` 装进一个**稳定的堆位置**并且永不移动它
    /// （本 crate 的现行做法 = `Box<Window>`，由 [`Client`] 独占拥有）。这不是讲究：
    /// `GWLP_USERDATA` 存的是一个裸 `isize`，而 `Window` 一旦被移动（移出 `Box`、
    /// `*self.win = other`），窗口回调拿到的就是野指针 —— 而那不会立刻炸，
    /// 它会先写坏别人的内存。
    fn install_userdata(&mut self) {
        unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, &raw mut *self as isize) };
    }

    /// 见 [`Client`] 的 `Drop`：清零必须是拆窗口第一步。
    fn clear_userdata(&mut self) {
        unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
    }
}

impl Drop for Window {
    /// 顺序：位图选回 DC → 删位图 → 删 DC。
    ///
    /// ⚠️ 窗口句柄**不在这里**删（归 [`Client`]）：`DestroyWindow` 会同步派发
    /// `WM_DESTROY`，而那时窗口回调读的就是 `GWLP_USERDATA` 里那个指针。把它拆成两个 `Drop`
    /// 不是讲究，是让"清零"与"删窗"的先后**没有写反的余地**——反了的症状不是崩在这一行，
    /// 是崩在一个正在析构的 `Window` 上（use-after-free 的教科书形状）。
    fn drop(&mut self) {
        if !self.dib.is_null() {
            unsafe {
                SelectObject(self.mem_dc, self.old_bmp);
                DeleteObject(self.dib);
            }
            self.dib = core::ptr::null_mut();
        }
        if !self.mem_dc.is_null() {
            unsafe { DeleteDC(self.mem_dc) };
            self.mem_dc = core::ptr::null_mut();
        }
    }
}

/// 60 Hz 唤醒源：一个周期性的高精度 waitable timer。
///
/// 为什么不用 `SetTimer`：那条路的分辨率 = 系统定时器精度（默认 10–16 ms 且**受别人影响** ——
/// 桌面上任何一个进程调 `timeBeginPeriod` 都会改掉我们的），而硬约束 #8 要的是
/// "同种子逐位重放" ⇒ 唤醒时刻不许被第三个进程左右。
///
/// 为什么不用 `GetMessageW` 阻塞循环：那条只能等消息、等不了句柄，而 `--seconds` 到点
/// 必须自己走出来（不能靠有人来点关闭）。正解 = `MsgWaitForMultipleObjectsEx`，
/// 句柄与消息**一起等**。
pub struct Clock {
    timer: HANDLE,
    path: TimerPath,
}

impl Clock {
    /// 造 + 上弦。
    ///
    /// `duetime` = [`TIMER_DUE_100NS`]：**负值 = 相对时间**（正值是绝对 UTC 100 ns 计数，
    /// 给 `0` 的意思是"立刻"）⇒ 相对量是唯一不会因时钟调整而漂移的写法。
    /// `lPeriod = TIMER_MS > 0` = **周期性**，之后不再需要重设 due。
    ///
    /// ⚠️ 失败必须报出来，**不许**退化成"没有定时线、靠消息驱动"：后者的症状是宠物只在有人
    /// 动鼠标时动，而那看起来像"省电生效了"。
    pub fn arm() -> Result<Self, String> {
        let hi = unsafe {
            CreateWaitableTimerExW(
                core::ptr::null(),
                core::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        let (timer, path) = if hi.is_null() {
            let e = unsafe { GetLastError() };
            // 退路：不带 flag 的那一个函数（它同样被 `Win32_Security` 门控，
            // 因为第一个参数是 `*const SECURITY_ATTRIBUTES`）。
            let plain = unsafe { CreateWaitableTimerW(core::ptr::null(), 0, core::ptr::null()) };
            if plain.is_null() {
                return Err(format!(
                    "两条定时器路都失败（HIGH_RESOLUTION→{e:#X}，普通档→{:#X}）⇒ 宠物不会有第一拍",
                    unsafe { GetLastError() }
                ));
            }
            (plain, TimerPath::Fallback(e))
        } else {
            (hi, TimerPath::HighResolution)
        };
        let due = TIMER_DUE_100NS;
        // 第 4 个参数 = 完成回调（`PTIMERAPCROUTINE`），给 `None` = 到点只让等待返回，
        // 不在我们的线程上跑 APC。这是有意的：回调会把"到点"塞进一个我们无法排序的时刻，
        // 而 `[Client::pump`] 要的正是"到点由我这一侧发现"。
        let ok = unsafe {
            SetWaitableTimer(timer, &raw const due, TIMER_MS, None, core::ptr::null(), 0)
        };
        if ok == 0 {
            unsafe { CloseHandle(timer) };
            return Err(format!(
                "SetWaitableTimer 失败（GetLastError={:#X}）⇒ 有句柄而永不到点，宠物不会动",
                unsafe { GetLastError() }
            ));
        }
        Ok(Clock { timer, path })
    }

    pub fn path(&self) -> TimerPath {
        self.path
    }

    /// 等一段时间，其间**不**派发消息（派发归 [`Client::pump`] 那一半）。
    ///
    /// `MWMO_INPUTAVAILABLE` 那一格必须有：不加它时，一条已经被内部队列拿走、但还没被我们
    /// `PeekMessageW` 抽到的移动消息会让等待睡满整个 `wait_ms` ⇒ 症状是"指针卡顿 16 ms"
    /// 而队列里明明有活。
    fn wait(&self, wait_ms: u32) -> Wake {
        // `nCount = 1` ⇒ 消息那一档固定是 `WAIT_OBJECT_0 + 1`。**写成 const 而不是一行
        // `WAIT_OBJECT_0 + 1`**：match 的模式位只收路径与字面量，`const` 项是路径、
        // 而加法是表达式（编译器在这里报的是 "expected a pattern, found an expression"，
        // 不是值的问题）。
        const WAIT_MSG: u32 = WAIT_OBJECT_0 + 1;
        let r = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                &raw const self.timer,
                wait_ms,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        match r {
            WAIT_OBJECT_0 => Wake::Timer,
            WAIT_MSG => Wake::Message,
            WAIT_TIMEOUT => Wake::Timeout,
            // ⚠️ `WAIT_FAILED` **不许**塌进 `Timeout`：那条路会把"句柄已经被关掉"读成
            // "这一轮没人来活"，然后无限睡下去。
            WAIT_FAILED => Wake::Failed(unsafe { GetLastError() }),
            other => Wake::Failed(other),
        }
    }
}

impl Drop for Clock {
    /// 先 `CancelWaitableTimer` 再 `CloseHandle`。
    ///
    /// 登记一句实话：**今天这一行是冗余的** —— 最后一个句柄关闭时内核对象随之销毁，
    /// 而我们没有完成回调（`SetWaitableTimer` 第 4 参数给的是 `None`），所以待决到点无处可去。
    /// 留着它是因为另一条还没来的改动：有人加上完成回调之后，"关句柄"就不再等于"不再被调用"。
    fn drop(&mut self) {
        if !self.timer.is_null() {
            unsafe {
                CancelWaitableTimer(self.timer);
                CloseHandle(self.timer);
            }
            self.timer = core::ptr::null_mut();
        }
    }
}

/// 注册表 + 共享派发槽 + 定时线的组合体：这一层的唯一公共入口，也是 [`Window`] 与
/// [`Slot`] **唯一的拥有者**。
///
/// 两处 `Box` 都是纪律不是方便：`GWLP_USERDATA` 存 `*mut Window`、各窗 `Window::slot` 存
/// `*mut Slot`，而 `Vec` 增长/`Client` 本身被移动都只搬指针不搬内容 ⇒
/// [`Window::install_userdata`] 的前置条件（稳定堆位置）自动成立。`Client` 在 `App` 里
/// 那种写法仍然被禁止（`app.rs` 头部的 UB 边界说明，M3 后只把 `Window::ev` 换成 `Slot::ev`）。
pub struct Client {
    // clippy 的 `vec_box` 在这里是**误报**：那层 `Box` 正是上面文档说的纪律
    // （`GWLP_USERDATA` 存 `*mut Window`，Vec 增长只搬指针不搬内容）。
    #[allow(clippy::vec_box)]
    wins: Vec<Box<Window>>,
    slot: Box<Slot>,
    clock: Clock,
}

impl Client {
    /// 一次走完三件**有先后关系**的事：DPI 感知 → 建本体窗 → 上弦。
    ///
    /// ⚠️ DPI 那一步必须在**注册窗口类之前**（`SetProcessDpiAwarenessContext` 一旦本进程有窗口
    /// 存在就返回失败，而它的失败**不带任何症状**，直到有人在 150% 屏上看到一只四分之一大的
    /// 宠物）。所以这里**不**把 [`set_dpi_awareness`] 留给调用方"记得先调一下"——
    /// 那种前置条件迟早会被忘；写成第一步就是让顺序由构造保证，而不是由纪律保证。
    pub fn open(at: (i32, i32), size: (i32, i32)) -> Result<Self, String> {
        let dpi_path = set_dpi_awareness();
        let mut slot = Box::new(Slot::new());
        let mut win = Box::new(Window::create(
            SurfaceKind::Pet,
            at,
            size,
            dpi_path,
            &raw mut *slot,
        )?);
        win.install_userdata();
        let clock = match Clock::arm() {
            Ok(c) => c,
            Err(e) => {
                // `win` 会在返回时析构（DC / 位图随之释放），但**窗口句柄不会** ——
                // 那一步归本结构的 `Drop`，而提前失败时 `Drop` 不跑 ⇒ 这一支自己删。
                // 漏掉的形状不是崩，是"每次建窗失败漏一个 HWND"，而句柄表满了以后的报错
                // 长得跟内存不足一模一样。
                win.clear_userdata();
                unsafe { DestroyWindow(win.hwnd) };
                return Err(e);
            }
        };
        Ok(Client {
            wins: vec![win],
            slot,
            clock,
        })
    }

    /// 开一面新窗（菜单 / 卡 / 面板都走这一条）。`size` 是**物理像素**，与 [`Self::open`]
    /// 同口径；几何（逻辑 → 物理）归业务侧的 `Frame` 算，窗口层不猜缩放。
    ///
    /// ⚠️ 已存在同 `kind` ⇒ 报错而不是叠第二扇：两扇同身份就是"同一个 `hit(kind,…)`
    /// 有两个答案"，而症状（一半像素点得着）画在屏上、不报在编译器上。
    ///
    /// 新面建完立即 `SetWindowPos(HWND_TOPMOST)` 都**不做**：样式里已带
    /// `WS_EX_TOPMOST`，`CreateWindowExW` 的新窗默认就在同类最上；"菜单必须压住宠物"
    /// 这一格留给 Wine 尖峰做判卷项，而不是留给注释。
    pub fn spawn(
        &mut self,
        kind: SurfaceKind,
        at: (i32, i32),
        size: (i32, i32),
    ) -> Result<(), String> {
        if self.window(kind).is_some() {
            return Err(format!(
                "spawn 拒绝：{kind:?} 面已经在注册表里 ⇒ 一进程每面至多一扇"
            ));
        }
        // 全进程同一 DPI 档（`set_dpi_awareness` 只可能成功跑过一次），新面抄本体面的读数。
        let dpi_path = self
            .window(SurfaceKind::Pet)
            .expect(
                "注册表里没有本体面 —— `open` 的唯一产物就是它，走到这里说明 Client 被绕开构造了",
            )
            .dpi_path();
        let mut win = Box::new(Window::create(
            kind,
            at,
            size,
            dpi_path,
            &raw mut *self.slot,
        )?);
        win.install_userdata();
        self.wins.push(win);
        Ok(())
    }

    /// 关一面窗。**没开过 = 成功**（调用方经常是"超时"与"点击"在竞速， double-close
    /// 报错会把一次良性竞争读成运行期故障）。
    ///
    /// ⚠️ 次序与 [`Self::drop`] 逐字同条：**先清 userdata，再 `DestroyWindow`**。因此那条
    /// `WM_DESTROY` 看到的是空槽 ⇒ 不落全局旗、不 `PostQuitMessage`（见回调那一臂）——
    /// "关一扇菜单把整个进程带走"就是没有这条次序的症状。
    pub fn close(&mut self, kind: SurfaceKind) -> Result<(), String> {
        let Some(idx) = self.wins.iter().position(|w| w.kind == kind) else {
            return Ok(());
        };
        self.wins[idx].clear_userdata();
        unsafe { DestroyWindow(self.wins[idx].hwnd) };
        self.wins.remove(idx);
        Ok(())
    }

    pub fn window(&self, kind: SurfaceKind) -> Option<&Window> {
        self.wins.iter().find(|w| w.kind == kind).map(Box::as_ref)
    }

    pub fn window_mut(&mut self, kind: SurfaceKind) -> Option<&mut Window> {
        self.wins
            .iter_mut()
            .find(|w| w.kind == kind)
            .map(|w| &mut **w)
    }

    /// 等 + 抽干 + 派发一轮（**所有面共用这一条泵**，见模块头"一泵一线"）。
    /// 返回唤醒原因，供业务侧决定这一轮要不要步进内核。
    ///
    /// ⚠️ **[`Slot::ev`] 的 `Some` 有效期恰好是本函数的调用期间**，返回时一律清回 `None`。
    /// 这条是整块 unsafe 里唯一真正的信任点：派发只可能发生在 `self.drain()` 里，
    /// 而 `drain` 是私有方法且只在本函数被调 ⇒ `ev` 不可能在 `pump` 之外被窗口回调看到。
    ///
    /// 为什么"抽干"要抽到队列空、而不是只处理一条：只处理一条会把上一轮的消息堆成一条
    /// 越来越长的滞后队列，而拖拽那条路径读的是**最后**一次位置 ⇒ 症状是"松手后宠物还在按
    /// 旧轨迹走"，而且只有卡顿时才出现。
    /// `Wake::Failed` 时**照样**抽一遍（`WM_QUIT` 可能就在里面），然后把错误往上送。
    ///
    /// ⚠️ 那个 `&'static mut` 是**消耗掉**的（线性借用，返回时随 `None` 一起放掉）。所以
    /// `app.rs` 的每一轮都要从那个已 `Box::leak` 的对象重新取一次借用，而不是把同一个引用
    /// 存下来再传第二遍。这不是绕路：`'static` 正是"派发期间不会有人把它收走"这句话的
    /// 类型写法，代价（漏一份进程级对象）与 Wayland 侧 `QueueHandle` 要的那一份是同一条。
    pub fn pump(&mut self, ev: &'static mut dyn Events, wait_ms: u32) -> Result<Wake, String> {
        self.slot.ev = Some(ev);
        let wake = self.clock.wait(wait_ms);
        self.drain();
        self.slot.ev = None;
        match wake {
            Wake::Failed(e) => Err(format!(
                "MsgWaitForMultipleObjectsEx 失败（GetLastError={e:#X}）⇒ 唤醒源不可用，本轮退出"
            )),
            w => Ok(w),
        }
    }

    fn drain(&mut self) {
        let mut msg = MSG::default();
        loop {
            let got = unsafe { PeekMessageW(&raw mut msg, core::ptr::null_mut(), 0, 0, PM_REMOVE) };
            if got == 0 {
                break;
            }
            // `PeekMessageW` 与 `GetMessageW` 在这里的分别是**反的**：`GetMessageW` 遇到
            // `WM_QUIT` 返回 0 并把它留在队列里，而 `PeekMessageW` 会把这条 `WM_QUIT`
            // 抽出来给我们。所以下面那一行不是防御性的，是唯一的退出通路之一。
            if msg.message == WM_QUIT {
                self.slot.quit = true;
                break;
            }
            unsafe {
                TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }

    /// 上屏某一面的像素。**面没开 = 错误而不是静默**：静默的形状是"计数在涨而屏上没东西"，
    /// 那是第一批点名最贵的失败（进程活着、一切看起来绿、宠物不存在）。
    pub fn present(&mut self, kind: SurfaceKind, at: (i32, i32), src: &[u8]) -> Result<(), String> {
        self.window_mut(kind)
            .ok_or_else(|| format!("present 拒绝：{kind:?} 面没开（先 spawn）"))?
            .present(at, src)
    }

    pub fn move_only(&mut self, kind: SurfaceKind, at: (i32, i32)) -> Result<(), String> {
        self.window_mut(kind)
            .ok_or_else(|| format!("move_only 拒绝：{kind:?} 面没开（先 spawn）"))?
            .move_only(at)
    }

    /// 换尺寸（DPI 变了、或格子档变了）。**换完必须紧跟一次 [`Self::present`]**。
    pub fn resize(&mut self, kind: SurfaceKind, size: (i32, i32)) -> Result<(), String> {
        self.window_mut(kind)
            .ok_or_else(|| format!("resize 拒绝：{kind:?} 面没开"))?
            .resize(size)
    }

    pub fn size(&self, kind: SurfaceKind) -> Option<(i32, i32)> {
        self.window(kind).map(Window::size)
    }

    pub fn scale(&self, kind: SurfaceKind) -> Option<f32> {
        self.window(kind).map(Window::scale)
    }

    /// 本进程的 DPI 档位（各面同一个数：`set_dpi_awareness` 只可能跑一次）。
    /// 找不到本体面不是运行期状态，是构造被绕开 ⇒ `expect` 说清楚。
    pub fn dpi_path(&self) -> DpiPath {
        self.window(SurfaceKind::Pet)
            .expect("注册表里没有本体面（`open` 的唯一产物）")
            .dpi_path()
    }

    pub fn timer_path(&self) -> TimerPath {
        self.clock.path()
    }

    pub fn quit(&self) -> bool {
        self.slot.quit
    }

    /// 请求退出（`--seconds` 到点走这条）。
    ///
    /// 置旗而**不** `PostQuitMessage`：后者只往队列塞一条 `WM_QUIT`，而它要等到下一次等待
    /// 结束才被抽到 —— 我们的等待可能在它到达时正好睡满一轮。旗是当场生效的。
    /// ⚠️ 外部那条关闭路径（任务管理器、注销、别人发 `WM_CLOSE`）**只算在本体面头上**，
    /// 见 [`wnd_proc`] 的 `WM_DESTROY` 臂：非本体面被销毁既不落旗也不 `PostQuitMessage`。
    pub fn request_quit(&mut self) {
        self.slot.quit = true;
    }
}

impl Drop for Client {
    /// ⚠️ **逐扇先清 `GWLP_USERDATA`，再 `DestroyWindow`**（见 [`Window::drop`] 那条同族理由）。
    /// `UnregisterClassW` 故意不调：类在还有实例时注销会失败，而我们没有"关窗后再开一扇"
    /// 这条路径 ⇒ 那条注销是一条永不成功的调用（本项目对它的命名是假开关）。
    /// 进程退出时系统自己回收那张表。
    ///
    /// `slot` 的 `Box` 在本函数之后才析构 —— 而 `DestroyWindow` 会同步派发 `WM_DESTROY`，
    /// 那时各窗的 `Window::slot` 还活着。次序写反（先放掉 slot）的症状不是崩在这一行，
    /// 是崩在窗口回调里（use-after-free 的教科书形状，与第一批那条是同一条）。
    fn drop(&mut self) {
        for win in &mut self.wins {
            win.clear_userdata();
            unsafe { DestroyWindow(win.hwnd) };
        }
    }
}

/// 在**注册窗口类之前**设 DPI 感知，返回走了哪条路。
///
/// 由 [`Client::open`] 的第一步调用。公开它是为了让测试与文档能点名这一格，
/// **不是**为了让调用方自己排序（那种前置条件见 [`Client::open`] 里那句"迟早会被忘"）。
///
/// 两档退路不区分原因（"系统太老"与"已经被 manifest 设过"）是有意的：区分要
/// `GetProcAddress` 动态载入，而两档缺的是**同一个能力**（每窗 DPI + `WM_DPICHANGED`）
/// ⇒ 同一句真话。
pub fn set_dpi_awareness() -> DpiPath {
    let v2 = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if v2 != 0 {
        return DpiPath::PerMonitorV2;
    }
    if unsafe { SetProcessDPIAware() } != 0 {
        return DpiPath::ProcessAware;
    }
    DpiPath::Assume100
}

/// 窗口回调。`GWLP_USERDATA` 为空时（建窗期的 `WM_NCCREATE`/`WM_CREATE`，与析构之后）
/// 一律走 `DefWindowProcW` ⇒ 这条缝对"还没接线"是宽容的，对"接错了"是响的。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let win = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Window;
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            // layered window 的颜色不由 DC 绘制 ⇒ 这里只把脏区清掉，免得 `WM_PAINT`
            // 每轮重发（那会变成一条永不满足的绘制循环 + 一条看不见的忙等）。
            unsafe { ValidateRect(hwnd, core::ptr::null()) };
            0
        }
        WM_NCHITTEST => {
            // ⚠️ `lParam` 是**屏幕**坐标（不是客户区）⇒ 必须先 `ScreenToClient`。
            // 漏这一步的症状在单屏零原点上**量不出来**（两套坐标数值相同），只有把宠物拖到
            // 主屏左边那块负坐标副屏上才现形 ⇒ 这是本平台"多屏未验"那一格里最可能踩的一脚，
            // 所以它写在代码里而不是只写在文档里。
            let mut pt = POINT {
                x: lparam_x(lparam),
                y: lparam_y(lparam),
            };
            unsafe { ScreenToClient(hwnd, &raw mut pt) };
            if win.is_null() {
                // 没接线 ⇒ **不接点击**（宁可不接，也不在窗口还没就绪时吃掉用户的一下）。
                return HTTRANSPARENT as LRESULT;
            }
            let kind = unsafe { (*win).kind };
            // 这里取的是**派发窗口内那一份借用**：`hit` 借的是 `&dyn Events`，
            // 而同一时刻没有任何人改 `win` 指向的 `Window` ⇒ 借用与可变状态不重叠。
            // 各面读**自己那面 buffer** 的 alpha（阈值同一条，见模块头"逐面命中策略"）。
            if let Some(ev) = unsafe { events(win) } {
                if ev.hit(kind, pt.x, pt.y) {
                    return HTCLIENT as LRESULT;
                }
            }
            HTTRANSPARENT as LRESULT
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            if win.is_null() {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // 捕获 = 本平台"甩动中指针滑出剪影也不丢事件"的那一半，对应 Wayland 侧
            // "把 input region 临时扩成整块 surface"那一族代码。**捕获本来就归 hwnd**，
            // 每扇窗自己的旗（多面共存时"谁在拖"没有第二个答案）。
            unsafe { SetCapture(hwnd) };
            unsafe { (*win).captured = true };
            let kind = unsafe { (*win).kind };
            let (x, y) = (lparam_x(lparam), lparam_y(lparam));
            if let Some(ev) = unsafe { events(win) } {
                ev.press(kind, x, y);
            }
            0
        }
        WM_LBUTTONUP => {
            if win.is_null() {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            // ⚠️ 旗**先**落，再 `ReleaseCapture`：后者会同步发一条 `WM_CAPTURECHANGED`，
            // 而那一支只在旗还是 `true` 时才转达（见 `Events::capture_lost` 那条警告）。
            unsafe { (*win).captured = false };
            unsafe { ReleaseCapture() };
            let kind = unsafe { (*win).kind };
            let (x, y) = (lparam_x(lparam), lparam_y(lparam));
            if let Some(ev) = unsafe { events(win) } {
                ev.release(kind, x, y);
            }
            0
        }
        WM_RBUTTONDOWN => {
            if win.is_null() {
                return 0;
            }
            let kind = unsafe { (*win).kind };
            if let Some(ev) = unsafe { events(win) } {
                ev.right_press(kind);
            }
            0
        }
        WM_RBUTTONUP => {
            // **吃掉抬起、不转 `DefWindowProcW`**：默认处理会在抬起后补一条 `WM_CONTEXTMENU`，
            // 而这一片的"右键"语义在**按下**那一刻就已经交付（开/关菜单），再让系统弹一份
            // 空上下文是第二个事实源。`win` 为空同样返回 0 —— 没接线时右键连"落点"都不该有。
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = (lparam_x(lparam), lparam_y(lparam));
            if win.is_null() {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let kind = unsafe { (*win).kind };
            if let Some(ev) = unsafe { events(win) } {
                ev.motion(kind, x, y);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CAPTURECHANGED => {
            if win.is_null() {
                return 0;
            }
            // `wparam` = 现在握着捕获的那个窗口。我们自己 `ReleaseCapture` 的那一次，
            // 旗已经在 `WM_LBUTTONUP` 里落了 ⇒ 走到这里还是 `true` 的只剩"被别人抢走"。
            let stolen = unsafe { (*win).captured };
            unsafe { (*win).captured = false };
            if stolen {
                let kind = unsafe { (*win).kind };
                if let Some(ev) = unsafe { events(win) } {
                    ev.capture_lost(kind);
                }
            }
            0
        }
        WM_DPICHANGED => {
            if win.is_null() {
                return 0;
            }
            // `wparam` 的低 16 位 = 新 dpi。这里不用 `GetDpiForWindow` 是因为那一刻窗口还挂在
            // **旧** DPI 上下文上（`WM_DPICHANGED` 的时序就是"通知你、随你改"），
            // 而 wParam 那个数**已经是**新的 ⇒ 拿它。
            let dpi = (wparam as u32) & 0xFFFF;
            let s = scale_from_dpi(dpi);
            unsafe { (*win).scale = s };
            let kind = unsafe { (*win).kind };
            if let Some(ev) = unsafe { events(win) } {
                ev.dpi_changed(kind, s);
            }
            // ⚠️ **不**照 `lParam` 那份"建议矩形" `SetWindowPos`：我们的尺寸由
            // `grid × scale + 2 × margin` 算（`layout::Frame`），而建议矩形假定的是"固定逻辑
            // 尺寸"。两者今天**恰好相等**，但那是两个事实源：以后格子档一变，系统那份会静默
            // 给出一个错的尺寸。新尺寸的落地由 `dpi_changed` 的业务侧走 `resize` + `present`。
            // 顺带：这里不按 `wparam` 高位判"哪一边变了"，因为跨屏拖动会连发多条，
            // 而 `Window::resize` 是幂等的。
            0
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            if win.is_null() {
                // **编程式关闭**那一支（[`Client::close`] / [`Client::drop`] 先清 userdata 再
                // 删窗 ⇒ 回调看到这里必然是空）。什么都不落：调用方在业务侧自己记
                // "这一面关了"。第一批写不出这一支的注释 —— 那时只有一个关法。
                return 0;
            }
            // **外部关闭**那一支（注销 / 任务管理器 / 别人发的 `WM_CLOSE`）只算在本体面头上：
            // 非本体面被销毁既不落全局旗也不 `PostQuitMessage`，否则"关一扇卡面"会带走进程。
            let kind = unsafe { (*win).kind };
            if kind.is_pet() {
                unsafe { (*(*win).slot).quit = true };
                unsafe { PostQuitMessage(0) };
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 共享槽里现在**有没有**业务对象（只在 [`Client::pump`] 的调用期间有）。
///
/// 抽出来是因为上面那六处派发点逐字同形，而"漏一次判空"这一族错在窗口回调里是
/// **直接崩**而不是报错。判空本身由 [`Slot::ev`] 那个 `Option` 承担 ⇒ 这个函数没有
/// 第二种"没有"要判（`win` 为空由调用方在取它之前判，见各派发臂的第一行；`win->slot`
/// 不为空由 [`Client`] 的构造与析构次序保证，见 [`Window::slot`]）。
///
/// 返回类型里那个显式的 `+ 'static` **不许简写掉**：`&'a mut dyn Events` 的省略界是
/// `'a` 而不是 `'static`，而 `&mut` 对内容项**不变** ⇒ 那个写法要的是
/// `dyn Events + 'a`，与本槽里的 `dyn Events + 'static` 是两个类型，编译期就报
/// "`'a` must outlive `'static`"（这一条是实测，不是推测）。
///
/// # Safety
/// `win` 必须是 [`Window::install_userdata`] 存进去的那个 `Window` 地址，而那块 `Window`
/// 的 `slot` 必须指向 [`Client`] 拥有的活 `Slot`；返回的借用只在那一次 [`Client::pump`]
/// 调用期间有效，**不许**留到 `pump` 返回之后。
unsafe fn events<'a>(win: *mut Window) -> Option<&'a mut (dyn Events + 'static)> {
    unsafe { (*(*win).slot).ev.as_deref_mut() }
}
