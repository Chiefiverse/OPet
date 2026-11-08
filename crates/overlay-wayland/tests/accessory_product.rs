//! 对 **`tools/pack.py --accessory` 真产出的那份配件产物**走一遍装备链。
//!
//! 这条测试存在的理由不是"再验一次 `AccessoryManifest`"（那个模块里有 24 条单测，全部拿
//! **当场构造的 JSON** 跑），而是补上那 24 条结构上碰不到的一段：**生产者产出的字节**。
//! "一个开关从来没被非默认值用过就等于它不存在"（AGENTS「--cell 假开关」那条）在这里的同族
//! 形状是"一份清单从来没有真产物走过，只有手写 fixture 走过"。
//!
//! 默认 `#[ignore]`：`assets/` 是烘焙产物、不进仓库。本机跑（先 pack 一份配件）：
//! ```sh
//! OPET_ACCESSORY=assets/accessories/cap_test/accessory.json \
//! OPET_BODY=assets/pets/tiger/pet.json \
//!   cargo test -p overlay-wayland --test accessory_product -- --ignored --nocapture
//! ```
//! **缺产物 = 失败，不是 SKIP。** 这条测试带 `#[ignore]` ⇒ `cargo test --workspace` 永远不会跑到
//! 它，唯一让它执行的途径是有人显式写 `--ignored`。在那个前提下"打印 SKIP 然后返回 `()`"
//! 产出的是一行 `1 passed`，而**它什么也没验** —— 这正是本项目反复出现的"计数器为 0 不等于
//! 代码对"那一族，且这条尤其实测：2026-11-02 我漏给 `OPET_ACCESSORY` 跑了一次，屏上就是
//! `SKIP：没给 OPET_ACCESSORY` + `test result: ok`。所以这里三个缺件分支全部 `panic`。
//! （同族的 `real_asset.rs` 已于同日改成同一个口径：缺件 = `panic!`，不再 SKIP-返回。）

use std::io::Read;
use std::path::{Path, PathBuf};

use image_webp::WebPDecoder;
use overlay_wayland::atlas::Atlas;
use overlay_wayland::region::Regions;
use petcore::assets::{AccessoryManifest, Manifest};
use petcore::behavior::AssetKey;
use petcore::intent::SURFACE_MARGIN;

/// 与 `tools/pack.py` 的 `ALPHA_THRESHOLD`、`tools/validate.py` 同一口径（三处必须一起改）。
const SPAN_ALPHA_THRESHOLD: u32 = 8;

/// 把 sheet 解成**直通 RGBA**（`Atlas` 里那份是预乘 BGRA 且私有，这里要的是原始 alpha）。
fn decode_rgba(path: &Path) -> Vec<u8> {
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("开 {} 失败: {e}", path.display()))
        .read_to_end(&mut buf)
        .unwrap_or_else(|e| panic!("读 {} 失败: {e}", path.display()));
    let mut decoder =
        WebPDecoder::new(std::io::Cursor::new(buf)).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let need = decoder.output_buffer_size().expect("sheet 尺寸超出 usize");
    let mut rgba = vec![0u8; need];
    decoder
        .read_image(&mut rgba)
        .unwrap_or_else(|e| panic!("解码 {:?}: {e}", path));
    rgba
}

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// 装备链的四步，与 `app.rs::check_accessory` + `decode` 逐句同序：清单 → 比对 → 轮廓齐备 →
/// `wl_region` 并集，最后才是两张 sheet。**顺序本身就是被断言的东西**：`validate_against` 在解码之前。
/// 生产路径 additionally 把这些包在库扫描里（`discover_accessories` ⇒ 一件坏了只跳过它），
/// 那层由 `app.rs` 的单测拿合成 fixture 覆盖；这条只补"真产物的字节"。
#[test]
#[ignore = "需要 pack.py --accessory 真产出的配件产物（仓库里没有）"]
fn packed_accessory_equips_against_its_body() {
    let acc = std::env::var("OPET_ACCESSORY")
        .unwrap_or_else(|_| panic!("没给 OPET_ACCESSORY（配件清单路径）⇒ 这条测试没验任何东西。先跑 tools/pack.py --accessory"));
    let body = std::env::var("OPET_BODY")
        .unwrap_or_else(|_| panic!("没给 OPET_BODY（本体清单路径）⇒ 这条测试没验任何东西"));
    let (acc_path, body_path) = (repo(&acc), repo(&body));
    if !acc_path.exists() || !body_path.exists() {
        panic!(
            "{} / {} 有一个不存在（先跑 tools/pack.py --accessory，三件产物须同目录）",
            acc_path.display(),
            body_path.display()
        );
    }

    let body = Manifest::load(body_path.to_str().unwrap()).expect("本体清单读不出");
    let acc = AccessoryManifest::load(acc_path.to_str().unwrap()).expect("配件清单读不出");

    // ① 生产者没往配件清单里塞第二个事实源。这条查的是**原始字节**而不是结构体：serde 默认
    //    忽略未知字段，所以"pack.py 多写了 anchor"这件事在 `parse` 上是无声的。
    let raw = std::fs::read_to_string(&acc_path).unwrap();
    for banned in [
        "\"anchor\"",
        "\"facing\"",
        "\"input_polygon\"",
        "\"outfit_anchors\"",
    ] {
        assert!(
            !raw.contains(banned),
            "配件清单写了 {banned} ⇒ 那是本体才有的字段，两张图集同一台相机时它必然等于本体那份，\
             存两份就是两个事实源（§9.2 第 17 条那次挂点账的成因）"
        );
    }

    // ② 这条路径存在的全部理由：真产物过得了 `validate_against`。
    acc.validate_against(&body)
        .unwrap_or_else(|e| panic!("真产物过不了 validate_against: {e}"));

    // ③ 反面对照：这条比对不是白跑的。改掉一项就必须红，否则②证明不了任何事。
    let mut tampered = acc.clone();
    tampered.grid.w = body.grid.w.saturating_sub(1).max(1);
    assert!(
        tampered.validate_against(&body).is_err(),
        "格子尺寸被改小一格而 validate_against 仍然放行 ⇒ ② 那条绿是假的"
    );
    let mut tampered = acc.clone();
    if let Some(track) = tampered.states.values_mut().next() {
        track.rows.reverse();
    }
    assert!(
        tampered.validate_against(&body).is_err(),
        "行表被反转而 validate_against 仍然放行 ⇒ 「贴到本体物理行号」这件事没人守"
    );

    // ④ 两张 sheet 的几何：配件那格的矩形必须与本体那格**逐维相等**（"偏移恒为 0"的像素侧）。
    let dir = acc_path.parent().unwrap();
    let body_dir = body_path.parent().unwrap();
    let acc_atlas = Atlas::load(
        dir.join("sheet.webp").to_str().unwrap(),
        acc.grid.cols as u32,
        acc.grid.rows as u32,
        acc.grid.w,
        acc.grid.h,
    )
    .expect("配件 sheet.webp 解不出");
    let body_atlas = Atlas::load(
        body_dir.join("sheet.webp").to_str().unwrap(),
        body.grid.cols as u32,
        body.grid.rows as u32,
        body.grid.w,
        body.grid.h,
    )
    .expect("本体 sheet.webp 解不出");
    assert_eq!(
        acc_atlas.width, body_atlas.width,
        "列宽不一致 ⇒ 同一 col 不是同一 x"
    );
    assert!(
        acc_atlas.height <= body_atlas.height,
        "配件图集 {} px 比本体 {} px 还高 ⇒ 声明的行越过了本体自己的表",
        acc_atlas.height,
        body_atlas.height
    );

    // ⑤ 轮廓：配件**会被画到的每一格**都得有自己的条目，缺格 = "画得出来而点不到"。
    let acc_regions = Regions::load(
        dir.join("polygon.json").to_str().unwrap(),
        acc.grid.w,
        acc.grid.h,
        SURFACE_MARGIN,
    )
    .expect("配件 polygon.json 读不出");
    let cells = acc.drawn_cells();
    if let Some(missing) = acc_regions.first_missing(&cells) {
        panic!(
            "配件 polygon.json 没有格 {:?}：清单声明 {} 格而轮廓只有 {} 格",
            missing,
            cells.len(),
            acc_regions.frame_count()
        );
    }
    assert_eq!(
        acc_regions.frame_count(),
        cells.len(),
        "轮廓条目数 ≠ 声明格数 ⇒ 多出来的格是从哪来的？"
    );

    // ⑤′ **像素与轮廓同空同满**（这条是这一轮真抓到的缺陷，不是加固）。pack.py 的配件轮廓循环
    //    曾把列的右边界写成 `(row + 1) * cell` ⇒ 凡是 `col > row` 的格子切到一个零宽数组，`spans`
    //    写成空列表，而**清单、行表、sheet 三件全对**。上面②与⑤只比键集、那条拼接断言在 `a = 0`
    //    时照样绿 ⇒ 这一族的错只有"拿像素比轮廓"才看得见。后果不是画不出来而是点不到：
    //    那格 0 个矩形 = 全透明穿透，配件的像素上屏了却点不到（"看得见摸不着"）。
    let raw_poly = std::fs::read_to_string(dir.join("polygon.json")).unwrap();
    let declared_threshold: u32 = serde_json::from_str::<serde_json::Value>(&raw_poly)
        .expect("配件 polygon.json 不是合法 JSON")
        .get("threshold")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or_else(|| panic!("配件 polygon.json 没写 threshold ⇒ 拿哪个阈值算的不可核对"));
    assert_eq!(
        declared_threshold, SPAN_ALPHA_THRESHOLD,
        "轮廓用阈值 {declared_threshold} 而像素侧按 {SPAN_ALPHA_THRESHOLD} 数 ⇒ 两边不同口径，同空同满这条白跑"
    );
    let px = decode_rgba(&dir.join("sheet.webp"));
    let (cw, ch) = (acc.grid.w as usize, acc.grid.h as usize);
    // 下面按 `stride` 取格的前提是**每行没有填充**；解码器若做了行对齐，整张表会静默错位，
    // 而"同空同满"会报出一堆看不出根因的红 ⇒ 先把这个前提钉住。
    assert_eq!(
        px.len(),
        acc_atlas.width as usize * acc_atlas.height as usize * 4,
        "解码缓冲不等于 width*height*4 ⇒ 每行有填充，按 stride 取格会整体错位"
    );
    let stride = px.len() / acc_atlas.height as usize;
    let mut disagree: Vec<String> = Vec::new();
    for (row, col) in &cells {
        let mut opaque = 0usize;
        for y in 0..ch {
            let start = (*row as usize * ch + y) * stride + *col as usize * cw * 4;
            opaque += px[start..start + cw * 4]
                .iter()
                .step_by(4)
                .filter(|&&a| u32::from(a) > declared_threshold)
                .count();
        }
        let rects = acc_regions
            .rects(*row, *col)
            .unwrap_or_else(|| panic!("格 ({row},{col}) 声明要画而轮廓表里没有"));
        if (opaque > 0) != !rects.is_empty() {
            disagree.push(format!(
                "({row},{col}): 像素 {opaque} 个不透明 / 矩形 {} 个",
                rects.len()
            ));
        }
    }
    assert!(
        disagree.is_empty(),
        "配件有 {} 格像素与轮廓不一致（同空同满被破）：\n  {}\
         \n⇒ 要么生产者切错了格（列右边界写成了 row），要么这批帧与这份 polygon 不是同一次 pack 的产物",
        disagree.len(),
        disagree.join("\n  ")
    );

    let body_regions = Regions::load(
        body_dir.join("polygon.json").to_str().unwrap(),
        body.grid.w,
        body.grid.h,
        SURFACE_MARGIN,
    )
    .expect("本体 polygon.json 读不出");
    let merged = body_regions
        .merged_with(&acc_regions)
        .expect("两个 wl_region 并集被拒");
    assert!(merged.frame_count() >= body_regions.frame_count());
    // 并集的语义 = **拼接而不是替换**：配件那一格就算全透明（0 个矩形）也不许把本体的可点区挖掉。
    // 这一条正是"帽子遮不住身体"在协议面上的形状，而 `first_missing` 分不出它。
    for (row, col) in &cells {
        let merged_rects = merged.rects(*row, *col).unwrap_or_else(|| {
            panic!("并集之后 ({row},{col}) 反而没了 ⇒ 装备把本体那一格的穿透性改坏了")
        });
        let b = body_regions.rects(*row, *col).map_or(0, |v| v.len());
        let a = acc_regions.rects(*row, *col).unwrap().len();
        assert_eq!(
            merged_rects.len(),
            b + a,
            "格 ({row},{col}) 的并集不是拼接：本体 {b} + 配件 {a} ≠ 并集 {}",
            merged_rects.len()
        );
    }

    // ⑥ 覆盖几档状态 ⇒ **只打印不判红**。一份只烘 `idle` 的配件是**合法**产物（`covers()` 为假
    //    ⇒ `blit_cell` 按设计不叠，见 `accessory.rs` 模块头第 2 条），所以这里不能拒绝它；但
    //    用户 2026-11-02 那句"只有idle的时候能显示帽子，其他动作都没有帽子"正是这个形状 ⇒
    //    这个数必须出现在人能读到的地方，否则"少烘了状态"与"叠加坏了"在故障面上完全一样。
    let miss: Vec<&str> = AssetKey::ALL
        .iter()
        .filter(|k| !acc.covers(**k))
        .map(|k| k.as_str())
        .collect();
    eprintln!(
        "OK 配件 {}（slot={:?} fits={:?}）覆盖 {}/{} 状态{}，声明 {} 格，sheet {}x{}",
        acc.slug,
        acc.slot,
        acc.fits,
        AssetKey::ALL.len() - miss.len(),
        AssetKey::ALL.len(),
        if miss.is_empty() {
            String::new()
        } else {
            format!("（缺 {}）", miss.join(","))
        },
        cells.len(),
        acc_atlas.width,
        acc_atlas.height,
    );
}
