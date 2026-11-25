//! 「概率」那一屏的五行：一行图例 + 四档各一行。
//!
//! 2026-11-24（M7-b 前置）整段从 `overlay-wayland/src/app.rs` 搬上来，**函数体一字未改**。
//!
//! 它是这一层里唯一**不碰库**的那条路，而且这是事实不是遗漏：公示对每个人都一样，
//! 让它去连库就等于"内存库（`--db` 没给）的玩家读不到公示"。也因此它不需要任何
//! `ensure_gacha()` 前置 ⇒ 两家客户端的「概率」那一行在演示态里同样有读数。
//!
//! 写成自由函数（不挂 `Gacha`、不挂 `App`）的理由是门禁要能直接吃到它：这一批第一次把
//! 「概率」的字送进像素，而"每一行都画得出来而且不破版"这件事的入口是鼠标，我绝不注入点击
//! ⇒ 它必须在 headless 里可调用。

use overlay_common::l10n::{Key, Lang};
use petcore::gacha::{DEFAULT_RATES_JSON, GachaParams, Rarity, RatesFile};

/// 「概率」那一屏要上屏的**五行整**：一行图例 + 四档各一行。
///
/// ⚠️ **这条路不碰库**，而且是故意的：它读的是编译期就嵌进二进制的 `rates.json`
/// （`DEFAULT_RATES_JSON` = `RATES_ASSET` 那份，与 `catalog rates --out` 写的是同一个产物），
/// 回答的是"承诺了什么"而不是"这次抽到了什么"。所以它**不需要** `ensure_gacha()` ——
/// 让它去连库就等于"内存库的玩家读不到公示"，而公示对每个人都一样。
///
/// `Published::check` 是硬约束 #2（"概率必须与公示一致"）的守门人：文件与代码不一致时
/// 这一行报**不一致**，而不是把那份已经在说谎的文件念给用户听。宁可不答，不可答错。
///
/// **五行 = `panel::MAX_ROWS - 1`**，一格余量都不留：这一面第六格固定是 `Close`，多一行
/// 就得靠 `panel::collection_rows` 那条截断补「+N 更多」，而"你还没看到的第四档概率"被
/// 截成一句话是这一屏最坏的失败形状。
///
/// 两处口径是明写的取舍（不是遗漏）：
/// - **两位小数**（`{:.2}`）而不是 `percent_str` 的四位：`SSR 60.0000>57.0147` 一定超 240 px
///   那一行的排版预算（§4 那次量化）。四位那一份没消失，它就在 `rates.json` 里。
/// - 原先那行"公示口径 method=/视界=/format="**不再上屏也不进日志**：五行装不下它，而点
///   「概率」要问的是"某一档多大概率"，不是"这份公示用什么口径推的"。
///
/// 写成**自由函数**（`Gacha` 与 `App` 都不碰）是为了让面板那两条门禁能直接吃到它：
/// 这一批第一次把「概率」的字送进像素，而"每一行都画得出来而且不破版"这件事的入口是鼠标，
/// 我绝不注入点击 ⇒ 它必须在 headless 里可调用。挂在 `App` 上就等于把自动化让给了真机。
pub fn published_rates_lines(lang: Lang) -> Result<Vec<String>, String> {
    let published = RatesFile::read(DEFAULT_RATES_JSON)?;
    let effective = published.check(&GachaParams::V1)?;
    // 百万分位 → 百分数。这里**只有**显示用的格式化碰浮点：`petcore` 里那份
    // `percent_str` 走整数是为了公示字符串逐字符可复现（硬约束 #8），而这一行不进任何
    // 重放契约，`{:.2}` 换来的排版余量才是这一屏要付的代价。
    let pct = |w: u32| w as f64 / 10_000.0;
    let mut lines = vec![format!("{} b>c", lang.t(Key::Rates))];
    for r in Rarity::ALL {
        lines.push(format!(
            "{} {:.2}>{:.2}",
            r.as_str(),
            pct(published.base.weight(r)),
            pct(effective.weight(r))
        ));
    }
    Ok(lines)
}
