//! 文字层：把入库的 CFF 子集（`assets/fonts/ui.otf`）栅格成卡片像素。
//!
//! 这是设计书 §6.1.4 判据③那条岔路的 (b) 路线落地：纯 Rust 栅格器 + 打包字体子集，
//! 一次性解决"精细度"和"中文/假名/谚文"两件事。上一版这里能画的只有 95 个 5×7 ASCII 位图
//! （[`crate::font`]），而盲盒目录里 30 件东西**全都有中文名** ⇒ 抽到「蓝帽」只能打 `cap_blue`。
//!
//! 三条这一层自己扛的东西，不要往调用方漏：
//!
//! 1. **预乘**。画布是 `wl_shm` 的 `Argb8888`，小端 = B,G,R,A，且 `wl_shm` 要的是**预乘**
//!    alpha（`atlas::premultiplied_bgra` 同一口径）；字形给的却是**直通**覆盖率。漏了预乘就是
//!    字边缘一圈黑描边，而"有多少像素被画过"这类断言照样全绿 —— M4 那条老坑换了个面回来
//!    （`docs/UI_REDESIGN_V2.md` §7-R3）。blend 里写的就是这条。
//! 2. **缓存有显式上界**。真抽那一档的 VmHWM 是 18,420 kB，19 MB 门禁的余量只有 ≈580 KB，
//!    所以 `arena` 在启动时**一次性** `resize` 到 `CACHE_MAX_BYTES` 之后不再长。满了就现栅格化
//!    不缓存：慢一点，但内存是个可先验的数，而不是"这次达标下次不达标"（§7-R4）。
//! 3. **字体出事不许炸进程**。[`face`] 返回 `Option`：资产坏了就一行字都不画，宠物照跑
//!    （硬约束 3「宠物崩溃不能影响桌面」优先于整洁）。但单测 `face_is_available` 强制断言它是
//!    `Some` —— 否则 CI 走 `font.rs` 兜底、真机走真字体，同一条截断会截在**不同的字**上（§7-R5）。
//!
//! ## 字号：逻辑定一次，物理只有三档
//!
//! 逻辑字号 [`LOGICAL_PX`] 与显示器缩放无关（`menu.rs` 那条"预算必须与 scale 无关"的老教训），
//! 物理档 [`TIERS`] 只有三个。`physical_px` 把任意 scale 夹到**最近的一档**，所以
//! `measure`/`draw`/`line_height` 只需要为这三个数各证一次，不用证一个连续域。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ab_glyph::{Font, FontArc, OutlinedGlyph, ScaleFont};

/// 入库的字体子集本体。**这就是资产**，不是从文件系统找的 —— `include_bytes!` 保证
/// 客户端二进制里那份与 `tools/subset_font.py --verify` 钉住摘要的那份是同一个。
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/ui.otf");

/// 码位清单（`assets/fonts/ui-codepoints.txt`，每行 `U+XXXX # 来源`）解析出的**全集**。
///
/// 这不是第二份清单 —— 它读的还是那一个文件，只是把解析器放在这里，让单元测试与
/// `tests/font_assets.rs` 用**同一个**口径（两处各写一份，就会在"哪一行算码位"上漂移）。
///
/// ⚠️ 十六进制必须显式给基数：`"0050".parse::<u32>()` 是**五十**而 `0x50` 是 `P`。本项目
/// 第一版就栽在这儿 —— 解析出 264 个"码位"、严格递增、无重复，三条结构断言全绿，实际值却
/// 一半是错的（含字母的直接变 `None` 被丢掉）。这正是"数字全绿但画面对不上"。
pub fn codes_list() -> Vec<u32> {
    const CODES: &str = include_str!("../assets/fonts/ui-codepoints.txt");
    CODES
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("U+")?;
            let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            u32::from_str_radix(&hex, 16).ok()
        })
        .collect()
}

/// 正文的逻辑字号（logical px）。取代 `menu.rs::TEXT_PX = 2`（那是"逻辑 2 px/字模像素"，
/// 只对手写 5×7 位图成立）。
///
/// ⚠️ **这一条偏离批准的方案（§4.1 与 §9-O1 写的是 12），依据是实测覆盖率而不是口味。**
/// 同一把尺子量「蓝」「盲」「帽」，`cff_spike_rasterizes_a_hanzi` 每次跑都把这张表打全（下面
/// 是本次实测）：
///
/// | 物理 px | 水墨包围盒 | 最密像素的覆盖率 | 覆盖率 ≥200 的像素数 |
/// |---|---|---|---|
/// | 12（方案的逻辑 12 @1×） | 8×8–8×9 | 179–208 | 0–1 |
/// | 18（方案的逻辑 12 @1.5× = 本机） | 11–12×12–13 | 230–246 | 3–19 |
/// | **16**（本档 @1×） | 11×11 | 230–233 | 3–11 |
/// | **24**（本档 @1.5× = 本机） | 15–16×15–17 | 254–255 | 25–51 |
/// | **32**（本档 @2×） | 20–21×21 | 255 | 92–114 |
///
/// "最密像素也到不了实心"意味着**没有任何一笔是填满一个像素的**：一笔 13 画的字被压进 8×8 的
/// 格子 ⇒ 那不是"有点小"，是整行糊成一团灰（§7-R6 点名的正是这个）。逻辑 16 在本机 1.5× 上落到
/// 24 物理 px，是唯一让 CJK 出现实心笔画核的那一档，而逻辑代价可控：`CARD_W − 2×PAD = 224` 还能
/// 排 14 个汉字，目录里最长的名字是 3 字；横向由 `every_locale_fits_the_card` 逐 locale × 逐档
/// 量，最宽的 en 行（`+999999 shards Balance 9,999,999`）占 180/224。
///
/// **`menu.rs` 的四个布局常数因此一个都不用改**（§4.5 的要求成立）：这份子集 `line_gap = 0`
/// （实测 16/24/32 px → ascent 13/19/26、descent −3/−5/−6），所以 **`line_height(px) == px`**
/// ⇒ 一行占的逻辑高度正好等于 `LOGICAL_PX`，`LINE_STEP = 18 ≥ 16 + 2` 仍成立，`ROW_H 26`、
/// `HEAD_H 22`、footer 的 26 px 带也都装得下一行 16。
pub const LOGICAL_PX: i32 = 16;

/// 物理字号档 = `round(LOGICAL_PX × scale)`，scale ∈ {1, 1.5, 2} ⇒ {16, 24, 32}。
///
/// 只有三档是**预算**不是审美：缓存键是 `(档, char)`，每多一档就要为同一批字形多存一份位图，
/// 而 arena 是死的（[`CACHE_MAX_BYTES`]，论证见 `one_tier_of_every_glyph_fits_the_arena`）。
pub const TIERS: [i32; 3] = [LOGICAL_PX, 24, 32];

/// 位图缓存的硬上限。`arena` 一开始就占满它 ⇒ VmHWM 的增量在启动那一刻就定了，不是"跑多少算多少"。
///
/// 口径：**必须容得下"一个档位 × 清单里全部 237 个码位"**，因为一次会话只用一档
/// （`pick_output()` 取一台显示器，scale 进程内恒定）。实测 `one_tier_of_every_glyph_fits_the_arena`：
/// 16 px = 20,155 B、24 px = 43,561 B、32 px = **73,564 B**（最坏）⇒ 96 KiB 有 ≈24 KiB 余量。
/// 装不下就会每 tick 现栅格化几个字 —— 那是判据④禁止的"靠重绘换好看"的另一种形态。
///
/// 溢出路径不是纸上的：`cache_never_exceeds_the_arena` 在**一个进程里连画三档**（共 137 KB >
/// 96 KB）⇒ 72 个字形走"现画不缓存"，一个都没丢。真会话碰不到这条，因为它是三档之和。
pub const CACHE_MAX_BYTES: usize = 96 * 1024;

pub type Face = FontArc;

/// 字体句柄；`None` ⇒ 资产坏了，调用方应当**不画字**而不是 panic。
///
/// `OnceLock` 而非 `lazy_static`：零新依赖，且解析只发生在第一次取用（不是启动必经路径 ⇒
/// `--probe` 这类只读诊断不会被一个 19 MB 的 .ttc 拖慢）。
pub fn face() -> Option<&'static Face> {
    static FACE: OnceLock<Option<Face>> = OnceLock::new();
    FACE.get_or_init(|| FontArc::try_from_slice(FONT_BYTES).ok())
        .as_ref()
}

/// `scale`（1.0 / 1.5 / 2.0…）→ 最近的物理字号档。越界一律 clamp，不返回没准备的字号。
///
/// 非有限值与 ≤0 的 scale 走最小档：宁可字小一点，也不要拿 NaN 去索引。
pub fn physical_px(scale: f32) -> i32 {
    if !scale.is_finite() || scale <= 0.0 {
        return TIERS[0];
    }
    let want = LOGICAL_PX as f32 * scale;
    *TIERS
        .iter()
        .min_by_key(|&&t| ((t as f32 - want).abs() * 1000.0) as i32)
        .unwrap_or(&TIERS[0])
}

/// 该字在这份字体里**有没有**字形（cmap 查得到且不是 `.notdef`）。
pub fn has_glyph(c: char) -> bool {
    face().is_some_and(|f| f.glyph_id(c).0 != 0)
}

/// 第一个画不出来的字（缺字 = 卡片上静默出现 `?`）。全都能画就 `None`。
///
/// 这是 §4.2 那对"清单 ⇔ 会渲染"相等门禁管不到的那一半：它们钉的是**清单**，而这里看的是
/// **真正到达画布的那串字**（有人绕过 `l10n` 硬写一句文案，就是从这儿红）。
pub fn missing(s: &str) -> Option<char> {
    let mut out = None;
    for c in s.chars() {
        // ASCII 空白不算"缺字"：它没有轮廓但也不该报（`"余额 1,240"` 里的空格）。
        if !c.is_ascii_whitespace() && !has_glyph(c) {
            out = Some(c);
            break;
        }
    }
    out
}

/// `c` 的推进宽度（整物理像素）。逐 char 取整，`draw` 的笔位走的是同一个数 ⇒
/// `measure` 与真正画出来的右边界**不会差 1 px**（截断判定靠这个一致性）。
fn advance(f: &Face, px: i32, c: char) -> i32 {
    let sf = f.as_scaled(px as f32);
    if c.is_ascii_whitespace() {
        // 空格/制表符在 cmap 里可能落在 .notdef（子集只收可打印 ASCII 之外的空白没意义）。
        return (px / 3).max(1);
    }
    sf.h_advance(sf.glyph_id(c)).round() as i32
}

/// 整串的宽度（物理像素）。
pub fn measure(s: &str, px: i32) -> i32 {
    let Some(f) = face() else { return 0 };
    s.chars().map(|c| advance(f, px, c)).sum()
}

/// 基线以上的度数（物理像素，正数）。
pub fn ascent(px: i32) -> i32 {
    face().map_or(px, |f| f.as_scaled(px as f32).ascent().round() as i32)
}

/// 基线以下的度数（物理像素，**负数**，与 ab_glyph 同口径）。
pub fn descent(px: i32) -> i32 {
    face().map_or(-(px / 4), |f| {
        f.as_scaled(px as f32).descent().round() as i32
    })
}

/// 一行占多少物理像素：`ascent − descent + line_gap`，全部由字体度量给，不是常数。
///
/// `menu.rs::LINE_STEP` 从"字模 7 px × scale 2 + 4 leading"改成吃这个数 ⇒ 换字体/换字号
/// 不用重新手算一遍行距（`menu_row_step_matches_the_font` 钉住这条口径）。
pub fn line_height(px: i32) -> i32 {
    face().map_or(px + px / 2, |f| {
        let sf = f.as_scaled(px as f32);
        (sf.ascent() - sf.descent() + sf.line_gap()).round() as i32
    })
}

/// 缓存里的一条位图：`off` 指向 `arena` 里的 `w×h` 字节覆盖率（行主序，无 pitch 补齐）。
#[derive(Clone, Copy)]
struct Sprite {
    w: u16,
    h: u16,
    /// 相对笔位基线的左上角偏移。
    left: i16,
    top: i16,
    /// `Some` = 在 arena 里；`None` = 装不下了，这条活在 `scratch` 里（只当次有效）。
    off: Option<u32>,
}

#[derive(Default)]
struct Cache {
    /// 启动时一次性占满，之后不长 ⇒ §2 的内存门禁量的是**这个**数，不是"这次跑了多少"。
    arena: Vec<u8>,
    used: usize,
    map: HashMap<(i32, char), Sprite>,
    /// 满了之后的退路。只有一个，因为一次只画一个 char。
    scratch: Vec<u8>,
    hits: usize,
    rasterized: usize,
    uncached: usize,
}

impl Cache {
    fn new() -> Self {
        // 关键：不是"用到再长"，是**先占死**。VmHWM 因此可以按常数先验。
        Self {
            arena: vec![0u8; CACHE_MAX_BYTES],
            ..Default::default()
        }
    }

    fn bytes(&self, sp: &Sprite) -> &[u8] {
        let n = sp.w as usize * sp.h as usize;
        match sp.off {
            Some(off) => &self.arena[off as usize..off as usize + n],
            // `None` 只可能是**这一次**刚栅格化、又装不进 arena 的字形 ⇒ scratch 里就是它。
            // 所以 overflow 路径决不能把这条记进 `map`（见 [`Cache::sprite`]）：下一次命中时
            // scratch 早被别的字换掉 ⇒ 画出来的是**另一个字**的位图，而没有任何数字会红。
            None => &self.scratch[..n],
        }
    }

    /// 拿到 `c` 在 `px` 档的位图；未命中就栅格化，能塞进 arena 就塞，塞不下走 `scratch`。
    ///
    /// arena 满 ⇒ **不逐出、也不进 `map`**。不逐出的理由（§7-R4）是"同一句文案在不同时刻
    /// 画得不一样"；不进 `map` 的理由更强 —— 位图只在 `scratch` 里活一次（[`Cache::bytes`]）。
    /// 代价是那批字每 tick 现栅格化：慢，但**内存仍是先验的常数**，且画出来的东西可复现。
    fn sprite(&mut self, f: &Face, px: i32, c: char) -> Option<Sprite> {
        if let Some(sp) = self.map.get(&(px, c)) {
            self.hits += 1;
            return Some(*sp);
        }
        let (sp, cov) = rasterize(f, px, c)?;
        self.rasterized += 1;
        let n = sp.w as usize * sp.h as usize;
        if self.used + n > self.arena.len() {
            self.uncached += 1;
            self.scratch = cov;
            return Some(sp);
        }
        let off = self.used as u32;
        self.arena[self.used..self.used + n].copy_from_slice(&cov);
        self.used += n;
        let sp = Sprite {
            off: Some(off),
            ..sp
        };
        self.map.insert((px, c), sp);
        Some(sp)
    }
}

/// 缓存藏在函数里而不是 `static`，是为了让 `arena` 的"一次性占满"发生在**第一次画字**时
/// （`--probe` 那种不画字的诊断路径就不该占 96 KiB）。
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new()))
}

/// 栅格化一个字形：返回 `(位置与尺寸, 覆盖率 w×h 字节)`。覆盖率是**直通**的 0..=255，
/// 预乘发生在 [`blend_pixel`] —— 顺序反了就是字边缘一圈黑描边（§7-R3）。
fn rasterize(f: &Face, px: i32, c: char) -> Option<(Sprite, Vec<u8>)> {
    let sf = f.as_scaled(px as f32);
    // position = (0,0) 且 y 已经是"屏幕向下、原点 = 基线笔位"⇒ px_bounds 的 min 直接就是偏移。
    let outlined: OutlinedGlyph = sf.outline_glyph(sf.scaled_glyph(c))?;
    let bounds = outlined.px_bounds();
    let w = bounds.width().max(0.0) as usize;
    let h = bounds.height().max(0.0) as usize;
    let sp = |off: Option<u32>| Sprite {
        w: w as u16,
        h: h as u16,
        left: bounds.min.x.round() as i16,
        // px_bounds 已经是"屏幕坐标（y 向下）、原点 = 基线笔位"⇒ min 直接就是要加的偏移。
        top: bounds.min.y.round() as i16,
        off,
    };
    if w == 0 || h == 0 {
        // 没有墨（空格、零宽字形）：不是错误，空位图。
        return Some((sp(None), Vec::new()));
    }
    let mut cov = vec![0u8; w * h];
    outlined.draw(|x, y, a| {
        let (x, y) = (x as usize, y as usize);
        if x < w && y < h {
            // 覆盖率是 0..=1 的 f32；乘 255 后四舍五入，>255 夹住（ab_glyph 允许 ≥1.0）。
            cov[y * w + x] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    });
    Some((sp(None), cov))
}

/// `rgb`（0xRRGGBB，直通色）以覆盖率 `a` 叠到**预乘** BGRA8 画布上的一个像素。
///
/// source-over，源 alpha = a、源 rgb 预乘 = rgb·a；画布已是预乘，所以要乘的是 `dst`：
/// `out = dst × (255 − a) + rgb × a`（除以 255，`+127` 是为了四舍五入而不是恒偏小）。
/// alpha 通道同式：`out_a = dst_a × (255 − a)/255 + a`。
///
/// ⚠️ 写成 `out = dst + rgb*a`（漏掉 `(255−a)` 那一项）就是"字边缘黑描边"的来源：
/// 背景在边缘像素上没有被压暗，等于把背景按不透明处理，而 a 只有半格。
#[inline]
fn blend_pixel(dst: &mut [u8], rgb: u32, a: u8) {
    if a == 0 {
        return;
    }
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let keep = 255 - a as u32;
    let mul = |c: u32, d: u8| ((c * a as u32 + d as u32 * keep + 127) / 255) as u8;
    // 小端 Argb8888 的内存顺序 = B, G, R, A
    dst[0] = mul(b, dst[0]);
    dst[1] = mul(g, dst[1]);
    dst[2] = mul(r, dst[2]);
    dst[3] = (a as u32 + dst[3] as u32 * keep / 255).min(255) as u8;
}

/// 画一串字，返回笔的落点（= 起点 + [`measure`] 的同一个数）。
///
/// * `canvas`/`stride` —— 预乘 BGRA8 画布与每行字节数（`card.rs` 的同一份）。
/// * `x`/`baseline_y` —— 第一个字的**笔位**与基线（物理像素，可以出界：出界的部分逐像素裁掉）。
/// * `px` —— 物理字号，取自 [`physical_px`]。
/// * `rgb` —— 0xRRGGBB 直通色。墨色由底色算出来的规则在 [`crate::card::ink_on`]，不在这里。
///
/// 字体不可用（[`face`] 返回 `None`）⇒ 什么都不画并返回 `x`；这是"降级"而不是错误。
pub fn draw(
    canvas: &mut [u8],
    stride: usize,
    mut x: i32,
    baseline_y: i32,
    px: i32,
    rgb: u32,
    s: &str,
) -> i32 {
    let Some(f) = face() else { return x };
    if s.is_empty() || stride == 0 {
        return x;
    }
    let rows = canvas.len() / stride;
    for c in s.chars() {
        let adv = advance(f, px, c);
        // cmap 里没有这个字 ⇒ 画 `?`。画个洞更"诚实"，但人眼对洞的分辨率远低于对 `?` 的，
        // 而这条本来就是给"忘了重裁字体"准备的现场证据（自动化那边由 `missing` 拦）。
        let glyph_c = if has_glyph(c) || c.is_ascii_whitespace() {
            c
        } else {
            '?'
        };
        if !c.is_ascii_whitespace() {
            let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(sp) = guard.sprite(f, px, glyph_c) {
                let (w, h) = (sp.w as usize, sp.h as usize);
                let (gx, gy) = (x + sp.left as i32, baseline_y + sp.top as i32);
                let cov = guard.bytes(&sp);
                for j in 0..h {
                    let sy = gy + j as i32;
                    if sy < 0 || sy as usize >= rows {
                        continue;
                    }
                    for i in 0..w {
                        let sx = gx + i as i32;
                        if sx < 0 || sx as usize * 4 >= stride {
                            continue;
                        }
                        let a = cov[j * w + i];
                        if a == 0 {
                            continue;
                        }
                        let base = (sy as usize) * stride + (sx as usize) * 4;
                        blend_pixel(&mut canvas[base..base + 4], rgb, a);
                    }
                }
            }
        }
        x += adv;
    }
    x
}

/// 已占用的缓存字节数（≤ [`CACHE_MAX_BYTES`]）。退出摘要打这个数，让"字体这层的内存代价"
/// 是个实测值而不是"应该不多"。
pub fn cache_bytes() -> usize {
    cache()
        .lock()
        .map(|c| c.used)
        .unwrap_or_else(|_| CACHE_MAX_BYTES)
}

/// 缓存的计数诊断：`(命中, 栅格化, 因 arena 满而没进缓存的字形数)`。
pub fn cache_stats() -> (usize, usize, usize) {
    match cache().lock() {
        Ok(c) => (c.hits, c.rasterized, c.uncached),
        Err(_) => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l10n::Lang;
    use petcore::gacha::Catalog;

    /// §7-R1 的 30 分钟门禁：**栅格器到底吃不吃 Type 2 charstring**，外加"字号档量不值得读"
    /// 的实测表（那张表就是 [`LOGICAL_PX`] 从方案的 12 改成 16 的唯一依据）。
    ///
    /// 本机所有中文字体都是 CFF（`fc-list :lang=zh fontformat` 唯一值 `CFF`），而不少纯 Rust
    /// 栅格器历史上只吃 `glyf`。这一条红了就是整轮返工，所以它排在所有别的活前面。
    #[test]
    fn cff_spike_rasterizes_a_hanzi() {
        let f = face().expect("CFF spike：字体解不出来 ⇒ §7-R1 的退路（cu2qu 转 glyf / 换字形）");
        let mut rows = Vec::new();
        for px in TIERS {
            for ch in ['蓝', '盲', '帽', '조', 'か'] {
                let (sp, v) = rasterize(f, px, ch).expect("清单里的字却栅格不出 ⇒ 资产与清单漂了");
                let cov = v.len();
                let (w, h) = (sp.w as usize, sp.h as usize);
                assert_eq!(cov, w * h, "位图长度与包围盒不符");
                let inked = v.iter().filter(|&&a| a > 0).count();
                let solid = v.iter().filter(|&&a| a == 255).count();
                let dense = v.iter().filter(|&&a| a >= 200).count();
                let peak = v.iter().max().copied().unwrap_or(0);
                // ① 有轮廓：CJK/谚文/假名在这一档应当接近满 em，笔画塌了才会小。
                //    高度**按文种分档**是实测出来的，不是给宽松：24 px 上「조」的墨是 15×13、
                //    「か」更矮 —— 谚文音节与假名的设计高度本来就低于 em 方格，而汉字不能。
                let min_h = match ch {
                    '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}' => px / 2,
                    _ => px * 6 / 10,
                };
                assert!(
                    w as i32 >= px * 6 / 10 && h as i32 >= min_h,
                    "{ch} @{px} 包围盒 {w}x{h} 太小（下限 {min_h}）⇒ 轮廓单位或缩放错了"
                );
                assert!(inked > px as usize, "{ch} @{px} 只有 {inked} 个像素有墨");
                assert!(inked < w * h, "{ch} @{px} 整块实心 ⇒ 抗锯齿没跑");
                // ② **可读性的下限长什么样**：最密的像素到不了实心，就意味着整行字都是半透明的
                //    灰。门的形状：量"最密那个像素够不够接近满"，**不**要求真有 255 —— 24 物理 px
                //    的「盲」实测峰值 254，那 1/255 的差没人看得出来，而"有没有笔画核"看得见。
                //    这一条在 16 px 档上是**故意**让它成立的（见下面的表），所以只在本机那一档硬判。
                rows.push(format!(
                    "  @{px:>2} px  {ch}  盒 {w:>2}x{h:<2} 有墨 {inked:>3} 峰值 {peak:>3} ≥200 {dense:>3} 全实心 {solid:>3}"
                ));
                if px == physical_px(1.5) {
                    assert!(
                        peak >= 240 && dense > 0,
                        "{ch} 在**本机 1.5× 那一档**（{px} px）上没有实心笔画核：峰值 {peak}、\
                         ≥200 的只有 {dense} 个 ⇒ 中文淡成一团灰，§7-R6 成立"
                    );
                }
            }
        }
        println!(
            "CFF spike 覆盖率实测（逻辑 {} px × scale 1/1.5/2 ⇒ 物理 {:?}）：\n{}",
            LOGICAL_PX,
            TIERS,
            rows.join("\n")
        );
    }

    /// arena 是死的，而一次会话只用一档（`pick_output()` 取一台显示器、scale 进程内恒定）。
    /// 所以这一档**清单里全部码位**必须装得下，否则那几个字每 tick 都要现栅格化 —— 那是"每 tick
    /// 重绘换好看"的另一种形态。这里量的是逐字形的 `w×h` 字节数之和，不是"应该差不多"。
    #[test]
    fn one_tier_of_every_glyph_fits_the_arena() {
        let f = face().unwrap();
        let cps = codes_list();
        assert!(cps.len() > 200, "清单解析出 {} 个 ⇒ 解析器坏了", cps.len());
        let mut worst = 0usize;
        let mut per_tier = Vec::new();
        for px in TIERS {
            let bytes: usize = cps
                .iter()
                .filter_map(|&cp| char::from_u32(cp))
                .filter_map(|c| rasterize(f, px, c).map(|(sp, _)| sp.w as usize * sp.h as usize))
                .sum();
            per_tier.push(format!("{px} px = {bytes} B"));
            worst = worst.max(bytes);
        }
        assert!(
            worst <= CACHE_MAX_BYTES,
            "单档全量字形要 {worst} B，arena 只有 {CACHE_MAX_BYTES} B ⇒ 要么加 arena 要么每 tick 现画：{per_tier:?}"
        );
        println!(
            "单档全量字形（{} 个码位）：{} · arena {CACHE_MAX_BYTES} B",
            cps.len(),
            per_tier.join(" · ")
        );
    }

    /// 清单说收了的字，**资产里必须真有**。fontTools 静默丢一个字（表面对、 cmap 空）时，
    /// `tests/font_assets.rs` 那两条"清单 ⇔ 会渲染"的相等门禁照样绿 —— 它们比的是清单，
    /// 不是字体。这一条补的是第三条腿：清单 ⊆ 真 cmap。三类文字各抽 5 个是 §4.2 的要求，
    /// 但既然整个清单都能查，就没必要只抽 15 个。
    #[test]
    fn every_listed_codepoint_has_a_glyph_in_the_asset() {
        let mut miss = Vec::new();
        let mut by_script = [0usize; 3]; // 汉字 / 假名 / 谚文
        for cp in codes_list() {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            if !has_glyph(c) {
                miss.push(format!("U+{cp:04X} {c}"));
            }
            if matches!(cp, 0x3040..=0x30ff) {
                by_script[1] += 1;
            } else if matches!(cp, 0xac00..=0xd7af) {
                by_script[2] += 1;
            } else if matches!(cp, 0x3400..=0x9fff | 0xf900..=0xfaff) {
                by_script[0] += 1;
            }
        }
        assert!(
            miss.is_empty(),
            "清单里 {} 个字在字体 cmap 里查不到：{} ⇒ 重跑 tools/subset_font.py --verify",
            miss.len(),
            miss.join("、")
        );
        for (name, n) in [
            ("汉字", by_script[0]),
            ("假名", by_script[1]),
            ("谚文", by_script[2]),
        ] {
            assert!(n >= 5, "{name} 只解出 {n} 个码位 ⇒ i18n 的字形大概没进子集");
        }
        println!(
            "三类字形齐：汉字 {} / 假名 {} / 谚文 {}",
            by_script[0], by_script[1], by_script[2]
        );
    }

    /// CI 不许走兜底（§7-R5）：`face()` 一旦 `None`，排版口径就换成了 `font.rs` 的 5×7，
    /// 同一条截断会截在不同的字上。这条与"降级不炸"是相反方向的钉。
    #[test]
    fn face_is_available() {
        assert!(
            face().is_some(),
            "字体子集解不出来 ⇒ 不许静默降级，见 §7-R5"
        );
        assert_eq!(
            FONT_BYTES.len(),
            35_316,
            "入库产物的大小变了 ⇒ 连同 tests/font_assets.rs 的摘要一起改了吗"
        );
    }

    /// §7-R3：预乘漏了会长出黑描边，而"画了多少像素"抓不住它。这里直接量边缘像素。
    #[test]
    fn edges_are_not_a_black_halo() {
        let px = TIERS[1];
        let w = 40;
        let h = 40;
        let mut canvas = vec![0u8; (w as usize) * (h as usize) * 4];
        // 中灰不透明底：预乘下 BGR = 各半。
        for p in canvas.chunks_exact_mut(4) {
            p.copy_from_slice(&[128, 128, 128, 255]);
        }
        // 白字。边缘像素如果"没被预乘"就会比底色更**暗**（把背景当不透明、又只乘了小 a）。
        draw(&mut canvas, (w as usize) * 4, 2, 30, px, 0xFF_FFFF, "蓝");
        let dst = |x: i32, y: i32| -> [u8; 3] {
            let b = (y as usize * w as usize + x as usize) * 4;
            [canvas[b], canvas[b + 1], canvas[b + 2]]
        };
        let mut edges = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c = dst(x, y);
                if c != [128, 128, 128] {
                    edges.push(c);
                }
            }
        }
        assert!(edges.len() > 20, "一个字都没画上去");
        // 白字叠中灰：任何一个被改过的像素都必须 ≥ 底色（source-over 只会往亮的方向走）。
        for c in &edges {
            assert!(
                c[0] >= 128 && c[1] >= 128 && c[2] >= 128,
                "边缘像素 {c:?} 比底色暗 ⇒ 这不是黑描边是什么（预乘漏了）"
            );
        }
        // 而且真的存在中间值（抗锯齿的灰阶），不是只有 0 与 255 两档。
        assert!(
            edges.iter().any(|c| (129..255).contains(&c[0])),
            "边缘没有任何灰阶 ⇒ 抗锯齿没跑"
        );
    }

    /// 画布外的像素不许写（越界就是踩别人内存，而 `stride` 由调用方给）。
    #[test]
    fn drawing_is_clipped_to_the_canvas() {
        let px = TIERS[0];
        let mut canvas = vec![0u8; 8 * 8 * 4];
        // 故意把基线放在画布外（负 y、以及超出右边界）。
        draw(&mut canvas, 8 * 4, -100, -100, px, 0x00_FF_00, "蓝帽 12");
        draw(&mut canvas, 8 * 4, 6, 4, px, 0x00_FF_00, "蓝帽 12");
        assert!(
            canvas.iter().any(|&b| b != 0),
            "越界时应当至少把落在屏内的那部分画上，而不是整串丢掉"
        );
    }

    /// `measure` 与 `draw` 的笔位必须是同一个数 —— 截断（`fit`）按 measure 判，画按 draw 走，
    /// 差 1 px 就会"说不超宽、画上却超宽"。
    #[test]
    fn measure_and_draw_agree_on_the_pen() {
        for px in TIERS {
            for s in ["盲盒", "SSR 蓝帽 新", "余额 1,240", "Language", "조각 +120"] {
                let mut canvas = vec![0u8; 600 * 4];
                let end = draw(&mut canvas, 600 * 4, 0, px + 2, px, 0xFF_FFFF, s);
                assert_eq!(end, measure(s, px), "「{s}」@{px}：笔位与量宽不一致");
            }
        }
    }

    #[test]
    fn physical_px_clamps_to_the_three_tiers() {
        assert_eq!(physical_px(1.0), LOGICAL_PX);
        assert_eq!(physical_px(1.0), 16);
        assert_eq!(physical_px(1.5), 24);
        assert_eq!(physical_px(2.0), 32);
        // 越界与怪值：全部夹到最近的档，绝不返回没准备的字号。
        assert_eq!(physical_px(0.5), 16);
        assert_eq!(physical_px(4.0), 32);
        assert_eq!(physical_px(0.0), 16);
        assert_eq!(physical_px(f32::NAN), 16);
        // 三档互不相同且都 >0：缓存键就是 (档, char)，两档撞上就会互相拿错位图。
        assert!(TIERS.windows(2).all(|w| w[0] < w[1]), "{TIERS:?}");
    }

    /// 行距由字体给，不是手写的常数（§4.5：`LINE_STEP` 改成吃这个数）。
    #[test]
    fn line_height_is_metric_driven_and_positive() {
        for px in TIERS {
            let lh = line_height(px);
            assert!(lh >= px, "{px} px 的行高 {lh} 连 em 都不够");
            assert!(
                lh <= px * 3 / 2 + 2,
                "{px} px 的行高 {lh} 太肥 ⇒ 卡片装不下"
            );
            assert!(
                ascent(px) > 0,
                "ascent 应当是正数：{px} px 读到 {}",
                ascent(px)
            );
            assert!(
                descent(px) <= 0,
                "descent 与 ab_glyph 同口径（负数）：{px} px 读到 {}",
                descent(px)
            );
        }
        println!(
            "度量：{} px → ascent {} / descent {} / line_height {}",
            TIERS
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            TIERS
                .iter()
                .map(|p| ascent(*p).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            TIERS
                .iter()
                .map(|p| descent(*p).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            TIERS
                .iter()
                .map(|p| line_height(*p).to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    /// arena 是"先占死"的，所以这里要能证明它**不会**因为画了很多字而越界。
    ///
    /// 这里在**一个进程里连画三档**（真会话只会有一档），三档之和 137 KB > arena 96 KB ⇒
    /// `uncached` 会非零 —— 那是要的：溢出路径的行为必须是"现栅格化、照样画出来、只是不进缓存"，
    /// 而不是丢字、也不是悄悄把 arena 长出去。单档装得下由
    /// [`one_tier_of_every_glyph_fits_the_arena`] 证。
    #[test]
    fn cache_never_exceeds_the_arena() {
        // 把四 locale 全部 UI 串 + 目录 30 个中文名，在三档上各画一遍（≈ 最坏情况）。
        let mut canvas = vec![0u8; 240 * 3 * 4];
        let mut texts: Vec<String> = Vec::new();
        for l in Lang::ALL {
            texts.extend(l.ui_strings());
        }
        for item in &Catalog::default_v1().unwrap().items {
            texts.push(item.name.clone());
        }
        for px in TIERS {
            for t in &texts {
                draw(&mut canvas, 240 * 4, 0, px, px, 0xFF_FFFF, t);
            }
        }
        assert!(
            cache_bytes() <= CACHE_MAX_BYTES,
            "缓存 {} B 超了 arena {CACHE_MAX_BYTES} B",
            cache_bytes()
        );
        let (hits, made, uncached) = cache_stats();
        // 这条是"计数器为 0 不等于代码对"的反面用法：这里**要**它非零，否则整个测试跑的
        // 就只是缓存命中路径，溢出那条路（真会话不会走、但必须正确）一次都没被踩过。
        assert!(
            uncached > 0,
            "连画三档（{made} 个字形）居然没踩到溢出路径 ⇒ arena 变小了还是这条测法失效了"
        );
        println!(
            "缓存：占 {}/{} B，栅格化 {made} 个字形，命中 {hits}，arena 满之后未缓存 {uncached}（上限是死数，超了就现画）",
            cache_bytes(),
            CACHE_MAX_BYTES,
        );
    }

    /// 溢出那条路最容易写错的地方：**把只活在 `scratch` 里的位图记进 `map`**。
    /// 那样下一次命中会读到**别的字**的位图（因为 scratch 早被换掉），而像素计数、缓存占用、
    /// 命中率三个数字全都照样绿。这里用一个 8 字节的假 arena 逼出溢出，再拿参考位图逐字节比。
    #[test]
    fn an_uncached_glyph_is_never_recorded_as_a_cache_hit() {
        let f = face().unwrap();
        let mut c = Cache::new();
        c.arena = vec![0u8; 8]; // 任何汉字都装不下 ⇒ 每个字都走溢出路径
        let blue = c.sprite(f, 32, '蓝').unwrap();
        assert_eq!(blue.off, None, "小 arena 里蓝 不该进 arena");
        let _ = c.sprite(f, 32, '盲').unwrap(); // scratch 现在装的是「盲」
        let again = c.sprite(f, 32, '蓝').unwrap();
        let want = rasterize(f, 32, '蓝').unwrap().1;
        assert_eq!(
            c.bytes(&again),
            want.as_slice(),
            "第二次取「蓝」拿到的是别的字的位图 ⇒ overflow 的字被记进 map 了"
        );
        assert_eq!(
            c.uncached, 3,
            "蓝、盲、蓝 三次都该走溢出路径（一次都不该进 map）"
        );
        assert_eq!(c.used, 0, "arena 一个字节都不该被占用");
        assert!(c.map.is_empty(), "map 必须空 —— 记进去就是上面那条 bug");
    }

    /// §4.2 门禁 ④：**en 比 zh 宽得多**（`Balance` vs `余额`、`Language` vs `语言`），
    /// 所以横向预算要**逐 locale × 逐档 × 最坏形状的数**各量一次：7 位余额带千分位、十连
    /// 明细、四档全非零、目录里最宽的物品名。只验中文那一档会绿得毫无意义。
    ///
    /// 口径：`menu.rs` 的 `CARD_W/PAD` 是**逻辑**像素而 [`measure`] 给**物理**像素 ⇒ 预算同乘
    /// scale（`scale = 1.0` 时就是老的那条 `2*PAD + … ≤ CARD_W` 算术的换字体版本）。
    /// `MAX_LINE_CHARS` 的"数字符数"口径在这里已经不够用了：CJK 每字 ≈1 em、拉丁字母窄得多，
    /// 按字数截必然一边宽一边窄 —— `menu::fit` 要改成逐 char 累加**像素**（同一步里把
    /// `MAX_LINE_CHARS` 换成 `MAX_LINE_PX`），这条先钉住"最坏的一行确实装得下"。
    #[test]
    fn every_locale_fits_the_card() {
        use crate::l10n::{
            Key, header_shards, lang_row, line_balance, line_error, line_item, line_more,
            line_pulled, line_shards, line_tiers,
        };
        use crate::menu::{CARD_W, PAD};
        use petcore::gacha::Rarity;

        let budget_logical = CARD_W - 2 * PAD;
        let catalog = Catalog::default_v1().unwrap();
        let mut report = Vec::new();
        for &scale in &[1.0f32, 1.5, 2.0] {
            let px = physical_px(scale);
            let budget = (budget_logical as f32 * scale).floor() as i32;
            // 物品名不翻译（§7-R12），但**宽度**随 locale 变不了 ⇒ 逐档取最宽的那个。
            let longest = catalog
                .items
                .iter()
                .map(|i| i.name.as_str())
                .max_by_key(|n| measure(n, px))
                .unwrap();
            report.push(format!("  @{px:>2} px（预算 {budget} px）："));
            let mut worst = (0i32, String::new(), Lang::Zh);
            for &l in &Lang::ALL {
                let solo: Vec<String> = vec![
                    l.t(Key::Pull1).into(),
                    l.t(Key::Pull10).into(),
                    l.t(Key::Close).into(),
                    l.t(Key::Burst).into(),
                    lang_row(l),
                    line_pulled(l, 10, Some(Rarity::SSR)),
                    line_item(l, Rarity::SSR, longest, true),
                    line_tiers(&[7, 1, 1, 1]),
                    line_shards(l, 999_999),
                    line_balance(l, 9_999_999),
                    line_more(l, 10),
                    // 错误行的正文会被 `fit` 截（同一行还有语言行），前缀必须自己站得住。
                    line_error(l, ""),
                ];
                // 同一行挤两段的最坏情况：标题行 = 标题 + `碎片 余额 演示`，底行 = 碎片 + 余额。
                // 中间只留一个空格的缝 —— 这是**下界**，真排版留得比它宽，所以这条只会更严。
                let joined: Vec<String> = vec![
                    format!("{} {}", l.t(Key::Title), header_shards(l, 9_999_999, true)),
                    format!("{} {}", l.t(Key::Title), header_shards(l, 9_999_999, false)),
                    format!("{} {}", line_shards(l, 999_999), line_balance(l, 9_999_999)),
                ];
                let mut local = (0i32, String::new());
                for s in solo.iter().chain(joined.iter()) {
                    let w = measure(s, px);
                    if w > local.0 {
                        local = (w, s.to_string());
                    }
                    if w > worst.0 {
                        worst = (w, s.to_string(), l);
                    }
                }
                report.push(format!("       {l:?} {:>3} px  「{}」", local.0, local.1));
            }
            assert!(
                worst.0 <= budget,
                "{:?} 档（{px} px）最宽行「{}」= {} px，超内容区 {budget} 逻辑 px ⇒ \
                 要么字号太大，要么这句本来就不该整行上屏（该走 `fit` 截断）：\n{}",
                worst.2,
                worst.1,
                worst.0,
                report.join("\n")
            );
        }
        println!(
            "横向预算实测（内容区 {budget_logical} 逻辑 px）：\n{}",
            report.join("\n")
        );
    }

    #[test]
    fn missing_reports_only_the_first_uncovered_char() {
        assert_eq!(missing("余额 1,240"), None);
        assert_eq!(missing("蓝帽"), None);
        // 用一个明确没收进子集的码位（CJK 扩展 A 段的第一字，清单里没有）。
        assert_eq!(missing("㐀"), '㐀'.into());
        assert_eq!(
            missing("帽㐀蓝"),
            '㐀'.into(),
            "应当报第一个缺字，而不是最后一个"
        );
        // 空白不算缺字（`"余额 1,240"` 的空格在 cmap 里也可能落在 .notdef）。
        assert_eq!(missing("a b"), None);
    }
}
