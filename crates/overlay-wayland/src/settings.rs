//! 界面生效值的**唯一**计算处：`--lang`/`--motion` > `config.toml` > `LC_ALL`/`LANG` > 默认
//! （`UI_REDESIGN_V2` §4.6 的优先级链）。
//!
//! 分成三个模块不是洁癖，是三条各有其主的规则：
//! · `petstore::config` 只管**文件语法**与路径，返回原始字符串（不解释语义 ⇒ 不产生
//!   `petstore → UI` 的反向依赖）；
//! · `l10n::Lang` 只管**词表**与它自己的码（`parse`/`code`/`self_name`）；
//! · 这里管**谁赢**，并且是**唯一**读 `LC_ALL`/`LANG` 的地方 —— 两处探测环境变量必然出现
//!   "一处认 `zh_CN` 一处不认"的那种漂移（M3 的 `clock.rs` 是同一手法：一个关注点一个出口）。
//!
//! ## 出错的两条路，形状不一样（§7-R14）
//!
//! · **命令行给错** ⇒ 返回 `Err`（`cli.rs` 据此退出）。这是历史 P2 的规矩："我显式说了 en，
//!   你给了我 zh"必须是错误，不能"降级 + 一行提示"糊过去。
//! · **文件/环境变量给错** ⇒ 出一条 `notices`（点名键名与看见的值），然后按链条往下走。
//!   这里不能退出：一条坏文件不该让宠物起不来，但**必须出声**，否则"配置支持多语言"就是
//!   一个只在 grep 里存在的功能。
//!
//! ⚠️ 本模块**不碰键盘导航**：菜单 surface 的 `keyboard_interactivity=None`（D14），
//! 所以生效语言只有"改文件/加参数 + 重启"和"点菜单底部语言行"两条路，后者只改本进程
//! （§4.6 不回写文件）。

use crate::l10n::Lang;
use petstore::config::{Config, UiKeys};

/// 生效值是从哪一层来的。**必须随值一起打印**：否则用户无法区分"我配的生效了"和
/// "恰好默认值和我配的一样"（验收 §6-4 那条优先级链就是靠这一列判的）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueSource {
    Cli,
    ConfigFile,
    EnvVar,
    Default,
}

impl ValueSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "命令行参数",
            Self::ConfigFile => "config.toml",
            Self::EnvVar => "环境变量",
            Self::Default => "默认值",
        }
    }
}

/// 动画强度档（`--motion` / `[ui] motion`）。设计书 §3.9 那条"动画强度可关 / 闪烁可关"，
/// 判据②点名要求它先于音效落地。
///
/// 分档语义定死在这里（三档各改什么、不改什么），渲染侧 [`crate::menu::anim_spec`] 是它
/// 唯一的执行者 —— 免得两处各发明一套口径。像素效果由 `card::Ctx.motion` 落地，
/// 测试 `motion_tiers_only_touch_burst_and_flash` / `motion_tiers_remove_the_white_flash_pixels`
/// 分别钉"参数表"和"真的少画了那一帧"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// 现状：burst 4/8/16/24 tick，SSR 白闪 2 tick
    Full,
    /// burst 缩到 `max(1, n/4)`，**白闪为 0**；dwell 不变（静态、基本不费电）
    Reduce,
    /// **跳过 burst**：抽取直接进 Reveal（提交数只减不增）
    Off,
}

impl Motion {
    pub const ALL: [Self; 3] = [Self::Full, Self::Reduce, Self::Off];
    /// 没给参数、文件里没写、写错了 ⇒ 这一档。`Full` = 用户已经签收过的那个观感。
    pub const DEFAULT: Self = Self::Full;

    pub const fn code(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduce => "reduce",
            Self::Off => "off",
        }
    }

    /// 只认三档，其余 `None`。与 `Lang::parse` 同一条规矩：大小写与前后空白无所谓，
    /// **但不接受缩写**（`o` 是 `off` 还是 `opet` 的什么，猜一次就可能错一辈子）。
    pub fn parse(raw: &str) -> Option<Self> {
        let v = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|m| m.code() == v)
    }
}

/// 报错/出声时把支持的语言列出来。**由 `Lang::ALL` 生成**，不在字符串里手抄一遍
/// （手抄的那一份会在加第五种语言时变成谎话）。
fn lang_codes() -> String {
    Lang::ALL
        .iter()
        .map(|l| l.code())
        .collect::<Vec<_>>()
        .join("/")
}

/// 同一条手法用在 motion 上。
fn motion_codes() -> String {
    Motion::ALL
        .iter()
        .map(|m| m.code())
        .collect::<Vec<_>>()
        .join("/")
}

/// 两句"认不出来"的报错**只在这里拼**：CLI 预检（[`validate_cli`]）与链条本体
/// （[`Settings::resolve`]）用的是同两个函数，所以"参数阶段说的支持范围"和"运行阶段说的"
/// 不可能漂移 —— 手抄两遍的话，加第五种语言时必然有一份变成谎话。
fn unknown_lang(v: &str) -> String {
    format!("--lang 的值 {v:?} 认不出来（只支持 {}）", lang_codes())
}

fn unknown_motion(v: &str) -> String {
    format!("--motion 的值 {v:?} 认不出来（只支持 {}）", motion_codes())
}

/// `--lang` / `--motion` 的**预检**，给 `cli::Args::options()` 用：参数写错要在解析阶段
/// 就退出，而不是等 `run()` 连上 Wayland、建好 surface 之后。
/// 合法与否只问 [`Lang::parse`] / [`Motion::parse`] 这一处，不在 CLI 再抄一张支持表。
pub fn validate_cli(lang: Option<&str>, motion: Option<&str>) -> Result<(), String> {
    if let Some(v) = lang {
        if Lang::parse(v).is_none() {
            return Err(unknown_lang(v));
        }
    }
    if let Some(v) = motion {
        if Motion::parse(v).is_none() {
            return Err(unknown_motion(v));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub lang: Lang,
    pub lang_from: ValueSource,
    pub motion: Motion,
    pub motion_from: ValueSource,
    /// 文件/环境变量里的值认不出来时的说明（**调用方必须打印**；见模块头的两条路）。
    pub notices: Vec<String>,
}

impl Settings {
    /// 链条本体。**纯函数**：环境变量由调用方取好传进来（`from_process` 是唯一读进程环境的地方），
    /// 这样四条分支都能在并行测试里逐条打到，而不用改进程环境（edition 2024 里那是 `unsafe`）。
    pub fn resolve(
        cli_lang: Option<&str>,
        cli_motion: Option<&str>,
        file: &UiKeys,
        env_lang: Option<&str>,
    ) -> Result<Self, String> {
        let mut notices: Vec<String> = Vec::new();

        // 先算 motion（它自己有"CLI 给错 = 错误"这条），再算 lang —— 两层的坏值都只可能出
        // 现在文件/环境变量那两档，notice 顺序因此固定，测试能数得清。
        let (motion, motion_from) = resolve_motion(cli_motion, file, &mut notices)?;

        // ── lang：CLI → 文件 → 环境变量 → 默认
        let mut lang = Lang::DEFAULT;
        let mut lang_from = ValueSource::Default;

        if let Some(v) = cli_lang {
            // 显式给错 = 错误，不是降级（见模块头）。
            match Lang::parse(v) {
                Some(l) => (lang, lang_from) = (l, ValueSource::Cli),
                None => return Err(unknown_lang(v)),
            }
        } else {
            // **逐级往下**：文件里是坏值 ⇒ 出声后继续试环境变量，最后才默认。停在坏值上直接
            // 用默认，会把"桌面语言本来是日文"这个仍然可用的信号扔掉。
            let chain = [
                (
                    file.lang.as_deref(),
                    ValueSource::ConfigFile,
                    "config.toml 的 [ui] lang",
                ),
                (env_lang, ValueSource::EnvVar, "环境变量 LC_ALL/LANG"),
            ];
            for (raw, from, name) in chain {
                let Some(v) = raw else { continue };
                match Lang::parse(v) {
                    Some(l) => {
                        lang = l;
                        lang_from = from;
                        break;
                    }
                    // `LC_ALL=C` 是真会出现的（VM、CI、别的发行版），静默落 zh 就永远没人知道
                    None => notices.push(format!(
                        "{name} = {v:?} 认不出来（只支持 {}）⇒ 跳过这一级",
                        lang_codes()
                    )),
                }
            }
        }

        Ok(Self {
            lang,
            lang_from,
            motion,
            motion_from,
            notices,
        })
    }

    /// 从真实进程环境取 `LC_ALL` → `LANG`（前者按 POSIX 约定覆盖后者），再走 [`Self::resolve`]。
    pub fn from_process(
        cli_lang: Option<&str>,
        cli_motion: Option<&str>,
        cfg: &Config,
    ) -> Result<Self, String> {
        let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        let env_lang = grab("LC_ALL").or_else(|| grab("LANG"));
        Self::resolve(cli_lang, cli_motion, &cfg.ui, env_lang.as_deref())
    }

    /// 一行生效值摘要（启动时打印；验收 §6-4 读的就是这一行）。
    pub fn summary(&self) -> String {
        format!(
            "ui: lang = {}（{}，自名 {}）｜ motion = {}（{}）",
            self.lang.code(),
            self.lang_from.as_str(),
            self.lang.self_name(),
            self.motion.code(),
            self.motion_from.as_str(),
        )
    }

    /// 把生效值与所有出声条目打到 stderr。顺序刻意是"**先摘要后抱怨**"：真机排查时第一行
    /// 就要看到生效值，而坏值说明是次要证据。
    pub fn announce(&self) {
        eprintln!("{}", self.summary());
        for n in &self.notices {
            eprintln!("ui: {n}");
        }
    }
}

/// `motion` 只有 CLI → 文件 → 默认三档（**没有环境变量这一级**：`LC_*` 里没有任何字段
/// 表达"动画强度"，硬编一个 `OPET_MOTION` 就是承诺一个没人约定的接口）。
fn resolve_motion(
    cli: Option<&str>,
    file: &UiKeys,
    notices: &mut Vec<String>,
) -> Result<(Motion, ValueSource), String> {
    if let Some(v) = cli {
        return match Motion::parse(v) {
            Some(m) => Ok((m, ValueSource::Cli)),
            None => Err(unknown_motion(v)),
        };
    }
    if let Some(v) = &file.motion {
        return match Motion::parse(v) {
            Some(m) => Ok((m, ValueSource::ConfigFile)),
            None => {
                notices.push(format!(
                    "config.toml 里 [ui] motion = {v:?} 认不出来（只支持 {}）⇒ 用默认 {}",
                    motion_codes(),
                    Motion::DEFAULT.code()
                ));
                Ok((Motion::DEFAULT, ValueSource::Default))
            }
        };
    }
    Ok((Motion::DEFAULT, ValueSource::Default))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(lang: Option<&str>, motion: Option<&str>) -> UiKeys {
        UiKeys {
            lang: lang.map(String::from),
            motion: motion.map(String::from),
        }
    }

    /// 优先级链逐层各测一次。**每一层都要同时给出"值"和"来源"**，只比值的话第 1 层和第 4 层
    /// 都落在 `zh` 上，测试会绿得什么都没验。
    #[test]
    fn cli_beats_file_beats_env_beats_default() {
        let f = file(Some("en"), None);

        let s = Settings::resolve(Some("ja"), None, &f, Some("ko_KR")).unwrap();
        assert_eq!((s.lang, s.lang_from), (Lang::Ja, ValueSource::Cli));

        let s = Settings::resolve(None, None, &f, Some("ko_KR")).unwrap();
        assert_eq!((s.lang, s.lang_from), (Lang::En, ValueSource::ConfigFile));

        let s = Settings::resolve(None, None, &file(None, None), Some("ja_JP.UTF-8")).unwrap();
        assert_eq!((s.lang, s.lang_from), (Lang::Ja, ValueSource::EnvVar));

        let s = Settings::resolve(None, None, &file(None, None), None).unwrap();
        assert_eq!(
            (s.lang, s.lang_from),
            (Lang::DEFAULT, ValueSource::Default),
            "什么都没给才落默认"
        );
        assert!(s.notices.is_empty(), "{:?}", s.notices);
    }

    /// 文件里的坏值：**出声 + 按链条往下**，不是退出、也不是静默当默认。
    #[test]
    fn unrecognized_file_value_is_announced_then_falls_through() {
        let s = Settings::resolve(None, None, &file(Some("fr"), None), Some("ko")).unwrap();
        assert_eq!(
            (s.lang, s.lang_from),
            (Lang::Ko, ValueSource::EnvVar),
            "文件坏了要落到下一级，而不是跳到默认"
        );
        assert_eq!(s.notices.len(), 1, "{:?}", s.notices);
        assert!(
            s.notices[0].contains("fr"),
            "要点名看见的值：{}",
            s.notices[0]
        );
        assert!(
            s.notices[0].contains("lang"),
            "要点名键名：{}",
            s.notices[0]
        );
    }

    /// `LC_ALL=C` / `POSIX` 在 VM、CI、别的发行版上都会遇到：落默认可以，**静默不行**。
    #[test]
    fn an_unrecognized_env_tag_is_announced_not_swallowed() {
        for tag in ["C", "POSIX", "fr_FR.UTF-8", "zh_TW"] {
            let s = Settings::resolve(None, None, &file(None, None), Some(tag)).unwrap();
            // `zh_TW` 落简体是**明写的限制**（本轮只有简体），但认得出来所以不算坏值
            if tag == "zh_TW" {
                assert_eq!(s.lang, Lang::Zh, "{tag} 应当认得");
                assert!(s.notices.is_empty(), "{:?}", s.notices);
                continue;
            }
            assert_eq!(s.lang, Lang::DEFAULT, "{tag} 该落默认");
            assert_eq!(s.lang_from, ValueSource::Default);
            assert_eq!(s.notices.len(), 1, "{tag} 静默退回默认 = 假功能：{s:?}");
            assert!(
                s.notices[0].contains(tag),
                "要打印看见了什么：{}",
                s.notices[0]
            );
        }
    }

    /// 命令行给错必须是 `Err`（沿用历史 P2 的规矩：显式说错了就别猜）。
    #[test]
    fn bad_cli_values_are_errors_not_fallbacks() {
        let e = Settings::resolve(Some("fr"), None, &file(None, None), None).unwrap_err();
        assert!(
            e.contains("--lang") && e.contains("fr") && e.contains("zh/en/ja/ko"),
            "{e}"
        );

        let e = Settings::resolve(None, Some("nope"), &file(None, None), None).unwrap_err();
        assert!(
            e.contains("--motion") && e.contains("nope") && e.contains("full/reduce/off"),
            "{e}"
        );

        // 而文件里同样的坏值**不**是错误（不能让一条坏文件拦住宠物启动）
        let s = Settings::resolve(None, None, &file(None, Some("nope")), None).unwrap();
        assert_eq!(
            (s.motion, s.motion_from),
            (Motion::DEFAULT, ValueSource::Default)
        );
        assert_eq!(s.notices.len(), 1, "{:?}", s.notices);
    }

    #[test]
    fn motion_chain_has_no_env_level() {
        let s = Settings::resolve(
            None,
            Some("off"),
            &file(Some("en"), Some("reduce")),
            Some("fr"),
        )
        .unwrap();
        assert_eq!((s.motion, s.motion_from), (Motion::Off, ValueSource::Cli));
        assert_eq!((s.lang, s.lang_from), (Lang::En, ValueSource::ConfigFile));
        // CLI 给全 ⇒ 文件里的 fr 根本不会被读，不该有 notice
        assert!(s.notices.is_empty(), "{:?}", s.notices);

        // 环境变量不参与 motion：`LANG` 是语言标签，不是动画档
        let s = Settings::resolve(None, None, &file(None, None), Some("ja_JP.UTF-8")).unwrap();
        assert_eq!(
            (s.motion, s.motion_from),
            (Motion::Full, ValueSource::Default)
        );

        for code in ["full", "reduce", "off", "FULL", " Off "] {
            assert!(Motion::parse(code).is_some(), "{code} 该认得");
        }
        for code in ["", " ", "o", "Off2", "high"] {
            assert_eq!(Motion::parse(code), None, "{code:?} 不该被猜成任何一档");
        }
    }

    /// 摘要行是**验收时人要读的**（§6-4），所以它的格式也要有测试钉住 —— 不然下一次改
    /// `summary` 会把"来源"这一列悄悄删掉，用户就分不清生效值是从哪层来的了。
    #[test]
    fn summary_names_both_values_and_their_sources() {
        let s = Settings::resolve(Some("en"), Some("reduce"), &file(None, None), None).unwrap();
        let line = s.summary();
        for needle in ["lang = en", "命令行参数", "motion = reduce", "English"] {
            assert!(line.contains(needle), "摘要缺 {needle}：{line}");
        }
        // 配置文件来源要说得出是 config.toml（用户判优先级链就看这一列）
        let s = Settings::resolve(None, None, &file(Some("ko"), Some("off")), None).unwrap();
        let line = s.summary();
        assert!(
            line.contains("config.toml") && line.contains("한국어"),
            "{line}"
        );
    }
}
