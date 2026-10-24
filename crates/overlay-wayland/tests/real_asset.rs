//! 对**真实烘焙资产**做一次解码 + 区域还原，打印 M4 门禁要的数（解码耗时、解码后字节、
//! 每格 `wl_region` 矩形数）。`assets/` 是烘焙产物、不进仓库，所以默认 `#[ignore]`；
//! 本机跑：`cargo test -p overlay-wayland --test real_asset -- --ignored --nocapture`。

use std::path::Path;

use overlay_wayland::atlas::Atlas;
use overlay_wayland::region::Regions;
use petcore::assets::Manifest;

#[test]
#[ignore = "需要本机烘焙产物 assets/pets/<pet>/，仓库里没有"]
fn real_asset_decodes_and_yields_regions() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/pets/cat_shiro");
    let pet = dir.join("pet.json");
    if !pet.exists() {
        eprintln!("SKIP：{} 不存在（先跑 tools/pack.py）", pet.display());
        return;
    }
    let manifest = Manifest::load(pet.to_str().unwrap()).expect("清单读不出");
    let (cols, rows, gw, gh) = (
        manifest.grid.cols as u32,
        manifest.grid.rows as u32,
        manifest.grid.w,
        manifest.grid.h,
    );
    let atlas = Atlas::load(dir.join("sheet.webp").to_str().unwrap(), cols, rows, gw, gh)
        .expect("sheet.webp 解不出");
    let regions = Regions::load(
        dir.join("polygon.json").to_str().unwrap(),
        gw,
        gh,
        petcore::intent::SURFACE_MARGIN,
    )
    .expect("polygon.json 读不出");

    let mut counts = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            if let Some(r) = regions.rects(row, col) {
                counts.push(r.len());
            }
        }
    }
    let sum: usize = counts.iter().sum();
    assert_eq!(counts.len(), (cols * rows) as usize, "每格都该有区域");
    assert_eq!(
        atlas.decoded_bytes(),
        atlas.width as usize * atlas.height as usize * 4
    );
    // 每个矩形必须落在 surface 内，否则 wl_region 会杀连接
    let (sw, sh) = regions.surface_size();
    for row in 0..rows {
        for col in 0..cols {
            for r in regions.rects(row, col).unwrap() {
                assert!(r.x >= 0 && r.y >= 0 && r.w > 0 && r.h > 0);
                assert!(r.x + r.w <= sw && r.y + r.h <= sh, "越界矩形 {r:?}");
            }
        }
    }

    println!(
        "cat_shiro: grid {gw}x{gh} cols {cols} rows {rows} | sheet {}x{} {} KB → 解码 {} ms / {:.3} MiB | 区域 {} 格共 {} 矩形（均值 {:.1}，最多 {}）",
        atlas.width,
        atlas.height,
        atlas.encoded_bytes / 1024,
        atlas.decode_ms,
        atlas.decoded_bytes() as f64 / (1024.0 * 1024.0),
        counts.len(),
        sum,
        sum as f64 / counts.len() as f64,
        counts.iter().max().copied().unwrap_or(0),
    );
}
