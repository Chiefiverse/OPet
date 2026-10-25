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
//! `×scale`。字号也按 logical 定（[`menu::TEXT_PX`]）：1× 屏 2 物理像素、1.5× 屏 3 物理像素，
//! 于是"20 个字符够不够宽"这类预算只需要证一次。

use petcore::gacha::Rarity;

use crate::font;
use crate::menu::{self, CARD_H, CARD_W, LINE_STEP, MenuAnim, PAD, ROW_H, RectI, TEXT_PX};

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
fn text_px(scale: f32) -> i32 {
    px(TEXT_PX, scale).max(1)
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
}

/// 画一帧卡片。`bw`/`bh` 是物理宽高，`stride` 是每行字节数（= bw×4）。
///
/// `dry_run` **不进** `menu::content_sig`：它是每进程恒定的量（不给 `--db` 就是内存库），
/// 所以签名相等时不可能"一次是演示、另一次是真档" ⇒ 漏了它不会让静止判定错过像素变化。
pub fn paint(canvas: &mut [u8], bw: i32, bh: i32, stride: usize, anim: &MenuAnim, ctx: Ctx) {
    let scale = ctx.scale;
    canvas.fill(0);
    fill(canvas, stride, 0, 0, bw, bh, bgra(BG));
    let border = if menu::white_flash(anim) { FLASH } else { EDGE };
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
            rarity,
            elapsed,
            lines,
        } => {
            paint_burst(canvas, stride, *rarity, *elapsed, lines.len(), ctx);
            paint_error(canvas, stride, ctx);
        }
        MenuAnim::Reveal { lines, best, .. } => {
            if menu::white_flash(anim) {
                // SSR 的"闪一下"必须是**看不到内容**的整卡过白（只画个白框等于没闪）。
                fill(canvas, stride, 0, 0, bw, bh, bgra(FLASH));
                return;
            }
            paint_reveal(canvas, stride, lines, *best, ctx);
            paint_error(canvas, stride, ctx);
        }
    }
}

/// 余额/演示标行。字模只认 ASCII，且金额不许被截断（读错钱比看不见更糟）。
fn header(balance: i64, dry_run: bool) -> String {
    if dry_run {
        format!("DRYRUN  SHARDS {balance}")
    } else {
        format!("SHARDS {balance}")
    }
}

fn paint_menu(canvas: &mut [u8], stride: usize, ctx: Ctx) {
    let scale = ctx.scale;
    let ts = text_px(scale);
    font::draw_str(
        canvas,
        stride,
        px(PAD, scale),
        px(PAD, scale),
        ts,
        if ctx.dry_run { WARN } else { FG_DIM },
        &header(ctx.balance, ctx.dry_run),
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
        stroke(
            canvas,
            stride,
            RectI::new(r.x, r.y, r.w, r.h),
            scale,
            BTN_EDGE,
        );
        // 垂直居中：字模 7 高 × logical 字号 ⇒ 行内上边距 (ROW_H - 14)/2
        let ty = r.y + (ROW_H - font::GLYPH_H * TEXT_PX).max(0) / 2;
        font::draw_str(
            canvas,
            stride,
            px(r.x + PAD / 2, scale),
            px(ty, scale),
            ts,
            FG,
            r.item.label(),
        );
    }
    paint_error(canvas, stride, ctx);
}

fn paint_burst(
    canvas: &mut [u8],
    stride: usize,
    rarity: Rarity,
    elapsed: u32,
    line_count: usize,
    ctx: Ctx,
) {
    let scale = ctx.scale;
    let spec = menu::anim_spec(rarity);
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
    // 扩散期也得读得出"在等什么"：中央一行提示，件数从结果行数来。
    let ts = text_px(scale);
    let msg = if line_count > 0 { "GETTING..." } else { "..." };
    let w = font::text_width(msg, ts);
    let (tx, ty) = (
        px(cx, scale) - w / 2,
        px(cy - font::GLYPH_H * TEXT_PX / 2, scale),
    );
    // 提示底下先垫一块档位主色的"牌子"：扩散刚开始时方块比这行字小，
    // 没有牌子的话深墨会有一半压在卡片底色上 ⇒ 深字压深底，同样读不出。
    let pad = ts.max(2);
    fill(
        canvas,
        stride,
        tx - pad,
        ty - pad,
        w + pad * 2,
        font::GLYPH_H * ts + pad * 2,
        bgra(spec.color),
    );
    font::draw_str(canvas, stride, tx, ty, ts, ink_on(spec.color), msg);
}

fn paint_reveal(canvas: &mut [u8], stride: usize, lines: &[String], best: Rarity, ctx: Ctx) {
    let scale = ctx.scale;
    let ts = text_px(scale);
    let spec = menu::anim_spec(best);
    for (i, l) in lines.iter().enumerate().take(menu::MAX_REPORT_LINES) {
        let y = PAD + (i as i32) * LINE_STEP;
        // 第一行是标题（含最高档）⇒ 用该档主色，其余走正文色：SSR 的金色要一眼可辨
        let color = if i == 0 { spec.color } else { FG };
        font::draw_str(canvas, stride, px(PAD, scale), px(y, scale), ts, color, l);
    }
}

fn paint_error(canvas: &mut [u8], stride: usize, ctx: Ctx) {
    if ctx.error.is_empty() {
        return;
    }
    let scale = ctx.scale;
    let y = CARD_H - PAD - font::GLYPH_H * TEXT_PX;
    font::draw_str(
        canvas,
        stride,
        px(PAD, scale),
        px(y, scale),
        text_px(scale),
        ERR_FG,
        &menu::fit(ctx.error),
    );
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
        fn at(scale: f32, anim: &MenuAnim, balance: i64, dry: bool, err: &str) -> Self {
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
            let spec = menu::anim_spec(r);
            let ink = ink_on(spec.color);
            assert_ne!(ink, spec.color, "{r}：墨色和块同色 ⇒ 提示必然看不见");
            for t in [0u32, spec.burst_ticks / 2] {
                let anim = MenuAnim::Burst {
                    rarity: r,
                    elapsed: t,
                    lines: vec!["A".to_string()],
                };
                let s = Shot::at(1.5, &anim, 0, true, "");
                // 提示的包围盒（logical）：字宽 = 10 字符 × STEP × logical 字号，垂直居中于卡心
                let half_w = 10 * font::STEP * TEXT_PX / 2;
                let want = bgra(ink);
                let n = s
                    .pixels((
                        CARD_W / 2 - half_w,
                        CARD_H / 2 - font::GLYPH_H * TEXT_PX / 2,
                        half_w * 2,
                        font::GLYPH_H * TEXT_PX,
                    ))
                    .filter(|p| *p == want)
                    .count();
                assert!(
                    n > 20,
                    "{r} 在 tick {t} 的提示区里只有 {n} 个墨色像素 ⇒ 这行字没画出来"
                );
            }
        }
    }

    #[test]
    fn burst_block_grows_tick_by_tick() {
        let spec = menu::anim_spec(Rarity::SSR);
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
        let y_over = PAD + menu::MAX_REPORT_LINES as i32 * LINE_STEP;
        assert!(!s2.has_ink((PAD, y_over, CARD_W - 2 * PAD, 10)));
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

    #[test]
    fn error_line_is_drawn_and_truncated_not_dropped() {
        let clean = Shot::at(1.0, &MenuAnim::Menu, 0, true, "");
        let bad = Shot::at(1.0, &MenuAnim::Menu, 0, true, "DB LOCKED");
        assert_ne!(
            clean.canvas, bad.canvas,
            "错误行必须改变像素（它出错时这是唯一的声音）"
        );
        let y = CARD_H - PAD - font::GLYPH_H * TEXT_PX;
        assert!(bad.has_ink((PAD, y, CARD_W - 2 * PAD, 10)));
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
        };
        paint(&mut tiny, 1, 1, 4, &MenuAnim::Menu, ctx);
        let mut zero: Vec<u8> = vec![];
        paint(&mut zero, 0, 0, 4, &MenuAnim::Menu, ctx);
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
            // 放大 3 倍出图：5×7 字模在原始尺寸下人眼读不出笔画
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
            &[
                ("cat_shiro".to_string(), Rarity::N, false),
                ("cat_mochi".to_string(), Rarity::N, true),
                ("cat_tofu".to_string(), Rarity::N, false),
                ("hat_red".to_string(), Rarity::N, true),
                ("hat_blue".to_string(), Rarity::N, false),
                ("scarf_y".to_string(), Rarity::N, false),
                ("bow_a".to_string(), Rarity::N, true),
                ("bell".to_string(), Rarity::R, false),
                ("ribbon".to_string(), Rarity::R, true),
                ("wing".to_string(), Rarity::SR, false),
            ],
            42,
            true,
        );
        let ssr = menu::report_lines(
            &[6, 3, 0, 1],
            &[
                ("cat_shiro".to_string(), Rarity::N, true),
                ("cat_mochi".to_string(), Rarity::N, false),
                ("cat_tofu".to_string(), Rarity::N, true),
                ("hat_red".to_string(), Rarity::N, false),
                ("hat_blue".to_string(), Rarity::N, true),
                ("scarf_y".to_string(), Rarity::N, false),
                ("bow_a".to_string(), Rarity::N, true),
                ("bell".to_string(), Rarity::R, false),
                ("ribbon".to_string(), Rarity::R, true),
                ("crown_gold".to_string(), Rarity::SSR, false),
            ],
            160,
            true,
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
