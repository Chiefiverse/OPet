//! 生效值链的**装载面**（Windows 那一面）：读配置文件 + 取进程环境变量，喂给共用层那条唯一的链。
//!
//! 链本体在 [`overlay_common::settings`]（`--lang` > `config.toml` > `LC_ALL`/`LANG` > 默认，
//! `UI_REDESIGN_V2` §4.6）。本模块**一个字都不实现那条链** —— 它只回答两个平台各自的问题：
//!
//! · **读哪条路径**：[`petstore::config::Config::read`] 里的 `cfg!(windows)` 挑中的是
//!   Windows 那条三档阶梯 `%LOCALAPPDATA%` → `%APPDATA%` → `%USERPROFILE%`，**不是**
//!   unix 的 `$XDG_CONFIG_HOME` → `$HOME/.config`。为什么配置文件要跟 `opet.db`、`pets/`
//!   同一个根族（`%LOCALAPPDATA%` 打头）而不是按"Windows 惯例"放 `%APPDATA%`：那三个东西是
//!   同一件事的三面（这台机器上这个用户的 OPet 状态），分散两处会让"备份我的宠物设置"变成
//!   两句回答。规则与四条链的对称性都在 `petstore::config` 里钉着（`AGENTS.md` 现场事实第 7
//!   条那一族"两个事实源"，这里不重述）。
//! · **读哪个环境变量**：`LC_ALL` → `LANG`，与 Wayland 那家同一条取法。
//!
//! ⚠️ **这里刻意不调 `GetUserDefaultUILanguage`。** 原生 Windows 通常没有 `LANG`，所以 env
//! 那一档在真机上一般是空的，生效值实际落在"文件 → 默认"两级。补一个 Win32 locale API 不是
//! 移植的收尾动作，而是一个**产品决定**（"宠物跟着系统语言走"意味着用户改系统设置就换界面，
//! 而面板上那一行语言是他自己点的 ⇒ 两者谁赢需要新的一条优先级规则，还要 `Win32_Globalization`
//! 特性与一次 DLL 白名单重测）。登记，不在本轮顺手做。
//!
//! 出错的两条路（形状与 Wayland 完全一致，判分在共用层）：**命令行给错 = `Err`**（`cli.rs`
//! 据此退出，"我显式说了 en 而你给了我 zh"不能降级糊过去）；**文件/环境变量给错 = 出一条
//! `notices` 后按链条往下走**（坏文件不该让宠物起不来，但必须出声）。
//!
//! 写回**不在本模块**：链本身一个字不改文件，落盘职责在 `App::persist_ui_key`（面板/菜单那一行
//! 点下去）与 [`run`](crate::app::run) 启动那一发的首轮写回，两处都只调
//! [`petstore::config::write_ui_key`] —— 它按当前平台选链，于是"写的那个文件"与"上面读的那个
//! 文件"结构性同址，不需要这里再保证一次。

use petstore::config::Config;

pub use overlay_common::settings::{
    FileUi, Motion, Settings, ValueSource, lang_codes, motion_codes, unknown_lang, unknown_motion,
    validate_cli,
};

/// 从真实进程环境取 `LC_ALL` → `LANG` + 从已读好的配置里取 `[ui]` 那两行，再走共用层的
/// [`Settings::resolve`]。
///
/// `cfg` 由调用方读（`app.rs` 启动那一处）：这里不重复算路径，否则"读哪个文件"就有了第二个
/// 事实源（Wayland 那家同一条纪律）。
pub fn from_process(
    cli_lang: Option<&str>,
    cli_motion: Option<&str>,
    cfg: &Config,
) -> Result<Settings, String> {
    let grab = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
    let env_lang = grab("LC_ALL").or_else(|| grab("LANG"));
    Settings::resolve(cli_lang, cli_motion, file_ui(cfg), env_lang.as_deref())
}

/// 把配置里那一层搬进链的入口（就是 [`from_process`] 里那一句搬运）。测例单独用它是因为
/// `from_process` 还要读**进程**环境变量，而那是并行测试的雷区（edition 2024 里改进程环境
/// 是 `unsafe`）⇒ 测例只钉"文件层 → 链"这一段。
fn file_ui(cfg: &Config) -> FileUi<'_> {
    FileUi {
        lang: cfg.ui.lang.as_deref(),
        motion: cfg.ui.motion.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use overlay_common::l10n::Lang;
    use petstore::config::ConfigEnv;

    /// 面板/菜单点一行 ⇒ `App::persist_ui_key` 落盘 ⇒ 下次启动那条链要把**同一个值**从文件层
    /// 认回来。这一条走的是整段接缝（Windows 链 → reader → [`FileUi`] → 共用层 `resolve`），
    /// 钉的东西很具体：**行上印的 `code()` 就是文件层认的那个字符串**。
    ///
    /// 为什么留在各家而不上提：`overlay-common` 不链 `petstore`（`no_platform_deps.rs` 守着），
    /// 而这一条测的正是"配置装载面"那一格。Wayland 走 unix 链，这里走 Windows 链 ⇒ 各测各的
    /// 那段搬运（`AGENTS.md` 现场事实第 7 条的"两个事实源"在这条接缝上的形状就是"两份搬运、
    /// 一条链"）。
    ///
    /// ⚠️ **路径注入用的是阶梯的兜底档**：`ConfigEnv::with_windows(None, None, tempdir)`。
    /// 理由是宿主上造不出"既 Windows-绝对、又可创建"的目录（`tempdir()` 是 `/tmp/…`，
    /// [`crate`] 依赖的 `is_windows_absolute` 判它不算 ⇒ 前三档全空，链落到
    /// "`%USERPROFILE%` 原值拼法"那一档，而那一档**不查绝对性**，于是宿主上可写）。
    /// **所以这条测的是码位往返，不是三档谁赢**：后者由 `petstore` 那四条纯字符串门禁钉着，
    /// 而"`%LOCALAPPDATA%\opet\config.toml` 上真长出字节"那一格**登记给 Wine 轮（M7-g）**，
    /// 不在宿主拿兜底档当它过了（`AGENTS.md`：Wine 判不了的格子不许当通过，宿主更不行）。
    #[test]
    fn every_written_code_comes_back_through_the_windows_loader() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = ConfigEnv::with_windows(None, None, dir.path().to_str());
        let read_back = || Config::read_windows_in(&env);
        let last_lang = Lang::ALL[Lang::ALL.len() - 1];

        for lang in Lang::ALL {
            let at = petstore::config::write_ui_key_windows_in(
                &env,
                petstore::config::KEY_LANG,
                lang.code(),
            )
            .unwrap_or_else(|e| panic!("写语言码 {:?} 该成功：{e}", lang.code()));
            // writer 返回的就是它落盘的那个路径 ⇒ 路径只有一处算（`App` 里不许再算一遍）。
            assert_eq!(
                at,
                read_back().path,
                "{lang:?} 的落点不是 Windows reader 看的那个文件"
            );
            let s = Settings::resolve(None, None, file_ui(&read_back()), None).unwrap();
            assert_eq!(
                (s.lang, s.lang_from),
                (lang, ValueSource::ConfigFile),
                "{lang:?} 写进去而链上认不回来（屏上点了有用、重启就丢 = 半条功能）：{s:?}"
            );
            assert!(s.notices.is_empty(), "{:?}", s.notices);
        }

        for motion in Motion::ALL {
            petstore::config::write_ui_key_windows_in(
                &env,
                petstore::config::KEY_MOTION,
                motion.code(),
            )
            .unwrap_or_else(|e| panic!("写动画码 {:?} 该成功：{e}", motion.code()));
            let s = Settings::resolve(None, None, file_ui(&read_back()), None).unwrap();
            assert_eq!(
                (s.motion, s.motion_from),
                (motion, ValueSource::ConfigFile),
                "{motion:?} 写进去而链上认不回来：{s:?}"
            );
            // 两个键互不相干：写 motion 必须留着上面那四次语言写入的最后一档
            //（"只改那一个键"这条规则在 UI 侧的唯一可见证据）。
            assert_eq!(s.lang, last_lang, "写 motion 把 lang 刷掉了");
        }
    }

    /// 本模块**只读 Windows 那条链**：`%LOCALAPPDATA%` 一旦是合法值，reader 看的就是它，
    /// 而不是 unix 链或那个临时目录里内容完好的那份文件。这一条防的是"移植时顺手用了 unix 链"——
    /// 那种 bug 在宿主上全绿（两家都读 unix 形制的路径），真机上症状是"设置存到 A、下次从 B 读"。
    #[test]
    fn the_loader_reads_the_windows_chain_not_the_unix_one() {
        let dir = tempfile::tempdir().expect("临时目录");
        std::fs::create_dir_all(dir.path().join("opet")).expect("建 opet/");
        std::fs::write(
            dir.path().join("opet").join("config.toml"),
            "[ui]\nlang = \"en\"\n",
        )
        .expect("写 config.toml");

        // 样本先立住：同一个目录，unix 链认它（绝对）而 Windows 链的三档**都不认**它。
        let via_unix = Config::read_in(&ConfigEnv::with(dir.path().to_str(), None));
        assert_eq!(via_unix.source, petstore::config::ConfigSource::File);
        assert_eq!(
            via_unix.ui.lang.as_deref(),
            Some("en"),
            "样本本身要先成立，否则下面那条断言是空的"
        );

        // 阶梯第一档是合法 Windows 绝对值 ⇒ 落点就是它，那个有内容的目录**不是**候选。
        // ⚠️ 三个形状坑，都在这条测例上踩过或会踩：
        //  1) 不许写真实 AppData 形制的字面量（旧写法 C:\Users\jun\AppData\Local）——宿主上恒
        //     不存在 ⇒ 白过，而这套 exe 在 Wine 里真跑，M7-e 起真轮次会往真 Local 写文件
        //     ⇒ "Absent" 红在环境不红在产品。
        //  2) 不许拿 unix 路径当档位——is_windows_absolute 不收，阶梯会**掉进回退档**，
        //     旧写法那条 path 断言在 Linux 上其实是回退值与期望值的巧合，不是阶梯判出来的。
        //  3) 档位用 UNC 形（\\ 前缀）：`PathBuf` 在 Linux 上也把 "C:\…" 当相对段（b[0] 不是
        //     '/'），而 UNC 以 '\\' 开头 ⇒ 两侧都是"绝对、不存在"，join 的分隔符混排无害。
        // 第二、三档投毒是白捡的加强：判"阶梯在第一档就停、不下探、不回退 unix 目录"。
        // 档位必须用 join 派生，不许 display() 拼字面量：宿主分隔符是 '/'、Wine 是 '\'
        //（tempdir 在 Wine 报 C:\users\jun\…），嵌一段带反斜杠的字面量在两侧形状都不一样。
        let local = r"\\opet-fake\local";
        let roam = dir.path().join("fake-roaming");
        std::fs::create_dir_all(&roam).expect("建假 Roaming 档");
        std::fs::write(roam.join("config.toml"), "[ui]\nlang = \"ja\"\n").expect("投毒第二档");
        let roam = roam.display().to_string();
        let ladder = Config::read_windows_in(&ConfigEnv::with_windows(
            Some(local),
            Some(&roam),
            dir.path().to_str(),
        ));
        assert_eq!(
            ladder.path,
            std::path::PathBuf::from(local)
                .join("opet")
                .join("config.toml"),
            "Windows reader 的第一档必须是 %LOCALAPPDATA%，不许落到临时目录那份"
        );
        assert_eq!(ladder.source, petstore::config::ConfigSource::Absent);
        assert!(
            ladder.ui.lang.is_none(),
            "读错了文件 = 那份 en 会出现在这里（ja 出现在这里 = 阶梯下探了）"
        );
        assert!(ladder.notices.is_empty(), "文件不存在必须彻底安静");

        // 档位在链上的两条路（§7-R14）：坏值出声而**不**失败，静默变默认的是"跳过那一档"。
        let bad = Settings::resolve(
            None,
            None,
            FileUi {
                lang: Some("klingon"),
                motion: None,
            },
            None,
        )
        .expect("文件层坏值不该让链失败");
        assert_eq!(bad.lang, Lang::DEFAULT, "坏值跳过那一档，落到默认");
        assert!(
            bad.notices.iter().any(|n| n.contains("klingon")),
            "坏值不出声 = 用户永远不知道自己写错了：{:?}",
            bad.notices
        );
    }
}
