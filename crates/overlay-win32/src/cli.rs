//! `opet-overlay-win32` 的参数面。
//!
//! ## 与 Wayland 那份 `cli` 的关系：形状照抄，**范围是真子集**
//!
//! 两份 hand-rolled 解析器（不引 clap）而不是一个共享解析器，是有意的分叉：
//! 参数面本来就是两个平台**各自的用户接口**（Windows 上没有 Wayland registry 可探测，
//! 而这一片也没有控制面可连），共享一层只会把"这边没有那件事"抹平成"那边传了个默认值"。
//! 真正必须共享的东西已经共享了 —— 图集字节与五档省电记账在 `overlay-common`。
//!
//! 但**写法**必须一致，否则同一仓库里出现两种方言，下次两边都要改的时候必然漏一边：
//! `--flag=值` 与 `--flag 值` 等价、值长得像 flag 就报错而不是吞掉、`Option<_>` 存原始值
//! 而把语义拒绝集中在 `Args::options()`、`Err(String)` 由调用方翻译成退出码 2。
//!
//! ## 这个子集之外的一条都不留"能解析、没人读"
//!
//! 剩下的 Wayland 侧真开关在这里由 [`REFUSED`] **点名拒绝并给理由**，而不是落到
//! "未知参数"那条默认分支。区别不是礼貌：默认分支说的是"我不认识这个词"，而这些
//! 用户没有打错字 —— 从 Wayland 那条命令行拷过来是最自然的动作，那句"未知"会把人
//! 支去数字符，而不是去问"这台机器上它为什么不存在"。
//!
//! ⚠️ 反面同样立着：**认下的每一个开关都必须真有人读**。所以 [`ACCEPTED`] 与 [`USAGE`]
//! 之间是**双向**平权（两条测试各钉一个方向），而"下游有人读"这一段在第 6/8 步各接一段。
//! M7e 那两条（`--lang` / `--motion`）从 [`REFUSED`] 搬进来时带的就是同一个条件：下游 =
//! [`crate::settings`] 那条链 + `App::ui`，开关与消费者同一批落地，绝没有"先收参数、以后再接"。

use std::path::PathBuf;

use crate::app::Options;

pub const USAGE: &str = "\
USAGE: opet-overlay-win32 [OPTIONS]

  Desktop pet on Windows: one layered, borderless, always-on-top window fed by the
  SAME baked atlas (sheet.webp + pet.json) and the SAME 60 Hz behavioural core the
  Wayland client drives. Pixels never cross into petcore: this process decodes the
  sheet and owns every byte it puts on screen.

  This binary is M5/M6/M7's Windows client: the pet on screen, the right-click menu,
  accessory dress-up, the gacha pull with its result card, the collection panel and
  the language/motion settings chain. What it does NOT have is the install surface:
  no tray icon, no control socket. The flags that would ask for those are REFUSED by
  name at the bottom of this text, each with its reason - never accepted-and-ignored.

  One content source is required:
    --manifest FILE     pet.json for the pet to show (produced by tools/pack.py)
    --test-rect         solid rectangle instead: checks that the window maps, that
                        DPI maths holds and that clicks fall through the silhouette,
                        without reading a single asset byte

OPTIONS:
  --manifest FILE       see above
  --test-rect           see above
  --db FILE            sqlite archive for gacha persistence (petstore, bundled
                       SQLite - linked since M5's second slice). Absolute path.
                       Not given = demo state: the startup line prints where the
                       Windows fallback WOULD write and no byte is written -
                       the same no-touch contract as the Wayland client's
                       missing --db (memory store, nothing near the real
                       archive).
  --seconds N           stop after N seconds; 0 = run until killed. Default 3: a pet
                        started by hand on a live desktop must not linger there.
  --monitor NAME        GDI device name of the monitor to live on, e.g. \\.\\DISPLAY1
                        (matched case-insensitively). Default = the primary monitor.
  --panel-top N         top desktop inset in LOGICAL pixels, as an OVERRIDE.
                        Without it the inset is measured (rcWork.top - rcMonitor.top
                        = the taskbar's height); with it your number wins and the
                        startup line prints which one was used.
                        This is NOT the Wayland meaning: there the protocol cannot
                        read the reserved strip at all, so the flag is added to zero.
  --panel-bottom N      same override rule at the bottom. Measuring alone is not
                        enough here - a third-party dock often takes no work-area
                        reservation, so this inset still has to be sayable.
  --seed N              seed for the behavioural core (default 1). Same seed replays
                        the same frames bit for bit.
  --lang CODE           UI language: zh / en / ja / ko. Not given = the chain decides:
                        [ui] lang in config.toml -> LC_ALL/LANG -> en. An unrecognised
                        code is an ERROR and exits - never a silent fall back to the
                        default (a typo and 'the language is wrong but nobody said so'
                        are two different failures). The startup line prints which file
                        was read and which tier won.
  --motion TIER         gacha card animation: full / reduce / off. reduce = the burst
                        opens at 1/4 the size and does NOT flash white; off = the burst
                        is skipped entirely (submissions only go down). Neither tier
                        touches dwell, the rarity ribbon, or the pet's own animation.
  --allow-unowned-accessories
                        dev exemption: dress the pet with accessories the inventory
                        does not own. Default OFF - letting an unowned item through
                        has to be an explicit decision. No effect without --db
                        (demo state has no inventory to check, so everything is
                        already let through and the hat page prints 「演示」
                        instead of this row's `*`).
  --allow-unowned-pets
                        same exemption for the rows that name a PET: without it a pet
                        you have not pulled stays a read-only line in the collection
                        panel even when its three files sit on disk. Default OFF.
                        It buys no on-screen mark (the hat page's `*` belongs to the
                        accessory flag only), and no effect without --db, same rule
                        as above.
  --probe            READ-ONLY diagnostic: print the monitors GDI enumerates (name,
                       physical rect, work-area top/bottom, which one is primary and
                       which one becomes the stage), the DPI this process can reach
                       and which DPI tier the run would use, the two insets computed
                       by the SAME function the startup line prints, and whether
                       UpdateLayeredWindow resolves in this process. No window is
                       created, no surface mapped, no asset byte read, and the
                       process' DPI awareness is NOT changed - so the DPI printed is
                       what an UNAWARE process sees, which on mixed-scale multi-monitor
                       is not necessarily what a real run reads per window.
                       Exit 0 = pass, 1 = the probe ran and did NOT pass (scripts read
                       the number, hence the same codes as the Wayland client).
                       Two things this deliberately does NOT check, each printed as
                       such: DWM composition state (layered windows predate it, so it
                       answers nothing here, while adding dwmapi.dll would force a
                       re-measure of the whole DLL import whitelist) and per-monitor
                       DPI (needs a window).
  -h, --help            print this text and exit.

NOT IN THIS BUILD (the parser names the reason for each; use opet-overlay-wayland
  for the full feature set):
  no --ipc   --ipc-path P          the control socket is a Unix-domain socket at
                                   $XDG_RUNTIME_DIR/opet.sock; no Windows transport
                                   has been designed for it yet.
  no --no-cursor                   it works around 'the client owns the cursor image'
                                   on Wayland. Windows draws the cursor itself; there
                                   is nothing to switch off, and a flag that buys
                                   nothing is a trap waiting for the next reader.
";

/// 本片**认下**的开关全集（`--help` / `-h` 不在里面：[`dispatch`] 在 [`parse`] 之前就把
/// 它们接住了，所以 `parse` 其实不认那个词 —— 把它写进这张表会让测试
/// `every_flag_this_build_takes_is_parsable_and_documented` 当场红，而那正是这张表需要
/// 能红的形状）。
///
/// 这张表与 [`USAGE`] 的关系由两条测试双向钉住：广告了必须认（防 `--cell` 那种假开关），
/// 认了必须广告（防"解析器偷偷长出一个没人知道的旗"）。
pub const ACCEPTED: &[&str] = &[
    "--manifest",
    "--test-rect",
    "--db",
    "--seconds",
    "--monitor",
    "--panel-top",
    "--panel-bottom",
    "--seed",
    "--lang",
    "--motion",
    "--probe",
    "--allow-unowned-accessories",
    "--allow-unowned-pets",
];

/// 本片**明确拒绝**的开关与各自的理由。
///
/// ⚠️ 每一条理由都必须说的是"这台机器上为什么没有那件事"，不是"还没做"。
/// 后者的有效期只到下一个里程碑，而前者是平台的形状：registry 不存在、光标由系统画。
/// （曾经第三条是"语言没有作用处" —— 那句话随着 M7e 一起删了：语言早就在屏上生效，缺的只是链。）
///
/// 达不到那个标准的条目要**自己承认**：`--ipc` 那一条的真实理由是"Windows 侧的传输还没设计"，
/// **不是**"这个系统没有 Unix 套接字"（Win10 起 `AF_UNIX` 真在）。所以那里写的是前者。
/// 把"没设计"写成"不可能"是这类注释最容易犯的错 —— 它读起来像一个决定，实际是一张
/// 不会兑现的期票，而下一个人会照着它省掉一整段设计。
///
/// **这张表会随开关转真而缩短**：`--lang` / `--motion` 两条在 M7e 整条搬进 [`ACCEPTED`]
/// （配置链落地 ⇒ 那句"缺的是链，不是这块面"兑现了），六条 → 四条；`--probe` 在 **M7f** 跟着走，
/// 四条 → **三条**。
///
/// ⚠️ `--probe` 这一条的毕业**换了语义而不只是换了归属**：旧文案说的是"打的是 Wayland
/// registry，这个平台上没有 registry"（那句话本身到 M7f 仍然成立），而 M7f 交出的不是 registry
/// 的翻版，是**这个平台真读得到的四格**（监视器 / DPI / 留白 / 分层窗口符号），逐格的出处与
/// 两格刻意的"不查"都写在 [`crate::probe`] 的模块头。留着旧文案 = 一句"这里没有那件事"，
/// 而那句话现在会被 `--probe` 自己的输出当场证伪。
///
/// 剩下三条各有归属：`--ipc` / `--ipc-path` / `--no-cursor` 归第三批与平台形状。
/// 缩短的时候**两条一起删**（`out_of_scope_flags_are_refused_by_name_with_a_reason` 那条
/// 下限断言跟着改口径，别让它悄悄扫空集）。
pub const REFUSED: &[(&str, &str)] = &[
    (
        "--ipc",
        "§3.12 的控制面是 `$XDG_RUNTIME_DIR/opet.sock` 那个 Unix 域套接字；Windows 侧的传输\
         （named pipe 还是本地 TCP）还没设计，开了等于开一个没人连的口子",
    ),
    (
        "--ipc-path",
        "同上：那条路径的默认值本身就是 Unix 域套接字的位置，套接字没有后端时覆盖它无意义",
    ),
    (
        "--no-cursor",
        "它绕的是 Wayland 那条「客户端自己拥有光标图像、主题加载失败就隐形」的病；\
         Windows 自己画光标，这里没有可关的东西 —— 买了零东西的开关是留给下一个读者的陷阱",
    ),
];

fn refusal(key: &str) -> Option<&'static str> {
    REFUSED
        .iter()
        .find(|(flag, _)| *flag == key)
        .map(|(_, why)| *why)
}

/// 两类拒绝的**唯一**分岔口：认得这个词 ⇒ 说清它为什么不在这里；不认得 ⇒ 才是未知参数。
fn refusal_message(key: &str) -> String {
    match refusal(key) {
        Some(why) => format!("{key} 不在 Windows 这一片里：{why}"),
        None => format!("未知参数: {key}"),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    /// `--probe`：**只读探测**，不建窗口、不改 DPI 感知档、不读资产。
    ///
    /// 它**不进** [`Options`]（与 Wayland 那家同形）：`Options` 是"一次真运行的全部输入"，
    /// 而探测那一条路在 `dispatch` 里就返回了。留一个 `probe: bool` 在 `Options` 里
    /// 只会多出一个"没人读的字段"（`--cell` 那一族假开关的形状）。
    probe: bool,
    manifest: Option<PathBuf>,
    test_rect: bool,
    db: Option<String>,
    seconds: Option<f32>,
    monitor: Option<String>,
    panel_top: Option<f32>,
    panel_bottom: Option<f32>,
    seed: Option<u64>,
    /// `--lang` 的原始值。合法性由 [`crate::settings::validate_cli`] 判（**不在这里抄一张表**），
    /// 语义由那条唯一的链判 ⇒ 这里只负责"原样带到底"（同 Wayland 侧 `cli::Args::lang`）。
    lang: Option<String>,
    /// `--motion` 的原始值，同上。
    motion: Option<String>,
    allow_unowned_accessories: bool,
    allow_unowned_pets: bool,
}

impl Args {
    /// 语义拒绝集中在这里（与 Wayland 侧同一个纪律：`parse` 只管形状，`options` 管含义）。
    ///
    /// `None` 与 `Some(0.0)` 在两个 panel 旗上**是两种意思**（"去量"与"我要 0"），
    /// 所以这里只做下限检查，不做 `unwrap_or(0.0)` 那种合并。
    pub fn options(&self) -> Result<Options, String> {
        if self.manifest.is_some() && self.test_rect {
            return Err("--manifest 与 --test-rect 只能给一个（矩形档一行资产都不读）".to_string());
        }
        if self.manifest.is_none() && !self.test_rect {
            return Err("要么 --manifest <pet.json> 上宠物，要么 --test-rect 验后端".to_string());
        }
        if let Some(s) = self.seconds {
            if s < 0.0 {
                return Err(format!(
                    "--seconds 不能是负数（给了 {s}）；要一直跑就写 0（0 = 永不自动退出）"
                ));
            }
        }
        for (flag, v) in [
            ("--panel-top", self.panel_top),
            ("--panel-bottom", self.panel_bottom),
        ] {
            if let Some(n) = v {
                if n < 0.0 {
                    return Err(format!(
                        "{flag} 不能是负数（给了 {n}）；留白是从屏幕边内缩量，负的等于把宠物推出屏幕"
                    ));
                }
            }
        }
        // 空值那一族（Wayland 侧最早是 `--db=` 与 `--seconds -5` 立下的规矩）：既不是"不给"也不是一个值，
        // 语义上是最坏的一种"看起来开了档"。`--monitor=` 空串在 Windows 上还多一层坑：它会被大小写不敏感的
        // 前缀匹配当成"匹配所有"，于是"我没指定显示器"变成"我指定了第一台命中的"。
        if self.monitor.as_deref().is_some_and(|m| m.trim().is_empty()) {
            return Err(
                "--monitor 给了空名字；不指定显示器就别给 --monitor（默认 = 主屏）".to_string(),
            );
        }
        // `--db=` 与 Wayland 同族同文案的骨架："不写真存档就别给 --db"，只是这一片的兜底档
        // 不是"内存库 + DRYRUN"而是"打实解路径、一个字不写"（M4 的口径，见 app::Options::db）。
        if self.db.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return Err(
                "--db 给了空路径；不写真存档就别给 --db（默认是兜底档：只打印实解位置，不写）"
                    .to_string(),
            );
        }
        // `--lang=` / `--motion=` 空值与任何不认识的值一样：**报错，不静默退默认**（R14）。
        // 判据表在共用层（`lang_codes()` / `motion_codes()`），这里不抄第二份 —— 抄了就有
        // "第五种语言加进 UI 而 Windows 的报错还写四种"那一族漂移（M7e 上提链的同一个理由）。
        crate::settings::validate_cli(self.lang.as_deref(), self.motion.as_deref())?;
        Ok(Options {
            manifest: self.manifest.clone(),
            test_rect: self.test_rect,
            db: self.db.clone(),
            seconds: match self.seconds {
                Some(0.0) => None,
                s => Some(s.unwrap_or(3.0)),
            },
            monitor: self.monitor.clone(),
            panel_top: self.panel_top,
            panel_bottom: self.panel_bottom,
            seed: self.seed.unwrap_or(1),
            lang: self.lang.clone(),
            motion: self.motion.clone(),
            allow_unowned_accessories: self.allow_unowned_accessories,
            allow_unowned_pets: self.allow_unowned_pets,
        })
    }
}

fn num<T: std::str::FromStr>(raw: &str, flag: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    raw.parse::<T>()
        .map_err(|e| format!("{flag} 不是数字: {e}"))
}

/// 同时接受 `--flag=值` 与 `--flag 值` 两种写法。
pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = match args[i].split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (args[i].as_str(), None),
        };
        let next_val = |idx: &mut usize| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            *idx += 1;
            let v = args
                .get(*idx)
                .cloned()
                .ok_or_else(|| format!("{key} 缺少参数值"))?;
            // 值长得像 flag ⇒ 用户漏写了值（`--monitor --seconds`）。吞掉它的后果是下一条旗被
            // 当成显示器名，于是**两个**参数都错而没有一个报错。负数是合法数值，不能一律当 flag。
            if v.starts_with('-') && v.parse::<f64>().is_err() {
                return Err(format!("{key} 缺少参数值（把 {v} 当成了值）"));
            }
            Ok(v)
        };
        match key {
            "--manifest" => a.manifest = Some(PathBuf::from(next_val(&mut i)?)),
            "--test-rect" => a.test_rect = true,
            "--db" => a.db = Some(next_val(&mut i)?),
            "--seconds" => a.seconds = Some(num(&next_val(&mut i)?, key)?),
            "--monitor" => a.monitor = Some(next_val(&mut i)?),
            "--panel-top" => a.panel_top = Some(num(&next_val(&mut i)?, key)?),
            "--panel-bottom" => a.panel_bottom = Some(num(&next_val(&mut i)?, key)?),
            "--seed" => a.seed = Some(num(&next_val(&mut i)?, key)?),
            // M7-e 转真：**只搬运，不解释**。值原样进 `Args`，合法性在 `options()` 问共用层，
            // 生效值与来源在 `settings::from_process` 那条唯一的链上算 ⇒ 这一家与 Wayland 那一
            // 家在"给了 en 而屏上是中文"这件事上必然同一个答案（两家各抄一张表才是漂移的入口）。
            "--lang" => a.lang = Some(next_val(&mut i)?),
            "--motion" => a.motion = Some(next_val(&mut i)?),
            // M7-f 转真。裸旗**拒收值**，与下面那两条豁免旗同形（同一个理由：`--probe=0`
            // 被读成"开了"就是静默换档）。⚠️ Wayland 那侧这一条是**吞** inline 的
            // （`a.probe = true` 不看值）⇒ 两边对同一个错误写法的读数不同，这条偏离与
            // 豁免旗那两条是同一条线，一并登记给 §6.1.26。
            "--probe" => {
                if let Some(v) = &inline {
                    return Err(format!(
                        "--probe 是裸旗，不接受值（给了 {v:?}）；\
                         要探测就别写值，不要探测就别写这条旗，没有第三种说法"
                    ));
                }
                a.probe = true;
            }
            // 裸旗**拒收值**而不是吞掉它。这一格真要说清三件事：
            // * `--allow-unowned-accessories=false` 被读成"开了"是最坏的一种静默 —— 用户明确
            //   写了"不要"，拿到的正是"要"。本项目对"看起来开了档"的一贯处置（`--db=`、
            //   `--monitor `、`--seconds -5`）都是报错，不是猜。
            // * 为什么只给这一条新旗立、不给 `--test-rect` 补：第一批的开关面逐字冻结
            //   （AGENTS.md「第一批」），要改就得连 Wayland 侧同族一起改并重新签收，那是另一轮的事。
            // * Wayland 侧这一条是**吞**的（`a.allow_unowned_accessories = true` 不看 inline），
            //   所以两边对同一个错误写法的读数不同 —— 这条偏离登记给 §6.1.26。
            "--allow-unowned-accessories" => {
                if let Some(v) = &inline {
                    return Err(format!(
                        "--allow-unowned-accessories 是裸旗，不接受值（给了 {v:?}）；\
                         要放行就别写它、要关着也别写它，没有第三种说法"
                    ));
                }
                a.allow_unowned_accessories = true;
            }
            // M7-d 转真。写法与上面那条**同形**（裸旗拒收值），理由也同一条：`=false` 读成
            // "开了"是静默换档。两条不合成一个 match 臂是选的 —— 报错文案必须点得出**哪一个**
            // 旗写错了，用户下一步才知道改哪个词。
            "--allow-unowned-pets" => {
                if let Some(v) = &inline {
                    return Err(format!(
                        "--allow-unowned-pets 是裸旗，不接受值（给了 {v:?}）；\
                         要放行就别写它、要关着也别写它，没有第三种说法"
                    ));
                }
                a.allow_unowned_pets = true;
            }
            other => return Err(refusal_message(other)),
        }
        i += 1;
    }
    Ok(a)
}

/// `dispatch` 的分支结果，调用方据此决定退出码与输出。
///
/// ⚠️ **`Probed` 这一档是 M7-f 才补上的**，而它补的正是原先那句"没有后端的旗"里**没后端的
/// 那一半**：旗现在有 [`crate::probe`] 这个真后端。退出码与 Wayland 那家**逐位相同**
/// （0 = 通过、1 = 探测跑了而没通过、2 = 参数/运行错），因为 Phase B 的脚本读的是数字。
pub enum Outcome {
    /// 打的是 `--help`，`String` 是用法文本
    Help(String),
    /// 跑过探测：`bool` = 通过与否
    Probed(bool),
    /// 真跑了一轮 overlay（正常退出或 `--seconds` 到点）
    Ran,
}

/// 解析 + 分派。`Err` = 参数/运行失败，由调用方决定退出码（2 与 1 的分工见 `src/main.rs`）。
pub fn dispatch(args: &[String]) -> Result<Outcome, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Outcome::Help(USAGE.to_string()));
    }
    let parsed = parse(args)?;
    if parsed.probe {
        // 探测不渲染任何东西 ⇒ 不吃语言/动画档，**但不许因此放过打错的值**：
        // `--probe --lang=jap` 静默成功等于告诉用户"这个码是对的"，而他下次真起 overlay 时
        // 看到的仍是默认档。`options()` 在探测路径上会因"没给 --manifest"先炸，所以这里
        // 单独预检一次（与 Wayland `cli.rs` 那一条同一个理由、同一份共用层判据）。
        crate::settings::validate_cli(parsed.lang.as_deref(), parsed.motion.as_deref())?;
        let report = crate::probe::collect(
            parsed.monitor.as_deref(),
            parsed.panel_top,
            parsed.panel_bottom,
        );
        print!("{}", crate::probe::format_report(&report));
        return Ok(Outcome::Probed(crate::probe::probe_ok(&report)));
    }
    crate::app::run(parsed.options()?).map(|()| Outcome::Ran)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// USAGE 每一行**行首**那个 `--flag`（`-h,` 那种不以 `--` 开头的第一个词不在此列）。
    ///
    /// 只查行首，与 Wayland 侧同一条口径：正文里提到的 `tools/pack.py --accessory` 是**生产者**
    /// 的开关，不是这条命令的。而本片"NOT IN THIS BUILD"那一节的行首写的是 `no`，
    /// 所以那八个被拒的旗**天然不在网里** —— [`no_refused_flag_is_advertised_as_an_option`]
    /// 断的就是这个排版选择没有骗人。
    fn advertised() -> Vec<&'static str> {
        USAGE
            .lines()
            .map(str::trim_start)
            .filter_map(|l| l.split_whitespace().next())
            .filter(|t| t.starts_with("--"))
            .collect()
    }

    #[test]
    fn both_spellings_of_flag_value_are_equal() {
        let a = parse(&v(&[
            "--test-rect",
            "--seconds=7",
            "--seed=42",
            r"--monitor=\\.\DISPLAY2",
        ]))
        .unwrap();
        let b = parse(&v(&[
            "--test-rect",
            "--seconds",
            "7",
            "--seed",
            "42",
            "--monitor",
            r"\\.\DISPLAY2",
        ]))
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.seconds, Some(7.0));
        assert_eq!(a.seed, Some(42));
        // 走 `options()` 而不是只看 `Args`：内联写法与分开写法必须在**最后一层**也相等，
        // 而 `Options` 才是运行期真读的那份（`Args` 相等但映射分叉，是这一条要抓的形状）。
        assert_eq!(
            a.options().unwrap().monitor.as_deref(),
            Some(r"\\.\DISPLAY2")
        );
    }

    #[test]
    fn missing_value_is_an_error_not_a_panic() {
        for args in [
            v(&["--seconds"]),
            v(&["--manifest"]),
            v(&["--panel-top"]),
            v(&["--seconds", "abc"]),
            v(&["--seed", "1.5"]),
        ] {
            assert!(parse(&args).is_err(), "{args:?} 应当报错");
        }
        // 真正不认识的词，走的是另一条分支（对比下面那条：这八个字**不是**未知参数）。
        let err = parse(&v(&["--nope"])).err().unwrap();
        assert!(err.contains("未知参数"), "{err}");
    }

    #[test]
    fn a_value_that_looks_like_a_flag_is_not_swallowed() {
        // 吞掉的话 `--seconds` 变成显示器名，于是两个参数都错而没有一个报错。
        let err = parse(&v(&["--monitor", "--seconds", "30"])).err().unwrap();
        assert!(
            err.contains("--monitor") && err.contains("缺少参数值"),
            "{err}"
        );
        assert!(parse(&v(&["--manifest", "a.json", "--seconds", "-1"])).is_ok());
        // 负数是合法数值形状，能不能过由 options() 判（下一条）。
        assert!(
            parse(&v(&["--manifest", "a.json", "--seconds", "-1"]))
                .unwrap()
                .options()
                .is_err()
        );
    }

    #[test]
    fn exactly_one_content_source_is_required() {
        // 两个都给 = 语义上自相矛盾（矩形档一行资产都不读，给了 manifest 也不会有人看它）
        let both = parse(&v(&["--manifest=a.json", "--test-rect"])).unwrap();
        assert!(both.options().is_err());
        // 一个都不给 = 会映射一块永远不提交的窗口
        let neither = parse(&v(&[])).unwrap();
        assert!(neither.options().is_err());
        assert!(parse(&v(&["--test-rect"])).unwrap().options().is_ok());
        assert!(
            parse(&v(&["--manifest", "a.json"]))
                .unwrap()
                .options()
                .is_ok()
        );
    }

    #[test]
    fn seconds_zero_means_forever_and_the_default_is_three() {
        let a = parse(&v(&["--test-rect", "--seconds", "0"])).unwrap();
        assert_eq!(a.options().unwrap().seconds, None);
        let b = parse(&v(&["--test-rect"])).unwrap();
        assert_eq!(
            b.options().unwrap().seconds,
            Some(3.0),
            "缺省必须是限时自毁：live 桌面上手工敲起来的宠物不许赖着"
        );
    }

    /// **这一条钉的是那两个旗在 Windows 上换了语义**（`Option<f32>` 而不是 `f32`）。
    ///
    /// 压成 `f32` 默认 0 会让"我要 0 留白"与"你去量工作区"变成同一个数，而后者才是常态：
    /// 于是那条旗**只在用户不写它时才有意义** = `--cell` 那一族（一个从来没被非默认值
    /// 用成功的开关，等于它不存在）。这里把三档分开断言，让"合并成 0"这一步必须红。
    #[test]
    fn a_panel_inset_of_zero_is_not_the_same_as_not_asking() {
        let measured = parse(&v(&["--test-rect"])).unwrap();
        let o = measured.options().unwrap();
        assert_eq!(
            (o.panel_top, o.panel_bottom),
            (None, None),
            "不给 ⇒ 现量（rcWork 那一头），不许在这里替用户决定成 0"
        );
        let zero = parse(&v(&["--test-rect", "--panel-top", "0"])).unwrap();
        assert_eq!(
            zero.options().unwrap().panel_top,
            Some(0.0),
            "`--panel-top 0` = 用户明说「不要留白」，与「没说话」必须还能分开"
        );
        let given = parse(&v(&["--test-rect", "--panel-bottom=48"])).unwrap();
        assert_eq!(given.options().unwrap().panel_bottom, Some(48.0));
    }

    #[test]
    fn an_empty_monitor_name_is_refused() {
        for args in [
            v(&["--monitor=", "--test-rect"]),
            v(&["--monitor", " ", "--test-rect"]),
        ] {
            let err = parse(&args)
                .and_then(|a| a.options())
                .err()
                .unwrap_or_else(|| panic!("{args:?} 空显示器名不该还能过"));
            assert!(err.contains("--monitor"), "{err}");
        }
    }

    /// `--db` 转真（第二批 M4）后的三条：两种写法在**最后一层**也相等、空值是拒绝而不是
    /// "开了一个空档"、不给是兜底档。与 Wayland 侧 `cli.rs` 那组同形，样本换成 Windows 拼法。
    #[test]
    fn db_is_accepted_and_an_empty_value_is_refused() {
        let eq = parse(&v(&["--test-rect", r"--db=C:\Users\jun\opet_test.db"]))
            .unwrap()
            .options()
            .unwrap();
        let sp = parse(&v(&["--test-rect", "--db", r"C:\Users\jun\opet_test.db"]))
            .unwrap()
            .options()
            .unwrap();
        assert_eq!(eq.db.as_deref(), Some(r"C:\Users\jun\opet_test.db"));
        assert_eq!(eq, sp, "内联与分开两种写法必须在 options() 之后仍相等");
        // 空值 = 最坏的"看起来开了档"：报错点名 --db，而不是静默退回兜底档。
        let err = parse(&v(&["--test-rect", "--db="]))
            .and_then(|a| a.options())
            .err()
            .unwrap_or_else(|| panic!("--db= 空值不该还能过"));
        assert!(err.contains("--db"), "{err}");
        // 不给 ⇒ None ⇒ 运行期走兜底解析（打实解路径、不写），这里只钉解析层不臆造路径。
        assert_eq!(
            parse(&v(&["--test-rect"])).unwrap().options().unwrap().db,
            None
        );
    }

    /// `--allow-unowned-accessories` 转真（第二批 M5e）后的三条：默认关、给了才开、
    /// **给值不是"开了一个档"而是报错**。
    ///
    /// 为什么第三条值得单独一格：`--flag=false` 被吞成"开"是本文件那条「空值/假档」规矩的
    /// 反面实例 —— 用户写了"不要"而拿到"要"，方向是**反的**，比 `--db=` 那种"给了个空档"更坏。
    /// Wayland 侧这一条是吞的（同一条命令行在两平台读数不同），那条偏离写进了臂上的注释。
    #[test]
    fn allow_unowned_accessories_defaults_off_and_a_value_is_refused() {
        let off = parse(&v(&["--test-rect"])).unwrap().options().unwrap();
        assert!(
            !off.allow_unowned_accessories,
            "默认必须关着 ⇒ 放行未拥有的配件是要显式给的决定（Wayland 侧 2026-11-05 第 3 问，\
             本平台同一条判据）"
        );
        let on = parse(&v(&["--test-rect", "--allow-unowned-accessories"]))
            .unwrap()
            .options()
            .unwrap();
        assert!(on.allow_unowned_accessories);
        assert!(on.test_rect, "裸旗不该顺手把矩形档挤掉");
        // 值 = 报错，而且点名是哪个旗、说的是"裸旗不接受值"。`false` 那一格是这一条的全部理由。
        for bad in [
            "--allow-unowned-accessories=false",
            "--allow-unowned-accessories=0",
            "--allow-unowned-accessories=x",
        ] {
            let err = parse(&v(&["--test-rect", bad]))
                .err()
                .unwrap_or_else(|| panic!("{bad} 不该还能过（裸旗吞值 = 把「不要」读成「要」）"));
            assert!(err.contains("--allow-unowned-accessories"), "{err}");
            assert!(err.contains("裸旗"), "{err} 没说清为什么拒绝");
        }
        // 后一个 token 长得像旗 ⇒ 它**不**被当成本旗的值吃掉（本旗不吃值，所以没有那条歧义）。
        let two = parse(&v(&[
            "--test-rect",
            "--allow-unowned-accessories",
            "--seed",
            "7",
        ]))
        .unwrap()
        .options()
        .unwrap();
        assert!(two.allow_unowned_accessories);
        assert_eq!(two.seed, 7, "下一旗被吃成值的话，种子会退回默认 1");
    }

    /// 两根旗**各自独立**（Wayland 侧 `cli.rs` 同一条拍板④的 Windows 版）。
    ///
    /// M7d 之前这一格断的是"本体那一根仍要**点名拒绝**"；P5 落地后它换成"两根都转真而互不相干"，
    /// 而存在理由一个字没变 —— 两个旗共用一个词根，最容易犯的错是顺手合并成一根。两条判据在
    /// [`overlay_common::dress::dress_exempt_for`] 是同一个函数、在参数面是**两个字段**，
    /// 而屏上那个 `*` 只由配件那一根点亮 ⇒ 合成一根必然出现"标打了而没放行"或反过来。
    #[test]
    fn the_two_unowned_switches_are_not_the_same_thing() {
        for flag in ["--allow-unowned-accessories", "--allow-unowned-pets"] {
            assert!(
                ACCEPTED.contains(&flag),
                "{flag} 必须已经转真（各自的消费者 = `App::dress_exempt` / `App::pet_exempt`）"
            );
            assert!(
                refusal(flag).is_none(),
                "{flag} 不再属于「点名拒绝」那张表：它同时进了 ACCEPTED 与 REFUSED = 参数面自相矛盾"
            );
        }

        let only_pets = parse(&v(&["--test-rect", "--allow-unowned-pets"]))
            .unwrap()
            .options()
            .unwrap();
        assert!(only_pets.allow_unowned_pets);
        assert!(
            !only_pets.allow_unowned_accessories,
            "开本体那一根顺手打开了配件那一根 ⇒ 两根其实是同一个字段"
        );

        let only_acc = parse(&v(&["--test-rect", "--allow-unowned-accessories"]))
            .unwrap()
            .options()
            .unwrap();
        assert!(only_acc.allow_unowned_accessories);
        assert!(
            !only_acc.allow_unowned_pets,
            "反过来同样不许串（这一格在只写一根的时代是读不出来的）"
        );

        // 裸旗拒收值那一格两根同形，而**文案必须点得出是哪一个**：复制上一条臂而忘了改名，
        // 用户按报错去改那个其实没写错的旗。
        for (flag, other) in [
            ("--allow-unowned-pets", "--allow-unowned-accessories"),
            ("--allow-unowned-accessories", "--allow-unowned-pets"),
        ] {
            let err = parse(&v(&["--test-rect", &format!("{flag}=false")]))
                .err()
                .unwrap_or_else(|| panic!("{flag}=false 这种写法必须报错"));
            assert!(err.contains(flag), "{err}");
            assert!(!err.contains(other), "报错文案指错了旗：{err}");
        }
    }

    // ── USAGE ↔ parser 双向平权 ─────────────────────────────────────────────

    /// 方向一：**广告了的每一个开关都真的存在**。
    ///
    /// 这条门禁的存在理由是本项目那条 `--cell` 假开关 —— 帮助里挂了一整轮而两处实现都拒收
    /// 非默认值。它只判"解析器认不认这个词"（值缺失、值非法都不算），因为假开关的**第一步**
    /// 永远是"帮助里先有了它"。
    #[test]
    fn every_flag_advertised_in_usage_exists() {
        let advertised = advertised();
        assert!(
            advertised.len() >= ACCEPTED.len(),
            "USAGE 行首只扫到 {} 个旗而 ACCEPTED 有 {} 个 ⇒ 排版变了、这条门禁正在扫空集: \
             {advertised:?}",
            advertised.len(),
            ACCEPTED.len()
        );
        for flag in advertised {
            assert!(
                ACCEPTED.contains(&flag),
                "USAGE 广告了 {flag} 但它不在 ACCEPTED 里 ⇒ 要么真加旗忘了登记，\
                 要么排版让「NOT IN THIS BUILD」那一节的某一行行首变成了旗（那是把拒绝写成了广告）"
            );
            let err = parse(&v(&[flag])).err().unwrap_or_default();
            assert!(!err.contains("未知参数"), "解析器不认 {flag}: {err}");
        }
    }

    /// 方向二：**认下的每一个开关都在 USAGE 里广告了**，而且 `ACCEPTED` 与 `REFUSED`
    /// 两个名字互不相干（同一个词既认又拒 = 两份事实源，而这条分岔只可能有一边先红）。
    #[test]
    fn every_flag_this_build_takes_is_parsable_and_documented() {
        for &flag in ACCEPTED {
            assert!(
                advertised().contains(&flag),
                "解析器认 {flag} 而 USAGE 行首没写它 ⇒ 帮助不完整（下一个平台照帮助写命令必漏）"
            );
            let err = parse(&v(&[flag])).err().unwrap_or_default();
            assert!(
                !err.contains("未知参数"),
                "{flag} 在 ACCEPTED 里却不被解析: {err}"
            );
            assert!(
                refusal(flag).is_none(),
                "{flag} 同时出现在 ACCEPTED 与 REFUSED 里"
            );
        }
        for &(flag, _) in REFUSED {
            assert!(
                !ACCEPTED.contains(&flag),
                "{flag} 既认又拒 ⇒ 分岔在哪一侧由匹配顺序定，那是个没人读的开关"
            );
        }
    }

    /// 范围外的旗必须**点名是哪个旗**、给得出**理由正文**，而且说的是"这里没有那件事"
    /// 而不是落到默认分支那句"未知参数"。
    ///
    /// 为什么不用 `未知参数` 打发它们：那几个旗用户没打错字，是从 Wayland 那条命令行拷来的。
    /// "未知"会让人去数字符，"这里没有 registry"会让人去问平台。
    ///
    /// ⚠️ 这一族缩过**四**次，四次都不是"有人删了一条而不登记"：八条 → 七条 = **第二批 M5e**
    /// （`--allow-unowned-accessories` 转真），七条 → 六条 = **M7d**（`--allow-unowned-pets`
    /// 转真，随换本体落地），六条 → **四条** = **M7e**（`--lang` + `--motion` 一起走：配置链、
    /// 生效值来源行与这两个开关同批落地 ⇒ 不存在收了参数没人读那一格），四条 → **三条** =
    /// **M7f**（`--probe` 随 [`crate::probe`] 那四格读数转真，而且转真的同时换了语义：交出的不是
    /// Wayland registry 的翻版）。剩下三条各有归属：`--ipc` / `--ipc-path` 归第三批，
    /// `--no-cursor` 归平台形状（它永远不该转真）。**这个下限就是那张登记表**：
    /// 删一条而不改这里，测试当场红。
    #[test]
    fn out_of_scope_flags_are_refused_by_name_with_a_reason() {
        assert!(
            REFUSED.len() >= 3,
            "本片范围外的旗应当有 3 条，实读 {} ⇒ 有人删了一条而没有登记归属",
            REFUSED.len()
        );
        for &(flag, why) in REFUSED {
            assert!(
                why.chars().count() >= 12,
                "{flag} 的理由只有一句「{why}」⇒ 那不是理由，是推托"
            );
            let inline = format!("{flag}=x");
            for args in [v(&[flag]), v(&["--test-rect", flag]), v(&[inline.as_str()])] {
                let err = parse(&args)
                    .err()
                    .unwrap_or_else(|| panic!("{flag} 在 {args:?} 下竟然解析成功了"));
                assert!(err.contains(flag), "错误没点名是哪个开关: {err}");
                assert!(
                    !err.contains("未知参数"),
                    "{flag} 是本仓库另一份客户端的真开关，回「未知」是假话: {err}"
                );
                assert!(err.contains(why), "错误没把理由带出来: {err}");
            }
        }
    }

    /// M7e：`--lang` / `--motion` **原样交给 `Options`**（解释在 `settings` 那条链上），
    /// 两种写法同义，不给 = `None`。最后那半句是这条测例存在的理由：一旦解析层把默认写成
    /// `Some("en")`，`config.toml` 与环境变量两级就永久失效，而屏上看不出任何区别。
    #[test]
    fn lang_and_motion_reach_the_options_verbatim() {
        let eq = parse(&v(&["--test-rect", "--lang=en", "--motion=reduce"]))
            .unwrap()
            .options()
            .unwrap();
        assert_eq!(eq.lang.as_deref(), Some("en"));
        assert_eq!(eq.motion.as_deref(), Some("reduce"));
        let sp = parse(&v(&["--test-rect", "--lang", "en", "--motion", "reduce"]))
            .unwrap()
            .options()
            .unwrap();
        assert_eq!(sp.lang, eq.lang, "两种写法必须同义");
        assert_eq!(sp.motion, eq.motion);
        let none = parse(&v(&["--test-rect"])).unwrap().options().unwrap();
        assert_eq!(none.lang, None);
        assert_eq!(none.motion, None);
        // 子标签合法的形制（`zh_TW` → 落简体）在两家必须是同一个答案，否则"同一份配置
        // 两个平台不同语言"这种报告就只能靠人猜是哪一层归一化掉的。
        let sub = parse(&v(&["--test-rect", "--lang=zh-tw"]))
            .unwrap()
            .options()
            .expect("zh_TW 是合法的（本轮只有简体文案 ⇒ 落 zh）");
        assert_eq!(sub.lang.as_deref(), Some("zh-tw"), "不许在解析层归一化");
    }

    /// 坏值 = **报错退出**，绝不静默退默认（R14）。报错要点名是哪个开关、列出支持范围，
    /// 否则用户只能翻源码。空值 `--lang=` 也算坏值：它是"看起来开了档"的最坏一种。
    #[test]
    fn bad_lang_or_motion_is_an_error_and_never_a_silent_default() {
        for bad in [
            "--lang=zz",
            "--lang=",
            "--lang=jap",
            "--motion=high",
            "--motion=",
            "--motion=off2",
        ] {
            let args = v(&["--test-rect", bad]);
            let got = parse(&args).and_then(|a| a.options());
            let Err(msg) = got else {
                panic!("「{bad}」被静默接受了 ⇒ 用户看不到自己写错了");
            };
            let which = if bad.starts_with("--lang") {
                "--lang"
            } else {
                "--motion"
            };
            assert!(msg.contains(which), "{bad} 的报错没点名开关：{msg}");
            assert!(
                msg.contains("zh/en/ja/ko") || msg.contains("full/reduce/off"),
                "{bad} 的报错没列出支持哪些值：{msg}"
            );
        }
    }

    /// 值长得像 flag 的那条老规矩对这两个新开关同样成立：`--lang --test-rect` 吞掉
    /// `--test-rect` 就等于"矩形档没了而没人报错"（这里连 `--manifest` 都没有 ⇒ 后面的
    /// 语义检查会报一句不相干的话，用户更找不到方向）。
    #[test]
    fn lang_and_motion_never_swallow_the_next_flag() {
        assert!(parse(&v(&["--lang", "--test-rect"])).is_err());
        assert!(parse(&v(&["--motion", "--test-rect"])).is_err());
        assert!(parse(&v(&["--lang"])).is_err(), "缺值也要报错");
        assert!(parse(&v(&["--motion"])).is_err(), "缺值也要报错");
    }

    /// M7f：`--probe` 是**裸旗**，而且它**不进 `Options`**。三半各防一种漂移：
    ///
    /// * **默认必须是 `false`**：漏写它若变成"只探测"，`opet-overlay-win32.exe --manifest X`
    ///   就变成"打一份报告、退 0、屏幕上什么都没有"—— 退 0 会让脚本以为宠物在跑。
    /// * **`options()` 仍然报错**：这是"旗没有被塞进运行参数面"的可见证据。`Options` 里留一个
    ///   没人读的 `probe` 字段，正是第一批登记过的那个形状病（一个开关同时活在两张表上，
    ///   于是它能被解析、被打印，而不被任何人执行）。
    /// * **`--probe=0` / `--probe=` 报错**：吞掉值 = 用户明确写了"不要"而拿到"要"。
    ///   ⚠️ Wayland 那侧这一条是吞的 ⇒ 同一个错误写法两家读数不同，偏离登记给 §6.1.26。
    #[test]
    fn probe_is_a_bare_flag_that_never_reaches_the_options() {
        let a = parse(&v(&["--probe"])).unwrap();
        assert!(a.probe);
        assert!(a.options().is_err(), "探测旗漏写默认值 ⇒ 它会变成运行档");
        assert!(!parse(&v(&["--test-rect"])).unwrap().probe);
        for bad in ["--probe=0", "--probe=false", "--probe="] {
            let err = parse(&v(&[bad])).err().unwrap_or_default();
            assert!(
                err.contains("--probe") && !err.contains("未知参数"),
                "{bad} 的报错要点名这条旗（不是当生词打发）: {err}"
            );
        }
        // 值长得像 flag 那条老规矩在这条旗上最贵：`--monitor --probe` 若把 `--probe` 当名字吞掉，
        // 用户要的是"只读探测"，拿到的是一扇映射到不存在的显示器上的窗。
        assert!(parse(&v(&["--monitor", "--probe"])).is_err());
        assert!(
            parse(&v(&["--probe", "--monitor"])).is_err(),
            "反方向同一条"
        );
    }

    /// 与 Wayland 侧**同名测例**同一条不变量：探测路径不渲染任何东西，所以它绕过 `options()`
    /// （那会因"没给 `--manifest`"先炸），但**不能因此放过打错的档**。R14 的口径是"任何一条
    /// 路径都不静默退默认"，不是"只有会画像素的那条不静默"：`--probe --lang=jap` 静退 0 =
    /// 探针在替用户确认"这个码是对的"。
    ///
    /// ⚠️ 宿主上 `collect()` 走的是"这个二进制不是为当前系统编的"那一条支（`probe.rs`），
    /// 所以这里的 `Probed(false)` 是**平台支的形状**，不是 Windows 探测结果 —— 真读数归 M7g。
    #[test]
    fn probe_path_still_rejects_a_bad_lang_or_motion() {
        for bad in [
            &["--probe", "--lang=jap"][..],
            &["--probe", "--motion=high"][..],
            &["--probe", "--lang", ""][..],
        ] {
            let Err(msg) = dispatch(&v(bad)) else {
                panic!("{bad:?} 被静默接受了 ⇒ 探测在告诉用户这个码是对的");
            };
            assert!(
                msg.contains("认不出来")
                    && (msg.contains("zh/en/ja/ko") || msg.contains("full/reduce/off")),
                "{bad:?} 的报错没点名开关与支持范围：{msg}"
            );
        }
        // 合法值在探测路径上必须放行，否则这条预检把诊断功能本身堵死了。
        assert!(matches!(
            dispatch(&v(&["--probe", "--lang=en", "--motion=reduce"])),
            Ok(Outcome::Probed(_))
        ));
    }

    /// 与上一条配对的方向：**被拒的旗不许长得像广告**，但**必须出现在帮助文本里**。
    /// 两半各防一种漂移。只断"在文本里"，就能把它写成 `OPTIONS:` 下的一行而让读者以为能用；
    /// 只断"不在行首"，就能把那八条从帮助里彻底删掉，于是从 Wayland 拷命令行的人只会撞到
    /// 一条错误而没有上下文。
    #[test]
    fn no_refused_flag_is_advertised_as_an_option() {
        for &(flag, _) in REFUSED {
            assert!(
                !USAGE
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(flag)),
                "{flag} 现在占在行首 ⇒ 它读起来像一个可用的开关"
            );
            assert!(
                USAGE.contains(flag),
                "{flag} 被拒但帮助里一个字都没提 ⇒ 拷命令行的人拿不到上下文"
            );
        }
    }

    /// 拒绝必须发生在**解析期**，而宿主上那一个 `run` 桩必须说真话。
    ///
    /// ⚠️ **这一条只在宿主上跑**（`#[cfg(not(windows))]`）：第 8 步之后 Windows 上的
    /// [`crate::app::run`] 是**真事件环**，测试里调它等于在 CI 机器上开一扇窗并睡到
    /// `--seconds` 到点 —— 那不是断言，那是把测试变成一个 GUI 程序。桩的行为仍然要被钉住，
    /// 因为交叉编译出来的 `.exe` 拿的是另一个 `run`，而"宿主桩说谎"的形状正是
    /// "在 Linux 上 `cargo run -p overlay-win32` 报了个成功"。
    #[cfg(not(windows))]
    #[test]
    fn the_stub_never_claims_success() {
        let o = parse(&v(&["--test-rect"])).unwrap().options().unwrap();
        let err = crate::app::run(o)
            .expect_err("宿主桩必须报错（返回 Ok = 在这台机器上假装跑过一轮 Windows overlay）");
        assert!(!err.is_empty());
        // 参数要在报错里可见：否则"解析错了"与"没接线"在 stdout 上长得一样。
        assert!(err.contains("种子=1"), "{err}");
    }
}
