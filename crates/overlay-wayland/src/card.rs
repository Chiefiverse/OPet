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
use crate::menu::{self, CARD_H, CARD_W, FOOTER_GAP, LINE_STEP, MenuAnim, PAD, RectI, TEXT_PX};
use crate::settings::Motion;
use crate::text;

/// 卡片底色（0xRRGGBB，全不透明 ⇒ 预乘等于原值）。
const BG: u32 = 0x1B_1E_23;
/// 卡片描边
const EDGE: u32 = 0x3A_40_48;
/// 按钮底与按钮描边
const BTN: u32 = 0x2A_30_38;
const BTN_EDGE: u32 = 0x4A_52_5C;
/// 正文与次要文字
const FG: u32 = 0xE6_E9_EE;
const FG_DIM: u32 = 0x9A_A0_A6;
/// DRYRUN 标：用 SSR 的金，一眼和真档区分开（内存库不写盘，这条必须显眼）
const WARN: u32 = 0xF5_B3_01;
/// 错误行（库打不开、存档写不进去都靠它出声，不 panic 也不退出）
const ERR_FG: u32 = 0xE5_48_4D;
/// SSR 白闪
const FLASH: u32 = 0xFF_FF_FF;

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
/// 不能炸进程。CI 那边 `text.rs::the_shipped_font_loads_and_covers_the_list` 强制它成功，
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
    let (x0, y0, h) = (px(r.x, scale), px(r.y, scale), px(r.h, scale));
    let top = y0 + ((h - line_h(scale)) / 2).max(0);
    // 右边界用 `px(x + w)`（一次舍入）而不是 `px(x) + px(w)`（两次）⇒ 与 `menu::footer()`
    // 的矩形、`hit_test` 的命中区同一条边线，差 1 px 就会出现"点得到却看着没贴齐"。
    let right = px(r.x + r.w, scale);
    let x = match align {
        Align::Left => x0,
        Align::Right => (right - line_w(s, scale)).max(x0),
    };
    draw_line(canvas, stride, x, top, scale, rgb, s);
}

/// 压在某块底色上的文字该用什么墨色（Rec.601 加权亮度，整数）。
///
/// 不这么做的话 SSR 的金与 N 的浅灰上就是**白字压亮块**：像素门禁只看到"这块画了东西"，
/// 于是全绿，而人眼读不出 —— 目视核对这一步唯一抓到的缺陷，所以这条判据逐档有测试钉住。
fn ink_on(rgb: u32) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
    if (r * 299 + g * 587 + b * 114) / 1000 > 140 {
        BG
    } else {
        FG
    }
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

/// 描一个 **logical** 矩形的 1 px 框（上下横线 + 左右竖线，线宽也按 logical→物理换算）。
fn stroke(canvas: &mut [u8], stride: usize, r: RectI, scale: f32, rgb: u32) {
    let t = px(1, scale).max(1);
    let (x, y, w, h) = (
        px(r.x, scale),
        px(r.y, scale),
        px(r.w, scale),
        px(r.h, scale),
    );
    let c = bgra(rgb);
    fill(canvas, stride, x, y, w, t, c);
    fill(canvas, stride, x, y + h - t, w, t, c);
    fill(canvas, stride, x, y, t, h, c);
    fill(canvas, stride, x + w - t, y, t, h, c);
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
/// `dry_run` 与 `motion` 都**不进** `menu::content_sig`：两者都是每进程恒定的量（不给
/// `--db` 就是内存库；`--motion` 没有运行中的 UI 入口能改），所以签名相等时不可能出现
/// "一次是演示另一次是真档"或"一次闪另一次不闪" ⇒ 漏了它们不会让静止判定错过像素变化。
pub fn paint(canvas: &mut [u8], bw: i32, bh: i32, stride: usize, anim: &MenuAnim, ctx: Ctx) {
    let scale = ctx.scale;
    canvas.fill(0);
    fill(canvas, stride, 0, 0, bw, bh, bgra(BG));
    let border = if menu::white_flash(anim, ctx.motion) {
        FLASH
    } else {
        EDGE
    };
    stroke(
        canvas,
        stride,
        RectI::new(0, 0, CARD_W, CARD_H),
        scale,
        border,
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
            if menu::white_flash(anim, ctx.motion) {
                // SSR 的"闪一下"必须是**看不到内容**的整卡过白（只画个白框等于没闪）。
                fill(canvas, stride, 0, 0, bw, bh, bgra(FLASH));
                return;
            }
            paint_reveal(canvas, stride, lines, *best, ctx);
            paint_footer(canvas, stride, anim, ctx);
        }
    }
}

/// 菜单态：标题行（左＝本语言的"盲盒"，右＝碎片 + 演示标）、三行按钮、底部语言行。
///
/// 标题行右端的徽标（演示）来自 [`l10n::header_shards`]：渲染层一个字面量都不写，否则就
/// 绕过了"会渲染的 ⊆ 清单 ⊆ 真 cmap"那三条字体门禁（§4.2）。徽标放标题行而不是单独一行，
/// 是给英文那种宽标签留出预算（§7-R13）。
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
        fill(
            canvas,
            stride,
            px(r.x, scale),
            px(r.y, scale),
            px(r.w, scale),
            px(r.h, scale),
            bgra(BTN),
        );
        stroke(canvas, stride, r.as_rect(), scale, BTN_EDGE);
        draw_in(
            canvas,
            stride,
            RectI::new(r.x + PAD / 2, r.y, r.w - PAD, r.h),
            scale,
            FG,
            &r.item.label(ctx.lang),
            Align::Left,
        );
    }
    paint_footer(canvas, stride, &MenuAnim::Menu, ctx);
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
        draw_in(
            canvas,
            stride,
            f.as_rect(),
            scale,
            ERR_FG,
            &err,
            Align::Left,
        );
    }
    draw_in(
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

fn paint_reveal(canvas: &mut [u8], stride: usize, lines: &[String], best: Rarity, ctx: Ctx) {
    let scale = ctx.scale;
    let spec = menu::anim_spec(best, ctx.motion);
    for (i, l) in lines.iter().enumerate().take(menu::MAX_REPORT_LINES) {
        let y = PAD + (i as i32) * LINE_STEP;
        // 第一行是标题（含最高档）⇒ 用该档主色，其余走正文色：SSR 的金色要一眼可辨。
        // 版式重排（提交 6）会把它换成满宽档位色带 + `ink_on`，这一轮**先不动**，
        // 以免"文字层"和"版式"两件事的像素差异混在一个提交里说不清。
        let r = RectI::new(PAD, y, CARD_W - 2 * PAD, LINE_STEP);
        let color = if i == 0 { spec.color } else { FG };
        draw_in(canvas, stride, r, scale, color, l, Align::Left);
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
                    .map(move |xx| {
                        let i = (yy as usize) * self.stride + (xx as usize) * 4;
                        self.canvas
                            .get(i..i + 4)
                            .map(|p| [p[0], p[1], p[2], p[3]])
                            .unwrap_or([0, 0, 0, 0])
                    })
            })
        }

        fn count(&self, r: (i32, i32, i32, i32), rgb: u32) -> u32 {
            let want = bgra(rgb);
            self.pixels(r).filter(|p| *p == want).count() as u32
        }

        /// 这块区域里有没有"画了东西"（既不是透明也不是卡片底色）。
        fn has_ink(&self, r: (i32, i32, i32, i32)) -> bool {
            let bg = bgra(BG);
            self.pixels(r).any(|p| p != bg && p != [0, 0, 0, 0])
        }

        fn all_opaque(&self) -> bool {
            self.canvas.chunks_exact(4).all(|p| p[3] == 255)
        }

        fn all_color(&self, rgb: u32) -> bool {
            let want = bgra(rgb);
            self.canvas.chunks_exact(4).all(|p| p == want.as_slice())
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
    fn card_is_opaque_at_every_scale_and_its_border_is_distinct() {
        for scale in [1.0, 1.5, 2.0] {
            let s = Shot::at(scale, &MenuAnim::Menu, 1200, true, "");
            assert!(!s.canvas.is_empty(), "scale {scale}：画布是空的");
            assert!(s.all_opaque(), "scale {scale}：卡片里有透明像素");
            assert_ne!(EDGE, BG, "描边和底色相同 ⇒ 画没画边框在像素上读不出来");
            assert!(
                s.count((0, 0, CARD_W, 1), EDGE) > 0,
                "scale {scale}：顶边描线没画出来"
            );
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
            // 三行：底 + 字都要有（`layout()` = 只有那三个可点行，footer 另算）
            for r in menu::layout() {
                let band = (r.x, r.y, r.w, r.h);
                assert!(
                    s.has_ink(band),
                    "scale {}：项 {:?} 这一行是空的（字号或行距漂了）",
                    scale,
                    r.item
                );
                assert!(
                    s.count((r.x + 2, r.y + 2, r.w - 4, r.h - 4), BTN) > 0,
                    "scale {scale}：按钮底没落在行矩形里 ⇒ 画的是一整块卡片"
                );
            }
            // 语言行：它**没有按钮底**（footer 直接画在卡片底色上），所以这里只查"画了字"。
            // 这条是"用户能自己设语言"在像素面的唯一证据 —— 矩形在却空白 = 假功能。
            let f = menu::footer();
            assert!(
                s.has_ink((f.x, f.y, f.w, f.h)),
                "scale {scale}：footer 那条带是空的 ⇒ 语言行没画出来"
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

    #[test]
    fn reveal_prints_every_report_line_and_stops_at_the_budget() {
        let lines: Vec<String> = (0..menu::MAX_REPORT_LINES)
            .map(|i| format!("LINE {i} 1234567"))
            .collect();
        let s = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: 5,
                lines,
                best: Rarity::SR,
            },
            0,
            true,
            "",
        );
        for i in 0..menu::MAX_REPORT_LINES {
            let y = PAD + (i as i32) * LINE_STEP;
            assert!(
                s.has_ink((PAD, y, CARD_W - 2 * PAD, 10)),
                "结果卡第 {i} 行是空的 ⇒ 行数预算和卡片高度脱钩了"
            );
        }
        // 超预算的行不该被画（越界不 panic，也不许挤爆卡片）
        let too_many: Vec<String> = (0..menu::MAX_REPORT_LINES + 2)
            .map(|i| format!("L{i}         "))
            .collect();
        let s2 = Shot::at(
            1.0,
            &MenuAnim::Reveal {
                elapsed: 5,
                lines: too_many,
                best: Rarity::N,
            },
            0,
            true,
            "",
        );
        // 超预算的行不该被画（越界不 panic，也不许挤爆卡片）。**探测带必须停在 footer 上沿**：
        // Reveal 相位本来就要在 footer 右槽画余额行（`y = FOOTER_Y`），拿 10 px 往下探会量到
        // 那行合法的墨，于是"截断了"和"没截断"给出同一个结果 —— 门禁自己骗自己。
        let y_over = PAD + menu::MAX_REPORT_LINES as i32 * LINE_STEP;
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

    #[test]
    fn ssr_flash_is_a_full_white_card_then_gone() {
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
            flash.all_color(FLASH),
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
        assert!(!after.all_color(FLASH), "白闪只有头几 tick");
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
        assert!(!r.all_color(FLASH), "非 SSR 档不该白闪");
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
            full.all_color(FLASH),
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
        let f = menu::footer();
        assert!(bad.has_ink((f.x, f.y, f.w, f.h)));
        let long = Shot::at(
            1.0,
            &MenuAnim::Menu,
            0,
            true,
            "A VERY LONG STORE ERROR THAT WILL NOT FIT THE CARD",
        );
        assert_ne!(long.canvas, bad.canvas, "超长错误要截断后仍然画出来");
        assert!(long.all_opaque());
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
        assert!(big.all_opaque());
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
