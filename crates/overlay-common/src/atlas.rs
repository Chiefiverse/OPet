//! 图集像素：解码 `sheet.webp` → BGRA8 **预乘** 缓冲，按格取像素。
//!
//! 为什么在这里做而不在内核：`OverlayBackend` 只传图集格坐标（§3.7），
//! Rust 侧 `petcore` 依赖只有 serde/serde_json（M1 的"内核不解码像素"约束）。
//!
//! **预乘 + 小端 B,G,R,A 这个字节布局是两个平台共同的口径**，不是某一家协议的要求：
//! Wayland 的 `wl_shm` `Argb8888` 与 Win32 `UpdateLayeredWindow` 带 `AC_SRC_ALPHA` 吃的
//! 都是"32 位小端、预乘"的同一串字节 ⇒ 烘焙产物（直通 alpha 的 RGBA）在这一层转一次，
//! 两边各自**零转换**接走，逐帧退化成整块拷贝。
//!
//! 因此本模块**不做**的事：不认 surface、不认 HWND、不认 buffer scale。缩放与 DPI
//! 由各客户端的 `to_phys` 那类函数解决（那边才有窗口系统）。

/// 判定"这个像素算不算剪影内（吃点击）"的 alpha 阈值。
///
/// ⚠️ **这是跨三个事实源的同一个数，口径只此一处**：`tools/pack.py` 的 `column_spans`
/// （产 `polygon.json` 的 span 表）、`tools/validate.py`（产物门禁）、
/// 各平台客户端的命中判定（Win32 靠 `WM_NCHITTEST` 读屏上像素的 alpha）。
/// 历史上它是三份各写一遍的 `8`（`tests/accessory_product.rs` 那条注释自己写着"三处必须一起改"）
/// ⇒ 现在由这里当出处，那两处引用它。
pub const ALPHA_HIT_THRESHOLD: u32 = 8;

use std::io::Read;
use std::time::Instant;

use image_webp::{ColorType, WebPDecoder, WebPEncoder};

#[derive(Debug)]
pub enum AtlasError {
    Io(String),
    Decode(String),
    /// 解码出来的尺寸与清单的 `cols*w × rows*h` 不一致
    Geometry {
        expected: (u32, u32),
        got: (u32, u32),
    },
    /// 取格越界，或目标缓冲装不下一次 blit
    Bounds {
        row: u32,
        col: u32,
        detail: String,
    },
}

impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtlasError::Io(s) => write!(f, "读文件失败: {s}"),
            AtlasError::Decode(s) => write!(f, "WebP 解码失败: {s}"),
            AtlasError::Geometry { expected, got } => {
                let (ew, eh) = expected;
                let (gw, gh) = got;
                write!(f, "图集尺寸不符：清单要求 {ew}x{eh}，实际 {gw}x{gh}")
            }
            AtlasError::Bounds { row, col, detail } => {
                write!(f, "取格 ({row},{col}) 越界: {detail}")
            }
        }
    }
}

impl std::error::Error for AtlasError {}

/// 解码后的整张图集，像素为 BGRA8 预乘。
#[derive(Debug)]
pub struct Atlas {
    pixels: Vec<u8>,
    /// sheet 的像素尺寸
    pub width: u32,
    pub height: u32,
    /// 单格尺寸（= 清单的 `grid.w/h`，内核里它就是宠物体型）
    pub cell_w: u32,
    pub cell_h: u32,
    pub cols: u32,
    pub rows: u32,
    /// 解码耗时（M4 门禁要这个数）
    pub decode_ms: u128,
    /// 编码后的字节数（磁盘侧门禁）
    pub encoded_bytes: usize,
}

impl Atlas {
    /// 解码并按清单校验尺寸。`grid_*` 来自 [`petcore::assets::Manifest`]。
    pub fn load(
        path: &str,
        cols: u32,
        rows: u32,
        grid_w: u32,
        grid_h: u32,
    ) -> Result<Atlas, AtlasError> {
        let t0 = Instant::now();
        let mut buf = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| AtlasError::Io(format!("{path}: {e}")))?
            .read_to_end(&mut buf)
            .map_err(|e| AtlasError::Io(format!("{path}: {e}")))?;
        let encoded_bytes = buf.len();

        let mut decoder = WebPDecoder::new(std::io::Cursor::new(buf))
            .map_err(|e| AtlasError::Decode(e.to_string()))?;
        let (width, height) = decoder.dimensions();
        if !decoder.has_alpha() {
            return Err(AtlasError::Decode(format!(
                "{path} 没有 alpha 通道，无法做点击穿透"
            )));
        }
        let need = decoder
            .output_buffer_size()
            .ok_or_else(|| AtlasError::Decode("尺寸超出 usize".into()))?;
        let mut rgba = vec![0u8; need];
        decoder
            .read_image(&mut rgba)
            .map_err(|e| AtlasError::Decode(e.to_string()))?;

        let expected = (cols * grid_w, rows * grid_h);
        if (width, height) != expected {
            return Err(AtlasError::Geometry {
                expected,
                got: (width, height),
            });
        }

        premultiply_in_place(&mut rgba);

        Ok(Atlas {
            pixels: rgba,
            width,
            height,
            cell_w: grid_w,
            cell_h: grid_h,
            cols,
            rows,
            decode_ms: t0.elapsed().as_millis(),
            encoded_bytes,
        })
    }

    /// **原地**换一张 sheet（P5 换本体唯一的那次解码），峰值 = **一份** sheet。
    ///
    /// 三条按便宜排序的自检全部排在**第一笔写之前**：编码尺寸、有无 alpha、
    /// 解码输出长度 == 现有缓冲长度。所以"格子档不同 / 不是 WebP / 没有 alpha"这三类
    /// 配错留下的是**原来那只完整的宠物**，而不是半张新图。
    ///
    /// ⚠️ 残余风险要明写：`read_image` 本身在**头已过检但文件被截断**时仍可能在写入途中
    /// 失败，那时 `self.pixels` 里是残缺的一张。不为此先解到临时缓冲，因为那正是
    /// load-then-swap 的形状 = **峰值两只本体**（≈147 MB），而 ≤200 MiB 那道门禁**抓不到它**
    /// —— 唯一防线是这段代码本身加上验收量一次 `VmHWM`。同理 `pixels` 是私有字段：
    /// 外面**没有**"先拿到新的一份再换掉"这条路可走。
    ///
    /// `cols/rows/cell_w/cell_h` 不被改动 ⇒ 它们仍是**当前清单**的函数（跨档在发现那一步就
    /// 出局了，见 `App::check_body`）；`width/height/decode_ms/encoded_bytes` 是新 sheet 的读数。
    pub fn reload(&mut self, path: &str) -> Result<(), AtlasError> {
        let t0 = Instant::now();
        let mut buf = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| AtlasError::Io(format!("{path}: {e}")))?
            .read_to_end(&mut buf)
            .map_err(|e| AtlasError::Io(format!("{path}: {e}")))?;
        let encoded_bytes = buf.len();

        let mut decoder = WebPDecoder::new(std::io::Cursor::new(&buf))
            .map_err(|e| AtlasError::Decode(e.to_string()))?;
        let (width, height) = decoder.dimensions();
        let expected = (self.cols * self.cell_w, self.rows * self.cell_h);
        if (width, height) != expected {
            return Err(AtlasError::Geometry {
                expected,
                got: (width, height),
            });
        }
        if !decoder.has_alpha() {
            return Err(AtlasError::Decode(format!(
                "{path} 没有 alpha 通道，无法做点击穿透"
            )));
        }
        let need = decoder
            .output_buffer_size()
            .ok_or_else(|| AtlasError::Decode("尺寸超出 usize".into()))?;
        if need != self.pixels.len() {
            // 上面 `(width,height) == cols*cell_w × rows*cell_h` 已经过了还走到这里，
            // 说明这个 `Atlas` 的格参数和它自己的缓冲长度不一致 —— 那是构造错误，不是配错档。
            return Err(AtlasError::Decode(format!(
                "{path} 解码要 {need} 字节，本缓冲 {} 字节",
                self.pixels.len()
            )));
        }

        decoder
            .read_image(&mut self.pixels)
            .map_err(|e| AtlasError::Decode(e.to_string()))?;
        premultiply_in_place(&mut self.pixels);

        self.width = width;
        self.height = height;
        self.decode_ms = t0.elapsed().as_millis();
        self.encoded_bytes = encoded_bytes;
        Ok(())
    }

    /// 单格左上角的字节偏移（格内第 0 行）；越界返回 `None`。
    ///
    /// 注意跨距是**整张 sheet 的宽度**，不是格子宽度 —— 少乘一个 `width` 就会串格。
    pub fn cell_offset(&self, row: u32, col: u32) -> Option<usize> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some((row * self.cell_h * self.width + col * self.cell_w) as usize * 4)
    }

    /// 把 `(row, col)` 格画进 `dst`（BGRA 预乘，`dst_stride` 为每行字节数）。
    ///
    /// `dst_w/dst_h` 可以与格子不同（HiDPI/分数缩放），用**最近邻**取整缩放：
    /// 例如 192 格的产物在 1.5× 下是 288，不做插值以免糊掉平涂边。
    pub fn blit(
        &self,
        row: u32,
        col: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
        dst_stride: usize,
    ) -> Result<(), AtlasError> {
        self.paint_cell(row, col, dst, dst_w, dst_h, dst_stride, |src, d| {
            d.copy_from_slice(&src);
        })
    }

    /// 把 `(row, col)` 格**叠**进 `dst`（source-over，见 [`composite_over`]）。
    ///
    /// 与 [`Self::blit`] 的唯一区别是那一次逐像素合并 —— 取样、边界、最近邻全部共用
    /// [`Self::paint_cell`]。**这是刻意的**：`paint_cell` 里 `src_take` 用格宽而不是 sheet
    /// 跨距、`dst_take` 用 `dst_w`，两条都是真机撞出来才修对的；复制一份循环 = 下一次只修
    /// 其中一份，而"配件画在最底行假报越界"这种症状在屏上只是缺一块，不会告诉你是哪份错了。
    ///
    /// ⚠️ 前提由 [`petcore::assets::AccessoryManifest::validate_against`] 在装备那一刻保证：**配件 sheet 与
    /// 本体 sheet 同 `cols/w/h`** ⇒ 同一个 `(row, col)` 在两块缓冲里落在屏幕上同一块矩形，
    /// 所以这里**不需要任何偏移参数**。偏移恒为 0 是被校验的性质，不是假设。
    pub fn blit_over(
        &self,
        row: u32,
        col: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
        dst_stride: usize,
    ) -> Result<(), AtlasError> {
        self.paint_cell(row, col, dst, dst_w, dst_h, dst_stride, |src, d| {
            let out = composite_over(src, [d[0], d[1], d[2], d[3]]);
            d.copy_from_slice(&out);
        })
    }

    /// `blit` / `blit_over` 共用的逐格取样：`put` 决定源像素怎么落到目标上。
    // 八个参数就是一次二维 blit 的全部事实：源格 `(row, col)` + 目标矩形 `(w, h, stride)` +
    // 混合方式。包成结构体只会让这两个唯一的调用点各跳一次定义，而换不来任何一个新错误形状。
    #[allow(clippy::too_many_arguments)]
    fn paint_cell<F>(
        &self,
        row: u32,
        col: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
        dst_stride: usize,
        mut put: F,
    ) -> Result<(), AtlasError>
    where
        F: FnMut([u8; 4], &mut [u8]),
    {
        let base = self
            .cell_offset(row, col)
            .ok_or_else(|| AtlasError::Bounds {
                row,
                col,
                detail: format!("图集只有 {} 行 {} 列", self.rows, self.cols),
            })?;
        if dst_w == 0 || dst_h == 0 {
            return Ok(());
        }
        let src_stride = (self.width * 4) as usize;
        // 每行实际从源读的字节数：是**格宽**，不是整张 sheet 的宽度。
        // 用 src_stride 做边界会在最后一行（尤其最底行格）假报越界。
        let src_take = (self.cell_w * 4) as usize;
        let dst_take = (dst_w * 4) as usize;
        for y in 0..dst_h {
            let sy = (y * self.cell_h) / dst_h;
            let srow = base + (sy as usize) * src_stride;
            let drow = (y as usize) * dst_stride;
            if srow + src_take > self.pixels.len() {
                return Err(AtlasError::Bounds {
                    row,
                    col,
                    detail: format!("源第 {sy} 行超出图集像素长度 {}", self.pixels.len()),
                });
            }
            if drow + dst_take > dst.len() {
                return Err(AtlasError::Bounds {
                    row,
                    col,
                    detail: format!("目标缓冲在第 {y} 行不够 {} 字节", dst.len()),
                });
            }
            // `chunks_exact_mut` 而不是手写 `di`：`dst_take == dst_w × 4` ⇒ 恰好 `dst_w`
            // 个块，每块直接是 `&mut [u8; 4]`（`put` 要的形状），少一处下标算术。
            for (x, d) in dst[drow..drow + dst_take].chunks_exact_mut(4).enumerate() {
                let sx = (x as u32 * self.cell_w) / dst_w;
                let si = srow + (sx as usize) * 4;
                put(
                    [
                        self.pixels[si],
                        self.pixels[si + 1],
                        self.pixels[si + 2],
                        self.pixels[si + 3],
                    ],
                    d,
                );
            }
        }
        Ok(())
    }

    /// 解码后的字节数（RSS 门禁的直接输入）。
    pub fn decoded_bytes(&self) -> usize {
        self.pixels.len()
    }
}

/// 直通 RGBA → 预乘 BGRA（`Argb8888` 在小端下的内存顺序）。
///
/// 向上取整的 `div_ceil` 在 `a == 255` 时精确等于 `c`（不会四舍五入偏 1）。
pub fn premultiplied_bgra(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    let mul = |c: u8| (c as u32 * a as u32).div_ceil(256) as u8;
    [mul(b), mul(g), mul(r), a]
}

/// 直通 RGBA → 预乘 BGRA，**原地**做：每个 4 字节只依赖它自己那 4 个字节 ⇒ 不留第二份缓冲。
///
/// 这条转换只有这一份（[`Atlas::load`] 与 [`Atlas::reload`] 共用）。它值得抽出来而不是两处
/// 各写一遍循环，是因为"原先另开一个 `Vec::with_capacity` 边读边写、峰值其实是 2 份、而注释
/// 写着不留第二份"那次修正只能存在一处：复制一份循环 = 给下一次"顺手改成新分配"留一个没人
/// 复查的落点，而它的症状不是崩，是 RSS 静默翻倍。
fn premultiply_in_place(px: &mut [u8]) {
    for px in px.chunks_exact_mut(4) {
        let [b, g, r, a] = premultiplied_bgra(px[0], px[1], px[2], px[3]);
        px[0] = b;
        px[1] = g;
        px[2] = r;
        px[3] = a;
    }
}

/// 预乘 BGRA 的 **source-over**：`out = src + dst × (1 − src.a)`，alpha 同式。
///
/// 全整数、无浮点、就近取整（`+127`）。`a == 255` 时 `keep == 0` ⇒ 精确等于 `src`；
/// `a == 0` 时精确等于 `dst`。这两个端点是叠加路径最常用的两档（帽子实体 / 透明背景），
/// 所以它们各自有单测，不留给"一般情况也对"推断。
///
/// ⚠️ **不 clamp，因为溢出对预乘输入不可能发生**：预乘 ⇒ `src ≤ src.a`，而
/// `(dst×(255−a)+127)/255 ≤ 255−a`，两项相加 `≤ a + (255−a) = 255`。⇒ **前提写在这里，
/// 判据钉在 `composite_over_never_overflows_premultiplied_source`（上界逐 `a` 取到）与
/// `premultiply_keeps_channel_below_alpha`（生产端确实满足 `src ≤ a`）两条**。
/// 传一份**直通**（未预乘）像素进来是调用方的错，症状是回绕成一个错色点而不是报错。
pub fn composite_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let keep = 255u32 - src[3] as u32;
    let one = |s: u8, d: u8| (s as u32 + (d as u32 * keep + 127) / 255) as u8;
    [
        one(src[0], dst[0]),
        one(src[1], dst[1]),
        one(src[2], dst[2]),
        one(src[3], dst[3]),
    ]
}

/// 测试用：把 RGBA 编成 lossless WebP（`image-webp` 的编码器只出 VP8L）。
pub fn encode_lossless_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    WebPEncoder::new(&mut out)
        .encode(rgba, width, height, ColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 合成图集：**每个像素的 alpha 都由这张表写下**，所以像素级判据能在没有烘焙产物的机器上跑。
///
/// 为什么要有这个模块（而不是让各平台客户端自己造一份 `Vec<u8>`）：Windows 那侧的
/// hit-test 与 blit 数学要在 **Linux 宿主**上就能红，而拿烘焙产物当样本的那条断言只能
/// `#[ignore]` —— 那就是零读数。这里给它一份真过一遍 WebP 编解码与预乘的图集。
///
/// ⚠️ 这里**没有**搬上面 `mod tests` 里那份四格纯色图集：它的四格全是 R=G=B 的纯色
/// （红 `[255,0,0]` 之外都是通道等值），拿它做字节序判据会把"踩错通道"和"踩对了"混成一谈。
/// 本模块的探针格故意让 **B=0 而 A=200**，见 [`DARK_OPAQUE`] 那条。
#[cfg(any(test, feature = "test-fixture"))]
pub mod fixture {
    use super::*;

    /// 探针图集的格尺寸 / 行列数（`cell_offset` 的跨距是整张 sheet 的宽，2×2 才量得出串格）。
    pub const CELL: u32 = 4;
    pub const COLS: u32 = 2;
    pub const ROWS: u32 = 2;

    /// 剪影格：中间两列（x∈{1,2}）从上到下不透明，其余全透。
    ///
    /// 它是 [`petcore::intent::PresentationIntent::hit_test_world`] 那四个探针的样本本体：
    /// 脚锚在格内、四角在余量里、帧上边界上下各 1 px 一读一不读。
    pub const SILHOUETTE: (u32, u32) = (0, 0);
    /// 这颗锚点落在剪影的不透明列上，且贴在格的最后一行（= 贴 margin 下边）。
    pub const FEET: [f32; 2] = [2.0, 3.0];
    /// alpha 探针格：四个像素逐个点名一种"尺子会读错"的形状，位置见下面四个常量。
    pub const ALPHA_PROBES: (u32, u32) = (0, 1);
    /// 整格全透 —— 画它必须得到一块全零的 DIB，量的是"取样没串到隔壁格"。
    pub const EMPTY: (u32, u32) = (1, 0);
    /// 三色互不相等的实心格：字节序判据用（`[10,20,30]` 只有落对顺序才可分辨）。
    pub const SOLID: (u32, u32) = (1, 1);

    // 下面四个坐标是 `ALPHA_PROBES` 格内的 `(x, y)`，直通 RGBA（源）→ 预乘 BGRA（屏）。
    // 每颗都注明"错的那把尺子会怎么答"，因为这四颗的全部意义就是让两种答法分开。

    /// `a == 8`，恰好**等于**阈值 ⇒ 正确答"穿透"（判据是 `>` 不是 `>=`，与 `tools/pack.py:111`
    /// 的 `alpha > ALPHA_THRESHOLD` 同一口径，而那份口径是 `polygon.json` 的 `threshold: 8` 的出处）。
    pub const A_EQ_8: (u32, u32) = (0, 0);
    /// `a == 9` ⇒ 阈值之上，正确答"命中"。与上一颗**成对**，单独一颗什么也不证明。
    pub const A_EQ_9: (u32, u32) = (1, 0);
    /// 源 `(R=9, G=0, B=0, A=0)`：预乘后四字节全零 ⇒ 正确答"穿透"。
    ///
    /// ⚠️ 这颗**不是**通道错位的判据（预乘把它抹成 `[0,0,0,0]`，读 index 0 与读 index 3 同答）。
    /// 它钉的是源→DIB 那一跳：一个"红但全透"的源像素到屏上必须还是全零，
    /// 否则屏上会多出一粒看得见的红点，而 `#[ignore]` 的产物门禁看不见它。
    pub const RED_ZERO_ALPHA: (u32, u32) = (2, 0);
    /// 源 `(R=255, G=0, B=0, A=200)` → 预乘 BGRA `[0, 0, 199, 200]`：**蓝道 0 而 alpha 200**。
    ///
    /// ⇒ 读 index 3 答"命中"，读 index 0（`§6.1.20⑮` 那条 `step_by(4)` 踩错通道的形状）答"穿透"。
    /// 这一颗才是通道判据，而它错的方向是**假阴性**：深色不透明的像素被当成洞，
    /// 症状是剪影上少一块、点不动，不是多一块。
    pub const DARK_OPAQUE: (u32, u32) = (3, 0);

    /// 探针图集的直通 RGBA 源像素（行优先，整张 8×8）。
    pub fn probe_rgba() -> Vec<u8> {
        let w = COLS * CELL;
        let h = ROWS * CELL;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mut put = |cx: u32, cy: u32, px: u32, py: u32, c: [u8; 4]| {
            let x = cx * CELL + px;
            let y = cy * CELL + py;
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&c);
        };
        // 剪影格：中间两列实心
        for y in 0..CELL {
            for x in 1..=2 {
                put(SILHOUETTE.1, SILHOUETTE.0, x, y, [255, 255, 255, 255]);
            }
        }
        // alpha 探针格
        put(
            ALPHA_PROBES.1,
            ALPHA_PROBES.0,
            A_EQ_8.0,
            A_EQ_8.1,
            [255, 255, 255, 8],
        );
        put(
            ALPHA_PROBES.1,
            ALPHA_PROBES.0,
            A_EQ_9.0,
            A_EQ_9.1,
            [255, 255, 255, 9],
        );
        put(
            ALPHA_PROBES.1,
            ALPHA_PROBES.0,
            RED_ZERO_ALPHA.0,
            RED_ZERO_ALPHA.1,
            [9, 0, 0, 0],
        );
        put(
            ALPHA_PROBES.1,
            ALPHA_PROBES.0,
            DARK_OPAQUE.0,
            DARK_OPAQUE.1,
            [255, 0, 0, 200],
        );
        // 实心格
        for y in 0..CELL {
            for x in 0..CELL {
                put(SOLID.1, SOLID.0, x, y, [10, 20, 30, 255]);
            }
        }
        rgba
    }

    /// 走一遍**真**编解码与真预乘路径的合成图集（与 [`Atlas::load`] 同一个转换器）。
    pub fn probe_atlas() -> Atlas {
        let webp = encode_lossless_rgba(&probe_rgba(), COLS * CELL, ROWS * CELL).expect("encode");
        let mut dec = WebPDecoder::new(std::io::Cursor::new(webp)).expect("decode");
        let mut pixels = vec![0u8; dec.output_buffer_size().unwrap()];
        dec.read_image(&mut pixels).expect("read");
        premultiply_in_place(&mut pixels);
        Atlas {
            pixels,
            width: COLS * CELL,
            height: ROWS * CELL,
            cell_w: CELL,
            cell_h: CELL,
            cols: COLS,
            rows: ROWS,
            decode_ms: 0,
            encoded_bytes: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_pixel_maps_to_bgra_identity() {
        assert_eq!(premultiplied_bgra(10, 20, 30, 255), [30, 20, 10, 255]);
    }

    #[test]
    fn transparent_pixel_is_zeroed() {
        assert_eq!(premultiplied_bgra(255, 128, 64, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn half_alpha_halves_channels() {
        // a=128：(255*128+255)/256 = 128 ⇒ 正好一半
        assert_eq!(premultiplied_bgra(255, 255, 255, 128), [128, 128, 128, 128]);
    }

    #[test]
    fn lossless_webp_roundtrips_bit_exact() {
        let w = 8u32;
        let h = 4u32;
        let mut rgba = Vec::new();
        for i in 0..(w * h) {
            rgba.extend_from_slice(&[
                (i % 256) as u8,
                (i * 7 % 256) as u8,
                (i * 13 % 256) as u8,
                (i * 5 % 256) as u8,
            ]);
        }
        let webp = encode_lossless_rgba(&rgba, w, h).expect("encode");
        let mut dec = WebPDecoder::new(std::io::Cursor::new(webp)).expect("decode");
        let mut back = vec![0u8; dec.output_buffer_size().unwrap()];
        dec.read_image(&mut back).expect("read");
        assert_eq!(back, rgba, "lossless WebP 必须逐位往返");
    }

    /// 2 列 × 2 行、格子 4×4 的合成图集：验证取格偏移与最近邻缩放。
    fn synthetic() -> Vec<u8> {
        let (w, h) = (8u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        // 每格填一种不透明色
        let fill = |rgba: &mut [u8], r: u32, c: u32, col: [u8; 4]| {
            for y in 0..4 {
                for x in 0..4 {
                    let i = (((r * 4 + y) * w + c * 4 + x) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&col);
                }
            }
        };
        fill(&mut rgba, 0, 0, [255, 0, 0, 255]);
        fill(&mut rgba, 0, 1, [0, 255, 0, 255]);
        fill(&mut rgba, 1, 0, [0, 0, 255, 255]);
        fill(&mut rgba, 1, 1, [1, 2, 3, 255]);
        rgba
    }

    fn atlas_from_synthetic() -> Atlas {
        let webp = encode_lossless_rgba(&synthetic(), 8, 8).expect("encode");
        let mut dec = WebPDecoder::new(std::io::Cursor::new(webp)).expect("decode");
        let mut rgba = vec![0u8; dec.output_buffer_size().unwrap()];
        dec.read_image(&mut rgba).expect("read");
        let mut pixels = Vec::new();
        for px in rgba.chunks_exact(4) {
            pixels.extend_from_slice(&premultiplied_bgra(px[0], px[1], px[2], px[3]));
        }
        Atlas {
            pixels,
            width: 8,
            height: 8,
            cell_w: 4,
            cell_h: 4,
            cols: 2,
            rows: 2,
            decode_ms: 0,
            encoded_bytes: 0,
        }
    }

    #[test]
    fn cell_offset_follows_sheet_stride() {
        let a = atlas_from_synthetic();
        assert_eq!(a.cell_offset(0, 0), Some(0));
        // row 1 起点 = 跳过 4 行、每行 8px×4B
        assert_eq!(a.cell_offset(1, 0), Some(4 * 8 * 4));
        assert_eq!(a.cell_offset(0, 1), Some(4 * 4));
        assert_eq!(a.cell_offset(2, 0), None, "越界行必须拒绝");
        assert_eq!(a.cell_offset(0, 2), None, "越界列必须拒绝");
    }

    #[test]
    fn blit_1to1_picks_the_right_cell() {
        let a = atlas_from_synthetic();
        for (row, col, want) in [
            (0u32, 0u32, [0u8, 0, 255, 255]), // 红 → BGR
            (0, 1, [0, 255, 0, 255]),
            (1, 0, [255, 0, 0, 255]),
            (1, 1, [3, 2, 1, 255]),
        ] {
            let mut dst = vec![0u8; 4 * 4 * 4];
            a.blit(row, col, &mut dst, 4, 4, 4 * 4).expect("blit");
            assert_eq!(&dst[0..4], &want[..], "格 ({row},{col}) 第一像素");
            assert!(
                dst.chunks_exact(4).all(|p| p == want),
                "格 ({row},{col}) 应整格同色"
            );
        }
    }

    #[test]
    fn blit_rejects_too_small_destination() {
        let a = atlas_from_synthetic();
        let mut dst = vec![0u8; 4 * 4 * 4 - 1];
        let e = a.blit(0, 0, &mut dst, 4, 4, 4 * 4).unwrap_err();
        assert!(matches!(e, AtlasError::Bounds { .. }), "{e}");
    }

    #[test]
    fn blit_upscales_by_two_with_nearest() {
        let a = atlas_from_synthetic();
        let mut dst = vec![0u8; 8 * 8 * 4];
        a.blit(1, 1, &mut dst, 8, 8, 8 * 4).expect("blit 2x");
        assert!(
            dst.chunks_exact(4).all(|p| p == [3, 2, 1, 255]),
            "2× 放大后仍应是同一颜色"
        );
    }

    #[test]
    fn blit_downscale_does_not_shift_within_cell() {
        let a = atlas_from_synthetic();
        let mut dst = vec![0u8; 2 * 2 * 4];
        a.blit(0, 1, &mut dst, 2, 2, 2 * 4).expect("blit 0.5x");
        assert!(
            dst.chunks_exact(4).all(|p| p == [0, 255, 0, 255]),
            "缩小取样必须留在自己格内"
        );
    }

    /// 一格左半不透明、右半全透明的图集 —— 叠加路径唯一有意义的形状（帽实体 + 透明背景）。
    ///
    /// 像素已是 BGRA **预乘**，所以透明那半是四个 0 而不是"颜色 + a=0"。
    fn half_transparent_atlas() -> Atlas {
        let (w, h) = (8u32, 8u32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..4u32 {
            for x in 0..4u32 {
                if x >= 2 {
                    continue; // 右半保持全 0 = 全透明
                }
                let i = ((y * w + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[30, 20, 10, 255]);
            }
        }
        Atlas {
            pixels,
            width: w,
            height: h,
            cell_w: 4,
            cell_h: 4,
            cols: 2,
            rows: 2,
            decode_ms: 0,
            encoded_bytes: 0,
        }
    }

    /// 空底子上"叠加"必须逐字节等于"拷贝" ⇒ 一次性钉住两条路径共用同一套几何
    /// （取格偏移、最近邻取样、边界判定），把合并数学单独留给 [`composite_over`] 那几条。
    #[test]
    fn blit_over_on_an_empty_destination_is_bit_equal_to_blit() {
        for &(dw, dh) in &[(4u32, 4u32), (8, 8), (2, 2)] {
            let a = atlas_from_synthetic();
            let stride = (dw * 4) as usize;
            let n = (dw * dh * 4) as usize;
            for row in 0..a.rows {
                for col in 0..a.cols {
                    let mut copy = vec![0u8; n];
                    let mut over = vec![0u8; n];
                    a.blit(row, col, &mut copy, dw, dh, stride)
                        .expect("blit 这格");
                    a.blit_over(row, col, &mut over, dw, dh, stride)
                        .expect("blit_over 这格");
                    assert_eq!(
                        copy, over,
                        "dst {dw}x{dh} 格 ({row},{col})：叠加与拷贝取到的像素分岔了"
                    );
                }
            }
        }
    }

    #[test]
    fn blit_over_leaves_the_transparent_part_of_the_cell_untouched() {
        // 这条是叠加功能的正面判据：帽子的透明背景**不许**在本体身上打洞。
        let a = half_transparent_atlas();
        let mut dst = vec![0u8; 4 * 4 * 4];
        for px in dst.chunks_exact_mut(4) {
            px.copy_from_slice(&[7, 8, 9, 255]); // 假设本体已经画好（不透明）
        }
        a.blit_over(0, 0, &mut dst, 4, 4, 4 * 4).expect("blit_over");
        for y in 0..4u32 {
            for x in 0..4u32 {
                let i = (y * 4 + x) as usize * 4;
                if x < 2 {
                    assert_eq!(&dst[i..i + 4], &[30, 20, 10, 255], "第 ({y},{x}) 该被盖住");
                } else {
                    assert_eq!(&dst[i..i + 4], &[7, 8, 9, 255], "第 ({y},{x}) 不许打洞");
                }
            }
        }
    }

    #[test]
    fn blit_over_reports_the_same_bounds_errors_as_blit() {
        let a = atlas_from_synthetic();
        // 目标太小：两条路径必须同样拒绝（叠加分支最容易写成"失败就悄悄不叠"）
        let mut dst = vec![0u8; 4 * 4 * 4 - 1];
        assert!(matches!(
            a.blit(0, 0, &mut dst, 4, 4, 4 * 4),
            Err(AtlasError::Bounds { .. })
        ));
        let mut dst = vec![0u8; 4 * 4 * 4];
        assert!(
            matches!(
                a.blit_over(9, 0, &mut dst, 4, 4, 4 * 4),
                Err(AtlasError::Bounds { .. })
            ),
            "越界行也要拒绝，不能当'这格没配件'放过"
        );
    }

    #[test]
    fn composite_over_is_exact_at_the_two_alpha_extremes() {
        // 叠加实际只会撞上这两档：实体（不透明）与背景（全透明）
        assert_eq!(
            composite_over([1, 2, 3, 255], [9, 9, 9, 255]),
            [1, 2, 3, 255],
            "a=255 必须完全盖住"
        );
        assert_eq!(
            composite_over([0, 0, 0, 0], [9, 8, 7, 200]),
            [9, 8, 7, 200],
            "a=0 必须原样留下底色"
        );
    }

    #[test]
    fn composite_over_rounds_to_nearest_not_down() {
        // a=128 ⇒ keep=127；RGB = 64 + (200×127 + 127)/255 = 64 + 100 = 164，
        // alpha 同式 = 128 + 100 = 228（预乘结果 164 ≤ 228，仍是合法预乘像素）。
        // RGB 真值 163.6 ⇒ 截断会给 163，所以这条区分的是"取整方向"而不只是"大致对"。
        assert_eq!(
            composite_over([64, 64, 64, 128], [200, 200, 200, 200]),
            [164, 164, 164, 228]
        );
    }

    #[test]
    fn composite_over_never_overflows_premultiplied_source() {
        // 函数里那句 `as u8` 不带 clamp，靠的是这条上界：src ≤ a ⇒ s + (d·keep+127)/255
        // ≤ a + (255−a) = 255。取**每个 a 的最坏合法输入**（s = a、d = 255）在 u32 里算，
        // 确认它从不越过 255 ⇒ 转换无损、clamp 是给不可能发生的情况做的错误处理。
        for a in 0u32..=255 {
            let keep = 255 - a;
            let want = a + (255 * keep + 127) / 255;
            assert!(
                want <= 255,
                "a={a} 最坏合法输入算出 {want} ⇒ 那时才需要 clamp"
            );
            assert_eq!(
                composite_over([a as u8; 4], [255; 4]),
                [want as u8; 4],
                "a={a}"
            );
        }
    }

    #[test]
    fn premultiply_keeps_channel_below_alpha() {
        // 上面那条的前提是"源是预乘的"。这里穷举 (通道值, alpha) 证明**生产端**确实交付
        // 了它 —— 两条合起来才是不溢出的完整论证，只留上一条等于把前提当假设。
        for a in 0u32..=255 {
            for c in 0u32..=255 {
                let [b, g, r, al] = premultiplied_bgra(c as u8, c as u8, c as u8, a as u8);
                for ch in [b, g, r] {
                    assert!(
                        ch as u32 <= al as u32,
                        "c={c} a={a} 时通道 {ch} 大于 alpha {al} ⇒ 不是预乘"
                    );
                }
            }
        }
    }

    /// 与 [`synthetic`] **同尺寸、内容不同**的另一张 sheet（`reload` 正向那半的输入）。
    ///
    /// 每格故意填**半透明**（`a = 128`）而不是不透明：那样 `premultiply_in_place` 在 reload
    /// 路径上才真的被测到（不透明时 `mul(c) == c`，忘了预乘也读不出来）。
    fn synthetic_other() -> Vec<u8> {
        let (w, h) = (8u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let fill = |rgba: &mut [u8], r: u32, c: u32, col: [u8; 4]| {
            for y in 0..4 {
                for x in 0..4 {
                    let i = (((r * 4 + y) * w + c * 4 + x) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&col);
                }
            }
        };
        fill(&mut rgba, 0, 0, [255, 0, 0, 128]);
        fill(&mut rgba, 0, 1, [0, 255, 0, 128]);
        fill(&mut rgba, 1, 0, [0, 0, 255, 128]);
        fill(&mut rgba, 1, 1, [1, 2, 3, 128]);
        rgba
    }

    /// `Atlas::reload` 的两半：**坏件留下原来那只完整的宠物**，**好件真的换掉像素且原地**。
    ///
    /// 这条是 P5 那条内存风险（reload 退化成 load-then-swap ⇒ 峰值两只本体 ≈147 MB，而
    /// 而 ≤200 MiB 那道门禁**抓不到**）在 headless 侧唯一的连接点。它读的是**行为**：
    /// `pixels.len()` 在成功 reload 前后逐字节相同 ⇒ 没有第二次分配、没有双份缓冲；
    /// 三类失败各自把 `pixels` 与四个元数据读数钉回原样 ⇒ 不可回头的半换状态不可达。
    /// 重量本身（`VmHWM`）另由验收量一次，两半都要有。
    #[test]
    fn reload_keeps_the_old_pixels_when_the_new_sheet_is_broken() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        let write = |name: &str, bytes: &[u8]| {
            std::fs::write(dir.path().join(name), bytes).expect("写临时 sheet")
        };
        write(
            "good.webp",
            &encode_lossless_rgba(&synthetic(), 8, 8).expect("编码"),
        );
        write(
            "other.webp",
            &encode_lossless_rgba(&synthetic_other(), 8, 8).expect("编码"),
        );
        // 档不同：16×8 = 4 列 × 2 行 @4px 的 sheet 喂给一张 2×2 图集（跨档本体就是这个形状）。
        let wide = vec![200u8; (16 * 8 * 4) as usize];
        write(
            "wrong_grid.webp",
            &encode_lossless_rgba(&wide, 16, 8).expect("编码"),
        );
        write("not_webp.webp", b"not a webp at all: no RIFF header here");

        let mut atlas = Atlas::load(&s("good.webp"), 2, 2, 4, 4).expect("起点图集");
        let pixels_before = atlas.pixels.clone();
        let meta_before = (atlas.width, atlas.height, atlas.encoded_bytes);
        assert_eq!(
            pixels_before.len(),
            8 * 8 * 4,
            "fixture 前提：@合成档的缓冲长度"
        );

        // 三类失败各有各的类型，不许混成一谈（"文件不在"和"档不同"在 stderr 上是两句话）。
        for (name, want, why) in [
            ("wrong_grid.webp", "Geometry", "格子档不同"),
            ("not_webp.webp", "Decode", "不是 WebP"),
            ("missing.webp", "Io", "文件不在"),
        ] {
            let path = s(name);
            let err = atlas.reload(&path).expect_err("坏件必须失败关闭");
            let got = match err {
                AtlasError::Io(_) => "Io",
                AtlasError::Decode(_) => "Decode",
                AtlasError::Geometry { .. } => "Geometry",
                AtlasError::Bounds { .. } => "Bounds",
            };
            assert_eq!(got, want, "{why} 必须报 {want}");
            assert_eq!(
                atlas.pixels, pixels_before,
                "reload 失败之后像素必须还是原来那一只（{why}）"
            );
            assert_eq!(
                (atlas.width, atlas.height, atlas.encoded_bytes),
                meta_before,
                "reload 失败之后元数据不许先走一步（{why}）"
            );
        }

        // 正向那半：同档的另一张必须真的改变像素，而且**原地**（长度不变 = 没有第二份）。
        atlas
            .reload(&s("other.webp"))
            .expect("同档的另一张必须成功");
        assert_ne!(
            atlas.pixels, pixels_before,
            "reload 成功却没换像素 = 空操作"
        );
        assert_eq!(
            atlas.pixels.len(),
            pixels_before.len(),
            "缓冲长度必须不变：变长就意味着 reload 被写成了 load-then-swap（峰值两只本体）"
        );
        assert_eq!(
            (atlas.width, atlas.height),
            (8, 8),
            "尺寸字段跟着新 sheet 走"
        );
        assert!(atlas.encoded_bytes > 0, "编码字节数必须换成新 sheet 的读数");
        // 逐值核一格：RGBA [255,0,0,128] → 预乘 BGRA = [0, 0, div_ceil(255·128,256)=128, 128]。
        // 这条同时证明 reload 走过 `premultiply_in_place`（忘了走它，红的那格会是 [0,0,255,128]）。
        assert_eq!(
            &atlas.pixels[..4],
            &[0, 0, 128, 128],
            "reload 之后第 0 格必须是**预乘**的 BGRA"
        );
    }
}
