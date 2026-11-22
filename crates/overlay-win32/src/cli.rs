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
//! 九个 Wayland 侧的真开关在这里由 [`REFUSED`] **点名拒绝并给理由**，而不是落到
//! "未知参数"那条默认分支。区别不是礼貌：默认分支说的是"我不认识这个词"，而这九条
//! 用户没有打错字 —— 从 Wayland 那条命令行拷过来是最自然的动作，那句"未知"会把人
//! 支去数字符，而不是去问"这台机器上它为什么不存在"。
//!
//! ⚠️ 反面同样立着：**认下的每一个开关都必须真有人读**。所以 [`ACCEPTED`] 与 [`USAGE`]
//! 之间是**双向**平权（两条测试各钉一个方向），而"下游有人读"这一段在第 6/8 步各接一段。

use std::path::PathBuf;

use crate::app::Options;

pub const USAGE: &str = "\
USAGE: opet-overlay-win32 [OPTIONS]

  Desktop pet on Windows: one layered, borderless, always-on-top window fed by the
  SAME baked atlas (sheet.webp + pet.json) and the SAME 60 Hz behavioural core the
  Wayland client drives. Pixels never cross into petcore: this process decodes the
  sheet and owns every byte it puts on screen.

  This binary is M5's first slice = the pet on screen and nothing else. No
  right-click menu, no gacha card, no collection panel, no accessories, no tray
  icon, no control socket. The flags that would ask for those are REFUSED by name
  at the bottom of this text, each with its reason - never accepted-and-ignored.

  One content source is required:
    --manifest FILE     pet.json for the pet to show (produced by tools/pack.py)
    --test-rect         solid rectangle instead: checks that the window maps, that
                        DPI maths holds and that clicks fall through the silhouette,
                        without reading a single asset byte

OPTIONS:
  --manifest FILE       see above
  --test-rect           see above
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
  -h, --help            print this text and exit.

NOT IN THIS BUILD (the parser names the reason for each; use opet-overlay-wayland
  for the full feature set):
  no --probe                       it prints the Wayland registry - which globals are
                                   bound and at which versions. There is no registry
                                   on this platform, and an empty list called PASS
                                   is worse than no check at all.
  no --ipc   --ipc-path P          the control socket is a Unix-domain socket at
                                   $XDG_RUNTIME_DIR/opet.sock; no Windows transport
                                   has been designed for it yet.
  no --db P                        gacha persistence lives in petstore (bundled
                                   SQLite), which this executable deliberately does
                                   not link - see the crate's Cargo.toml.
  no --lang L                      this slice draws no text at all (no menu, no card,
                                   no panel), so a language has nothing to say in.
  no --motion M                    full/reduce/off gates card and panel motion; both
                                   are absent here, so the value would change nothing.
  no --no-cursor                   it works around 'the client owns the cursor image'
                                   on Wayland. Windows draws the cursor itself; there
                                   is nothing to switch off, and a flag that buys
                                   nothing is a trap waiting for the next reader.
  no --allow-unowned-accessories   both flags exempt rows of the collection panel,
  no --allow-unowned-pets          which does not exist in this build.
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
    "--seconds",
    "--monitor",
    "--panel-top",
    "--panel-bottom",
    "--seed",
];

/// 本片**明确拒绝**的开关与各自的理由。
///
/// ⚠️ 每一条理由都必须说的是"这台机器上为什么没有那件事"，不是"还没做"。
/// 后者的有效期只到下一个里程碑，而前者是平台的形状：registry 不存在、这一片屏上一个字都不画
/// 所以语言没有作用处。
///
/// 达不到那个标准的条目要**自己承认**：`--ipc` 那一条的真实理由是"Windows 侧的传输还没设计"，
/// **不是**"这个系统没有 Unix 套接字"（Win10 起 `AF_UNIX` 真在）。所以那里写的是前者。
/// 把"没设计"写成"不可能"是这类注释最容易犯的错 —— 它读起来像一个决定，实际是一张
/// 不会兑现的期票，而下一个人会照着它省掉一整段设计。
pub const REFUSED: &[(&str, &str)] = &[
    (
        "--probe",
        "打印的是 Wayland registry（绑到哪些 global、各是什么版本），这个平台上没有 registry \
         ⇒ 硬跑会给出一份空清单并称它通过",
    ),
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
        "--db",
        "抽卡与账本在 `petstore`（bundled SQLite），而本可执行文件**有意**不链它 —— \
         那会把 C 交叉编译放上关键路径（见 crates/overlay-win32/Cargo.toml 顶上那段）",
    ),
    (
        "--lang",
        "这一片屏上一个字都不画（没有菜单、没有卡面、没有面板），语言没有可作用的地方；\
         给它 = 让用户以为某处文字会跟着变",
    ),
    (
        "--motion",
        "full/reduce/off 管的是卡面与面板的动效，两者都不在本片 ⇒ 这个值改了什么都不会发生",
    ),
    (
        "--no-cursor",
        "它绕的是 Wayland 那条「客户端自己拥有光标图像、主题加载失败就隐形」的病；\
         Windows 自己画光标，这里没有可关的东西 —— 买了零东西的开关是留给下一个读者的陷阱",
    ),
    (
        "--allow-unowned-accessories",
        "豁免的是收藏册里的行，而本片没有收藏册（配件本身也不在这一片）",
    ),
    (
        "--allow-unowned-pets",
        "豁免的是收藏册里的宠物行，而本片没有收藏册：屏上是谁完全由 --manifest 定",
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
    manifest: Option<PathBuf>,
    test_rect: bool,
    seconds: Option<f32>,
    monitor: Option<String>,
    panel_top: Option<f32>,
    panel_bottom: Option<f32>,
    seed: Option<u64>,
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
        // 空值那一族（Wayland 侧是 `--db=`）：既不是"不给"也不是一个值，语义上是最坏的一种
        // "看起来开了档"。`--monitor=` 空串在 Windows 上还多一层坑：它会被大小写不敏感的
        // 前缀匹配当成"匹配所有"，于是"我没指定显示器"变成"我指定了第一台命中的"。
        if self.monitor.as_deref().is_some_and(|m| m.trim().is_empty()) {
            return Err(
                "--monitor 给了空名字；不指定显示器就别给 --monitor（默认 = 主屏）".to_string(),
            );
        }
        Ok(Options {
            manifest: self.manifest.clone(),
            test_rect: self.test_rect,
            seconds: match self.seconds {
                Some(0.0) => None,
                s => Some(s.unwrap_or(3.0)),
            },
            monitor: self.monitor.clone(),
            panel_top: self.panel_top,
            panel_bottom: self.panel_bottom,
            seed: self.seed.unwrap_or(1),
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
            "--seconds" => a.seconds = Some(num(&next_val(&mut i)?, key)?),
            "--monitor" => a.monitor = Some(next_val(&mut i)?),
            "--panel-top" => a.panel_top = Some(num(&next_val(&mut i)?, key)?),
            "--panel-bottom" => a.panel_bottom = Some(num(&next_val(&mut i)?, key)?),
            "--seed" => a.seed = Some(num(&next_val(&mut i)?, key)?),
            other => return Err(refusal_message(other)),
        }
        i += 1;
    }
    Ok(a)
}

/// `dispatch` 的分支结果，调用方据此决定退出码与输出。
///
/// 比 Wayland 那份少一档 `Probed`：`--probe` 不在本片（见 [`REFUSED`]），
/// 而**没有**给它补一个"打印显示器与 DPI"的 Windows 版 —— 那是下一片的事，
/// 现在补就是一个没有后端的旗。
pub enum Outcome {
    /// 打的是 `--help`，`String` 是用法文本
    Help(String),
    /// 真跑了一轮 overlay（正常退出或 `--seconds` 到点）
    Ran,
}

/// 解析 + 分派。`Err` = 参数/运行失败，由调用方决定退出码（2 与 1 的分工见 `src/main.rs`）。
pub fn dispatch(args: &[String]) -> Result<Outcome, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Outcome::Help(USAGE.to_string()));
    }
    let parsed = parse(args)?;
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
    /// 所以那九个被拒的旗**天然不在网里** —— [`no_refused_flag_is_advertised_as_an_option`]
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
        // 真正不认识的词，走的是另一条分支（对比下面那条：这九个字**不是**未知参数）。
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

    /// 九条拒绝必须**点名是哪个旗**、给得出**理由正文**，而且说的是"这里没有那件事"
    /// 而不是落到默认分支那句"未知参数"。
    ///
    /// 为什么不用 `未知参数` 打发它们：那九个字用户没打错，是从 Wayland 那条命令行拷来的。
    /// "未知"会让人去数字符，"这里没有 registry"会让人去问平台。
    #[test]
    fn out_of_scope_flags_are_refused_by_name_with_a_reason() {
        assert!(
            REFUSED.len() >= 9,
            "本片范围外的旗应当有 9 条，实读 {} ⇒ 有人删了一条而不是一一登记",
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

    /// 与上一条配对的方向：**被拒的旗不许长得像广告**，但**必须出现在帮助文本里**。
    ///
    /// 两半各防一种漂移。只断"在文本里"，就能把它写成 `OPTIONS:` 下的一行而让读者以为能用；
    /// 只断"不在行首"，就能把那九条从帮助里彻底删掉，于是从 Wayland 拷命令行的人只会撞到
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
