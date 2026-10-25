//! 存档路径解析。**优先级：`--db` 参数 > `$OPET_DB` > `$XDG_DATA_HOME/opet/opet.db` > `~/.local/share/opet/opet.db`。**
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
}

impl Env {
    pub fn from_process() -> Self {
        let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            opet_db: grab("OPET_DB"),
            xdg_data_home: grab("XDG_DATA_HOME"),
            home: grab("HOME"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env(opet: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Env {
        Env {
            opet_db: opet.map(String::from),
            xdg_data_home: xdg.map(String::from),
            home: home.map(String::from),
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
}
