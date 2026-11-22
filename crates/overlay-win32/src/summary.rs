//! 退出摘要的**形状**，与两个进程计数采集器。
//!
//! ## 为什么格式化和采集要分开写、而且格式化必须能在宿主上跑
//!
//! 历轮验收报告靠 `grep` 三根 token 取数：`VmHWM:`、`VmRSS:`、`CPU 时间`。这三根是跨平台
//! 唯一的接口面 —— Wayland 那侧的出处是 `overlay-wayland/src/app.rs` 的 `report()` 与
//! `proc_self_summary()`。Windows 这侧如果只有采集器在真机上跑、格式没人验过，那么"数没变、
//! 标签变了"这件事的第一现场就是**用户拷回来一份 grep 不出来的 stdout**，而那一轮按
//! §6.1.22 的纪律已经作废了（`taskkill` 掉的轮次没有摘要 = 不是证据，标签错了同样不是证据）。
//! ⇒ [`lines`] 是纯函数，在本机就有测例；`#[cfg(windows)]` 的两个采集函数只负责填数。
//!
//! ## 两档 `0` 必须当场说清它是"结构上恒 0"还是"量到了 0"
//!
//! `延后` 与 `帧回调看门狗` 在 Wayland 读的是 SlotPool 的在飞 buffer 背压（`frame_pending`）：
//! 合成器不回 `frame` 回调就累积"延后"，看门狗数就是"我强制解锁了几次"。Windows 的
//! `UpdateLayeredWindow` 是**同步调用、没有回执** ⇒ 这两档构造不出非零值。把 `延后 0` 原样
//! 打出来会被读成"这一轮通过了一次延后测量"，而本项目对这一族的命名很明确：**零读数不是通过**
//! （§6.1.20⑮"读错通道的尺子"、§6.1.14"那条提示从未上屏"都是同一件事）。⇒ [`lines`] 在数字
//! 后面直接带一句 inline 说明，不留给读者猜。
//!
//! ## 同一族的第三种形状：**采集失败**不是 `0`，是 `n/a` + 原因
//!
//! [`Run::mem`] 与 [`Run::cpu_secs`] 是**第 8 步从裸值改成 `Option` 的**。原因很具体：
//! `collect_memory()` 一失败，裸值的默认就是全 0，摘要会打成 `VmHWM: 0 kB` —— 而
//! "这轮没吃内存"与"这台机器上没量到"在验收表上是两个相反的结论，前者还长得像一个通过的读数。
//! ⇒ 现在是 `Option` + 一句 `*_reason`，token 一个不少（grep 仍然命中），旁边写 `n/a`。
//!
//! ## `VmHWM` 在这个平台上不是同一条线
//!
//! `PeakWorkingSetSize` 与 Linux 的 `VmHWM` 语义差在**Windows 会在内存压力下修剪工作集**：
//! 被 trim 掉的页不算在 WorkingSet 里，所以 `WorkingSetSize` 低可能只是"被换出去了"，不是
//! "省下来了"。更近的对应物是 `PrivateUsage`（= `GetProcessMemoryInfo` 的 `PrivateWorkingSet`，
//! 不可共享的私有提交页）。⇒ 三个都打，并明写 ≤200 MiB 那条门禁判在哪个上。这条口径唯一
//! 出处 = 设计书 §6.1.20①（11-10 拍板放宽那一格），历史读数一律不改写。

use overlay_common::accounting::Counters;

/// 本片没链进来的那几块面的统一说法。
///
/// ⚠️ 不能省成"什么都不打"：`菜单/面板/字体` 那三行在 Wayland 是有数的，Windows 上不打
/// 会被读成"数为 0"，而 0 与"没有这块面"是两个完全不同的结论（同一族那句"不要把'没跑到'
/// 读成'通过'"）。
pub const NOT_BUILT: &str = "本构建未接（M5 最小片）";

/// 三个内存读数，单位**字节**（KB 换算留给 [`memory_line`]，这样采集器不必猜口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Memory {
    /// `PROCESS_MEMORY_COUNTERS.PeakWorkingSetSize`
    pub peak_working_set: u64,
    /// `PROCESS_MEMORY_COUNTERS.WorkingSetSize`
    pub working_set: u64,
    /// `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`
    pub private_working_set: u64,
}

/// 一轮运行的全部摘要输入。
#[derive(Clone, Copy, Debug)]
pub struct Run<'a> {
    pub ticks: u64,
    pub wall_secs: f64,
    pub counters: &'a Counters,
    /// surface 的逻辑尺寸（= 图集格 + 2×margin，来自 `crate::layout::Frame`）。
    pub surface_logical: (u32, u32),
    /// 窗口物理尺寸 = buffer 尺寸，两者必须逐像素相等（`crate::layout::Frame::surface_phys`）。
    pub buffer_physical: (i32, i32),
    pub scale: f32,
    /// DPI 走的是哪条路，**必须**打印（`crate::window::DpiPath` 的 `as_str`）。
    pub dpi_path: &'a str,
    /// `Some((state 的 Debug, 指纹))`；`None` = `--test-rect` 那一档没接内核。
    pub kernel: Option<(&'a str, u64)>,
    /// 采集**成没成**。`None` 时 [`Run::mem_reason`] 必须给原因。
    ///
    /// ⚠️ 为什么不是 `Memory` 裸值：采集器失败时一个默认的 `Memory::default()` 会打成
    /// `VmHWM: 0 kB`，而"这轮吃了 0 字节"与"这台机器上没量到"在验收表上是两个相反的结论
    /// （同族那条红线 = §6.1.20⑮"读错通道的尺子"、以及本项目一直守的"没有不写成 0"）。
    pub mem: Option<Memory>,
    pub mem_reason: Option<&'a str>,
    pub cpu_secs: Option<f64>,
    pub cpu_reason: Option<&'a str>,
}

/// 摘要正文，一行一个元素。
///
/// 前三行的 token 与 Wayland `report()` 逐字对齐（`overlay:` / `  核对: 五档合计` /
/// `  surface 逻辑`），差异只在两处：**被 inline 标注的恒 0 档**与**没链进来的那三行**。
pub fn lines(run: &Run) -> Vec<String> {
    let c = run.counters;
    // 三档占比的分母：与 Wayland 同一个式子（提交 + 仅移动 + 无事，max 1 防零除）。
    let t = (c.commits + c.moves + c.idle).max(1) as f64;
    let pct = |n: u64| 100.0 * n as f64 / t;

    let mut out = vec![format!(
        "  ui: {NOT_BUILT} —— 无卡片/面板/字体/配件/库存 ⇒ 本片的 UI 只有宠物那几个像素"
    )];

    out.push(format!(
        "overlay: {} tick / {:.1}s 墙钟；提交 {}（{:.1}%）、仅移动 {}（{:.1}%）、无事 {}（{:.1}%）、延后 {}、buffer 错误 {}、帧回调看门狗 {}（{}）",
        run.ticks,
        run.wall_secs.max(0.001),
        c.commits,
        pct(c.commits),
        c.moves,
        pct(c.moves),
        c.idle,
        pct(c.idle),
        c.deferred,
        c.buffer_errors,
        c.watchdogs,
        INLINE_STRUCTURAL_ZERO,
    ));

    let five = c.tick_accounted();
    out.push(format!(
        "  核对: 五档合计 {} vs tick {} ⇒ {}｜非 tick 补画: 帧回调补投 {}（结构上恒 0，见上） / DPI 变更补画 {}",
        five,
        run.ticks,
        if five == run.ticks {
            "OK（互斥且齐）"
        } else {
            "不一致：有 tick 没归进任何一档，或某档重复计数"
        },
        c.replay_commits,
        c.configure_commits,
    ));

    out.push(format!(
        "  surface 逻辑 {}x{} ⇒ 窗口/buffer {}x{}（scale {:.2}×，DPI 来源 {}）",
        run.surface_logical.0,
        run.surface_logical.1,
        run.buffer_physical.0,
        run.buffer_physical.1,
        run.scale,
        run.dpi_path,
    ));

    // 这三行是"这块面不存在"，不是"这块面省电"。
    for which in ["菜单", "面板", "字体"] {
        out.push(format!("  {which}: {NOT_BUILT}"));
    }

    if let Some((state, fp)) = run.kernel {
        out.push(format!("  内核: state={state} 指纹={fp:016x}"));
    }

    out.push(format!("  {}", memory_line(run)));
    out
}

/// 摘要里那行 inline 说明。写成常量是为了让测例断的是**这一句**，不是"有没有括号"。
pub const INLINE_STRUCTURAL_ZERO: &str =
    "后两档在本平台结构上恒 0：UpdateLayeredWindow 是同步调用、没有在飞帧回调";

/// `VmHWM:` / `VmRSS:` / `CPU 时间` 三根 token + 私有工作集 + 口径说明。
///
/// 前两行的标签**故意**沿用 Linux 的名字：验收脚本 grep 的是它们。真实的 Windows 字段名
/// 在括号里给出，免得有人把 `VmHWM` 读成 `/proc` 里那个内核高水位。
///
/// ⚠️ **采集失败时三根 token 照样在，只是旁边是 `n/a（原因）而不是数**。写成"整行消失"
/// 会让 grep 静默失配（那一轮被读成"脚本坏了"），写成"0"会被读成"这轮没吃内存"，
/// 而后者是这一族里唯一会让人**按错误结论签收**的那一种。
pub fn memory_line(run: &Run) -> String {
    let kb = |n: u64| n / 1024;
    let mem = match run.mem {
        Some(m) => format!(
            "VmHWM: {} kB（PeakWorkingSetSize）  VmRSS: {} kB（WorkingSetSize）  PrivateWorkingSet: {} kB",
            kb(m.peak_working_set),
            kb(m.working_set),
            kb(m.private_working_set),
        ),
        None => format!(
            "VmHWM: {}  VmRSS: {}  PrivateWorkingSet: {}",
            unavailable(run.mem_reason),
            unavailable(run.mem_reason),
            unavailable(run.mem_reason),
        ),
    };
    let cpu = match run.cpu_secs {
        Some(s) => format!("CPU 时间 {s:.2}s（GetProcessTimes user+kernel）"),
        None => format!("CPU 时间 {}", unavailable(run.cpu_reason)),
    };
    format!(
        "{mem}  {cpu}｜≤200 MiB 那条门禁判在 PrivateWorkingSet 上：Windows 会在内存压力下修剪工作集 ⇒ Peak/Working 两个数是会被换出去的页也算在外的口径"
    )
}

/// `n/a` 那一种写法：有原因就把原因带上，一行之内说清"为什么这个数不在"。
fn unavailable(reason: Option<&str>) -> String {
    match reason {
        Some(r) => format!("n/a（采集失败：{r}）"),
        None => "n/a（这一轮没有采集）".to_string(),
    }
}

// ───────────────────────────── 采集器（只有 Windows 上有实现）─────────────────────────────

/// 当前进程的伪句柄。
///
/// C 那边 `GetCurrentProcess()` 就是这个宏的展开（`(HANDLE)-1`），而 `windows-sys` 不导出
/// 宏 ⇒ 只能自己写。**不要**对它 `CloseHandle`（伪句柄不拥有内核对象）。
#[cfg(windows)]
const THIS_PROCESS: windows_sys::Win32::Foundation::HANDLE = -1isize as _;

/// `K32GetProcessMemoryInfo` + `PROCESS_MEMORY_COUNTERS_EX`。
///
/// 取 `PrivateUsage` 要的是 **EX** 那份结构（基础那份没有私有提交那一格）⇒ `cb` 必须填
/// `size_of::<EX>()`，填成基础结构的 size 会让 API 直接失败（`ERROR_BAD_PARAMETER`），
/// 而失败在这里的症状是"摘要里三个数全 0"，跟"这轮没吃内存"长得一模一样。
/// ⇒ 采集失败必须**报出来**，见调用方。
#[cfg(windows)]
pub fn collect_memory() -> Result<Memory, String> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: core::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..unsafe { core::mem::zeroed() }
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(THIS_PROCESS, (&raw mut counters) as *mut _, counters.cb)
    };
    if ok == 0 {
        return Err(format!(
            "K32GetProcessMemoryInfo 失败（cb={}）⇒ 摘要里的三个内存数不可用，别读成 0",
            counters.cb
        ));
    }
    Ok(Memory {
        peak_working_set: counters.PeakWorkingSetSize as u64,
        working_set: counters.WorkingSetSize as u64,
        private_working_set: counters.PrivateUsage as u64,
    })
}

/// `(user + kernel) / 1e7` 秒 —— 与 `/proc/self/stat` 的 `utime+stime`（ticks/USER_HZ=100）
/// 同一口径：**进程累计 CPU**，不是墙钟占比。历轮"空闲 ≤1.5%、活跃 ≤5%"那两档就是这么算的，
/// 所以这里也必须打**秒数**再让验收方除以墙钟，别在客户端里改成百分比（那就是第二个事实源）。
#[cfg(windows)]
pub fn collect_cpu_secs() -> Result<f64, String> {
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let mut creation = core::mem::MaybeUninit::uninit();
    let mut exit = core::mem::MaybeUninit::uninit();
    let mut kernel = core::mem::MaybeUninit::uninit();
    let mut user = core::mem::MaybeUninit::uninit();
    let ok = unsafe {
        GetProcessTimes(
            THIS_PROCESS,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return Err("GetProcessTimes 失败 ⇒ CPU 时间不可用，别读成 0".to_string());
    }
    let to_100ns = |ft: windows_sys::Win32::Foundation::FILETIME| {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    };
    let k = unsafe { to_100ns(kernel.assume_init()) };
    let u = unsafe { to_100ns(user.assume_init()) };
    Ok((k + u) as f64 / 1e7)
}

/// 宿主上这两个采集器不存在（没有那些 API），但摘要**格式**必须照样能测 ⇒ 给一份可填的零值。
#[cfg(not(windows))]
pub fn collect_memory() -> Result<Memory, String> {
    Err("本构建目标不是 Windows：没有 K32GetProcessMemoryInfo".to_string())
}

#[cfg(not(windows))]
pub fn collect_cpu_secs() -> Result<f64, String> {
    Err("本构建目标不是 Windows：没有 GetProcessTimes".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters_with(c: &mut Counters, commits: u64, moves: u64, idle: u64) {
        c.commits = commits;
        c.moves = moves;
        c.idle = idle;
    }

    /// 按**行首**取那一行，不按下标。
    ///
    /// 下标断言会在有人往摘要里插一行时红成"测试坏了"，而这里想读的从来不是"第几行"，
    /// 是"overlay 那一行的内容"。前缀就是历轮报告 grep 用的那个前缀 ⇒ 用它，测例与验收
    /// 读的才是同一个东西。
    fn line_with(all: &[String], prefix: &str) -> String {
        all.iter()
            .find(|l| l.trim_start().starts_with(prefix))
            .unwrap_or_else(|| {
                panic!(
                    "摘要里没有以 {prefix:?} 起头的行；全部=\n{}",
                    all.join("\n")
                )
            })
            .clone()
    }

    fn run<'a>(c: &'a Counters) -> Run<'a> {
        Run {
            ticks: 0,
            wall_secs: 300.0,
            counters: c,
            surface_logical: (224, 224),
            buffer_physical: (224, 224),
            scale: 1.0,
            dpi_path: "test",
            kernel: Some(("Idle", 0xea97_1392_0553_6cd8)),
            mem: Some(Memory {
                peak_working_set: 107_092 * 1024,
                working_set: 90_000 * 1024,
                private_working_set: 88_000 * 1024,
            }),
            mem_reason: None,
            cpu_secs: Some(7.17),
            cpu_reason: None,
        }
    }

    /// 那三根 token 是本项目的接口面：历轮报告靠 grep 它们取数。
    ///
    /// 断在**内容**而不是断在"某行存在"：`VmHWM` 少一个冒号，grep 就静默失配而摘要看起来
    /// 完全正常 —— 这是"沉默降级"那一族在仪器侧的版本。
    #[test]
    fn the_three_grep_tokens_are_literal() {
        let c = Counters::default();
        let joined = lines(&run(&c)).join("\n");
        for token in ["VmHWM:", "VmRSS:", "CPU 时间"] {
            assert!(
                joined.contains(token),
                "摘要少了 grep token {token:?}；验收脚本读的是这三根 ⇒ 现在它静默失配。\n摘要={joined}"
            );
        }
    }

    /// 每个数旁边还得是它自己那格，否则 token 在而数错 = 更坏。
    #[test]
    fn the_number_next_to_each_token_is_the_field_it_claims() {
        let c = Counters::default();
        let line = memory_line(&run(&c));
        assert!(line.contains("VmHWM: 107092 kB"), "{line}");
        assert!(line.contains("VmRSS: 90000 kB"), "{line}");
        assert!(line.contains("PrivateWorkingSet: 88000 kB"), "{line}");
        assert!(line.contains("CPU 时间 7.17s"), "{line}");
        // 107,092 kB 这个数不是随便挑的：它是 §6.1.20① 那批带配件真机轮的 VmHWM 读数，
        // 拿它当样本，"字节→kB 除了 1024"这件事在测例里就是一个可对照的真值。
        assert!(line.contains("门禁判在 PrivateWorkingSet"), "{line}");
    }

    /// `延后` 与 `看门狗` 的 0 必须**当场**带着"结构上恒 0"那句话。
    ///
    /// 这条测例断的是 inline 说明那一句原文。若有人想把它挪进文档，测例红 ⇒ 那是设计要的红：
    /// 读数是就地读的，说明必须与数同一行。
    #[test]
    fn structurally_zero_tiers_are_marked_in_line_not_just_printed() {
        let c = Counters {
            commits: 10,
            ..Default::default()
        };
        let out = lines(&run(&c));
        let line = line_with(&out, "overlay:");
        assert!(line.contains("延后 0"), "{line}");
        assert!(line.contains("帧回调看门狗 0"), "{line}");
        assert!(line.contains(INLINE_STRUCTURAL_ZERO), "{line}");
        // 补投那一档在"核对"行里同样标了，两处都要在。
        assert!(line_with(&out, "核对:").contains("结构上恒 0"));
    }

    /// 没链进来的三块面：一行一个，且**不是**空行也不是"0"。
    #[test]
    fn absent_surfaces_say_absent_three_times() {
        let c = Counters::default();
        let all = lines(&run(&c)).join("\n");
        assert_eq!(all.matches(NOT_BUILT).count(), 4, "{all}"); // ui + 菜单 + 面板 + 字体
        for which in ["菜单:", "面板:", "字体:"] {
            assert!(all.contains(which), "缺 {which} 那一行");
        }
        assert!(
            !all.contains("菜单: 0"),
            "菜单被打成了数 ⇒ 会被读成'量过且省'"
        );
    }

    /// 占比的分母是三档之和，不是 tick 总数（与 Wayland 同式）。
    ///
    /// 用 10/5/5 而不是任意数：三者之和 20 ⇒ 50.0/25.0/25.0 三个读数互不相同，
    /// 任何一处把分母写成 ticks 或把档写错，三个数同时不成立。
    #[test]
    fn the_three_percentages_come_from_the_three_tier_denominator() {
        let mut c = Counters::default();
        counters_with(&mut c, 10, 5, 5);
        let mut r = run(&c);
        r.ticks = 100; // tick 总数与三档之和**不等** ⇒ 分母若误用 ticks，占比会变成 10/5/5%
        let line = line_with(&lines(&r), "overlay:");
        assert!(line.contains("提交 10（50.0%）"), "{line}");
        assert!(line.contains("仅移动 5（25.0%）"), "{line}");
        assert!(line.contains("无事 5（25.0%）"), "{line}");
    }

    /// 失败形状要**整份摘要**都拿得到，不只那一行：`report()` 打的是 `lines()`，
    /// 而 `lines()` 有一支自己拼内存行的旧写法的可能（历史上有过）。只测 `memory_line`
    /// 的话，`lines()` 里换成一份手写默认值这条漂移没人拦。
    #[test]
    fn the_failed_collector_shape_survives_the_whole_summary() {
        let c = Counters::default();
        let mut r = run(&c);
        r.mem = None;
        r.mem_reason = Some("模拟失败");
        r.cpu_secs = None;
        r.cpu_reason = Some("模拟失败");
        let all = lines(&r).join("\n");
        assert!(all.contains("VmHWM: n/a（采集失败：模拟失败）"), "{all}");
        assert!(
            !all.contains(" 0 kB"),
            "摘要里出现了 0 kB ⇒ 失败被打成了读数: {all}"
        );
    }

    #[test]
    fn zero_counters_does_not_divide_by_zero() {
        let c = Counters::default();
        let line = line_with(&lines(&run(&c)), "overlay:");
        assert!(line.contains("提交 0（0.0%）"), "{line}");
    }

    /// 五档 vs tick 那句结论要在**两种**情况下各打对。
    ///
    /// 不一致那一支必须真能构造出来：本片非 tick 补画只有 DPI 变更一档，所以"tick 有数而
    /// 五档全 0"是真实可能出现的形状（present 全部早退），而它恰恰是最该被读成 bug 的形状。
    #[test]
    fn the_reconciliation_line_names_both_verdicts() {
        let c = Counters::default();
        let ok = run(&c);
        assert!(
            line_with(&lines(&ok), "核对:").contains("OK（互斥且齐）"),
            "ticks=0 且五档=0 ⇒ 应当判齐"
        );
        let mut bad = run(&c);
        bad.ticks = 18_000;
        assert_eq!(bad.counters.tick_accounted(), 0, "构造不出不一致");
        let text = lines(&bad).join("\n");
        assert!(text.contains("不一致：有 tick 没归进任何一档"), "{text}");
    }

    #[test]
    fn the_kernel_line_is_absent_when_the_kernel_is_not_wired() {
        let c = Counters::default();
        let mut r = run(&c);
        r.kernel = None;
        let all = lines(&r).join("\n");
        assert!(!all.contains("内核: state="), "{all}");
        assert!(all.contains(NOT_BUILT), "{all}");
    }

    /// `--test-rect` 那一档不接内核 ⇒ 指纹那一行**整行消失**，而不是打成 `0000…`。
    ///
    /// 后者会被 grep 成"这轮指纹是 0 ⇒ 有问题"，而真相是"这一档没有 Sim"。
    /// 与上面同一族的纪律：**没有**不能写成 **0**。
    #[test]
    fn no_kernel_drops_the_line_instead_of_printing_a_zero_fingerprint() {
        let c = Counters::default();
        let mut r = run(&c);
        r.kernel = None;
        let all = lines(&r).join("\n");
        assert!(!all.contains("0000000000000000"), "{all}");
        r.kernel = Some(("Idle", 0));
        assert!(
            line_with(&lines(&r), "内核:").contains("指纹=0000000000000000"),
            "显式给了指纹 0 时那一行要在，且数就是 0"
        );
    }

    /// 采集失败的那一种形状：**三根 token 全在，旁边是 `n/a` 与原因，而不是 `0`**。
    ///
    /// 这条测例是 M5 第 8 步把 `Run::mem` / `Run::cpu_secs` 从裸值改成 `Option` 的理由本身。
    /// 改之前，`collect_memory()` 一失败就会打出 `VmHWM: 0 kB` —— 而验收表上"这轮没吃内存"
    /// 与"这台机器上没量到"是两个相反的结论，且前者看起来完全像一个通过的读数。
    #[test]
    fn a_failed_collector_prints_n_a_with_the_reason_not_a_zero() {
        let c = Counters::default();
        let mut r = run(&c);
        r.mem = None;
        r.mem_reason = Some("K32GetProcessMemoryInfo 失败（cb=88）");
        r.cpu_secs = None;
        r.cpu_reason = Some("GetProcessTimes 失败");
        let line = memory_line(&r);
        for token in ["VmHWM:", "VmRSS:", "PrivateWorkingSet:", "CPU 时间"] {
            assert!(
                line.contains(token),
                "token {token:?} 掉了 ⇒ grep 静默失配: {line}"
            );
        }
        assert!(
            line.contains("n/a（采集失败：K32GetProcessMemoryInfo 失败（cb=88））"),
            "{line}"
        );
        assert!(
            line.contains("n/a（采集失败：GetProcessTimes 失败）"),
            "{line}"
        );
        assert!(
            !line.contains("VmHWM: 0 kB"),
            "失败被打成了 0 ⇒ 会被读成一个通过的读数: {line}"
        );
        // 没有原因可给时也要说话，不许留空格。
        let mut bare = r;
        bare.mem_reason = None;
        assert!(
            memory_line(&bare).contains("VmHWM: n/a（这一轮没有采集）"),
            "{bare:?}"
        );
    }

    /// `dpi_path` 不许被打成"看起来成功"的默认值。
    ///
    /// 这条断的是"走了哪条 DPI 路"必须出现在摘要里（硬坑清单第 1 条：已登记的降级 ≠ 静默的降级）。
    /// 用一份写着 fallback 的 Run 验它真被摊到屏上。
    #[test]
    fn the_dpi_path_is_printed_whichever_branch_was_taken() {
        let c = Counters::default();
        let mut r = run(&c);
        r.dpi_path = "GetDpiForSystem 退回路（V2 上下文设置失败）";
        let line = line_with(&lines(&r), "surface 逻辑");
        assert!(line.contains(r.dpi_path), "{line}");
    }
}
