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
//! 2026-11-22（M5 第二批拍板③）：`Motion`/`ValueSource` 的类型本体、`validate_cli` 预检与
//! 两句"认不出来"的报错**上提到 `overlay_common::ui::settings`**（Win32 客户端要用同一套
//! 档位语义和同一份措辞），本文件只剩链条，靠再导出接回 —— 所以下面 `crate::settings::…`
//! 这些既有路径一个都没变。
//!
//! ## 出错的两条路，形状不一样（§7-R14）
//!
//! · **命令行给错** ⇒ 返回 `Err`（`cli.rs` 据此退出）。这是历史 P2 的规矩："我显式说了 en，
//!   你给了我 zh"必须是错误，不能"降级 + 一行提示"糊过去。
//! · **文件/环境变量给错** ⇒ 出一条 `notices`（点名键名与看见的值），然后按链条往下走。
//!   这里不能退出：一条坏文件不该让宠物起不来，但**必须出声**，否则"配置支持多语言"就是
//!   一个只在 grep 里存在的功能。
//!
//! ⚠️ 本模块**不碰键盘导航**：菜单与面板的 surface 都是 `keyboard_interactivity=None`（D14），
//! 所以生效语言只有"改文件/加参数 + 重启"和"点面板上的语言/动画行"两条路。
//!
//! **第二条路现在会写回文件**（拍板④「设置功能……点击菜单弹出窗口……并实现功能」）。旧版这里
//! 写的是"只改本进程（§4.6 不回写文件）"，那条理由 = 当时只有自写的平坦 reader，读改写会把
//! 用户注释刷平；`petstore::config::write_ui_key`（`toml_edit` 保 decor + 落盘前后各用**我们的**
//! reader 判一遍）把那个理由消掉了 ⇒ 写回的职责在 `app::App::persist_ui_key`，**不在本模块**：
//! 这里只管"谁赢"这条链，而链条本身的行为一个字没改（文件仍是 CLI 之下、环境变量之上）。

use crate::l10n::Lang;
use petstore::config::{Config, UiKeys};

pub use overlay_common::ui::settings::{
    Motion, ValueSource, lang_codes, motion_codes, unknown_lang, unknown_motion, validate_cli,
};

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
                    // `LC_ALL=C` 是真会出现的（VM、CI、别的发行版），静默落 en 就永远没人知道
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
        // 点行那一档说的只是"这一路的来源"，**不替文件担保**（写回可能失败，见 MenuClick 那条注释）
        let mut s = Settings::resolve(None, None, &file(Some("ko"), None), None).unwrap();
        s.lang_from = ValueSource::MenuClick;
        assert!(
            !s.summary().contains("不持久") && s.summary().contains("当场切换"),
            "{}",
            s.summary()
        );
    }

    /// 面板上点一行 ⇒ `app::App::persist_ui_key` 走 `write_ui_key_in` 落盘 ⇒ 下次启动
    /// [`Settings::resolve`] 要把**同一个值**从文件那一层认回来。这一条走的是整条链，
    /// 而它钉的东西很具体：**行上印的 `code()` 就是文件层认的那个字符串**。
    ///
    /// 为什么值得单独一条，而不是信 `petstore` 那 15 条 writer 测试：那些测的是**语法**
    /// （注释仍在、内联表拒改、非法 TOML 不碰磁盘），而"我们打算写进去的那四个语言码 /
    /// 三个动画码，写进去之后 UI 那条链还认不认得"是**跨模块**的那一段 —— 哪天有人把
    /// `Lang::code()` 改成 `"zh-CN"`，文件里就是一个下一次启动被当坏值跳过的键：面板点了
    /// 当场有反应（本进程改了），重启就回默认。那正是"看起来是功能、实际是半条功能"的形状，
    /// 而真机上验它要点鼠标（我绝不注入点击）⇒ 只有 headless 这一条能管。
    #[test]
    fn every_written_code_comes_back_through_the_chain() {
        let dir = tempfile::tempdir().expect("临时目录");
        // 注入路径，绝不碰用户的 `~/.config/opet/config.toml`（同 `ConfigEnv::with` 的存在理由）。
        let env = petstore::config::ConfigEnv::with(dir.path().to_str(), None);
        let read_back = || petstore::config::Config::read_in(&env);
        // 上面那个循环最后一个写进去的语言 = 文件里此刻的 `lang`。
        let last_lang = Lang::ALL[Lang::ALL.len() - 1];

        for lang in Lang::ALL {
            let at =
                petstore::config::write_ui_key_in(&env, petstore::config::KEY_LANG, lang.code())
                    .unwrap_or_else(|e| panic!("写语言码 {:?} 该成功：{e}", lang.code()));
            // writer 返回的就是它落盘的那个路径 ⇒ 路径也只有一处算（`App` 里不许再算一遍）。
            assert_eq!(
                at,
                read_back().path,
                "{lang:?} 的落点不是 reader 看的那个文件"
            );
            let s = Settings::resolve(None, None, &read_back().ui, None).unwrap();
            assert_eq!(
                (s.lang, s.lang_from),
                (lang, ValueSource::ConfigFile),
                "{lang:?} 写进去而链上认不回来（屏上点了有用、重启就丢 = 半条功能）：{s:?}"
            );
            assert!(s.notices.is_empty(), "{:?}", s.notices);
        }

        for motion in Motion::ALL {
            petstore::config::write_ui_key_in(&env, petstore::config::KEY_MOTION, motion.code())
                .unwrap_or_else(|e| panic!("写动画码 {:?} 该成功：{e}", motion.code()));
            let cfg = read_back();
            let s = Settings::resolve(None, None, &cfg.ui, None).unwrap();
            assert_eq!(
                (s.motion, s.motion_from),
                (motion, ValueSource::ConfigFile),
                "{motion:?} 写进去而链上认不回来：{s:?}"
            );
            // 两个键互不相干：写 motion 必须把上面那四次语言写入的**最后那一档**留着
            // （"只改那一个键"这条规则在 UI 侧的唯一可见证据）。
            assert_eq!(s.lang, last_lang, "写 motion 把 lang 刷掉了");
        }
    }
}
