//! 用户配置文件：`$XDG_CONFIG_HOME/opet/config.toml`（`UI_REDESIGN_V2` §4.6 + 设计书 §9.2 第 19 条）。
//!
//! **读 = 自写的那只平坦 reader，只认 `[段]` + `key = "值"`**：为两个键引一个 TOML 依赖去
//! **读**不划算，而且平坦语法出错时我们能精确说清"第几行、哪个键、为什么"，库通常只会回一个
//! `TomlError`。**写 = `toml_edit`**（见 [`write_ui_key`]）：读改写必须由一个保住 decor 的
//! 库来做，自写 writer 会把用户手写的注释刷平。两边不是同一套实现，正是那条往返测试要钉的东西。
//!
//! 值一律**原样返回字符串**，本模块不解释语义。`lang` 认不认得由
//! `overlay_common::l10n::Lang::parse` 判，`motion` 由
//! `overlay_common::ui::settings::Motion::parse` 判（两个都在绘制层，2026-11-22 上提）——
//! 反过来（这里就解释值）会让 `petstore` 依赖 UI 侧的枚举，而 UI 侧已经依赖 `petstore`。
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
//! **unix 链**：`$XDG_CONFIG_HOME/opet/config.toml` → `$HOME/.config/opet/config.toml`。
//! **Windows 链**（M7e 起）：`%LOCALAPPDATA%\opet\config.toml` → `%APPDATA%\opet\config.toml` →
//! `%USERPROFILE%\opet\config.toml`。两条链与 [`crate::DbPath`] / [`crate::AssetRoot`] 完全同族：
//! 目录变量必须是**该平台形制的绝对路径**才算，相对值等于没设，继续回退。
//! ⚠️ **Windows 那三档查的是 `paths::is_windows_absolute`（盘符 / UNC），不是
//! `Path::is_absolute`** —— 后者在 unix 上判 `C:\x` 为假、在 Windows 上判 `/tmp/x` 为真，
//! 用它就让"哪一档赢"变成"在哪个平台上编译"的函数，而那件事在 [`Config::read_on`] 是**参数**。
//! 由测试 `config_path_follows_the_same_xdg_rule_as_the_archive`（unix 侧）与
//! `the_windows_config_chain_is_a_strict_ladder`（Windows 侧）钉住两边各自同形。
//!
//! 分岔只有 [`Config::read`] / [`write_ui_key`] 这两处（各写一次 `cfg!(windows)`），其余入口
//! (`read_in` / `read_windows_in` / `read_on`) 都吃显式平台参数 —— 与 `paths.rs` 那条
//! "分岔本身是参数，所以宿主测例钉得住"的纪律逐字同形。
//!
//! ⚠️ **回写走 [`write_ui_key`]，且只回写 `[ui] lang` / `[ui] motion` 这两个键**。写的时候
//! 一次只碰那一个键、把它的 decor 原样摘贴回去，所以用户手写的注释与不认识的段都不动。
//! 这条设计的前提是"reader 读得懂 writer 写出的字节" ⇒ 由 `writing_a_commented_key_keeps_the_comment`
//! 那一族测试钉住，**判据是我们的 reader，不是 `toml_edit` 再 parse 一遍**（后者等于用写它的库证明它写对了）。

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
///
/// ⚠️ 这张表**不点名平台**（与 [`crate::paths::Env`] 同一条纪律）：unix 链读前两个、
/// Windows 链读后三个，各自只看自己那几个 ⇒ "同一份快照在两个平台上各落到哪个文件"
/// 在宿主上就测得到（见 `the_windows_config_chain_is_a_strict_ladder`）。
#[derive(Clone, Debug, Default)]
pub struct ConfigEnv {
    pub xdg_config_home: Option<String>,
    pub home: Option<String>,
    /// `%LOCALAPPDATA%`（Windows 链第一档；unix 上恒 `None`，读了也不进 unix 链）。
    pub local_appdata: Option<String>,
    /// `%APPDATA%`（roaming）。
    pub appdata: Option<String>,
    /// `%USERPROFILE%`（Windows 的"家"，与 unix 的 `home` **各读各的变量**）。
    pub userprofile: Option<String>,
}

impl ConfigEnv {
    pub fn from_process() -> Self {
        let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            xdg_config_home: grab("XDG_CONFIG_HOME"),
            home: grab("HOME"),
            // 三条 Windows 变量在 unix 上恒 None ⇒ 无条件读，不为不存在的变量写 cfg 分支。
            local_appdata: grab("LOCALAPPDATA"),
            appdata: grab("APPDATA"),
            userprofile: grab("USERPROFILE"),
        }
    }

    /// 只给 unix 链那两级回退用的环境变量（测试注入形状）。
    pub fn with(xdg: Option<&str>, home: Option<&str>) -> Self {
        Self {
            xdg_config_home: xdg.map(String::from),
            home: home.map(String::from),
            ..Self::default()
        }
    }

    /// 只给 Windows 链那三级回退用的环境变量（同一族的测试注入形状）。
    pub fn with_windows(local: Option<&str>, app: Option<&str>, user: Option<&str>) -> Self {
        Self {
            local_appdata: local.map(String::from),
            appdata: app.map(String::from),
            userprofile: user.map(String::from),
            ..Self::default()
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
    /// 按**当前平台**的链读那份文件（`cfg!(windows)` 在本文件里只出现两次：这里与
    /// [`write_ui_key`]）。测试一律走 [`read_in`](Self::read_in) /
    /// [`read_windows_in`](Self::read_windows_in)，把平台做成显式入参而不是依赖编译目标。
    pub fn read() -> Self {
        Self::read_on(&ConfigEnv::from_process(), cfg!(windows))
    }

    /// unix 链：`$XDG_CONFIG_HOME/opet/config.toml` → `$HOME/.config/opet/config.toml`。
    pub fn read_in(env: &ConfigEnv) -> Self {
        Self::read_at(config_path(env))
    }

    /// Windows 链：`%LOCALAPPDATA%` → `%APPDATA%` → `%USERPROFILE%`（见 [`config_path_windows`]）。
    pub fn read_windows_in(env: &ConfigEnv) -> Self {
        Self::read_at(config_path_windows(env))
    }

    /// **分岔本身是参数**：与 [`crate::paths::DbPath::resolve_on`] 同一个形状、同一个理由 ——
    /// 分岔写错在真机上的形状是"Windows 去读了一棵 unix 拼法的树"，而它编译得过。
    pub fn read_on(env: &ConfigEnv, on_windows: bool) -> Self {
        Self::read_at(config_path_on(env, on_windows))
    }

    /// 两条链的**共同**读法：路径已由链定好，这里只管"存在 / 不存在 / 读不出来"三档。
    fn read_at(path: PathBuf) -> Self {
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
    /// —— 文件还不存在时，用户要先知道该建在哪、写哪两个键，[`write_ui_key`] 才有个落点。
    pub fn describe(&self) -> String {
        let at = self.path.display();
        match self.source {
            ConfigSource::Absent => {
                format!("{at} 不存在（点菜单里的语言/动画会写它；手工建也行，写 [ui] lang/motion）")
            }
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

/// Windows 链，与 [`crate::paths::DbPath::resolve_windows_in`] /
/// [`crate::AssetRoot::resolve_windows_in`] 同一条三档阶梯：
/// `%LOCALAPPDATA%\opet\config.toml` → `%APPDATA%\opet\config.toml` →
/// `%USERPROFILE%\opet\config.toml`。
///
/// 为什么 Windows 上配置文件与资产/存档**同一个根族**而不是 `%APPDATA%` 优先：这三个东西是
/// 同一件事的三面（这台机器上这个用户的 OPet 状态），分散到两处会让"备份我的宠物设置"变成
/// 两句回答。而 `%LOCALAPPDATA%` 打头 = 与 `opet.db`、`pets/` 同一档，用户在资源管理器里
/// 打开一个目录就能看全。
///
/// ⚠️ 与 unix 链的**唯一**不对称：这里**没有** `~/.config` 那一形。Windows 上 `$HOME` 不存在
/// （Wine 会造一个，但那是 Wine 的事，见 `docs/DESIGN.md` 里"D5 Wine 判不了"那条纪律），
/// 拿 unix 拼法兜底只会建出一棵没人读的树。三档全不可用 ⇒ 兜底是 `%USERPROFILE%` 的空值拼法，
/// 路径变成**相对**的 `opet\config.toml`，而 [`Config::describe`] 照样把它打出来 —— 那一行就是
/// 用户看出"这台机器上根没解析出来"的唯一入口（同 `DbPath` 的 `UserProfile` 兜底档）。
fn config_path_windows(env: &ConfigEnv) -> PathBuf {
    for value in [
        env.local_appdata.as_ref(),
        env.appdata.as_ref(),
        env.userprofile.as_ref(),
    ] {
        if let Some(v) = value.filter(|v| crate::paths::is_windows_absolute(v.as_str())) {
            return PathBuf::from(v).join("opet").join("config.toml");
        }
    }
    PathBuf::from(env.userprofile.clone().unwrap_or_default())
        .join("opet")
        .join("config.toml")
}

/// 选链 = 参数（理由见 [`Config::read_on`]）。`cfg!` 只在 [`Config::read`] 与
/// [`write_ui_key`] 两处出现。
fn config_path_on(env: &ConfigEnv, on_windows: bool) -> PathBuf {
    if on_windows {
        config_path_windows(env)
    } else {
        config_path(env)
    }
}

/// 对账时要读回的那一个键。抽出来是因为 writer 里有**两处**都要问同一个问题（落盘前的
/// 字节、落盘后的磁盘），两处各写一遍 `match` 早晚漂移成两个键。
fn reads_back(key: &str, ui: &UiKeys) -> Option<String> {
    match key {
        KEY_LANG => ui.lang.clone(),
        _ => ui.motion.clone(),
    }
}

/// 写回 `[ui]` 的一个键，**按当前平台的链**（与 [`Config::read`] 对称：读的是哪个文件，
/// 写的就是哪个文件 —— 两条链各算各的会写出"设置存到 A、下次从 B 读"那对幽灵）。
pub fn write_ui_key(key: &str, value: &str) -> Result<PathBuf, String> {
    write_ui_key_on(&ConfigEnv::from_process(), cfg!(windows), key, value)
}

/// unix 链的写回（`$XDG_CONFIG_HOME` → `$HOME/.config`）。
pub fn write_ui_key_in(env: &ConfigEnv, key: &str, value: &str) -> Result<PathBuf, String> {
    write_ui_key_at(config_path(env), key, value)
}

/// Windows 链的写回（`%LOCALAPPDATA%` → `%APPDATA%` → `%USERPROFILE%`）。
pub fn write_ui_key_windows_in(env: &ConfigEnv, key: &str, value: &str) -> Result<PathBuf, String> {
    write_ui_key_at(config_path_windows(env), key, value)
}

/// 选链 = 参数（同 [`Config::read_on`]）。
pub fn write_ui_key_on(
    env: &ConfigEnv,
    on_windows: bool,
    key: &str,
    value: &str,
) -> Result<PathBuf, String> {
    write_ui_key_at(config_path_on(env, on_windows), key, value)
}

/// 写回 `[ui] lang` / `[ui] motion`，其余一概不碰（设计书 §9.2 第 19 条）。两条链的**共同**
/// 写法：`path` 已经由链定好，这里不再算路径。
///
/// 五条规则，每一条都是"违反了就等于把用户的文件弄坏"：
/// · **只改那一个键**：注释、空行、`[render]` 那些本轮不生效的段必须逐行活着 ⇒ 所以读改写
///   交给 `toml_edit`（它保留 decor），**不**能拿这只手写 reader 反推整份文件；
/// · **写之前先要求这份文件是合法 TOML**：解析不回来就**拒绝改写**，因为那时任何"改写"都会
///   把用户手写的东西刷平 —— 报错并交出路径，比"帮你修好文件"诚实；
/// · **写盘走同目录的临时文件 + rename**：`fs::write` 写一半被杀会把配置截断，那比丢注释更坏；
/// · **落盘前先用我们的 reader 判一次**那串要写出的字节（[`parse`]）：读不回同一个值就**在碰
///   磁盘之前**退出 ⇒ 那条报错说"文件未改动"是真话。抓的是 `ui = { lang = "zh" }` 这种
///   TOML 合法而我们的 reader 不认的形状；
/// · **落盘后再从**那个文件**读回来对账**（[`Config::read_at`]，**两处都绝不用 `toml_edit` 再
///   parse 一遍自验** —— 那等于用写它的库证明它写对了）。第二次判的东西和第一次不同：同目录里被
///   并发改写、rename 没换成功，只有"真在文件上的字节"这一侧看得见，而那一次已经改过文件 ⇒
///   所以它的报错只能说"已写出而读不回"，不能说"原样"。
///   ⚠️ M7e 起这里读的是**传进来的那个 `path`**，不是"再按环境算一次路径"：链有了两条，
///   重算一次就可能读到另一条链的文件，那句"从磁盘读回来"于是变成假话。
fn write_ui_key_at(path: PathBuf, key: &str, value: &str) -> Result<PathBuf, String> {
    if !UI_KEYS.contains(&key) {
        return Err(format!(
            "`{key}` 不是 [{SECTION_UI}] 的键（本轮只认 {UI_KEYS:?}）⇒ 没有写任何东西"
        ));
    }
    if value.trim().is_empty() {
        return Err(format!(
            "`{key}` 的值为空 ⇒ 拒绝写回（空值会被 reader 判成\"未设置\"并出声，写进去只是把用户原来的设置抹掉）"
        ));
    }
    let at = path.display().to_string();

    // 已存在的文件读不出来 = 拒绝，不是"当没有文件重写一份"。
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{at}: 现有配置文件读不出来（{e}）⇒ 拒绝改写")),
    };
    let mut doc: toml_edit::DocumentMut = if text.is_empty() {
        toml_edit::DocumentMut::default()
    } else {
        text.parse().map_err(|e| {
            format!("{at}: 现有内容不是合法 TOML（{e}）⇒ 拒绝改写（改写会把注释与其余行刷平）")
        })?
    };
    if doc.get(SECTION_UI).is_none() {
        doc[SECTION_UI] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    // 把原值的 decor（前导空白 + 行尾注释）摘下来贴回新值 ⇒ `lang = "zh"   # 界面语言`
    // 改完只剩注释换不掉，那正是"只改一个键"的含义。
    let decor = doc
        .get(SECTION_UI)
        .and_then(|ui| ui.get(key))
        .and_then(|it| it.as_value())
        .map(|v| v.decor().clone());
    let mut item = toml_edit::value(value);
    if let Some(d) = decor {
        if let Some(v) = item.as_value_mut() {
            *v.decor_mut() = d;
        }
    }
    doc[SECTION_UI][key] = item;
    let bytes = doc.to_string();

    // **落盘之前**先用我们自己的 reader 判一次（`parse` = reader 的纯函数部分）。放这一步的理由：
    // 一旦 rename 过去再发现读不回来，用户的文件已经被改坏了，那句报错救不回来。
    // 抓的是 `ui = { lang = "zh" }` 这种 **TOML 合法而我们的 reader 不认**的形状。
    let (preset, _) = parse(&bytes, &path);
    if reads_back(key, &preset).as_deref() != Some(value) {
        return Err(format!(
            "{at}: 将要写出的字节里 `{key}` 读不回 {value:?}（我们的 reader 只认 [{SECTION_UI}] 段下\
             `key = \"值\"` 那一行）⇒ **文件未改动**，请手工检查这份配置（例如内联表 `ui = {{ {key} = \"…\" }}` 这种写法）"
        ));
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}: {e}", dir.display()))?;
    }
    let tmp = {
        let mut name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".to_string());
        name.push_str(".opet-tmp");
        path.with_file_name(name)
    };
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写不了 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{at}: 落盘失败（{e}）⇒ 原文件未改动")
    })?;

    // 再从**磁盘上那个文件**读回一次：前面那次判的是"要写的字节"，这一次判的是"真在那个文件
    // 上的字节"（同一目录里被别人同时改写、rename 语义、临时文件没换成功，都只有这一侧看得见）。
    // ⚠️ 读的是 `path` 而**不是**"再按环境算一次"：链有了两条之后，重算可能落到另一个文件，
    // 那一句"从磁盘读回来"就变成假话（M7e 加 Windows 链时改的就是这一格）。
    let back = Config::read_at(path.clone());
    let got = reads_back(key, &back.ui);
    if got.as_deref() != Some(value) {
        return Err(format!(
            "{at}: 写进去 {value:?} 而从磁盘读回来 {got:?} ⇒ writer 与 reader 不是同一套语法，\
             这条设置重启后不会生效（⚠️ **文件已经改动**，到这一步没法退回，请手工检查）"
        ));
    }
    Ok(path)
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
        // 认到的键**故意不打印**（生效值与来源由 `overlay_common::ui::settings` 那一条摘要出）；
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
///
/// **右引号后面允许"空白 + `#` 注释"**（`lang = "zh" # 界面语言`）：这既是 TOML 的合法写法，
/// 也是 [`write_ui_key`] 写回的字节必然带的东西（`toml_edit` 把行尾注释留在 value 的 decor 里）。
/// reader 当初不接受它 = 我们自己写的文件我们自己读不回去，而"设置完语言重启就没了"比"打错
/// 引号"更常被撞上。严格 TOML 的裸回车、第二个值、`key = "a" "b"` 这类仍一律出声。
fn unquote(raw: &str) -> Result<String, String> {
    let Some(rest) = raw.strip_prefix('"') else {
        return Err(if raw.is_empty() {
            "等号右边是空的".to_string()
        } else {
            format!("值要用双引号括起来，这里写的是 {raw}")
        });
    };
    // 第一个 `"` 即闭合引号 ⇒ 引号内**从不**跨引号配对，`"a" "b"` 会走到下面那条
    // "右引号后面还写着东西"，而不是静默吞掉后半截。
    let Some(close) = rest.find('"') else {
        return Err(format!("少了右边的引号：{raw}"));
    };
    let inner = &rest[..close];
    let after = rest[close + 1..].trim_start();
    if !after.is_empty() && !after.starts_with('#') {
        return Err(format!(
            "右引号后面还写着东西：{raw}（只允许空白或 `#` 注释；本轮不支持转义 `\\\"`）"
        ));
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
    ///
    /// ⚠️ 样本路径用 [`crate::paths::host_abs`]（cwd 打底）而**不硬写 `/tmp/…`**：
    /// 硬写 unix 绝对形制在 Windows target 上不成绝对路径，整条断言会因样本而非规则失败
    /// —— 那是 M0 Wine 五红之一。规则本身与平台无关，样本必须无关。
    #[test]
    fn config_path_follows_the_same_xdg_rule_as_the_archive() {
        let xdg = crate::paths::host_abs("xdg-cfg");
        let home = crate::paths::host_abs("home_jun");
        let abs = Config::read_in(&ConfigEnv::with(Some(&xdg), Some(&home)));
        assert_eq!(
            abs.path,
            PathBuf::from(&xdg).join("opet").join("config.toml"),
            "$XDG_CONFIG_HOME 绝对 ⇒ 直接用它"
        );

        let rel = Config::read_in(&ConfigEnv::with(Some("relative/nope"), Some(&home)));
        assert_eq!(
            rel.path,
            PathBuf::from(&home)
                .join(".config")
                .join("opet")
                .join("config.toml"),
            "XDG 规范：相对值等于没设 ⇒ 必须回退 $HOME/.config"
        );

        // 与 DbPath 的对照：同一个相对值，两条链都必须落到 $HOME 而不是拼出相对路径。
        let db = crate::DbPath::resolve_in(
            &crate::paths::Env {
                opet_db: None,
                xdg_data_home: Some("relative/nope".into()),
                home: Some(home.clone()),
                ..crate::paths::Env::default()
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

    /// Windows 链是一条**严格阶梯**：四个档位各问一次"谁赢"。
    ///
    /// ⚠️ 这一族是**纯字符串**断言，样本是假路径（`C:\Users\…`）。宿主上**不许**伪造一个
    /// "看着能跑"的真写盘：`tempdir()` 是 `/tmp/…` 而 `is_windows_absolute` 判它不算 ⇒ 链会
    /// 落到兜底档，测的就不是那一档了；UNC 拼法更会在仓库 cwd 里长出反斜杠目录。
    /// **`%LOCALAPPDATA%` 上真长出字节那一格登记给 Wine 轮（M7-g），不在这里当通过。**
    /// 比较一律走"两边同一个 `join` 建出来的 `PathBuf`"而**不比字符串** ⇒ unix / Windows
    /// 宿主同绿（分隔符是宿主的事，不是这条规则的事）。
    #[test]
    fn the_windows_config_chain_is_a_strict_ladder() {
        let local = r"C:\Users\jun\AppData\Local";
        let roam = r"C:\Users\jun\AppData\Roaming";
        let user = r"C:\Users\jun";
        let at = |root: &str| PathBuf::from(root).join("opet").join("config.toml");

        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(
                Some(local),
                Some(roam),
                Some(user)
            )),
            at(local),
            "三档齐 ⇒ %LOCALAPPDATA% 赢（与 `opet.db`、`pets/` 同一个根）"
        );
        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(None, Some(roam), Some(user))),
            at(roam),
            "缺 %LOCALAPPDATA% ⇒ 退 %APPDATA%"
        );
        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(None, None, Some(user))),
            at(user),
            "只剩 %USERPROFILE%"
        );
        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(Some(""), Some(roam), Some(user))),
            at(roam),
            "空串 = 没设 ⇒ 不许拼出 `\\opet\\config.toml` 这种无根路径"
        );
    }

    /// 判"什么算绝对"用的是 [`crate::paths::is_windows_absolute`] 而**不是**
    /// `Path::is_absolute`。这个区别不是风格：后者在 unix 上认 `/home/jun` 绝对、在 Windows 上
    /// 不认 `C:\…` 绝对 ⇒ "哪一档赢"会变成"在哪个平台编译"的函数，而 `petstore` 两边都要编。
    /// 与库根/存档共用同一条判据（`paths::resolve_windows_in`），两处各写一遍迟早漂移成
    /// "库在 %LOCALAPPDATA% 而配置在别处"。
    #[test]
    fn the_windows_chain_only_accepts_values_that_are_windows_absolute() {
        let unc = r"\\nas\opet";
        let user = r"C:\Users\jun";
        // 盘符相对（`C:foo`）随"该盘的当前目录"变，绝不能当根
        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(
                Some("C:foo"),
                Some(unc),
                Some(user)
            )),
            PathBuf::from(unc).join("opet").join("config.toml"),
            "`C:foo` 不算 ⇒ 让给下一档（UNC 算）"
        );
        // unix 形制（Wine 会造一个 $HOME，真 Windows 不会）在这条链上不算
        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(
                Some("/home/jun"),
                None,
                Some(user)
            )),
            PathBuf::from(user).join("opet").join("config.toml"),
            "`/home/…` 不是 Windows 绝对 ⇒ 退 %USERPROFILE%"
        );
    }

    /// 三档全不可用 ⇒ 兜底**保留** `%USERPROFILE%` 的原始值（哪怕它不成绝对路径），而不是静默
    /// 变成空串。理由：[`Config::describe`] 打的那一行是用户看出"这台机器上根没解析出来"的唯一
    /// 入口，抹平它 = 症状消失而病还在（同 `DbPath` 的 `UserProfile` 兜底档）。
    #[test]
    fn the_windows_fallback_keeps_the_unusable_value_instead_of_silently_emptying_it() {
        let p = config_path_windows(&ConfigEnv::with_windows(
            Some("Local"),
            Some("Roaming"),
            Some("C:relative"),
        ));
        assert_eq!(
            p,
            PathBuf::from("C:relative").join("opet").join("config.toml"),
            "兜底要用 %USERPROFILE% 的原值，不许换成空串或 `opet\\config.toml`"
        );
        assert!(
            !crate::paths::is_windows_absolute(&p.to_string_lossy()),
            "这一格测的就是'它确实不可用'：兜底档不是 Windows 绝对 ⇒ 只能靠 describe 出声"
        );
        assert_eq!(
            config_path_windows(&ConfigEnv::with_windows(None, None, None)),
            PathBuf::from("").join("opet").join("config.toml"),
            "三个变量全无 = 相对 `opet\\config.toml`（同 DbPath 的空兜底）"
        );
    }

    /// 选链是**参数**不是编译目标：同一份 `ConfigEnv`（五种变量全填）喂 `read_on` 的两边，
    /// 必须各拿各的文件。`cfg!(windows)` 在本文件里只许出现在 [`Config::read`] 与
    /// [`write_ui_key`] 两处 ⇒ 除此之外任何"平台自己决定"的路径都是没接线的第三条链。
    ///
    /// ⚠️ unix 侧样本必须是**两侧都绝对**的路径，不许用 `/xdg` 这类字面量：`config_path`
    /// 判绝对用的是宿主 `Path::is_absolute`（unix 链在 unix 上跑，这是本模块唯一允许它的地方），
    /// 而 `"/xdg"` 在 Windows/Wine 上判否 ⇒ 退 `$HOME` 档，断言就变成"在哪个平台编译"的函数
    ///（M8 的 Wine 脚本第一轮正是红在这里——红的是样本的平台假设，不是链）。tempdir 在
    /// 宿主是 `/tmp/…`、在 Wine 是 `C:\users\…\Temp\…`，两侧各自绝对 ⇒ 链的语义不变、样本中性。
    #[test]
    fn the_two_chains_are_one_env_apart_and_nothing_else() {
        let dir = tempfile::tempdir().expect("临时目录");
        let xdg = dir.path().join("xdg").display().to_string();
        let home = dir.path().join("home").display().to_string();
        // Windows 侧样本同一条纪律：**不许**用 C:\Users\jun\AppData\Local 这种在某个宿主上
        // 会真实存在的值 —— Wine 的 %LOCALAPPDATA% 正映射到它，而 M7-e 起的真轮次往那里写过
        // config.toml ⇒ "Absent" 会红在环境（M8 第一轮实测）。用不存在的假档；Roaming 档
        // 指进 tempdir 并投毒，反证"阶梯在第一档就停、不下探"。
        let fake_local = r"C:\opet-missing-local-a1b2";
        let roam = dir.path().join("roaming");
        std::fs::create_dir_all(roam.join("opet")).expect("建投毒档");
        std::fs::write(
            roam.join("opet").join("config.toml"),
            "[ui]\nlang = \"ja\"\n",
        )
        .expect("投毒");
        let roam = roam.display().to_string();
        let env = ConfigEnv {
            xdg_config_home: Some(xdg.clone()),
            home: Some(home.clone()),
            local_appdata: Some(fake_local.into()),
            appdata: Some(roam.clone()),
            userprofile: Some(r"C:\Users\jun".into()),
        };
        let unix = Config::read_on(&env, false);
        let win = Config::read_on(&env, true);
        assert_eq!(
            unix.path,
            PathBuf::from(&xdg).join("opet").join("config.toml"),
            "unix 链看 XDG/$HOME，不许被 Windows 三档污染"
        );
        assert_eq!(
            win.path,
            PathBuf::from(fake_local).join("opet").join("config.toml"),
            "Windows 链看三档，不许回落 `$HOME/.config`（那棵树在真 Windows 上没人读）"
        );
        assert_ne!(
            unix.path, win.path,
            "两条链必须真的分岔，一样 = 有一边没接线"
        );
        // 两份都不存在 ⇒ 同一种沉默（Absent + 零出声），平台不该改变出声行为
        assert_eq!(unix.source, ConfigSource::Absent);
        assert_eq!(win.source, ConfigSource::Absent);
        assert!(
            win.ui.lang.is_none(),
            "ja 出现在这里 = 阶梯下探到投毒的 Roaming 档了"
        );
        assert!(unix.notices.is_empty() && win.notices.is_empty());
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

    // ---- writer（§9.2 第 19 条：带注释的 config.toml 写回后注释仍在）----

    /// 一份"用户真的会手写"的配置，**writer 的对照样本**。这十行里每一行的存在理由都要在
    /// 写完后仍然成立：顶部两条注释、一条空行、`[ui]` 两个键、其中一条带**行尾注释**、
    /// 又一条空行 + 一条注释、还有一个我们本轮不认识的 `[render]` 段与它的**数值**键。
    const COMMENTED: &str = r#"# OPet 的用户设置
# 手工编辑也行，菜单里的语言/动画也会写这里

[ui]
lang = "zh"   # 界面语言
motion = "full"

# 下面这段本轮不生效，reader 要出声而字节要原样留着
[render]
fps_active = 30
"#;

    /// writer 的沙盒。**刻意不复用 [`write_and_read`]**：它返回时 `TempDir` 就 drop 了，
    /// 而这一族测试有一半要在写完之后去读**磁盘上的字节**。
    struct Sandbox {
        dir: tempfile::TempDir,
        env: ConfigEnv,
    }

    impl Sandbox {
        /// `seed = None` = 那份配置还不存在（第一次写的那条路径）。
        fn with_text(seed: Option<&str>) -> Self {
            let dir = tempfile::tempdir().expect("临时目录");
            let conf = dir.path().join("opet");
            std::fs::create_dir_all(&conf).expect("建 opet/");
            if let Some(text) = seed {
                std::fs::write(conf.join("config.toml"), text).expect("写种子配置");
            }
            let env = ConfigEnv::with(dir.path().to_str(), Some("/home/nobody"));
            Self { dir, env }
        }

        /// 走 `config_path` 而不是自己拼 `opet/config.toml`：这一族测试要钉的就是
        /// **writer 落在 reader 会去读的那个路径上**，共用同一个函数才有这个保证。
        fn path(&self) -> PathBuf {
            config_path(&self.env)
        }

        /// 磁盘上的原始字节 —— "注释仍在"是字节层面的事，只看 reader 认不认得会漏掉整类缺陷。
        fn raw(&self) -> String {
            std::fs::read_to_string(self.path()).expect("读回 config.toml")
        }

        /// 临时文件有没有留在目录里（rename 成功 = 不该留；rename 失败 = 也得删掉）。
        fn leftovers(&self) -> Vec<String> {
            let mut out = Vec::new();
            for entry in std::fs::read_dir(self.dir.path().join("opet")).expect("读目录") {
                let name = entry
                    .expect("目录项")
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                if name.contains(".opet-tmp") {
                    out.push(name);
                }
            }
            out
        }
    }

    /// 旗舰那条：**改一个键，别的东西一个字节都不许多动**。
    ///
    /// 断言分三层，缺任何一层都放过一种真 bug：① **逐行 diff**（只有一行变 ⇒ 抓"整份重写"）
    /// ② **那一行里注释还在**（抓 decor 丢失）③ **我们的 reader 读得回来**（抓"写出 reader 不认
    /// 的字节"，而这一步正是 R 要解决的：`toml_edit` 把行尾注释写进值的 suffix ⇒
    /// `lang = "en"   # 界面语言` ⇒ R 之前的 reader 判"少了右边的引号"）。
    #[test]
    fn writing_a_commented_key_keeps_the_comment() {
        let sb = Sandbox::with_text(Some(COMMENTED));
        let before = sb.raw();

        let written = write_ui_key_in(&sb.env, KEY_LANG, "en").expect("写 lang 应当成功");
        assert_eq!(written, sb.path(), "返回的路径要就是 reader 读的那个");

        let after = sb.raw();
        let b: Vec<&str> = before.lines().collect();
        let a: Vec<&str> = after.lines().collect();
        assert_eq!(a.len(), b.len(), "行数变了 = 整份被重写：\n{after}");
        let diff: Vec<usize> = (0..b.len()).filter(|i| a[*i] != b[*i]).collect();
        assert_eq!(
            diff,
            [4],
            "改动面必须正好是 lang 那一行（第 5 行）；其余逐字节不动。差异行 = {:?}",
            diff.iter().map(|i| (i + 1, a[*i])).collect::<Vec<_>>()
        );
        assert!(a[4].contains("\"en\""), "新值没写进去：{}", a[4]);
        assert!(a[4].contains("# 界面语言"), "行尾注释被刷平了：{}", a[4]);

        // 第三层：判据是我们的 reader，不是 toml_edit 再 parse 一遍。
        let cfg = Config::read_in(&sb.env);
        assert_eq!(
            cfg.ui.lang.as_deref(),
            Some("en"),
            "写进去读不回：{}",
            sb.raw()
        );
        assert_eq!(cfg.ui.motion.as_deref(), Some("full"), "没让它动的键不许动");
        assert_eq!(
            cfg.notices.len(),
            1,
            "那一段 `[render] 本轮不生效` 必须还在（它没了 = 用户那段被删了）：{:?}",
            cfg.notices
        );
        assert!(
            sb.leftovers().is_empty(),
            "临时文件没清：{:?}",
            sb.leftovers()
        );
    }

    /// 上面那条测了 `lang`；这条测**另一个键**，而且断言的是"没被点的那一行逐字节相同"。
    /// 分页式写法（先整份反序列化再整体序列化）最容易在这里露出：它会顺手把别的行的间距、
    /// 引号风格、注释位置一起"规范化"。
    #[test]
    fn writing_the_other_key_leaves_the_first_line_byte_identical() {
        let sb = Sandbox::with_text(Some(COMMENTED));
        let before = sb.raw();

        write_ui_key_in(&sb.env, KEY_MOTION, "reduce").expect("写 motion 应当成功");

        let after = sb.raw();
        let b: Vec<&str> = before.lines().collect();
        let a: Vec<&str> = after.lines().collect();
        assert_eq!(a.len(), b.len(), "{after}");
        assert_eq!(a[4], b[4], "lang 那一行被蹭到了：{:?} → {:?}", b[4], a[4]);
        let diff: Vec<usize> = (0..b.len()).filter(|i| a[*i] != b[*i]).collect();
        assert_eq!(diff, [5], "改动面必须正好是 motion 那一行");
        assert!(
            a[5].contains("reduce") && !a[5].contains("full"),
            "{}",
            a[5]
        );

        let cfg = Config::read_in(&sb.env);
        assert_eq!(cfg.ui.lang.as_deref(), Some("zh"));
        assert_eq!(cfg.ui.motion.as_deref(), Some("reduce"));
        assert!(sb.leftovers().is_empty());
    }

    /// `toml_edit` 会写出行尾注释的**四种来源**，逐一由 reader 判（探针里 R 之前红的三个形状
    /// 都在这一张表里，第四个是"文件里根本没有 `[ui]` 段"）。
    ///
    /// 一张表而不是四条测试：它们的判据完全同形（"写完 reader 读得回、出声条数对"），
    /// 拆开就是把同一个断言抄四遍。
    #[test]
    fn the_shapes_toml_edit_emits_all_read_back_in_our_reader() {
        let cases: Vec<(&str, &str, &str, &str, usize)> = vec![
            (
                "原值带行尾注释",
                COMMENTED,
                KEY_LANG,
                "en",
                1, // 那一条 [render] 不生效
            ),
            (
                "两个键都带注释而只动其中一个",
                r#"[ui]
lang = "zh" # A
motion = "full" # B
"#,
                KEY_LANG,
                "en",
                0,
            ),
            (
                "单引号（reader 本来不认）被改成我们的写法",
                r#"[ui]
lang = 'zh' # 单引号
"#,
                KEY_LANG,
                "en",
                0,
            ),
            (
                "文件里没有 [ui] 段",
                r#"[render]
fps_active = 30
"#,
                KEY_MOTION,
                "off",
                1,
            ),
        ];

        for (name, seed, key, value, want_notices) in cases {
            let sb = Sandbox::with_text(Some(seed));
            let e = write_ui_key_in(&sb.env, key, value);
            assert!(e.is_ok(), "[{name}] 写失败：{:?}", e.err());

            let after = sb.raw();
            let cfg = Config::read_in(&sb.env);
            assert_eq!(
                reads_back(key, &cfg.ui).as_deref(),
                Some(value),
                "[{name}] 写完读不回，字节是：\n{after}"
            );
            assert_eq!(
                cfg.notices.len(),
                want_notices,
                "[{name}] 出声条数不对（多了=吵，少了=把用户的段吞了）：{:?}\n{after}",
                cfg.notices
            );
            assert!(
                sb.leftovers().is_empty(),
                "[{name}] 临时文件没清：{:?}",
                sb.leftovers()
            );
        }

        // 那两条"注释"的具体下落：动了的那条与没动的条都要活着。
        let sb = Sandbox::with_text(Some(
            r#"[ui]
lang = "zh" # A
motion = "full" # B
"#,
        ));
        write_ui_key_in(&sb.env, KEY_LANG, "en").unwrap();
        let after = sb.raw();
        assert!(
            after.contains("# A") && after.contains("# B"),
            "两条注释少一条：{after}"
        );
        assert!(
            after.contains("motion = \"full\""),
            "没动的键被规范化了：{after}"
        );
    }

    /// R 的另一半：**接受**"空白 + `#` 注释"，而**不接受**别的尾巴。
    /// 只测前一半会把 reader 从"严格"改成"宽松"，那正是这条设计最不该有的副作用。
    #[test]
    fn a_trailing_comment_is_allowed_while_trailing_junk_is_not() {
        let ok = write_and_read(
            r#"[ui]
lang = "zh"   # 界面语言
motion = "full"	# 动画强度（这一行故意用 tab 分隔注释）
"#,
        );
        assert_eq!(ok.ui.lang.as_deref(), Some("zh"));
        assert_eq!(ok.ui.motion.as_deref(), Some("full"), "tab 也算空白");
        assert!(
            ok.notices.is_empty(),
            "行尾注释不是错误，出声就是吵：{:?}",
            ok.notices
        );

        let bad = write_and_read("[ui]\nlang = \"a\" \"b\"\n");
        assert_eq!(bad.ui.lang, None, "尾巴不是注释却放行了");
        assert_eq!(bad.notices.len(), 1, "{:?}", bad.notices);
        assert!(
            bad.notices[0].contains("右引号后面还写着东西"),
            "{}",
            bad.notices[0]
        );
    }

    /// 拒绝改写的第一种：**现有文件不是合法 TOML**。这时连 `toml_edit` 都不该碰它。
    #[test]
    fn a_file_we_cannot_parse_is_refused_and_untouched() {
        let junk = "[ui\nlang = \nfps_active = oops\n";
        let sb = Sandbox::with_text(Some(junk));
        let before = sb.raw();

        let e = write_ui_key_in(&sb.env, KEY_LANG, "en").expect_err("非法 TOML 必须拒绝改写");
        assert!(
            e.contains("不是合法 TOML") && e.contains("拒绝改写"),
            "要说清为什么拒：{e}"
        );
        assert_eq!(sb.raw(), before, "拒绝就得一个字节都不动");
        assert!(sb.leftovers().is_empty());
    }

    /// 拒绝改写的第二种：**TOML 合法而我们的 reader 不认**（内联表）。
    ///
    /// 这条测的重点不是"报错"而是**报错的时机**：那道判卷落在 rename 之前 ⇒ 用户的文件
    /// 必须还是原样，那句"文件未改动"才是真话（写完之后才判的版本会让这句话变成假话）。
    #[test]
    fn an_inline_table_is_refused_before_the_file_is_touched() {
        let seed = "ui = { lang = \"zh\" }\n";
        let sb = Sandbox::with_text(Some(seed));
        let before = sb.raw();

        let e = write_ui_key_in(&sb.env, KEY_LANG, "en")
            .expect_err("reader 读不出内联表 ⇒ 不许写出去让它变成读得回的形状");
        assert!(e.contains("文件未改动"), "{e}");
        assert_eq!(
            sb.raw(),
            before,
            "这道红线守的是用户的文件，不是我们的错误信息"
        );
        assert!(sb.leftovers().is_empty());
    }

    /// 两种"根本不该走到磁盘"的输入：不是 `[ui]` 的键、空值。
    #[test]
    fn a_key_outside_ui_and_an_empty_value_write_nothing() {
        let sb = Sandbox::with_text(Some(COMMENTED));
        let before = sb.raw();

        let e = write_ui_key_in(&sb.env, "fps_active", "30").expect_err("段外的键要拒");
        assert!(
            e.contains("不是 [ui] 的键") && e.contains("没有写任何东西"),
            "{e}"
        );

        let e = write_ui_key_in(&sb.env, KEY_LANG, "   ").expect_err("空值要拒");
        assert!(e.contains("值为空") && e.contains("拒绝写回"), "{e}");

        assert_eq!(sb.raw(), before, "这两种拒绝根本没碰文件");
        assert!(sb.leftovers().is_empty());
    }

    /// 文件不存在时**要能把它造出来**（不然"设置持久化"这个功能在第一次用之前是空的）。
    #[test]
    fn the_first_write_creates_the_file_and_the_ui_section() {
        let sb = Sandbox::with_text(None);
        assert!(!sb.path().exists(), "前提：还不存在");

        let written = write_ui_key_in(&sb.env, KEY_LANG, "zh").expect("不存在也得写出来");
        assert_eq!(written, sb.path());

        let after = sb.raw();
        assert!(after.contains("[ui]"), "要自带段头：{after:?}");
        assert!(after.contains("lang = \"zh\""), "{after:?}");

        let cfg = Config::read_in(&sb.env);
        assert_eq!(cfg.source, ConfigSource::File);
        assert_eq!(cfg.ui.lang.as_deref(), Some("zh"));
        assert!(
            cfg.notices.is_empty(),
            "自己写的字节不该吵：{:?}",
            cfg.notices
        );
        assert!(sb.leftovers().is_empty());
    }
}
