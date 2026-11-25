//! 存档路径解析。**优先级（unix 链）：`--db` 参数 > `$OPET_DB` > `$XDG_DATA_HOME/opet/opet.db` > `~/.local/share/opet/opet.db`。**
//!
//! **Windows 链**（M5 第二批接入）：`--db` > `$OPET_DB` > `%LOCALAPPDATA%\opet\opet.db` >
//! `%APPDATA%\opet\opet.db` > `%USERPROFILE%\opet\opet.db`。两链共用 [`Env`] 快照与
//! [`PathSource`]，由 [`DbPath::resolve_on`] 这一个纯函数分岔 —— 分岔本身是参数不是 `cfg`，
//! 所以"同一份 Env 在两个平台上各得到哪条链"在**宿主上**就能钉死。
//!
//! 同一文件里还有 [`AssetRoot`]：**装好的资产（本体 / 配件）住哪儿**。它照同一条三级形状，
//! 但**少 `--db` 那一档**（命令行不给资产根第二个入口，见那里的文档）。读写两侧都从它取根。
//! Windows 侧的资产根链与 DbPath 链同族（`$OPET_*` > `%LOCALAPPDATA%\opet\<子目录>` >
//! `%APPDATA%…` > `%USERPROFILE%…`），2026-11-23 M5 补齐 —— 它的第一个消费者 = 本批
//! 菜单 + 换装的配件发现（`overlay-win32`），不留"能解析、没人读"的分支。
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
    /// `%LOCALAPPDATA%\\opet\\opet.db`（Windows 链的第一档默认）
    LocalAppData,
    /// `%APPDATA%\\opet\\opet.db`（roaming 档）
    AppDataRoaming,
    /// `%USERPROFILE%\\opet\\opet.db`（Windows 链的规范回退）
    UserProfile,
}

impl PathSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CliArg => "cli --db",
            Self::EnvVar => "$OPET_DB",
            Self::XdgDataHome => "$XDG_DATA_HOME",
            Self::HomeFallback => "$HOME/.local/share",
            Self::LocalAppData => "%LOCALAPPDATA%",
            Self::AppDataRoaming => "%APPDATA%",
            Self::UserProfile => "%USERPROFILE%",
        }
    }

    /// 是不是"用户真实存档"那一类：用户没指哪，是代码自己解析出来的。
    pub const fn is_default(self) -> bool {
        matches!(
            self,
            Self::XdgDataHome
                | Self::HomeFallback
                | Self::LocalAppData
                | Self::AppDataRoaming
                | Self::UserProfile
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbPath {
    pub path: PathBuf,
    pub source: PathSource,
}

/// 一次解析所需的环境变量快照。
///
/// ⚠️ 这张表**不点名平台**：unix 链读前四个、Windows 链读 `opet_db` + 后三个，而两链各自
/// 只看自己那几个 —— 选链的工作在 [`DbPath::resolve_on`]，它把平台做成参数而不是 `cfg`，
/// 这样分岔本身在宿主上就测得到。
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub opet_db: Option<String>,
    pub xdg_data_home: Option<String>,
    pub home: Option<String>,
    /// 配件库根（`app.rs` 的换装页与 `opet pack` 都读它）。
    pub opet_accessories: Option<String>,
    /// 本体库根（与配件**对称**的那一级，见 [`AssetRoot`]）。
    pub opet_pets: Option<String>,
    /// `%LOCALAPPDATA%`（Windows 链第一档；unix 上恒 `None`，读了也不进 unix 链）。
    pub local_appdata: Option<String>,
    /// `%APPDATA%`（roaming）。
    pub appdata: Option<String>,
    /// `%USERPROFILE%`（Windows 的"家"，与 unix 的 `home` **各读各的变量**）。
    pub userprofile: Option<String>,
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
            // 三条 Windows 变量在 unix 上恒 None ⇒ 无条件读，不为不存在的变量写 cfg 分支。
            local_appdata: grab("LOCALAPPDATA"),
            appdata: grab("APPDATA"),
            userprofile: grab("USERPROFILE"),
        }
    }
}

impl DbPath {
    pub fn resolve(explicit: Option<&Path>) -> Self {
        Self::resolve_on(&Env::from_process(), explicit, cfg!(windows))
    }

    /// **分岔本身是参数**：`cfg!` 只出现在 [`Self::resolve`] 一处，所以"同一份 Env 在
    /// Windows 上走哪条链、在非 Windows 上走哪条链"是宿主测例直接钉得住的事。
    /// 分岔写错在真机上的形状是"Windows 打开了一份 unix 拼法的路径"，而它不会当场炸 ——
    /// 会在第一次 `create_dir_all` 时建出一棵怪树。
    pub fn resolve_on(env: &Env, explicit: Option<&Path>, on_windows: bool) -> Self {
        if on_windows {
            Self::resolve_windows_in(env, explicit)
        } else {
            Self::resolve_in(env, explicit)
        }
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

    /// Windows 链：`--db` > `$OPET_DB` > `%LOCALAPPDATA%` > `%APPDATA%` > `%USERPROFILE%`。
    ///
    /// 三档目录变量各自要过 [`is_windows_absolute`]，**相对值等于没设** —— 与 unix 链的
    /// `$XDG_DATA_HOME` 同一条规范取向。`$OPET_DB` 那档不检查形制（它是完整文件路径，
    /// 语义与 unix 链逐字相同；绝对性由 `Store::open_with` 把关，两条链共用那一关）。
    pub fn resolve_windows_in(env: &Env, explicit: Option<&Path>) -> Self {
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
        for (value, source) in [
            (env.local_appdata.as_ref(), PathSource::LocalAppData),
            (env.appdata.as_ref(), PathSource::AppDataRoaming),
            (env.userprofile.as_ref(), PathSource::UserProfile),
        ] {
            if let Some(v) = value.filter(|v| is_windows_absolute(v.as_str())) {
                return Self {
                    path: PathBuf::from(v).join("opet").join("opet.db"),
                    source,
                };
            }
        }
        // 三档全不可用 ⇒ 与 unix 链 `HomeFallback` 同形：`unwrap_or_default` 兜底、
        // 档名如实报 `UserProfile`。`from_process` 出来的真实环境走不到这一支；
        // 它存在的意义是让"解析函数自己也要扛住空值"这条纪律（unix 侧
        // `empty_values_are_treated_as_unset`）在两链之间不用各写一遍。
        Self {
            path: PathBuf::from(env.userprofile.clone().unwrap_or_default())
                .join("opet")
                .join("opet.db"),
            source: PathSource::UserProfile,
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

/// Windows 语义的"绝对路径"判别，**纯字符串**——不用 `Path::is_absolute`。
///
/// 为什么手写而不是调平台函数：平台那份在 windows target 上才是对的，而这一层要能在**宿主
/// 测例里跑整套矩阵**（本项目"能做成宿主上跑的数学一律不进 `cfg`"那条切模块纪律）。
/// 手写版与平台版的判同在 Windows target 上由同一批测例二次核对（Wine 全量跑 petstore）。
/// 规则：
/// - 盘符式 `X:\…` 或 `X:/…`（接受正斜杠 = 与项目其他入口的宽容取向一致，Win32 文件 API
///   本来就归一化两种分隔符）；`X:foo` 是**盘相对**，不算绝对 —— 平台 `is_absolute` 说的
///   也是这个，不是我的巧合。
/// - UNC `\\server\share…`，含长路径前缀 `\\?\C:\…`；裸 `\\`（后无主机名）不是路径，不算。
///
/// ⚠️ `pub(crate)` 的只有这一条**判据**：`config.rs` 的 Windows 配置根链要用同一个"什么算绝对"
/// 的答案，两处各写一遍迟早漂移成"库在 `%LOCALAPPDATA%` 而配置在别处"。
/// **选链的函数不在这里共享** —— 每条链各自决定读哪几个变量、兜底给谁。
pub(crate) fn is_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    let drive = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/');
    let unc = b.len() >= 3 && b[0] == b'\\' && b[1] == b'\\';
    drive || unc
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
    /// `%LOCALAPPDATA%\opet\<子目录>`（Windows 链第一档默认）
    LocalAppData,
    /// `%APPDATA%\opet\<子目录>`（roaming 档）
    AppDataRoaming,
    /// `%USERPROFILE%\opet\<子目录>`（Windows 链的规范回退）
    UserProfile,
}

impl AssetRootSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvVar => "环境变量",
            Self::XdgDataHome => "$XDG_DATA_HOME",
            Self::HomeFallback => "$HOME/.local/share",
            Self::LocalAppData => "%LOCALAPPDATA%",
            Self::AppDataRoaming => "%APPDATA%",
            Self::UserProfile => "%USERPROFILE%",
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
        Self::resolve_on(&Env::from_process(), kind, cfg!(windows))
    }

    /// 选链 = 参数，与 [`DbPath::resolve_on`] 同一个形状同一个理由：分岔写错在真机上是
    /// "Windows 拼出一棵 unix 形状的空目录树"，而宿主测例只有把平台做成入参才钉得住它。
    pub fn resolve_on(env: &Env, kind: ItemKind, on_windows: bool) -> Option<Self> {
        if on_windows {
            Self::resolve_windows_in(env, kind)
        } else {
            Self::resolve_in(env, kind)
        }
    }

    /// Windows 资产根链：`$OPET_ACCESSORIES`/`$OPET_PETS` > `%LOCALAPPDATA%\opet\<子目录>` >
    /// `%APPDATA%\opet\<子目录>` > `%USERPROFILE%\opet\<子目录>`。
    ///
    /// ⚠️ 它与 [`DbPath::resolve_windows_in`] 有一处**有意不同**，别顺手"统一"掉：
    /// `$OPET_DB` 那档在那里不查形制，因为绝对性由 `Store::open_with` 统一把关；
    /// 而**这里没有下游把关**（资产根是拿去 join 再读的），所以环境变量档照样查
    /// [`is_windows_absolute`] —— 一条相对路径的 `OPET_PETS` 在 Windows 上等于没设，
    /// 继续走目录档。这是"读者负责自己输入的形状"在本文件里的第二个实例（第一个是
    /// `resolve_in` 里 XDG 相对值视为未设）。
    ///
    /// 与 unix 链同一处保持：`$HOME` 兜底档**不查形制**（这里对应 `%USERPROFILE%`）——
    /// unix 的 `resolve_in` 也没对 `$HOME` 做绝对性检查，两条链在"家目录是最后的兜底"
    /// 上对齐。全不可用 ⇒ `None` = 没有这个根，**不猜**（与 unix 链同一判断）。
    pub fn resolve_windows_in(env: &Env, kind: ItemKind) -> Option<Self> {
        let from_env = match kind {
            ItemKind::Pet => env.opet_pets.as_ref(),
            ItemKind::Accessory => env.opet_accessories.as_ref(),
        };
        if let Some(p) = from_env.filter(|p| is_windows_absolute(p)) {
            return Some(Self {
                kind,
                path: PathBuf::from(p),
                source: AssetRootSource::EnvVar,
            });
        }
        for (value, source) in [
            (env.local_appdata.as_ref(), AssetRootSource::LocalAppData),
            (env.appdata.as_ref(), AssetRootSource::AppDataRoaming),
        ] {
            if let Some(v) = value.filter(|v| is_windows_absolute(v.as_str())) {
                return Some(Self {
                    kind,
                    path: PathBuf::from(v).join("opet").join(Self::subdir(kind)),
                    source,
                });
            }
        }
        let home = env.userprofile.as_deref().filter(|h| !h.is_empty())?;
        Some(Self {
            kind,
            path: PathBuf::from(home).join("opet").join(Self::subdir(kind)),
            source: AssetRootSource::UserProfile,
        })
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

/// **当前 target 上保证绝对**的路径拼法：以前测例硬写 `/tmp/…`，在 unix 上是绝对、
/// 在 Windows target 上不是 —— M0 那五条 Wine 红全数都是这个成因（断言本身验的是
/// "相对值等于没设"这条规范，与平台无关；错的只是样本字符串的绝对性）。
/// 用 cwd 打底后同一份断言在宿主与 `x86_64-pc-windows-gnu`（Wine 跑测）两边同真。
/// `config.rs` 的同族测例共用这一条（那里原先也是硬写 `/tmp`）。
#[cfg(test)]
pub(crate) fn host_abs(sub: &str) -> String {
    std::env::current_dir()
        .expect("测例必然有 cwd")
        .join(sub)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(sub: &str) -> String {
        host_abs(sub)
    }

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
        let xdg = abs("xdg_data");
        let home = abs("home_jun");
        let got = DbPath::resolve_in(&env(None, Some(&xdg), Some(&home)), None);
        assert_eq!(got.source, PathSource::XdgDataHome);
        assert_eq!(got.path, PathBuf::from(&xdg).join("opet").join("opet.db"));

        // 相对值按规范等于没设 ⇒ 回退 $HOME
        let rel = DbPath::resolve_in(&env(None, Some("relative/nope"), Some(&home)), None);
        assert_eq!(rel.source, PathSource::HomeFallback);
        assert_eq!(
            rel.path,
            PathBuf::from(&home)
                .join(".local")
                .join("share")
                .join("opet")
                .join("opet.db")
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
        let xdg = abs("xdg");
        let p = DbPath::resolve_in(&env(None, Some(&xdg), None), None);
        let want = PathBuf::from(&xdg).join("opet");
        assert_eq!(
            p.parent(),
            want.as_path(),
            "父目录是 opet 那一层，不是 opet.db 文件"
        );
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
        let xdg = abs("tmp_xdg");
        let home = abs("home_jun");
        let got = AssetRoot::resolve_in(&aenv(None, None, Some(&xdg), Some(&home)), ItemKind::Pet)
            .unwrap();
        assert_eq!(got.source, AssetRootSource::XdgDataHome);
        assert_eq!(got.path, PathBuf::from(&xdg).join("opet").join("pets"));

        // 相对值按规范等于没设 ⇒ 回退 $HOME（同 DbPath 那条，不拼出相对路径）
        let home_root = AssetRoot::resolve_in(
            &aenv(None, None, Some("relative/nope"), Some(&home)),
            ItemKind::Accessory,
        )
        .unwrap();
        assert_eq!(home_root.source, AssetRootSource::HomeFallback);
        assert_eq!(
            home_root.path,
            PathBuf::from(&home)
                .join(".local")
                .join("share")
                .join("opet")
                .join("accessories")
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
        let xdg = abs("tmp_xdg");
        let home = abs("home_jun");
        let e = aenv(Some(""), Some(""), Some(&xdg), Some(&home));
        let pet = AssetRoot::resolve_in(&e, ItemKind::Pet).unwrap();
        assert_eq!(pet.source, AssetRootSource::XdgDataHome);
        assert!(
            pet.path.is_absolute(),
            "空串兜出来的根不许是相对路径（{}）",
            pet.path.display()
        );
        assert_eq!(pet.path, PathBuf::from(&xdg).join("opet").join("pets"));
    }

    // ── Windows 链（M5 第二批 M4）──────────────────────────────────────────
    // 样本一律写 Windows 形制（`C:\…`）：链的判据 `is_windows_absolute` 是纯字符串，
    // 所以这一族在宿主与 windows target 上**逐字同真** —— 这正是"分岔做成参数"买回来的东西。

    fn wenv(opet: Option<&str>, local: Option<&str>, app: Option<&str>, user: Option<&str>) -> Env {
        Env {
            opet_db: opet.map(String::from),
            local_appdata: local.map(String::from),
            appdata: app.map(String::from),
            userprofile: user.map(String::from),
            ..Env::default()
        }
    }

    const L: &str = r"C:\Users\jun\AppData\Local";
    const R: &str = r"C:\Users\jun\AppData\Roaming";
    const U: &str = r"C:\Users\jun";

    /// 五档优先级**矩阵**：全给齐时每一档各赢一次（一次循环里逐档摘掉上面的，看它是否接任）。
    #[test]
    fn windows_chain_priority_is_a_strict_ladder() {
        let want = [
            (PathSource::CliArg, PathBuf::from(r"D:\given\mine.db")),
            (PathSource::EnvVar, PathBuf::from(r"E:\opet_db.db")),
            (
                PathSource::LocalAppData,
                PathBuf::from(L).join("opet").join("opet.db"),
            ),
            (
                PathSource::AppDataRoaming,
                PathBuf::from(R).join("opet").join("opet.db"),
            ),
            (
                PathSource::UserProfile,
                PathBuf::from(U).join("opet").join("opet.db"),
            ),
        ];
        for (i, (src, path)) in want.iter().enumerate() {
            // 逐档摘掉"上面的"：第 i 档赢的时候，比它低的全在场、比它高的已摘干净
            // ⇒ 每一行断言实际证的是"这一档压得住下面所有档"，而不是孤立的一次命中。
            let mut e = wenv(Some(r"E:\opet_db.db"), Some(L), Some(R), Some(U));
            // 摘掉**高于第 i 档**的每一档（i 是 0 基：第 1 档 EnvVar 要赢时 `opet_db` 得留着）。
            if i >= 2 {
                e.opet_db = None;
            }
            if i >= 3 {
                e.local_appdata = None;
            }
            if i >= 4 {
                e.appdata = None;
            }
            let explicit = if i == 0 {
                Some(std::path::Path::new(r"D:\given\mine.db"))
            } else {
                None
            };
            let got = DbPath::resolve_windows_in(&e, explicit);
            assert_eq!(&got.source, src, "第 {i} 档应赢，实得 {:?}", got.source);
            assert_eq!(&got.path, path, "第 {i} 档路径拼错: {}", got.path.display());
        }
    }

    /// `is_windows_absolute` 的判据形状，含三个**容易写反**的样本：
    /// 盘相对 `C:foo`（不算绝对 —— 平台也是这么答的）、裸 `\\`（不是路径）、
    /// unix 绝对 `/tmp/x`（Windows 语义里是**当前盘的根相对**，不算绝对）。
    #[test]
    fn the_windows_absolute_matcher_says_what_the_platform_says() {
        for s in [
            r"C:\Users\jun",
            r"D:\a",
            "C:/Users/jun",
            r"C:\",
            r"\\server\share\opet",
            r"\\?\C:\Users\jun",
        ] {
            assert!(is_windows_absolute(s), "{s} 应当算绝对");
        }
        for s in [
            "",
            "relative\\nope",
            "foo",
            r"C:foo",
            r"\single\lead",
            "/tmp/xdg",
            r"\\",
            "C:",
        ] {
            assert!(!is_windows_absolute(s), "{s} 应当**不**算绝对");
        }
    }

    /// 相对/坏形制的目录变量 = 没设 ⇒ 继续往下走；三档全坏 ⇒ `unwrap_or_default` 兜底
    /// 并**如实报档名**（与 unix 链 `HomeFallback` 同形，见 `resolve_windows_in` 的注释）。
    #[test]
    fn relative_windows_variables_are_treated_as_unset() {
        let e = wenv(None, Some(r"relative\Local"), Some(R), Some(U));
        let got = DbPath::resolve_windows_in(&e, None);
        assert_eq!(got.source, PathSource::AppDataRoaming);

        let e = wenv(None, Some(r"C:bad"), None, Some(r"\notunc"));
        let got = DbPath::resolve_windows_in(&e, None);
        assert_eq!(got.source, PathSource::UserProfile);

        let got = DbPath::resolve_windows_in(&Env::default(), None);
        assert_eq!(got.source, PathSource::UserProfile);
        assert_eq!(
            got.path,
            PathBuf::from("opet").join("opet.db"),
            "兜底是相对拼法，档名必须如实"
        );
    }

    /// **选链本身**在宿主上钉死：同一份"两平台变量都给齐"的 Env，
    /// `on_windows=true` 必须无视 XDG 三档、`false` 必须无视 Windows 三档。
    /// 这条分岔如果反了，症状是"Windows 打开了一份 unix 拼法的路径"，而它编译得过、
    /// 类型也对 —— 唯一的防线就是这一条。
    #[test]
    fn the_chain_dispatch_is_pinned_on_the_host() {
        let e = Env {
            xdg_data_home: Some(abs("xdg")),
            home: Some(abs("home_jun")),
            local_appdata: Some(L.into()),
            ..Env::default()
        };
        let w = DbPath::resolve_on(&e, None, true);
        assert_eq!(w.source, PathSource::LocalAppData);
        let u = DbPath::resolve_on(&e, None, false);
        assert_eq!(u.source, PathSource::XdgDataHome);
        // Windows 链不吃 `home`/`xdg_data_home`，unix 链不吃 `local_appdata` —— 各读各的。
        assert_eq!(
            DbPath::resolve_on(&Env::default(), None, true).source,
            PathSource::UserProfile
        );
        assert_eq!(
            DbPath::resolve_on(&Env::default(), None, false).source,
            PathSource::HomeFallback
        );
    }

    /// 三个新档全在"用户真实存档"里（`--db` 没给就可能写它们 ⇒ 必须被 `is_default` 拦住），
    /// 而 `as_str` 打的必须是**真实变量名**：启动行与报错都靠这行让用户看懂根是谁给的。
    #[test]
    fn windows_default_tiers_are_flagged_and_named_by_their_variables() {
        for src in [
            PathSource::LocalAppData,
            PathSource::AppDataRoaming,
            PathSource::UserProfile,
        ] {
            assert!(src.is_default(), "{src:?} 是默认档 ⇒ 必须算真存档");
        }
        assert_eq!(PathSource::LocalAppData.as_str(), "%LOCALAPPDATA%");
        assert_eq!(PathSource::AppDataRoaming.as_str(), "%APPDATA%");
        assert_eq!(PathSource::UserProfile.as_str(), "%USERPROFILE%");
    }

    /// Windows 资产根链专用 Env：`acc`/`pet` 是两个**各读各的**环境变量，`local/app/user`
    /// 是 Windows 三档目录。四个都可 `None`（= 未设）。
    fn wenv_asset(
        acc: Option<&str>,
        pet: Option<&str>,
        local: Option<&str>,
        app: Option<&str>,
        user: Option<&str>,
    ) -> Env {
        Env {
            opet_accessories: acc.map(String::from),
            opet_pets: pet.map(String::from),
            local_appdata: local.map(String::from),
            appdata: app.map(String::from),
            userprofile: user.map(String::from),
            ..Env::default()
        }
    }

    /// Windows 资产根链的四档矩阵 + 两类资产各走各的变量。摘档方式同 DbPath 那一
    /// （第 i 档赢时比它高的已摘干净），而这条还钉一件事：`pets` 读 `OPET_PETS`、
    /// `accessories` 读 `OPET_ACCESSORIES` —— 复制粘贴分支最容易把这两个变量接反。
    #[test]
    fn windows_asset_root_chain_is_a_strict_ladder() {
        // (变量值, 期望源, 期望拼法) 的逐档摘取；accessories 链先跑，pets 链同样形状。
        for (kind, envval, sub) in [
            (ItemKind::Accessory, r"E:\acc_db", "accessories"),
            (ItemKind::Pet, r"E:\pet_db", "pets"),
        ] {
            let want = [
                (AssetRootSource::EnvVar, PathBuf::from(envval)),
                (
                    AssetRootSource::LocalAppData,
                    PathBuf::from(L).join("opet").join(sub),
                ),
                (
                    AssetRootSource::AppDataRoaming,
                    PathBuf::from(R).join("opet").join(sub),
                ),
                (
                    AssetRootSource::UserProfile,
                    PathBuf::from(U).join("opet").join(sub),
                ),
            ];
            for (i, (src, path)) in want.iter().enumerate() {
                // 第 0 档 = 环境变量赢；其后逐档摘掉高于第 i 档的目录变量。
                let env_ok = if i == 0 { Some(envval) } else { None };
                let (acc, pet) = match kind {
                    ItemKind::Accessory => (env_ok, None),
                    _ => (None, env_ok),
                };
                let mut e = wenv_asset(acc, pet, Some(L), Some(R), Some(U));
                // 第 i 档赢 = 比它高的档（索引 < i）全摘掉。四档 0 基：env=0/local=1/app=2/user=3。
                if i >= 2 {
                    e.local_appdata = None;
                }
                if i >= 3 {
                    e.appdata = None;
                }
                let got = AssetRoot::resolve_windows_in(&e, kind).expect("资产根链必须有兜底");
                assert_eq!(
                    &got.source, src,
                    "第 {i} 档应赢（{kind:?}），实得 {:?}",
                    got.source
                );
                assert_eq!(&got.path, path, "第 {i} 档路径拼错: {}", got.path.display());
            }
        }
    }

    /// Windows 资产根与 unix 资产根**共享**的判据：相对目录值 = 没设（继续下探），
    /// 三档目录全坏 ⇒ 落 `%USERPROFILE%` 且如实报档名，全空 ⇒ `None`（**不猜根**，
    /// 因为资产根没有 DbPath 那样"下游 `open_with` 把绝对性"的兜底，只能在这一层拒）。
    #[test]
    fn windows_asset_root_relative_or_empty_yields_none_not_a_guess() {
        let e = wenv_asset(None, None, Some(r"relative\\Local"), Some(R), Some(U));
        let got = AssetRoot::resolve_windows_in(&e, ItemKind::Accessory).unwrap();
        assert_eq!(got.source, AssetRootSource::AppDataRoaming);

        let e = wenv_asset(None, None, Some(r"C:bad"), None, Some(r"\notunc"));
        let got = AssetRoot::resolve_windows_in(&e, ItemKind::Pet).unwrap();
        assert_eq!(got.source, AssetRootSource::UserProfile);

        // 全空 ⇒ None：`overlay-win32` 的配件发现据此答"这台机器没有配件库"，不建空树。
        let none = AssetRoot::resolve_windows_in(&Env::default(), ItemKind::Accessory);
        assert!(none.is_none(), "资产根不许在无变量时猜一个根：{none:?}");
    }

    /// **资产根的选链**也在宿主上钉死（与 DbPath 的 `the_chain_dispatch_is_pinned_on_the_host`
    /// 配对）：同一份"两平台变量都给齐"的 Env，`resolve_on(true)` 走 Windows 目录链、
    /// `resolve_on(false)` 走 XDG/home 链，两条各读各的变量。
    #[test]
    fn the_asset_root_chain_dispatch_is_pinned_on_the_host() {
        let e = Env {
            xdg_data_home: Some(abs("xdg")),
            home: Some(abs("home_jun")),
            local_appdata: Some(L.into()),
            ..Env::default()
        };
        let w = AssetRoot::resolve_on(&e, ItemKind::Accessory, true).unwrap();
        assert_eq!(w.source, AssetRootSource::LocalAppData);
        let u = AssetRoot::resolve_on(&e, ItemKind::Accessory, false).unwrap();
        assert_eq!(u.source, AssetRootSource::XdgDataHome);
        // 空 Env 在两链的兜底不同：Windows ⇒ `None`（无变量），unix ⇒ HomeFallback（`$HOME` 也空时才是 None）。
        assert!(AssetRoot::resolve_on(&Env::default(), ItemKind::Accessory, true).is_none());
        assert!(AssetRoot::resolve_on(&Env::default(), ItemKind::Accessory, false).is_none());
    }
}
