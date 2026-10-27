//! 菜单卡片的**像素层**：吃一块 `wl_shm` 的 Argb8888 画布（小端 = B,G,R,A，需预乘），
//! 画出三种相位：菜单 / 稀有度扩散 / 结果卡。
//!
//! 这里**不出现任何 wayland 类型**（和 `menu.rs` 同族约束）：只吃 `(canvas, 尺寸, 缩放)`。
//! 好处不是"整洁"，而是**能在 headless 里出图** —— `OPET_CARD_DUMP=1 cargo test -p
//! overlay-wayland -- card::tests::dump_each_phase` 会把每个相位写成 `/tmp/` 下的 PPM。
//! 像素级的数字门禁看不见"少了一块"（M2 的"整张脸不存在"是被 14 项全绿放过去的），
//! 所以这一步的验收必须包含目视。
//!
//! 坐标口径：[`menu`] 的常量全是 **logical**，画布是 **物理**像素，所以本模块唯一的换算是
//! `×scale`。字号同样按 logical 定（[`crate::text::LOGICAL_PX`] = 16 逻辑像素 ⇒ 物理档由
//! [`crate::text::physical_px`] 给出，1× = 16、1.5× = 24、2× = 32），于是"这行字装不装得下"
//! 这类预算只需要证一次。⚠️ 换算**只发生在 [`px`] 与 [`draw_in`] 两处**：字宽/字高走
//! [`line_w`]/[`line_h`]，断言侧走 `Shot::w_log`/`h_log`，谁再自己乘一遍 scale 就会出现
//! "画的位置和命中的位置差 1 px"那一类只有真机看得见的 bug。

use petcore::gacha::Rarity;

use crate::font;
use crate::l10n::{self, Key, Lang};
use crate::menu::{self, CARD_H, CARD_W, CORNER, FOOTER_GAP, MenuAnim, PAD, RectI, TEXT_PX};
use crate::settings::Motion;
use crate::text;

/// 卡片底板色（§4.5 的 `#1B1D22`；旧的 `#1B1E23` 差一档蓝，新版按方案取齐）。
const BG: u32 = 0x1B_1D_22;
/// 卡片描边（§4.5 的 `#2E323A`）。**非文字** ⇒ 不参与 4.5:1 那条门禁（它比底板亮就行）。
const EDGE: u32 = 0x2E_32_3A;
/// 行底（菜单项与结果卡明细共用，§4.5 的 `#22252B`）与行左端的强调条
const ROW: u32 = 0x22_25_2B;
const ACCENT: u32 = 0x4A_52_5C;
/// 正文与次要文字
const FG: u32 = 0xE6_E9_EE;
const FG_DIM: u32 = 0x9A_A0_A6;
/// **深墨**：压在亮块（档位色带、burst 牌）上的那一支。
///
/// ⚠️ 这里刻意**不**复用 `BG`，是本轮实测逼出来的：`BG` 是 `#1B1D22`，它对 SR 的
/// `#A855F7` 只有 **4.26:1**（差 0.24 就"看着还行但门禁红"），而近黑 `#0A0C0F` 对四档
/// 全部 ≥ 4.95 ⇒ 同一支墨既要能压住亮金、又要能压住紫。见 `ink_on` 与
/// `every_paintable_block_has_a_legible_ink`。
const INK: u32 = 0x0A_0C_0F;
/// DRYRUN 标：用 SSR 的金，一眼和真档区分开（内存库不写盘，这条必须显眼）
const WARN: u32 = 0xF5_B3_01;
/// 错误行（库打不开、存档写不进去都靠它出声，不 panic 也不退出）。
///
/// ⚠️ 从 `#E5484D` **提亮**到 `#FF7B80`：旧值在新底板上只有 **4.31:1**，而这一行是唯一
/// 要让人"当场知道出事了"的红字 ⇒ 4.5:1 门禁直接把它判红。错误正文本身仍是英文
/// （`petstore` 的 `Display`，本轮不译，§7-R9），全文同时进 stderr。
const ERR_FG: u32 = 0xFF_7B_80;
/// SSR 白闪：判据②要求的"亮度上限 0.85"⇒ `0xD8` = 216/255 = 0.847。
///
/// 原来这里是纯白 `#FFFFFF`：整卡过白对前庭敏感的人是最坏的一种闪烁（面积最大、亮度最高），
/// 而"闪一下"要传达的只是"这抽不一般"，不需要把亮度推到上限。
/// `flash_is_capped_at_85_percent_brightness` 钉的是这个数，不是注释。
const FLASH: u32 = 0xD8_D8_D8;

/// 0xRRGGBB ⇒ 画布上的 4 字节。`wl_shm::Format::Argb8888` 在小端机器上内存序是 B,G,R,A，
/// 且要求**预乘** alpha；这里所有颜色 a=255 ⇒ 预乘不变，所以 M4 那条"直通 alpha 塞进来
/// 会出黑描边"的坑在 UI 侧不存在（卡片没有半透明像素，透明只发生在卡片之外）。
pub const fn bgra(rgb: u32) -> [u8; 4] {
    let (r, g, b) = ((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
    [b as u8, g as u8, r as u8, 255]
}

/// logical → 物理（四舍五入，和 `app::to_phys_of` 同口径）。
fn px(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

/// 物理矩形 `(x, y, w, h)` —— 本模块内部**唯一**的几何形态（逻辑矩形一律经 [`phys`] 进来）。
type Phys = (i32, i32, i32, i32);

/// logical 矩形 → **物理** `(x, y, w, h)`。
///
/// 两条规矩都为了"画的位置和命中的位置差 1 px"那一类只有真机看得见的 bug：
/// ① 右/下边取 `px(x + w)` 一次舍入，**不是** `px(x) + px(w)` 两次 ⇒ 与 [`draw_in`] 的右边、
/// `hit_test` 的命中矩形、`app.rs` 的 input region 落在同一条线；
/// ② 全模块只有这里和 [`draw_in`] 做这道换算，谁再自己乘一遍 scale 就会出现两处几何漂移。
fn phys(r: RectI, scale: f32) -> Phys {
    let (x0, x1) = (px(r.x, scale), px(r.x + r.w, scale));
    let (y0, y1) = (px(r.y, scale), px(r.y + r.h, scale));
    (x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

/// 一个 logical 矩形在这一档上给文字的**物理**预算。
///
/// ⚠️ 这里**不能**用 [`phys`] 的宽：`phys` 的宽是 `logical × scale` 再取整，而 [`menu::fit_to`]
/// 把物理预算折回逻辑域时除的是**字号档**（[`menu`] 的 `tier_ratio` ∈ {1, 1.5, 2}）。1.25× 的屏
/// 夹在 16 px 档（比 = 1.0）上，两个口径一旦混用，同一句话在 1.25× 就比 1×/1.5×/2× **少截一个
/// 字** ⇒ "截点与缩放无关"（提交 5 论证过、由 `report_budget_is_scale_independent` 钉住的那条）
/// 当场失效。所以文字预算一律走 [`menu::budget_for`]，和 `line_budget_px` 同一个函数。
fn budget_of(r: RectI, scale: f32) -> i32 {
    menu::budget_for(r.w, scale)
}

/// 物理字号 = logical 字号 × 缩放，至少 1（算成 0 会把整行文字抹掉 —— 那是"看不见"
/// 而不是"报错"，最难查的一种）。
///
/// ⚠️ 只有**降级路径**（`font.rs` 的 5×7 点阵）用它。真字体走 [`text::physical_px`] 的档位表
/// （16/24/32），字号是 `LOGICAL_PX = 16` 而不是"字模像素 ×2"，两者不同量纲，别混。
fn text_px(scale: f32) -> i32 {
    px(TEXT_PX, scale).max(1)
}

/// 字体资产在不在。整个模块只在这一处分叉 ⇒ "真字体 / 降级点阵"不会各画一半。
///
/// 留降级路径的理由不是整洁，是硬约束 3「宠物崩溃不能影响桌面」：资产出事只能不画字，
/// 不能炸进程。CI 那边 `text.rs::face_is_available` 与
/// `text.rs::every_listed_codepoint_has_a_glyph_in_the_asset` 强制它成功，
/// 否则会出现"CI 走兜底、真机走真字体"两套排版（§7-R5）。
fn font_ready() -> bool {
    text::face().is_some()
}

/// 一行的**物理**字号。
fn face_px(scale: f32) -> i32 {
    if font_ready() {
        text::physical_px(scale)
    } else {
        text_px(scale)
    }
}

/// 一行字有多宽（**物理**像素）。右对齐/居中用它 ⇒ 与 [`menu::line_width`] 完全同口径
/// （两处都是逐 char advance 相加），所以"截在哪里"和"画到哪里"不可能差 1 px。
fn line_w(s: &str, scale: f32) -> i32 {
    if font_ready() {
        text::measure(s, text::physical_px(scale))
    } else {
        font::text_width(s, text_px(scale))
    }
}

/// 一行字占多高（**物理**像素）：字体度量给的行高，或点阵的 7 × 字号。
fn line_h(scale: f32) -> i32 {
    if font_ready() {
        text::line_height(text::physical_px(scale))
    } else {
        font::GLYPH_H * text_px(scale)
    }
}

/// 画一行字。`x`/`top` 是**物理**像素的左上角（不是基线 —— 基线由字体度量在这里算）。
///
/// `top + ascent` 就是基线：这一行**上沿贴 top**，所以调用方只需要给矩形，不需要知道
/// 字体度量的存在。降级点阵本来就从 top 画起，两者观感一致。
fn draw_line(canvas: &mut [u8], stride: usize, x: i32, top: i32, scale: f32, rgb: u32, s: &str) {
    if font_ready() {
        let px = face_px(scale);
        text::draw(canvas, stride, x, top + text::ascent(px), px, rgb, s);
    } else {
        font::draw_str(canvas, stride, x, top, text_px(scale), rgb, s);
    }
}

/// 在 logical 矩形 `r` 里**垂直居中**画一行字，水平位置由 `align` 决定。
/// 抽出来是因为"行带 + 底部语言行"两类共五处都靠它，而每处自己算 `(h - line_h)/2`
/// 迟早会有一处忘了换口径。
enum Align {
    Left,
    /// 贴矩形的**右内边**，不是卡片右边：底部那一行左边还压着错误行。
    Right,
}

fn draw_in(
    canvas: &mut [u8],
    stride: usize,
    r: RectI,
    scale: f32,
    rgb: u32,
    s: &str,
    align: Align,
) {
    let (x, top) = pen_in(r, s, scale, align);
    draw_line(canvas, stride, x, top, scale, rgb, s);
}

/// [`draw_in`] 的**落笔点**（物理像素，字框左上）。单独成函数只为了一个理由：字和它底下的
/// 底板必须是**同一个算式**的两个调用 —— 各算一份就会出现"底板偏 1 px、字压出边"，
/// 而那只有出图过眼看得见（判据①）。
fn pen_in(r: RectI, s: &str, scale: f32, align: Align) -> (i32, i32) {
    let (x0, y0, h) = (px(r.x, scale), px(r.y, scale), px(r.h, scale));
    let top = y0 + ((h - line_h(scale)) / 2).max(0);
    // 右边界用 `px(x + w)`（一次舍入）而不是 `px(x) + px(w)`（两次）⇒ 与 `menu::footer()`
    // 的矩形、`hit_test` 的命中区同一条边线，差 1 px 就会出现"点得到却看着没贴齐"。
    let right = px(r.x + r.w, scale);
    let x = match align {
        Align::Left => x0,
        Align::Right => (right - line_w(s, scale)).max(x0),
    };
    (x, top)
}

/// 画一行字，**先在它底下垫一块 [`BG`] 底板**（只盖住字自己的框 + 一圈气）。
///
/// 为什么 footer 那两条需要而别处不需要：它们压着的底色**不是恒定的** —— `Burst` 的扩散块
/// 会长到盖住卡片下半部（出图实测：N 档 `elapsed=2` 时"余"字整字埋进浅灰块里，而
/// `FG_DIM` 压 N 灰只有 1.4:1 = 读不出）。`ink_on` 在这里**结构上救不了**：同一个字一半在块内
/// 一半在块外 ⇒ 任何单一墨色都必错一半，正解是让"字底下的颜色"重新变成已知量（M3-b 给
/// burst 牌垫一块 `spec.color` 底板是同一条先例，那次也是出图才发现）。
///
/// 菜单 / 结果卡两态下这块板是 `BG` 压 `BG` ⇒ 逐像素不可见，只有 burst 态真的看得见它。
fn draw_in_plated(
    canvas: &mut [u8],
    stride: usize,
    r: RectI,
    scale: f32,
    rgb: u32,
    s: &str,
    align: Align,
) {
    let (x, top) = pen_in(r, s, scale, align);
    let (w, h) = (line_w(s, scale), line_h(scale));
    let pad = (h / 6).max(1);
    fill(
        canvas,
        stride,
        x - pad,
        top - pad,
        w + 2 * pad,
        h + 2 * pad,
        bgra(BG),
    );
    draw_line(canvas, stride, x, top, scale, rgb, s);
}

/// sRGB 逐通道**线性化**后的相对亮度权重（×10³ 定标）。
///
/// 为什么用查表而不是现算 `powf`：WCAG 的 `((c+0.055)/1.055)^2.4` 只有 **256 个可能的输入**，
/// 所以"整数对比度"不需要近似 —— 把参考公式离线算好钉进表，运行时全整数、跨平台逐位一致
/// （`contrast_x1000` 的判据是"门禁阈值"，一个依赖 libm 版本的实现不该当判据）。
/// 表本身由 `the_linearization_table_matches_the_reference_formula` 对着 `f64` 原式逐个核。
const LINEAR_X1000: [u32; 256] = [
    0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, // c   0.. 15（c/12.92 线性段）
    5, 6, 6, 7, 7, 7, 8, 9, 9, 10, 10, 11, 12, 12, 13, 14, // c  16.. 31
    14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, // c  32.. 47
    30, 31, 32, 33, 34, 36, 37, 38, 40, 41, 42, 44, 45, 47, 48, 50, // c  48.. 63
    51, 53, 54, 56, 58, 60, 61, 63, 65, 67, 68, 70, 72, 74, 76, 78, // c  64.. 79
    80, 82, 84, 87, 89, 91, 93, 95, 98, 100, 102, 105, 107, 109, 112, 114, // c  80.. 95
    117, 120, 122, 125, 127, 130, 133, 136, 138, 141, 144, 147, 150, 153, 156,
    159, // c  96..111
    162, 165, 168, 171, 175, 178, 181, 184, 188, 191, 195, 198, 202, 205, 209,
    212, // c 112..127
    216, 220, 223, 227, 231, 235, 238, 242, 246, 250, 254, 258, 262, 266, 270,
    275, // c 128..143
    279, 283, 287, 292, 296, 301, 305, 309, 314, 319, 323, 328, 332, 337, 342,
    347, // c 144..159
    352, 356, 361, 366, 371, 376, 381, 386, 392, 397, 402, 407, 413, 418, 423,
    429, // c 160..175
    434, 440, 445, 451, 456, 462, 468, 474, 479, 485, 491, 497, 503, 509, 515,
    521, // c 176..191
    527, 533, 539, 546, 552, 558, 565, 571, 578, 584, 591, 597, 604, 610, 617,
    624, // c 192..207
    631, 638, 644, 651, 658, 665, 672, 680, 687, 694, 701, 708, 716, 723, 730,
    738, // c 208..223
    745, 753, 761, 768, 776, 784, 791, 799, 807, 815, 823, 831, 839, 847, 855,
    863, // c 224..239
    871, 880, 888, 896, 905, 913, 922, 930, 939, 947, 956, 965, 973, 982, 991,
    1000, // c 240..255
];

/// WCAG 相对亮度 × 10⁷（0..=10_000_000）。`L = 0.2126R + 0.7152G + 0.0722B`，通道先线性化。
///
/// 定标是 **10⁷** 而不是 10⁶，因为它是两级的乘积：权重 `2126/7152/722` 已经是 ×10⁴，
/// 而 [`LINEAR_X1000`] 又 ×10³。白 = `2126+7152+722 = 10000` 个 `LINEAR_X1000[255] = 1000`
/// ⇒ 10_000_000。**这条不是注释里的洁癖**：`contrast_x1e3` 的 `+0.05` 偏移必须跟着同一把尺子，
/// 写成 `50_000`（= 10⁶ 口径的 0.05）会把所有比值放大近一倍 ⇒ 4.5:1 的门禁会放过不合格的对
/// （黑白会报 201‰ 每单位 = 21:1，而错口径给出的是 210 = 21:1×10）。
fn luma_x1e7(rgb: u32) -> u64 {
    let (r, g, b) = (
        LINEAR_X1000[((rgb >> 16) & 0xFF) as usize] as u64,
        LINEAR_X1000[((rgb >> 8) & 0xFF) as usize] as u64,
        LINEAR_X1000[(rgb & 0xFF) as usize] as u64,
    );
    2126 * r + 7152 * g + 722 * b
}

/// 两个颜色的对比度 × 1000（整数，向下取整 ⇒ 只会低估，不会虚高过线）。
///
/// 口径 = WCAG 2.1：`(L_max + 0.05) / (L_min + 0.05)`，正文阈值 **4500**（4.5:1）。
/// `500_000` 就是那个 `+0.05` 在 ×10⁷ 定标下的样子（见 [`luma_x1e7`]）。定标取 ×1000 而不是
/// 浮点比较，是为了"逐档 × 逐 locale × 逐底色"那条遍历能写成 `>=` 整数式。
fn contrast_x1000(a: u32, b: u32) -> u32 {
    let (la, lb) = (luma_x1e7(a), luma_x1e7(b));
    let (hi, lo) = (la.max(lb), la.min(lb));
    (((hi + 500_000) * 1000) / (lo + 500_000)) as u32
}

/// 正文对比度下限（×1000）。§4.5 的"机器防线"就是这个数。
const CONTRAST_MIN: u32 = 4500;

/// 压在某块底色上的文字该用什么墨色。
///
/// **规则的形状没变**（上一轮那条"白字压金底读不出"的教训：墨色必须由**底色**算出来，
/// 不许是常量），变的是**判据**：从"Rec.601 加权亮度大于 140"换成"两支候选里取对比度更高的
/// 一支"，而候选从 `BG`/`FG` 换成 `INK`/`FG`。三处都是实测逼的，不是口味：
///
/// | 底色 | 旧规则给的墨 | 实际对比度 | 新规则给的墨 | 实际对比度 |
/// |---|---|---|---|---|
/// | R `#3B82F6` | FG（Rec.601 = 121 < 140） | **3.02** ✗ | INK | 5.32 ✓ |
/// | SR `#A855F7` | FG（128 < 140） | **3.25** ✗ | INK | 4.95 ✓ |
/// | SR `#A855F7` | 若改用 `BG` 当深墨 | — | 4.26 ✗ | INK ⇒ 4.95 ✓ |
/// | SSR `#F5B301` | BG（178 > 140） | 9.10 ✓ | INK | 10.57 ✓ |
///
/// ⇒ 旧的"亮度 > 140 出深墨"在 R/SR 上**判错了方向**：Rec.601 是对 **gamma 编码值**加权，
/// 而 WCAG 先线性化 ⇒ 一个"看着挺亮"的蓝紫在 WCAG 眼里更亮，该用深墨。这正是"数字全绿但
/// 画面读不出"那一族的第二个形状（第一个是"有墨但同色"）。判据遍历见
/// `every_paintable_block_has_a_legible_ink`。
fn ink_on(rgb: u32) -> u32 {
    let ink = if contrast_x1000(INK, rgb) >= contrast_x1000(FG, rgb) {
        INK
    } else {
        FG
    };
    // 与 `every_paintable_block_has_a_legible_ink` 同一把尺子，但立在**调用点**：那条测试遍历的是
    // 调色板，这一条抓的是"某个新底色只在这里被用上、调色板遍历没覆盖到"。release 构建里
    // `debug_assert` 编掉（硬约束 3：客户端不许因为排版常量而炸），所以它只是开发期的第二道门。
    debug_assert!(
        contrast_x1000(ink, rgb) >= CONTRAST_MIN,
        "底色 #{rgb:06X} 上最好的墨只有 {}‰ < {CONTRAST_MIN}‰ ⇒ 这行字画得出、读不出",
        contrast_x1000(ink, rgb)
    );
    ink
}

/// 填一个**物理**矩形（半开区间，越界逐像素裁剪，绝不 panic）。
fn fill(canvas: &mut [u8], stride: usize, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
    if w <= 0 || h <= 0 || stride == 0 {
        return;
    }
    let rows = canvas.len() / stride;
    for yy in y..(y + h) {
        if yy < 0 || yy >= rows as i32 {
            continue;
        }
        let base = (yy as usize) * stride;
        for xx in x..(x + w) {
            let i = base + (xx as usize) * 4;
            if let Some(slot) = canvas.get_mut(i..i + 4) {
                slot.copy_from_slice(&color);
            }
        }
    }
}

/// 半径 `r` 的圆角框里，第 `q` 列 / 第 `p` 行那个像素（都从最外排数起）在弧线**内**吗。
///
/// 判据是"像素中心 `(q+0.5, p+0.5)` 到圆心 `(r, r)` 的距离 ≤ r"，两边乘 4 写成纯整数
/// （`a² + b² ≤ (2r)²`，`a = 2(r-q)-1`）⇒ 没有浮点、没有近似，同一份代码在任何档上都出
/// 同一个剪影。`r = 1` 时**一个像素都不削**（那是几何，不是漏实现）：半径 1 的弧整块落在
/// 首像素中心之内，所以 `CORNER = 3` 在 1× 上其实只削掉一个角点 —— 这也是为什么出图要按
/// **1.5× 真机口径**看（判据①），1× 的圆角几乎看不出来。
const fn corner_inside(q: i32, p: i32, r: i32) -> bool {
    let a = 2 * (r - q) - 1;
    let b = 2 * (r - p) - 1;
    a * a + b * b <= 4 * r * r
}

/// 第 `j` 行（从最外排数起，要求 `j < r`）左右各要缩进多少像素。
fn corner_inset(j: i32, r: i32) -> i32 {
    (0..r).find(|&i| corner_inside(i, j, r)).unwrap_or_default()
}

/// 半径收进矩形里（圆角不能超过短边的一半，否则圆心跑到矩形外、剪影变成沙漏）。
const fn clamp_radius(r: i32, w: i32, h: i32) -> i32 {
    let cap = if w < h { w / 2 } else { h / 2 };
    if r > cap {
        cap
    } else if r < 0 {
        0
    } else {
        r
    }
}

/// 填一个**物理**圆角矩形。成本是 `O(h + 4r²)`（逐行一次 `fill` + 角框里逐像素），
/// 不是 `O(w·h)` 的逐像素判定 —— 一轮 `paint` 要画十几块，判据④的 CPU 门禁在这儿也生效。
///
/// 矩形按 [`Phys`] 一整块传：`(x, y, w, h)` 摊成四个参数时这个函数是 9 个入参，clippy 的
/// `too_many_arguments` 会响（门禁要求零 warning），而"四个数其实是一个矩形"正是这里的语义。
fn fill_round(canvas: &mut [u8], stride: usize, rect: Phys, radius: i32, color: [u8; 4]) {
    let (x, y, w, h) = rect;
    let r = clamp_radius(radius, w, h);
    for j in 0..h.max(0) {
        let inset = if j < r {
            corner_inset(j, r)
        } else if j >= h - r {
            corner_inset(h - 1 - j, r)
        } else {
            0
        };
        fill(
            canvas,
            stride,
            x + inset,
            y + j,
            (w - 2 * inset).max(0),
            1,
            color,
        );
    }
}

/// 像素 `(i, j)`（相对矩形左上角）落在半径 `r` 的圆角矩形**内**吗。
///
/// 两条边界情形都得管住：`r = 0`（没圆角 ⇒ 整块都在内）、以及"这一格不在任何角框里"
/// （`q ≥ r` 或 `p ≥ r` ⇒ 直线段上的像素，一律在内）。后者漏了就会把描边描成一段一段的
/// —— 因为 [`corner_inside`] 的 `r - q` 在 `q > r` 时变成负数，平方后反而"超出半径"。
fn in_round(i: i32, j: i32, w: i32, h: i32, r: i32) -> bool {
    let (q, p) = (i.min(w - 1 - i), j.min(h - 1 - j));
    q >= r || p >= r || corner_inside(q, p, r)
}

/// 描一个**物理**圆角矩形的框（只遍历靠边 `t` 的那一圈，不遍历内部）。
fn stroke_round(canvas: &mut [u8], stride: usize, rect: Phys, radius: i32, t: i32, color: [u8; 4]) {
    let (x, y, w, h) = rect;
    let r = clamp_radius(radius, w, h);
    let t = t.min(w / 2).min(h / 2).max(1);
    for j in 0..h {
        for i in 0..w {
            // 只有"离某条边 < t"的像素属于框；圆角外的像素交给 [`clip_to_plate`] 清掉。
            let near_edge = i < t || i >= w - t || j < t || j >= h - t;
            if near_edge && in_round(i, j, w, h, r) {
                fill(canvas, stride, x + i, y + j, 1, 1, color);
            }
        }
    }
}

/// 把底板圆角**之外**的像素清成全透明 —— 每个相位的最后一道。
///
/// 为什么最后统一清、而不是让每一块各自圆角：`Burst` 的扩散块与 SSR 的过白是**整卡方块**
/// 填充，它们会把圆角盖成直角 ⇒ "这一帧卡片是方的"只有出图过眼才看得出来，而像素计数只会
/// 报告"这四格画了东西"。剪影必须与相位无关（判据④：静止/动画的差别只能在内容上）。
fn clip_to_plate(canvas: &mut [u8], bw: i32, bh: i32, stride: usize, radius: i32) {
    let r = clamp_radius(radius, bw, bh);
    if r <= 1 {
        return; // 半径 1 削不掉任何东西（见 [`corner_inside`]），省一趟
    }
    let clear = [0u8; 4];
    for j in 0..r {
        let inset = corner_inset(j, r);
        for i in 0..inset {
            for (xx, yy) in [
                (i, j),
                (bw - 1 - i, j),
                (i, bh - 1 - j),
                (bw - 1 - i, bh - 1 - j),
            ] {
                fill(canvas, stride, xx, yy, 1, 1, clear);
            }
        }
    }
}

/// 一帧卡片上"与相位无关"的信息。打包成一处的理由不是好看：`paint` 和三个相位函数本来
/// 要把这四个值逐个往下透传，参数表已经踩到 clippy 的门禁，而抽取层只会往里加字段。
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub balance: i64,
    pub dry_run: bool,
    pub error: &'a str,
    pub scale: f32,
    /// 动画强度档。**每进程恒定**（同 `dry_run`）⇒ 不进 `menu::content_sig`，但画 burst
    /// 与白闪时必须看它，否则 `--motion reduce/off` 就只是"读出来打印一行"的假功能。
    pub motion: Motion,
    /// 生效语言。**每进程内可变**（点语言行就换）⇒ 它**必须**进 `menu::content_sig`，
    /// 否则换了字的同一帧会被判成"内容没变"而停在旧 buffer 上（省电闸门反过来把改动挡了）。
    /// 结果卡上的字不受它影响：那些串在抽取那一刻就烤好了（§7-R10）。
    pub lang: Lang,
}

/// 画一帧卡片。`bw`/`bh` 是物理宽高，`stride` 是每行字节数（= bw×4）。
///
/// 画法（§4.5 的底板三层，顺序是有理由的）：
/// 1. 圆角外圈 `EDGE` → 2. 内缩 1 px 的 `BG` → 3. 相位内容 → 4. **重描一圈** `EDGE` →
/// 5. 四角按同一半径清成透明。
///
/// 第 4 步为什么要描**两遍**：结果卡的档位色带是**满宽**的（§4.5），先画的描边会被它盖掉，
/// 于是"卡片上沿没有线、左右下有线"这种不对称只有出图过眼才看得见。第 5 步同理由：burst
/// 的扩散块和 SSR 的过白都是整卡方块填充，不统一清一次就会让那几个相位变成方卡片。
///
/// `dry_run` 与 `motion` 都**不进** `menu::content_sig`：两者都是每进程恒定的量（不给
/// `--db` 就是内存库；`--motion` 没有运行中的 UI 入口能改），所以签名相等时不可能出现
/// "一次是演示另一次是真档"或"一次闪另一次不闪" ⇒ 漏了它们不会让静止判定错过像素变化。
pub fn paint(canvas: &mut [u8], bw: i32, bh: i32, stride: usize, anim: &MenuAnim, ctx: Ctx) {
    let scale = ctx.scale;
    let corner = px(CORNER, scale);
    let t = px(1, scale).max(1);
    let flash = menu::white_flash(anim, ctx.motion);
    canvas.fill(0);
    fill_round(canvas, stride, (0, 0, bw, bh), corner, bgra(EDGE));
    fill_round(
        canvas,
        stride,
        (t, t, (bw - 2 * t).max(0), (bh - 2 * t).max(0)),
        (corner - t).max(0),
        bgra(BG),
    );

    match anim {
        MenuAnim::Menu => paint_menu(canvas, stride, ctx),
        MenuAnim::Burst {
            rarity, elapsed, ..
        } => {
            paint_burst(canvas, stride, *rarity, *elapsed, ctx);
            paint_footer(canvas, stride, anim, ctx);
        }
        MenuAnim::Reveal { lines, best, .. } => {
            if flash {
                // SSR 的"闪一下"必须是**看不到内容**的整卡过白（只画个白框等于没闪）。
                // 亮度已经按判据②夹到 0.85（见 [`FLASH`]），不再是纯白。
                fill_round(canvas, stride, (0, 0, bw, bh), corner, bgra(FLASH));
            } else {
                paint_reveal(canvas, stride, lines, *best, ctx);
                paint_footer(canvas, stride, anim, ctx);
            }
        }
    }
    stroke_round(
        canvas,
        stride,
        (0, 0, bw, bh),
        corner,
        t,
        bgra(if flash { FLASH } else { EDGE }),
    );
    clip_to_plate(canvas, bw, bh, stride, corner);
}

/// 菜单态：标题行（左＝本语言的"盲盒"，右＝碎片 + 演示标）、三行按钮、底部语言行。
///
/// 标题行右端的徽标（演示）来自 [`l10n::header_shards`]：渲染层一个字面量都不写，否则就
/// 绕过了"会渲染的 ⊆ 清单 ⊆ 真 cmap"那三条字体门禁（§4.2）。徽标放标题行而不是单独一行，
/// 是给英文那种宽标签留出预算（§7-R13）。
///
/// 三行的版式（§4.5）= 圆角行底 `ROW` + 左端 3 px 强调条 + 让出强调条之后的标签。行底/强调条/
/// 文字矩形全部从 [`menu`] 的几何函数取，**这里不重算**：`hit_test` 按整行命中，而 draw 按
/// `label_rect` 画字，两套各算一份就会出现"字的位置和点的位置差一格"。
fn paint_menu(canvas: &mut [u8], stride: usize, ctx: Ctx) {
    let scale = ctx.scale;
    let head = RectI::new(PAD, PAD, CARD_W - 2 * PAD, menu::HEAD_H);
    // 标题在左、余额在右：两头都贴同一张矩形的边，中间的空隙随内容自然变。
    draw_in(
        canvas,
        stride,
        head,
        scale,
        if ctx.dry_run { WARN } else { FG_DIM },
        &l10n::header_shards(ctx.lang, ctx.balance, ctx.dry_run),
        Align::Right,
    );
    draw_in(
        canvas,
        stride,
        head,
        scale,
        FG,
        ctx.lang.t(Key::Title),
        Align::Left,
    );
    for r in menu::layout() {
        let row = r.as_rect();
        row_plate(canvas, stride, row, scale);
        row_accent(canvas, stride, row, scale);
        let label = menu::label_rect(row);
        // 标签也过 `fit_to`：`every_locale_fits_the_card` 证明的是"四 locale × 三档都装得下"，
        // 那是**当前文案**的事实；而强调条是这一轮**新挤占**的 9 逻辑 px。画之前按预算截一次
        // 等于"文案以后改了也不会顶穿出界"——宁可补一个 `…`，也不要"字压到描边上"。
        let text = menu::fit_to(&r.item.label(ctx.lang), budget_of(label, scale), scale);
        draw_in(canvas, stride, label, scale, FG, &text, Align::Left);
    }
    paint_footer(canvas, stride, &MenuAnim::Menu, ctx);
}

/// 画一块**圆角行底**（§4.5 的 `#22252B`）。菜单行与结果卡明细共用。
fn row_plate(canvas: &mut [u8], stride: usize, row: RectI, scale: f32) {
    fill_round(
        canvas,
        stride,
        phys(row, scale),
        px(menu::ROW_CORNER, scale),
        bgra(ROW),
    );
}

/// 画菜单行左端的 3 px **强调条**（§4.5）。结果卡明细行**不画**：那里的信息密度已经由
/// 档位色带给出，多一排竖条只会让六行明细看起来像进度条。
///
/// 它画在行底**内侧** [`menu::ROW_CORNER`]：顶到边的方条会把圆角行底的左上/左下两个角填回
/// 直角，而"这一行的角是方的"只有出图过眼才看得出来，像素计数全绿。
fn row_accent(canvas: &mut [u8], stride: usize, row: RectI, scale: f32) {
    let (x, y, w, h) = phys(menu::accent_bar(row), scale);
    fill(canvas, stride, x, y, w, h, bgra(ACCENT));
}

/// 底部那一行（`FOOTER_Y..FOOTER_Y+FOOTER_H`）：左＝错误行，右＝**随相位变**的东西。
///
/// 静止档右边是语言行（可点 ⇒ 显示"下一次点下去会变成什么"），burst/reveal 档右边是余额行
/// —— 语言行在这两档**点不到**（[`MenuAnim::is_busy`] 挡住点击），把点不到的东西画上去
/// 就是假按钮（D14 那条"永不触发的假功能"同族）。
///
/// 两者同行带 ⇒ 错误行的预算不是整行，而是 `内容区 − 右段宽 − FOOTER_GAP`，用
/// [`menu::fit_to`] 逐 char 按**像素**截。**被截的永远是错误正文而不是金额**（读错钱比
/// 看不见更糟）；错误全文同时进 stderr（`app.rs` 的 `gacha: {msg}`），卡片只承担"出事了"这一眼。
fn paint_footer(canvas: &mut [u8], stride: usize, anim: &MenuAnim, ctx: Ctx) {
    let scale = ctx.scale;
    let right = match anim {
        MenuAnim::Menu => menu::MenuItem::Lang.label(ctx.lang),
        _ => l10n::line_balance(ctx.lang, ctx.balance),
    };
    let f = menu::footer();
    let gap = px(FOOTER_GAP, scale);
    let budget = (menu::line_budget_px(scale) - line_w(&right, scale) - gap).max(0);
    // **没出错就一个字都不画**。`line_error(lang, "")` 只剩前缀，于是每张正常卡的左下角挂着
    // 一行红字"抽取失败："—— 出图过眼抓到的（像素门禁只看见"footer 有墨"，玩家看见的是
    // "我这抽失败了"）。判据①那条"数字全绿但画面对不上"同族，钉成 `no_error_...` 测试。
    let err = if ctx.error.is_empty() {
        String::new()
    } else {
        menu::fit_to(&l10n::line_error(ctx.lang, ctx.error), budget, scale)
    };
    if !err.is_empty() {
        // 矩形仍然给整条 footer 带：`err` 已经按"右段让出来的"预算截过，这里只负责贴左。
        // （预算是**物理**像素、矩形是 **logical**，所以不能把 budget 塞进 `RectI`。）
        draw_in_plated(
            canvas,
            stride,
            f.as_rect(),
            scale,
            ERR_FG,
            &err,
            Align::Left,
        );
    }
    draw_in_plated(
        canvas,
        stride,
        f.as_rect(),
        scale,
        FG_DIM,
        &right,
        Align::Right,
    );
}

fn paint_burst(canvas: &mut [u8], stride: usize, rarity: Rarity, elapsed: u32, ctx: Ctx) {
    let scale = ctx.scale;
    let spec = menu::anim_spec(rarity, ctx.motion);
    let rad = menu::burst_radius(elapsed, spec);
    let (cx, cy) = (CARD_W / 2, CARD_H / 2);
    fill(
        canvas,
        stride,
        px(cx - rad, scale),
        px(cy - rad, scale),
        px(rad * 2, scale),
        px(rad * 2, scale),
        bgra(spec.color),
    );
    // 扩散期也得读得出"在等什么"：中央一块牌 + 本语言的"开箱中…"。
    let msg = ctx.lang.t(Key::Burst).to_string();
    let (w, h) = (line_w(&msg, scale), line_h(scale));
    // 牌 = 字 + 一圈 pad，**跟着字宽走**（不是卡片宽）：换语言换宽度，写死的牌子会让
    // 英文的"Opening…"压出牌外，而深墨压深底 = 读不出（上一轮白字金底同族）。
    let pad = (h / 4).max(2);
    let (tx, ty) = (px(cx, scale) - w / 2, px(cy, scale) - h / 2 - pad);
    fill(
        canvas,
        stride,
        tx - pad,
        ty - pad,
        w + pad * 2,
        h + pad * 2,
        bgra(spec.color),
    );
    draw_line(
        canvas,
        stride,
        tx,
        ty + pad,
        scale,
        ink_on(spec.color),
        &msg,
    );
}

/// 结果卡：顶端**满宽档位色带**（第 0 行）+ [`menu::reveal_rows()`] 的明细行。
///
/// 色带为什么满宽而明细行只到内容区：档位是这一屏唯一要一眼读出的东西，"一条有底色的带"比
/// "一行彩色字"读得快（N 与 R 的差别不该靠认字）。而**满宽必然盖住 1 px 描边和圆角** ⇒
/// 描边由 [`paint`] 后画、四角由 [`clip_to_plate`] 统一清，这里只管填色带本体。
///
/// 色带上的墨色走 [`ink_on`]（由带色算，不是常量）：上一轮"白字压金底读不出"那条教训从
/// burst 扩到 reveal，判据见 `every_paintable_block_has_a_legible_ink`。
fn paint_reveal(canvas: &mut [u8], stride: usize, lines: &[String], best: Rarity, ctx: Ctx) {
    let scale = ctx.scale;
    let spec = menu::anim_spec(best, ctx.motion);
    let band = menu::band();
    let (bx, by, bw, bh) = phys(band, scale);
    fill(canvas, stride, bx, by, bw, bh, bgra(spec.color));
    if let Some(title) = lines.first() {
        // 色带里的文字仍然留 `PAD` 的左右气：顶到边的 CJK 笔画会看起来像"被切了一刀"，
        // 而带子本身已经承担了"这是档位"，不需要文字再贴边。
        let text = RectI::new(band.x + PAD, band.y, (band.w - 2 * PAD).max(0), band.h);
        let ink = ink_on(spec.color);
        draw_in(
            canvas,
            stride,
            text,
            scale,
            ink,
            &menu::fit_to(title, budget_of(text, scale), scale),
            Align::Left,
        );
    }
    for (row, l) in menu::reveal_rows().into_iter().zip(lines.iter().skip(1)) {
        row_plate(canvas, stride, row, scale);
        let label = menu::row_text(row);
        draw_in(
            canvas,
            stride,
            label,
            scale,
            FG,
            &menu::fit_to(l, budget_of(label, scale), scale),
            Align::Left,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个相位的画布 + 它的物理尺寸（所有断言都从这里取口径，别各自再算一遍）。
    struct Shot {
        canvas: Vec<u8>,
        bw: i32,
        bh: i32,
        stride: usize,
        scale: f32,
    }

    impl Shot {
        /// **默认档（`full` + `zh`）**的一次出图 —— 已签收的那套像素口径走的就是中文。
        /// 现有断言全部从这里取，于是"这一轮没改掉默认观感"本身就是测试形状。
        fn at(scale: f32, anim: &MenuAnim, balance: i64, dry: bool, err: &str) -> Self {
            Self::with_motion(scale, anim, balance, dry, err, Motion::Full)
        }

        fn with_motion(
            scale: f32,
            anim: &MenuAnim,
            balance: i64,
            dry: bool,
            err: &str,
            motion: Motion,
        ) -> Self {
            Self::paint_at(scale, anim, balance, dry, err, motion, Lang::DEFAULT)
        }

        /// 逐 locale 出图（提交 6/7 的对比度断言与出图过眼要用；`en` 比 `zh` 宽得多，
        /// 只测中文等于没测 §7-R13 那条"破版"风险）。
        fn at_lang(
            scale: f32,
            anim: &MenuAnim,
            balance: i64,
            dry: bool,
            err: &str,
            motion: Motion,
            lang: Lang,
        ) -> Self {
            Self::paint_at(scale, anim, balance, dry, err, motion, lang)
        }

        fn paint_at(
            scale: f32,
            anim: &MenuAnim,
            balance: i64,
            dry: bool,
            err: &str,
            motion: Motion,
            lang: Lang,
        ) -> Self {
            let (bw, bh) = (px(CARD_W, scale).max(1), px(CARD_H, scale).max(1));
            let stride = (bw * 4) as usize;
            let mut canvas = vec![0u8; stride * bh as usize];
            paint(
                &mut canvas,
                bw,
                bh,
                stride,
                anim,
                Ctx {
                    balance,
                    dry_run: dry,
                    error: err,
                    scale,
                    motion,
                    lang,
                },
            );
            Shot {
                canvas,
                bw,
                bh,
                stride,
                scale,
            }
        }

        /// logical 矩形里的像素（半开区间，越界自动丢掉）。
        fn pixels(&self, r: (i32, i32, i32, i32)) -> impl Iterator<Item = [u8; 4]> + '_ {
            let (x, y, w, h) = (
                px(r.0, self.scale),
                px(r.1, self.scale),
                px(r.2, self.scale),
                px(r.3, self.scale),
            );
            (y..y + h).flat_map(move |yy| {
                (x..x + w)
                    .filter(move |&xx| xx >= 0 && yy >= 0)
                    .map(move |xx| self.phys_at(xx, yy))
            })
        }

        fn count(&self, r: (i32, i32, i32, i32), rgb: u32) -> u32 {
            let want = bgra(rgb);
            self.pixels(r).filter(|p| *p == want).count() as u32
        }

        /// 这块区域里有多少格**不透明**像素"离 `ink` 比离 `bg` 近"（三通道 L1）。
        ///
        /// 为什么判"字有没有画出来"不能直接用 [`count`](Self::count)：抗锯齿下绝大多数
        /// 字内像素是 `out = bg·(1−a) + ink·a` 的**混合色**，逐通道正好等于 `ink` 的格子
        /// 只在笔画正中 —— 1× 那一档中文笔画只有一两个物理像素宽，实测计数是 **0**，
        /// 于是"字确实画了"的断言会红在一条测不上任何东西的口径上。方向仍然分得开：
        /// 混合色离墨近就是墨，离底近就是底 ⇒ 画错颜色、画在别处，这条照样红。
        fn blends_toward(&self, r: (i32, i32, i32, i32), bg: u32, ink: u32) -> u32 {
            let (bg, ink) = (bgra(bg), bgra(ink));
            let dist = |p: [u8; 4], c: [u8; 4]| -> i32 {
                (p[0] as i32 - c[0] as i32).abs()
                    + (p[1] as i32 - c[1] as i32).abs()
                    + (p[2] as i32 - c[2] as i32).abs()
            };
            self.pixels(r)
                .filter(|p| p[3] != 0 && dist(*p, ink) < dist(*p, bg))
                .count() as u32
        }

        /// 这块区域里有没有"画了东西"（既不是透明也不是卡片底色）。
        fn has_ink(&self, r: (i32, i32, i32, i32)) -> bool {
            let bg = bgra(BG);
            self.pixels(r).any(|p| p != bg && p != [0, 0, 0, 0])
        }

        /// 物理像素（越界一律当透明）。断言侧只有这一处直接取像素。
        fn phys_at(&self, xx: i32, yy: i32) -> [u8; 4] {
            let i = (yy as usize) * self.stride + (xx as usize) * 4;
            self.canvas
                .get(i..i + 4)
                .map(|p| [p[0], p[1], p[2], p[3]])
                .unwrap_or([0, 0, 0, 0])
        }

        /// 这个物理像素在**底板剪影**里吗。`clip_to_plate` 清的正是这一族的补集，所以判据
        /// 用同一个 [`in_round`] ⇒ "圆角画没画"不靠猜。
        fn in_plate(&self, xx: i32, yy: i32) -> bool {
            in_round(xx, yy, self.bw, self.bh, px(CORNER, self.scale))
        }

        /// **剪影**（不透明像素的集合）：相位之间必须逐坐标相同。
        fn opaque_mask(&self) -> Vec<(i32, i32)> {
            (0..self.bh)
                .flat_map(|yy| (0..self.bw).map(move |xx| (xx, yy)))
                .filter(|&(x, y)| self.phys_at(x, y)[3] != 0)
                .collect()
        }

        /// 圆角版取代旧的那条"整卡全不透明"：**剪影内全不透明、剪影外全透明**。
        ///
        /// 旧断言在圆角之后必然红（四角本来就是透明的），而把它改成"只查剪影内"就漏掉了
        /// "透明像素跑进剪影里"（= 卡片上有个洞）这一半。两边一起查才是原来的强度。
        fn silhouette_ok(&self) -> bool {
            (0..self.bh).all(|yy| {
                (0..self.bw).all(|xx| {
                    self.phys_at(xx, yy)[3] == if self.in_plate(xx, yy) { 255 } else { 0 }
                })
            })
        }

        /// 剪影**之内**每一格都是 `rgb`（剪影外仍是透明）。取代旧的"整卡一个色"。
        fn all_in_plate(&self, rgb: u32) -> bool {
            let want = bgra(rgb);
            (0..self.bh).all(|yy| {
                (0..self.bw).all(|xx| {
                    let got = self.phys_at(xx, yy);
                    if self.in_plate(xx, yy) {
                        got == want
                    } else {
                        got[3] == 0
                    }
                })
            })
        }

        /// 一句字在本 shot 缩放下的**逻辑**宽。`line_w` 返回物理像素，而 [`Shot::pixels`]
        /// 收逻辑矩形 ⇒ 换算只准在这一处发生（上一轮的排版口径就散在三处过）。
        /// `ceil` 是故意的：框只许比字大，不许小一格就把"字明明画了"断成红。
        fn w_log(&self, s: &str) -> i32 {
            (line_w(s, self.scale) as f32 / self.scale).ceil() as i32
        }

        /// 字体行高的**逻辑**值（同上换算规则）。
        fn h_log(&self) -> i32 {
            (line_h(self.scale) as f32 / self.scale).ceil() as i32
        }
    }

    #[test]
    fn card_is_opaque_inside_the_plate_at_every_scale_and_its_border_is_distinct() {
        for scale in [1.0, 1.5, 2.0] {
            let s = Shot::at(scale, &MenuAnim::Menu, 1200, true, "");
            assert!(!s.canvas.is_empty(), "scale {scale}：画布是空的");
            assert!(
                s.silhouette_ok(),
                "scale {scale}：剪影不干净（里面有洞，或四角外面还有墨）"
            );
            // 剪影**必须真的比整卡小**，否则"圆角"只是个没生效的参数：CORNER = 3 在 1.5×
            // 上是 4~5 px 的角，削掉的像素数在出图上是看得见的（判据①），在这里是可数的。
            let lost = (s.bw * s.bh) as usize - s.opaque_mask().len();
            assert!(
                lost > 0,
                "scale {scale}：圆角一个像素都没削 ⇒ `CORNER` 是假参数（半径 1 确实削不动，见 `corner_inside`）"
            );
            assert_ne!(EDGE, BG, "描边和底色相同 ⇒ 画没画边框在像素上读不出来");
            assert!(
                s.count((0, 0, CARD_W, 1), EDGE) > 0,
                "scale {scale}：顶边描线没画出来"
            );
        }
    }

    /// 判据④"静止/动画的差别只能在内容上"在**形状**这一维的表达：四角透明的那些格子必须
    /// 每个相位都一模一样。`Burst` 的扩散块和 SSR 的过白都是**整卡方块**填充，少了
    /// `clip_to_plate` 那一道就会让"这一帧卡片是方的"，而像素计数只会报告"这四格画了东西"。
    #[test]
    fn the_card_silhouette_is_the_same_in_every_phase() {
        for scale in [1.0, 1.5, 2.0] {
            let ssr = MenuAnim::Reveal {
                elapsed: 0,
                lines: vec!["SSR!".to_string(), "A".to_string()],
                best: Rarity::SSR,
            };
            let shots = [
                Shot::at(scale, &MenuAnim::Menu, 1200, false, ""),
                Shot::at(
                    scale,
                    &MenuAnim::Burst {
                        rarity: Rarity::N,
                        elapsed: 0,
                        lines: vec!["N".to_string()],
                    },
                    0,
                    true,
                    "",
                ),
                Shot::at(
                    scale,
                    &MenuAnim::Burst {
                        rarity: Rarity::SSR,
                        elapsed: 12,
                        lines: vec!["SSR!".to_string()],
                    },
                    0,
                    true,
                    "",
                ),
                Shot::at(scale, &ssr, 0, false, ""),
                Shot::at(
                    scale,
                    &MenuAnim::Reveal {
                        elapsed: menu::SSR_FLASH_TICKS,
                        lines: vec!["SSR!".to_string(), "A".to_string()],
                        best: Rarity::SSR,
                    },
                    0,
                    false,
                    "",
                ),
            ];
            for s in &shots {
                assert!(s.silhouette_ok(), "scale {scale}：这个相位的剪影不干净");
            }
            let want = shots[0].opaque_mask();
            for (i, s) in shots.iter().enumerate().skip(1) {
                assert_eq!(
                    s.opaque_mask(),
                    want,
                    "scale {scale}：相位 {i} 的剪影和菜单态不同 ⇒ 那个相位把圆角盖成了直角"
                );
            }
        }
    }

    /// §4.5 那条"新增机器防线"的第一半：**查表本身**。
    ///
    /// `LINEAR_X1000` 是把 WCAG 的 `((c+0.055)/1.055)^2.4` 离线算进常量表，好处是运行时全整数、
    /// 跨平台逐位一致；代价是**表抄错了没有任何测试会红**（对比度照样算得出数，只是判据偏移）。
    /// 所以这里用 `f64` 原式逐个通道对一遍，把表钉回它声称的那个公式。容差 1 个千分位单位，
    /// 唯一的触发点是 `c = 1`（原式 0.305 ⇒ 舍入是 0，表给 1）：非零通道不该塌成 0，而这个方向
    /// 只会把暗色**抬亮** ⇒ 对比度只会被低估。本调色板最低通道是 `INK` 的 `0x0A`，碰不到那一格。
    #[test]
    fn the_linearization_table_matches_the_reference_formula() {
        let reference = |c: u32| -> f64 {
            let x = c as f64 / 255.0;
            if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        assert_eq!(LINEAR_X1000[0], 0, "黑必须是 0，否则整卡的对比度全部虚高");
        assert_eq!(
            LINEAR_X1000[255], 1000,
            "白必须是 1，否则 'L = 1' 那条参照点没了"
        );
        let mut prev = 0u32;
        for c in 0..=255u32 {
            let got = LINEAR_X1000[c as usize];
            let want = (reference(c) * 1000.0).round() as u32;
            assert!(
                got.abs_diff(want) <= 1,
                "通道 {c}：表给 {got}，参考公式给 {want} ⇒ 表不是这个公式（例如拿 gamma 2.2 近似过）"
            );
            assert!(got >= prev, "通道 {c}：线性化表必须单调不减");
            prev = got;
        }
        // 换一条**近似**公式（gamma 2.2，最常见的"看着差不多"写法）必须被这条抓住：
        // 它在中间调上偏到十几个千分位单位，不是 1。
        let gamma = |c: u32| ((c as f64 / 255.0).powf(2.2) * 1000.0).round() as u32;
        let worst = (0..=255u32)
            .map(|c| gamma(c).abs_diff(LINEAR_X1000[c as usize]))
            .max();
        assert!(
            worst.unwrap_or(0) > 1,
            "这条对照没咬住 gamma 2.2 ⇒ 容差放得太宽，表错了也不会红"
        );
    }

    /// §4.5 那条"新增机器防线"：任何**会被当成文字底色**的颜色，都必须有一支 ≥ 4.5:1 的墨，
    /// 而且 [`ink_on`] 真的选中它。逐档（四稀有度 × 三种 motion）× 逐板色遍历。
    ///
    /// 为什么判据是"最好的那支也过线"而不是"ink_on 选出来的那支过线"：前者意味着**无论版式
    /// 怎么用**都读得出，后者只保证今天这一版。上一轮的白字金底就是"版式写死一支墨"的后果。
    #[test]
    fn every_paintable_block_has_a_legible_ink() {
        // 版式里所有"字压在上面"的底色：底板、描边、行底、强调条、白闪、演示标，
        // 再加上四档在三种 motion 下的色（`anim_spec` 不动 color，这一族本来就是重复的）。
        let mut blocks = vec![BG, EDGE, ROW, ACCENT, FLASH, WARN];
        for r in Rarity::ALL {
            for m in Motion::ALL {
                blocks.push(menu::anim_spec(r, m).color);
            }
        }
        for b in blocks {
            let ink = ink_on(b);
            assert_ne!(ink, b, "底色 #{b:06X} 上选的墨和底色同色 ⇒ 必然看不见");
            let best = contrast_x1000(INK, b).max(contrast_x1000(FG, b));
            let got = contrast_x1000(ink, b);
            assert!(
                got >= CONTRAST_MIN,
                "底色 #{b:06X}：`ink_on` 给了 #{ink:06X} = {got}‰，连 4.5:1 都不到（两支候选里最好的也才 {best}‰）"
            );
            assert_eq!(got, best, "底色 #{b:06X}：`ink_on` 没取对比度更高的那支");
        }
        // **写死**的搭配（不是 `ink_on` 选的）逐条单独量：版式改了而这几对没跟着改，就是红。
        for (fg, bg, what) in [
            (FG, BG, "卡片上的正文"),
            (FG, ROW, "行底上的正文（菜单标签 / 明细）"),
            (FG_DIM, BG, "次要文字（余额 / 语言行）压在底板上"),
            (FG_DIM, ROW, "次要文字压在行底上"),
            (ERR_FG, BG, "错误行压在底板上"),
            (WARN, BG, "DRYRUN 演示标"),
        ] {
            assert!(
                contrast_x1000(fg, bg) >= CONTRAST_MIN,
                "{what}：#{fg:06X} on #{bg:06X} = {}‰ < {CONTRAST_MIN}‰",
                contrast_x1000(fg, bg)
            );
        }
        // **反面对照（这条门禁能不能红，只由这一段证明）**：上一轮那条真 bug —— 白字压 SSR
        // 金底 —— 用现在这把尺子必须量出"不合格"。如果它读成 ≥ 4500，那么上面所有遍历都是
        // 白跑的（阈值定错方向、或定标又漂了 10×，都会绿成一片，而卡片照样读不出）。
        let gold = menu::anim_spec(Rarity::SSR, Motion::Full).color;
        assert!(
            contrast_x1000(FG, gold) < CONTRAST_MIN,
            "白字压金底量出 {}‰ ⇒ 尺子本身错了（`luma_x1e7` 的定标或 4.5:1 的偏移）：\
             这条对照正是上一轮的交付缺陷，它必须不合格",
            contrast_x1000(FG, gold)
        );
        assert!(
            contrast_x1000(INK, gold) >= CONTRAST_MIN,
            "同一块金底上深墨不到 4.5:1 ⇒ 不是版式的锅，是金底本身不能当底色用"
        );
    }

    /// 判据②的**数字**形式：白闪那一帧不许把亮度推到上限。
    ///
    /// 两条一起查：通道值 ≤ 85%（方案 §4.5 那句"亮度上限 0.85"的字面口径）**且** WCAG 相对
    /// 亮度 ≤ 85%（真正和"前庭负担"相关的那个量，纯白在这里是 100%）。只查通道的话，
    /// `#FFFFFF` 换成 `#FAFAFA` 也算过，而那还是判据②点名的形状。
    #[test]
    fn flash_is_capped_at_85_percent_brightness() {
        let max_channel = |rgb: u32| ((rgb >> 16) & 0xFF).max((rgb >> 8) & 0xFF).max(rgb & 0xFF);
        let m = max_channel(FLASH);
        assert_ne!(FLASH, 0xFF_FF_FF, "纯白正是判据②点名的最坏形状");
        assert!(
            m * 100 <= 255 * 85,
            "白闪通道值 {m}/255 = {:.1}% > 85%",
            m as f64 * 100.0 / 255.0
        );
        // 也不能暗到"闪"读不出来：夹在 80%–85% 之间才既是信号又不刺眼。
        assert!(
            m * 100 >= 255 * 80,
            "白闪通道值只有 {m} ⇒ 和卡片描边/行底拉不开，'这抽不一般'没传达出来"
        );
        let luma = luma_x1e7(FLASH);
        assert!(
            luma <= 8_500_000,
            "白闪的相对亮度 {luma}/10_000_000 超过 85%"
        );
        // 也不能暗到看不见：85% 是上限，但"闪"要真的比卡片的任何一块底色亮。
        // 比的是**相对亮度**而不是打包整数：`0xD8D8D8 < 0xF5B301` 按整数成立、按亮度不成立，
        // 写 `FLASH > BG` 那种比较迟早判错方向。
        for rgb in [
            BG,
            EDGE,
            ROW,
            menu::anim_spec(Rarity::SSR, Motion::Full).color,
        ] {
            assert!(
                luma_x1e7(FLASH) > luma_x1e7(rgb),
                "白闪 #{FLASH:06X} 不比 #{rgb:06X} 亮 ⇒ 那一帧读起来是'暗了一下'"
            );
        }
    }

    /// 三态版式的**逐 locale × 逐档 × 逐缩放**遍历：色带上的字必须真的以 `ink_on(带色)`
    /// 落在纸上。上一轮的对比度缺陷（白字压金底、压浅灰底）就是"只测中文 / 只测一档"漏掉的，
    /// 而 §7-R13 那条"en 比 zh 宽"也只能在同一把尺子上一起量。
    #[test]
    fn band_text_is_legible_for_every_locale_tier_and_scale() {
        for &lang in &Lang::ALL {
            for &scale in &[1.0f32, 1.5, 2.0] {
                for r in Rarity::ALL {
                    let spec = menu::anim_spec(r, Motion::Full);
                    let ink = ink_on(spec.color);
                    assert!(
                        contrast_x1000(ink, spec.color) >= CONTRAST_MIN,
                        "{lang:?} @{scale}× {r}：色带墨色只有 {}‰",
                        contrast_x1000(ink, spec.color)
                    );
                    let lines = menu::report_lines(
                        &[0, 0, 0, 0],
                        &ten_items([(r, false); 10]),
                        42,
                        lang,
                        scale,
                    );
                    let s = Shot::at_lang(
                        scale,
                        &MenuAnim::Reveal {
                            elapsed: menu::SSR_FLASH_TICKS,
                            lines,
                            best: r,
                        },
                        1_200,
                        false,
                        "",
                        Motion::Full,
                        lang,
                    );
                    let band = menu::band();
                    assert!(
                        s.blends_toward((PAD, band.y, CARD_W - 2 * PAD, band.h), spec.color, ink)
                            > 20,
                        "{lang:?} @{scale}× {r}：色带上几乎没有朝墨色混合的像素 ⇒ 那行标题没画出来"
                    );
                    // 明细行的正文（`FG` on `ROW`）也得真的在纸上，截断成空串就是假版式。
                    let row = menu::reveal_rows()[0];
                    assert!(
                        s.blends_toward((row.x, row.y, row.w, row.h), ROW, FG) > 20,
                        "{lang:?} @{scale}× {r}：明细第一行没有正文像素"
                    );
                }
            }
        }
    }

    #[test]
    fn menu_phase_inks_the_header_and_every_row() {
        for scale in [1.0, 1.5] {
            let s = Shot::at(scale, &MenuAnim::Menu, 1200, false, "");
            assert!(
                s.has_ink((PAD, PAD, CARD_W - 2 * PAD, 10)),
                "scale {scale}：余额行是空的"
            );
            // 三行：行底 + 强调条 + 字都要有（`layout()` = 只有那三个可点行，footer 另算）
            for r in menu::layout() {
                let row = r.as_rect();
                let band = (row.x, row.y, row.w, row.h);
                assert!(
                    s.has_ink(band),
                    "scale {}：项 {:?} 这一行是空的（字号或行距漂了）",
                    scale,
                    r.item
                );
                assert!(
                    s.count((row.x + 2, row.y + 2, row.w - 4, row.h - 4), ROW) > 0,
                    "scale {scale}：项 {:?} 的行底没落在行矩形里 ⇒ 画的是一整块卡片",
                    r.item
                );
                // 强调条（§4.5 的新几何）：它**在行底里**、离左边 `ROW_CORNER`，画在圆角行底
                // 之内而不是顶上 ⇒ 顶到边会把角填回直角（`row_accent` 那段）。
                let bar = menu::accent_bar(row);
                assert!(
                    s.count((bar.x, bar.y, bar.w, bar.h), ACCENT) > 0,
                    "scale {scale}：项 {:?} 没有强调条 ⇒ 版式没真的换",
                    r.item
                );
                assert!(
                    bar.x > row.x
                        && bar.y > row.y
                        && bar.x + bar.w < row.x + row.w
                        && bar.y + bar.h < row.y + row.h,
                    "scale {scale}：项 {:?} 的强调条顶到了行边 ⇒ 会把圆角行底左上/左下角填回直角",
                    r.item
                );
            }
            // 语言行：它**没有行底**（footer 直接画在卡片底色上），所以这里只查"画了字"。
            // 这条是"用户能自己设语言"在像素面的唯一证据 —— 矩形在却空白 = 假功能。
            let f = menu::footer();
            assert!(
                s.has_ink((f.x, f.y, f.w, f.h)),
                "scale {scale}：footer 那条带是空的 ⇒ 语言行没画出来"
            );
            assert_eq!(
                s.count((f.x, f.y, f.w, f.h), ROW),
                0,
                "scale {scale}：语言行不该有行底（它不是那三行之一）"
            );
        }
    }

    #[test]
    fn dryrun_stamp_changes_the_header_pixels() {
        // `content_sig` 不含 dry_run（每进程恒定），所以单独钉住"演示标真的画得出来"：
        // 内存库和真档如果长得一样，用户会把演示当成落库。
        let real = Shot::at(1.5, &MenuAnim::Menu, 1200, false, "");
        let demo = Shot::at(1.5, &MenuAnim::Menu, 1200, true, "");
        assert_ne!(real.canvas, demo.canvas, "DRYRUN 标必须看得见");
        assert!(demo.has_ink((PAD, PAD, CARD_W - 2 * PAD, 10)));
    }

    /// 目视核对抓到的缺陷：白字压在金块（SSR）与浅灰块（N）上，像素门禁只看到"这里画了
    /// 东西"于是全绿，人眼却读不出。⇒ 墨色按亮度选，并且逐档钉住"提示那一行真的有墨色像素"。
    #[test]
    fn burst_message_is_legible_on_every_rarity() {
        for r in Rarity::ALL {
            let spec = menu::anim_spec(r, Motion::Full);
            let ink = ink_on(spec.color);
            assert_ne!(ink, spec.color, "{r}：墨色和块同色 ⇒ 提示必然看不见");
            for t in [0u32, spec.burst_ticks / 2] {
                let anim = MenuAnim::Burst {
                    rarity: r,
                    elapsed: t,
                    lines: vec!["A".to_string()],
                };
                let s = Shot::at(1.5, &anim, 0, true, "");
                // 提示的包围盒 = **当前语言那句话**的真实字宽/行高，居中于卡心（`paint_burst`
                // 就是这么摆的）。旧的 `10 × STEP × TEXT_PX` 是 5×7 点阵的算术，换成真字体后
                // 那个框既框不住"开箱中…"也框不住"Opening…"。
                let msg = Lang::Zh.t(Key::Burst);
                let (w, h) = (s.w_log(msg), s.h_log());
                let want = bgra(ink);
                let n = s
                    .pixels((CARD_W / 2 - w / 2, CARD_H / 2 - h / 2 - 2, w, h + 4))
                    .filter(|p| *p == want)
                    .count();
                assert!(
                    n > 20,
                    "{r} 在 tick {t} 的提示区里只有 {n} 个墨色像素 ⇒ 这行字没画出来（{msg:?}）"
                );
            }
        }
    }

    /// 换语言必须真的换字：`en` 的 burst 牌用英文且**按英文字宽重排**（牌子宽度跟着
    /// `measure` 走，不是把中文的牌子换个内容）。这条抓的是"语言只影响菜单项、结果卡
    /// 还是中文"那一族半成品。
    #[test]
    fn burst_plaque_relays_out_for_every_locale() {
        let anim = MenuAnim::Burst {
            rarity: Rarity::SR,
            elapsed: 4,
            lines: vec!["A".to_string()],
        };
        let zh = Shot::at(1.5, &anim, 0, true, "");
        // `Zh` 自己排除：它就是对照组，"换语言必须换像素"对它没有意义
        for l in Lang::ALL.iter().copied().filter(|l| *l != Lang::Zh) {
            let s = Shot::at_lang(1.5, &anim, 0, true, "", Motion::Full, l);
            assert_ne!(s.canvas, zh.canvas, "{l:?}：换了语言卡片一个像素都没动");
            // 牌子里必须有墨，且墨色由底色算出（不是常量）
            let ink = bgra(ink_on(menu::anim_spec(Rarity::SR, Motion::Full).color));
            let (w, h) = (s.w_log(l.t(Key::Burst)), s.h_log());
            let n = s
                .pixels((CARD_W / 2 - w / 2, CARD_H / 2 - h / 2 - 2, w, h + 4))
                .filter(|p| *p == ink)
                .count();
            assert!(n > 20, "{l:?}：提示区里只有 {n} 个墨色像素");
        }
    }

    #[test]
    fn burst_block_grows_tick_by_tick() {
        let spec = menu::anim_spec(Rarity::SSR, Motion::Full);
        let mut prev = 0;
        for t in 0..spec.burst_ticks {
            let anim = MenuAnim::Burst {
                rarity: Rarity::SSR,
                elapsed: t,
                lines: vec!["A".to_string()],
            };
            let s = Shot::at(1.5, &anim, 0, true, "");
            let n = s.count((0, 0, CARD_W, CARD_H), spec.color);
            assert!(n > prev, "tick {t} 的扩散面积没变大：{n} ≤ {prev}");
            prev = n;
        }
        // 扩散面积单调到"几乎铺满卡片"，但末tick 仍留得出边框
        assert!(prev > 0);
    }

    /// burst 的扩散块**会长到盖住 footer 那条带**，而那一行的字必须仍然读得出。
    ///
    /// 出图过眼抓到的（判据①，机器门禁全绿）：N 档 `elapsed=2` 时浅灰块已经埋住"余"字整字，
    /// 而 `FG_DIM` 压 N 灰只有 **1.4:1** ⇒ 玩家看到的是"余额"变成"额"。`ink_on` 在这里
    /// **结构上救不了**：同一个字一半在块内一半在块外，任何单一墨色都必错一半 ⇒ 正解是把
    /// "字底下的颜色"重新变成已知量（[`draw_in_plated`]）。
    ///
    /// 两条断言缺一不可：① **几何前提**（真有某些 tick 盖住 footer）—— 少了它，这条会在
    /// "版式改了、burst 再也不盖到那里"之后绿着测空气（M4 那条"计数器为 0 不等于代码对"）；
    /// ② 逐 tick × 逐档，footer 右段的字都朝 `FG_DIM`/`BG` 这一对混合，而不是朝档位色。
    #[test]
    fn footer_text_keeps_a_known_background_during_the_burst() {
        let f = menu::footer().as_rect();
        let (cx, cy) = (CARD_W / 2, CARD_H / 2);
        let seg = (f.x + f.w / 2, f.y, f.w / 2, f.h);
        let mut covered = 0;
        for &r in &Rarity::ALL {
            let spec = menu::anim_spec(r, Motion::Full);
            for t in 0..spec.burst_ticks {
                let rad = menu::burst_radius(t, spec);
                let covers = cx + rad >= f.x + f.w * 3 / 4 && cy + rad >= f.y + f.h / 2;
                covered += covers as u32;
                let s = Shot::at(
                    1.5,
                    &MenuAnim::Burst {
                        rarity: r,
                        elapsed: t,
                        lines: vec!["A".to_string()],
                    },
                    1_234,
                    false,
                    "",
                );
                assert!(
                    s.blends_toward(seg, BG, FG_DIM) > 20,
                    "{r} tick {t}（盖住 footer = {covers}）：余额那支字没有朝 FG_DIM/BG 混合的像素 \
                     ⇒ 底板没画、或字被色块吃掉了"
                );
            }
        }
        assert!(
            covered > 0,
            "没有任何一个 burst tick 盖到 footer ⇒ 上面那条断言测的是空气（几何前提没了就去改这条）"
        );
    }

    /// 结果卡的三态版式（§4.5）：第 0 行是**满宽档位色带**，其余行走 [`menu::reveal_rows`]。
    /// 旧断言里那条"`y = PAD + i*LINE_STEP`"的等距循环已经不存在了 ⇒ 行的位置只能从
    /// `menu` 的几何函数取，测试和 draw 各算一套就等于没测（上一轮的排版口径散在三处过）。
    #[test]
    fn reveal_prints_every_report_line_and_stops_at_the_budget() {
        let lines: Vec<String> = (0..menu::MAX_REPORT_LINES)
            .map(|i| format!("LINE {i} 1234567"))
            .collect();
        let best = Rarity::SR;
        let spec = menu::anim_spec(best, Motion::Full);
        let s = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: menu::SSR_FLASH_TICKS,
                lines,
                best,
            },
            0,
            true,
            "",
        );
        // 色带：档色画满 + 标题用**带色算出来的墨**（不是常量，也不是 `spec.color` 本身）
        let band = menu::band();
        assert!(
            s.count((PAD, band.y + 2, CARD_W - 2 * PAD, 2), spec.color) > 0,
            "档位色带没画出色"
        );
        assert!(
            s.count((PAD, band.y, CARD_W - 2 * PAD, band.h), ink_on(spec.color)) > 0,
            "色带上的字不是 `ink_on(带色)` ⇒ 墨色又是常量了"
        );
        let rows = menu::reveal_rows();
        assert_eq!(rows.len(), menu::MAX_REPORT_LINES - 1, "带 + 明细 = 全部行");
        for (i, row) in rows.iter().enumerate() {
            // 查**正文色**的像素而不是 `has_ink`：行底本身就是"有东西"，用它等于什么都没测。
            assert!(
                s.count((row.x + 2, row.y, row.w - 4, row.h), FG) > 0,
                "结果卡明细第 {i} 行没有正文墨色像素 ⇒ 那行字没画出来（行数预算和卡片高度脱钩了）"
            );
            assert!(
                s.count((row.x + 2, row.y + 2, row.w - 4, row.h - 4), ROW) > 0,
                "明细第 {i} 行没有行底 ⇒ 版式没真的换"
            );
        }
        // 超预算的行不该被画（越界不 panic，也不许挤爆卡片）
        let too_many: Vec<String> = (0..menu::MAX_REPORT_LINES + 2)
            .map(|i| format!("L{i}         "))
            .collect();
        let s2 = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: menu::SSR_FLASH_TICKS,
                lines: too_many,
                best: Rarity::N,
            },
            0,
            true,
            "",
        );
        // 探测带 = **最后一行底边到 footer 上沿**那条空缝。拿 `MAX_REPORT_LINES` 行往下探会量到
        // footer 那一行合法的余额字，于是"截断了"和"没截断"给出同一个结果 —— 门禁自己骗自己。
        let last = rows.last().unwrap();
        let y_over = last.y + last.h;
        assert!(
            y_over < menu::FOOTER_Y,
            "预算底边 {y_over} 已经过了 footer 上沿 {} ⇒ 探测带没有干净的地面可量",
            menu::FOOTER_Y
        );
        assert!(
            !s2.has_ink((PAD, y_over, CARD_W - 2 * PAD, menu::FOOTER_Y - y_over)),
            "结果卡画穿了行数预算 ⇒ 越界的行没被丢掉"
        );
    }

    /// 白闪那一帧必须是**剪影内整块过色**（只画个框等于没闪），且只有头几 tick。
    ///
    /// ⚠️ 谓词换成了 [`Shot::all_in_plate`] 而不是旧的 `all_color`：圆角之后"画布每一格都
    /// 是这个色"永远不成立，于是**负向**那两条（"闪完了""非 SSR 不闪"）会因为四角透明而
    /// 白送通过 —— 门禁自己骗自己的形状。换成"剪影内全过色"之后，正负两条量的是同一件事。
    #[test]
    fn ssr_flash_fills_the_plate_then_gone() {
        let flash = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: 0,
                lines: vec!["SSR!".to_string()],
                best: Rarity::SSR,
            },
            0,
            false,
            "",
        );
        assert!(
            flash.all_in_plate(FLASH),
            "SSR 白闪必须整卡过白（只画白框等于没闪）"
        );
        let after = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: menu::SSR_FLASH_TICKS,
                lines: vec!["SSR!".to_string()],
                best: Rarity::SSR,
            },
            0,
            false,
            "",
        );
        assert!(
            !after.all_in_plate(FLASH),
            "白闪只有头几 tick ⇒ 闪完之后还整卡过色"
        );
        let r = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: 0,
                lines: vec!["R!".to_string()],
                best: Rarity::R,
            },
            0,
            false,
            "",
        );
        assert!(!r.all_in_plate(FLASH), "非 SSR 档不该白闪");
    }

    /// 判据②（"可访问性先于音效"）的**像素**证明，不是参数证明：
    /// `reduce`/`off` 下 SSR 结果卡的**第 0 tick** 必须与"full 档闪完之后那一帧"逐字节相同
    /// ⇒ 过白那一帧真的没了，而不是"标志位读了但还照着旧样子画"。
    /// `paint_reveal` 不读 `elapsed`，所以这两帧相等是唯一可能的等式（也正因此这条抓的是闪烁窗口）。
    #[test]
    fn motion_tiers_remove_the_white_flash_pixels() {
        let lines = vec!["SSR!".to_string()];
        let past = Shot::at(
            1.5,
            &MenuAnim::Reveal {
                elapsed: menu::SSR_FLASH_TICKS,
                lines: lines.clone(),
                best: Rarity::SSR,
            },
            0,
            false,
            "",
        );
        for motion in [Motion::Reduce, Motion::Off] {
            let head = Shot::with_motion(
                1.5,
                &MenuAnim::Reveal {
                    elapsed: 0,
                    lines: lines.clone(),
                    best: Rarity::SSR,
                },
                0,
                false,
                "",
                motion,
            );
            assert_eq!(
                head.canvas, past.canvas,
                "{motion:?} 档在 SSR 结果卡头一帧仍画出了别的东西（白闪没真的关掉）"
            );
        }
        // 反面对照：同一帧在 full 档必须**整卡过白**，否则上面那个等式可能只是"两边都没画"。
        let full = Shot::with_motion(
            1.5,
            &MenuAnim::Reveal {
                elapsed: 0,
                lines: lines.clone(),
                best: Rarity::SSR,
            },
            0,
            false,
            "",
            Motion::Full,
        );
        assert!(
            full.all_in_plate(FLASH),
            "full 档的白闪必须还在（默认观感不许被这轮改掉）"
        );
        assert_ne!(
            full.canvas, past.canvas,
            "full 与 reduce 必须真的画出不同的东西"
        );
    }

    #[test]
    fn error_line_is_drawn_and_truncated_not_dropped() {
        let clean = Shot::at(1.0, &MenuAnim::Menu, 0, true, "");
        let bad = Shot::at(1.0, &MenuAnim::Menu, 0, true, "DB LOCKED");
        assert_ne!(
            clean.canvas, bad.canvas,
            "错误行必须改变像素（它出错时这是唯一的声音）"
        );
        // 错误现在住在 footer 那条带里（左对齐），带高由字体行高定，不再是 5×7 点阵的算术。
        // 查**错误色**的像素而不是 `has_ink`：同一条带的右端本来就有语言行 ⇒ "有墨"永远成立，
        // 那一半断言等于没测（上一轮的空前缀 bug 正是躲在这种"总有东西"的谓词后面）。
        let f = menu::footer();
        assert!(
            bad.blends_toward((f.x, f.y, f.w / 2, f.h), BG, ERR_FG) > 20,
            "footer 左半没有朝错误色混合的像素 ⇒ 错误行没画出来"
        );
        let long = Shot::at(
            1.0,
            &MenuAnim::Menu,
            0,
            true,
            "A VERY LONG STORE ERROR THAT WILL NOT FIT THE CARD",
        );
        assert_ne!(long.canvas, bad.canvas, "超长错误要截断后仍然画出来");
        assert!(long.silhouette_ok());
    }

    /// **没有错误 ⇒ footer 左半必须一个像素都不动**（连"抽取失败："这个空前缀都不许出现）。
    /// 这条是出图过眼抓到的：`line_error(lang, "")` 只剩前缀，每张正常卡左下角挂一行红字，
    /// 而上面那条 `error_line_is_drawn_and_truncated_not_dropped` 只断言"有错误时 footer 有墨"
    /// ⇒ 它抓不到"没错误时也有墨"。判据①的教科书案例。
    #[test]
    fn no_error_never_prints_the_failure_prefix() {
        for &scale in &[1.0f32, 1.5, 2.0] {
            for &lang in &Lang::ALL {
                let clean = Shot::at_lang(scale, &MenuAnim::Menu, 0, true, "", Motion::Full, lang);
                let f = menu::footer();
                assert!(
                    !clean.has_ink((f.x, f.y, f.w / 2, f.h)),
                    "{lang:?}/{scale}×：没有错误却在 footer 左半画了东西（空前缀）"
                );
                let bad = Shot::at_lang(scale, &MenuAnim::Menu, 0, true, "x", Motion::Full, lang);
                assert!(
                    bad.has_ink((f.x, f.y, f.w / 2, f.h)),
                    "{lang:?}/{scale}×：真出错了左半却没画 ⇒ 卡片不会报告失败"
                );
            }
        }
    }

    /// 错误 + 语言行同一条带的分工：错误截到"右段让出来的"预算内，**语言行一个字都不许被压**。
    /// 这条是 `paint_footer` 里那个 `budget` 减法的存在理由 —— 少了它，超长错误会把语言行
    /// 吃掉，而语言行是"用户自己设语言"唯一的入口（出事后反而没法换语言）。
    /// 判据 = 与"没有错误"那张图的右端**逐像素相同**，不是"边界那一列是不是底色"：
    /// 后者要拿舍入后的列号猜边界，前者没有可猜的东西。
    #[test]
    fn long_error_never_eats_the_language_row() {
        let bad = Shot::at(
            1.5,
            &MenuAnim::Menu,
            1200,
            true,
            "THIS STORE ERROR IS DELIBERATELY ABSURDLY LONG SO IT MUST BE CUT",
        );
        let clean = Shot::at(1.5, &MenuAnim::Menu, 1200, true, "");
        let f = menu::footer();
        // 右端那一块 = 语言行整句（标签 + 该语言自名），+1 px 是 `ceil` 的余量
        let row = menu::MenuItem::Lang.label(Lang::Zh);
        let band = (
            f.x + f.w - clean.w_log(&row) - 1,
            f.y,
            clean.w_log(&row) + 1,
            f.h,
        );
        assert!(bad.has_ink(band), "语言行没画出来");
        assert_eq!(
            bad.pixels(band).collect::<Vec<_>>(),
            clean.pixels(band).collect::<Vec<_>>(),
            "超长错误压进了语言行 ⇒ 预算没给右段让位"
        );
        // 错误确实还在（同一条带的左端）：整带换了像素，但右端那一块没换
        assert_ne!(
            bad.canvas, clean.canvas,
            "错误行一个字都没画 ⇒ 出事没人知道"
        );
    }

    #[test]
    fn pathological_scale_and_canvas_only_clip_never_panic() {
        // 合成器给 4×、或池给了个 1 像素的画布：只能裁剪，不能炸（硬约束 3）
        let big = Shot::at(4.0, &MenuAnim::Menu, 7, true, "E");
        assert!(big.silhouette_ok());
        let mut tiny = vec![0u8; 4];
        let ctx = Ctx {
            balance: 7,
            dry_run: true,
            error: "E",
            scale: 1.0,
            motion: Motion::Full,
            lang: Lang::Zh,
        };
        paint(&mut tiny, 1, 1, 4, &MenuAnim::Menu, ctx);
        let mut zero: Vec<u8> = vec![];
        paint(&mut zero, 0, 0, 4, &MenuAnim::Menu, ctx);
    }

    /// 目录里**真实**的前 10 件名字（中文）配上给定的档位与新旧序列。
    ///
    /// 出图必须走 `app.rs` 那条真实路径（`catalog.find(..).name`），不能再喂英文 slug ——
    /// 喂 slug 的图看着很干净，恰恰因为真正会顶到边的东西没上屏。
    fn ten_items(rs: [(Rarity, bool); 10]) -> Vec<(String, Rarity, bool)> {
        let catalog = petcore::gacha::Catalog::default_v1().expect("内嵌目录读不出来");
        catalog
            .items
            .iter()
            .zip(rs)
            .map(|(i, (r, n))| (i.name.clone(), r, n))
            .collect()
    }

    #[test]
    fn dump_uses_real_catalog_names_that_need_the_font() {
        // 这条不是出图，而是把"出图用的那份目录数据"钉在 CI 里：`ten_items` 喂给
        // `#[ignore]` 的出图用例，而它取的名字必须是**非 ASCII** —— 目录哪天改成单语英文，
        // 出图看着仍然很干净，只是再也量不到 CJK 的宽度（§7-R12 那条取舍就悄悄没了）。
        let items = ten_items([(Rarity::N, false); 10]);
        assert_eq!(items.len(), 10);
        assert!(
            items.iter().all(|(n, _, _)| !n.is_ascii()),
            "目录名字全变 ASCII 了 ⇒ 出图测不到 CJK 宽度：{items:?}"
        );
    }

    /// 目视核对（M2 的教训：数字门禁看不见"少了一块"）：
    /// `OPET_CARD_DUMP=1 cargo test -p overlay-wayland --lib -- card::tests::dump_each_phase --nocapture`
    /// 把每个相位写成 /tmp/opet_card_*.ppm，再 `magick` 转 PNG 过眼。
    #[test]
    #[ignore = "要 OPET_CARD_DUMP=1，且图必须人看"]
    fn dump_each_phase_for_eyeball() {
        if std::env::var("OPET_CARD_DUMP").is_err() {
            return;
        }
        let dump = |name: &str, scale: f32, anim: &MenuAnim, balance: i64, dry: bool, err: &str| {
            let s = Shot::at(scale, anim, balance, dry, err);
            // 放大 3 倍出图：16 逻辑 px 的 CJK 笔画在原始尺寸下人眼读不出（判据①要的是
            // "看得清笔画"，不是"看见有字"）
            let (k, ow, oh) = (3i32, s.bw * 3, s.bh * 3);
            let mut big = vec![0u8; (ow * oh * 3) as usize];
            for yy in 0..oh {
                for xx in 0..ow {
                    let src = ((yy / k) as usize) * s.stride + ((xx / k) as usize) * 4;
                    let d = ((yy as usize) * ow as usize + xx as usize) * 3;
                    // BGRA → RGB（卡片全不透明，不需要预乘还原）
                    big[d] = s.canvas[src + 2];
                    big[d + 1] = s.canvas[src + 1];
                    big[d + 2] = s.canvas[src];
                }
            }
            let path = format!("/tmp/opet_card_{name}.ppm");
            let mut bytes = format!("P6\n{ow} {oh}\n255\n").into_bytes();
            bytes.extend_from_slice(&big);
            std::fs::write(&path, bytes).unwrap();
            println!("{path}");
        };
        let ten = menu::report_lines(
            &[7, 2, 1, 0],
            &ten_items([
                (Rarity::N, false),
                (Rarity::N, true),
                (Rarity::N, false),
                (Rarity::N, true),
                (Rarity::N, false),
                (Rarity::N, false),
                (Rarity::N, true),
                (Rarity::R, false),
                (Rarity::R, true),
                (Rarity::SR, false),
            ]),
            42,
            Lang::Zh,
            1.5,
        );
        let ssr = menu::report_lines(
            &[6, 3, 0, 1],
            &ten_items([
                (Rarity::N, true),
                (Rarity::N, false),
                (Rarity::N, true),
                (Rarity::N, false),
                (Rarity::N, true),
                (Rarity::N, false),
                (Rarity::N, true),
                (Rarity::R, false),
                (Rarity::R, true),
                (Rarity::SSR, false),
            ]),
            160,
            Lang::Zh,
            1.5,
        );
        dump("menu_dry", 1.5, &MenuAnim::Menu, 1200, true, "");
        dump("menu_real", 1.5, &MenuAnim::Menu, 87, false, "");
        dump(
            "menu_err",
            1.5,
            &MenuAnim::Menu,
            0,
            true,
            "DB LOCKED BY ANOTHER OPEN",
        );
        for t in [0u32, 3, 8, 16, 23] {
            dump(
                &format!("burst_ssr_{t:02}"),
                1.5,
                &MenuAnim::Burst {
                    rarity: Rarity::SSR,
                    elapsed: t,
                    lines: ssr.clone(),
                },
                0,
                true,
                "",
            );
        }
        dump(
            "burst_n",
            1.5,
            &MenuAnim::Burst {
                rarity: Rarity::N,
                elapsed: 2,
                lines: ten.clone(),
            },
            0,
            true,
            "",
        );
        for (name, elapsed, lines, best) in [
            ("flash_ssr", 0u32, ssr.clone(), Rarity::SSR),
            ("reveal_ssr", 3, ssr.clone(), Rarity::SSR),
            ("reveal_ten", 3, ten.clone(), Rarity::SR),
        ] {
            dump(
                name,
                1.5,
                &MenuAnim::Reveal {
                    elapsed,
                    lines,
                    best,
                },
                140,
                true,
                "",
            );
        }
        dump(
            "reveal_1x",
            1.0,
            &MenuAnim::Reveal {
                elapsed: 3,
                lines: ssr.clone(),
                best: Rarity::SSR,
            },
            140,
            true,
            "",
        );
    }
}
