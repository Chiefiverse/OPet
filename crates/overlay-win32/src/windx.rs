//! Win32 边界上的两类**手工编解码**：`lParam` 的整数打包，与 UTF-16 的进出。
//! 类型全是自己定义的（`i32`/`isize`/`Vec<u16>`/`&[u16]`），所以宿主上能测。
//!
//! ⚠️ 这个文件之所以存在而不是直接 `use windows_sys::...::GET_X_LPARAM`：`windows-sys`
//! **不导出** `GET_X_LPARAM`（它是 `windowsx.h` 里的 C 宏，而 `-sys` crate 只搬函数与类型）。
//! 于是每个人都会自己写一遍，而写错的那一版（`LOWORD` / `HIWORD` = **无符号**）在主屏左边的
//! 副屏上炸 —— 那里窗口坐标是真的负的。这条在本项目里有名字：读错通道的尺子。
//!
//! 打包那一半的判据 = 下面前三例，其中 `a_negative_coordinate_survives_the_unpack` 是全部
//! 意义所在：无符号那一版在正坐标上**逐位相同**，所以"我在主屏上试过了"永远量不到它。
//!
//! UTF-16 那一半（[`wide`] / [`from_wide`]）管的是另外两种形状：**少了终止符就往堆里读**，
//! 以及**零填充的定长字段读回来带一条看不见的尾巴** ⇒ 名字比较恒假。两例各钉一边，
//! 且都配了"错的那一版确实给出别的数"的反面对照（没有反面对照的测例在验自己的假设）。

/// 从 `LPARAM` 取 x。
///
/// 形参是 `isize` 而不是 `i32`，因为那**就是** `LPARAM` 的类型（windows-sys 给的是
/// `isize` ⇒ 64 位上是 64 位）。这不是"签名宽一点"：坐标那一对只有低 32 位有效，而
/// `(lparam as u16)` 截的正是低 16 位，所以在 64 位 LPARAM 上与在 32 位上一字不差。
///
/// 等价于 C 的 `GET_X_LPARAM`：`(short)(LOWORD(lParam))`。要点是那次 `as i16` 的**符号扩展**，
/// 而不是 `& 0xFFFF`。
pub fn lparam_x(lparam: isize) -> i32 {
    (lparam as u16) as i16 as i32
}

/// 从 `LPARAM` 取 y = `GET_Y_LPARAM`。
///
/// 第 32 位以上是什么都行：`>> 16` 之后 `as u16` 只取第 16–31 位。真正要紧的是**先移位再截
/// 16 位再 `as i16`** 这个次序（反过来写成先 `as i16` 再 `>> 16` 会在负 y 上丢符号），
/// 见 `the_sign_is_extended_not_truncated` 的反面对照。
pub fn lparam_y(lparam: isize) -> i32 {
    ((lparam >> 16) as u16) as i16 as i32
}

/// 打包回一个 `LPARAM`。**只在测例与文档里出现**：真消息由系统给。
///
/// 它存在的理由是"下面那三例不许靠手写十六进制"—— 手写的常量一旦与解包式子同错，
/// 测试就在验自己的假设（本项目 `合成件 vs 真件` 那一族）。有了配对的打包器，
/// 三例都能写成 `(x, y) → pack → unpack == (x, y)` 这种不会自证的形状。
#[cfg(test)]
fn pack_lparam(x: i16, y: i16) -> isize {
    (((y as u16) as isize) << 16) | ((x as u16) as isize)
}

// ─────────────────────────── UTF-16 进出 ───────────────────────────

/// Rust `&str` → Win32 那个 `PCWSTR` 要的**以 NUL 结尾**的 UTF-16 缓冲。
///
/// 返回值是 `Vec<u16>` 而不是裸指针：调用方拿它是值，指针从值的**内存**里出来
/// （`wide("opet-pet").as_ptr()`），于是生命期由调用点那一条表达式管着，而 `PCWSTR`
/// 要的 `*const u16` 不需要任何 OS 类型就能写出来 ⇒ 这一对函数留在宿主侧。
///
/// ⚠️ 末尾那颗 `0` 不是格式偏好。`RegisterClassExW` 的 `lpszClassName`、
/// `CreateWindowExW` 的窗口名、`SetWaitableTimer` 都不读长度字段 ⇒ 少了终止符时
/// 它**继续往下读堆**：类名变成 `opet-pet` 后面接上当时内存里的任意东西。那个形状最阴的地方
/// 是"有时候正好接上一个 0"⇒ 同一份 `.exe` 有时注册成功、有时 `CreateWindowExW` 报
/// `ERROR_FILE_NOT_FOUND`（类不存在）。本项目对这一族的命名是**沉默降级**：
/// 报错发生在别处、而根因在这里。⇒ [`the_terminator_is_always_last_and_only_last`] 钉它。
pub fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// Win32 定长 UTF-16 字段（`MONITORINFOEXW::szDevice` 那一类 `[u16; 32]`）→ `String`。
///
/// **在第一个 NUL 处截断**，这一条是本函数的全部意义：那些字段是"零填充的定长数组"，
/// 而 `String::from_utf16_lossy(buf)` 会把整份零尾巴收进去 ⇒ 得到 `\\.\DISPLAY1` 后面
/// 挂一串 NUL。那个串**看起来**就是设备名（打印时尾巴看不见！），
/// 而 `name == wanted` 从此恒假 ⇒ `--monitor \\.\DISPLAY1` 报"找不到那台显示器"，
/// 而屏幕上那行明明写着它。⇒ 这是本项目"提示不是判据"那一族的 Windows 版。
///
/// 满 32 个字符而**没有** NUL 的情况（设备名最长正好 31 + 终止符，所以实际不会满；
/// 但结构体是 `[u16; 32]` 而我不能靠"实际不会"）：整段照收，不 panic、不越界。
///
/// 坏 UTF-16（落单的代理）：走 lossy 替换符，**不 panic**。理由与 D3 那条硬约束同源
/// （宠物崩不许影响桌面）：一个来自系统的怪名字不该让进程起不来。
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主屏上：与无符号那一版**逐位相同** ⇒ 这条只能证明"没写反 x/y"。
    #[test]
    fn positive_coordinates_unpack_as_their_16bit_halves() {
        let p = pack_lparam(1234, 567);
        assert_eq!((lparam_x(p), lparam_y(p)), (1234, 567));
    }

    /// 主屏**左边**那块屏给的就是负 x（本机的多屏形状：`hyprctl` 里也见过负 origin，
    /// Windows 同理）。`LOWORD` 那一版会把 `-3200` 读成 `62336` ⇒ 指针一落到副屏，
    /// 内核收到的 x 就是"屏幕右边外面"，宠物当场卡住不动而窗口还在。
    #[test]
    fn a_negative_coordinate_survives_the_unpack() {
        for (x, y) in [
            (-3200i16, 400),
            (400, -1080),
            (-1, -1),
            (i16::MIN, i16::MAX),
        ] {
            let p = pack_lparam(x, y);
            assert_eq!(
                (lparam_x(p), lparam_y(p)),
                (x as i32, y as i32),
                "({x},{y}) 解错了：实读 ({}, {})",
                lparam_x(p),
                lparam_y(p)
            );
        }
        // 而且错的那一版**确实**会给出别的数 —— 否则上面这条是在验空集（同 `no_platform_deps`
        // 那条 `the_census_is_not_empty` 的理由）。
        let p = pack_lparam(-3200, 400);
        assert_eq!(
            p as u16 as i32, 62336,
            "无符号读法应当给正数，否则这条对照没意义"
        );
    }

    /// 16 位边界：`i16::MIN` 与 `-1` 都必须带符号扩展，`u16` 截断不许发生。
    ///
    /// 这一条与上一条同族但**分开写**，因为它打的是另一处：`>> 16` 之后再 `as i16`
    /// 与先 `as i16` 再 `>> 16` 在两处都能写错，而错向不同（前者对、后者在负 y 上符号丢失）。
    #[test]
    fn the_sign_is_extended_not_truncated() {
        assert_eq!(lparam_x(pack_lparam(-1, 0)), -1);
        assert_eq!(lparam_y(pack_lparam(0, -1)), -1);
        assert_eq!(lparam_x(pack_lparam(i16::MIN, 0)), -32768);
        assert_eq!(lparam_y(pack_lparam(0, i16::MIN)), -32768);
        // 反面对照：如果 y 那一支写成 `(lparam >> 16) as u16 as i32`（少了 `i16`），
        // -1 会变成 65535。这里把它显式算一遍，证明我们的写法与那个不同。
        let p = pack_lparam(0, -1);
        assert_ne!(((p >> 16) as u16) as i32, lparam_y(p));
    }

    /// 终止符必须在**最后**且**只有**一颗。
    ///
    /// 断"最后一个 == 0"不够：写成 `push(0)` 两次（或 `s` 本身结尾带 NUL）也能满足，
    /// 于是这条与下一条合起来才封住两边 —— 多的那颗不会让 API 出错，但会让
    /// `from_wide(wide(x))` 不等于 `x`，而那个往返是 monitor.rs 里名字比较的唯一依据。
    #[test]
    fn the_terminator_is_always_last_and_only_last() {
        for s in ["opet-pet", "\\\\.\\DISPLAY1", "", "宠物"] {
            let w = wide(s);
            assert_eq!(
                w.last().copied(),
                Some(0),
                "{s:?} 没有 NUL 结尾 ⇒ RegisterClassExW 会读穿堆"
            );
            let nul = w.iter().filter(|&&c| c == 0).count();
            assert_eq!(nul, 1, "{s:?} 的 NUL 不止一颗 ⇒ 往返会丢字符");
            assert_eq!(w.len(), s.encode_utf16().count() + 1, "{s:?} 长度不对");
        }
    }

    /// 零填充定长数组必须在第一个 NUL 处断掉。
    ///
    /// 这条断的是**假名字**那一族：尾巴不收，`==` 恒假，屏幕上明明写着那台显示器而程序
    /// 说找不到。用 `[u16; 32]` 而不是切片：那才是 `szDevice` 的真实形状。
    #[test]
    fn a_zero_padded_field_reads_back_as_the_name_only() {
        let mut buf = [0u16; 32];
        for (i, u) in wide("\\\\.\\DISPLAY1").into_iter().take(32).enumerate() {
            buf[i] = u;
        }
        assert_eq!(from_wide(&buf), "\\\\.\\DISPLAY1");
        // 反面对照：不收尾巴的那一版**确实**给出别的数，否则上面那条是空集。
        let naive = String::from_utf16_lossy(&buf);
        assert_ne!(
            naive, "\\\\.\\DISPLAY1",
            "整段读出来与名字相同 ⇒ 这条对照失效"
        );
        assert!(naive.chars().count() == 32 && naive.len() > "\\\\.\\DISPLAY1".len());
    }

    /// 满格无 NUL：不 panic、不越界，整段照收。
    #[test]
    fn an_unterminated_full_buffer_is_taken_whole() {
        let buf: Vec<u16> = "x".repeat(32).encode_utf16().collect();
        assert_eq!(buf.len(), 32);
        assert_eq!(from_wide(&buf), "x".repeat(32));
    }

    /// 坏 UTF-16 走替换符而**不 panic**；代理对原样往返。
    #[test]
    fn broken_utf16_degrades_lossily_and_surrogate_pairs_round_trip() {
        // 落单的高代理：Windows 那边不会因此死掉，我们也不许。
        let broken = [0xD83Cu16, b'A' as u16, 0];
        let s = from_wide(&broken);
        assert!(s.chars().count() >= 2, "{s:?}");
        assert!(s.contains('A'), "坏元素不许连带吃掉后面的合法字符");
        // 非 BMP（emoji 是代理对）：wide 用 encode_utf16 ⇒ 两颗 code unit，往返无损。
        let name = "宠物\u{1F43E}";
        assert_eq!(from_wide(&wide(name)), name);
        assert_eq!(
            wide(name).len(),
            5,
            "宠(1) + 物(1) + 狗爪(代理对 2) + 终止(1)"
        );
    }
}
