//! 显示器枚举（`#[cfg(windows)]`）。
//!
//! 这个文件点的是 **GDI 显示器结构**（`HMONITOR` / `MONITORINFOEXW` / `RECT`），
//! 不碰窗口 ⇒ 它不违反 `lib.rs` 那条"`window.rs` 是唯一点名窗口/合成类 OS 类型的文件"
//! （那条规矩的判据是"是不是窗口/合成类"，不是数文件名，见 `lib.rs` 里那一段的展开）。
//!
//! ## 为什么它单独一个文件，而不是塞进 `window.rs`
//!
//! 因为**两拨调用者**要它：启动时算舞台（`app.rs`），以及 `WM_DPICHANGED` 之后重算
//! （`window.rs` 的窗口过程）。放进任何一边的话，另一边就得反向依赖，而那两个方向
//! 都会把"业务模块点名 OS 类型"那条线擦掉一格。
//!
//! ## `rcWork` 是本片唯一读的两条留白
//!
//! 顶与底（水平任务栏的两种位置）。`rcWork.left/right` —— 也就是**竖排任务栏**——
//! 内核根本没有对应的输入（`layout::PhysMon` 的注释里登记了这件事）⇒ **未接，
//! 不是"读了但没用"**。第 11 步的回写要照这一句写。

use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows_sys::core::BOOL;

use crate::layout::PhysMon;
use crate::windx::from_wide;

/// 枚举所有显示器。
///
/// ⚠️ **失败一台就跳掉那一台并打一行，失败全部才报错** —— 这两档不是一句"容错"：
/// 虚拟显示驱动 / 远程桌面 / 采集卡报出的显示器里，`GetMonitorInfoW` 返回 0
/// 是**实测会发生的形状**（Windows 的文档只说"句柄无效"，而无效句柄在这一族里
/// 常常是"刚刚被拔掉的那台"）。把其中一台的失败升级成"宠物起不来"，就等于让
/// 一次拔线干掉整个功能 —— 与硬约束 3（宠物崩溃/消失不许影响桌面）相邻的那条。
/// 反过来，**一台都没有**才是真出错：那时连舞台矩形都算不出，静默返回空表
/// 只会在下游炸成一句"矩形退化"，看不出根因在这里。
///
/// 为什么把坏的那台**打出来**而不是丢掉：本项目"沉默降级"那一族（`resolve_bones`
/// 缺尾骨、`from_wide` 不截断）的教训全是同一句 —— 症状出现在别处时，
/// 没有那一行就查不到根因在这里。
///
/// 顺序 = 系统枚举顺序（`EnumDisplayMonitors` 不保证按 x 排）。这不构成问题，
/// 因为选台走 [`crate::layout::choose`] 的"点名 > 主屏 > 第一台"三档，
/// 而 `--monitor` 那条路本来就不依赖顺序。
pub fn enumerate() -> Result<Vec<PhysMon>, String> {
    let mut out: Vec<PhysMon> = Vec::new();
    let mut skipped = 0usize;

    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(on_monitor),
            &mut Collector {
                out: &mut out,
                skipped: &mut skipped,
            } as *mut Collector as LPARAM,
        )
    };
    // `EnumDisplayMonitors` 返回 FALSE 也可能是"没有显示器"，所以这里不据它报错；
    // 真正的判据是手上那份表空不空（上面那段）。
    let _ = ok;

    if out.is_empty() {
        return Err(format!(
            "EnumDisplayMonitors 一台可用显示器都没给（回调跳过 {skipped} 台）\
             ⇒ 无法计算舞台"
        ));
    }
    if skipped > 0 {
        eprintln!(
            "monitors: 枚举到 {} 台，跳过 {skipped} 台（上面有逐台的原因）",
            out.len()
        );
    }
    Ok(out)
}

/// 回调的载荷。`LPARAM` 只能装一个指针，所以"跳过的台数"也得上车 ——
/// 它不能是一个 `static mut`（那是本文件里唯一会真炸的形状：回调在枚举线程上跑，
/// 而那个线程将来不一定是调 `enumerate` 的那个）。
struct Collector<'a> {
    out: &'a mut Vec<PhysMon>,
    skipped: &'a mut usize,
}

/// `MONITORENUMPROC`：返回 `TRUE` 继续枚举，`FALSE` 提前停。
///
/// ⚠️ **这一台失败时返回 `TRUE`（继续）** —— 返回 `FALSE` 会把整场枚举停掉，
/// 于是"跳过坏的那一台"变成"只拿到坏的那台之前的几台"，而拔掉的屏常常排在后面。
unsafe extern "system" fn on_monitor(
    hmonitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
    _hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    _rc_monitor: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let col = unsafe { &mut *(data as *mut Collector) };

    // `cbSize` 要的是**外层那个 EXW 结构**的大小，不是内层 `MONITORINFO` 的。
    // 写错的形状很具体：填 `size_of::<MONITORINFO>()` ⇒ `GetMonitorInfoW` 返回 FALSE
    // ⇒ 每一台都被当成坏屏跳过 ⇒ 空表 ⇒ 上面那句"一台都没有"。
    // 那一句话错、症状在另一句话上，所以这里把它当注释写下。
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..unsafe { std::mem::zeroed() }
        },
        szDevice: [0; 32],
    };
    if unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut MONITORINFO) } == 0 {
        eprintln!("monitors: 一台显示器的 GetMonitorInfoW 失败 ⇒ 跳过这一台");
        *col.skipped += 1;
        return 1;
    }

    let mon = info.monitorInfo;
    col.out.push(PhysMon {
        name: from_wide(&info.szDevice),
        left: mon.rcMonitor.left,
        top: mon.rcMonitor.top,
        right: mon.rcMonitor.right,
        bottom: mon.rcMonitor.bottom,
        work_top: mon.rcWork.top,
        work_bottom: mon.rcWork.bottom,
        os_primary: mon.dwFlags & MONITORINFOF_PRIMARY != 0,
    });
    1
}
