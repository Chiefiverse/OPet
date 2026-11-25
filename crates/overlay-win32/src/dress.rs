//! 换装的**资产**那一半：落库、扫盘、解码（第二批 M5e）。
//!
//! ## M7-b0（2026-11-24）从这里拿走了什么
//!
//! 原来标题里还有"连库"两个字：`Gacha`（连库 + 装目录 + `balance` + `pull`）当时是本文件里
//! 手写的一份，注释写着"与 Wayland `Gacha::pull` 同一份语义，逐条对齐"。**对齐是人做的事**，
//! 而分叉的症状先出现在屏上、不报在编译器上 ⇒ 那一份已删，两家现在点同一个
//! [`overlay_report::Gacha`]。同一层还带上了收藏册的行生产者、本体库扫盘（
//! [`overlay_report::bodies::check_body`]）与「概率」那五行，Windows 侧 M7 的面板直接复用。
//!
//! ⚠️ 下面"两处平台形状差异"讲的都是**配件**（本文件仍自己扫），而**配件与本体在这一格上
//! 不对称是有意的**：配件侧本平台确实没有独立轮廓对象（第 1 条），所以那边是 3 道；本体侧的
//! 扫盘要产出 `regions` 给 Wayland 用，Windows 只是**读不到它**而不是不需要它 ⇒ 共用的那一份
//! 保留第 ④ 道（取舍与理由写在 `overlay-report/src/bodies.rs` 模块头）。
//!
//! ## 为什么这个模块不带 `#[cfg(windows)]`
//!
//! [`crate::app`] 顶上那一节解释了"Windows 那一整块收进 `#[cfg(windows)] mod win`"：宿主上
//! 看不见的代码必须在类型系统层面也不存在，否则 clippy 的未使用导入会红。那个理由在这里
//! **不成立**，因为这一文件用到的类型没有一个是 Windows 的：
//! `AccessoryManifest` / [`Atlas`] / [`Worn`] / [`Gacha`] 全跨平台（`Gacha` 来自
//! `overlay-report`，M7-b0 之后它是**两家共用**的那一份，本文件只留调用点）。
//!
//! 于是这一层是**唯一能把换装判据打到红的地方**。`App` 要连 Windows 才构造得起来（`Client::open`
//! 是第一步），而本项目对"屏上 A、记忆里 B"这一族形状的取证手段从来是**自动化断言 + 源级门禁**
//! （`overlay-wayland` 那五条 `demo_mode_*` / `pixels_and_the_ledger_*`），cross-compile 只证类型
//! 与链接、不跑一行。所以"演示态一个字都不写""穿上必然同时改像素与那一行账"这两条判据
//! 在这里拆成自由函数 —— 与 [`overlay_common::dress`] 头里那条先例同一条：**纯决策在共用层，
//! IO 与像素留在各家**，而各家那份 IO 也得能在宿主上红。
//!
//! ## 与 Wayland 那一份的两处**平台形状**差异（不是两套判据）
//!
//! 1. **门禁 3 道而不是 5 道**：那边是 `load` / `validate_against` / `polygon.json` 装载 /
//!    逐格轮廓齐不齐 / 解码。本平台**没有独立轮廓对象** —— `WM_NCHITTEST` 读的就是叠完之后的
//!    那份像素（[`crate::stage`] 模块头"为什么 Windows 读像素、Wayland 读 `polygon.json`"），
//!    所以"帽子画出来了而点不到"这一族在结构上不存在：画上了就点得到，没画上就点不到。
//!    剩下两道 = 启动的 `load` + `validate_against`，第五道（解码）排在穿上那一刻，同那边。
//! 2. **没有 `merged` 并集**：[`Candidate`] 因此只有元数据，穿上不需要"换一份轮廓对象"这一步。

use std::path::{Path, PathBuf};

use petcore::assets::{AccessoryManifest, Manifest};
use petcore::behavior::AssetKey;
use petcore::gacha::ItemKind;
use petstore::paths::AssetRoot;

use overlay_common::atlas::Atlas;

use overlay_report::Gacha;

use crate::stage::Worn;

/// 穿着这件事**落不落库**的唯一判据（P3）。两个动作共用一条理由：屏上穿的与库里记着的
/// 必须是同一件事，否则重启回到另一顶帽子，而"重启后还穿着"（J3）就成了一句注释。
///
/// `enum` 而不是 `bool`：这一处有三个**互斥**的结论，而两个布尔量能表示 4 种、其中 2 种
/// 是没人能解释的（本项目"能表示就迟早会被表示错"那一族）。
pub enum WornWrite {
    /// 真写进库了（`--db` 且连得上）。
    Written,
    /// **演示态一个字都不写**，也不报错 —— 但调用方必须打一行，否则用户会以为记下了。
    DemoSkipped,
    /// 给了 `--db` 而这一轮连不上库 ⇒ 没地方可写。错误只是一行字（硬约束 3）。
    NoStore,
}

/// 穿上 = 把"这一格现在穿这件"写进 `equipped`。**自由函数而不是 `App` 的方法**，同 Wayland
/// 那条理由（`App` 离开 Windows 构造不起来，而这一句只吃一个 `Option<Gacha>` ⇒ 拆出来才打得红），
/// 在本平台还要多一层：`App` 整个在 `#[cfg(windows)]` 里，不拆就**一条宿主断言都写不出**。
///
/// `dry_run` 排在最前且**不调** `ensure_gacha`：演示态连"顺手把内存库那一行写上"都不该做，
/// 因为那行永远不可能在重启后还在，而屏上没有这件事。
pub fn write_equip(
    gacha: &mut Option<Gacha>,
    dry_run: bool,
    slot: &str,
    item_id: &str,
) -> Result<WornWrite, String> {
    if dry_run {
        return Ok(WornWrite::DemoSkipped);
    }
    let Some(g) = gacha.as_mut() else {
        return Ok(WornWrite::NoStore);
    };
    g.store
        .equip(petstore::DEFAULT_PROFILE, slot, item_id)
        .map_err(|e| format!("equipped 写入失败: {e}"))?;
    Ok(WornWrite::Written)
}

/// 脱下 = 删掉那一格。与 [`write_equip`] 同一条演示态判据，同拆法。
pub fn clear_equip(
    gacha: &mut Option<Gacha>,
    dry_run: bool,
    slot: &str,
) -> Result<WornWrite, String> {
    if dry_run {
        return Ok(WornWrite::DemoSkipped);
    }
    let Some(g) = gacha.as_mut() else {
        return Ok(WornWrite::NoStore);
    };
    g.store
        .unequip(petstore::DEFAULT_PROFILE, slot)
        .map_err(|e| format!("equipped 删除失败: {e}"))?;
    Ok(WornWrite::Written)
}

/// 一件**磁盘上合格**的配件：元数据 + 所在目录，**一像素未解**。
///
/// `merged` 在这一族里不存在（本平台没有独立轮廓对象，见模块头差异 1），所以启动门禁交出的
/// 就是纯元数据 ⇒ [`crate::app`] 里"穿上"的唯一失败模式只剩"sheet 解不开"。把一件坏配件的
/// 判定推迟到用户点下那一行，等于让屏上一个能点而没有反馈的控件（本项目对假按钮的一贯立场）。
#[derive(Debug)]
pub struct Candidate {
    /// **库存那一侧的身份** = 库目录名。`owned()` 过滤、`inventory.item_id`、恢复那一行的比对
    /// 认的都是这一个（`AssetDir` 的 `id` 也是目录名）⇒ 三条路必须同一个键。
    pub id: String,
    /// 清单里写的 slug（`accessory.json` 的 `slug`，也是 `fits` 比的那个名字）。
    ///
    /// ⚠️ 它与 [`Self::id`] **可以不相等**（生产者 `pack.py --accessory` 让两者相等，手工塞进库的
    /// 未必），而不相等就是"屏上这件叫 A、库存记的是 B"。[`discover_accessories`] 为此专门打一行。
    pub slug: String,
    /// 三件套所在目录（`sheet.webp` 与 `accessory.json` 同目录 = 生产者 `pack.py --accessory` 的约定；
    /// 第三件 `polygon.json` 本平台**不读**，但包里有它，所以那句话仍然写"三件套"）。
    pub dir: PathBuf,
    pub manifest: AccessoryManifest,
}

/// 配件库在哪 —— **只由环境决定，命令行没有第二个入口**（Wayland 同一条裁定）。
///
/// 规则本身不在这里：它在 [`petstore::paths::AssetRoot`]，与本体侧那一根、与安装器写入的那一根
/// 同源（M5d 把 Windows 那条链补上：`%LOCALAPPDATA%\opet\accessories` 那一族）。
///
/// 返回 `Option` 而不是直接 `PathBuf`：`%USERPROFILE%` 与 `$HOME` 都读不到时**没有库**，
/// 不去猜一个根目录下的 `/.local/share`。那一档下换装页三行照样画出来、照样点得到，
/// 只是"穿上"打一行"库里没有可用配件"——**没有东西可穿不等于没有这个功能**。
pub fn accessory_root() -> Option<PathBuf> {
    AssetRoot::resolve(ItemKind::Accessory).map(|r| r.path)
}

/// [`discover_accessories`] 的内层：一个目录 → 一件候选，或一句点名它错在哪的说明。
///
/// 单列成函数有两个理由，原样继承 Wayland：① `dir` 在这里只被**借走**（`&Path`），调用方随后
/// 还要拿它打"跳过谁"那行。② 真产物那一轮（`tests/accessory_product.rs` 的 Windows 等价物）
/// 照同一套顺序走时，收一个 `&Path` 比收一整个库目录更好摆。
pub fn check_accessory(dir: &Path, body: &Manifest) -> Result<Candidate, String> {
    let path = dir.join("accessory.json");
    let name = path.to_str().ok_or("配件清单路径不是 UTF-8")?;
    let acc = AccessoryManifest::load(name).map_err(|e| format!("清单 {name} 加载失败: {e}"))?;
    // 这道比对是 D16 路线 B 唯一真正防的东西：配件与本体**不是同一份网格**时，叠加不会报错，
    // 只会把帽子画歪 —— 而屏上画歪的帽子没有任何门禁看得见。
    acc.validate_against(body)
        .map_err(|e| format!("拒绝配件 {}（{e}）", acc.slug))?;
    Ok(Candidate {
        // 身份 = **目录名**，不是 slug：库存的 `item_id` 与安装器落盘的 `<id>/` 认的都是这一级。
        id: dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} 的目录名不是合法 UTF-8 ⇒ 无法与库存对齐", dir.display()))?
            .to_string(),
        slug: acc.slug.clone(),
        dir: dir.to_path_buf(),
        manifest: acc,
    })
}

/// 启动时扫一遍配件库 ⇒ 换装页能选的那张表（**只有元数据，一像素未解**）。
///
/// 解码为什么必须留到穿上那一刻，同 Wayland 那条字面理由：`VmHWM` 是**高水位**，启动时
/// "每件解码一次再丢掉"与"全部常驻"给出同一个峰值（那边实测 59,164 kB 越过用户签的 57,344 kB）。
/// 顺序也是同一件事：`validate_against` 是纯元数据比对，而 sheet 解码是几十毫秒和几 MiB。
///
/// ⚠️ **这里一个 `Err` 都不出**：库里一个坏文件的后果是"少一件可穿"，不是"宠物起不来"
/// （硬约束 3）。每条跳过打一行**点名是哪件、错在哪一道**的 stderr ⇒ 少了什么用户看得见。
pub fn discover_accessories(root: &Path, body: &Manifest) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!(
                "accessories: 库 {} 打不开（{e}）⇒ 换装页「穿上」点了不会有效果",
                root.display()
            );
            return found;
        }
    };
    // 目录名参与排序：`read_dir` 的顺序由文件系统给，同一台机器上两次启动可能不同 ⇒
    // 换装页的"换一件"循环会把用户从一个配件直接跳到另一个不相干的配件。
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("accessory.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        match check_accessory(&dir, body) {
            Ok(cand) => {
                // 两个名字不一致 ⇒ 库存那本账（认目录名）与屏上那件（叫 slug）会对不上，而
                // "为什么这件我抽到了却不出现"是问不出来的。
                if cand.id != cand.slug {
                    eprintln!(
                        "accessories:   ⚠ 目录名 {} 与清单 slug {} 不一致 ⇒ 库存按 {} 记账、屏上叫 {}",
                        cand.id, cand.slug, cand.id, cand.slug
                    );
                }
                // **覆盖哪些状态**必须打出来，而且缺的时候要把缺的点名。这条行是本轮唯一的
                // "为什么有些动作看不到帽子"的出口：清单只声明 `idle` 是一件**合法**的配件
                // （`covers()` 为假 ⇒ `blit_over` 按设计不叠），而它上屏后的形状与"叠加坏了"
                // 一模一样，只有这一行分得开两者。
                let all = AssetKey::ALL;
                let have: Vec<&str> = all
                    .iter()
                    .filter(|k| cand.manifest.covers(**k))
                    .map(|k| k.as_str())
                    .collect();
                if have.len() == all.len() {
                    eprintln!(
                        "accessories:   {} 叠 {:?} 档 · 覆盖 {}/{} 状态 · sheet 未解（穿上才读）",
                        cand.slug,
                        cand.manifest.slot,
                        have.len(),
                        all.len(),
                    );
                } else {
                    let miss: Vec<&str> = all
                        .iter()
                        .filter(|k| !cand.manifest.covers(**k))
                        .map(|k| k.as_str())
                        .collect();
                    eprintln!(
                        "accessories:   {} 叠 {:?} 档 · 覆盖 {}/{} 状态 ⇒ **只有这 {} 档会看到它**（缺 {}）\
                         · sheet 未解（穿上才读）",
                        cand.slug,
                        cand.manifest.slot,
                        have.len(),
                        all.len(),
                        have.len(),
                        miss.join(","),
                    );
                }
                found.push(cand);
            }
            Err(err) => eprintln!(
                "accessories: 跳过 {}（{}）⇒ 这一件不进换装页，其余不受影响",
                dir.display(),
                err
            ),
        }
    }
    eprintln!(
        "accessories: 库 {} ⇒ {} 件可用{}",
        root.display(),
        found.len(),
        if found.is_empty() {
            "（「穿上」不会有效果；把 `pack.py --accessory` 的三件套放进这里）"
        } else {
            ""
        }
    );
    found
}

/// 起点本体的身份 = `--manifest` 那个路径的**目录名**（`id = 目录名`那条既有裁定 ⇒
/// `split_body_row` 剥下来的那一行与它比才有读数）。
///
/// 矩形档不读盘，内置 fixture 也没有目录名可取 ⇒ 退化成清单 `slug`，而且**在这里**说清，
/// 不留到屏上去猜。
pub fn start_body_id(manifest_path: Option<&Path>, slug: &str) -> String {
    manifest_path
        .and_then(|p| p.parent())
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .map(String::from)
        .unwrap_or_else(|| slug.to_string())
}

/// **到这一步才解码**第二张 sheet ⇒ 换装的唯一内存执行点。
///
/// 只剩一道可能失败 = sheet 读不开，而它的产物是一个 `Err` 字符串（调用方打一行、停在未穿），
/// 不是 panic。Wayland 那一份在这里多一步"取并集轮廓"，本平台没有那个对象（模块头差异 2）。
pub fn decode(cand: &Candidate) -> Result<Worn, String> {
    let sheet = cand.dir.join("sheet.webp");
    let acc = &cand.manifest;
    let atlas = Atlas::load(
        sheet.to_str().ok_or("配件 sheet 路径不是 UTF-8")?,
        acc.grid.cols as u32,
        acc.grid.rows as u32,
        acc.grid.w,
        acc.grid.h,
    )
    .map_err(|e| format!("{}: {e}", sheet.display()))?;
    eprintln!(
        "accessory: 穿上 {}（{}x{} {} KB → 解码 {} ms / {:.3} MiB）",
        acc.slug,
        atlas.width,
        atlas.height,
        atlas.encoded_bytes / 1024,
        atlas.decode_ms,
        atlas.decoded_bytes() as f64 / (1024.0 * 1024.0),
    );
    Ok(Worn {
        manifest: acc.clone(),
        atlas,
    })
}

#[cfg(test)]
mod tests {
    use overlay_common::dress::plan_restore;
    use overlay_common::l10n::Key;
    // `Gacha` 上提 `overlay-report`（M7-b0）之后，这几个名字只剩**测试**在读：生产行的路
    // 径在共用层，本文件的 lib 侧不点名它们 ⇒ 显式提到这里而不是顶层。
    use overlay_common::l10n::Lang;
    use petcore::gacha::{Catalog, Currency, Rarity};
    use petstore::Store;

    use super::*;

    /// 本体清单（16 px 格 / 8 列 / 4 行，两档状态）。与 `overlay-wayland` 那条 `BODY_JSON`
    /// **同一份字面量**：两平台拿同一份输入比，`validate_against` 的结论才不许分叉。
    const BODY_JSON: &str = r#"{"format":2,"slug":"tiny","sheet":"sheet.webp","grid":{"cols":8,"rows":4,"w":16,"h":16},"states":{"idle":{"rows":[0,1],"fps":8,"loop":true},"run":{"rows":[2,3],"fps":16,"loop":true}},"anchor":{"feet":[8,15]}}"#;

    /// 全过的帽子：`fits` 指得上面的本体，`idle` 的 rows/fps/loop 与本体**逐位**相等。
    const HAT_JSON: &str = r#"{"format":2,"slug":"cap","display_name":"蓝帽","slot":"head","fits":["tiny"],"sheet":"sheet.webp","grid":{"cols":8,"rows":4,"w":16,"h":16},"states":{"idle":{"rows":[0,1],"fps":8,"loop":true}}}"#;

    const CELL: u32 = 16;
    const COLS: u32 = 8;
    const ROWS: u32 = 4;

    fn body() -> Manifest {
        Manifest::parse(BODY_JSON).expect("本体清单该合法")
    }

    /// 往库里落一件配件的**清单**（像素由各个测例自己决定要不要给）。
    fn hat_in(dir: &Path, name: &str, json: &str) {
        let path = dir.join(name).join("accessory.json");
        std::fs::create_dir_all(path.parent().expect("有父目录")).expect("建目录");
        std::fs::write(&path, json.as_bytes()).expect("写清单");
    }

    /// `rows` 行全不透明像素的 lossless webp（**只给 `decode` 那一条测例用**）。
    fn sheet_bytes(rows: u32) -> Vec<u8> {
        let w = COLS * CELL;
        let h = rows * CELL;
        let rgba = vec![255u8; (w * h * 4) as usize];
        overlay_common::atlas::encode_lossless_rgba(&rgba, w, h).expect("lossless webp 该编得出来")
    }

    fn sole(dir: &Path, body: &Manifest) -> Candidate {
        let found = discover_accessories(dir, body);
        assert_eq!(found.len(), 1, "这套资产该恰好产出一件候选，实际 {found:?}");
        found.into_iter().next().expect("上面查过 len == 1")
    }

    // ── ① 落库那三档：演示态一个字都不写（P3；overlay-common 头里点名要 Win32 自带这条）───

    /// **演示态一个字都不落库**（FLOW §9 P3 明写"这条要写进测试，防顺手修好"）。
    ///
    /// ⚠️ 这条不是 Wayland 那条的复制粘贴就完事：共用层只放**纯判据**，而 `write_equip` 是 IO，
    /// 今天全网有两份实现（`overlay_common::dress` 模块头最后一段把这笔债写在了明面上）。
    /// 两份实现 ⇒ 两份都得有同一组断言，否则 Win32 侧那份改了没人知道。
    ///
    /// 三档顺序 = 危险程度：① 库就在手边也不许写（拿**真好用的**内存库测，`None` 那一档证不了
    /// `dry_run` 起了作用）② 同一只库同一个动作，只是这轮不是演示态 ⇒ 必须真写得进去
    /// ③ 连不上库不许 panic、也不许编出"写好了"的结论。
    #[test]
    fn demo_mode_writes_nothing_while_a_real_store_keeps_the_row() {
        let mut gacha = Some(Gacha::connect(None).expect("内存库该建得起来"));
        let read = |g: &Option<Gacha>| {
            g.as_ref()
                .unwrap()
                .store
                .equipped(petstore::DEFAULT_PROFILE)
        };
        assert!(
            matches!(
                write_equip(&mut gacha, true, "head", "cap").unwrap(),
                WornWrite::DemoSkipped
            ),
            "演示态要的是 DemoSkipped 这个**结论**，不是 Ok(())（调用方按结论决定打哪一行）"
        );
        assert!(
            read(&gacha).unwrap().is_empty(),
            "无 `--db` 那一轮写进内存库 = 对用户说「记下了」而它必然不还在"
        );
        assert!(matches!(
            write_equip(&mut gacha, false, "head", "cap").unwrap(),
            WornWrite::Written
        ));
        assert_eq!(
            read(&gacha).unwrap(),
            vec![("head".to_string(), "cap".to_string())],
            "①②合起来才证明拦下来的是 `dry_run` 这个条件本身"
        );
        let mut none = None;
        assert!(matches!(
            write_equip(&mut none, false, "head", "cap").unwrap(),
            WornWrite::NoStore
        ));
    }

    /// 脱下 = 删那一格。同上一条的三档，方向反过来（"删不掉"的后果是重启帽子自己回来）。
    #[test]
    fn unworn_clears_the_row_and_demo_has_nothing_to_clear() {
        let mut gacha = Some(Gacha::connect(None).expect("内存库该建得起来"));
        write_equip(&mut gacha, false, "head", "cap").unwrap();
        assert!(matches!(
            clear_equip(&mut gacha, true, "head").unwrap(),
            WornWrite::DemoSkipped
        ));
        assert!(
            !gacha
                .as_ref()
                .unwrap()
                .store
                .equipped(petstore::DEFAULT_PROFILE)
                .unwrap()
                .is_empty(),
            "演示态的「脱下」也不许动库：屏上没了而库里还记着，是**另一种**谎，不是同一种"
        );
        assert!(matches!(
            clear_equip(&mut gacha, false, "head").unwrap(),
            WornWrite::Written
        ));
        assert!(
            gacha
                .as_ref()
                .unwrap()
                .store
                .equipped(petstore::DEFAULT_PROFILE)
                .unwrap()
                .is_empty()
        );
        let mut none = None;
        assert!(matches!(
            clear_equip(&mut none, false, "head").unwrap(),
            WornWrite::NoStore
        ));
    }

    /// **UI 那两个写函数写的，就是下一个进程要打开的那只文件**（P3 唯一一条走完的写链）。
    ///
    /// 上面两条用内存库，`petstore/tests/equipped_roundtrip.rs` 测 `Store` 本体 ⇒ 两边都绿而
    /// 中间没接上，是本项目"两个事实源互不相认"那一族的真实形状。这里走**文件**：写四轮 →
    ///  drop → 第二个 `Gacha` 开同一路径 → 读回来的必须是最后一笔，且那一笔必须还能被
    /// [`plan_restore`] 走回 `wear`（否则 J3"重启还穿着"只在库里成立、在屏上不成立）。
    #[test]
    fn the_ui_writes_the_very_file_the_next_process_opens() {
        let dir = tempfile::tempdir().expect("临时目录该建得起来");
        let path = dir.path().join("opet.db");
        let path_str = path.to_str().expect("临时路径该是 UTF-8");

        let mut a = Some(Gacha::connect(Some(path_str)).expect("带 `--db` 那条路该连得上文件库"));
        for id in ["cap", "beanie"] {
            assert!(matches!(
                write_equip(&mut a, false, "head", id).unwrap(),
                WornWrite::Written
            ));
        }
        assert!(matches!(
            clear_equip(&mut a, false, "head").unwrap(),
            WornWrite::Written
        ));
        assert!(matches!(
            write_equip(&mut a, false, "head", "cap").unwrap(),
            WornWrite::Written
        ));
        drop(a);

        let b = Gacha::connect(Some(path_str)).expect("第二个进程该开得了同一只文件");
        let rows = b
            .store
            .equipped(petstore::DEFAULT_PROFILE)
            .expect("读 equipped 不该失败");
        assert_eq!(
            rows,
            vec![("head".to_string(), "cap".to_string())],
            "写了四轮而文件里没有净效果 ⇒ 那些笔只活在进程 A 的连接上（J3 必然红）"
        );
        let ids: Vec<String> = vec!["cap".into()];
        let slots: Vec<String> = vec!["head".into()];
        let plan = plan_restore(&rows, &ids, &slots, &[0]);
        assert_eq!(
            plan.wear,
            Some(0),
            "文件里那一行必须能走回 `wear`：{:?}",
            plan.notes
        );
    }

    // ── ② 扫盘：一件坏文件的后果是"少一件可穿"，不是"宠物起不来"（硬约束 3）───

    /// 三个方向的空库都必须是**空表**而不是错误：① 目录不存在 ② 目录存在而空
    /// ③ 目录里有子目录而**没有** `accessory.json`。
    ///
    /// 第 ③ 档最容易被写错：拿 `read_dir` 的每一项直接当候选，那么用户手放的 `README/`
    /// 会变成换装页里一行点了必然失败的配件（本项目对假按钮的一贯立场）。
    /// 本平台这里没有"轮廓被改动"可断（Wayland 那一条断的是本体 `Regions` 逐位不变）⇒
    /// 断言只剩候选表本身。
    #[test]
    fn an_empty_library_yields_no_candidates() {
        let dir = tempfile::tempdir().expect("临时目录");
        let m = body();
        let missing = dir.path().join("nope");
        assert!(
            discover_accessories(&missing, &m).is_empty(),
            "库不存在 ⇒ 应当是「没有可穿」，不是错误、更不是 panic"
        );
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).expect("建空库");
        assert!(
            discover_accessories(&empty, &m).is_empty(),
            "空目录 ⇒ 零候选"
        );
        std::fs::create_dir(dir.path().join("notes")).expect("建无关目录");
        assert!(
            discover_accessories(dir.path(), &m).is_empty(),
            "没有 accessory.json 的目录不该被当成一件配件"
        );
    }

    /// **启动这条路根本不打开 `sheet.webp`** —— 库里把像素文件删掉，候选照样产出。
    ///
    /// 断的不是健壮性而是**内存档 M1 的字面含义**：`VmHWM` 是**高水位**，"每件解码一次再丢掉"
    /// 与"全部常驻"给出同一个峰值（Wayland 那边实测 59,164 kB 越过用户签的 57,344 kB）。
    /// 哪天有人把 [`Atlas::load`] 挪回 [`check_accessory`]，这份没有像素的库会立刻让它红。
    #[test]
    fn discovery_never_touches_the_sheet_so_a_pixel_less_library_still_lists_it() {
        let dir = tempfile::tempdir().expect("临时目录");
        hat_in(dir.path(), "cap", HAT_JSON);
        let cand = sole(dir.path(), &body());
        assert_eq!(cand.id, "cap");
        assert!(
            !cand.dir.join("sheet.webp").exists(),
            "前提没成立：这件居然有像素文件，那这条什么都没断"
        );
    }

    /// 顺序由**目录名**给，不由文件系统给 ⇒ "换一件"的循环在同一份库上可重放。
    ///
    /// `read_dir` 的返回顺序是实现细节（同一台机器两次启动都可能不同），不排序的话
    /// 用户连点两次"换一件"会在两个不相干的配件之间跳。写盘顺序**故意**与字典序相反，
    /// 这样"没排序"必然读出 `[zeta, alpha]` 而不是碰巧绿。
    #[test]
    fn discovery_sorts_by_dir_name_so_the_cycle_is_replayable() {
        let dir = tempfile::tempdir().expect("临时目录");
        hat_in(dir.path(), "zeta_hat", HAT_JSON);
        hat_in(dir.path(), "alpha_hat", HAT_JSON);
        let found = discover_accessories(dir.path(), &body());
        let ids: Vec<&str> = found.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["alpha_hat", "zeta_hat"],
            "换装页的循环序没按目录名排"
        );
    }

    /// 坏的那一件**只亏它自己**：格子尺寸漂了一档 ⇒ 被跳过，同一库里好的那件照常产出。
    ///
    /// `validate_against` 是 D16 路线 B 唯一真正防住"帽子画歪"的一道，而画歪的帽子在屏上
    /// 没有任何门禁看得见 ⇒ 这一条的红必须能区分"门禁松了"（`found.len()==2`）与
    /// "门禁太狠"（`found.is_empty()`），所以库里必须同时放一件好的。
    #[test]
    fn a_rejected_accessory_only_costs_itself() {
        let dir = tempfile::tempdir().expect("临时目录");
        hat_in(dir.path(), "cap", HAT_JSON);
        let drifted = HAT_JSON.replace(r#""w":16,"h":16"#, r#""w":32,"h":32"#);
        assert_ne!(drifted, HAT_JSON, "替换没生效 ⇒ 这条测试什么也没测");
        hat_in(dir.path(), "wide_hat", &drifted);
        let found = discover_accessories(dir.path(), &body());
        let ids: Vec<&str> = found.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["cap"],
            "格子漂移的那件必须出局，而它同库那件必须留下"
        );
    }

    /// 目录名 ≠ 清单 slug 时**两个名字都留着**，而且各归各用。
    ///
    /// `pack.py --accessory` 让两者相等，手工塞进库的未必。库存那本账认目录名（`item_id`），
    /// 屏上那件叫 slug ⇒ 只留一个就会在恢复那一刻对不上，而 [`Candidate`] 为此专门打一行 ⚠️。
    /// 这一条钉的是"两个字段真的分开了"，不是"那行日志打了"（stderr 在测例里不可 grep）。
    #[test]
    fn a_dir_name_that_is_not_the_slug_keeps_both_names() {
        let dir = tempfile::tempdir().expect("临时目录");
        let renamed = HAT_JSON.replace(r#""slug":"cap""#, r#""slug":"blue_cap""#);
        assert_ne!(renamed, HAT_JSON, "替换没生效 ⇒ 这条测试什么也没测");
        hat_in(dir.path(), "cap", &renamed);
        let cand = sole(dir.path(), &body());
        assert_eq!(cand.id, "cap", "库存那侧的键必须是目录名");
        assert_eq!(cand.slug, "blue_cap", "屏上那侧的名字必须是清单 slug");
    }

    // ── ③ 解码：扫盘之后**只剩**这一道可能失败，且它是 `Err` 不是 panic ────────────

    /// [`decode`] 的两档：有像素 ⇒ `Worn`（尺寸来自清单，不是来自文件）；没像素 ⇒ `Err`
    /// **点名是哪个文件**（调用方打一行、停在未穿，屏上不变）。
    #[test]
    fn decode_is_the_only_remaining_failure_and_it_names_the_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        hat_in(dir.path(), "cap", HAT_JSON);
        let cand = sole(dir.path(), &body());

        // `Worn` 没有 `Debug`（`Atlas` 扛着整张解码位图，派生它等于给 panic 路径准备一份
        // 几百 KB 的打印）⇒ 这一条不用 `expect_err`，用 `match`。
        let worn = match decode(&cand) {
            Err(e) => e,
            Ok(_) => panic!("这件没有 sheet.webp，应当失败而不是解出个东西"),
        };
        assert!(
            worn.contains("sheet.webp"),
            "失败行不点文件名 ⇒ 用户无法区分「路径不对」与「清单坏了」: {worn}"
        );

        std::fs::write(cand.dir.join("sheet.webp"), sheet_bytes(ROWS)).expect("写像素");
        let worn = decode(&cand).expect("有像素就该解得开");
        assert_eq!(worn.atlas.width, COLS * CELL, "宽度该按清单的 cols×w");
        assert_eq!(worn.atlas.height, ROWS * CELL, "高度该按清单的 rows");
        assert_eq!(worn.manifest.slug, "cap", "Worn 带的必须是这一件自己的清单");
    }

    // ── ④ 起点本体的身份 ────────────────────────────────────────────────────────

    /// 恢复那一条要把库里 `body=` 那一行与"本轮起点本体"比对，而**身份 = 目录名**。
    /// 矩形档不读盘、内置 fixture 也没有目录名 ⇒ 退化成 slug，这一条钉住退化分支真的会退化。
    #[test]
    fn start_body_id_reads_the_directory_and_falls_back_to_the_slug() {
        assert_eq!(
            start_body_id(
                Some(Path::new("/x/assets/pets/tiger/pet.json")),
                "cat_shiro"
            ),
            "tiger",
            "`--manifest` 在位时身份是**清单往上那一级目录名**，不是清单里的 slug"
        );
        assert_eq!(
            start_body_id(None, "cat_shiro"),
            "cat_shiro",
            "没有清单路径（矩形档 / 内置 fixture）才允许退化成 slug"
        );
        // 根目录下的清单没有父级 ⇒ 同样退化，而不是 panic 或空串。
        assert_eq!(start_body_id(Some(Path::new("pet.json")), "cap"), "cap");
    }

    // ── ⑤ 源级门禁：像素与账在两家客户端里各只有一个落点 ──────────────────────────

    /// `app.rs` 的**非测试**部分，两条源级门禁共用。
    ///
    /// 本平台今天没有 `mod tests`（`App` 构造不起来 ⇒ 写了也跑不了），但门禁自己的字面量
    /// 就写在下面这些函数里 ⇒ 切掉测试模块才不数到自己（以后谁给 `app.rs` 加测例，这条照旧成立）。
    fn app_code() -> &'static str {
        let src = include_str!("app.rs");
        match src.find("\nmod tests {") {
            Some(at) => &src[..at],
            None => src,
        }
    }

    /// 取某个函数的**体**（从 `fn name(` 起，到下一个同级 `fn` 或本函数收尾的 `}`）。
    ///
    /// 缩进 8 空格 = `#[cfg(windows)] mod win { impl App { … } }` 三层之内。找不到就 panic
    /// "这条门禁白跑" —— 门禁自己必须能被"函数改名/搬走"这件事打断，否则它断言的是一个
    /// 已经不存在的形状（同族：`cli.rs` 的 `every_flag_advertised_in_usage_exists`）。
    fn fn_body(code: &str, name: &str) -> String {
        let at = format!("fn {name}(");
        let start = code
            .find(&at)
            .unwrap_or_else(|| panic!("找不到 `{at}` ⇒ 这条门禁白跑"));
        let rest = &code[start..];
        let end = rest[at.len()..]
            .find("\n        fn ")
            .or_else(|| rest[at.len()..].find("\n        }"))
            .map(|d| at.len() + d)
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// **"改了像素"与"记进 equipped"在源码里必须是同一个动作**（P3 的接线门禁）。
    ///
    /// Wayland 那一条（`pixels_and_the_ledger_are_written_by_one_function_only`）不是洁癖，是
    /// 变异实测逼出来的：删掉 `equip` 里那句 `persist_worn`，那边 526 条全绿 —— 因为 `App` 在
    /// 没有 compositor 的机器上构造不起来。本平台**更极端**：`App` 整个在 `#[cfg(windows)]` 里，
    /// 宿主上一条构造测例都写不出来 ⇒ 这一族唯一的防线就是读源码文本。
    ///
    /// 与 `cli.rs` 的 `every_flag_advertised_in_usage_exists` 同族。**读源码文本的门禁天生脆**，
    /// 所以只数最小的几件事，每条都带"为什么是这一条"；重构改名会红，那是**要人来看一眼**的红。
    ///
    /// ⚠️ 数的是**代码形状**而不是散文：needle 一律带上 `(` 或 `=`，注释里提到同一个函数名
    /// 不会被数进来（这一条在 M7b 那扇门上差点踩到 —— 面板的字段注释里就写着
    /// `panel::rects(kind, rows)`）。
    #[test]
    fn the_pixels_and_the_ledger_are_written_by_one_function_each() {
        let code = app_code();
        let body = |name: &str| -> String { fn_body(code, name) };

        // ① 把配件放上身的赋值**全文件只许有一处**，而那一处必须在 `wear` 里。
        let worn_some = code.matches("self.worn = Some(").count();
        assert_eq!(
            worn_some, 1,
            "把配件放上身写了 {worn_some} 处 ⇒ 有一处绕过了 `wear`，而它不会记 equipped"
        );
        let wears = body("wear");
        assert!(
            wears.contains("self.worn = Some("),
            "唯一那处上身必须在 `wear` 里，不然它就不在记账的那条路上"
        );

        // ② `wear` 自己必须带那句记账（① 只保证"别处没有"，不保证"这里有"）。
        assert!(
            wears.contains("persist_worn"),
            "`wear` 里没有 `persist_worn` ⇒ 三处装载全都不记账，而宿主测不出来"
        );

        // ③ 五个装载点都真的走 `wear`：穿上（`equip`）/ 换一件（`cycle_accessory`）/ 启动恢复
        //    （`restore_worn`）/ 收藏册点名（`pick_and_wear`，M7c）/ 换本体后重穿幸存那件
        //    （`swap_body`，M7d）。少一个 = 那条路又自己写了一遍像素。
        //    ⚠️ **4 → 5 是 P5（换本体）那一格逼出来的，与 Wayland 那个 5 同一条理由**：换完本体后
        //    `candidates` 是**新可见集**，旧帽子如果还在里面，它的下标已经不是身上那格的旧下标
        //    ⇒ 只能按 id 重认一次再走 `wear`。在这条路上自己写那四行赋值 = 屏上戴着帽子而
        //    `equipped` 记的是上一只本体的那一件（= 变异 G 那一族，只是换了主语）。
        let calls = code.matches("self.wear(").count();
        assert_eq!(
            calls, 5,
            "`wear` 应当被 穿上 / 换一件 / 启动恢复 / 收藏册点名 / 换本体后重穿幸存那件 五处共用，\
             实际调用点 {calls} 个"
        );

        // ④ 释放像素也只有一个落点（`take_worn`），而"脱下"必须把账一起删掉。
        let takes = code.matches("self.worn.take()").count();
        assert_eq!(
            takes, 1,
            "`self.worn.take()` 写了 {takes} 处 ⇒ 有一处会留下'库里还穿着而屏上没了'那一半"
        );
        let unwears = body("unwear");
        assert!(
            unwears.contains("self.take_worn(") && unwears.contains("persist_unworn"),
            "`unwear` 必须同时是「释放」与「删那一行账」，拆开了就是变异 G 的镜像形状"
        );

        // ⑤ 落库那两个 IO 各只有一个调用者（`persist_worn` / `persist_unworn`）⇒ 演示态那一档
        //    不可能被某条新路径绕过去。
        //    ⚠️ **`write_equip` 从 1 变 2 是 M7d 的 `persist_body`**（本体那一行也走同一个 IO），
        //    两处的形状必须仍是"只有一行 `match write_equip(…)`、三档结论各自出声" ⇒ 判据仍然只有一份。
        //    Wayland 那侧同一条计数也是 2（`persist_body` + `persist_worn`），两家用同一判据不靠约定。
        assert_eq!(
            code.matches("write_equip(").count(),
            2,
            "`write_equip` 的调用者不是恰好 `persist_worn` + `persist_body` 这两个转发 ⇒ \
             有人在函数外自己落库，演示态那一档就有了第二份实现"
        );
        let persist_worn = fn_body(code, "persist_worn");
        let persist_body = fn_body(code, "persist_body");
        assert!(
            persist_worn.contains("write_equip(") && persist_body.contains("write_equip("),
            "那两处都必须在 `persist_*` 里面（在外面 = `dress_exempt_for` 那三档结论被复制了一份）"
        );
        assert_eq!(
            code.matches("clear_equip(").count(),
            1,
            "`clear_equip` 有了第二个调用者 ⇒ 同上，方向反过来"
        );
    }

    // ── ⑤b 面板接线（M7b）源级门禁：每件事只有一扇门 ──────────────────────────────

    /// 第三块 surface 的接线门禁。
    ///
    /// **为什么必须有、而且必须读源码文本**：`App` 整个在 `#[cfg(windows)]` 里 ⇒ 宿主写不出
    /// 构造测例，交叉编译只证类型与链接（"能编译"不许被引用成"通过"，D5）。面板比菜单多的
    /// 风险恰好是三件"错了也不会红"的事：① 建窗/关窗各只有一扇门（多一扇 = 有一处绕开互斥，
    /// 屏上两块面而 `panel_commits` 只数其中一块）；② 派发期间不许碰 `Client`（`Events` 回调
    /// 发生在 `pump` 内部，那时 client 正被借用 —— 那是 UB 边界，不是风格）；③ 命中表与行
    /// 同生同改（`69fd158` 那一族：点的是这一屏的行、画的是另一屏的字）。
    ///
    /// ⚠️ ⑧/⑨ 两族里那些"恰好是 N"的计数是**约定而不是测量**：M7c 落地时 ⑧ 从欠账变成正向断言，
    /// M7d 落地时 ⑨ 从 0 改成 1（同时 `self.wear(` 4 → 5、`write_equip(` 1 → 2）。以后每接一块
    /// 面就同一处置：**把数改对并写下为什么**，不许删断言。
    #[test]
    fn the_panel_is_wired_through_one_door_each() {
        let code = app_code();

        // ① 建窗与关窗各一处，而且各自落在该落的那个函数里。
        assert_eq!(
            code.matches("spawn(SurfaceKind::Panel").count(),
            1,
            "面板建窗有第二处 ⇒ 有一处绕开 `apply_panel`，而互斥（开面板先关菜单）只写在那里"
        );
        assert_eq!(
            code.matches("close(SurfaceKind::Panel").count(),
            1,
            "面板关窗有第二处 ⇒ `panel: Option<…>` 会与真实窗口分家"
        );
        let apply = fn_body(code, "apply_panel");
        let close_now = fn_body(code, "close_panel_now");
        assert!(
            apply.contains("spawn(SurfaceKind::Panel")
                && close_now.contains("close(SurfaceKind::Panel"),
            "两处窗口动作必须分别在 `apply_panel`（环外落地）与 `close_panel_now` 里"
        );

        // ② 互斥的唯一守卫在 `apply_panel`：开那一面之前先关菜单。
        assert!(
            apply.contains("self.close_menu(client)"),
            "`apply_panel` 不再先关菜单 ⇒ 同层两块面同屏，而它们的堆叠序没有任何定义"
        );

        // ③ 派发侧不许碰窗口：回调在 `pump` 里跑，那时没有安全的 `&mut Client`。
        for name in ["dispatch_menu", "dispatch_panel"] {
            let body = fn_body(code, name);
            assert!(
                !body.contains("client"),
                "`{name}` 里出现了 client ⇒ 有人在派发期间动窗口（`App` 文档头那条线性借用纪律）。\
                 正解是记旗，由 `round` 落地"
            );
        }
        // 旗必须被 `round` 落地，否则"记了旗而没人办"就是点了没反应的假按钮。
        let round = fn_body(code, "round");
        assert_eq!(
            round.matches("self.apply_panel(client)").count(),
            1,
            "`pending_panel` 没有在事件环里落地 ⇒ 面板永远开不出来"
        );
        assert_eq!(
            round.matches("self.update_panel(client)").count(),
            1,
            "`update_panel` 没被每轮调 ⇒ 面板建了窗却一帧都不画"
        );

        // ④ present 与计数器各一个落点（`panel_commits` 必须与 `menu_commits` 分开数）。
        assert_eq!(
            code.matches("present(SurfaceKind::Panel").count(),
            1,
            "面板 present 有第二处 ⇒ 有一处绕过 `panel::content_sig` 那道闸门（静止恒 1 提交的判据作废）"
        );
        assert_eq!(
            code.matches("counters.panel_commits += 1").count(),
            1,
            "`panel_commits` 有两个 +1 ⇒ 摘要里那一行不再说明任何事"
        );
        assert!(
            fn_body(code, "update_panel").contains("counters.panel_commits += 1"),
            "那个 +1 必须在 `update_panel` 里（与 `draw` 的返回值同处），别处记都是双记"
        );

        // ⑤ 命中表只有两个出生地：`PanelState::new` 的字段初始化、`set_lines` 的换行。
        assert_eq!(
            code.matches("items: panel::rects(").count()
                + code.matches("self.items = panel::rects(").count(),
            2,
            "命中表的出生地不是恰好两处（`PanelState::new` 的字段初始化 + `set_lines`）⇒\
             多一处 = 有人绕过换行入口，少一处 = 有一条路只改 `rows`/`kind` 不改表（`69fd158`）"
        );
        let set_lines = fn_body(code, "set_lines");
        assert!(
            set_lines.contains("self.items = panel::rects(")
                && set_lines.contains("self.rows = rows")
                && set_lines.contains("self.kind = kind"),
            "`set_lines` 必须一次改齐 kind/rows/items（少一样 = 屏上与命中表分家）"
        );
        assert!(
            !set_lines.contains("force_draw"),
            "原地换行不该置 `force_draw`：kind/lang/行文本/错误行全进 `panel::content_sig`"
        );

        // ⑥ 分派只走 `panel::press`（把关在命中侧），摆位只走 `panel::placement`。
        assert_eq!(
            code.matches("panel::press(").count(),
            1,
            "第二处命中判定 ⇒ 有人在客户端自己实现「actionable」，共用层那条门禁就管不到这里了"
        );
        assert_eq!(
            code.matches("panel::placement(").count(),
            1,
            "第二处摆位 ⇒ 面板与菜单不再共享「placement」那一份\"屏上装得下\"的保证"
        );

        // ⑦ 右键的第三臂：面板开着时右键**关面板**，不是去开菜单。
        assert!(
            fn_body(code, "toggle_menu").contains("} else if self.panel.is_some()"),
            "第三臂掉了 ⇒ 面板开着时右键会去开菜单，正好把互斥破掉（症状：右键面板弹出菜单）"
        );

        // ⑧ 收藏册那一本（M7c 落地）：逐件屏只有一个生产者，而点名的那一臂只有一条路。
        //    `album_screen` 的两个调用者是**有意的两份**（`panel_rows` 给"面板已经开着、跟着
        //    语言/动画档重算"，`collection_album` 给"从菜单或翻页进来、顺带把同一批字打进
        //    stderr"），第三个调用者就意味着有人绕过了页码归一。
        assert_eq!(
            code.matches("self.album_screen(").count(),
            2,
            "逐件屏的生产者不是两处（实际 {} 处）⇒ 多出来的那一处绕过 `album_page` 的页码归一，\
             屏上会漂页；少掉的那一处意味着有一本退回汇总行",
            code.matches("self.album_screen(").count()
        );
        assert!(
            fn_body(code, "collection_panel").contains("self.collection_album(page)"),
            "Owned 那一本没有岔给 `collection_album` ⇒ 菜单进来的收藏册又是汇总行，\
             而 `panel::PanelItem::More` 与 `Wear(i)` 从此产不出来"
        );
        assert!(
            fn_body(code, "dispatch_panel").contains("self.pick_and_wear(i)"),
            "`Wear(i)` 那一臂没有走 `pick_and_wear` ⇒ 点名要么静默不动，要么自己实现了一遍穿法"
        );
        assert_eq!(
            code.matches("self.pick_and_wear(").count(),
            1,
            "点名的调用点不是 1 处 ⇒ 有第二条路在写 `picked` 而它不经那道所有权闸"
        );

        // ⑨ P5（M7d 落地）：换本体这一族的"唯一落点 + 九步次序"，逐条照 Wayland
        //    `pixels_and_the_ledger_are_written_by_one_function_only` 的 ⑥/⑦/⑥′，只改两处主语：
        //    · `self.swap_body(` 那边是 **2**（`on_panel_press` 的 `Body` 臂 + §3.12 的
        //      `ipc_apply`），本平台是 **1** —— 控制套接字仍在 `cli::REFUSED`（第三批），
        //      **接它那天这个数要跟着涨**，与上面 `self.wear(` 那条同一族约定；
        //    · 母本第 3 步"换穿透轮廓"在这里不存在（本平台没有 `Regions` 消费者），所以那条
        //      半换形状不需要断言，而 `self.manifest = ` 那一处仍然是唯一写入点。
        assert_eq!(
            code.matches("self.swap_body(").count(),
            1,
            "`swap_body` 只许有 `dispatch_panel` 的 `Body` 臂这一个调用点（§3.12 接进来时改成 2），\
             实际 {} 处 ⇒ 多出来的那份会自己决定这八步的次序",
            code.matches("self.swap_body(").count()
        );
        assert_eq!(
            code.matches("self.persist_body(").count(),
            1,
            "`equipped` 的 `body` 行只许在 `swap_body` 末尾写一次（先像素后账），实际 {} 处",
            code.matches("self.persist_body(").count()
        );
        assert_eq!(
            code.matches("self.manifest = ").count(),
            1,
            "本体清单只有一个写入点（`swap_body` 第 4 步）⇒ 第二处赋值就是\"清单是新的而 atlas \
             是旧的\"那个半换状态的另一半（实际 {} 处）",
            code.matches("self.manifest = ").count()
        );
        // ⚠️ needle 带 `&`：`atlas.reload(` 这个字面量在 [`App::swap_body`] 那条**文档注释**里
        // 也出现过（它把这条门禁的口径写在注释上），不带 `&` 会数到文档而不是代码。
        assert_eq!(
            code.matches("atlas.reload(&").count(),
            1,
            "原地重载只许有一处 ⇒ 写成 load-then-swap（先 `Atlas::load` 再换掉）峰值就是**两只\
             本体 + 一顶帽 ≈147 MB，而它落在 ≤200 MiB 门禁内 ⇒ 什么都不会红**，只有这一条 + \
             验收那一次 `VmHWM` 能抓（实际 {} 处）",
            code.matches("atlas.reload(&").count()
        );

        // ⑨′ **八步的次序**（本轮最承重的一族）。`reload` 是不可回头的那一条线，所以：
        //    判定与"卸下旧帽"排在它**前**（失败时屏上还是原来那只、账一个字不动），
        //    记账、`drawn_cell = None`、`candidates` 重跑排在它**后**。
        //    · 删掉 `drawn_cell = None` = 那边变异 G 的 P5 版：四份产物的行表逐字节相同 ⇒
        //      `cell != drawn_cell` 那道门永不开 ⇒ 五百多条全绿而真机失败（屏上静默不变）。
        //      ⇒ 这一行**只有**下面这条 textual 读数，真机上唯一的读数是一张截图。
        let swap = fn_body(code, "swap_body");
        let said = |needle: &str| {
            swap.find(needle)
                .unwrap_or_else(|| panic!("`swap_body` 里没有 `{needle}` ⇒ 那八步被拆了"))
        };
        assert!(
            said("plan_swap_body(") < said("atlas.reload(&"),
            "判定排在解像素之后 ⇒ 被拒的那一次已经改过图集，而日志说的是\"没换\""
        );
        assert!(
            said("self.take_worn()") < said("atlas.reload(&"),
            "旧帽没先卸下就去解新本体 ⇒ 峰值 = 本体 + 旧帽 + 新本体（那边 M1 档的 148,076 kB）"
        );
        assert!(
            said("atlas.reload(&") < said("self.persist_body("),
            "账排在像素之前 ⇒ 解码失败那一轮库里记着一只屏上没有的本体"
        );
        assert!(
            said("atlas.reload(&") < said("self.drawn_cell = None"),
            "`drawn_cell` 的复位没排在 `reload` 之后 ⇒ 换本体是一次屏上静默不变（变异 G 的 P5 版）"
        );
        assert!(
            said("discover_accessories(") < said("let fresh = self.dress_view()"),
            "`picked` 在 `candidates` 重跑之前被改写 ⇒ 拿旧下标去索引新表，点下去是另一件"
        );
        // 母本 ⑥′：**第 5 步那个库位置**。11-07 那边交互主轮实测抓到的一刀，而它**不在**上面任何
        // 一条的射程里（次序全对、只是根取错了）：`swap_body` 写成 `pet_root()` ⇒ 配件库被扫成
        // 本体库那个目录 ⇒ 换完本体帽子当场掉。它连注释都不许出现那个函数名 = 这一刀在文本里的
        // 形状就是"有人准备这么写"，而注释是给人看的，判据不区分注释与代码。
        assert!(
            !swap.contains("pet_root(") && !swap.contains("discover_pets("),
            "`swap_body` 里出现了本体库的扫盘调用 ⇒ 重扫配件用的是本体库的根，换完本体配件全灭"
        );

        // ⑩ 收藏册**接线**（不是判据）：`album_screen` 必须把两张表都交给共用层。
        //     这一条防的是 M7c 那一批的中间形状悄悄回来 —— 本体那两参写成 `&[], &[]` 时屏上
        //     宠物行全部退化成只读，而宿主这 76 条与共用层几百条**一条都不会红**（那边同一格的
        //     读数 `the_album_routes_pets_to_the_body_index_space` 测的是**判据**，这条测**接线**）。
        let album = fn_body(code, "album_screen");
        assert!(
            album.contains("let pet_view = self.pet_view();")
                && album.contains(
                    "&g.album_entries(&owned, &ids, &view.visible, &pet_ids, &pet_view.visible)"
                ),
            "`album_screen` 没有把本体那两张表喂进 `album_entries` ⇒ 收藏册的宠物行又变回只读\
             （屏上看不出、测例读不到，只有这一条有读数）"
        );
        assert!(
            !album.contains("&[], &[]"),
            "`album_screen` 里出现了空表实参 ⇒ 就是上面那条恒空的写法回来了"
        );
        assert!(
            fn_body(code, "dispatch_panel").contains("self.swap_body(i)"),
            "`Body(i)` 那一臂没有走 `swap_body` ⇒ 点名本体要么静默不动，要么自己实现了一遍换法"
        );
        assert!(
            !code.contains("M7 面板接线"),
            "菜单那六行的占位臂（\"点了只出声\"）不许复活：入口现在真的开面板"
        );
    }

    // ── ⑥ 抽卡（M6）：与 Wayland `app.rs` 那四条**同一条断言**，一条都不许改弱 ─────────
    //
    // 为什么这一节必须存在，而不是"反正共用 `menu::report_lines`，那边绿就等于这边绿"：
    // `Gacha::pull` 是**两份实现**（各家一份，与本文件头的 [`write_equip`] 同族），共用的只有
    // 下面那层几何与词表。而 Win32 侧唯一能打到它红的地方就是这里 —— `app::win` 整个在
    // `#[cfg(windows)]` 里，交叉 clippy 只证类型与链接、不跑一行 ⇒ "能编译"不许被引用成"通过"。

    /// 从结果卡的明细行里切出**名字那一段**，切不出来返回 `None`。
    ///
    /// 行形状 = `"{档} {name} {tag}"`（[`overlay_common::l10n::line_item`]），而
    /// [`overlay_common::menu::fit`] 可能把尾巴截成一个 `…` ⇒ 截断时 tag 已经不在，剩下的就是
    /// 名字。**为什么不直接判整行非 ASCII**：zh/ja/ko 的 tag（「新」「重复」「신규」）本身就是
    /// 非 ASCII，判整行会把"名字退回 `item_id`"当成通过 —— 变异实测（2026-11-24，把
    /// [`Gacha::pull`] 的 catalog 查名换成 `item_id` 后本文件 15 条**全绿**，打出来的是
    /// `"SR panda 新"`）。Wayland 那条同名的测例原本也在这个洞里，已一起改成判名字段。
    fn item_name_field(line: &str, lang: Lang) -> Option<&str> {
        let rest = Rarity::ALL
            .iter()
            .find_map(|r| line.strip_prefix(r.as_str()))?
            .strip_prefix(' ')?;
        [lang.t(Key::New), lang.t(Key::Dup)]
            .iter()
            .find_map(|t| rest.strip_suffix(t))
            .unwrap_or(rest)
            .strip_suffix(' ')
            .filter(|n| !n.is_empty() && *n != "…")
    }

    /// **内存库十连**：连路径都没有 ⇒ 一个字节都不产生；标题行同时说出抽了几次与最高档；
    /// 结果行的名字段必须是**目录里的中文名**而不是 `item_id`；余额只加不减（D2）。
    #[test]
    fn memory_store_pulls_and_bakes_the_locale_into_the_report() {
        let mut g = Gacha::connect(None).expect("内存库该建得起来");
        assert!(
            g.store.path().is_none(),
            "内存库必须连路径都没有 ⇒ 一条文件都不会产生"
        );
        let before = g.balance();
        let (lines, best, balance) = g.pull(10, true, Lang::Zh, 1.5).expect("十连该成功");
        assert!(
            lines[0].starts_with("10 抽") && lines[0].contains(best.as_str()),
            "默认档的标题行要同时说出抽了几次和最高档：{lines:?}"
        );
        // 名字段的判据是"能在目录里对上"（前缀，耐截断），不是"看起来像中文"——后者会把任何一个
        // 非 ASCII 的坏值都放过，而这一处要防的正是"卡片还在打 slug"。
        let named = lines
            .iter()
            .skip(1)
            .filter_map(|l| item_name_field(l, Lang::Zh))
            .filter(|f| !f.is_ascii() && g.catalog.items.iter().any(|i| i.name.starts_with(f)))
            .count();
        assert!(
            named > 0,
            "结果行的名字段对不上目录里的中文名 ⇒ catalog 查找没接上，卡片还在打 slug：{lines:?}"
        );
        assert!(
            balance >= before,
            "抽取只加碎片（v1 免费软通货，D2），余额倒退说明扣了不该扣的钱"
        );
        // 演示标**不在这些行里**（它由 `card.rs` 的标题行徽标承担）：这里钉住的是"内存库这条路
        // 确实一个字节都没写盘"。
        assert!(
            !lines.iter().any(|l| l.contains("DRYRUN")),
            "演示标跑进结果行了 ⇒ 卡片上会有两处，而只有一处是签收过的"
        );
    }

    /// 逐 locale 真抽十连，每一行过一遍字体子集。这条抓的是"某个 locale 的模板里有一个没进
    /// 子集的字"，而物品名来自目录、不在 `ui_strings` 里 ⇒ **缺字只有这里能红**（画布上它只会
    /// 变成一个 `?` 或方框）。尺 = [`overlay_common::text::missing`]，与栅格化那条路同源。
    #[test]
    fn every_locale_pull_line_is_sayable_with_the_bundled_font() {
        let mut g = Gacha::connect(None).expect("内存库该建得起来");
        for l in Lang::ALL {
            let (lines, _best, _b) = g.pull(10, true, l, 1.5).expect("十连该成功");
            for line in &lines {
                if let Some(c) = overlay_common::text::missing(line) {
                    panic!("{l:?} 的结果行「{line}」里的 {c:?} 不在字体子集里");
                }
            }
        }
    }

    /// **整链重放**（本文件 [`Gacha::pull`] 头登记的那条替代仪器）：真落库 → 换一条连接重开 →
    /// `Store::verify` 从磁盘上那 10 行重算哈希链与 `verify_replay`。
    ///
    /// ⚠️ 这条**不是**"同 `--seed` 跑两次 ⇒ 卡面逐同"（计划里那句已被否证：`--seed` 喂行为内核，
    /// 库里 `meta.seed` 首次播种走系统随机源，接 `Store::reseed` = 清空账本）。确定性在这里的
    /// 含义是"账本能被重放"，不是"下一次抽取可预测"。
    #[test]
    fn real_archive_is_written_and_replays_the_chain() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("opet.db");
        let mut g = Gacha::connect(path.to_str()).expect("--db 指向可写路径就该建库");
        // 这次传 `en`：既验"落库这条路"，也验 `lang` 参数真的走到底（默认档在上一条测过了）。
        let (lines, _best, balance) = g.pull(10, false, Lang::En, 1.5).expect("十连该成功");
        assert!(
            lines[0].starts_with("10 pulled"),
            "英文标题行没按 locale 走（`lang` 参数没接上）：{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("DRYRUN") || l.contains(Lang::En.t(Key::DryRun))),
            "真落库不许打演示标（否则用户不知道哪次真的写进了存档）"
        );
        // 换一条连接重开：只有磁盘上真有东西，才认得这 10 条、链才验得过
        let reopened = Store::open(&path).expect("重开存档");
        let catalog = Catalog::default_v1().unwrap();
        let rep = reopened
            .verify(
                &catalog,
                petstore::DEFAULT_PROFILE,
                petstore::DEFAULT_BANNER,
            )
            .expect("账本该自洽");
        assert_eq!(rep.len, 10, "落库条数");
        assert_eq!(
            balance,
            i64::try_from(reopened.shards(Currency::Shards).unwrap()).unwrap(),
            "菜单显示的余额必须就是库里那个数"
        );
    }

    /// 相对路径取决于 cwd，而宠物可能被服务起 ⇒ **只能拒绝，不能猜**；拒绝必须发生在一笔写之前。
    ///
    /// 这一条在 Windows 侧比 Linux 侧更值得钉：`Store::open` 用的是平台的 `is_absolute`，所以在
    /// windows target 上 `C:\…` 与 `\\srv\share\…` 算绝对、`X:foo`（**盘相对**）不算 —— 与
    /// [`petstore::paths`] 里那个纯字符串的 `is_windows_absolute` 同一套规则，两边各测各的矩阵。
    #[test]
    fn a_relative_path_is_refused_before_anything_is_written() {
        let Err(err) = Gacha::connect(Some("opet_m5_relative.db")) else {
            panic!("相对路径该被拒绝");
        };
        assert!(err.contains("绝对路径"), "错误要读得懂: {err}");
        assert!(
            !std::path::Path::new("opet_m5_relative.db").exists(),
            "拒绝就该一个字都没写"
        );
    }
}
