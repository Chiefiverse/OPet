//! 换装的**库与资产**那一半：连库、落库、扫盘、解码（第二批 M5e）。
//!
//! ## 为什么这个模块不带 `#[cfg(windows)]`
//!
//! [`crate::app`] 顶上那一节解释了"Windows 那一整块收进 `#[cfg(windows)] mod win`"：宿主上
//! 看不见的代码必须在类型系统层面也不存在，否则 clippy 的未使用导入会红。那个理由在这里
//! **不成立**，因为这一文件用到的类型没有一个是 Windows 的：
//! `Store` / `Catalog` / `AccessoryManifest` / [`Atlas`] / [`Worn`] 全跨平台。
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
use petcore::gacha::{Catalog, Currency, GachaParams, ItemKind, Rarity};
use petstore::Store;
use petstore::paths::AssetRoot;

use overlay_common::atlas::Atlas;
use overlay_common::l10n::Lang;
use overlay_common::menu;

use crate::stage::Worn;

/// 库与目录成对出现（`connect` 一起建、一起失败），所以没有"有库没目录"的中间态。
///
/// M5e 起它从 `app::win` 搬到这里，只有一个理由：`app::win` 是 `#[cfg(windows)]`-only，
/// 而 [`write_equip`] 要它 ⇒ 不搬就得让"演示态不落库"这条判据在宿主上没有读数。
/// Wayland `app.rs` 那个同名的 `Gacha` 还留在原处 —— 那边整块本来就能在宿主上编译（layer-shell
/// 的测试靠 `--probe` 之外的那条构造路径），不需要这一刀。
pub struct Gacha {
    pub store: Store,
    /// 公示目录。`connect` 已经把它装进库了，这里留一份是为了**显示名**（[`Gacha::pull`]
    /// 查 `item_id → name`）—— 与 Wayland 那份 `Gacha` 同一个字段、同一个理由。
    catalog: Catalog,
}

impl Gacha {
    /// 连库 + 装目录。`None` = 内存库（一条文件都不产生，UI 全程打 DRYRUN 标）。
    ///
    /// 与 Wayland `Gacha::connect` 逐字同构（含"不做 canonicalize：它要求文件已存在，
    /// 第一次运行必然失败，反而把'能建库'这条正路堵死"那条处置）。
    pub fn connect(db: Option<&str>) -> Result<Self, String> {
        let mut store = match db {
            None => Store::in_memory().map_err(|e| format!("内存库建不起来: {e}"))?,
            Some(p) => Store::open(Path::new(p)).map_err(|e| format!("打不开存档 {p}: {e}"))?,
        };
        let catalog = Catalog::default_v1().map_err(|e| format!("公示目录读不出来: {e}"))?;
        store
            .install_catalog(&catalog)
            .map_err(|e| format!("装目录失败: {e}"))?;
        Ok(Self { store, catalog })
    }

    /// 碎片余额。`u64 → i64` 只在天文数字上饱和 —— 宁可显示封顶，也不许把余额读成负数。
    pub fn balance(&self) -> i64 {
        self.store
            .shards(Currency::Shards)
            .ok()
            .map(|v| i64::try_from(v).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    /// 抽 `count` 次并落库，返回菜单要的三样：结果卡行、最高档、抽完的余额（M6）。
    ///
    /// 与 Wayland `Gacha::pull` 同一份语义，逐条对齐：
    /// · **错误只是一条给人看的字**（进菜单错误行），调用方不许因为它而停环（硬约束 3）；
    /// · 显示名 = catalog 里的**中文**名，查不到回退 `item_id` ⇒ **绝不留白**（四 locale 一律
    ///   显示目录中文，那条取舍 = §7-R12，不在这里重新讨论）；
    /// · `lang`/`scale` 在**这一刻**烤进字符串 ⇒ 已经挂着的卡不会被后来的语言/缩放切换重排
    ///   （§7-R10，那条测例 `baked_lines_keep_the_language_they_were_pulled_in` 在共用层）。
    ///
    /// ⚠️ **seed 不在参数面上**，而且这不是漏接线：库里那一枚 `meta.seed` 第一次播种走的是
    /// 系统随机源（`Store::bootstrap`），而 `--seed` 喂的是**行为内核**的骰子。把 `--seed`
    /// 接成 `Store::reseed` 会**清空账本与库存**（那函数自己的注释就写着"留着旧历史，链锚点
    /// 就和新的 seed 对不上，`audit verify` 必然红"），GUI 里一次点击抹掉用户的历史是不许
    /// 发生的事 ⇒ 两家客户端都不做。计划里那句"同 `--seed` 跑两次 ⇒ 卡面逐同"因此**在 GUI
    /// 这条路上不成立**，Windows 侧的确定性改由本文件测试里那条
    /// `real_archive_is_written_and_replays_the_chain` 与真机上的 `audit verify` 出（M8 的回报表
    /// 按这两格填）。
    pub fn pull(
        &mut self,
        count: u32,
        dry_run: bool,
        lang: Lang,
        scale: f32,
    ) -> Result<(Vec<String>, Rarity, i64), String> {
        let report = self
            .store
            .pull_for(
                &GachaParams::V1,
                &self.catalog,
                petstore::DEFAULT_BANNER,
                petstore::DEFAULT_PROFILE,
                count,
            )
            .map_err(|e| format!("PULL {count}: {e}"))?;
        let items: Vec<(String, Rarity, bool)> = report
            .outcomes
            .iter()
            .map(|o| {
                let name = self
                    .catalog
                    .find(&o.item_id)
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| o.item_id.clone());
                (name, o.rarity, o.duplicate)
            })
            .collect();
        let lines = menu::report_lines(
            &report.counts,
            &items,
            u64::from(report.shards_gained),
            lang,
            scale,
        );
        let best = menu::best_rarity(&report.counts).unwrap_or(Rarity::N);
        let balance = self.balance();
        eprintln!(
            "gacha: 抽 {count} ⇒ N={} R={} SR={} SSR={} 碎片 +{} 余额 {balance}（{}）",
            report.counts[0],
            report.counts[1],
            report.counts[2],
            report.counts[3],
            report.shards_gained,
            if dry_run { "DRYRUN" } else { "已落库" }
        );
        Ok((lines, best, balance))
    }
}

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

    /// **"改了像素"与"记进 equipped"在源码里必须是同一个动作**（P3 的接线门禁）。
    ///
    /// Wayland 那一条（`pixels_and_the_ledger_are_written_by_one_function_only`）不是洁癖，是
    /// 变异实测逼出来的：删掉 `equip` 里那句 `persist_worn`，那边 526 条全绿 —— 因为 `App` 在
    /// 没有 compositor 的机器上构造不起来。本平台**更极端**：`App` 整个在 `#[cfg(windows)]` 里，
    /// 宿主上一条构造测例都写不出来 ⇒ 这一族唯一的防线就是读源码文本。
    ///
    /// 与 `cli.rs` 的 `every_flag_advertised_in_usage_exists` 同族。**读源码文本的门禁天生脆**，
    /// 所以只数最小的几件事，每条都带"为什么是这一条"；重构改名会红，那是**要人来看一眼**的红。
    #[test]
    fn the_pixels_and_the_ledger_are_written_by_one_function_each() {
        let src = include_str!("app.rs");
        // 只数测试模块**之外**的代码。本平台今天没有这个模块（构造不起来 ⇒ 写了也跑不了），
        // 但这条门禁自己的字面量就在下面这几行里 ⇒ 以后谁给 `app.rs` 加了测例，切掉它才不数到自己。
        let code = match src.find("\nmod tests {") {
            Some(at) => &src[..at],
            None => src,
        };

        // 函数体 = 从 `fn name(` 起，到下一个同级 `fn` 或本函数收尾的那个 `}`。
        // 缩进 8 空格 = `#[cfg(windows)] mod win { impl App { … } }` 三层之内。
        let body = |name: &str| -> String {
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
        };

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

        // ③ 三个装载点都真的走 `wear`：穿上（`equip`）/ 换一件（`cycle_accessory`）/ 启动恢复
        //    （`restore_worn`）。少一个 = 那条路又自己写了一遍像素。
        //    ⚠️ 与 Wayland 那个 5 的差 = 本平台没有收藏册点名（P4，M7）与换本体（P5）⇒
        //    **这两块面接进来时这个数要跟着涨**，红的时候先看是不是那一格落地了。
        let calls = code.matches("self.wear(").count();
        assert_eq!(
            calls, 3,
            "`wear` 应当被 穿上 / 换一件 / 启动恢复 三处共用，实际调用点 {calls} 个"
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
        assert_eq!(
            code.matches("write_equip(").count(),
            1,
            "`write_equip` 有了第二个调用者 ⇒ 演示态的判据出现了第二份实现"
        );
        assert_eq!(
            code.matches("clear_equip(").count(),
            1,
            "`clear_equip` 有了第二个调用者 ⇒ 同上，方向反过来"
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
