//! 用户配置文件的读取：`$XDG_CONFIG_HOME/opet/config.toml`（`UI_REDESIGN_V2` §4.6）。
//!
//! **只读、只认平坦的 `[段]` + `key = "值"`** —— 自写 ~120 行而不是引 `toml` crate：本轮
//! 只要两个键，为它加一个依赖（还要按 D8 逐条核许可证）不划算，而且平坦语法出错时我们能
//! 精确说清"第几行、哪个键、为什么"，库通常只会回一个 `TomlError`。
//!
//! 值一律**原样返回字符串**，本模块不解释语义。`lang` 认不认得由
//! [`overlay_wayland::l10n::Lang::parse`] 判，`motion` 由
//! [`overlay_wayland::settings::Motion::parse`] 判 —— 反过来（这里就解释值）会让
//! `petstore` 依赖 UI 侧的枚举，而 UI 侧已经依赖 `petstore`。
//!
//! ## 为什么"绝不静默"是这一条的主线
//!
//! 配置文件最容易做坏的形状是**读不到就当默认**：用户写了 `lang = "en"` 却因打错成
//! `lng` 而什么都没发生，看起来"支持配置"、实际上不支持（§7-R14）。所以本模块的规则是：
//! · 文件**不存在** = 什么都不打印（这不是错误，是"没用过这个功能"）；
//! · 文件存在但**读失败** / 某行**语法不认** / 某个已知键的**值为空** / 出现本轮**不支持的段**
//!   ⇒ 每条都产出一句点名**文件路径 + 行号或键名**的 `notices`，值退回未设置。
//! `notices` 是要被调用方打印的（[`Config::announce`]）；只在测试里比对而不落地，等于
//! 把这条规则做成了"函数存在但没人调"。
//!
//! ## 路径回退与 `paths.rs` 同一条规则
//!
//! `$XDG_CONFIG_HOME/opet/config.toml` → `$HOME/.config/opet/config.toml`。**相对值的
//! `XDG_CONFIG_HOME` 按 XDG 规范等于没设**，继续回退 —— 与 [`crate::DbPath`] 完全一致，
//! 刻意由测试 `config_path_follows_the_same_xdg_rule_as_the_archive` 钉住两边同形，
//! 免得将来一处改了另一处没改（那就是"两套 XDG 语义"）。
//!
//! ⚠️ **本轮不回写这个文件**（§4.7/R15）：菜单里点语言只改本进程。自写 reader 保不住注释，
//! 读改写会把用户的文件刷平 —— 丢注释比"重启回默认"更坏。

use std::path::{Path, PathBuf};

/// 唯一支持的段。其它段（`[render]`/`[audio]`…）**会被读出来并明说本轮不生效**。
pub const SECTION_UI: &str = "ui";
/// `[ui]` 下的两个键。
pub const KEY_LANG: &str = "lang";
pub const KEY_MOTION: &str = "motion";
pub const UI_KEYS: [&str; 2] = [KEY_LANG, KEY_MOTION];

/// 文件这一层的状态。**注意 `Absent` 与 `Unreadable` 不是一回事**：前者是"用户没在用这个
/// 功能"（不许打印任何东西），后者是"用户在用而且写坏了"（必须出声）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    /// 按路径解析出来的文件存在，而且读到了内容
    File,
    /// 解析出来的路径上没有文件
    Absent,
    /// 路径存在但读不出来（权限、是目录、不是 UTF-8…）
    Unreadable,
}

impl ConfigSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "已读取",
            Self::Absent => "不存在",
            Self::Unreadable => "读失败",
        }
    }
}

/// `[ui]` 里本轮认识的键（原始字符串，`None` = 没写或被拒绝）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiKeys {
    pub lang: Option<String>,
    pub motion: Option<String>,
}

impl UiKeys {
    fn set(&mut self, key: &str, value: String) -> bool {
        match key {
            KEY_LANG => self.lang = Some(value),
            KEY_MOTION => self.motion = Some(value),
            _ => return false,
        }
        true
    }
}

/// 一次解析所需的环境变量快照（注入式，理由同 `paths::Env`：进程环境是并行测试的雷区）。
#[derive(Clone, Debug, Default)]
pub struct ConfigEnv {
    pub xdg_config_home: Option<String>,
    pub home: Option<String>,
}

impl ConfigEnv {
    pub fn from_process() -> Self {
        let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            xdg_config_home: grab("XDG_CONFIG_HOME"),
            home: grab("HOME"),
        }
    }

    /// 只给路径回退用的环境变量（测试注入形状）。
    pub fn with(xdg: Option<&str>, home: Option<&str>) -> Self {
        Self {
            xdg_config_home: xdg.map(String::from),
            home: home.map(String::from),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// 解析出来的文件位置（不管存不存在，报错都要点名它）。
    pub path: PathBuf,
    pub source: ConfigSource,
    pub ui: UiKeys,
    /// 需要出声的条目（已含文件路径与键名/行号）。**调用方必须打印**。
    pub notices: Vec<String>,
}

impl Config {
    pub fn read() -> Self {
        Self::read_in(&ConfigEnv::from_process())
    }

    pub fn read_in(env: &ConfigEnv) -> Self {
        let path = config_path(env);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (ui, notices) = parse(&text, &path);
                Self {
                    path,
                    source: ConfigSource::File,
                    ui,
                    notices,
                }
            }
            // `NotFound` 是唯一"沉默"的分支；其余一律出声（见模块头）。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self {
                path,
                source: ConfigSource::Absent,
                ui: UiKeys::default(),
                notices: Vec::new(),
            },
            Err(e) => {
                let notice = format!("{}: 配置文件读不出来（{e}）⇒ 全部用默认值", path.display());
                Self {
                    path,
                    source: ConfigSource::Unreadable,
                    ui: UiKeys::default(),
                    notices: vec![notice],
                }
            }
        }
    }

    /// 一句话说明这份配置是从哪个文件来的、状态如何（启动摘要用）。
    ///
    /// `Absent` 那一支**刻意把路径打出来**：这是"用户自己设置语言"这个功能唯一的发现入口
    /// —— 菜单里点语言只改本进程（本轮不回写文件），要让设置活过重启就得知道该写哪个文件。
    pub fn describe(&self) -> String {
        let at = self.path.display();
        match self.source {
            ConfigSource::Absent => format!("{at} 不存在（要持久化就建它，写 [ui] lang/motion）"),
            s => format!("{at}（{}）", s.as_str()),
        }
    }

    /// 把 `notices` 打到 stderr。启动时调用一次（只在启动读，不做 watcher：设计书 §4
    /// 禁止定时轮询）。
    pub fn announce(&self) {
        for n in &self.notices {
            eprintln!("config: {n}");
        }
    }
}

/// 与 `DbPath::resolve_in` 同一条回退规则：XDG 值必须是**绝对路径**才算，否则回退 `$HOME`。
fn config_path(env: &ConfigEnv) -> PathBuf {
    if let Some(xdg) = &env.xdg_config_home {
        let p = Path::new(xdg);
        if p.is_absolute() {
            return p.join("opet").join("config.toml");
        }
    }
    PathBuf::from(env.home.clone().unwrap_or_default())
        .join(".config")
        .join("opet")
        .join("config.toml")
}

/// 平坦语法：**注释 `#`、`[段]`、`key = "值"`**。返回认到的 `[ui]` 键 + 每一条出声的说明。
///
/// 刻意不做的事（每条都是因为"做了就等于承诺一个本轮没有的功能"）：多行字符串、转义、
/// 内联数组、其它段里的键、数值/布尔的原生类型（`fps_active = 30` 这种在 `[render]` 里
/// 只会得到"本轮不生效"一行）。
fn parse(text: &str, path: &Path) -> (UiKeys, Vec<String>) {
    let mut ui = UiKeys::default();
    let mut notices: Vec<String> = Vec::new();
    let mut section = String::new();
    let mut seen_in_section: Vec<(String, usize)> = Vec::new();
    // 每一条出声都自带文件路径：用户可能有 `$XDG_CONFIG_HOME` 与 `$HOME/.config` 两处，
    // 只说"第 3 行"不说哪个文件，等于让人猜。
    let at = path.display().to_string();

    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let no = idx + 1;

        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                notices.push(format!(
                    "{at}: 第 {no} 行的段头不合法（缺 `]`）：{line} ⇒ 当作没写"
                ));
                continue;
            };
            let name = name.trim().to_string();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                notices.push(format!(
                    "{at}: 第 {no} 行的段名不合法：{name:?}（只认字母数字下划线）⇒ 当作没写"
                ));
                continue;
            }
            if name != SECTION_UI {
                notices.push(format!(
                    "{at}: [{name}] 段**本轮不生效**（只有 [{SECTION_UI}] 的 {UI_KEYS:?} 会被读）\
                     ⇒ 这个段里的键一律忽略"
                ));
            }
            section = name;
            seen_in_section.clear();
            continue;
        }

        let Some((raw_key, raw_value)) = line.split_once('=') else {
            notices.push(format!(
                "{at}: 第 {no} 行既不是段头也不是 `key = \"值\"`：{line} ⇒ 忽略"
            ));
            continue;
        };
        let key = raw_key.trim().to_string();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            notices.push(format!(
                "{at}: 第 {no} 行的键名不合法：{key:?} ⇒ 忽略这一行"
            ));
            continue;
        }

        if section != SECTION_UI {
            // `[render]` 这类段：段头那一条已经出过声，键不再重复吵。
            // 但写在**任何段之前**的键必须出声 —— 否则"打错段头 ⇒ 下面的键全被静默吞掉"
            // 正是这一轮要避免的那个假功能形状。
            if section.is_empty() {
                notices.push(format!(
                    "{at}: 第 {no} 行的 `{key}` 写在任何 [{SECTION_UI}] 段之前 ⇒ 忽略（键要放在段下）"
                ));
            }
            continue;
        }

        if !UI_KEYS.contains(&key.as_str()) {
            notices.push(format!(
                "{at}: [{SECTION_UI}] 不认这个键 `{key}`（本轮只有 {UI_KEYS:?}）⇒ 忽略"
            ));
            continue;
        }
        if let Some((_, prev)) = seen_in_section.iter().find(|(k, _)| *k == key) {
            notices.push(format!(
                "{at}: 第 {no} 行重复了 [{SECTION_UI}] 的 `{key}`（第 {prev} 行也写了）⇒ 以后一条为准"
            ));
        }
        let value = match unquote(raw_value.trim()) {
            Ok(v) => v,
            Err(why) => {
                notices.push(format!(
                    "{at}: [{SECTION_UI}] 的 `{key}` 在第 {no} 行写法不对（{why}）⇒ 按未设置处理"
                ));
                continue;
            }
        };
        if value.trim().is_empty() {
            notices.push(format!(
                "{at}: [{SECTION_UI}] 的 `{key}` 在第 {no} 行是空值 ⇒ 按未设置处理"
            ));
            continue;
        }
        // 认到的键**故意不打印**（生效值与来源由 `overlay_wayland::settings` 那一条摘要出）；
        // 这里只记行数，用来点名重复键打架的两行。
        if !ui.set(&key, value) {
            notices.push(format!(
                "{at}: 内部错误：`{key}` 在 `UI_KEYS` 里却没有 setter ⇒ 请报 issue"
            ));
        }
        seen_in_section.push((key, no));
    }

    (ui, notices)
}

/// 只认 `"…"` 一种写法。给出错信息时把用户写的原样带回去，别只说"格式错"。
fn unquote(raw: &str) -> Result<String, String> {
    let Some(rest) = raw.strip_prefix('"') else {
        return Err(if raw.is_empty() {
            "等号右边是空的".to_string()
        } else {
            format!("值要用双引号括起来，这里写的是 {raw}")
        });
    };
    let Some(inner) = rest.strip_suffix('"') else {
        return Err(format!("少了右边的引号：{raw}"));
    };
    if inner.contains('"') {
        return Err(format!("引号不成对：{raw}（本轮不支持转义 `\\\"`）"));
    }
    Ok(inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把一段文本写进临时文件并把 `XDG_CONFIG_HOME` 指过去。
    fn write_and_read(text: &str) -> Config {
        let dir = tempfile::tempdir().expect("临时目录");
        let conf_dir = dir.path().join("opet");
        std::fs::create_dir_all(&conf_dir).expect("建 opet/");
        std::fs::write(conf_dir.join("config.toml"), text).expect("写 config.toml");
        let env = ConfigEnv::with(dir.path().to_str(), Some("/home/nobody"));
        let cfg = Config::read_in(&env);
        // dir 在 cfg 之后 drop，路径还有效（断言要用它比对）
        assert!(
            cfg.path.exists() || cfg.source == ConfigSource::Absent,
            "解析出来的路径应该指向刚写的那个文件"
        );
        cfg
    }

    fn lines(notices: &[String]) -> String {
        notices.join("\n")
    }

    /// 文件不存在 = **一句都不许多说**。这条测的是"沉默是设计"而不是"忘了实现"。
    #[test]
    fn absent_file_is_completely_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let env = ConfigEnv::with(dir.path().to_str(), Some("/home/nobody"));
        let cfg = Config::read_in(&env);
        assert_eq!(cfg.source, ConfigSource::Absent);
        assert_eq!(cfg.ui, UiKeys::default(), "不存在就不该有值");
        assert!(
            cfg.notices.is_empty(),
            "文件不存在还出声 = 每次启动都吵用户：{}",
            lines(&cfg.notices)
        );
        assert_eq!(dir.path().join("opet").join("config.toml"), cfg.path);
    }

    /// 路径回退必须和 `paths.rs` 同一条规则（XDG 只认绝对值）。两处不一致就是"两套 XDG 语义"。
    #[test]
    fn config_path_follows_the_same_xdg_rule_as_the_archive() {
        let abs = Config::read_in(&ConfigEnv::with(Some("/tmp/xdg-cfg"), Some("/home/jun")));
        assert_eq!(
            abs.path,
            PathBuf::from("/tmp/xdg-cfg/opet/config.toml"),
            "$XDG_CONFIG_HOME 绝对 ⇒ 直接用它"
        );

        let rel = Config::read_in(&ConfigEnv::with(Some("relative/nope"), Some("/home/jun")));
        assert_eq!(
            rel.path,
            PathBuf::from("/home/jun/.config/opet/config.toml"),
            "XDG 规范：相对值等于没设 ⇒ 必须回退 $HOME/.config"
        );

        // 与 DbPath 的对照：同一个相对值，两条链都必须落到 $HOME 而不是拼出相对路径。
        let db = crate::DbPath::resolve_in(
            &crate::paths::Env {
                opet_db: None,
                xdg_data_home: Some("relative/nope".into()),
                home: Some("/home/jun".into()),
            },
            None,
        );
        assert!(
            rel.path.is_absolute() && db.path.is_absolute(),
            "回退后两边都得是绝对路径：{:?} {:?}",
            rel.path,
            db.path
        );
    }

    #[test]
    fn recognized_ui_keys_come_back_raw() {
        let cfg = write_and_read(
            "\
# OPet 的用户设置
[ui]
lang = \"en\"
motion = \"reduce\"
",
        );
        assert_eq!(cfg.source, ConfigSource::File);
        assert_eq!(cfg.ui.lang.as_deref(), Some("en"));
        assert_eq!(cfg.ui.motion.as_deref(), Some("reduce"));
        assert!(
            cfg.notices.is_empty(),
            "合法文件不该出声：{}",
            lines(&cfg.notices)
        );
        // 值原样返回、**不 trim 不转小写**：解释语义在 UI 侧（`Lang::parse` 自己会 trim
        // 并取子标签）。两处都归一化就会出现"reader 认得而 parse 不认得"的第二套口径。
        assert_eq!(cfg.ui.lang.as_deref(), Some("en"), "reader 只许回原文");

        let spaced = write_and_read("[ui]\nlang = \" zh \"\n");
        assert_eq!(
            spaced.ui.lang.as_deref(),
            Some(" zh "),
            "引号内的空白必须原样带出（`Lang::parse` 会 trim；这里先 trim 就没人看得见脏值）"
        );
    }

    /// **每一条被拒的行都要点名文件与键/行号**。测的是"不静默"这条硬规则本身：
    /// 一条都不许只回 `None`。
    #[test]
    fn every_rejected_line_names_the_file_and_the_key() {
        let cfg = write_and_read(
            "\
lang = \"zh\"
[ui]
lang = en
motion = \"\"
motion2 = \"full\"
motion = \"
[ui
[nonsense!]
",
        );
        let text = lines(&cfg.notices);
        let want = [
            ("写在任何", "段前的键必须出声（打错段头就会静默吞键）"),
            ("双引号", "未加引号的值"),
            ("空值", "key = \"\""),
            ("不认这个键", "[ui] 里的未知键"),
            ("少了右边的引号", "引号没闭合"),
            ("段头不合法", "[ui"),
            ("段名不合法", "[nonsense!]"),
        ];
        for (needle, why) in want {
            assert!(text.contains(needle), "漏了「{why}」这一条的出声：\n{text}");
        }
        // 一行一条，既不少（漏出声）也不多（同一条重复吵）
        assert_eq!(cfg.notices.len(), want.len(), "出声条数不对：\n{text}");
        // 文件路径必须在每一条里（否则用户有两台机器/两个 XDG 时无从下手）
        for n in &cfg.notices {
            assert!(
                n.contains(&cfg.path.display().to_string()),
                "这条没点名文件路径：{n}"
            );
        }
        // 全部被拒 ⇒ 没有任何值生效（不能"部分生效"还装作读到了）
        assert_eq!(cfg.ui, UiKeys::default(), "被拒的行不该留下值：{text}");
    }

    /// §1.1：`[render] fps_active` 本轮**不接**，但也不许静默忽略 —— 那是 D14 反对的假功能。
    #[test]
    fn unsupported_sections_are_announced_and_their_keys_ignored() {
        let cfg = write_and_read(
            "\
[render]
fps_active = 30
[audio]
volume = \"80\"
[ui]
lang = \"ja\"
",
        );
        let text = lines(&cfg.notices);
        assert!(text.contains("本轮不生效"), "没说不生效就是假功能：{text}");
        assert!(
            text.contains("render") && text.contains("audio"),
            "两个段都要点名：{text}"
        );
        assert_eq!(cfg.notices.len(), 2, "一段一句，别为每个键重复吵：{text}");
        assert_eq!(
            cfg.ui.lang.as_deref(),
            Some("ja"),
            "不认识段不影响认识段（不能一坏俱坏）"
        );
        assert_eq!(cfg.ui.motion, None);
    }

    /// 读不出来（这里是"路径上是个目录"）与不存在必须区分开，而且都要有各自的口径。
    #[test]
    fn unreadable_file_is_announced_and_leaves_values_unset() {
        let dir = tempfile::tempdir().unwrap();
        let opet = dir.path().join("opet");
        std::fs::create_dir_all(opet.join("config.toml")).unwrap(); // 目录冒充文件
        let cfg = Config::read_in(&ConfigEnv::with(dir.path().to_str(), None));
        assert_eq!(cfg.source, ConfigSource::Unreadable);
        assert_eq!(cfg.notices.len(), 1, "{:?}", cfg.notices);
        assert!(cfg.notices[0].contains("读不出来"), "{}", cfg.notices[0]);
        assert_eq!(cfg.ui, UiKeys::default());
        assert!(cfg.describe().contains("读失败"), "{}", cfg.describe());
    }

    #[test]
    fn duplicate_key_is_announced_and_the_later_one_wins() {
        let cfg = write_and_read(
            "\
[ui]
lang = \"zh\"
lang = \"ko\"
",
        );
        assert_eq!(cfg.ui.lang.as_deref(), Some("ko"));
        assert_eq!(cfg.notices.len(), 1, "{:?}", cfg.notices);
        let n = &cfg.notices[0];
        assert!(
            n.contains("重复") && n.contains("第 3 行") && n.contains("第 2 行"),
            "重复键要点名两行：{n}"
        );
    }
}
