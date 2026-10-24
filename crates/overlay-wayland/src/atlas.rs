//! 图集像素：解码 `sheet.webp` → BGRA8 **预乘** 缓冲，按格取像素。
//!
//! 为什么在这里做而不在内核：`OverlayBackend` 只传图集格坐标（§3.7），
//! Rust 侧 `petcore` 依赖只有 serde/serde_json（M1 的"内核不解码像素"约束）。
//!
//! `wl_shm` 的 `Argb8888` 要求**预乘** alpha，且小端内存布局是 B,G,R,A；
//! 烘焙产物是直通 alpha 的 RGBA ⇒ 加载时一次性转好，逐帧退化成整块拷贝。

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

        // RGBA(直通) → BGRA(预乘)，**原地**做：每个 4 字节只依赖它自己那 4 个字节 ⇒
        // 不需要第二份缓冲。（原先另开一个 `Vec::with_capacity(need)` 边读边写，峰值
        // 其实是 2×11.25 MiB，而注释写着"不留第二份"—— 注释是假的。）
        for px in rgba.chunks_exact_mut(4) {
            let [b, g, r, a] = premultiplied_bgra(px[0], px[1], px[2], px[3]);
            px[0] = b;
            px[1] = g;
            px[2] = r;
            px[3] = a;
        }

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
    /// 96 格的产物在 1.5× 下是 144，不做插值以免糊掉平涂边。
    pub fn blit(
        &self,
        row: u32,
        col: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
        dst_stride: usize,
    ) -> Result<(), AtlasError> {
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
            for x in 0..dst_w {
                let sx = (x * self.cell_w) / dst_w;
                let si = srow + (sx as usize) * 4;
                let di = drow + (x as usize) * 4;
                dst[di..di + 4].copy_from_slice(&self.pixels[si..si + 4]);
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

/// 测试用：把 RGBA 编成 lossless WebP（`image-webp` 的编码器只出 VP8L）。
pub fn encode_lossless_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    WebPEncoder::new(&mut out)
        .encode(rgba, width, height, ColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
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
}
