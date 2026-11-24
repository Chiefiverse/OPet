//! 窗口那块 DIB 的字节，与"这一屏上这个点有没有宠物"的唯一答案。
//!
//! ⚠️ 与 [`crate::layout`] 同一条规矩：**这里没有 `windows-sys` 类型**。`window.rs`（第 8 步）
//! 把 [`Stage::bytes`] 交给 `UpdateLayeredWindow`、把 [`Stage::hit`] 接给 `WM_NCHITTEST`，
//! 而两处数学都在宿主上被验过。这不是为了好看：屏上"缺一块"和"多一块"这两种症状
//! 都没有门禁能读出根因，只有把判据做成能在 Linux 上红的断言才算有证据。
//!
//! ## 为什么 Windows 读像素、Wayland 读 `polygon.json`，而这不是两个事实源
//!
//! `overlay_common::region` 那份矩形是 `tools/pack.py` 在**烘焙那一刻**从 alpha 算出来的
//! 预计算近似（`column_spans`，`tools/pack.py:109`），Wayland 用它是因为 `wl_region` 只收矩形。
//! Win32 的 `WM_NCHITTEST` 是逐点同步回答的，那就直接读**刚交出去的那份像素**本身。
//! 阈值同源 = [`ALPHA_HIT_THRESHOLD`]，比较式同形（`>` 而不是 `>=`）。
//! 把矩形近似搬到 Windows 上才会造出两个事实源：分叉的形态是屏上剪影与可点区域不一致，
//! 而那**只有手会摸出来**。

use overlay_common::atlas::{ALPHA_HIT_THRESHOLD, Atlas};
use petcore::assets::AccessoryManifest;
use petcore::behavior::AssetKey;

use crate::layout::Frame;

/// 一块窗口大小的 BGRA8 **预乘**缓冲。
///
/// 字节序是预乘 BGRA（小端 `Argb8888`）= `wl_shm` 与 `UpdateLayeredWindow(AC_SRC_ALPHA)`
/// **共同**要的那一份（D12 的对称美：`overlay_common::atlas` 出来的字节两边都不许再改一道）。
pub struct Stage {
    px: Vec<u8>,
    frame: Frame,
}

/// 一件**穿着中**的配件：清单（回答"这一档状态烘过像素吗"）+ 图集（供像素）。
///
/// 与 Wayland 侧 `app.rs` 的 `Equipped` 同形同理由：两个字段必须同时存在才画得对，
/// 铺成两个 `Option` 就能表示"有清单没像素"这种一画就错的组合。配件的命中并集在本平台
/// 不需要单独维护 —— `hit` 读的就是叠完之后的那份像素（模块头"为什么 Windows 读像素"）。
pub struct Worn {
    pub manifest: AccessoryManifest,
    pub atlas: Atlas,
}

impl Stage {
    pub fn new(frame: Frame) -> Self {
        Self {
            px: vec![0u8; frame.buffer_bytes()],
            frame,
        }
    }

    /// 换缩放 / 换显示器：重新分配并清空。
    ///
    /// 为什么必须重分配而不是"按最大档留着缓冲"：`CreateDIBSection` 的位图与 `bytes` 这一段
    /// 是同一批内存（第 8 步），尺寸与 `biWidth/biHeight` 不一致 = 越界写，而那不会崩、
    /// 只会把宠物右边裁掉或把别的数据画进窗口。
    pub fn resize(&mut self, frame: Frame) {
        self.frame = frame;
        self.px = vec![0u8; frame.buffer_bytes()];
    }

    pub fn frame(&self) -> Frame {
        self.frame
    }

    /// 交给 `UpdateLayeredWindow` 的那段字节（长度恒 = `frame().buffer_bytes()`）。
    pub fn bytes(&self) -> &[u8] {
        &self.px
    }

    /// 画一格：**整块清零后**把该格最近邻放到 margin 内侧。
    ///
    /// 那一次 `fill(0)` 不是保险丝而是必需 —— Windows 的 ULW 没有"这张 buffer 用完还给你"
    /// 的回执（Wayland 侧每个 `wl_buffer` 是独立的池对象），所以下一帧写的**常常是同一块内存**。
    /// 不清零的后果是拖影：宠物走过的位置留一层上一次的像素，而它只在移动时出现、
    /// 停下就没了，最容易被人读成"合成的问题"。
    ///
    /// 失败 ⇒ 缓冲区已经清零但没画上新格（"这一帧是空的"）。这是有意的：
    /// 半张宠物在屏上与"宠物缺了一块"同形，而空帧下一 tick 就被覆盖，不需要额外状态。
    pub fn paint(&mut self, atlas: &Atlas, row: u32, col: u32) -> Result<(), String> {
        self.paint_composited(atlas, None, (row, col), AssetKey::Idle)
    }

    /// 本体 + 穿着中配件的合成分：**整块清零 → 本体覆盖 → 配件 source-over**。
    ///
    /// 与 Wayland 的 `blit_cell`（`overlay-wayland/src/app.rs:2052`）**逐项同式**：
    /// - 顺序恒定"本体先、配件后"，反过来帽子会变成透明底；
    /// - `covers(key)` 那道门是**数据决定的可见性**：一份只声明 `idle` 的配件，在 `walk`
    ///   那几行就是不叠 —— 拿 `idle` 的像素往 `run` 的本体上叠，画的是别的东西，而屏上
    ///   看不出来；
    /// - 配件叠加失败 ⇒ **整趟作废**（返回 `Err`，调用方不得 present）："半顶帽子"在屏上
    ///   与"帽子歪了"同形，是又一种读不出根因的错。
    ///
    /// 与那边的一处**形状**差异（不是数学差异）：`body` 在这里不是 `Option` —— Wayland 的
    /// `blit_cell` 要兼画 `--test-rect`（两份图集都没有），本平台的矩形档走 [`Stage::paint_solid`]，
    /// 本体缺席在这里不是可达状态。`key` 只在 `worn` 在场时被读；`worn` 为 `None` 时随便给。
    pub fn paint_composited(
        &mut self,
        body: &Atlas,
        worn: Option<&Worn>,
        cell: (u32, u32),
        key: AssetKey,
    ) -> Result<(), String> {
        self.px.fill(0);
        let (pw, ph) = self.frame.cell_phys();
        let off = self.frame.cell_offset_bytes();
        let take = off.min(self.px.len());
        // `dst_w/dst_h` 用**缩放后**的格尺寸、`stride` 用整块跨距：与 `app.rs:4704 draw_cell`
        // 逐项同式。放大由 `Atlas::blit` 的最近邻做掉，ULW 只搬像素（它若自己缩放 = 糊边）。
        let (row, col) = cell;
        body.blit(
            row,
            col,
            &mut self.px[take..],
            pw as u32,
            ph as u32,
            self.frame.stride(),
        )
        .map_err(|e| format!("blit ({row},{col}) 失败: {e}"))?;
        if let Some(acc) = worn.filter(|a| a.manifest.covers(key)) {
            acc.atlas
                .blit_over(
                    row,
                    col,
                    &mut self.px[take..],
                    pw as u32,
                    ph as u32,
                    self.frame.stride(),
                )
                .map_err(|e| format!("配件叠加 ({row},{col}) 失败: {e}"))?;
        }
        Ok(())
    }

    /// 纯色矩形档（`--test-rect`）：margin 那圈保持**全透**、格内填**预乘品红**。
    ///
    /// 颜色与版式都照 Wayland 的 `test_rect_frame`（`overlay-wayland/src/app.rs`）：
    /// 同一份预乘品红 `(R=255,G=0,B=255,A=255)`，BGRA 内存序下 = 小端 `0xFFFF_00FF`，
    /// 同一圈 `SURFACE_MARGIN` 透明余量。
    /// 不是抄好看：那一档要答的两个问题（窗口映射上了吗 / 剪影外点得穿吗）在两个平台上
    /// 必须是**同一张图**，否则"Wayland 过了 Windows 没过"读不出是平台还是代码。
    ///
    /// 先 `fill(0)` 与 [`Self::paint`] 同一条理由：这块内存会被复用。
    pub fn paint_solid(&mut self) {
        self.px.fill(0);
        let (pw, ph) = self.frame.cell_phys();
        let stride = self.frame.stride();
        let off = self.frame.cell_offset_bytes();
        for y in 0..ph {
            let base = off + (y as usize) * stride;
            for x in 0..pw {
                let i = base + (x as usize) * 4;
                if let Some(slot) = self.px.get_mut(i..i + 4) {
                    slot.copy_from_slice(&[0xFF, 0x00, 0xFF, 0xFF]);
                }
            }
        }
    }

    /// 窗口物理像素 → 该点 alpha；越界给 `None`（**不许** `as usize` 直接把负数当索引）。
    pub fn alpha_at(&self, x: i32, y: i32) -> Option<u8> {
        let (w, h) = self.frame.surface_phys();
        if x < 0 || y < 0 || x >= w || y >= h {
            return None;
        }
        let i = (y as usize) * self.frame.stride() + (x as usize) * 4;
        // `i + 4 <= len` 由上面的宽高校验与 `buffer_bytes()` 的定义给出；这里仍用 `get`，
        // 因为 `resize` 与 `frame` 若哪天不同步，越界应当判"穿透"（点不动 = 用户还能用桌面），
        // 而不是 panic 掉整个宠物进程。
        self.px.get(i + 3).copied()
    }

    /// 命中判定：这一点是不是宠物。`false` = 穿透到桌面。
    ///
    /// 口径 = `alpha > ALPHA_HIT_THRESHOLD`（**严格大于**），与 `tools/pack.py:111` 的
    /// `alpha > ALPHA_THRESHOLD` 逐字同形 —— 那份近似与这里的读数共用一条阈值线，
    /// 两个平台的可点区域才是同一件事。改成 `>=` 会让 `a == 8` 那一圈抗锯齿边缘在
    /// Windows 上可点、在 Wayland 上不可点。
    ///
    /// ⚠️ 读的是 index **3**。本项目在 §6.1.20⑮ 已经因为 `step_by(4)` 踩到 index 0 错一次
    /// （那条门禁量的是蓝道而不是 alpha），而预乘数据下这个错的形态是**假阴性**：
    /// 深色不透明像素的蓝道恰好接近 0 ⇒ 被当成洞 ⇒ 症状是"宠物身上有一块点不动"。
    /// 判据钉在 `hit_reads_alpha_and_not_a_colour_channel`。
    pub fn hit(&self, x: i32, y: i32) -> bool {
        self.alpha_at(x, y)
            .is_some_and(|a| a > ALPHA_HIT_THRESHOLD as u8)
    }
}

#[cfg(test)]
mod tests {
    use overlay_common::atlas::fixture;

    use super::*;

    /// 缩放 1.0 的一帧，配 `fixture::probe_atlas()`（4 px 格）。
    ///
    /// ⚠️ 测例里的格尺寸**跟着图集走**（`fixture::CELL` = 4）而不是用真的 192：
    /// 探针像素是按格内整数坐标点名的，192 格上那些位置根本没有数据。
    /// 1.25 / 1.5 两档的换算在 [`crate::layout`] 里测，那里不需要真像素。
    fn probe_frame() -> Frame {
        Frame::new(1.0, fixture::CELL, fixture::CELL).expect("缩放 1.0 合法")
    }

    /// 格内整数逻辑坐标 → stage 像素（scale 1.0 下 `to_phys` 是恒等，所以能逐像素点名）。
    fn at(frame: &Frame, x: u32, y: u32) -> (i32, i32) {
        frame.cell_to_stage(x as f32, y as f32)
    }

    // ── ① 字节序 ────────────────────────────────────────────────────────────

    /// 源 `(R=10, G=20, B=30, A=255)` 到 DIB 必须是 `[30, 20, 10, 255]`。
    ///
    /// 三个通道互不相等 ⇒ 这一条**只有**字节序对了才可能绿。用白色当样本不行：
    /// `[255,255,255,255]` 在四种排列下逐位相同，那是一个永远读不出错的样本
    /// （同 §6.1.20⑮ 那条"量的不是要量的那一维"）。
    #[test]
    fn premultiplied_bytes_land_in_bgra_order() {
        let f = probe_frame();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::SOLID.0, fixture::SOLID.1)
            .expect("paint");
        let (x, y) = at(&f, 0, 0);
        let i = (y as usize) * f.stride() + (x as usize) * 4;
        assert_eq!(
            &s.bytes()[i..i + 4],
            &[30, 20, 10, 255],
            "BGRA 顺序不对（或预乘动了不该动的通道）"
        );
    }

    // ── ② 命中读的必须是 alpha，而且阈值那一边界要成对 ───────────────────────

    /// 四颗探针像素一次过。每一颗都注明"错的那把尺子会怎么答"，因为它们的全部意义就是分开。
    #[test]
    fn hit_reads_alpha_and_not_a_colour_channel() {
        let f = probe_frame();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::ALPHA_PROBES.0, fixture::ALPHA_PROBES.1)
            .expect("paint");

        let alpha = |p: (u32, u32)| {
            let (x, y) = at(&f, p.0, p.1);
            s.alpha_at(x, y)
        };
        let hit = |p: (u32, u32)| {
            let (x, y) = at(&f, p.0, p.1);
            s.hit(x, y)
        };

        // a == 8：等于阈值 ⇒ 穿透。这条与下一条**成对**，单独一条什么也不证明
        // （它只能证明"两边一致地错"）。
        assert_eq!(alpha(fixture::A_EQ_8), Some(8));
        assert!(!hit(fixture::A_EQ_8), "a==8 不该命中：判据是 > 不是 >=");
        // a == 9：阈值之上 ⇒ 命中。
        assert_eq!(alpha(fixture::A_EQ_9), Some(9));
        assert!(hit(fixture::A_EQ_9), "a==9 该命中");

        // 源 `(9,0,0,0)`：预乘后必须**四字节全零**，屏上不留一粒红点。
        // ⚠️ 这颗不区分"读哪个通道"（两个都答 0），它钉的是源→DIB 那一跳。
        let (rx, ry) = at(&f, fixture::RED_ZERO_ALPHA.0, fixture::RED_ZERO_ALPHA.1);
        let ri = (ry as usize) * f.stride() + (rx as usize) * 4;
        assert_eq!(
            &s.bytes()[ri..ri + 4],
            &[0, 0, 0, 0],
            "全透源像素必须落到全零"
        );
        assert!(!hit(fixture::RED_ZERO_ALPHA));

        // 源 `(255,0,0,200)` → 预乘 `[0, 0, 199, 200]`：**蓝道 0、alpha 200**。
        // ⇒ 读 index 3 命中，读 index 0（§6.1.20⑮ 那一族）判穿透。这颗才是通道判据。
        let (dx, dy) = at(&f, fixture::DARK_OPAQUE.0, fixture::DARK_OPAQUE.1);
        let di = (dy as usize) * f.stride() + (dx as usize) * 4;
        assert_eq!(s.bytes()[di], 0, "前提：这颗像素的**蓝道**必须是 0");
        assert_eq!(
            s.bytes()[di + 3],
            200,
            "前提：这颗像素的 **alpha** 必须是 200"
        );
        assert!(
            hit(fixture::DARK_OPAQUE),
            "深色不透明的像素点不动 = 读错了通道（或预乘把 alpha 弄丢了）"
        );
    }

    // ── ③ `backend_contract.rs:34` 那四个探针的移植 ──────────────────────────

    /// 剪影格：脚锚命中、包围盒角落穿透、帧上边界之外穿透、之内 1 px 命中。
    ///
    /// 与 `crates/petcore/tests/backend_contract.rs` 的
    /// `pointer_hits_the_pet_but_not_the_surface_margin` 逐条对应，只是那边过
    /// `polygon.json` 的近似、这边过真像素。**探针按格内相对坐标而不是绝对 px**，
    /// 那条纪律照抄过来（原注释：@96 时那次上探还有 94 px 余量、@192 只剩 4 px ⇒
    /// 绝对值再提一档就假红）。
    #[test]
    fn the_ported_contract_probes_still_answer_on_real_pixels() {
        let f = probe_frame();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::SILHOUETTE.0, fixture::SILHOUETTE.1)
            .expect("paint");

        // ① 脚下锚点必须命中宠物
        let (ax, ay) = f.cell_to_stage(fixture::FEET[0], fixture::FEET[1]);
        assert!(s.hit(ax, ay), "脚锚点穿透了 ⇒ 拖不起来、也点不到");

        // ② 包围盒左上角（margin 那一圈）必须穿透，否则桌面点不动
        assert!(!s.hit(0, 0), "窗口角上吃点击");
        let (w, h) = f.surface_phys();
        assert!(!s.hit(w - 1, h - 1), "右下角同理");
        // margin 那一圈**整圈**都要穿透。逐点查比"查一个角"贵不了多少，而角上恰好是
        // 最容易写对的一颗（`fill(0)` 之后它就是 0）。
        for k in 0..w {
            assert!(
                !s.hit(k, 0) || k >= f.margin_phys(),
                "顶边 margin 上有点 (k={k})"
            );
            assert!(!s.hit(k, h - 1), "底边 margin 上不该有点 (k={k})");
        }

        // ③④ 帧上边界**外** 1 逻辑 px 穿透、**内** 1 px 命中（后者是正面对照，
        //     防"永远不命中也绿"—— 那正是原注释里那 1 px 的全部作用）
        let (ox, _) = f.cell_to_stage(fixture::FEET[0], 0.0);
        let above = f.margin_phys() - 1;
        assert!(!s.hit(ox, above), "帧上边界之外不该吃输入");
        let inside = f.margin_phys();
        assert!(
            s.hit(ox, inside),
            "正面对照：帧内第一行必须命中，否则上一条是假绿"
        );
    }

    // ── ④ 串格 / 残留：画错的那一档会留下什么 ────────────────────────────────

    /// 整格全透的探针格画完，必须**一个字节都不剩**，且隔壁格的实心不能渗过来。
    ///
    /// 这一条打的是 `Atlas::cell_offset` 的跨距（`row * cell_h * width`，那个 `width` 是
    /// **整张 sheet 的宽**而不是格宽）：少乘一次就会串格，而串格的症状是"宠物边缘有条纹"，
    /// 五档计数与指纹都不看它。
    #[test]
    fn an_empty_cell_paints_nothing_and_bleeds_nothing() {
        let f = probe_frame();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::EMPTY.0, fixture::EMPTY.1)
            .expect("paint");
        assert!(
            s.bytes().iter().all(|&b| b == 0),
            "画一颗全透的格却留下了字节 ⇒ 取样串到了隔壁格（或没有先清零）"
        );
        let (x, y) = at(&f, 0, 0);
        assert!(!s.hit(x, y));
    }

    /// 换帧必须**盖掉**上一帧，不是叠上去。
    ///
    /// ULW 与 Wayland 的 `SlotPool` 最大的差别就在这里：Windows 没有"这块 buffer 还在飞"
    /// 的回执，下一帧写的常常是同一块内存。忘了清零 = 拖影，而拖影只在移动时出现。
    #[test]
    fn paint_replaces_the_previous_frame_rather_than_compositing() {
        let f = probe_frame();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::SOLID.0, fixture::SOLID.1)
            .expect("paint");
        let (x, y) = at(&f, 0, 0);
        assert!(s.hit(x, y), "前提：实心格该命中");

        s.paint(&a, fixture::EMPTY.0, fixture::EMPTY.1)
            .expect("paint");
        assert!(
            !s.hit(x, y),
            "上一帧的像素还在 ⇒ `paint` 没清零，移动时会拖影"
        );
    }

    // ── ⑤ 越界与换档 ────────────────────────────────────────────────────────

    /// 负坐标与越界一律"穿透"，**不许** panic、也不许回绕成一个合法下标。
    ///
    /// ⚠️ 这条的形状很眼熟：Wayland 侧"合成器取消指针 ⇒ `drag` 永远为真"（M4 复核 P0）
    /// 是同一个族的另一支 —— **外部输入的边界情况必须有一个不会伤人的答案**。
    /// 这里 `i32 as usize` 一旦为负就是天文数字，`px.get()` 给 `None` ⇒ 穿透，
    /// 而如果我写的是 `px[i]`，Debug 下 panic、Release 下 = 宠物进程没了。
    #[test]
    fn out_of_range_is_a_pass_through_not_a_panic() {
        let f = probe_frame();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::SILHOUETTE.0, fixture::SILHOUETTE.1)
            .expect("paint");
        let (w, h) = f.surface_phys();
        for (x, y) in [
            (-1, 0),
            (0, -1),
            (-9999, -9999),
            (w, 0),
            (0, h),
            (w + 1, h + 1),
            (i32::MIN, i32::MAX),
        ] {
            assert!(!s.hit(x, y), "({x},{y}) 越界还判命中");
            assert_eq!(s.alpha_at(x, y), None, "({x},{y}) 越界还有读数");
        }
    }

    /// DPI 变了（1.0 → 1.5）⇒ 缓冲按新档重分配，旧像素一格都不留。
    ///
    /// 第 8 步在 `WM_DPICHANGED` 里要走这条路；旧代码的教训是"尺寸换了而缓冲没换"
    /// 不会崩，只会把宠物右边裁掉一列。
    #[test]
    fn resize_follows_the_new_scale_and_clears() {
        let small = probe_frame();
        let mut s = Stage::new(small);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::SOLID.0, fixture::SOLID.1)
            .expect("paint");
        assert_eq!(s.bytes().len(), small.buffer_bytes());

        let big = Frame::new(1.5, fixture::CELL, fixture::CELL).expect("1.5 合法");
        s.resize(big);
        assert_eq!(s.frame(), big);
        assert_eq!(s.bytes().len(), big.buffer_bytes());
        assert_ne!(
            s.bytes().len(),
            small.buffer_bytes(),
            "前提：两档尺寸必须不同，否则这条在验空集"
        );
        assert!(s.bytes().iter().all(|&b| b == 0), "换档留下了上一帧的字节");
        // 新档下 margin 内侧那颗像素仍是可达的（换算跟着 frame 走，没有缓存旧尺寸）
        let (x, y) = s.frame().cell_to_stage(0.0, 0.0);
        assert_eq!(s.alpha_at(x, y), Some(0));
    }

    /// 格尺寸与图集不一致时，放大走 `Atlas::blit` 的最近邻 —— 而**不能**剩下黑边。
    ///
    /// 这一条测的是第 8 步那个"ULW 必须逐像素 1:1"的前提：`Stage` 的缓冲已经放大过，
    /// 所以 `srcRect` 与窗口尺寸相等。用 4 格图集配 2× 来演（真机是 192 在 1.5× 下出 288）。
    ///
    /// ⚠️ **遍历的是物理像素，不是 [`Frame::cell_to_stage`]。** 我第一版拿 `cell_to_stage`
    /// 去走 `0..8`，于是 2× 下那个入参被当成**逻辑** px ⇒ 物理上到了 `0..16`，
    /// 越出 8 px 的格、读进 margin 那圈全透明的余量里 ⇒ 假红。
    /// 这正是本项目"读错通道的尺子"那一族（§6.1.20⑮ 的 `step_by(4)` 踩到蓝道是同一种错）：
    /// **函数按它的契约用才有效，而"两套坐标系"的边界必须写在调用点上。**
    #[test]
    fn upscaling_happens_here_not_at_the_compositor() {
        let f = Frame::new(2.0, fixture::CELL, fixture::CELL).expect("2.0 合法");
        let (pw, ph) = f.cell_phys();
        assert_eq!((pw, ph), (8, 8), "前提：4 px 格在 2× 下是 8 px");
        let m = f.margin_phys();
        let mut s = Stage::new(f);
        let a = fixture::probe_atlas();
        s.paint(&a, fixture::SOLID.0, fixture::SOLID.1)
            .expect("paint");
        for dy in 0..ph {
            for dx in 0..pw {
                assert_eq!(
                    s.alpha_at(m + dx, m + dy),
                    Some(255),
                    "放大后物理格内 ({dx},{dy}) 空洞 ⇒ 有黑/透边，屏上是一圈描边"
                );
            }
        }
        // 正面对照：格**外**那一圈（margin）必须还是透明的，否则上面那条循环
        // 其实是在"整块都非零"上巧合通过的 —— 那它就什么都没测。
        assert_eq!(s.alpha_at(0, 0), Some(0), "窗口角上被画到了");
        assert_eq!(
            s.alpha_at(m - 1, m - 1),
            Some(0),
            "格左上角外 1 px 被画到了"
        );
    }

    /// `--test-rect` 那一档：格内**预乘品红**、margin 那圈**全透**、而 margin 那圈**点不动**。
    ///
    /// 这一档是 M4 第 3 步的原样移植，答的还是那两问（映射上了吗 / 剪影外点得穿吗），
    /// 所以它必须和 Wayland 的 `test_rect_frame` 出**同一张图**：颜色逐字节对，
    /// 而命中面恰好等于画出来的那块（`hit` 与 `alpha_at` 同源，但只有这一档里
    /// "画的 == 命中的"这件事是**用户能直接看见**的 —— 品红方块点得动、四周点不动）。
    #[test]
    fn test_rect_paints_magenta_inside_and_nothing_outside() {
        let f = Frame::new(1.0, fixture::CELL, fixture::CELL).expect("1.0 合法");
        let m = f.margin_phys();
        let (pw, ph) = f.cell_phys();
        let mut s = Stage::new(f);
        s.paint_solid();

        let (w, h) = s.frame().surface_phys();
        let mut painted = 0;
        for y in 0..h {
            for x in 0..w {
                let i = (y as usize) * s.frame().stride() + (x as usize) * 4;
                let px = &s.bytes()[i..i + 4];
                let in_cell = (m..m + pw).contains(&x) && (m..m + ph).contains(&y);
                if in_cell {
                    painted += 1;
                    // BGRA 小端下的预乘品红 = 0xFFFF_00FF 的四字节
                    assert_eq!(px, &[0xFF, 0x00, 0xFF, 0xFF], "({x},{y}) 格内不是品红");
                    assert!(s.hit(x, y), "画到的 ({x},{y}) 点不动");
                } else {
                    assert_eq!(px, &[0, 0, 0, 0], "({x},{y}) 格外不透明 ⇒ 吃掉桌面点击");
                    assert!(!s.hit(x, y), "格外 ({x},{y}) 竟可点");
                }
            }
        }
        assert_eq!(
            painted,
            (pw * ph) as usize,
            "前提：循环真覆盖到了整格，否则上面两问是在验空集"
        );
    }

    /// `paint_solid` 与 `paint` 复用同一块缓冲 ⇒ 先品红后图集不会漏色，反过来也是。
    ///
    /// 顺序两向都测：`--test-rect` 与真宠物在同一次运行里互斥，但**换档**（`resize` 之后
    /// 重画）走的都是同一对函数，`px.fill(0)` 少一次就是上一帧的字节混进这一帧。
    #[test]
    fn the_two_paint_paths_do_not_leave_each_others_bytes() {
        let a = fixture::probe_atlas();
        let f = Frame::new(1.0, fixture::CELL, fixture::CELL).expect("1.0 合法");
        let (pw, ph) = f.cell_phys();
        let m = f.margin_phys();

        let mut s = Stage::new(f);
        s.paint_solid();
        s.paint(&a, fixture::SOLID.0, fixture::SOLID.1)
            .expect("paint");
        for dy in 0..ph {
            for dx in 0..pw {
                let px = s.alpha_at(m + dx, m + dy);
                assert_ne!(px, Some(0), "({dx},{dy}) 被图集盖成透 ⇒ fill 顺序错");
            }
        }
        // 品红不许残留在格内任何一道的 R/G/B 上（`paint`  full-replace 的另一种露馅形状：
        // 只改 alpha 不改颜色 = 屏幕上宠物发紫）
        let i = (m as usize) * s.frame().stride() + (m as usize) * 4;
        assert_ne!(&s.bytes()[i..i + 2], &[0xFF, 0x00], "BG 还是品红");

        let mut t = Stage::new(f);
        t.paint(&a, fixture::SOLID.0, fixture::SOLID.1)
            .expect("paint");
        t.paint_solid();
        for dy in 0..ph {
            for dx in 0..pw {
                let i = ((m + dy) as usize) * t.frame().stride() + ((m + dx) as usize) * 4;
                assert_eq!(&t.bytes()[i..i + 4], &[0xFF, 0x00, 0xFF, 0xFF]);
            }
        }
        assert_eq!(t.alpha_at(0, 0), Some(0), "品红那圈把 margin 也填了");
    }

    // ── ⑥ 本体 + 配件合成（M5b：与 Wayland `blit_cell` 同一份数学）─────────────

    use petcore::assets::{Grid, StateTrack};
    use petcore::gacha::catalog::Slot;
    use std::collections::HashMap;

    /// 手搓一份只用来回答 `covers` 的配件清单（`covers` 只看 `states` 的键集，
    /// 完整校验是装备那一刻 `validate_against` 的事，不归这块画布管）。
    fn acc_manifest(states: &[&str]) -> AccessoryManifest {
        AccessoryManifest {
            format: 1,
            slug: "test_hat".into(),
            display_name: None,
            slot: Slot::Head,
            fits: vec![],
            sheet: "sheet.webp".into(),
            grid: Grid {
                cols: fixture::COLS as usize,
                rows: fixture::ROWS as usize,
                w: fixture::CELL,
                h: fixture::CELL,
            },
            states: states
                .iter()
                .map(|s| {
                    (
                        s.to_string(),
                        StateTrack {
                            rows: vec![0, 1],
                            fps: 5.0,
                            looped: None,
                            note: None,
                        },
                    )
                })
                .collect::<HashMap<_, _>>(),
        }
    }

    fn worn_with(atlas: Atlas, states: &[&str]) -> Worn {
        Worn {
            manifest: acc_manifest(states),
            atlas,
        }
    }

    /// 逐格**纯色**的图集（整格 4×4 同一颗直通 RGBA）。
    ///
    /// 为什么要自己造一份而不是连 `probe_atlas()` 调两次：合成判据要"本体一格、配件另一格
    /// 内容**不同**"，而同一份图集两侧只能取同一个 `(row,col)` —— 两边全同的叠加把
    /// "顺序写反"与"根本没叠"两种病都藏住（`composite_over(x, x)` 与 `x` 在 `a==0/255`
    /// 两档恰好等值，正是探针格挑出来的那两档）。
    /// `lossless` 往返 + 预乘都在 `Atlas::load` 真路径上走，不留测试专用的后门构造。
    fn sheet_atlas(
        dir: &std::path::Path,
        name: &str,
        cell_rgba: impl Fn(u32, u32) -> [u8; 4],
        rows: u32,
    ) -> Atlas {
        let w = fixture::COLS * fixture::CELL;
        let h = rows * fixture::CELL;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for cy in 0..rows {
            for cx in 0..fixture::COLS {
                let c = cell_rgba(cy, cx);
                for py in 0..fixture::CELL {
                    for px in 0..fixture::CELL {
                        let i = (((cy * fixture::CELL + py) * w + cx * fixture::CELL + px)
                            as usize)
                            * 4;
                        rgba[i..i + 4].copy_from_slice(&c);
                    }
                }
            }
        }
        let webp = overlay_common::atlas::encode_lossless_rgba(&rgba, w, h).expect("encode");
        let path = dir.join(format!("{name}.webp"));
        std::fs::write(&path, webp).expect("write");
        Atlas::load(
            path.to_str().expect("UTF-8"),
            fixture::COLS,
            rows,
            fixture::CELL,
            fixture::CELL,
        )
        .expect("load")
    }

    /// stage 像素缓冲里格内 `(x,y)` 的四字节（scale 1.0 ⇒ 物理 = 逻辑，点名无歧义）。
    fn px_at(f: &Frame, bytes: &[u8], x: i32, y: i32) -> [u8; 4] {
        let (gx, gy) = f.cell_to_stage(x as f32, y as f32);
        let i = (gy as usize) * f.stride() + (gx as usize) * 4;
        bytes[i..i + 4].try_into().expect("四字节")
    }

    /// **接线判据**（数学本身钉在 `overlay-common::atlas` 的单测，这里不重验公式）：
    /// ① `covers` 闸门真接在出图上 —— 只在 `idle` 声明过的配件，`run` 里一帧都不许叠；
    /// ② 叠的是 **source-over** 而不是覆盖 —— 半透明配件下本体颜色必须按手算混进结果；
    /// ③ 顺序是本体先、配件后 —— 不透明配件必须盖住本体（写反 = 本体盖帽子，红变蓝）。
    /// 期望值全部**手算写死**：配件 `(0,0)` 格直通 `[0,255,0,128]` → 预乘 `[0,128,0,128]`，
    /// 本体蓝 → 预乘 `[255,0,0,255]`；`keep = 255−128 = 127` ⇒
    /// `B: 0+(255·127+127)/255 = 127`，`G: 128+(0·127+127)/255 = 128+0`，
    /// `R: 0+(0·127+127)/255 = 0`，`A: 128+(255·127+127)/255 = 128+127` ⇒ `[127,128,0,255]`。
    #[test]
    fn worn_overlay_is_source_over_and_covers_gates_the_state() {
        let dir = tempfile::tempdir().expect("临时目录");
        let f = probe_frame();
        let body = sheet_atlas(dir.path(), "body", |_, _| [0, 0, 255, 255], fixture::ROWS);
        let hat = sheet_atlas(
            dir.path(),
            "hat",
            |r, c| {
                if (r, c) == (0, 0) {
                    [0, 255, 0, 128] // 半透明绿：判"混"而不是"盖"
                } else {
                    [255, 0, 0, 255] // 不透明红：判顺序
                }
            },
            fixture::ROWS,
        );
        let acc = worn_with(hat, &["idle"]);
        let mut s = Stage::new(f);

        // ② + ③：`idle` 覆盖的两格
        s.paint_composited(&body, Some(&acc), (0, 0), AssetKey::Idle)
            .expect("idle 该叠上");
        assert_eq!(
            px_at(&f, s.bytes(), 1, 1),
            [127, 128, 0, 255],
            "(0,0) 不是 source-over 的手算值 ⇒ 混合或取样错"
        );
        s.paint_composited(&body, Some(&acc), (0, 1), AssetKey::Idle)
            .expect("配件声明了 idle 的 rows=[0,1]，(0,1) 也该叠");
        assert_eq!(
            px_at(&f, s.bytes(), 1, 1),
            [0, 0, 255, 255],
            "(0,1) 不透明配件没盖住本体蓝 ⇒ 顺序反了（本体后画）或没叠"
        );

        // ①：同一件配件、同一个格，状态换成没声明过的 `run` ⇒ 逐字节回到纯本体
        s.paint_composited(&body, Some(&acc), (0, 0), AssetKey::Run)
            .expect("run 也该出图（只是不叠）");
        let (pw, ph) = f.cell_phys();
        for y in 0..ph {
            for x in 0..pw {
                assert_eq!(
                    px_at(&f, s.bytes(), x, y),
                    [255, 0, 0, 255],
                    "run 里帽子漏出来了 ({x},{y})"
                );
            }
        }
    }

    /// 命中读的是**叠完**的那份 alpha：本体全透 + 配件可点 ⇒ 命中；配件恰好压在线下
    /// （a==8）⇒ 叠完仍穿透。两颗配对：前一颗单独存在时，"配件从没画进缓冲"也能绿；
    /// 后一颗挡住"叠加把 alpha 拉满"的错。
    #[test]
    fn hit_follows_the_composite_not_just_the_body() {
        let dir = tempfile::tempdir().expect("临时目录");
        let f = probe_frame();
        let body = sheet_atlas(dir.path(), "clear", |_, _| [0, 0, 0, 0], fixture::ROWS);
        let hat9 = sheet_atlas(dir.path(), "hat9", |_, _| [255, 255, 255, 9], fixture::ROWS);
        let hat8 = sheet_atlas(dir.path(), "hat8", |_, _| [255, 255, 255, 8], fixture::ROWS);
        let mut s = Stage::new(f);
        let (hx, hy) = at(&f, 1, 1); // 格内 (1,1) → 窗口坐标（margin 之内）

        s.paint_composited(&body, None, (0, 0), AssetKey::Idle)
            .expect("裸本体");
        assert!(!s.hit(hx, hy), "前提：全透本体该点不动");

        s.paint_composited(
            &body,
            Some(&worn_with(hat9, &["idle"])),
            (0, 0),
            AssetKey::Idle,
        )
        .expect("帽子 a=9");
        assert!(
            s.hit(hx, hy),
            "帽子可点而叠完点不动 ⇒ 命中没读合成后的像素（或配件没叠）"
        );

        s.paint_composited(
            &body,
            Some(&worn_with(hat8, &["idle"])),
            (0, 0),
            AssetKey::Idle,
        )
        .expect("帽子 a=8");
        assert!(
            !s.hit(hx, hy),
            "a==8 压线：叠加后必须还是 8（阈值 `>` 与 Wayland/pack.py 同口径）"
        );
    }

    /// 两侧失败**分得开**（照 Wayland `blit_cell_names_the_side_that_failed`）：清单说 2 行
    /// 而配件像素只有 1 行 = "不同批产物"的形状，错必须点名是哪一侧，否则真机上
    /// "帽子没显示"和"宠物没了"是同一条日志。本体侧失败还顺带量"先清零"的次序：
    /// 缓冲必须还是全零（清零发生在取样之前）。
    #[test]
    fn paint_composited_names_the_side_that_failed() {
        let dir = tempfile::tempdir().expect("临时目录");
        let f = probe_frame();
        let body = sheet_atlas(dir.path(), "full", |_, _| [0, 0, 0, 0], fixture::ROWS);
        let short = sheet_atlas(dir.path(), "short", |_, _| [0, 0, 0, 0], 1);
        let mut s = Stage::new(f);

        let Err(err) = s.paint_composited(
            &body,
            Some(&worn_with(short, &["idle"])),
            (1, 0),
            AssetKey::Idle,
        ) else {
            panic!("配件图集只有 1 行，而行 1 被点名 ⇒ 必须失败");
        };
        assert!(
            err.starts_with("配件叠加"),
            "要指明失败的是配件那一侧: {err}"
        );
        assert!(err.contains("图集只有 1 行"), "要带上图集自己的说法: {err}");

        let Err(err) = s.paint_composited(&body, None, (5, 0), AssetKey::Idle) else {
            panic!("本体图集只有 2 行 ⇒ 行 5 必须失败");
        };
        assert!(err.starts_with("blit"), "本体侧不该被写成配件的错: {err}");
        assert!(
            s.bytes().iter().all(|&b| b == 0),
            "本体失败后缓冲不许留任何字节（先清零、后取样）"
        );
    }

    /// [`Stage::paint`] 这个包装必须**逐字节**等于"无配件的合成" —— 它现在被 `present`
    /// 与那批第一批冻结测例共走。哪天包装里被塞进第二份画格逻辑（或 `fill(0)` 少一次），
    /// 这条先红，而不是让矩形档/宠物档在屏上悄悄分叉。
    #[test]
    fn the_plain_paint_wrapper_is_exactly_a_composite_without_accessory() {
        let a = fixture::probe_atlas();
        for cell in [fixture::SILHOUETTE, fixture::ALPHA_PROBES, fixture::EMPTY] {
            let mut plain = Stage::new(probe_frame());
            plain.paint(&a, cell.0, cell.1).expect("paint");
            let mut comp = Stage::new(probe_frame());
            // `key` 在无配件时**不该被读** —— 故意给一个配件永不会声明的组合也无妨，
            // 这一条同时钉住"`worn` 缺席 ⇒ 不碰清单"。
            comp.paint_composited(&a, None, cell, AssetKey::Climb)
                .expect("paint_composited");
            assert_eq!(
                plain.bytes(),
                comp.bytes(),
                "cell {cell:?}：包装与合成分叉了"
            );
        }
    }

    /// 同参数连叠两次逐字节不变（ULW 复用同一块内存的那条老规矩在合成分上继续成立），
    /// 换回无配件也不能留帽子字节。
    #[test]
    fn compositing_is_replace_not_accumulate() {
        let dir = tempfile::tempdir().expect("临时目录");
        let f = probe_frame();
        let body = sheet_atlas(dir.path(), "body2", |_, _| [0, 0, 255, 255], fixture::ROWS);
        let acc = worn_with(
            sheet_atlas(dir.path(), "hat2", |_, _| [0, 255, 0, 128], fixture::ROWS),
            &["idle"],
        );
        let mut s = Stage::new(f);
        s.paint_composited(&body, Some(&acc), (0, 0), AssetKey::Idle)
            .expect("first");
        let once = s.bytes().to_vec();
        s.paint_composited(&body, Some(&acc), (0, 0), AssetKey::Idle)
            .expect("second");
        assert_eq!(s.bytes(), once, "连叠两次不逐字节同 ⇒ 在累加不是在替换");
        s.paint_composited(&body, None, (0, 0), AssetKey::Idle)
            .expect("脱下");
        for y in 0..f.cell_phys().1 {
            for x in 0..f.cell_phys().0 {
                assert_eq!(
                    px_at(&f, s.bytes(), x, y),
                    [255, 0, 0, 255],
                    "脱下后 ({x},{y}) 还留着绿 ⇒ 没先清零"
                );
            }
        }
    }
}
