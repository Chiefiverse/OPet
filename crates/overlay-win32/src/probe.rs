//! `opet-overlay-win32 --probe`：这台机器能不能撑起这块 overlay，四格读数（M7f 转真）。
//!
//! ## 这一格与 Wayland 那一家**同名不同物**，而且不改名才是诚实
//!
//! 设计书 §6.2 给 Phase B 第 1 步的要求是"客户端自带诊断，因为 Arch 上没有
//! `wayland-info`/`weston-info`"。`overlay-wayland/src/probe.rs` 交的是 registry（绑到哪些
//! global、各什么版本）+ output 几何；本平台**没有 registry 可枚举**，所以同名开关交的是四件
//! 真读得到的事：
//!
//! 1. **监视器**：逐台 GDI 设备名 + 物理矩形 + `rcWork` 顶/底 + 哪一台是 OS 主屏，并标出
//!    [`crate::layout::choose`] 会挑中哪一台当舞台（`--monitor` > 主屏 > 第一台）。
//! 2. **DPI**：`GetDpiForSystem` 那个整数，**加上它属于哪一档**（见下面第二条读数）。
//! 3. **留白**：舞台那台的 [`crate::layout::Insets`]，由 [`crate::layout::resolve_insets`]
//!    算 —— 与真跑启动行**同一个函数**，所以"探针说 40 而宠物站在 0"这种谎在这 struct 上
//!    根本写不出来。
//! 4. **分层窗口支持**：`UpdateLayeredWindow` 这个符号在本进程里解析得到吗。
//!
//! ⚠️ **它不是"没有后端的旗"了，但也不是 Wayland 那份的翻译**：`cli.rs` 原先拒绝它的理由
//! （"打的是 registry，这个平台上没有 registry ⇒ 空清单称通过比不查更坏"）现在由"清单不空、
//! 而且每一项都说得出出处"回答。那句话留在 `cli.rs` 的历史段里，不要删。
//!
//! ## 两格刻意的"不查"，都写进输出而**不是**留白
//!
//! ① **不查 DWM 合成状态**（`DwmIsCompositionEnabled`）。看着像"分层窗口支持"的天然读数，
//! 实际不是：layered window + `AC_SRC_ALPHA` 早于 DWM（Win2000 就是那条路），合成关了也照样
//! 逐像素 alpha ⇒ 那一格答不了本探针要答的问题，却要往导入表里添一个 `dwmapi.dll`
//! （DLL 白名单 16 → 17，`scripts/build_windows.sh` 那张表得整张重拿）。**为一个答不了问题的
//! 读数重测白名单，是不划算的**，所以这里明写"没查"，不查而不说才是骗。
//! ② **不建窗口、不改进程 DPI 感知档**——唯一例外是**线程级**感知那一格，且它是被证伪才加的：
//! 真机第一轮（2026-11-26，§6.1.26 ⑩）在 125% 屏上，unaware 档的 `GetMonitorInfoW` 把
//! 1920x1080 报成 1536x864（= 物理 ÷ scale 的**虚拟化**矩形），而这一格印的标签是"物理"
//! ⇒ 探针在打一句带谎的数。修法不是 `SetProcessDpiAwarenessContext`（那会让探针读到的档
//! 变成"探针进程自己的"，真跑读的是**窗口挂上显示器之后**的每窗 DPI，`window.rs:471`
//! 那个顺序不能反），而是 `SetThreadDpiAwarenessContext`：只抬本线程这一次枚举，办完
//! 立刻把返回的旧值写回。进程档纹丝不动，同 user32、导入表不多一个名字。
//! DPI 那一档的读数语义不变（未感知**进程**那档的系统整数），变的只有矩形。
//!
//! 反过来，**符号可见性**（第 4 格与 DPI 那一档的下半）是探针能诚实回答的：
//! `GetProcAddress` 解析不到 `SetProcessDpiAwarenessContext` = 这台机器给不了
//! [`crate::window::DpiPath::PerMonitorV2`]，宠物会停在开机那一档缩放。那句话只有在这里说
//! 才来得及（用户还没跑第一轮）。
//!
//! ⚠️ 但**前提是句柄拿对了**：`GetModuleHandleW(null)` 给的是 `.exe` 自己的模块，不是
//! "已加载 DLL 的默认搜索路径"。第一版拿它查四枚 user32 符号 ⇒ Wine 上一份全"解析不到"的报告，
//! 而同一份 `.exe` 在同一个 Wine 上画得出宠物。修法 = 点名 `"user32.dll"`（`read_symbols`
//! 旁边记了完整因果）。这一格是"**仪器自己造假**"那一族：报告长得像读数，错在探针。

#[cfg(windows)]
use windows_sys::Win32::Foundation::HMODULE;
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use crate::layout::{Insets, PhysMon, describe_inset, scale_from_dpi};

/// 本机能给哪一条 DPI 通路。**由符号可见性判**，不由版本号猜。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpiSupport {
    /// `SetProcessDpiAwarenessContext` 与 `GetDpiForWindow` 都解析得到 ⇒ 真跑能走每窗那一档。
    PerMonitorV2,
    /// 只有旧的 `SetProcessDPIAware`（Vista 起恒在）⇒ 真跑会走进程档，拖屏不重算。
    ProcessAware,
    /// 连 `GetDpiForSystem` 都解析不到（Win 8.1 之前）⇒ 真跑只能按 1.00 算。
    Assume100,
}

impl DpiSupport {
    pub fn as_str(self) -> &'static str {
        match self {
            DpiSupport::PerMonitorV2 => "每窗那一档可达（拖屏会重算缩放）",
            DpiSupport::ProcessAware => "只有进程档 ⇒ 拖到另一块缩放不同的屏上，宠物停在开机那一档",
            DpiSupport::Assume100 => "两条都不可达 ⇒ 按 1.00 算",
        }
    }
}

/// 分层窗口与 DPI 那四枚符号的可见性。`false` 的语义是**"本进程解析不到这个名字"**，
/// 不是"这台机器不支持"（后者要版本判定，而版本判定在 Windows 上恰恰是最容易撒谎的一格：
/// `GetVersionExW` 受 manifest 影响，`wmic`/注册表又不属于本进程的视角）。
///
/// ⚠️ 四枚**全部是运行时解析**来的，本 `.exe` 没有静态导入它们中任何一个（静态导入会让整个
/// 二进制在缺符号的机器上连 `main` 都进不去）⇒ 这一格的 `false` 是真读数。哪天有人把它们
/// 改成静态导入，这句话就成了假话，而它写在这里就是为了那一刻。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Symbols {
    /// 探针本体（第 4 格）：没有它就没有逐像素 alpha。
    pub update_layered_window: bool,
    pub set_process_dpi_awareness_context: bool,
    pub get_dpi_for_window: bool,
    pub get_dpi_for_system: bool,
}

impl Symbols {
    fn dpi_support(self) -> DpiSupport {
        if self.set_process_dpi_awareness_context && self.get_dpi_for_window {
            DpiSupport::PerMonitorV2
        } else if self.get_dpi_for_system {
            DpiSupport::ProcessAware
        } else {
            DpiSupport::Assume100
        }
    }
}

/// 一次探测的全部读数。**空字段与"读到了 0"是两回事**，所以能缺席的东西一律 `Option`。
#[derive(Clone, Debug, PartialEq)]
pub struct ProbeReport {
    /// GDI 枚举到的每一台。空表 = 一台都没有（`monitor::enumerate` 那一条硬拒绝的形状）。
    pub monitors: Vec<PhysMon>,
    /// [`crate::layout::choose`] 挑中的舞台那台的名字；`None` = 没有舞台可算。
    pub stage: Option<String>,
    /// 探针看到的 `--monitor` 覆盖值（原样带回来，好让"点了不存在的那台"在输出里可见）。
    pub wanted_monitor: Option<String>,
    /// 舞台那台给出的整数 DPI（未感知档，见模块头第②条）。
    pub system_dpi: Option<u32>,
    pub symbols: Symbols,
    /// 舞台那台的留白。**只在有舞台且有 DPI 时才算得出**（逻辑 = 物理 ÷ scale）。
    pub insets: Option<Insets>,
    /// 一句"这次探测本身有没有失败"。有它就不必靠空表猜根因。
    pub session_error: Option<String>,
}

impl ProbeReport {
    /// 空报告（宿主与非 Windows 路径的返回值）。
    fn unavailable(msg: &str) -> Self {
        Self {
            monitors: Vec::new(),
            stage: None,
            wanted_monitor: None,
            system_dpi: None,
            symbols: Symbols::default(),
            insets: None,
            session_error: Some(msg.to_string()),
        }
    }

    /// 舞台那台的生效缩放。没有 DPI 就是 `None` —— **不**在这里替它编一个 1.00，
    /// 因为"探针没读到"与"这台机器给 96"在报告里必须是两种形状。
    pub fn scale(&self) -> Option<f32> {
        self.system_dpi.map(scale_from_dpi)
    }
}

/// 通过判定：连得上显示器、有舞台、而且分层窗口那个符号在。
///
/// 与 Wayland 那家同一个纪律：**判据不写进正文**，正文只给读数；调用方
/// （`cli::dispatch` → `main.rs` 的退出码 1）读这一个布尔。
pub fn probe_ok(r: &ProbeReport) -> bool {
    r.session_error.is_none() && r.stage.is_some() && r.symbols.update_layered_window
}

/// Phase B 报告里贴的就是它。**纯函数**：输入全在 [`ProbeReport`] 里，
/// 所以这一整段格式化在 Linux 宿主上逐格可测（`collect` 那半边才要真机）。
pub fn format_report(r: &ProbeReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "== opet-overlay-win32 --probe");
    if let Some(e) = &r.session_error {
        // 失败那一支只说这一句就返回：半张表比空表更容易被读成"没有问题"。
        let _ = writeln!(s, "探测失败: {e}");
        return s;
    }

    // ── 1. 监视器 ──
    let _ = writeln!(s, "== 显示器（GDI 枚举，物理像素）");
    if r.monitors.is_empty() {
        let _ = writeln!(s, "  （一台都没有）");
    }
    for m in &r.monitors {
        let mark = match (&r.stage, r.wanted_monitor.as_deref()) {
            (Some(name), _) if name.eq_ignore_ascii_case(&m.name) => "舞台",
            (None, Some(_)) => "点名未命中",
            _ if m.os_primary => "OS 主屏",
            _ => "",
        };
        let _ = writeln!(
            s,
            "  {:<16} {}x{} @({},{})  工作区顶={} 底={}  {}{}",
            if m.name.is_empty() { "?" } else { &m.name },
            m.right - m.left,
            m.bottom - m.top,
            m.left,
            m.top,
            m.work_top,
            m.work_bottom,
            if m.os_primary { "主屏" } else { "    " },
            if mark.is_empty() {
                String::new()
            } else {
                format!("⇒ {mark}")
            }
        );
    }
    if let Some(w) = &r.wanted_monitor {
        let _ = writeln!(
            s,
            "  （--monitor 点名 {w}，匹配是大小写不敏感的整名；上面没有这一台 = 这个名字打错了）"
        );
    }

    // ── 2. DPI ──
    let _ = writeln!(s, "== DPI");
    match r.system_dpi {
        Some(d) => {
            let _ = writeln!(
                s,
                "  GetDpiForSystem = {d} ⇒ scale {}（⚠️ 探针不建窗口、不改感知档，所以这是**未感知进程**那一档；真跑读的是窗口挂上显示器之后的每窗 DPI，两者在混缩放多屏上可以不同）",
                r.scale().unwrap_or(1.0)
            );
        }
        None => {
            let _ = writeln!(
                s,
                "  GetDpiForSystem 读不到 ⇒ 这里没有缩放读数，真跑会按 1.00 算并打一行日志"
            );
        }
    }
    let _ = writeln!(
        s,
        "  本机可达的 DPI 通路 = {}",
        r.symbols.dpi_support().as_str()
    );
    for (name, present) in [
        (
            "SetProcessDpiAwarenessContext",
            r.symbols.set_process_dpi_awareness_context,
        ),
        ("GetDpiForWindow", r.symbols.get_dpi_for_window),
        ("GetDpiForSystem", r.symbols.get_dpi_for_system),
    ] {
        let _ = writeln!(
            s,
            "    {name:<34} {}",
            if present {
                "解析得到"
            } else {
                "解析不到"
            }
        );
    }

    // ── 3. 留白 ──
    let _ = writeln!(s, "== 留白（舞台那台，逻辑像素）");
    match (&r.insets, r.scale()) {
        (Some(ins), Some(sc)) => {
            let _ = writeln!(
                s,
                "  {}",
                describe_inset("panel_top", &ins.top, ins.measured_top)
            );
            let _ = writeln!(
                s,
                "  {}",
                describe_inset("panel_bottom", &ins.bottom, ins.measured_bottom)
            );
            // 现量是物理矩形减出来的，这里把它回乘一次给"量到的物理像素"，
            // 免得读的人拿逻辑数去对 `rcWork`（那两处是两套单位，M4 踩坑第 2 条的同族）。
            let _ = writeln!(
                s,
                "  现量物理：顶={} 底={}（÷ {sc} = 上面那两个逻辑数；竖排任务栏的左右留白内核没有输入，未接）",
                (ins.measured_top * sc).round() as i32,
                (ins.measured_bottom * sc).round() as i32,
            );
        }
        _ => {
            let _ = writeln!(
                s,
                "  算不出（没有舞台或缺 DPI）⇒ 上面两格哪一格空着就是根因"
            );
        }
    }

    // ── 4. 分层窗口 ──
    let _ = writeln!(s, "== 分层窗口");
    let _ = writeln!(
        s,
        "  UpdateLayeredWindow {}",
        if r.symbols.update_layered_window {
            "解析得到 ⇒ 逐像素 alpha 这条路可用"
        } else {
            "解析不到 ⇒ 本客户端无法画任何东西"
        }
    );
    let _ = writeln!(
        s,
        "  没查：DWM 合成状态（layered window 不依赖合成，而查它要给导入表添 dwmapi.dll ⇒ 白名单整张重拿；这一格答不了这里要答的问题）"
    );

    let _ = writeln!(
        s,
        "== 判定：舞台={:?}、分层窗口={}、DPI={} ⇒ {}",
        r.stage,
        if r.symbols.update_layered_window {
            "有"
        } else {
            "无"
        },
        r.system_dpi
            .map(|d| d.to_string())
            .unwrap_or_else(|| "读不到".into()),
        if probe_ok(r) { "PASS" } else { "FAIL" }
    );
    s
}

/// 真去问一遍操作系统。**唯一碰 OS 类型的采集函数**（模块头那条"格式化全在宿主可测"的另一半）。
#[cfg(windows)]
pub fn collect(
    wanted: Option<&str>,
    flag_top: Option<f32>,
    flag_bottom: Option<f32>,
) -> ProbeReport {
    use crate::layout::{choose, resolve_insets};
    // 与 §6.1.26 ⑩ 同一条根：unaware 档上 `GetMonitorInfoW` 交回的是**虚拟化**矩形
    // （物理 ÷ scale 再被下游除一次）。但 M7 拍板"probe 不改进程 DPI 感知档"照旧
    // 有效 ⇒ 正确的旋钮是**线程级**感知：只管本线程这一次枚举，办完立刻恢复。
    // 进程档纹丝不动（同函数、同 user32，导入表一个名字都不多），读到的却是真物理矩形。
    type DpiCtx = windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT;
    let prev: DpiCtx = unsafe {
        windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };
    let monitors = match crate::monitor::enumerate() {
        Ok(v) => v,
        Err(e) => {
            restore_thread_dpi(prev);
            return ProbeReport::unavailable(&e);
        }
    };
    restore_thread_dpi(prev);
    let stage = choose(&monitors, wanted).cloned();
    let symbols = match read_symbols() {
        Some(s) => s,
        None => {
            return ProbeReport::unavailable(
                "user32.dll 在本进程里没有模块句柄 ⇒ 符号那一格没有读数（这不等于\"这台机器没有\"）",
            );
        }
    };
    let system_dpi = if symbols.get_dpi_for_system {
        match unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } {
            0 => None,
            d => Some(d),
        }
    } else {
        None
    };
    let insets = match (&stage, system_dpi) {
        (Some(m), Some(d)) => Some(resolve_insets(m, scale_from_dpi(d), flag_top, flag_bottom)),
        _ => None,
    };
    ProbeReport {
        stage: stage.map(|m| m.name.clone()),
        wanted_monitor: wanted.map(str::to_string),
        monitors,
        system_dpi,
        symbols,
        insets,
        session_error: None,
    }
}

/// 把 `SetThreadDpiAwarenessContext` 返回的旧值原样写回。文档明说返回句**可以**当输入恢复
/// （包括那两枚特殊值 `OLD`/`INVALID`），所以这里**无条件回写**，不加 `is_null` 判断——
/// 加了反而是自造第三档。
#[cfg(windows)]
fn restore_thread_dpi(prev: windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT) {
    unsafe { windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(prev) };
}

/// 非 Windows 上探针**没有东西可探**：这里不给一份"全 0 的清单"，因为那份清单长得像读数。
/// 本 crate 的宿主编译只服务于测试（`lib.rs` 那张表），所以这一支存在的理由是
/// "`--probe` 在宿主上也要能跑完 `dispatch` 而不炸"。
#[cfg(not(windows))]
pub fn collect(
    wanted: Option<&str>,
    _flag_top: Option<f32>,
    _flag_bottom: Option<f32>,
) -> ProbeReport {
    ProbeReport::unavailable(&format!(
        "这个二进制不是为当前系统编的（本探针读的是 GDI 与 user32）；\
         点名显示器 {:?} 没有被尝试",
        wanted
    ))
}

#[cfg(windows)]
fn read_symbols() -> Option<Symbols> {
    // ⚠️ **参数不能是 `null`**。第一版这里写的是 `GetModuleHandleW(std::ptr::null())`，
    // Wine 一轮把四枚符号全打成"解析不到"，而同一份 `.exe` 在同一个 Wine 上画得出宠物
    //（ULW 叠帧是 M5 第二批的实测现象）⇒ 那是**假阴性，不是平台读数**。原因写在 MSDN 的那句
    // "NULL = 返回**创建本进程那个文件**（`.exe`）的句柄"里：exe 模块的导出表里只有它自己的符号，
    // 拿它查 `UpdateLayeredWindow` 必然查不到。**要查 user32 就得点名 user32。**
    // `window.rs` 用 `null` 是对的另一半：那里要的是 `hinstance` 本身，不是符号查找。
    //
    // 探针**不**静态链接下面四个名字（静态导入会在缺符号的机器上把整个 `.exe` 挡在 `main`
    // 之前），所以这里逐枚解析；解析不到就是这台机器真的给不了那一档。
    let name = crate::windx::wide("user32.dll");
    let user32: HMODULE = unsafe { GetModuleHandleW(name.as_ptr()) };
    if user32.is_null() {
        // 没有模块句柄 = **这一格没有读数**，而"四枚都解析不到"是一个读数。两者不能同形
        //（`summary.rs` 那条"没链进来不许写成 0"的同族）。交回 `collect` 走 `session_error`。
        return None;
    }
    let has = |sym: &[u8]| -> bool {
        let mut cstr = sym.to_vec();
        cstr.push(0);
        unsafe { GetProcAddress(user32, cstr.as_ptr()) }.is_some()
    };
    Some(Symbols {
        update_layered_window: has(b"UpdateLayeredWindow"),
        set_process_dpi_awareness_context: has(b"SetProcessDpiAwarenessContext"),
        get_dpi_for_window: has(b"GetDpiForWindow"),
        get_dpi_for_system: has(b"GetDpiForSystem"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{BASE_DPI, InsetSource, resolve_insets};

    fn mon(name: &str, left: i32, top: i32, right: i32, bottom: i32, primary: bool) -> PhysMon {
        PhysMon {
            name: name.to_string(),
            left,
            top,
            right,
            bottom,
            work_top: top + 40,
            work_bottom: bottom - 40,
            os_primary: primary,
        }
    }

    fn all_symbols() -> Symbols {
        Symbols {
            update_layered_window: true,
            set_process_dpi_awareness_context: true,
            get_dpi_for_window: true,
            get_dpi_for_system: true,
        }
    }

    fn report(monitors: Vec<PhysMon>, stage: Option<&str>, dpi: Option<u32>) -> ProbeReport {
        let scale = dpi.map(scale_from_dpi);
        let insets = match (
            monitors.iter().find(|m| Some(m.name.as_str()) == stage),
            scale,
        ) {
            (Some(m), Some(sc)) => Some(resolve_insets(m, sc, None, None)),
            _ => None,
        };
        ProbeReport {
            stage: stage.map(str::to_string),
            wanted_monitor: None,
            monitors,
            system_dpi: dpi,
            symbols: all_symbols(),
            insets,
            session_error: None,
        }
    }

    /// 四节标题**恒**存在，且顺序固定。存在的理由不是排版偏好：Phase B 脚本与人都按
    /// `== <节>` 找数，缺节 = 那一格从来没被读。
    ///
    /// ⚠️ 判定行也以 `== ` 开头（它是这张表的最后一行），所以这里数的是**六**行而不是四行。
    /// 断言写成"前五行的内容 + 第六行是判定"，而不是把判定行的整句文本钉死 —— 后者会把三个
    /// 读数一起钉住，那正是下面几条测例各自要单独打的东西。
    #[test]
    fn the_four_sections_are_always_there_in_order() {
        let txt = format_report(&report(
            vec![mon(r"\\.\DISPLAY1", 0, 0, 1920, 1080, true)],
            Some(r"\\.\DISPLAY1"),
            Some(96),
        ));
        let secs: Vec<&str> = txt.lines().filter_map(|l| l.strip_prefix("== ")).collect();
        assert_eq!(
            secs.len(),
            6,
            "判定行之外多/少了一节，或某节标题不再是行首: {txt}"
        );
        assert_eq!(
            secs[..5],
            [
                "opet-overlay-win32 --probe",
                "显示器（GDI 枚举，物理像素）",
                "DPI",
                "留白（舞台那台，逻辑像素）",
                "分层窗口",
            ],
            "{txt}"
        );
        assert!(secs[5].starts_with("判定："), "{txt}");
        assert!(txt.ends_with("⇒ PASS\n"), "{txt}");
    }

    /// 留白那一节必须**复述真跑那一条式子**：现量 40 物理 px、scale 1.0 ⇒ `40（现量）`，
    /// 而旗覆盖时要把没赢的那个数也带出来。这一格与 `app.rs` 的启动行同源（同一个
    /// `resolve_insets` + 同一个 `describe_inset`），所以这里红 = 两处一起变过。
    #[test]
    fn the_inset_section_repeats_the_startups_own_math() {
        let r = report(vec![mon("m", 0, 0, 1920, 1080, true)], Some("m"), Some(96));
        let txt = format_report(&r);
        assert!(txt.contains("panel_top 40（现量）"), "{txt}");
        assert!(txt.contains("panel_bottom 40（现量）"), "{txt}");
        assert!(txt.contains("现量物理：顶=40 底=40"), "{txt}");

        // 150% 那一档：物理还是 40，逻辑变成 26.666…。除不尽**正是**要看的形状。
        let r150 = report(vec![mon("m", 0, 0, 1920, 1080, true)], Some("m"), Some(144));
        let txt150 = format_report(&r150);
        let ins = r150.insets.as_ref().unwrap();
        assert_eq!(ins.top.source, InsetSource::Measured);
        assert!(
            (ins.top.px - 40.0 / 1.5).abs() < 1e-4,
            "逻辑留白必须是物理 ÷ scale：{}",
            ins.top.px
        );
        assert!(txt150.contains("÷ 1.5"), "{txt150}");

        // 旗覆盖：探针没给旗（上面那份）与给了旗的两行文案必须不同形。
        let mut with_flag = report(vec![mon("m", 0, 0, 1920, 1080, true)], Some("m"), Some(96));
        with_flag.insets = Some(resolve_insets(&with_flag.monitors[0], 1.0, Some(0.0), None));
        let txt_flag = format_report(&with_flag);
        assert!(
            txt_flag.contains("panel_top 0（旗覆盖，现量 40）"),
            "{txt_flag}"
        );
    }

    /// "读不到"与"读到 0"是两种形状：**不许把缺读数写成 `0`**（与 `summary.rs` 那条
    /// "没链进来不许写成 0"同一条纪律）。
    ///
    /// ⚠️ 缺 DPI 这一格**仍然判 PASS**，这不是漏了一条判据：真跑在读不到 DPI 时按 1.00 画并
    /// 打一行日志（`window.rs` 那条 DPI 通路），所以"这台机器撑不起 overlay"不成立。诚实的地方
    /// 在于**判定行自己要把这个缺读数带出来** —— 一句"⇒ PASS"而 DPI 那格空着，读的人才真会上当。
    #[test]
    fn a_missing_reading_is_not_a_zero() {
        let no_dpi = report(vec![mon("m", 0, 0, 1920, 1080, true)], Some("m"), None);
        let txt = format_report(&no_dpi);
        assert!(txt.contains("GetDpiForSystem 读不到"), "{txt}");
        assert!(txt.contains("算不出"), "{txt}");
        assert!(!txt.contains("scale 0"), "把缺读数写成 0 了：{txt}");
        assert_eq!(no_dpi.scale(), None);
        assert!(no_dpi.insets.is_none());
        // 缺读数进判定行，且判定行仍然在（不是"这一格空着就不打"）。
        assert!(txt.ends_with("DPI=读不到 ⇒ PASS\n"), "{txt}");
        // 而"有舞台但没有分层窗口符号"那一种才是真的撑不起：同一条表上 PASS 必须翻。
        let mut no_ulw = no_dpi.clone();
        no_ulw.symbols.update_layered_window = false;
        assert!(format_report(&no_ulw).ends_with("⇒ FAIL\n"));
    }

    /// 一台都没有 = `session_error` 那一条路径：正文只有**一句**，四节标题一个都不打。
    /// 半张长得像读数的表比一句话危险得多（`AGENTS.md` 沉默降级那一族）。
    #[test]
    fn an_unavailable_probe_says_one_sentence() {
        let txt = format_report(&ProbeReport::unavailable(
            "EnumDisplayMonitors 一台可用显示器都没给",
        ));
        assert_eq!(txt.lines().count(), 2, "{txt}");
        assert!(
            txt.starts_with("== opet-overlay-win32 --probe\n探测失败: "),
            "{txt}"
        );
        assert!(!txt.contains("== 显示器"), "{txt}");
    }

    /// 符号可见性 → DPI 通路的三档分派，含中间那一档**只缺** per-monitor 两枚的形状。
    #[test]
    fn dpi_tier_comes_from_the_symbols_not_from_a_version_number() {
        assert_eq!(all_symbols().dpi_support(), DpiSupport::PerMonitorV2);
        let old = Symbols {
            update_layered_window: true,
            set_process_dpi_awareness_context: false,
            get_dpi_for_window: false,
            get_dpi_for_system: true,
        };
        assert_eq!(old.dpi_support(), DpiSupport::ProcessAware);
        let stone = Symbols {
            get_dpi_for_system: false,
            ..Default::default()
        };
        assert_eq!(stone.dpi_support(), DpiSupport::Assume100);
        // 文案要说"会怎样"，不是"缺什么"：读的人要能据此决定要不要拖屏跨缩放。
        assert!(old.dpi_support().as_str().contains("拖到另一块"));
    }

    /// `--monitor` 点名点名未命中时，表上每一台都**不许**被标成舞台，而那一句要出现。
    #[test]
    fn a_named_monitor_that_is_not_there_is_said_out_loud() {
        let mut r = report(
            vec![mon(r"\\.\DISPLAY1", 0, 0, 1920, 1080, true)],
            None,
            Some(96),
        );
        r.stage = None;
        r.wanted_monitor = Some(r"\\.\DISPLAY9".to_string());
        let txt = format_report(&r);
        assert!(txt.contains("点名未命中"), "{txt}");
        assert!(txt.contains("这个名字打错了"), "{txt}");
        assert!(!txt.contains("⇒ 舞台"), "{txt}");
        assert!(!probe_ok(&r), "点名未命中却判通过 = 宠物会在错误的屏上");
    }

    /// 通过判定的三条**逐个**能红（不是"全有才绿"的一句空话）。
    #[test]
    fn the_pass_gate_has_three_independent_inputs() {
        let good = report(vec![mon("m", 0, 0, 1920, 1080, true)], Some("m"), Some(96));
        assert!(probe_ok(&good));

        let no_stage = report(vec![mon("m", 0, 0, 1920, 1080, true)], None, Some(96));
        assert!(!probe_ok(&no_stage));

        let mut no_ulw = good.clone();
        no_ulw.symbols.update_layered_window = false;
        assert!(!probe_ok(&no_ulw));

        let mut failed = good.clone();
        failed.session_error = Some("x".into());
        assert!(!probe_ok(&failed));
    }

    /// `BASE_DPI` 与 `scale_from_dpi` 在这一格里是同一个基准：探针打的 scale 数与
    /// `Frame::scale` 必须同式，否则"探针说 1.5 而窗口按 1.5 画"这句话是两条式子的巧合。
    #[test]
    fn the_probe_scale_is_the_frames_own_formula() {
        for (dpi, want) in [(96u32, 1.0f32), (120, 1.25), (144, 1.5), (192, 2.0)] {
            let r = report(vec![mon("m", 0, 0, 1920, 1080, true)], Some("m"), Some(dpi));
            assert_eq!(r.scale(), Some(want), "{dpi} DPI 档与真跑不同式");
            assert_eq!(want, dpi as f32 / BASE_DPI);
        }
    }
}
