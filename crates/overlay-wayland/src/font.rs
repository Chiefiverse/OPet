//! 零依赖的 5×7 ASCII 位图字模：开箱菜单与结果卡的**全部**文本渲染。
//!
//! 为什么不用字体库（ab_glyph 之类）：展示内容只有 `item_id`、`N/R/SR/SSR`、数字和
//! `PULL/CLOSE/DRYRUN` 这些 ASCII（`catalog.rs` 生成的 id 就是小写 ASCII），而菜单是
//! overlay 客户端里唯一需要文字的东西 ⇒ 一张 95 项的静态表换掉整条字体依赖链，
//! 并且能像 `atlas.rs`/`region.rs` 一样对 `&mut [u8]` 画布纯单测。
//!
//! 表以**行主序**书写（`ROWS[c][r]` 的低 5 位 = 第 r 行从左到右的 5 个像素），因为那种写法
//! 能直接目视核对；渲染需要列主序（一列一个字节、bit0 = 顶行，`put_pixel` 按位取点），
//! 由 [`glyph_cols`] 在调用点转置。编译期用位断言钉住"行只用 5 位"，越界即红。
//!
//! 内存序与 `atlas.rs` 相同：**BGRA8 预乘**（`Argb8888` 小端）。这里画的是完全不透明
//! 的实色像素，所以"预乘"就是颜色本身；透明像素**什么都不写**（画布必须先清成 0）。

/// 字模覆盖的可打印 ASCII 区间（含端点）：`' '`..`'~'`，共 95 个。
pub const FIRST: u8 = 0x20;
pub const LAST: u8 = 0x7E;
pub const GLYPH_W: i32 = 5;
pub const GLYPH_H: i32 = 7;
/// 字符步进 = 字宽 + 1 列间距。
pub const STEP: i32 = 6;

/// 每个 glyph 7 行、每行低 5 位是从左到右的像素（bit4 = 最左列）。
/// 顺序 = `0x20..=0x7E` 逐字节，写乱一个整张表就错位，所以表长有编译期断言。
#[rustfmt::skip]
const ROWS: [[u8; GLYPH_H as usize]; 95] = [
    // 0x20 ' '      0x21 '!'      0x22 '"'      0x23 '#'      0x24 '$'      0x25 '%'      0x26 '&'      0x27 '\''
    [0,0,0,0,0,0,0],[0,0,0x04,0,0,0,0],[0x0A,0x0A,0,0,0,0,0],[0x1F,0x11,0x1F,0x11,0x1F,0x11,0],[0x07,0x05,0x0E,0x04,0x1C,0x14,0x1C],[0x19,0x19,0x02,0x04,0x08,0x13,0x13],[0x0C,0x12,0x14,0x08,0x15,0x12,0x0D],[0x04,0x04,0x04,0,0,0,0],
    // 0x28 '('      0x29 ')'      0x2A '*'      0x2B '+'      0x2C ','      0x2D '-'      0x2E '.'      0x2F '/'
    [0x02,0x04,0x08,0x08,0x08,0x04,0x02],[0x08,0x04,0x02,0x02,0x02,0x04,0x08],[0x04,0x15,0x0E,0x15,0x04,0,0],[0x04,0x04,0x1F,0x04,0x04,0,0],[0,0,0,0,0x04,0x04,0x08],[0,0,0,0x1F,0,0,0],[0,0,0,0,0x0E,0x0E,0],[0x10,0x10,0x08,0x04,0x02,0x01,0x01],
    // 0x30 '0'      0x31 '1'      0x32 '2'      0x33 '3'      0x34 '4'      0x35 '5'      0x36 '6'      0x37 '7'
    [0x0E,0x11,0x13,0x15,0x19,0x11,0x0E],[0x04,0x0C,0x04,0x04,0x04,0x04,0x0E],[0x0E,0x11,0x01,0x02,0x04,0x08,0x1F],[0x1F,0x02,0x04,0x02,0x01,0x11,0x0E],[0x02,0x06,0x0A,0x12,0x1F,0x02,0x02],[0x1F,0x10,0x1E,0x01,0x01,0x11,0x0E],[0x06,0x08,0x10,0x1E,0x11,0x11,0x0E],[0x1F,0x01,0x02,0x04,0x08,0x08,0x08],
    // 0x38 '8'      0x39 '9'      0x3A ':'      0x3B ';'      0x3C '<'      0x3D '='      0x3E '>'      0x3F '?'
    [0x0E,0x11,0x11,0x0E,0x11,0x11,0x0E],[0x0E,0x11,0x11,0x0F,0x01,0x02,0x0C],[0,0x0C,0x0C,0,0x0C,0x0C,0],[0,0x0C,0x0C,0,0x0C,0x04,0x08],[0x02,0x04,0x08,0x10,0x08,0x04,0x02],[0,0,0x1F,0,0x1F,0,0],[0x10,0x08,0x04,0x02,0x04,0x08,0x10],[0x0E,0x11,0x01,0x02,0x04,0,0x04],
    // 0x40 '@'      0x41 'A'      0x42 'B'      0x43 'C'      0x44 'D'      0x45 'E'      0x46 'F'      0x47 'G'
    [0x0E,0x11,0x17,0x15,0x17,0x10,0x0E],[0x0E,0x11,0x11,0x1F,0x11,0x11,0x11],[0x1E,0x11,0x11,0x1E,0x11,0x11,0x1E],[0x0E,0x11,0x10,0x10,0x10,0x11,0x0E],[0x1C,0x12,0x11,0x11,0x11,0x12,0x1C],[0x1F,0x10,0x10,0x1E,0x10,0x10,0x1F],[0x1F,0x10,0x10,0x1E,0x10,0x10,0x10],[0x0E,0x11,0x10,0x17,0x11,0x11,0x0F],
    // 0x48 'H'      0x49 'I'      0x4A 'J'      0x4B 'K'      0x4C 'L'      0x4D 'M'      0x4E 'N'      0x4F 'O'
    [0x11,0x11,0x11,0x1F,0x11,0x11,0x11],[0x0E,0x04,0x04,0x04,0x04,0x04,0x0E],[0x07,0x02,0x02,0x02,0x02,0x12,0x0C],[0x11,0x12,0x14,0x18,0x14,0x12,0x11],[0x10,0x10,0x10,0x10,0x10,0x10,0x1F],[0x11,0x1B,0x15,0x15,0x11,0x11,0x11],[0x11,0x19,0x15,0x13,0x11,0x11,0x11],[0x0E,0x11,0x11,0x11,0x11,0x11,0x0E],
    // 0x50 'P'      0x51 'Q'      0x52 'R'      0x53 'S'      0x54 'T'      0x55 'U'      0x56 'V'      0x57 'W'
    [0x1E,0x11,0x11,0x1E,0x10,0x10,0x10],[0x0E,0x11,0x11,0x11,0x15,0x12,0x0D],[0x1E,0x11,0x11,0x1E,0x14,0x12,0x11],[0x0F,0x10,0x10,0x0E,0x01,0x01,0x1E],[0x1F,0x04,0x04,0x04,0x04,0x04,0x04],[0x11,0x11,0x11,0x11,0x11,0x11,0x0E],[0x11,0x11,0x11,0x11,0x11,0x0A,0x04],[0x11,0x11,0x11,0x15,0x15,0x15,0x0A],
    // 0x58 'X'      0x59 'Y'      0x5A 'Z'      0x5B '['      0x5C '\\'     0x5D ']'      0x5E '^'      0x5F '_'
    [0x11,0x11,0x0A,0x04,0x0A,0x11,0x11],[0x11,0x11,0x0A,0x04,0x04,0x04,0x04],[0x1F,0x01,0x02,0x04,0x08,0x10,0x1F],[0x07,0x04,0x04,0x04,0x04,0x04,0x07],[0x01,0x01,0x02,0x04,0x08,0x10,0x10],[0x1C,0x04,0x04,0x04,0x04,0x04,0x1C],[0x04,0x0A,0x11,0x00,0x00,0x00,0x00],[0,0,0,0,0,0,0x1F],
    // 0x60 '`'      0x61 'a'      0x62 'b'      0x63 'c'      0x64 'd'      0x65 'e'      0x66 'f'      0x67 'g'
    [0x08,0x04,0x02,0,0,0,0],[0,0,0x0E,0x01,0x0F,0x11,0x0F],[0x10,0x10,0x1C,0x12,0x12,0x12,0x1C],[0,0,0x0F,0x10,0x10,0x10,0x0F],[0x01,0x01,0x0F,0x11,0x11,0x11,0x0F],[0,0x0E,0x11,0x1F,0x10,0x11,0x0E],[0x06,0x09,0x08,0x1C,0x08,0x08,0x08],[0,0x0F,0x11,0x11,0x0F,0x01,0x0E],
    // 0x68 'h'      0x69 'i'      0x6A 'j'      0x6B 'k'      0x6C 'l'      0x6D 'm'      0x6E 'n'      0x6F 'o'
    [0x10,0x10,0x1C,0x12,0x12,0x12,0x12],[0,0x04,0,0x0C,0x04,0x04,0x0E],[0x02,0x02,0,0x06,0x02,0x02,0x0C],[0x10,0x10,0x12,0x14,0x18,0x14,0x12],[0x0C,0x04,0x04,0x04,0x04,0x04,0x0E],[0,0,0x16,0x15,0x15,0x15,0x15],[0,0,0x1C,0x12,0x12,0x12,0x12],[0,0,0x0E,0x11,0x11,0x11,0x0E],
    // 0x70 'p'      0x71 'q'      0x72 'r'      0x73 's'      0x74 't'      0x75 'u'      0x76 'v'      0x77 'w'
    [0,0x1C,0x12,0x12,0x1C,0x10,0x10],[0,0x0F,0x11,0x11,0x0F,0x01,0x01],[0,0,0x1C,0x12,0x10,0x10,0x10],[0,0,0x0F,0x10,0x0E,0x01,0x1E],[0x08,0x08,0x1C,0x08,0x08,0x09,0x06],[0,0,0x12,0x12,0x12,0x13,0x0F],[0,0,0x11,0x11,0x11,0x0A,0x04],[0,0,0x11,0x11,0x15,0x1B,0x11],
    // 0x78 'x'      0x79 'y'      0x7A 'z'      0x7B '{'      0x7C '|'      0x7D '}'      0x7E '~'
    [0,0,0x11,0x0A,0x04,0x0A,0x11],[0,0,0x11,0x11,0x0F,0x01,0x0E],[0,0,0x1F,0x02,0x04,0x08,0x1F],[0x03,0x04,0x04,0x08,0x04,0x04,0x03],[0x04,0x04,0x04,0x04,0x04,0x04,0x04],[0x18,0x04,0x04,0x02,0x04,0x04,0x18],[0,0,0x09,0x12,0,0,0],
];

/// 表长必须覆盖 `FIRST..=LAST` 逐字节；少一个就整张表错位，编译期即红。
const _: () = assert!(ROWS.len() == (LAST - FIRST + 1) as usize);

/// 行主序 → 列主序：`out[c]` 的第 r 位（bit0 = 顶行）= `rows[r]` 的第 `4-c` 位。
const fn glyph_cols(idx: usize) -> [u8; GLYPH_W as usize] {
    let rows = ROWS[idx];
    let mut cols = [0u8; GLYPH_W as usize];
    let mut r = 0;
    while r < GLYPH_H as usize {
        // 行只用低 5 位（bit4..bit0 = 左..右）；越界说明书写错了点阵。
        assert!(rows[r] & !0x1F == 0, "行点阵用了第 6 位以上");
        let mut c = 0;
        while c < GLYPH_W as usize {
            if rows[r] & (1 << (4 - c)) != 0 {
                cols[c] |= 1 << r;
            }
            c += 1;
        }
        r += 1;
    }
    cols
}

/// 列主序字模表（渲染用），由 [`glyph_cols`] 在编译期从行主序源表转置。
static GLYPHS: [[u8; GLYPH_W as usize]; 95] = {
    const fn table() -> [[u8; GLYPH_W as usize]; 95] {
        let mut t = [[0u8; GLYPH_W as usize]; 95];
        let mut i = 0;
        while i < 95 {
            t[i] = glyph_cols(i);
            i += 1;
        }
        t
    }
    table()
};

/// 画一段文本到 BGRA8 画布。`x/y` 与 `scale` 都是同一坐标口径（调用方负责乘缩放），
/// `pitch` 是画布一行的**字节数**。越界像素逐点裁剪（不 panic）；非 ASCII 画成 `'?'`。
pub fn draw_str(canvas: &mut [u8], pitch: usize, x: i32, y: i32, scale: i32, color: u32, s: &str) {
    let scale = scale.max(1);
    for (ci, ch) in s.chars().enumerate() {
        let glyph = glyph_for(ch);
        let gx = x + (ci as i32) * STEP * scale;
        for (col, &bits) in glyph.iter().enumerate() {
            for row in 0..GLYPH_H as usize {
                if bits & (1 << row) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        put_pixel(
                            canvas,
                            pitch,
                            gx + col as i32 * scale + sx,
                            y + row as i32 * scale + sy,
                            color,
                        );
                    }
                }
            }
        }
    }
}

/// 文本宽度（与 `scale` 同口径，含末尾间距）。
pub fn text_width(s: &str, scale: i32) -> i32 {
    s.chars().count() as i32 * STEP * scale.max(1)
}

fn glyph_for(ch: char) -> &'static [u8; GLYPH_W as usize] {
    match u8::try_from(ch) {
        Ok(b) if (FIRST..=LAST).contains(&b) => &GLYPHS[(b - FIRST) as usize],
        // 契约：菜单文本都是我们自己拼的 ASCII；真出现非 ASCII（目录被改坏）画 '?' 而不是崩。
        _ => &GLYPHS[(b'?' - FIRST) as usize],
    }
}

fn put_pixel(canvas: &mut [u8], pitch: usize, x: i32, y: i32, color: u32) {
    if x < 0 || y < 0 {
        return;
    }
    let off = y as usize * pitch + x as usize * 4;
    let Some(px) = canvas.get_mut(off..off + 4) else {
        return;
    };
    // 0xRRGGBB → BGRA 预乘（alpha = 255，实色）。
    let (b, g, r) = (color & 0xFF, (color >> 8) & 0xFF, (color >> 16) & 0xFF);
    px[0] = b as u8;
    px[1] = g as u8;
    px[2] = r as u8;
    px[3] = 255;
}

/// 测试用：把某个字节画成 7 行 `#`/`.` 文本，出问题时直接对照位阵读。
#[cfg(test)]
pub fn ascii_art(b: u8) -> String {
    let idx = (b.saturating_sub(FIRST)).min(94) as usize;
    let mut s = String::new();
    for r in 0..GLYPH_H as usize {
        for col in GLYPHS[idx] {
            s.push(if col & (1 << r) != 0 { '#' } else { '.' });
        }
        s.push('\n');
    }
    s
}

/// 测试用：行主序源表（转置一致性断言要直接读源表）。
#[cfg(test)]
fn source_rows() -> &'static [[u8; GLYPH_H as usize]; 95] {
    &ROWS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_is_empty_and_letters_have_a_real_footprint() {
        assert_eq!(GLYPHS[0], [0; GLYPH_W as usize], "空格必须全空");
        let lit = |b: u8| -> u32 {
            GLYPHS[(b - FIRST) as usize]
                .iter()
                .map(|c| c.count_ones())
                .sum()
        };
        for b in b'A' as usize..=b'Z' as usize {
            let b = b as u8;
            assert!(lit(b) >= 10, "'{}' 点亮像素太少：{}", b as char, lit(b));
        }
        for &b in b"08EHMW" {
            assert!(lit(b) >= 10);
        }
        assert!(lit(b'.') >= 2 && lit(b'.') <= 6);
        assert!(lit(b'1') < lit(b'8'), "'1' 该比 '8' 稀疏");
    }

    /// 行主序源表转列主序再画出来，必须和源表逐行一致 —— 这条断言让"目视核对源表"
    /// 直接等价于"核对渲染结果"，转置函数写错就红。
    #[test]
    fn transposition_round_trips_the_source_table() {
        for (idx, rows) in source_rows().iter().enumerate() {
            let cols = glyph_cols(idx);
            for (r, &row) in rows.iter().enumerate() {
                for (c, &col) in cols.iter().enumerate() {
                    let src = row & (1 << (4 - c)) != 0;
                    let got = col & (1 << r) != 0;
                    assert_eq!(got, src, "glyph {} 行 {} 列 {} 转置漂移", idx, r, c);
                }
            }
        }
    }

    #[test]
    fn draw_writes_opaque_bgra_at_the_expected_offsets() {
        let (_, h, pitch) = (20usize, 10usize, 20 * 4);
        let mut c = vec![0u8; pitch * h];
        // 'A' 的顶行是 0x0E = .###. ⇒ (x+1..x+3, y) 三个像素。
        draw_str(&mut c, pitch, 2, 1, 1, 0x0000FF, "A");
        let px = |x: usize, y: usize| c[y * pitch + x * 4..][..4].to_vec();
        assert_eq!(px(3, 1), vec![255, 0, 0, 255], "BGRA 序或落点错了");
        assert_eq!(px(2, 1), vec![0, 0, 0, 0], "顶行第一列不该亮");
        assert_eq!(px(0, 0), vec![0, 0, 0, 0], "未画到的像素必须保持透明");
    }

    #[test]
    fn scale_multiplies_the_footprint() {
        let (pitch, h) = (64 * 4, 32usize);
        let mut a = vec![0u8; pitch * h];
        let mut b = vec![0u8; pitch * h];
        draw_str(&mut a, pitch, 0, 0, 1, 0x00FF00, "H");
        draw_str(&mut b, pitch, 0, 0, 2, 0x00FF00, "H");
        let lit = |v: &[u8]| v.chunks_exact(4).filter(|p| p[3] == 255).count();
        assert_eq!(lit(&b), lit(&a) * 4, "2× 点亮的像素应是 1× 的 4 倍");
        assert_eq!(text_width("H", 1), 6);
        assert_eq!(text_width("H", 2), 12);
        assert_eq!(text_width("", 2), 0);
    }

    #[test]
    fn out_of_bounds_is_clipped_not_a_panic() {
        let pitch = 16 * 4;
        let mut c = vec![0u8; pitch * 16];
        draw_str(&mut c, pitch, -4, -4, 1, 0xFFFFFF, "ABC DEF");
        draw_str(&mut c, pitch, 15, 15, 1, 0xFFFFFF, "ABC DEF");
        draw_str(&mut c, pitch, 1000, 1000, 3, 0xFFFFFF, "XYZ");
        draw_str(&mut c, pitch, 4, 4, 1, 0xFFFFFF, "你好"); // 非 ASCII ⇒ 画 '?'，不许 panic
        assert!(c.chunks_exact(4).any(|p| p[3] == 255));
    }

    /// 画布比字形小 ⇒ 逐点裁剪：只画得下的那个像素进颜色，其余整趟不 panic。
    #[test]
    fn tiny_canvas_is_clipped_pixel_by_pixel() {
        let mut c = vec![0u8; 4];
        draw_str(&mut c, 4, 0, 0, 1, 0x00FF00, "M");
        // 'M' 的 (0,0) 由源表首行 0x11 决定（bit4 = 最左列）⇒ 左上角必须亮。
        assert_eq!(
            c,
            vec![0x00, 0xFF, 0x00, 255],
            "第一个像素应写进去，且不越界"
        );

        let mut empty: Vec<u8> = Vec::new();
        draw_str(&mut empty, 0, 0, 0, 1, 0xFFFFFF, "MMMM");
        assert!(empty.is_empty(), "零长度画布不该被写");
    }

    /// 关键骨架钉死：这几支一旦写歪（行序颠倒、转置反、整表错位）形状立刻崩，而
    /// "点亮像素总数"这类数字门禁抓不住 —— 所以直接钉成人眼能读的 7 行点阵。
    /// 期望值按源表**手读**写出，不是从代码 dump 回来的（dump 回来就自我实现了）。
    #[test]
    fn key_glyph_skeletons_are_pinned() {
        let art = |b: u8| ascii_art(b);
        assert_eq!(
            art(b'H'),
            concat!(
                "#...#\n", "#...#\n", "#...#\n", "#####\n", "#...#\n", "#...#\n", "#...#\n"
            )
        );
        assert_eq!(
            art(b'A'),
            concat!(
                ".###.\n", "#...#\n", "#...#\n", "#####\n", "#...#\n", "#...#\n", "#...#\n"
            )
        );
        assert_eq!(
            art(b'0'),
            concat!(
                ".###.\n", "#...#\n", "#..##\n", "#.#.#\n", "##..#\n", "#...#\n", ".###.\n"
            ),
            "零里那道斜杠是 '0' 与 'O' 唯一的区别"
        );
        assert_eq!(
            art(b'1'),
            concat!(
                "..#..\n", ".##..\n", "..#..\n", "..#..\n", "..#..\n", "..#..\n", ".###.\n"
            )
        );
        assert_eq!(
            art(b'-'),
            concat!(
                ".....\n", ".....\n", ".....\n", "#####\n", ".....\n", ".....\n", ".....\n"
            )
        );
        assert_eq!(
            art(b'.'),
            concat!(
                ".....\n", ".....\n", ".....\n", ".....\n", ".###.\n", ".###.\n", ".....\n"
            )
        );
    }

    /// 目视基准：最容易写错成同形的那几对必须可区分（历史上 'g'/'e'、'p'/'q'、'w'/'W'
    /// 都在赶出来的表里塌成过同一个形状，像素数相同 ⇒ 数字门禁抓不住）。
    #[test]
    fn confusable_pairs_stay_distinguishable() {
        let g = |b: u8| GLYPHS[(b - FIRST) as usize];
        for (a, b) in [
            (b'g', b'e'),
            (b'p', b'q'),
            (b'b', b'd'),
            (
                b'w', b'W', /* 大小写同形可接受，但点阵必须不同 */
            ),
            (b'0', b'O'),
            (b'1', b'l'),
            (b'5', b's'),
        ] {
            assert_ne!(
                g(a),
                g(b),
                "'{}' 与 '{}' 塌成同一形状",
                a as char,
                b as char
            );
        }
    }
}
