//! 界面**值类型**与 CLI 预检：`Motion` / `Lang` 的档位语义、来源标签、报错拼法。
//!
//! 2026-11-22 随绘制层上提到这里（M5 第二批拍板③）。拆法是两条各有其主的规则：
//! · **这里**管"某个字符串认不认得、认出来是哪一档、报错怎么说" —— 纯函数，零 IO；
//! · **各家客户端的 `settings.rs`** 管"谁赢"（`--lang` > `config.toml` > `LC_ALL`/`LANG` >
//!   默认那条链，`UI_REDESIGN_V2` §4.6）—— 链条要读 `petstore::config`，装载面各家不同，
//!   但两家用的是**同一份档位语义与同一两句报错**（`unknown_lang`/`unknown_motion` 在这里，
//!   复制出第二份必然在"加第五种语言"那天变成谎话）。
//!
//! ⚠️ `Lang` 本体（词表与码）在 [`crate::l10n`]，这里只引用它做档位判定。

use crate::l10n::Lang;

/// 生效值是从哪一层来的。**必须随值一起打印**：否则用户无法区分"我配的生效了"和
/// "恰好默认值和我配的一样"（验收 §6-4 那条优先级链就是靠这一列判的）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueSource {
    Cli,
    ConfigFile,
    EnvVar,
    Default,
    /// 语言/动画行当场点出来的（`lang` 与 `motion` **两档都有**这一入口，都在面板上）。
    ///
    /// 留着这一档是为了让退出摘要不说谎：启动时读到的那一档在点过行之后就不是生效来源了，
    /// 把 `lang_from` 停在 `ConfigFile` 等于告诉排查的人"文件里原本写着 en"。
    ///
    /// ⚠️ **这一档现在也写回文件**（拍板④），但 `as_str` 里**不写"已持久化"**：写回可能失败
    /// （文件不是合法 TOML、目录不可写、reader 读不回那个形状），而这一列在退出摘要里是
    /// **无条件打印**的，一句"已持久化"在失败那次就是假话。落没落地由两处单独说：stderr 的
    /// `ui: lang = "…" 已写回 <路径>` 与面板/菜单那条错误行（客户端的 `App::persist_ui_key`）。
    MenuClick,
    /// §3.12 `lang.set` 经控制套接字改的那一次。与 [`Self::MenuClick`] 同一形状的理由：
    /// 退出摘要的来源列必须说实话，把 socket 改的记成"菜单/面板当场切换"就是把被控实例
    /// 的一轮读成有人摸过它 —— 而"这轮到底谁动的"正是并发对照轮要排除的事。
    Ipc,
}

impl ValueSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "命令行参数",
            Self::ConfigFile => "config.toml",
            Self::EnvVar => "环境变量",
            Self::Default => "默认值",
            Self::MenuClick => "菜单/面板当场切换",
            Self::Ipc => "控制套接字(opet ctl)",
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

    /// 设置页那一行的循环顺序：full → reduce → off → full。与 [`Self::ALL`] 同序 ⇒
    /// "屏幕上试的顺序"与"`--motion` 给的顺序"不会分家。
    pub const fn next(self) -> Self {
        match self {
            Self::Full => Self::Reduce,
            Self::Reduce => Self::Off,
            Self::Off => Self::Full,
        }
    }
}

/// 报错/出声时把支持的语言列出来。**由 `Lang::ALL` 生成**，不在字符串里手抄一遍
/// （手抄的那一份会在加第五种语言时变成谎话）。
pub fn lang_codes() -> String {
    Lang::ALL
        .iter()
        .map(|l| l.code())
        .collect::<Vec<_>>()
        .join("/")
}

/// 同一条手法用在 motion 上。
pub fn motion_codes() -> String {
    Motion::ALL
        .iter()
        .map(|m| m.code())
        .collect::<Vec<_>>()
        .join("/")
}

/// 两句"认不出来"的报错**只在这里拼**：CLI 预检（[`validate_cli`]）与各家的链条本体
/// （`Settings::resolve`）用的是同两个函数，所以"参数阶段说的支持范围"和"运行阶段说的"
/// 不可能漂移 —— 手抄两遍的话，加第五种语言时必然有一份变成谎话。
pub fn unknown_lang(v: &str) -> String {
    format!("--lang 的值 {v:?} 认不出来（只支持 {}）", lang_codes())
}

pub fn unknown_motion(v: &str) -> String {
    format!("--motion 的值 {v:?} 认不出来（只支持 {}）", motion_codes())
}

/// `--lang` / `--motion` 的**预检**，给各家 `cli` 的参数解析用：参数写错要在解析阶段
/// 就退出，而不是等客户端连上显示服务、建好 surface 之后。
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
