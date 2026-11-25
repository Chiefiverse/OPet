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

/// 曾经用于"这块面还没链进来"的统一说法。**M7b 起摘要里没有任何一行打它了**（面板行是最后
/// 一个使用者，它毕业了），留着只有一个用途：测例反向钉住"**一个字都不许再打它**"。
///
/// ⚠️ 反向断言不是形式主义。这一族的原文是"本构建未接（**M5 最小片**）"，而 M5 最小片这个
/// 说法在 M7 之后已经是假话 ⇒ 以后哪一块面真的掉了，正确处置是**改 `ui:` 那行的未接清单**
/// 并写明是哪一批欠的（M7f 起那半是**空的**，四族都在已接里，所以新增一欠项要连着改那句
/// "这一行没有欠项"），不是把这行僵尸 token 请回来。
/// 断言 count == 0 就是把"请回来"变成一次红。
///
/// 从这个族**毕业**的四行：`ui:`（M5e 起改打"已接/未接"两张清单，M6 卡面、M7b 面板进已接那半）、
/// `菜单`（M5c 起打状态 + `menu_commits` 真数）、`字体`（M5e 起打 [`Font`] 那五个实测数 ——
/// 菜单文字从 M5c 那一刻就真在走 `overlay_common::text`，那一行当时就该是数）与
/// `面板`（M7b 起打状态 + `panel_commits` 真数；本平台无帧回调 ⇒ 看门狗那一档结构性恒 0，
/// 与 `Deferred`/`Replay` 同一句平台理由，所以它**不**打成 0，打的是说明）。
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

/// 字体那一层的五个数，**由调用方在退出那一刻取好后交进来**。
///
/// ⚠️ 为什么不在 [`lines`] 里现取：`face`/缓存/命中数全是进程全局，而 `lines()` 是宿主测例
/// 的固定输入 → 固定输出。让它读全局 ⇒ 同一份摘要会随"这个二进制里先跑了哪条测例"变，
/// 而 `the_three_grep_tokens_are_literal` 那一条的红必须是内容红、不是竞态红。
/// 采集只有一个入口 = [`Font::collect`]，由 `app.rs` 的 `report()` 调。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Font {
    /// `true` = 子集字体加载成功；`false` = 降级 5×7 点阵（与 Wayland 同一句判词）。
    pub face_real: bool,
    /// 字形缓存 arena 当前占用（字节）。
    pub cache_bytes: usize,
    /// 命中 / 栅格化 / 未缓存 三档，原文与 Wayland 的 `text::cache_stats()` 对齐。
    pub hits: usize,
    pub made: usize,
    pub uncached: usize,
}

impl Font {
    /// 从 `overlay_common::text` 的全局里取当下这一格。**只在退出摘要前调一次**。
    pub fn collect() -> Self {
        let (hits, made, uncached) = overlay_common::text::cache_stats();
        Self {
            face_real: overlay_common::text::face().is_some(),
            cache_bytes: overlay_common::text::cache_bytes(),
            hits,
            made,
            uncached,
        }
    }
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
    /// 退出那一刻菜单面开没开着（M5c 接线；提交数在 `counters.menu_commits`，这一格只是状态）。
    pub menu_open: bool,
    /// 退出那一刻面板开没开着（M7b 接线；提交数在 `counters.panel_commits`）。
    ///
    /// ⚠️ 它与 `menu_open` **不会同时为真**（互斥由 `App::apply_panel` 落地那一刻关菜单保证）。
    /// 摘要不打"互斥"这一格断言：`lines()` 没有能力拒绝一对 `true`，而那条不变量属于
    /// `app.rs` 那一侧（源级门禁 + Wine 目视），这里只是把两个状态各自说清楚。
    pub panel_open: bool,
    /// 字体那一层的五个数（M5e 接线；采集只在 [`Font::collect`]，理由写在那儿的注释里）。
    pub font: Font,
    /// 生效值那一行的正文 = `overlay_common::settings::Settings::summary()` 的返回值
    /// （它自己以 `ui: ` 起头，本函数只补那两个前导空格）。M7e 接线。
    ///
    /// 为什么把**整行**交进来而不是 `lang` / `motion` 两个值：那一行的格式、来源标签、
    /// `｜` 分隔符都是共用层的事实，在这里重排一遍就有了第二个格式事实源（两家的摘要
    /// 迟早长得不一样，而验收脚本 grep 的是同一个 token）。
    ///
    /// ⚠️ 于是摘要里有**两行以 `ui:` 起头**（这一行与 [`lines`] 开头那条"已接/未接"清单）。
    /// 读它的脚本与测例都要按更长的前缀取（`ui: lang =` / `ui: 已接`），只写 `ui:` 会命中
    /// 清单那一行 —— 静默读到错的那一行，是"读错通道的尺子"那一族。Wayland 那边只有一行
    /// （它没有已接/未接清单，那半张表是本平台随 M5 第一批长出来的）。
    pub ui_effective: &'a str,
}

/// 摘要正文，一行一个元素。
///
/// 前三行的 token 与 Wayland `report()` 逐字对齐（`overlay:` / `  核对: 五档合计` /
/// `  surface 逻辑`），差异只在**被 inline 标注的恒 0 档**（`Deferred`/`Replay`/两面看门狗）。
/// `菜单`、`字体`、`面板` 三行从 M5c / M5e / M7b 起与 Wayland 一样是真数 —— 它们共用
/// `overlay_common`，本来就没有"这平台量不出"这一说。
pub fn lines(run: &Run) -> Vec<String> {
    let c = run.counters;
    // 三档占比的分母：与 Wayland 同一个式子（提交 + 仅移动 + 无事，max 1 防零除）。
    let t = (c.commits + c.moves + c.idle).max(1) as f64;
    let pct = |n: u64| 100.0 * n as f64 / t;

    // `ui:` 从 M5e 起不再借用 [`NOT_BUILT`]：菜单面与换装都接完了，那一顶帽子就在屏上戴着，
    // 而"本构建未接"会让读表的人以为这一轮什么 UI 都没测到。M6 起卡面、M7b 起面板、M7c 起点名、
    // M7d 起换本体、M7e 起配置链都进"已接"。
    // ⚠️ **M7e 是这一行第三次换轴、M7f 是第四次**：第一次（M7b）从"面"换到"面上的格"，第二次
    // （M7d）从"格"换到"开关"，第三次（M7e）语言/动效随配置链毕业 ⇒ 只剩 `--probe`，第四次
    // （**M7f**）= `--probe` 自己毕业 ⇒ **未接那半第一次是空的**。
    // 每换一次轴都要把**两半一起重写**：只改未接那半，读者会拿"已接"那半的旧清单去对功能表，
    // 而那条清单已经少了一整族（面板毕业那一轮踩的就是这个）。
    // ⚠️ 空清单不等于这行没用了：它现在钉的是**另一种**东西 —— 欠项从"没接上"换成了"没量过"，
    // 而后者不归这一行管（`AGENTS.md`：Wine 判不了的格子不许当通过，真机那一轮仍欠）。
    // 把"未接 = 0"读成"全部通过"正是这一行最容易被误读的地方，所以那句话就写在这行里。
    let mut out = vec![
        "  ui: 已接 = 宠物像素 + 菜单面（M5c）+ 换装（M5e）+ 卡面（M6）+ 面板面（M7b）+ 收藏册点名（M7c）+ 换本体（M7d）+ 配置链（M7e）+ --probe 只读诊断（M7f）｜未接 = 面 / 格 / 开关 / 诊断旗四族都已清零 ⇒ 这一行没有欠项；还欠的这批读数是**没在 Windows 真机与 Wine 上量过**（M7g），那不是「没接上」"
            .to_string(),
    ];
    // 生效值那一行排在清单之后、`overlay:` 之前 = 与 Wayland `report()` 同一个位置（摘要开头）。
    // 它必须在**退出摘要**里再打一遍：启动那一行早就滚出屏幕了，而验收读的是 20 s/300 s 那一轮
    // 末尾的数（"我 `--lang=en` 起的到底生效没有"要能在同一份日志里答）。
    out.push(format!("  {}", run.ui_effective));

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

    // 菜单与面板两行从 M5c / M7b 起都是**真数**（与 Wayland 的 `菜单: N 提交`、
    // `面板: N 提交 / N 看门狗` 同族同位）。两行的提交数**必须分开读**：共用一个计数就等于
    // "面板静止时看不出它有没有在偷投帧"。看门狗那一档写死在说明里而不是打 0：
    // 本平台没有在飞帧回调，那个数与宠物那侧"帧回调看门狗"同一句结构性理由。
    out.push(format!(
        "  菜单: {}（{} 提交，静止时应当恒 0 增长；本平台无帧回调 ⇒ 看门狗结构上恒 0）",
        if run.menu_open { "开着" } else { "没开" },
        c.menu_commits,
    ));
    // 字体这行从 M5e 起是**真数**（与 Wayland `report()` 同一句式、同一批 token）。两个平台
    // 共用 `overlay_common::text` ⇒ 命中/栅格化那两个数本来就该由同一份全局出来，这里只是
    // 第一次把它打出来。缓存上限是编译期常量，两边读的是同一个数。
    out.push(format!(
        "  字体: face={} 资产 {} KiB / 字形缓存 {} B（上限 {} B，命中 {}，栅格化 {}，未缓存 {}）",
        if run.font.face_real {
            "真字体"
        } else {
            "降级 5×7 点阵"
        },
        overlay_common::text::FONT_BYTES.len() / 1024,
        run.font.cache_bytes,
        overlay_common::text::CACHE_MAX_BYTES,
        run.font.hits,
        run.font.made,
        run.font.uncached,
    ));
    out.push(format!(
        "  面板: {}（{} 提交，静止时应当恒 0 增长；本平台无帧回调 ⇒ 看门狗结构上恒 0）",
        if run.panel_open { "开着" } else { "没开" },
        c.panel_commits,
    ));

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
            menu_open: false,
            panel_open: false,
            font: Font::default(),
            // 样本行 = `Settings::summary()` 那个格式（共用层的事实，这里只借它的形状）。
            ui_effective: "ui: lang = en（默认，自名 English）｜ motion = full（默认）",
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

    /// 摘要里**没有任何一行再说"未接"**：四行全部打真数（`ui:` 打两张清单、`菜单`/`面板` 打
    /// 状态 + 提交数、`字体` 打 [`Font`] 那五个数）。
    ///
    /// 它们从"未接"变成"有数"是同一件事的两面 —— 菜单文字**早就**在走 `overlay_common::text`，
    /// 那一行写着"未接"是错的，不是保守；面板同理，M7b 那块面一上屏，`面板: 本构建未接`
    /// 就从"保守"变成"假话"。
    ///
    /// ⚠️ `ui:` 那一行的断法是**双向**的，而且**按 `｜` 切成两半再断**：卡面从 M6 起毕业后，
    /// `卡面（M6）`这个 token 还在同一行里（挪进了"已接"那半）⇒ 拿整行 `contains` 会把"未接
    /// 清单被并进已接"这种改写读成通过。两半各查一遍，且已接的不许同时出现在未接那半。
    /// 只断"不含未接"还会放过最坏的那种改写 —— 把整行删掉。
    ///
    /// ⚠️ M7e 起取那一行**必须用长前缀 `ui: 已接`**：摘要里现在有两行以 `ui:` 起头（另一行是
    /// 生效值），只写 `ui:` 会靠"谁排在前面"命中 —— 顺序一变，这条测例读的就不是它想读的那行。
    ///
    /// ⚠️ 字体那五个数断的是**交进来的那五个**，不是"某处有数"：全局采集一旦混进 `lines()`，
    /// 同一份摘要会随测例执行顺序变，而这条测例的红必须长得很像"标签写错了"而不是"运气不好"。
    #[test]
    fn absent_surfaces_say_absent_while_the_graduated_ones_say_numbers() {
        let c = Counters::default();
        let out = lines(&run(&c));
        let all = out.join("\n");
        // M7b 起这一族在摘要里没有使用者：打它 = 说"这块面不存在"，而四块面都在。
        assert_eq!(all.matches(NOT_BUILT).count(), 0, "{all}");
        let ui = line_with(&out, "ui: 已接");
        let (built_half, missing_half) = ui
            .split_once("｜")
            .expect("ui: 必须是「已接 …｜未接 …」两段 ⇒ 少一半就没有可读的清单");
        for built in [
            "宠物像素",
            "菜单面（M5c）",
            "换装（M5e）",
            "卡面（M6）",
            "面板面（M7b）",
            "收藏册点名（M7c）",
            "换本体（M7d）",
            "配置链（M7e）",
            "--probe 只读诊断（M7f）",
        ] {
            assert!(
                built_half.contains(built),
                "已接的那块面被漏报：{built}\n{ui}"
            );
            assert!(
                !missing_half.contains(built),
                "同一块面同时出现在未接那半 ⇒ 清单自相矛盾：{built}\n{ui}"
            );
        }
        // 未接那半换过**四**次轴：M7b = "哪块面不存在" → "面上的哪一格还欠"；M7d = 那一格也接
        // 完了 ⇒ 说的是**开关**；M7e = 语言/动效随配置链毕业 ⇒ 只剩 `--probe`；M7f = 它自己毕业
        // ⇒ 那半第一次没有欠项。
        // ⚠️ 于是这一族的断言**方向翻了**：M7e 之前是"`--probe` 必须出现在未接"，现在是"必须
        // 出现在已接、且不许出现在未接"。写法仍然留成列表（而不是摊平一句 `assert!`），因为这张
        // 表还会再长；`for x in ["--probe"]` 会被 clippy 判单元素循环 ⇒ 先落成变量再遍历。
        let graduated_switches = ["--probe"];
        for done in graduated_switches {
            assert!(
                built_half.contains(done),
                "已接的那条开关被说成未接：{done}\n{ui}"
            );
            assert!(
                !missing_half.contains(done),
                "毕业的东西还留在未接那半：{done}\n{ui}"
            );
        }
        // 空欠项必须**自己说它是空的**：只断"未接那半不含旗名"会放过最坏的那一种改写 ——
        // 把未接那半整段删掉（`｜` 少一半由上面那条 expect 挡，整段话术没了则由这一条挡）。
        assert!(missing_half.contains("没有欠项"), "{ui}");
        assert!(missing_half.contains("没在 Windows 真机"), "{ui}");
        // ── 面板行：两个状态 + 提交数 + 那句结构性说明，四个断语各自独立 ──
        let panel = line_with(&out, "面板:");
        assert!(panel.contains("没开"), "{panel}");
        assert!(panel.contains("0 提交"), "{panel}");
        assert!(
            panel.contains("看门狗结构上恒 0"),
            "面板行的看门狗那一档不能只打 0（那是平台形状，不是测量结果）：{panel}"
        );
        let menu = line_with(&out, "菜单:");
        assert!(menu.contains("没开"), "{menu}");
        assert!(menu.contains("0 提交"), "{menu}");
        // "开着"那一档由 `menu_open`/`panel_open` 真值驱动，不是把数打上去就算数：
        let c2 = Counters {
            menu_commits: 7,
            panel_commits: 3,
            ..Default::default()
        };
        let mut r = run(&c2);
        r.menu_open = true;
        let menu = line_with(&lines(&r), "菜单:");
        assert!(menu.contains("开着") && menu.contains("7 提交"), "{menu}");
        // 面板**单独**驱动一次：两行的提交数必须来自两个计数器（共用一个的话，这里会读到
        // `7 提交`，而 `菜单` 行那一条断言照样绿 —— 那正是"两块面互相替对方放行"的形状）。
        let mut r = run(&c2);
        r.panel_open = true;
        let panel = line_with(&lines(&r), "面板:");
        let menu = line_with(&lines(&r), "菜单:");
        assert!(
            panel.contains("开着") && panel.contains("3 提交"),
            "{panel}"
        );
        assert!(menu.contains("没开") && menu.contains("7 提交"), "{menu}");

        // ── 字体行：两个 face 分支 + 五个数各自到位 ──
        let mut r = run(&c);
        r.font = Font {
            face_real: true,
            cache_bytes: 4096,
            hits: 61,
            made: 12,
            uncached: 3,
        };
        let font = line_with(&lines(&r), "字体:");
        assert!(font.contains("face=真字体"), "{font}");
        assert!(font.contains("字形缓存 4096 B"), "{font}");
        assert!(font.contains("命中 61"), "{font}");
        assert!(font.contains("栅格化 12"), "{font}");
        assert!(font.contains("未缓存 3"), "{font}");
        // 上限那个数是编译期常量，两平台读同一个 ⇒ 断它不为 0，就断掉了"把上限漏进分子"那一类
        // 抄写错（Wayland 那一行里它是括号里的第二个数）。
        assert!(
            font.contains(&format!("上限 {} B", overlay_common::text::CACHE_MAX_BYTES)),
            "{font}"
        );
        r.font.face_real = false;
        let font = line_with(&lines(&r), "字体:");
        assert!(font.contains("face=降级 5×7 点阵"), "{font}");
    }

    /// 生效值那一行（M7e 接线）：**整行原文来自共用层**，本函数只在前面补两个空格。
    ///
    /// 三件事各防一种漂移：① **逐字**出现 ⇒ 在这里重排格式就有了第二个格式事实源，两家的
    /// 摘要长得不一样，而验收脚本 grep 的是同一个 token；② 位置排在"已接/未接"清单之后、
    /// `overlay:` 之前 ⇒ 摘要开头才是人会在 300 s 那份日志末尾找的那一格（Wayland 同位）；
    /// ③ 两行 `ui:` 不是一行打印了两次 —— 清单说的是"这块面在不在"，生效值说的是"这一档从
    /// 哪一层来"，合并成一行就会在下一次换轴时被顺手删掉一半。
    #[test]
    fn the_effective_settings_line_is_verbatim_and_where_the_report_reads_it() {
        let c = Counters::default();
        let mut r = run(&c);
        let sample = "ui: lang = ja（命令行，自名 日本語）｜ motion = off（config.toml）";
        r.ui_effective = sample;
        let out = lines(&r);
        let at = out
            .iter()
            .position(|l| l == &format!("  {sample}"))
            .expect("生效值那一行必须逐字出现在摘要里（前导两个空格 + 共用层原文）");
        let listed = out
            .iter()
            .position(|l| l.trim_start().starts_with("ui: 已接"))
            .expect("清单那一行也得在");
        let overlay = out
            .iter()
            .position(|l| l.starts_with("overlay:"))
            .expect("overlay 计数那一行也得在");
        assert!(
            listed < at && at < overlay,
            "顺序必须是 清单 → 生效值 → overlay 计数：\n{}",
            out.join("\n")
        );
        assert_eq!(
            out.iter()
                .filter(|l| l.trim_start().starts_with("ui:"))
                .count(),
            2,
            "以 `ui:` 起头的行现在恰好两行，多一行 = 有第三个事实源：\n{}",
            out.join("\n")
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
        // 内核不在，UI 那三行**照打**（M7b 前这里断的是 `NOT_BUILT` 还在，因为面板那一行
        // 靠它占位；现在没有占位符可断了，改成断"面板行仍然带着数在场"）。
        let panel = line_with(&lines(&r), "面板:");
        assert!(
            panel.contains("没开") && panel.contains("0 提交"),
            "{panel}"
        );
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
