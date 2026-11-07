//! 命令行入口的**共用**部分：`opet-overlay-wayland` 独立二进制与 `opet overlay` 都走这里，
//! 免得 hand-rolled 解析写两份（口径与 `opet-cli` 一致：不引 clap）。

use crate::app::{self, Options};
use crate::probe;
use crate::settings;

pub const USAGE: &str = "\
opet overlay [--probe | --seconds N] …（独立二进制 opet-overlay-wayland 同参数）

  --probe            只读打印 Wayland registry 与显示器几何，不映射任何 surface
  --seconds N        运行 N 秒后自动退出（默认 3；0 = 一直跑）
  --manifest FILE    宠物清单（pet.json）；不给 = 内核内置 fixture（无像素，画不出东西）
  --monitor NAME     指定舞台显示器
  --panel-bottom N   底边面板高度（逻辑像素，默认 0；决定地面往上抬多少）
  --panel-top N      顶边栏高度（逻辑像素，默认 0）。layer-shell 的 margin 从可用区量起，
                     顶部有栏时不给这个值 ⇒ 宠物整体下移一个栏高、脚掉到屏幕外
  --seed N           内核随机种子（默认 1）
  --db PATH          抽取落库的 sqlite 存档（绝对路径）。**不给就是内存库**：菜单与结果卡
                     一律打 DRYRUN 标、不产生任何文件 —— 与 `opet pull` 的默认存档红线同族，
                     别让一条随手起的 overlay 写脏用户真存档
  --test-rect        不接内核，画一块纯色矩形（验证 surface 映射与点击穿透）
  --no-cursor        不设光标表面（调试用；代价是鼠标在宠物上方可能隐形）
  --lang CODE        界面语言：zh / en / ja / ko。不给 = 走 `config.toml` 的 `[ui] lang`，
                     再退 `LC_ALL`/`LANG`，最后默认 zh。**认不出来直接报错退出**，
                     不静默退回默认（打错了和「语言不对但没人说」是两种完全不同的故障）
  --motion TIER      抽卡动画强度：full / reduce / off。reduce = 开箱扩散缩到 1/4 且**不闪白**，
                     off = 整个跳过扩散（只减提交、不增）；两档都不动 dwell 与稀有度色带，
                     也**完全不碰本体动画**（宠物该走还是走）。
                     判据②「闪烁可关」就是这一条 —— 前庭敏感是硬需求，优先于音效

配件（换装）：命令行**没有**开关，只有屏上那三行（换一件 / 穿上 / 脱下）。
  库里有什么由启动时扫目录得出并逐件打进 stderr。库的位置只由环境决定：
    $OPET_ACCESSORIES → $XDG_DATA_HOME/opet/accessories（仅当它是绝对路径）
    → $HOME/.local/share/opet/accessories
  一件配件 = 该目录下一个子目录里的三件套 `accessory.json` + `sheet.webp` + `polygon.json`
  （生产者 = `tools/pack.py --accessory … --body <本体 pet.json>`）。清单逐项对齐本体
  （格子/列数/行表/fps/循环标志），对不上就**点名是哪一档漂移并跳过这一件** —— 错位画在
  屏上只是「帽子歪了」，永远不会有报错，所以只能在装备那一刻判（D16 路线 B 的地基）。
  与旧版唯一不同：坏的一件被跳过而**不影响其余**，宠物照常起（硬约束 3）。

  --allow-unowned-accessories
                     开发态豁免：**没抽到、没装包得到的配件也进换装页、也能穿**。默认不开
                     （用户 2026-11-05 第 3 问拍板 = 玩家不能穿未拥有）。开着的每一轮，
                     帽子页标题带**尾随一个 `*` 并染成警示色** ⇒ 屏上永远读得出「这轮是豁免态」，
                     不打标的放行就是假门禁（`--cell` 假开关那一族）。
                     不给 `--db` 时这条**不生效也不需要**：内存库 ⇒ 库存恒空 ⇒ 那是演示态，
                     本来就放行，由标题行右端已有的「演示」标承担（同 DRYRUN 那一族）。

本体（换宠物）：库位置同样**只由环境决定**，命令行没有第二个入口（`--accessory` 已被删过
  的那条先例）：$OPET_PETS → $XDG_DATA_HOME/opet/pets（仅当它是绝对路径）
  → $HOME/.local/share/opet/pets。一只本体 = 一个子目录里的 `pet.json` + `sheet.webp` +
  `polygon.json`（生产者 = `tools/pack.py`）。收藏册里每一只宠物一行，点它**当场换成那只**；
  格档与当前本体不同的那一只在**启动扫盘那一步**就出局（运行时不动 surface 尺寸，跨档只能
  重启），出局原因逐只点名进 stderr ⇒ 那一行在收藏册里是只读行，不是「点了被拒」。
  ⚠️ 换本体**不动 `--manifest`**：那条仍是硬要求，它现在的意思是「库里没有 body 行时用什么」。

  --allow-unowned-pets
                     开发态豁免：**没抽到的宠物也点得动、也能换**。默认不开（与上面那条同一个
                     拍板）。**故意与 `--allow-unowned-accessories` 分成两条**：那条的屏上记号是
                     帽子页标题的 `*`，而帽子页的列表里从头就没有宠物 ⇒ 拿一条开关管两类，就会
                     出现「标打了而没放行」或「放行了而没有标」（本项目登记过的同一族）。
                     不给 `--db` 时这条同样**不生效也不需要**（演示态一律放行）。
";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub probe: bool,
    pub seconds: Option<f32>,
    pub manifest: Option<String>,
    pub monitor: Option<String>,
    pub panel_bottom: Option<f32>,
    pub panel_top: Option<f32>,
    pub seed: Option<u64>,
    /// sqlite 存档路径；`None` = 内存库（UI 打 DRYRUN 标，不写盘）
    pub db: Option<String>,
    pub test_rect: bool,
    pub no_cursor: bool,
    /// `--lang` 的原始值。合法性由 [`settings::validate_cli`] 判（**不在这里抄一张表**），
    /// 解释（大小写/子标签/优先级链）更不在这里 —— 那是 `settings` 唯一的一份口径。
    pub lang: Option<String>,
    pub motion: Option<String>,
    /// 开发态豁免：未拥有的配件也进换装页。`--db` 没给时不生效（演示态本来就放行）。
    pub allow_unowned_accessories: bool,
    /// 开发态豁免：未拥有的宠物也能换。**与上面那条分开**是拍板④：那条的屏上记号（帽子页的
    /// `*`）作用域里没有宠物，合并就会"标打了而没放行"。`--db` 没给时同样不生效。
    pub allow_unowned_pets: bool,
}

impl Args {
    pub fn options(&self) -> Result<Options, String> {
        // **放在最前面**：一个打错的语言码比"没给 manifest"更具体，也更容易是用户真正想问的事。
        // `--lang=` / `--motion=` 空值与任何不认识的值一样：**报错，不静默退默认**（R14）。
        // 这一族最早是 `--seconds -5` 与 `--db=` 那两条立下的规矩：宁可退出，也不留一个
        // "看起来开了档但其实没用上"的会话。
        // 合法值表只有 `settings` 一份（CLI 不抄第二份，否则加第五种语言时这里必然滞后）。
        settings::validate_cli(self.lang.as_deref(), self.motion.as_deref())?;
        if !self.test_rect && self.manifest.is_none() {
            return Err(
                "没给 --manifest 就没有像素可画。要么给清单，要么用 --test-rect 只验 surface。\n\
                 （内置 fixture 只有元数据，M1 时代没有图集）"
                    .to_string(),
            );
        }
        // `--seconds 0` = 一直跑；**负数必须是错**，不能悄悄变成"永远跑" —— live 会话上
        // 一个赖着不走的 overlay 正是我们给自己立的红线。
        if self.seconds.is_some_and(|s| s < 0.0) {
            return Err(format!(
                "--seconds 不能是负数（给了 {:?}）；要一直跑就写 0",
                self.seconds.unwrap_or(0.0)
            ));
        }
        let pb = self.panel_bottom.unwrap_or(0.0);
        if pb < 0.0 {
            return Err(format!("--panel-bottom 不能是负数（给了 {pb}）"));
        }
        let pt = self.panel_top.unwrap_or(0.0);
        if pt < 0.0 {
            return Err(format!("--panel-top 不能是负数（给了 {pt}）"));
        }
        // `--db=` 空值：既不是"不给"（内存库）也不是路径，语义上是最坏的一种"看起来开了档"。
        if self.db.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return Err(
                "--db 给了空路径；不写真存档就别给 --db（默认是内存库 + DRYRUN 标）".to_string(),
            );
        }
        // 配件不再从这里进来（`--accessory` 已删，用户 2026-11-02 原话"配件不是应该由菜单选择吗，
        // 这也不合理"）。⇒ 这一层没有任何配件相关的分支可写：**库里少一件、坏一件都不会让进程
        // 在这里拒绝启动**，那是 `app::discover_accessories` 的 stderr。留下的唯一形状是
        // `--test-rect`：那条路径一行资产都不读，而配件是**叠在本体像素上**的 ⇒ 没有本体就没有
        // 可叠的对象。这一条不靠命令行判，靠 `app::run()` 里 `body_regions` 是 `None`（见那里）。
        Ok(Options {
            manifest: self.manifest.clone(),
            seconds: match self.seconds {
                Some(0.0) => None,
                s => Some(s.unwrap_or(3.0)),
            },
            monitor: self.monitor.clone(),
            panel_bottom: pb,
            panel_top: pt,
            seed: self.seed.unwrap_or(1),
            db: self.db.clone(),
            test_rect: self.test_rect,
            no_cursor: self.no_cursor,
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
            // 值长得像 flag ⇒ 说明用户漏写了值（`--monitor --probe`）。吞掉它的话
            // `--probe` 就永远不会执行而是起一个 surface —— live 会话上最坏的一种错。
            // 负数是合法数值（`--seconds -5` 由 options() 明确拒绝），不能一律当 flag。
            if v.starts_with('-') && v.parse::<f64>().is_err() {
                return Err(format!("{key} 缺少参数值（把 {v} 当成了值）"));
            }
            Ok(v)
        };
        match key {
            "--seconds" => a.seconds = Some(num(&next_val(&mut i)?, key)?),
            "--panel-bottom" => a.panel_bottom = Some(num(&next_val(&mut i)?, key)?),
            "--panel-top" => a.panel_top = Some(num(&next_val(&mut i)?, key)?),
            "--seed" => a.seed = Some(num(&next_val(&mut i)?, key)?),
            "--manifest" => a.manifest = Some(next_val(&mut i)?),
            "--monitor" => a.monitor = Some(next_val(&mut i)?),
            "--db" => a.db = Some(next_val(&mut i)?),
            "--lang" => a.lang = Some(next_val(&mut i)?),
            "--motion" => a.motion = Some(next_val(&mut i)?),
            "--probe" => a.probe = true,
            "--test-rect" => a.test_rect = true,
            "--no-cursor" => a.no_cursor = true,
            "--allow-unowned-accessories" => a.allow_unowned_accessories = true,
            "--allow-unowned-pets" => a.allow_unowned_pets = true,
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

/// `--probe` 的通过判定：连得上会话，且有 `zwlr_layer_shell_v1`。
pub fn probe_ok(report: &probe::ProbeReport) -> bool {
    report.session_error.is_none() && report.has("zwlr_layer_shell_v1")
}

/// `dispatch` 的分支结果，调用方据此决定退出码与输出。
pub enum Outcome {
    /// 打的是 `--help`，`String` 是用法文本
    Help(String),
    /// 跑过探测：`bool` = 通过与否
    Probed(bool),
    /// 真跑了一轮 overlay（正常退出或 `--seconds` 到点）
    Ran,
}

/// 解析 + 分派。`Err` = 参数/运行失败，由调用方决定退出码。
pub fn dispatch(args: &[String]) -> Result<Outcome, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Outcome::Help(USAGE.to_string()));
    }
    let parsed = parse(args)?;
    if parsed.probe {
        // 探测本身不吃语言/动画档，但**不能因此放过打错的值**：`--probe --lang=jap` 静默成功
        // 就等于告诉用户"这个码是对的"，而他下次真起 overlay 时看到的仍是默认档。
        // `options()` 在探测路径上会因"没给 manifest"先炸，所以这里单独预检一次。
        settings::validate_cli(parsed.lang.as_deref(), parsed.motion.as_deref())?;
        let report = probe::probe();
        print!("{}", probe::format_report(&report));
        return Ok(Outcome::Probed(probe_ok(&report)));
    }
    app::run(parsed.options()?).map(|()| Outcome::Ran)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn both_spellings_of_flag_value_are_equal() {
        let a = parse(&v(&["--seconds=7", "--seed=42", "--monitor=eDP-1"])).unwrap();
        let b = parse(&v(&[
            "--seconds",
            "7",
            "--seed",
            "42",
            "--monitor",
            "eDP-1",
        ]))
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.seconds, Some(7.0));
        assert_eq!(a.seed, Some(42));
    }

    #[test]
    fn missing_value_is_an_error_not_a_panic() {
        assert!(parse(&v(&["--seconds"])).is_err());
        assert!(parse(&v(&["--nope"])).is_err());
        assert!(parse(&v(&["--seconds", "abc"])).is_err());
    }

    #[test]
    fn seconds_zero_means_forever() {
        let a = parse(&v(&["--test-rect", "--seconds", "0"])).unwrap();
        assert_eq!(a.options().unwrap().seconds, None);
        // 不给 seconds ⇒ 默认 3 秒（live 会话上绝不能无限赖着）
        let b = parse(&v(&["--test-rect"])).unwrap();
        assert_eq!(b.options().unwrap().seconds, Some(3.0));
    }

    #[test]
    fn no_manifest_no_test_rect_is_refused() {
        // 否则会映射一块永远不提交的空 surface
        let a = parse(&v(&[])).unwrap();
        assert!(a.options().is_err());
    }

    /// **`--accessory` 已经不存在**（用户 2026-11-02 原话"配件不是应该由菜单选择吗，这也不合理"
    /// ⇒ 菜单是选配件的唯一入口）。删掉一个开关最容易留下的形状 = 帮助里还写着它，而解析器
    /// 已经拒绝它，或反过来解析器还认而文档不说。所以两条都断：命令行走老命令必须**报错而不
    /// 是静默忽略**（"我给了配件而它没穿"与"我没给"在屏上完全一样），而 USAGE 里不许再出现
    /// 以 `--accessory` 开头的条目。
    ///
    /// 顺带把老命令的另外两种形状也钉住：它们原来各有一条专属拒绝，现在都归"未知参数"这一条。
    #[test]
    fn the_removed_accessory_flag_fails_loudly() {
        for args in [
            v(&["--manifest", "a.json", "--accessory", "b.json"]),
            v(&["--manifest=a.json", "--accessory=b.json"]),
            v(&["--test-rect", "--accessory=b.json"]),
            v(&["--manifest", "a.json", "--accessory"]),
        ] {
            let err = parse(&args)
                .err()
                .unwrap_or_else(|| panic!("`--accessory` 已经删掉，{args:?} 不该还能解析成功"));
            assert!(err.contains("未知参数"), "{args:?} ⇒ {err}");
            assert!(err.contains("--accessory"), "错误要点名是哪个开关: {err}");
        }
        assert!(
            !USAGE
                .lines()
                .any(|l| l.trim_start().starts_with("--accessory")),
            "帮助里不许再广告一个解析器不认的开关"
        );
    }

    /// **帮助里广告的每一个开关都真的存在** —— 本项目吃过 `--cell` 那条假开关（`--help` 里挂了
    /// 一整轮而两处实现都拒收非默认值）。这条门禁把它换成可判的形状：USAGE 每一行**行首**那
    /// 个 `--flag` 都必须被解析器认识（值缺失、值非法都不算，只算"这个开关存不存在"）。
    ///
    /// 只查行首：正文里提到的 `tools/pack.py --accessory` 是**生产者**的开关，不是这条命令的。
    #[test]
    fn every_flag_advertised_in_usage_exists() {
        let advertised = USAGE
            .lines()
            .map(str::trim_start)
            .filter_map(|l| l.split_whitespace().next())
            .filter(|t| t.starts_with("--"))
            .collect::<Vec<_>>();
        assert!(
            advertised.len() >= 10,
            "一条都没扫到 ⇒ 这条门禁白跑: {advertised:?}"
        );
        for flag in advertised {
            let err = parse(&v(&[flag])).err().unwrap_or_default();
            assert!(
                !err.contains("未知参数"),
                "USAGE 广告了 {flag} 而解析器不认: {err}"
            );
        }
    }

    /// `--allow-unowned-accessories` 必须真的走到 `Options`，且**默认是关的**。
    ///
    /// 这条为什么不能只靠 `every_flag_advertised_in_usage_exists`：那条证明的是"解析器不拒绝
    /// 它"，而"不拒绝"与"下游有人读"是两件事 —— `--cell` 那轮的形状就是帮助里在、解析器认、
    /// 而两处实现都不读那个值。这一条把最后一段（解析 → `Options`）接上；再下游（可见集与屏上
    /// 那个 `*`）由 `app::tests` 与 `card::tests` 各断一段。
    #[test]
    fn allow_unowned_defaults_off_and_reaches_options() {
        let off = parse(&v(&["--test-rect"])).unwrap().options().unwrap();
        assert!(
            !off.allow_unowned_accessories,
            "默认必须关着 ⇒ 放行未拥有的配件是要显式给的决定（用户 2026-11-05 第 3 问）"
        );
        let on = parse(&v(&["--test-rect", "--allow-unowned-accessories"]))
            .unwrap()
            .options()
            .unwrap();
        assert!(on.allow_unowned_accessories);
    }

    /// 拍板④：`--allow-unowned-pets` 是**第二条**开关，且与配件那条**互不牵连**。
    ///
    /// 为什么单独立一条而不是把上面那条改成两个布尔：合并的后果不是"少一行断言"而是"少一件事
    /// 的读数" —— 帽子页那个 `*` 的判据读 `allow_unowned_accessories`，而列表里根本没有宠物，
    /// 所以"给了 pets 而屏上打星号"或"给了 accessories 而宠物点不动"都是拿一条开关冒充两件事。
    /// 这两格真值表只有分别构造两次输入才打得红。
    #[test]
    fn the_two_unowned_switches_are_independent_and_default_off() {
        let off = parse(&v(&["--test-rect"])).unwrap().options().unwrap();
        assert!(
            !off.allow_unowned_pets,
            "默认必须关着 ⇒ 放行未拥有的宠物同样是要显式给的决定"
        );

        let only_pets = parse(&v(&["--test-rect", "--allow-unowned-pets"]))
            .unwrap()
            .options()
            .unwrap();
        assert!(only_pets.allow_unowned_pets);
        assert!(
            !only_pets.allow_unowned_accessories,
            "只放行宠物不该顺手放行配件 ⇒ 帽子页那个 `*` 不该因此点亮"
        );

        let only_acc = parse(&v(&["--test-rect", "--allow-unowned-accessories"]))
            .unwrap()
            .options()
            .unwrap();
        assert!(only_acc.allow_unowned_accessories);
        assert!(
            !only_acc.allow_unowned_pets,
            "只放行配件不该顺手放行本体 ⇒ 反过来同样是两个事实源"
        );
    }

    #[test]
    fn negative_seconds_is_refused_not_treated_as_forever() {
        // live 会话红线：`--seconds -5` 绝不能静默变成"永远跑"
        let a = parse(&v(&["--test-rect", "--seconds", "-5"])).unwrap();
        assert!(a.options().is_err(), "负数秒必须报错");
        let b = parse(&v(&["--test-rect", "--panel-bottom", "-1"])).unwrap();
        assert!(b.options().is_err(), "负的面板高度没有意义");
        let c = parse(&v(&["--test-rect", "--panel-top", "-1"])).unwrap();
        assert!(c.options().is_err(), "负的栏高没有意义");
    }

    #[test]
    fn panel_top_reaches_the_options() {
        // 本机顶栏 20 逻辑像素：不给这个值 ⇒ 宠物整体下移 20，脚掉到屏幕外
        let a = parse(&v(&["--test-rect", "--panel-top=20"])).unwrap();
        assert_eq!(a.options().unwrap().panel_top, 20.0);
        assert_eq!(
            parse(&v(&["--test-rect"]))
                .unwrap()
                .options()
                .unwrap()
                .panel_top,
            0.0
        );
    }

    #[test]
    fn a_flag_never_swallow_the_next_flag_as_its_value() {
        // `--monitor --probe` 若把 `--probe` 当值吞掉，probe 就不会执行而是起 surface
        assert!(parse(&v(&["--monitor", "--probe"])).is_err());
        assert!(parse(&v(&["--manifest", "--seconds", "3"])).is_err());
        // 但 `=` 写法允许值本身带横杠（负数由 options() 那层拒绝）
        assert_eq!(
            parse(&v(&["--monitor=-L1"])).unwrap().monitor,
            Some("-L1".to_string())
        );
    }

    #[test]
    fn writing_to_disk_is_opt_in() {
        // 不给 `--db` ⇒ `None`，走内存库 + DRYRUN 标，**不产生任何文件**。
        // 这条和 `opet pull` 的默认存档门禁同族：一条随手起的 overlay 不该写脏真存档。
        assert_eq!(
            parse(&v(&["--test-rect"])).unwrap().options().unwrap().db,
            None
        );
        let eq = parse(&v(&["--test-rect", "--db=/tmp/opet_m3b.db"]))
            .unwrap()
            .options()
            .unwrap();
        let sp = parse(&v(&["--test-rect", "--db", "/tmp/opet_m3b.db"]))
            .unwrap()
            .options()
            .unwrap();
        assert_eq!(eq.db.as_deref(), Some("/tmp/opet_m3b.db"));
        assert_eq!(eq.db, sp.db, "两种写法必须同义");
        // 空值是第三种"看起来开了档"：既不是内存库也不是路径 ⇒ 必须报错
        assert!(
            parse(&v(&["--test-rect", "--db="]))
                .unwrap()
                .options()
                .is_err()
        );
    }

    /// `--lang` / `--motion` 走到底：**认得出来的值原样交给 `Options`**（解释在
    /// `settings` 那一处），认不出来的一律报错退出，**不静默退回默认档**。
    /// 静默退默认 = "我明明说了日文，卡片还是英文，而全程没有任何一句话" —— 这一族
    /// 和 `--db=` 空值、`--seconds -5` 是同一条红线（R14）。
    #[test]
    fn lang_and_motion_reach_the_options_verbatim() {
        let eq = parse(&v(&["--test-rect", "--lang=en", "--motion=reduce"]))
            .unwrap()
            .options()
            .unwrap();
        assert_eq!(eq.lang.as_deref(), Some("en"));
        assert_eq!(eq.motion.as_deref(), Some("reduce"));
        // 两种写法同义（同 `--db`）
        let sp = parse(&v(&["--test-rect", "--lang", "en", "--motion", "reduce"]))
            .unwrap()
            .options()
            .unwrap();
        assert_eq!(sp.lang, eq.lang, "两种写法必须同义");
        assert_eq!(sp.motion, eq.motion);
        // 不给 ⇒ `None`（= 让下一级说话），这一条必须测：把默认写成 `Some("zh")`
        // 就会永久盖掉 config.toml 与环境变量两级。
        let none = parse(&v(&["--test-rect"])).unwrap().options().unwrap();
        assert_eq!(none.lang, None);
        assert_eq!(none.motion, None);
    }

    #[test]
    fn bad_lang_or_motion_is_an_error_and_never_a_silent_default() {
        for bad in [
            "--lang=zh-tw", // 子标签本身能解析出 zh（合法），先测真正认不出的
            "--lang=zz",
            "--lang=",
            "--motion=high",
            "--motion=",
            "--motion=off2",
        ] {
            let args = v(&["--test-rect", bad]);
            let got = parse(&args).unwrap().options();
            if bad == "--lang=zh-tw" {
                // `zh_TW` 是**合法**的（落简体，本轮只有简体文案）⇒ 只有真认不出的才炸
                assert!(got.is_ok(), "{bad} 不该被拒：{got:?}");
                continue;
            }
            let Err(msg) = got else {
                panic!("「{bad}」被静默接受了 ⇒ 用户看不到自己写错了");
            };
            // 报错必须点名是哪个开关、并列出支持范围（否则用户只能翻源码）
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

    /// 值长得像 flag 的那条老规矩对两个新开关同样成立：`--lang --probe` 吞掉 `--probe`
    /// 就等于"探测变成起 surface"，是 live 会话上最坏的一种错。
    #[test]
    fn lang_and_motion_never_swallow_the_next_flag() {
        assert!(parse(&v(&["--lang", "--probe"])).is_err());
        assert!(parse(&v(&["--motion", "--probe"])).is_err());
        assert!(parse(&v(&["--lang"])).is_err(), "缺值也要报错");
    }

    /// `--probe` 这条路径**不渲染任何东西**，所以它绕过 `options()`（那会因"没 manifest"先炸），
    /// 但**不能因此放过打错的档**：`--probe --lang=jap` 静默成功 = 对用户说谎（他下次真起
    /// overlay 时看到的仍是默认档）。R14 的口径是"任何一条路径都不静默退默认"，不是
    /// "只有会画像素的那条不静默"。
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
        // 合法值在探测路径上必须放行（否则这条预检就把诊断功能给堵死了）
        assert!(matches!(
            dispatch(&v(&["--probe", "--lang=en", "--motion=reduce"])),
            Ok(Outcome::Probed(_))
        ));
    }
}
