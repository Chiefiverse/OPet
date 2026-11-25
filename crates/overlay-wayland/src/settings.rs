//! 生效值链的**装载面**：读配置文件 + 取进程环境变量，喂给共用层那条唯一的链。
//!
//! 分成三个模块不是洁癖，是三条各有其主的规则：
//! · `petstore::config` 只管**文件语法**与路径，返回原始字符串（不解释语义 ⇒ 不产生
//!   `petstore → UI` 的反向依赖）；
//! · `l10n::Lang` 只管**词表**与它自己的码（`parse`/`code`/`self_name`）；
//! · **`overlay_common::ui::settings` 管"谁赢"**（`--lang` > `config.toml` > `LC_ALL`/`LANG` >
//!   默认，`UI_REDESIGN_V2` §4.6）—— 2026-11-24（M7）把链本体提上去了，覆盖 2026-11-22
//!   拍板③那句"解析链留在各家客户端"。当时留在这里的理由是"链要读 `petstore::config`，而
//!   `Lang` 还在 wayland 里"，两个前提都没了；而两家各一份链 = 同一份优先级规则两个事实源，
//!   症状是"加了第五种语言，Windows 那份的报错还写四种"。**这里只剩搬运与一次进程环境读取**，
//!   那条链在这个 crate 里没有任何一行副本。
//!
//! ⚠️ 上面那句"唯一读 `LC_ALL`/`LANG` 的地方"**现在只对这一家成立**：`from_process` 是这一层
//! 的出口，共用层的 `Settings::resolve` 吃的是**传进来的字符串**（纯函数，四条分支能在并行
//! 测试里逐条打到，而不用改进程环境 —— edition 2024 里那是 `unsafe`）。
//!
//! ## 出错的两条路，形状不一样（§7-R14）
//!
//! 两条都在共用层判完：**命令行给错 = `Err`**（`cli.rs` 据此退出，"我显式说了 en 而你给了我
//! zh"不能降级糊过去）；**文件/环境变量给错 = 出一条 `notices` 后按链条往下走**（一条坏文件
//! 不该让宠物起不来，但必须出声，否则"配置支持多语言"只在 grep 里存在）。这里原样透传。
//!
//! ⚠️ 本模块**不碰键盘导航**：菜单与面板的 surface 都是 `keyboard_interactivity=None`（D14），
//! 所以生效语言只有"改文件/加参数 + 重启"和"点面板上的语言/动画行"两条路。
//!
//! **第二条路会写回文件**（拍板④「设置功能……点击菜单弹出窗口……并实现功能」）。旧版这里
//! 写的是"只改本进程（§4.6 不回写文件）"，那条理由 = 当时只有自写的平坦 reader，读改写会把
//! 用户注释刷平；`petstore::config::write_ui_key`（`toml_edit` 保 decor + 落盘前后各用**我们的**
//! reader 判一遍）把那个理由消掉了 ⇒ 写回的职责在 `app::App::persist_ui_key`，**不在本模块**：
//! 这里只管把两层取出来喂进链，链本身一个字不改（文件仍是 CLI 之下、环境变量之上）。

use petstore::config::Config;

pub use overlay_common::ui::settings::{
    FileUi, Motion, Settings, ValueSource, lang_codes, motion_codes, unknown_lang, unknown_motion,
    validate_cli,
};

/// 从真实进程环境取 `LC_ALL` → `LANG`（前者按 POSIX 约定覆盖后者）+ 从已读好的配置里取
/// `[ui]` 那两行，再走共用层的 [`Settings::resolve`]。
///
/// `cfg` 由调用方读（`app.rs` 启动那一处），因为**读哪条路径**这件事各家不同：`Config::read()`
/// 走环境变量，测例走 `Config::read_in(env)`。这里不重复算路径。
pub fn from_process(
    cli_lang: Option<&str>,
    cli_motion: Option<&str>,
    cfg: &Config,
) -> Result<Settings, String> {
    let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
    let env_lang = grab("LC_ALL").or_else(|| grab("LANG"));
    Settings::resolve(
        cli_lang,
        cli_motion,
        FileUi {
            lang: cfg.ui.lang.as_deref(),
            motion: cfg.ui.motion.as_deref(),
        },
        env_lang.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l10n::Lang;

    /// 面板上点一行 ⇒ `app::App::persist_ui_key` 走 `write_ui_key_in` 落盘 ⇒ 下次启动
    /// 那条链要把**同一个值**从文件那一层认回来。这一条走的是整条链（reader → `FileUi` →
    /// 共用层的 `resolve`），而它钉的东西很具体：**行上印的 `code()` 就是文件层认的那个字符串**。
    ///
    /// 为什么这条**留在各家**而不跟着链一起上提：`overlay-common` 不链 `petstore`（那是它
    /// 立身之本的一部分，由 `no_platform_deps.rs` 守着），而这一条测的正两家的**接缝**——
    /// 配置装载面。Wayland 读 `$XDG_CONFIG_HOME`、Windows 读 `%LOCALAPPDATA%`，两家的
    /// 路径不同而码位相同 ⇒ 各测各的那一段搬运。
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
        // 把文件里那一层搬进链的入口（就是 `from_process` 里那三行搬运，测例自己走一遍）。
        fn ui(cfg: &Config) -> FileUi<'_> {
            FileUi {
                lang: cfg.ui.lang.as_deref(),
                motion: cfg.ui.motion.as_deref(),
            }
        }
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
            let s = Settings::resolve(None, None, ui(&read_back()), None).unwrap();
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
            let s = Settings::resolve(None, None, ui(&cfg), None).unwrap();
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
