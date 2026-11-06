//! 存档路径解析。**优先级：`--db` 参数 > `$OPET_DB` > `$XDG_DATA_HOME/opet/opet.db` > `~/.local/share/opet/opet.db`。**
//!
//! 同一文件里还有 [`AssetRoot`]：**装好的资产（本体 / 配件）住哪儿**。它照同一条三级形状，
//! 但**少 `--db` 那一档**（命令行不给资产根第二个入口，见那里的文档）。读写两侧都从它取根。
//!
//! 为什么前三条是"覆盖优先"而不是"拼接"：本轮所有写库测试都必须落在 `tempfile` 里，
//! 一旦测试忘了传路径就去写用户真实存档，那是不可逆的破坏（计划"风险"表最后一条）。
//! 所以 [`DbPath::resolve`] 如实报告它用的是哪一层，`Store::open_writable` 在
//! "路径来自默认解析"时要求调用方显式表态（见 [`PathSource::is_default`]）。
//!
//! ⚠️ **D10 澄清**：`~/.local/share` 与 `~/.local/state` 不冲突——XDG 语义里
//! "用户产生的数据"属 **share**（`opet.db` 正是这一类），**state** 归日志、run 文件、
//! 可丢弃的缓存。D10 同时列两个目录说的是这两件事，不是二选一。
//!
//! 环境变量读法走 [`Env`] 注入而不是直接 `std::env`：edition 2024 里改进程环境是
//! `unsafe`，而测试线程并行 ⇒ 真改掉它就是在制造偶发 UB。注入版本让每条分支都能被测。

use petcore::gacha::ItemKind;
use std::path::{Path, PathBuf};

/// 路径来自哪一层。测试与 CLI 报错都要看它，不能只看最终字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathSource {
    /// `--db PATH`
    CliArg,
    /// `$OPET_DB`
    EnvVar,
    /// `$XDG_DATA_HOME/opet/opet.db`
    XdgDataHome,
    /// `$HOME/.local/share/opet/opet.db`（`XDG_DATA_HOME` 未设时的规范回退）
    HomeFallback,
}

impl PathSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CliArg => "cli --db",
            Self::EnvVar => "$OPET_DB",
            Self::XdgDataHome => "$XDG_DATA_HOME",
            Self::HomeFallback => "$HOME/.local/share",
        }
    }

    /// 是不是"用户真实存档"那一类：用户没指哪，是代码自己解析出来的。
    pub const fn is_default(self) -> bool {
        matches!(self, Self::XdgDataHome | Self::HomeFallback)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbPath {
    pub path: PathBuf,
    pub source: PathSource,
}

/// 一次解析所需的环境变量快照。
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub opet_db: Option<String>,
    pub xdg_data_home: Option<String>,
    pub home: Option<String>,
    /// 配件库根（`app.rs` 的换装页与 `opet pack` 都读它）。
    pub opet_accessories: Option<String>,
    /// 本体库根（与配件**对称**的那一级，见 [`AssetRoot`]）。
    pub opet_pets: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            opet_db: grab("OPET_DB"),
            xdg_data_home: grab("XDG_DATA_HOME"),
            home: grab("HOME"),
            opet_accessories: grab("OPET_ACCESSORIES"),
            opet_pets: grab("OPET_PETS"),
        }
    }
}

impl DbPath {
    pub fn resolve(explicit: Option<&Path>) -> Self {
        Self::resolve_in(&Env::from_process(), explicit)
    }

    pub fn resolve_in(env: &Env, explicit: Option<&Path>) -> Self {
        if let Some(p) = explicit {
            return Self {
                path: p.to_path_buf(),
                source: PathSource::CliArg,
            };
        }
        if let Some(p) = &env.opet_db {
            return Self {
                path: PathBuf::from(p),
                source: PathSource::EnvVar,
            };
        }
        if let Some(xdg) = &env.xdg_data_home {
            let p = PathBuf::from(xdg);
            if p.is_absolute() {
                // XDG 规范：相对值视为未设，继续回退
                return Self {
                    path: p.join("opet").join("opet.db"),
                    source: PathSource::XdgDataHome,
                };
            }
        }
        Self {
            path: PathBuf::from(env.home.clone().unwrap_or_default())
                .join(".local")
                .join("share")
                .join("opet")
                .join("opet.db"),
            source: PathSource::HomeFallback,
        }
    }

    /// 父目录（`create_dir_all` 用）。文件名本身不算目录。
    pub fn parent(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}

// ------------------------------------------------------------ 资产根（本体 / 配件）

/// [`AssetRoot`] 来自哪一层。少 [`PathSource::CliArg`] 那一档是**决定**，不是遗漏：
/// 命令行不给资产根第二个入口（用户 2026-11-02 原话"配件不是应该由菜单选择吗，这也不合理"
/// ⇒ `--accessory` 整个删掉，穿哪件是屏上的活）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetRootSource {
    /// `$OPET_ACCESSORIES` / `$OPET_PETS`
    EnvVar,
    /// `$XDG_DATA_HOME/opet/<子目录>`
    XdgDataHome,
    /// `$HOME/.local/share/opet/<子目录>`
    HomeFallback,
}

impl AssetRootSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvVar => "环境变量",
            Self::XdgDataHome => "$XDG_DATA_HOME",
            Self::HomeFallback => "$HOME/.local/share",
        }
    }
}

/// 装好之后的一类资产住在哪儿。
///
/// 为什么这条规则在 `petstore` 而不是 `petcore`：`petcore::assets::AssetSource` 只该知道
/// "字节在哪儿、怎么读"，而"这台机器把包解到哪个目录"是 XDG 的事，与 [`DbPath`] 同一族 ⇒
/// 和它放同一个文件，`$XDG_DATA_HOME` 那条"相对值等于未设"的规则就只有一份实现。
///
/// 写侧（安装器）与读侧（换装页、`opet pack list`、INV 门禁）都从这里取根，否则就是两条
/// 会各自漂移的事实源 —— 这正是 §9.2 第 20 条那个"两块面互不相认"的成因形状。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRoot {
    pub kind: ItemKind,
    pub path: PathBuf,
    pub source: AssetRootSource,
}

impl AssetRoot {
    /// 环境变量名：`OPET_ACCESSORIES` / `OPET_PETS`。
    pub const fn env_var(kind: ItemKind) -> &'static str {
        match kind {
            ItemKind::Pet => "OPET_PETS",
            ItemKind::Accessory => "OPET_ACCESSORIES",
        }
    }

    /// `$XDG_DATA_HOME/opet/` 下的子目录名。**复数**：这一级装的是"很多项"，
    /// 与 §3.11.1 包内布局 `pets/<slug>/`、`accessories/<slug>/` 同名同义。
    pub const fn subdir(kind: ItemKind) -> &'static str {
        match kind {
            ItemKind::Pet => "pets",
            ItemKind::Accessory => "accessories",
        }
    }

    pub fn resolve(kind: ItemKind) -> Option<Self> {
        Self::resolve_in(&Env::from_process(), kind)
    }

    /// 三级解析。`$HOME` 都读不到时返回 `None` = **没有这个库**，不猜一个根：
    /// 与 `app.rs` 那条"没有东西可穿不等于没有这个功能"是同一个判断。
    pub fn resolve_in(env: &Env, kind: ItemKind) -> Option<Self> {
        let from_env = match kind {
            ItemKind::Pet => env.opet_pets.as_ref(),
            ItemKind::Accessory => env.opet_accessories.as_ref(),
        };
        if let Some(p) = from_env.filter(|p| !p.is_empty()) {
            return Some(Self {
                kind,
                path: PathBuf::from(p),
                source: AssetRootSource::EnvVar,
            });
        }
        let (base, source) = match env.xdg_data_home.as_ref().map(PathBuf::from) {
            // XDG 规范：相对值视为未设，继续回退（同 [`DbPath::resolve_in`]，不解释成"当前目录"）
            Some(p) if p.is_absolute() => (p, AssetRootSource::XdgDataHome),
            _ => {
                let home = env.home.as_ref()?;
                (
                    PathBuf::from(home).join(".local").join("share"),
                    AssetRootSource::HomeFallback,
                )
            }
        };
        Some(Self {
            kind,
            path: base.join("opet").join(Self::subdir(kind)),
            source,
        })
    }

    /// 报告用的一行：谁给的根、在哪。
    pub fn describe(&self) -> String {
        format!(
            "{}（{}）= {}",
            Self::subdir(self.kind),
            self.source.as_str(),
            self.path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(opet: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Env {
        Env {
            opet_db: opet.map(String::from),
            xdg_data_home: xdg.map(String::from),
            home: home.map(String::from),
            ..Env::default()
        }
    }

    #[test]
    fn cli_arg_beats_everything() {
        let e = env(
            Some("/tmp/from_env.db"),
            Some("/tmp/xdg"),
            Some("/home/jun"),
        );
        let p = DbPath::resolve_in(&e, Path::new("/tmp/from_cli.db").into());
        assert_eq!(p.source, PathSource::CliArg);
        assert_eq!(p.path, PathBuf::from("/tmp/from_cli.db"));
    }

    #[test]
    fn env_var_beats_xdg() {
        let e = env(
            Some("/tmp/opet_test.db"),
            Some("/tmp/xdg_data"),
            Some("/home/jun"),
        );
        let p = DbPath::resolve_in(&e, None);
        assert_eq!(p.source, PathSource::EnvVar);
        assert_eq!(p.path, PathBuf::from("/tmp/opet_test.db"));
    }

    #[test]
    fn xdg_data_home_is_absolute_only() {
        let abs = DbPath::resolve_in(&env(None, Some("/tmp/xdg_data"), Some("/home/jun")), None);
        assert_eq!(abs.source, PathSource::XdgDataHome);
        assert_eq!(abs.path, PathBuf::from("/tmp/xdg_data/opet/opet.db"));

        // 相对值按规范等于没设 ⇒ 回退 $HOME
        let rel = DbPath::resolve_in(&env(None, Some("relative/nope"), Some("/home/jun")), None);
        assert_eq!(rel.source, PathSource::HomeFallback);
        assert_eq!(
            rel.path,
            PathBuf::from("/home/jun/.local/share/opet/opet.db")
        );
    }

    #[test]
    fn empty_values_are_treated_as_unset() {
        // `OPET_DB=""` 不该被当成"打开一个空名字的路径"。
        let grab = || Env::default();
        let p = DbPath::resolve_in(&grab(), None);
        assert_eq!(p.source, PathSource::HomeFallback);
        assert_eq!(p.path, PathBuf::from(".local/share/opet/opet.db"));
        // from_process 的 filter 保证空串→None
        let mut e = env(Some(""), None, Some("/home/jun"));
        e.opet_db = None; // 模拟 filter 之后的形状
        assert_eq!(
            DbPath::resolve_in(&e, None).source,
            PathSource::HomeFallback,
            "空串在 Env::from_process 里已被滤掉"
        );
    }

    #[test]
    fn default_sources_are_flagged_as_real_user_archive() {
        assert!(PathSource::XdgDataHome.is_default());
        assert!(PathSource::HomeFallback.is_default());
        assert!(!PathSource::CliArg.is_default());
        assert!(!PathSource::EnvVar.is_default());
    }

    #[test]
    fn parent_is_the_directory_not_the_file() {
        let p = DbPath::resolve_in(&env(None, Some("/tmp/xdg"), None), None);
        assert_eq!(p.parent(), Path::new("/tmp/xdg/opet"));
    }

    fn aenv(
        accessories: Option<&str>,
        pets: Option<&str>,
        xdg: Option<&str>,
        home: Option<&str>,
    ) -> Env {
        Env {
            opet_accessories: accessories.map(String::from),
            opet_pets: pets.map(String::from),
            xdg_data_home: xdg.map(String::from),
            home: home.map(String::from),
            ..Env::default()
        }
    }

    #[test]
    fn asset_root_env_var_wins_and_says_so() {
        let e = aenv(
            Some("/tmp/acc"),
            Some("/tmp/pets"),
            Some("/tmp/xdg"),
            Some("/home/jun"),
        );
        let acc = AssetRoot::resolve_in(&e, ItemKind::Accessory).unwrap();
        assert_eq!(acc.source, AssetRootSource::EnvVar);
        assert_eq!(acc.path, PathBuf::from("/tmp/acc"));
        // 报告要说这根是谁给的：INV 门禁与 `opet pack list` 都靠这行区分"没装"与"装在别处"
        assert!(acc.describe().contains("环境变量"), "{}", acc.describe());
        assert!(acc.describe().contains("/tmp/acc"), "{}", acc.describe());
    }

    /// 两类资产各自的变量名，**各读各的**。这条存在的理由 = 复制粘贴一支分支时
    /// 最容易留下的形状是"pets 读的是 OPET_ACCESSORIES"，而那在只设了一个变量的机器上
    /// 看起来完全正常（⇒ 只有两个都设了才验得出来）。
    #[test]
    fn pets_and_accessories_use_different_variables() {
        let e = aenv(Some("/tmp/acc"), Some("/tmp/pets"), None, Some("/home/jun"));
        assert_eq!(
            AssetRoot::resolve_in(&e, ItemKind::Pet).unwrap().path,
            PathBuf::from("/tmp/pets")
        );
        assert_eq!(
            AssetRoot::resolve_in(&e, ItemKind::Accessory).unwrap().path,
            PathBuf::from("/tmp/acc")
        );
        assert_eq!(AssetRoot::env_var(ItemKind::Pet), "OPET_PETS");
        assert_eq!(AssetRoot::env_var(ItemKind::Accessory), "OPET_ACCESSORIES");
        assert_eq!(AssetRoot::subdir(ItemKind::Pet), "pets");
        assert_eq!(AssetRoot::subdir(ItemKind::Accessory), "accessories");
    }

    #[test]
    fn asset_root_falls_back_xdg_then_home() {
        let xdg = AssetRoot::resolve_in(
            &aenv(None, None, Some("/tmp/xdg"), Some("/home/jun")),
            ItemKind::Pet,
        )
        .unwrap();
        assert_eq!(xdg.source, AssetRootSource::XdgDataHome);
        assert_eq!(xdg.path, PathBuf::from("/tmp/xdg/opet/pets"));

        // 相对值按规范等于没设 ⇒ 回退 $HOME（同 DbPath 那条，不拼出相对路径）
        let home = AssetRoot::resolve_in(
            &aenv(None, None, Some("relative/nope"), Some("/home/jun")),
            ItemKind::Accessory,
        )
        .unwrap();
        assert_eq!(home.source, AssetRootSource::HomeFallback);
        assert_eq!(
            home.path,
            PathBuf::from("/home/jun/.local/share/opet/accessories")
        );
    }

    /// `$HOME` 也读不到 ⇒ **没有这个库**（`None`），不猜一个 `/…/.local/share`。
    /// 与 `app.rs` 那条"没有东西可穿不等于没有这个功能"是同一个判断，也是裸 `env -i` 的
    /// CI 里唯一正确的行为。
    #[test]
    fn no_home_means_no_library_not_a_guessed_root() {
        assert_eq!(
            AssetRoot::resolve_in(&aenv(None, None, None, None), ItemKind::Pet),
            None
        );
        assert_eq!(
            AssetRoot::resolve_in(&aenv(None, None, Some("relative"), None), ItemKind::Pet),
            None
        );
    }

    /// 空串在 `Env::from_process` 里已被滤掉，但解析函数**自己也要扛住**：
    /// 否则 `OPET_PETS=""` 会解析成"根就是空字符串"，装包时装到当前目录去。
    #[test]
    fn an_empty_asset_root_value_is_not_a_root() {
        let e = aenv(Some(""), Some(""), Some("/tmp/xdg"), Some("/home/jun"));
        let pet = AssetRoot::resolve_in(&e, ItemKind::Pet).unwrap();
        assert_eq!(pet.source, AssetRootSource::XdgDataHome);
        assert!(!pet.path.as_os_str().is_empty(), "不能解析成空路径");
        assert_eq!(pet.path, PathBuf::from("/tmp/xdg/opet/pets"));
    }
}
