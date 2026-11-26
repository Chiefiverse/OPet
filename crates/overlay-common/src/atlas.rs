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
use petcore::assets::Manifest;

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
    /// **分段存放里某一张自己读不来**（`AtlasSet::load` 专用）。
    ///
    /// 这一档存在的理由不是好看：三段装载时，"第 2 张尺寸不符"与"图集尺寸不符"是两句话，
    /// 前者告诉你**重烘哪一段**、后者什么都不告诉你。少了段序，装载失败在日志里就是一句
    /// 无法行动的谎（"这只宠物的像素坏了"，而像素其实有两张是好的）。
    Sheet {
        index: usize,
        file: String,
        reason: String,
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
            AtlasError::Sheet {
                index,
                file,
                reason,
            } => write!(f, "第 {index} 张（{file}）读不来: {reason}"),
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

    /// 只读**文件头**（尺寸、有无 alpha），一像素都不解。
    ///
    /// 它存在的唯一理由 = [`AtlasSet::reload_segments`] 的第一趟：整只换本体时，"第三张对不上"
    /// 必须在碰第一张的像素**之前**就知道 —— 分段把"半换"的代价从一只宠物放大成一只宠物的几段。
    /// 它**不是** [`Self::load`] 的替代品（解码长度、真正的解码都在后面）。
    /// 读进内存的是编码字节（磁盘侧 ≤4 MiB 那一档），不是解码后的像素 ⇒ 不碰峰值。
    pub fn peek(
        path: &str,
        cols: u32,
        rows: u32,
        grid_w: u32,
        grid_h: u32,
    ) -> Result<(), AtlasError> {
        let mut buf = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| AtlasError::Io(format!("{path}: {e}")))?
            .read_to_end(&mut buf)
            .map_err(|e| AtlasError::Io(format!("{path}: {e}")))?;
        let decoder = WebPDecoder::new(std::io::Cursor::new(buf))
            .map_err(|e| AtlasError::Decode(e.to_string()))?;
        let (width, height) = decoder.dimensions();
        let expected = (cols * grid_w, rows * grid_h);
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

/// 一只本体的**全部像素**：N 张 sheet 的集合，取格只有一个口。
///
/// 为什么要有这一层（而不是让客户端拿 `Vec<Atlas>` 自己索引）：分段存放（DESIGN §6.1.27
/// 裁决 1）之后，"全局第 row 行在哪一张"这件事**必须**由 [`Manifest::locate`] 回答。
/// 若每个取格点各自 `locate` 一次再索引，就有了 N 份"我猜它归第几张"的写法，而漏掉任何一次
/// 都是**静默取到另一张的同序号行** —— 症状是一只宠物身上混着两批像素，没有任何一处报错。
/// 这一层把那个形状收敛成 [`AtlasSet::at`] 一个函数：段序号只从这里出。
///
/// **惰性加载缝（批2 登记，§6.1.29）**：本结构当前是 eager 的——`load()` 一次性解码所有段。
/// 未来唯一的形变 = `sheets: Vec<Atlas>` → `Vec<Option<Atlas>>`（None = 已声明但未解码）。
/// `at()` / `blit()` 入口加一个"确已装载"判定，冷路径复用 [`AtlasSet::reload`] 作为装载原语；
/// `files()` / `segments()` / `decoded_bytes()` / `fits()` 需改为读描述符列表而非 `sheets.len()`。
/// 本批不实现惰性解码，只登记接口方向。
#[derive(Debug)]
pub struct AtlasSet {
    sheets: Vec<Atlas>,
    /// 装载那一刻从清单抄来的文件名，顺序 = 段序号（摘要与报错都读它，不许再拼）
    files: Vec<String>,
}

impl AtlasSet {
    /// 逐张装载：文件名与**每张自己的行数**都来自 [`Manifest::sheet_segments`]。
    ///
    /// ⚠️ 校验用的是那张**自己声明的高度**，不是全局 `grid.rows`：拿全局行数去比第二段必然
    /// 报"尺寸不符"，而那只宠物可能是好的 —— 那是我们把两个数混成一个的错，不是产物的错。
    /// 任一张读不来就整只失败（不留"两张里的一张"这种半只宠物），且点名是哪一张。
    pub fn load(dir: &std::path::Path, manifest: &Manifest) -> Result<Self, AtlasError> {
        let mut sheets = Vec::new();
        let mut files = Vec::new();
        for (index, (file, rows)) in manifest.sheet_segments().into_iter().enumerate() {
            let path = dir.join(file);
            let loaded: Result<Atlas, String> = match path.to_str() {
                None => Err("路径不是 UTF-8".to_owned()),
                Some(s) => Atlas::load(
                    s,
                    manifest.grid.cols as u32,
                    rows as u32,
                    manifest.grid.w,
                    manifest.grid.h,
                )
                .map_err(|e| e.to_string()),
            };
            match loaded {
                Ok(atlas) => {
                    sheets.push(atlas);
                    files.push(file.to_owned());
                }
                Err(reason) => {
                    return Err(AtlasError::Sheet {
                        index,
                        file: file.to_owned(),
                        reason,
                    });
                }
            }
        }
        Ok(Self { sheets, files })
    }

    pub fn len(&self) -> usize {
        self.sheets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sheets.is_empty()
    }

    /// 装载用的文件名表（段序）。摘要行"这张像素来自哪个文件"只能读它。
    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn sheet(&self, index: usize) -> Option<&Atlas> {
        self.sheets.get(index)
    }

    /// 逐段 `(文件名, 图集)`，序 = 段序。日志与摘要读它，不许自己拿 `files()` 去配 `sheet(i)`
    /// —— 那样两处任一为空就静默少打一行，而"两张 sheet 只报告了一张"正是本层要消灭的形状。
    pub fn segments(&self) -> impl Iterator<Item = (&str, &Atlas)> {
        self.files
            .iter()
            .zip(&self.sheets)
            .map(|(f, a)| (f.as_str(), a))
    }

    /// **本 crate 里唯一**把全局行号换成"哪一张的哪一行"的函数：换算整包交给
    /// [`Manifest::locate`]，这里只加两道它管不着的检查（段序在装载过的集合里、列在格子里）。
    /// 在别处再写一遍段表遍历 = 第二个换算口，那正是本层要消灭的东西。
    pub fn at<'a>(
        &'a self,
        manifest: &Manifest,
        row: u32,
        col: u32,
    ) -> Result<(&'a Atlas, u32, u32), AtlasError> {
        let (index, local) = manifest
            .locate(row as usize)
            .ok_or_else(|| AtlasError::Bounds {
                row,
                col,
                detail: format!(
                    "全局行 {row} 落不进任何一张（清单共 {} 行）",
                    manifest.total_rows()
                ),
            })?;
        let sheet = self.sheets.get(index).ok_or_else(|| AtlasError::Bounds {
            row,
            col,
            detail: format!(
                "清单说这一行在第 {index} 张，而那只本体只装来 {} 张",
                self.sheets.len()
            ),
        })?;
        if col >= sheet.cols {
            return Err(AtlasError::Bounds {
                row,
                col,
                detail: format!("列越界：那张只有 {} 列", sheet.cols),
            });
        }
        Ok((sheet, local as u32, col))
    }

    /// 按**段序**原地重载那一张（P5 换本体是整只换、走 [`Self::load`]；这一档留给"同一段的
    /// 像素要重读"那一族，比如惰性加载批）。参数与失败形状同 [`Atlas::reload`]。
    ///
    /// 失败一律翻成 [`AtlasError::Sheet`]：底层的 `Geometry`/`Decode`/`Io` 病因留在
    /// `reason` 文字里（那是"为什么"），段序与文件名是这一层才知道的"是哪一张"。
    /// 只有越界那一段没有名字可报 ⇒ `file` 留空，不许编一个。
    pub fn reload(&mut self, index: usize, path: &str) -> Result<(), AtlasError> {
        let have = self.sheets.len();
        let file = self.files.get(index).cloned().unwrap_or_default();
        let Some(sheet) = self.sheets.get_mut(index) else {
            return Err(AtlasError::Sheet {
                index,
                file,
                reason: format!("集合里只有 {have} 张"),
            });
        };
        sheet.reload(path).map_err(|e| AtlasError::Sheet {
            index,
            file,
            reason: e.to_string(),
        })
    }

    /// 当前集合能不能**原地**接住这份清单 = 段数与各段行数逐段相同。
    ///
    /// P5 换本体读它，判据故意**窄**：只有同段形才谈得上"原地"。不同段形（单张 ↔ 分段）
    /// 不是错误，只是接不住 ⇒ 调用方那条路走整只 [`Self::load`]，那一刻峰值是两只本体。
    /// 格子档（`cols/w/h`）**不在这里比**：行数相同而档不同是合法输入，它由
    /// [`reload_segments`](Self::reload_segments) 第一趟的 [`Atlas::peek`] 逐张兜住（按新清单的
    /// 期望尺寸报"图集尺寸不符"），而 [`Atlas::reload`] 自己还有一道同尺寸自检 ⇒ 两处都失败关闭。
    pub fn fits(&self, manifest: &Manifest) -> bool {
        let segs = manifest.sheet_segments();
        segs.len() == self.sheets.len()
            && segs
                .iter()
                .zip(&self.sheets)
                .all(|((_, rows), atlas)| *rows as u32 == atlas.rows)
    }

    /// **整只原地**换像素：逐段 [`Self::reload`]，并把 [`Self::files`] 换成新清单里的名字。
    ///
    /// 两趟是这条函数的全部形状：
    /// 1. 第一趟只 [`Atlas::peek`]（读文件头）⇒ 任何一张对不上都在**碰像素之前**退出，
    ///    留下的仍是原来那只完整的宠物。分段之后"半换"比单张时更糟（身上混两只宠物的段），
    ///    所以这一趟不是优化，是它存在的理由。
    /// 2. 第二趟逐段原地解（每段自己的三条自检照旧，见 [`Atlas::reload`]）。
    ///
    /// ⚠️ 残余风险照旧继承 [`Atlas::reload`] 那一段：头过了而 `read_image` 中途失败（文件被截断）
    /// 仍会留下一张残缺的。这里为此多做一步 —— 第二趟一旦失败，把**已经换掉的**那几张按旧名字
    /// 回读回去（回滚再失败会在 `reason` 里点名是哪一张），因为调用方在失败分支上打的是
    /// "本体没换"，而那句话必须是真的。前提 = 旧文件还在原地，这换了本体的那一刻成立。
    ///
    /// `files` 是**最后一步整体赋值**，不在循环里逐段改：回滚只还原像素，名字若已先走一步，
    /// 失败之后摘要与报错读的就是"新那只"的文件名（`a_body_that_passes_the_header_but_fails_to_decode_is_rolled_back`
    /// 抓的就是这个形状）。
    ///
    /// 两道防线各有一条"只有它能守住"的输入 ⇒ 都不可删：peek 那趟在**旧件已删**时是唯一防线
    /// （[`the_header_pass_is_the_only_defense_when_the_old_sheet_is_gone`]），回滚那步在**头过而解码崩**
    /// （截断件）时是唯一防线。
    pub fn reload_segments(
        &mut self,
        dir: &std::path::Path,
        manifest: &Manifest,
    ) -> Result<(), AtlasError> {
        let segs = manifest.sheet_segments();
        if !self.fits(manifest) {
            return Err(AtlasError::Sheet {
                index: self.sheets.len().min(segs.len()),
                file: String::new(),
                reason: format!(
                    "段形不同：集合 {} 段，清单 {} 段",
                    self.sheets.len(),
                    segs.len()
                ),
            });
        }
        for (index, &(file, _)) in segs.iter().enumerate() {
            let path = dir.join(file);
            // 期望尺寸取**这一段自己的**参数（`Atlas::reload` 待会儿比的正是这三个数）⇒
            // peek 是 reload 的严格子集：第一趟说过的每一张，第二趟都不会再翻脸。
            let a = &self.sheets[index];
            let reason = match path.to_str() {
                None => "路径不是 UTF-8".to_owned(),
                Some(s) => match Atlas::peek(s, a.cols, a.rows, a.cell_w, a.cell_h) {
                    Ok(()) => continue,
                    Err(e) => e.to_string(),
                },
            };
            return Err(AtlasError::Sheet {
                index,
                file: file.to_owned(),
                reason,
            });
        }
        let old_files = self.files.clone();
        let mut pending = Vec::with_capacity(segs.len());
        for (index, seg) in segs.iter().enumerate() {
            let file = seg.0.to_owned();
            let path = dir.join(&file).to_string_lossy().into_owned();
            if let Err(e) = self.reload(index, &path) {
                // 回滚已换掉的那几张（0..index）。失败也继续试完，把回滚的失败一起带进报错。
                // `files` 到此刻还是旧名字（下面 `pending` 只在全部成功后才落账）⇒ 回滚像素之后
                // 两个事实源自然对齐，不需要另外还原名字表。
                let mut rollback = String::new();
                for (j, back_name) in old_files.iter().enumerate().take(index) {
                    let back = dir.join(back_name).to_string_lossy().into_owned();
                    if let Err(r) = self.reload(j, &back) {
                        rollback.push_str(&format!("；回滚第 {j} 张也失败（{r}）"));
                    }
                }
                return Err(match e {
                    AtlasError::Sheet { reason, .. } => AtlasError::Sheet {
                        index,
                        file,
                        reason: format!("{reason}（已换的 {index} 张已回读旧件{rollback}）"),
                    },
                    other => other,
                });
            }
            pending.push(file);
        }
        self.files = pending;
        Ok(())
    }

    /// **全集**的解码字节数 = RSS 门禁的直接输入（单张时代它就是 `Atlas::decoded_bytes`）。
    pub fn decoded_bytes(&self) -> usize {
        self.sheets.iter().map(|a| a.decoded_bytes()).sum()
    }

    /// 逐格 blit（全局行号）。见 [`Self::at`] 为什么行号换算只能走一处。
    #[allow(clippy::too_many_arguments)]
    pub fn blit(
        &self,
        manifest: &Manifest,
        row: u32,
        col: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
        dst_stride: usize,
    ) -> Result<(), AtlasError> {
        let (sheet, local, col) = self.at(manifest, row, col)?;
        sheet
            .blit(local, col, dst, dst_w, dst_h, dst_stride)
            .map_err(|e| match e {
                // 段内行号对调用方没有意义（它给的是全局行），换回全局号再报，
                // 否则日志里那一行指不到任何一帧。
                AtlasError::Bounds { detail, .. } => AtlasError::Bounds { row, col, detail },
                other => other,
            })
    }

    /// 逐格叠加（source-over），形状与 [`Self::blit`] 完全一致 —— 共用 [`Self::at`]。
    #[allow(clippy::too_many_arguments)]
    pub fn blit_over(
        &self,
        manifest: &Manifest,
        row: u32,
        col: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
        dst_stride: usize,
    ) -> Result<(), AtlasError> {
        let (sheet, local, col) = self.at(manifest, row, col)?;
        sheet
            .blit_over(local, col, dst, dst_w, dst_h, dst_stride)
            .map_err(|e| match e {
                AtlasError::Bounds { detail, .. } => AtlasError::Bounds { row, col, detail },
                other => other,
            })
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
                AtlasError::Sheet { .. } => "Sheet",
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

    // ---- AtlasSet：分段存放（DESIGN §6.1.27 裁决 1「行号全局连续，sheet 只是分段存放」）----

    /// 4 个全局行各自一种纯色（RGBA）。行号是**全局**的，所以同一张表能横跨两段。
    /// 全用不透明 ⇒ 预乘退化成通道换序，像素判据读的是"取到哪一段"而不是取整。
    const ROW_RGBA: [[u8; 4]; 4] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 255, 255],
    ];

    /// 不透明时 `premultiply_in_place` 只做 RGBA→BGRA 换序，期望值可以直接写出。
    fn bgra(rgba: [u8; 4]) -> [u8; 4] {
        [rgba[2], rgba[1], rgba[0], rgba[3]]
    }

    /// 一整段像素：`cols` 恒 8（清单硬约束）、格子 4×4，第 `i` 行整行涂 `colors[i]`。
    fn solid_segment(colors: &[[u8; 4]]) -> Vec<u8> {
        let (cols, cell) = (8u32, 4u32);
        let (w, h) = (cols * cell, colors.len() as u32 * cell);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (r, col) in colors.iter().enumerate() {
            for y in 0..cell {
                for x in 0..w {
                    let i = (((r as u32 * cell + y) * w + x) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(col);
                }
            }
        }
        rgba
    }

    /// 分段清单：4 全局行拆成两张各 2 行（`a` 盖 0-1，`b` 盖 2-3）。
    fn split_manifest(a: &str, b: &str) -> Manifest {
        split_manifest_at(a, b, 2)
    }

    /// 同上，但**切法**可给：`first` 行进 `a`、其余进 `b`（1+3 与 2+2 段数相同而形状不同）。
    fn split_manifest_at(a: &str, b: &str, first: usize) -> Manifest {
        let second = 4 - first;
        Manifest::parse(&format!(
            r#"{{"format": 3, "slug": "split_probe",
                "grid": {{"cols": 8, "rows": 4, "w": 4, "h": 4}},
                "states": {{
                  "idle": {{"rows": [0, 1], "fps": 8, "loop": true}},
                  "walk": {{"rows": [2, 3], "fps": 8, "loop": true}}}},
                "anchor": {{"feet": [2, 3]}},
                "input_polygon": "auto_from_alpha",
                "sheets": [{{"file": "{a}", "rows": {first}}}, {{"file": "{b}", "rows": {second}}}]}}"#
        ))
        .expect("分段样本清单应当合法")
    }

    /// 单张老路径清单（`format = 2`、只有 `sheet`）。
    fn legacy_manifest(sheet: &str, rows: usize) -> Manifest {
        let track = (0..rows).collect::<Vec<_>>();
        Manifest::parse(&format!(
            r#"{{"format": 2, "slug": "legacy_probe", "sheet": "{sheet}",
                "grid": {{"cols": 8, "rows": {rows}, "w": 4, "h": 4}},
                "states": {{"idle": {{"rows": {track:?}, "fps": 8, "loop": true}}}},
                "anchor": {{"feet": [2, 3]}},
                "input_polygon": "auto_from_alpha"}}"#
        ))
        .expect("单张样本清单应当合法")
    }

    /// 写一段像素：行数 = `colors.len()` ⇒ 坏样本就是靠"少给一行颜色"造出来的。
    fn write_segment(dir: &std::path::Path, name: &str, colors: &[[u8; 4]]) {
        let webp = encode_lossless_rgba(&solid_segment(colors), 8 * 4, colors.len() as u32 * 4)
            .expect("编码");
        std::fs::write(dir.join(name), webp).expect("写临时段");
    }

    /// 走 `AtlasSet::blit` 真路径取全局 `(row, col)` 格的第 0 个像素（预乘 BGRA）。
    fn pixel(set: &AtlasSet, m: &Manifest, row: u32, col: u32) -> [u8; 4] {
        let mut dst = vec![0u8; (4 * 4 * 4) as usize];
        set.blit(m, row, col, &mut dst, 4, 4, 16).expect("取格");
        let mut out = [0u8; 4];
        out.copy_from_slice(&dst[..4]);
        out
    }

    /// 装载点名"第几张读不来"，而且**不跳过**：半只宠物在屏上的症状是身上混两批像素、
    /// 没有任何一处报错，所以这里必须是失败关闭 + 可行动的出处（重烘哪一段）。
    #[test]
    fn a_bad_second_sheet_is_named_not_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = split_manifest("sheet_a.webp", "sheet_b.webp");
        // 第二段只烘出 1 行（`--at` 抄错、或重烘中途被打断的形状），第一段是好的。
        write_segment(dir.path(), "sheet_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "sheet_b.webp", &ROW_RGBA[2..3]);
        // 单看这一张，底层只会说"图集尺寸不符"——它不知道自己是第几段。
        let solo = Atlas::load(
            &dir.path().join("sheet_b.webp").to_string_lossy(),
            8,
            2,
            4,
            4,
        );
        assert!(
            matches!(solo, Err(AtlasError::Geometry { .. })),
            "前提：底层那一句本来给不出段序: {solo:?}"
        );

        let err = AtlasSet::load(dir.path(), &m).expect_err("坏段必须整只失败，不许跳过后继续");
        match &err {
            AtlasError::Sheet {
                index,
                file,
                reason,
            } => {
                assert_eq!(*index, 1, "第二段坏掉不许报成第 0 张");
                assert_eq!(file, "sheet_b.webp", "要报**清单里**的文件名，重烘才有目标");
                assert!(
                    reason.contains("图集尺寸不符"),
                    "底层的病因不许丢: {reason}"
                );
            }
            other => panic!("分段装载失败必须报 Sheet 档，拿到 {other:?}"),
        }
        assert!(
            err.to_string().contains("第 1 张（sheet_b.webp）"),
            "日志要能直接行动: {err}"
        );

        // 换一张坏的 ⇒ 段序跟着走。少了这半，上面的 `index == 1` 可能只是恒打 1 的谎。
        std::fs::write(dir.path().join("sheet_a.webp"), b"not a webp at all").expect("写坏第一段");
        let first = AtlasSet::load(dir.path(), &m).expect_err("第一段坏掉也要失败");
        match first {
            AtlasError::Sheet {
                index,
                file,
                reason,
            } => {
                assert_eq!(
                    (index, file.as_str()),
                    (0, "sheet_a.webp"),
                    "点名必须跟着坏的那一张走"
                );
                assert!(reason.contains("WebP"), "病因还是那一句: {reason}");
            }
            other => panic!("拿到 {other:?}"),
        }
    }

    /// `decoded_bytes` 是 RSS 门禁（D16 ≤200 MiB）的直接输入 ⇒ 分段之后它必须给**全集**。
    /// 反向对照 = 同一批像素合成一张：总字节必须分毫不差，否则"只是换个存放"就不成立。
    #[test]
    fn the_set_reports_total_decoded_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = split_manifest("sheet_a.webp", "sheet_b.webp");
        write_segment(dir.path(), "sheet_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "sheet_b.webp", &ROW_RGBA[2..]);
        let set = AtlasSet::load(dir.path(), &m).expect("装载分段本体");
        assert_eq!(set.len(), 2, "两张都在，不是只装来第一张");
        assert!(!set.is_empty());
        let one = 8 * 4 * 2 * 4 * 4; // 8 列 × 4px × 2 行 × 4px × 4 字节
        assert_eq!(
            set.decoded_bytes(),
            one * 2,
            "全集读数 = 两张之和（只报第一张或最大那张，预算就少算一半）"
        );
        let sum: usize = (0..set.len())
            .map(|i| set.sheet(i).expect("段序在集合里").decoded_bytes())
            .sum();
        assert_eq!(set.decoded_bytes(), sum, "同一个预算只能有一把尺");

        // 反向对照：同样 4 行走单张路径 ⇒ 字节数与分段完全相等。
        let whole = legacy_manifest("all.webp", 4);
        write_segment(dir.path(), "all.webp", &ROW_RGBA);
        let single = AtlasSet::load(dir.path(), &whole).expect("装载单张本体");
        assert_eq!(single.len(), 1, "老路径就是一张，不许凭空多一段");
        assert_eq!(
            single.decoded_bytes(),
            set.decoded_bytes(),
            "分段只改存放：像素预算必须逐字节不变"
        );
    }

    /// 正面钉住"行号全局连续"这条裁决：两张各自的第 0 行在**命中表**与**像素**里都是两回事。
    /// 若 `Regions` 的键换成段内行号（`region.rs:145` 那个裸 `(row,col)`），第二条会覆盖第一条
    /// ⇒ 症状是某一行永远点不中，而装载与像素全绿，只有眼睛看不见。
    #[test]
    fn two_sheets_do_not_collide_in_the_region_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = split_manifest("sheet_a.webp", "sheet_b.webp");
        write_segment(dir.path(), "sheet_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "sheet_b.webp", &ROW_RGBA[2..]);
        // 全局行 0（第一段的第 0 行）与全局行 2（第二段的第 0 行）各写一份**不同**轮廓；
        // 行 1 给空 spans = 全透明帧（"什么都不遮"那一档），行 3 干脆不写 = 表里没有这格。
        std::fs::write(
            dir.path().join("polygon.json"),
            r#"{"cell": {"w": 4, "h": 4}, "threshold": 8, "frames": [
                 {"row": 0, "col": 0, "spans": [[0, 0, 3], [2, 1, 1]]},
                 {"row": 1, "col": 0, "spans": []},
                 {"row": 2, "col": 0, "spans": [[1, 0, 1]]}]}"#,
        )
        .expect("写 polygon");
        let regions = crate::region::Regions::load(
            &dir.path().join("polygon.json").to_string_lossy(),
            4,
            4,
            0.0,
        )
        .expect("读 polygon");
        let r0 = regions.rects(0, 0).expect("全局行 0 在表里");
        let r2 = regions.rects(2, 0).expect("全局行 2 在表里");
        assert_eq!(r0.len(), 2, "两个不相连的 run 不该被合并");
        assert_eq!(r2.len(), 1);
        assert_ne!(r0, r2, "段内同号的两行必须各自成一条记录，不许互相覆盖");
        assert_eq!(
            regions.rects(1, 0),
            Some(&[][..]),
            "全透明帧 = 空表（什么都不遮），与「表里没这格」是两件事"
        );
        assert_eq!(
            regions.rects(3, 0),
            None,
            "没写过的格子不该被别张的同序号行顶替"
        );

        // 像素那一半：`at` 给的是两个不同的 Atlas，段内行号却都是 0。
        let set = AtlasSet::load(dir.path(), &m).expect("装载分段本体");
        let (sa, la, _) = set.at(&m, 0, 0).expect("全局行 0");
        let (sb, lb, _) = set.at(&m, 2, 0).expect("全局行 2");
        assert_eq!(
            (la, lb),
            (0, 0),
            "两行在各自段内的序号相同 = 全局行号才是唯一的键"
        );
        assert!(
            !std::ptr::eq(sa, sb),
            "同序号行必须落在不同张上，否则它们就是同一个格"
        );
        assert_eq!(pixel(&set, &m, 0, 0), bgra(ROW_RGBA[0]));
        assert_eq!(pixel(&set, &m, 2, 0), bgra(ROW_RGBA[2]));
    }

    /// 装载用的文件名**只从清单来**：目录里另放一份名叫 `sheet.webp` 的诱饵，老写法
    /// （`app.rs:736` 那一族拼死文件名）在这份样本上会安静画出错的那只、且全链无错。
    #[test]
    fn sheet_names_come_from_the_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        // other.webp = 红/绿；诱饵 sheet.webp = 蓝/白。两份都合法，只有名字不同。
        write_segment(dir.path(), "other.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "sheet.webp", &ROW_RGBA[2..]);
        let named = legacy_manifest("other.webp", 2);
        let set = AtlasSet::load(dir.path(), &named).expect("按清单装载");
        assert_eq!(
            set.files().to_vec(),
            vec!["other.webp".to_owned()],
            "装载用的名字必须是从清单抄来的那一个"
        );
        assert_eq!(
            pixel(&set, &named, 0, 0),
            bgra(ROW_RGBA[0]),
            "清单点 other.webp，就许拿它的像素（拿到诱饵 = 文件名被拼死了）"
        );

        // 老路径的另一半：清单写的就是 sheet.webp ⇒ 照旧读它（这条不许被上面那半顺带改掉）。
        let plain = legacy_manifest("sheet.webp", 2);
        let set2 = AtlasSet::load(dir.path(), &plain).expect("单张老路径");
        assert_eq!(set2.files(), vec!["sheet.webp".to_owned()].as_slice());
        assert_eq!(pixel(&set2, &plain, 0, 0), bgra(ROW_RGBA[2]));

        // 分段路径同理，而且名字不必长成 sheet_0/sheet_1。
        let m3 = split_manifest("head.webp", "tail.webp");
        write_segment(dir.path(), "head.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "tail.webp", &ROW_RGBA[2..]);
        let set3 = AtlasSet::load(dir.path(), &m3).expect("装载自造段名的本体");
        assert_eq!(
            set3.files().to_vec(),
            vec!["head.webp".to_owned(), "tail.webp".to_owned()],
            "段序 = 清单里 sheets 的顺序"
        );
        assert_eq!(pixel(&set3, &m3, 0, 0), bgra(ROW_RGBA[0]));
        assert_eq!(pixel(&set3, &m3, 3, 0), bgra(ROW_RGBA[3]));
        // 全局行号越界要报"落不进任何一张"，不许回绕到某一张的第 0 行。
        let over = set3
            .at(&m3, 4, 0)
            .expect_err("4 行本体的第 4 行不存在（全局号从 0 数）");
        assert!(
            matches!(over, AtlasError::Bounds { .. }),
            "越界报 Bounds: {over:?}"
        );
    }

    /// 按段重载：**别的段一个字节都不许动**，坏段与越界段都不许留下半张新图。
    ///
    /// 计划第 ④ 步的四条测之外多加的一条：`AtlasSet::reload` 接受的是**段序**，
    /// 越界那一支若没人测，就是一个"永远不红的死分支"（`Atlas::reload` 的三条自检
    /// 只保证单张内不半换，管不到"索引指向集合外"）。
    #[test]
    fn a_segment_reloads_without_disturbing_the_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = split_manifest("sheet_a.webp", "sheet_b.webp");
        write_segment(dir.path(), "sheet_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "sheet_b.webp", &ROW_RGBA[2..]);
        let mut set = AtlasSet::load(dir.path(), &m).expect("装载分段本体");
        let bytes_before = set.decoded_bytes();
        let b_before = pixel(&set, &m, 2, 0);

        // 越界段序：报"只有 2 张"，不许 panic、也不许动任何一段。
        let err = set
            .reload(7, &dir.path().join("sheet_a.webp").to_string_lossy())
            .expect_err("第 7 段不存在");
        match err {
            AtlasError::Sheet {
                index,
                file,
                ref reason,
            } => {
                assert_eq!(index, 7);
                assert!(file.is_empty(), "越界那一段没有名字，不许编一个: {file}");
                assert!(reason.contains("只有 2 张"), "要给出集合的大小: {reason}");
            }
            other => panic!("越界要报 Sheet 档，拿到 {other:?}"),
        }
        assert_eq!(
            pixel(&set, &m, 0, 0),
            bgra(ROW_RGBA[0]),
            "越界重载不该动第一段"
        );

        // 坏件：段 1 换成单行张 ⇒ 失败关闭，像素与预算读数都留在原样。
        write_segment(dir.path(), "broken.webp", &ROW_RGBA[..1]);
        let bad = set
            .reload(1, &dir.path().join("broken.webp").to_string_lossy())
            .expect_err("档不同必须失败");
        assert!(
            matches!(bad, AtlasError::Sheet { index: 1, .. }),
            "重载失败也要点名是第几段: {bad}"
        );
        assert_eq!(pixel(&set, &m, 2, 0), b_before, "坏件不许留下半张新图");
        assert_eq!(set.decoded_bytes(), bytes_before, "失败之后预算读数不变");

        // 好件：段 1 换成"同样两行、颜色不同"的一张 ⇒ 只有段 1 变，段 0 与总字节不动。
        write_segment(dir.path(), "swapped.webp", &[ROW_RGBA[1], ROW_RGBA[0]]);
        set.reload(1, &dir.path().join("swapped.webp").to_string_lossy())
            .expect("同档的另一张必须成功");
        assert_eq!(
            pixel(&set, &m, 2, 0),
            bgra(ROW_RGBA[1]),
            "段 1 的像素要真的换掉"
        );
        assert_eq!(
            pixel(&set, &m, 0, 0),
            bgra(ROW_RGBA[0]),
            "段 0 不许被顺带换掉"
        );
        assert_eq!(
            set.decoded_bytes(),
            bytes_before,
            "同档重载 = 原地，不留第二份"
        );
    }

    /// P5 换本体的分段版：**要么整只换过去，要么一笔像素都不动**。
    ///
    /// 承重的那半是负例 2：坏的那张在**第二段**，而第一段是好的 ⇒ 没有"先逐张读头"那一趟的话，
    /// 第一段已经被换成新本体、函数却返回 Err，调用方打的是"本体没换"——屏上却是两只宠物的段
    /// 混在一起。这条测就是那一趟的证据（也是 `fits` 与"段形不同不许硬原地"的证据）。
    #[test]
    fn a_whole_body_swaps_in_place_or_not_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = split_manifest("sheet_a.webp", "sheet_b.webp");
        write_segment(dir.path(), "sheet_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "sheet_b.webp", &ROW_RGBA[2..]);
        let mut set = AtlasSet::load(dir.path(), &old).expect("装载起点本体");
        let bytes_before = set.decoded_bytes();

        // 正例：同样两段、换个名字与一套新像素 ⇒ 原地换掉，预算不变，名字表跟过去。
        let new = split_manifest("head.webp", "tail.webp");
        assert!(
            set.fits(&new),
            "同段形必须报 fits，否则调用方会白付一次双份峰值"
        );
        write_segment(dir.path(), "head.webp", &[ROW_RGBA[3], ROW_RGBA[0]]);
        write_segment(dir.path(), "tail.webp", &[ROW_RGBA[1], ROW_RGBA[2]]);
        set.reload_segments(dir.path(), &new)
            .expect("同段形的整只换必须成功");
        assert_eq!(
            set.files().to_vec(),
            vec!["head.webp".to_owned(), "tail.webp".to_owned()],
            "摘要与报错读的文件名要跟着新清单走（旧名字留着 = 两个事实源）"
        );
        assert_eq!(
            pixel(&set, &new, 0, 0),
            bgra(ROW_RGBA[3]),
            "段 0 换成了新件"
        );
        assert_eq!(
            pixel(&set, &new, 2, 0),
            bgra(ROW_RGBA[1]),
            "段 1 换成了新件"
        );
        assert_eq!(
            set.decoded_bytes(),
            bytes_before,
            "整只原地换不留第二份缓冲"
        );

        // 负例 1：段形不同（单张 ↔ 分段）⇒ 报"段形不同"，像素一个字节都不动。
        let single = legacy_manifest("one.webp", 4);
        write_segment(dir.path(), "one.webp", &ROW_RGBA);
        assert!(!set.fits(&single), "一段 vs 两段必须报不 fits");
        let err = set
            .reload_segments(dir.path(), &single)
            .expect_err("段形不同不许硬原地");
        assert!(
            matches!(&err, AtlasError::Sheet { reason, .. } if reason.contains("段形不同")),
            "要说出为什么接不住: {err}"
        );
        assert_eq!(
            pixel(&set, &new, 0, 0),
            bgra(ROW_RGBA[3]),
            "被拒之后仍是刚才那只"
        );

        // 负例 1b：**段数相同而切法不同**（1+3 vs 2+2）⇒ 一样不许硬原地。
        // 这一格管的是"客户端选哪条路"，不是报错文案：`swap_body` 读 `fits` 决定原地换还是整只
        // `load`，`fits` 若只数段数，一条本来走得通的整只换会被换成一条必然失败的原地换
        // （`fits-blind-to-rows` 变异 = 全绿 ⇒ 曾没有测例看着它）。
        let uneven = split_manifest_at("uneven_a.webp", "uneven_b.webp", 1);
        write_segment(dir.path(), "uneven_a.webp", &ROW_RGBA[..1]);
        write_segment(dir.path(), "uneven_b.webp", &ROW_RGBA[1..]);
        assert!(
            !set.fits(&uneven),
            "同段数不同切法必须报不 fits（逐段行数就是比这个）"
        );
        let err = set
            .reload_segments(dir.path(), &uneven)
            .expect_err("切法不同接不住，要退回整只装载");
        assert!(
            matches!(&err, AtlasError::Sheet { reason, .. } if reason.contains("段形不同")),
            "要说出为什么接不住: {err}"
        );
        assert_eq!(
            [pixel(&set, &new, 0, 0), pixel(&set, &new, 2, 0)],
            [bgra(ROW_RGBA[3]), bgra(ROW_RGBA[1])],
            "被拒之后两只都没动"
        );

        // 负例 2（承重）：**第二段**坏、第一段好 ⇒ 第一趟就要退，第一段一个像素都不许换。
        let also = split_manifest("good.webp", "broken.webp");
        assert!(set.fits(&also), "段形相同，坏的是内容不是形状");
        write_segment(dir.path(), "good.webp", &[ROW_RGBA[2], ROW_RGBA[2]]);
        write_segment(dir.path(), "broken.webp", &ROW_RGBA[..1]);
        let err = set
            .reload_segments(dir.path(), &also)
            .expect_err("坏段必须失败");
        match &err {
            AtlasError::Sheet {
                index,
                file,
                reason,
            } => {
                assert_eq!(
                    (*index, file.as_str()),
                    (1, "broken.webp"),
                    "点名坏的那一张"
                );
                assert!(reason.contains("图集尺寸不符"), "病因要留住: {reason}");
            }
            other => panic!("拿到 {other:?}"),
        }
        assert_eq!(
            [pixel(&set, &new, 0, 0), pixel(&set, &new, 2, 0)],
            [bgra(ROW_RGBA[3]), bgra(ROW_RGBA[1])],
            "失败之后屏上还是同一只 —— 第一段没被换成新本体（那趟 peek 的意义）"
        );
        assert_eq!(
            set.files().to_vec(),
            vec!["head.webp".to_owned(), "tail.webp".to_owned()],
            "名字表也不许先走一步"
        );
    }

    /// 第一趟 peek 唯一不可替代的那一格 = **旧件已经不在盘上**，回滚这条路本身断了。
    ///
    /// 上一条测里"坏段"被接住其实有两道防线（先逐张读头、失败后回读旧件），互为冗余 ⇒ 只问那条
    /// 测红不红，答不了"第一趟是不是死重"（现场事实第 10 条同族：一条防线被另一条遮住时，删掉它
    /// 测例照样绿）。这一条把冗余拆掉：旧文件删了，"没碰像素"只能由第一趟保证。
    /// ⚠️ 反过来它也只是**防线顺序**的证据，不是最常见失败形状的证据 —— 真机上旧件通常还在。
    #[test]
    fn the_header_pass_is_the_only_defense_when_the_old_sheet_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = split_manifest("gone_a.webp", "gone_b.webp");
        write_segment(dir.path(), "gone_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "gone_b.webp", &ROW_RGBA[2..]);
        let mut set = AtlasSet::load(dir.path(), &old).expect("装载起点本体");
        std::fs::remove_file(dir.path().join("gone_a.webp")).expect("删掉第一段旧件");
        std::fs::remove_file(dir.path().join("gone_b.webp")).expect("删掉第二段旧件");
        let before = [pixel(&set, &old, 0, 0), pixel(&set, &old, 2, 0)];

        let bad_tail = split_manifest("good.webp", "broken.webp");
        // 新第一段与旧第一段**不同色**（旧 = 红/绿）⇒ 换过没换过在像素上看得见；同色的话
        // "半换"与"整只没动"是同一个读数，这条测就成了恒绿的假仪器。
        write_segment(dir.path(), "good.webp", &[ROW_RGBA[3], ROW_RGBA[2]]);
        write_segment(dir.path(), "broken.webp", &ROW_RGBA[..1]);
        let err = set
            .reload_segments(dir.path(), &bad_tail)
            .expect_err("第二段坏必须整体失败");
        assert!(
            matches!(&err, AtlasError::Sheet { index: 1, file, .. } if file == "broken.webp"),
            "拿到 {err:?}"
        );
        assert!(
            !err.to_string().contains("回滚"),
            "旧件已删 ⇒ 守住只可能是第一趟的功劳，报错里不许出现回滚的痕迹: {err}"
        );
        assert_eq!(
            [pixel(&set, &old, 0, 0), pixel(&set, &old, 2, 0)],
            before,
            "第一段一个像素都没动（回滚在这里做不到）"
        );
        assert_eq!(
            set.files().to_vec(),
            vec!["gone_a.webp".to_owned(), "gone_b.webp".to_owned()],
            "名字表也不许先走一步"
        );
    }

    /// 回滚唯一不可替代的那一格 = **文件头过了、解码中途崩**（截断的 webp）。
    ///
    /// 与上一条配成一对：那条把回滚拆掉（旧件删了），这一条把 peek 拆掉（截断件读得出尺寸、
    /// 解不出像素）⇒ 两道防线各自都有一条"只有它能守住"的输入，谁都不是死重。
    /// 前提那句 `peek` 必须 `Ok` 是这条测的意义所在：否则红的是第一趟，不是回滚。
    /// ⚠️ 顺带一条实测：截断 1 字节的 lossless webp 连 `reload` 都能过（探针里 163/164 = `Ok`）
    /// ⇒ 体积/像素类门禁管不到"少一个字节"，那归 zip 的 CRC 与 sha256（见 §6.1.27）。
    #[test]
    fn a_body_that_passes_the_header_but_fails_to_decode_is_rolled_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = split_manifest("keep_a.webp", "keep_b.webp");
        write_segment(dir.path(), "keep_a.webp", &ROW_RGBA[..2]);
        write_segment(dir.path(), "keep_b.webp", &ROW_RGBA[2..]);
        let mut set = AtlasSet::load(dir.path(), &old).expect("装载起点本体");
        let before = [pixel(&set, &old, 0, 0), pixel(&set, &old, 2, 0)];
        let bytes_before = set.decoded_bytes();

        // 新第一段用**另一套颜色**：照抄旧件的话，"换过了"与"回滚了"在像素上不可分辨，
        // 这条测就成了恒绿的假仪器（`rollback-off` 变异第一次就是这么活下来的）。
        let whole =
            encode_lossless_rgba(&solid_segment(&ROW_RGBA[2..]), 8 * 4, 2 * 4).expect("编码");
        assert_ne!(
            bgra(ROW_RGBA[2]),
            before[0],
            "前提：新第一段要与旧第一段不同色，否则回滚有没有发生根本看不见"
        );
        std::fs::write(dir.path().join("ok.webp"), &whole).expect("写完整件");
        std::fs::write(dir.path().join("trunc.webp"), &whole[..whole.len() / 2]).expect("写截断件");
        let trunc = dir.path().join("trunc.webp");
        let trunc = trunc.to_string_lossy().into_owned();
        assert!(
            Atlas::peek(&trunc, 8, 2, 4, 4).is_ok(),
            "前提：截断件要能过第一趟，否则这条测的不是回滚"
        );
        let mut probe = Atlas::load(&dir.path().join("ok.webp").to_string_lossy(), 8, 2, 4, 4)
            .expect("完整件装载");
        assert!(
            matches!(probe.reload(&trunc), Err(AtlasError::Decode(_))),
            "前提：同一个文件在 `reload` 那里必须失败"
        );

        let half_new = split_manifest("ok.webp", "trunc.webp");
        let err = set
            .reload_segments(dir.path(), &half_new)
            .expect_err("解不开的段必须整体失败");
        assert!(
            matches!(&err, AtlasError::Sheet { index: 1, file, .. } if file == "trunc.webp"),
            "拿到 {err:?}"
        );
        assert_eq!(
            [pixel(&set, &old, 0, 0), pixel(&set, &old, 2, 0)],
            before,
            "第一段先换过去又解不开第二段 ⇒ 必须回读旧件，屏上仍是原来那只"
        );
        assert_eq!(
            set.files().to_vec(),
            vec!["keep_a.webp".to_owned(), "keep_b.webp".to_owned()],
            "回滚之后名字表也要留在旧的那只"
        );
        assert_eq!(set.decoded_bytes(), bytes_before, "失败之后预算读数不变");
    }
}
