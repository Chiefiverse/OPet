//! 本体库：启动扫盘的四道门禁、一只可换本体的形状，与"点中一只宠物"的纯判定。
//!
//! 2026-11-24（M7-b 前置）整段从 `overlay-wayland/src/app.rs` 搬上来，**函数体一字未改**。
//!
//! ## 为什么"发现"可以共用而"穿上"不行
//!
//! [`check_body`] 一像素都不解：清单 → 格子档 → 两件伴生产物存在 → 轮廓解得开。第四道读的是
//! `overlay_common::region::Regions`，它在第一批就上了共用层（零平台类型，产出的是一叠
//! `CellRect`），所以**两家都能解**：Wayland 拿它建 `wl_region`，Windows 的 `WM_NCHITTEST`
//! 走的是叠完之后的那份像素（"屏上像素是唯一真值"，见 `overlay-win32/src/stage.rs` 模块头）。
//! Windows 于是**用不到** [`BodyCandidate::regions`] 这一格，但仍为它付几 KB —— 换回来的是一条
//! 完整的三件套产物要求（`pack.py` 永远写 `polygon.json`），两家同一本账。
//! 这一格是明写的取舍，不是遗漏：**以后谁要在 Windows 侧把它删掉，先回答"少一个文件的本体包
//! 算不算合法产物"**，那才是这条门禁的意义。
//!
//! [`plan_swap_body`] 是纯函数（四个 `bool`），方法论与 `overlay_common::dress::plan_pick_wear`
//! 同一条：`App` 在没有 compositor / 没有窗口站的机器上构造不起来 ⇒ 判定必须先改写成
//! 写得出来、也测得出来的形状。四道的**顺序**是判据的一部分，别当排版看。

use overlay_common::dress::BODY_SLOT;
use overlay_common::region::Regions;
use petcore::assets::{Grid, Manifest};
use petcore::gacha::ItemKind;
use petcore::intent::SURFACE_MARGIN;
use petstore::Store;

/// 「点中一只宠物」要判的四档结论（**纯**版本；方法论与 [`PickWear`] 同一条：`App` 在没有
/// compositor 的机器上构造不起来 ⇒ 判定必须先改写成写得出来、也测得出来的形状）。
///
/// 比配件那一族多一档 `Same` 而少一档 `AlreadyWorn` 的写法差别不是审美：**"身上已经是这只"
/// 是一个不需要"穿着"这个前提的状态**（本体永远在，`worn` 那格才可能是 `None`），所以它的判据
/// 是 `id` 而不是下标 —— 下标是`Body` 那套（`App::bodies`），而"当前是哪只"记在
/// `App::body_id`。用下标比就会有两个"当前本体是谁"。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SwapBody {
    /// 四道都过 ⇒ 走 `App::swap_body` 那九步。
    Switch,
    /// 点的就是当前这只 ⇒ 不解像素、不重投帧、**账也不写**（写一次 `body` 行等于给"没发生的
    /// 那次换"留一条重启读数）。
    Same,
    /// 某一道没过 ⇒ 一个字都不动。带**那道的名字**，因为三道的文案是不同的事实：
    /// "这一格根本没有这只"、"档不同所以运行时换不了"、"库存没验过拥有"是三句不同的回答。
    Refused(&'static str),
}

/// [`SwapBody`] 的判定本体，四道的**顺序 = 便宜的、以及"错了会更贵"的在前**。
///
/// ⚠️ `same_grid` 排在库存那道**之前**，理由不是"更便宜"而是"更贵的那个错"：`atlas.reload`
/// 自检的是"新 sheet 的像素尺寸 == `cols×cell_w, rows×cell_h`"，而那三个数仍是**旧**本体的
/// （`App::atlas` 只在构造时由清单算出，`reload` 不动它们）。所以在档不同的时候，失败关闭
/// 的形状是"白付一次读盘 + 一次解码尝试 + 一句几何错误"，而**赌赢**（两份 WebP 尺寸恰好相同）
/// 的形状是"新像素配旧行表 ⇒ 屏上是一堆没人能解释的格子"。后者正是本项目反复要杀的静默。
///
/// 这一道的现实读数在扫盘那一步（[`check_body`] 第 ② 道 ⇒ 跨档的那只压根进不了
/// `App::bodies`，收藏册里是只读行、点不到）。留在这里是**第二道保险**：唯一合法走到它的
/// 调用者是"以后有人绕过发现直接往 `bodies` 里塞一只"，而那件事的读数只能来自单测。
///
/// `same_id` 排在最后（与 [`plan_pick_wear`] 把 `AlreadyWorn` 排在 `Refused` 之后同一个理由）：
/// 三道门禁里任何一道没过都不该给出"已经是这一只"这句**听起来像成功**的话 —— 那会把"我为什么
/// 点不动它"回答成一个不需要回答的问题。
pub fn plan_swap_body(
    in_range: bool,
    gate_passes: bool,
    same_id: bool,
    same_grid: bool,
) -> SwapBody {
    if !in_range {
        return SwapBody::Refused("这一格不在本体库里（启动扫盘之后库被改过？）");
    }
    if !same_grid {
        return SwapBody::Refused(
            "格子档与当前本体不同 ⇒ 运行时不动 surface 尺寸 / 目标尺寸 / shm 池，跨档只能重启",
        );
    }
    if !gate_passes {
        return SwapBody::Refused(
            "库存那道没过（未拥有，或给了 --db 而这一轮读不出库存）⇒ 没验过拥有不换",
        );
    }
    if same_id {
        return SwapBody::Same;
    }
    SwapBody::Switch
}
/// 本体库在哪 —— **只由环境决定**，与 [`accessory_root`] 是同一根规则的两个分支
/// （`petstore::paths::AssetRoot`：`OPET_PETS` → `$XDG_DATA_HOME/opet/pets` →
/// `$HOME/.local/share/opet/pets`）。**没有 `--pets` 命令行入口**：`--accessory` 已被用户删过
/// 的那条先例（"配件不是应该由菜单选择吗"），本体侧同理，库位置是装机事实不是用户意图。
pub fn pet_root() -> Option<std::path::PathBuf> {
    petstore::paths::AssetRoot::resolve(ItemKind::Pet).map(|r| r.path)
}

/// 收藏册里一只**换得上去**的本体 = 一个通过全部发现门禁的目录（**一像素未解**）。
///
/// 与 `Candidate` 同族，但多一个 `leaked`：`Sim<'m>` 与 `App::manifest` 的类型是
/// `&'static Manifest`，而这张表是**自己拥有**清单的 `Vec` ⇒ 借用活不到那一刻。
/// `load_manifest`（`:1390`）那边是直接 `Box::leak` 一份，这里照同一条策略，但**延到第一次
/// 真要换上它时才泄**并通过 [`BodyCandidate::leaked`] 缓存 ⇒ 一份进程内每只不同的宠物只泄一份
/// （几 KB），不是每次换都泄一份。
/// （不可行的另一条路：把清单存进一个 self-referential arena —— `QueueHandle<App>` 要求
/// `App: 'static`，任何"挂在 `App` 里的借用"都构造不出来。）
pub struct BodyCandidate {
    /// 库存那一侧的身份 = **库目录名**（与 `Candidate::id` 同一条既有裁定：`inventory.item_id`、
    /// P1 的 INV 门禁、`AssetDir::locate`、`equipped` 的 `body` 行认的都是这一个）。
    pub id: String,
    /// 清单里的 `slug` = `fits` 比的那个名字。⚠️ 它与 [`Self::id`] **可以不相等**，见
    /// [`discover_pets`] 那行 ⚠（今天四份 @192 产物里三份的 slug 写着 `cat_shiro` 就是这一格）。
    pub slug: String,
    pub dir: std::path::PathBuf,
    pub grid: Grid,
    pub manifest: Manifest,
    /// 本体自己的穿透轮廓（发现第 ④ 道解的，见 [`check_body`]）。
    ///
    /// **存下来而不是换的时候再解一次**：`App::swap_body` 的第 3 步于是没有任何 IO、不可能失败，
    /// "新清单 + 旧轮廓"这个半换形状就构造不出来。代价是每只几 KB，相对一张 sheet 的 45 MiB 是零。
    pub regions: Regions,
    pub leaked: Option<&'static Manifest>,
}

impl BodyCandidate {
    /// 第一次真要换上它时才 `Box::leak`，之后返缓存（字段说明里那三条理由）。
    pub fn leaked(&mut self) -> &'static Manifest {
        match self.leaked {
            Some(m) => m,
            None => {
                let m: &'static Manifest = Box::leak(Box::new(self.manifest.clone()));
                self.leaked = Some(m);
                m
            }
        }
    }

    /// 那只本体的**全部像素文件**，段序同 [`Manifest::sheet_segments`]（单张时代就是一条）。
    ///
    /// ⚠️ 名字来自清单，不拼 `"sheet.webp"`：拼死文件名 = 第二个"像素从哪来"的事实源，而它
    /// 只在清单恰好这么叫的时候对（§6.1.27 裁决 6）。`check_body` 第 ③ 道与 `AtlasSet` 装载
    /// 读的都是这一条 ⇒ 三处同一个键。
    pub fn sheets(&self) -> Vec<std::path::PathBuf> {
        self.manifest
            .sheet_files()
            .into_iter()
            .map(|f| self.dir.join(f))
            .collect()
    }
}

/// [`discover_pets`] 的内层：一个目录 → 一只可换的本体，或一句点名它错在哪的说明。
///
/// 门禁按**便宜**排序（清单 → 档 → 存在性 → 轮廓），一像素都不解。与 `check_accessory`
/// 同族，**两处故意不同**：
///
/// ② **格子档整块比较**（`Grid: PartialEq` 逐字段含 `cols/rows/w/h`），判据不是只比 `w/h`：
///    `rows` 决定解码缓冲大小与 polygon 面，`cols` 决定寻址。跨档在**扫盘这一步**出局 ⇒
///    收藏册那一行是只读、没有强调条（= P4 立好的形状，不是新发明）。为什么不在点击时报错：
///    `layer.set_size` / `viewport.set_destination` / `SlotPool` 那三处只在 `run()` 里发生一次，
///    运行时换档要走一遍 configure 往返；**启动允许跨档（surface 还没建），运行时不允许** ——
///    这条不对称是协议限制不是数据模型限制。
///
/// ④ **没有**配件侧那道 `first_missing` 逐格轮廓覆盖检查。本体的 `region_rects` 有设计好的
///    "整格兜底"，缺一格只是"点击面积稍大"；配件缺格是"帽子画在 region 外"。在这里照抄过来
///    就是又一条**假门禁**（它会拒绝一堆画得对的本体）。
pub fn check_body(dir: &std::path::Path, cur: &Grid) -> Result<BodyCandidate, String> {
    let path = dir.join("pet.json");
    let name = path.to_str().ok_or("本体清单路径不是 UTF-8")?;
    // ① 清单本身。`Manifest::validate` 已覆盖 format / slug / `cols==8` / 格像素 / sheet 像素 /
    //    `idle` 必在 / 状态名 / rows 越界 / fps / `anchor.feet` 在框内 / `outfit_anchors` /
    //    `input_polygon` / facing ⇒ 这里不重复任何一道。
    let manifest = Manifest::load(name).map_err(|e| format!("清单 {name} 加载失败: {e}"))?;
    // ② 格子档（拍板② 的落点）。
    if manifest.grid != *cur {
        let (a, b) = (cur, &manifest.grid);
        return Err(format!(
            "拒绝本体 {}：格子档不同（当前 {}×{}、{} 列 × {} 行；这一份 {}×{}、{} 列 × {} 行）\
             ⇒ 运行时换本体不动 surface 尺寸，跨档只能重启",
            manifest.slug, a.w, a.h, a.cols, a.rows, b.w, b.h, b.cols, b.rows
        ));
    }
    // ③ 伴生产物**存在**（不读内容 ⇒ 把 `discovery_never_touches_the_sheet…` 那条扩到宠物）。
    //    像素那几件的名字逐条来自清单：分段本体少一张 = 拒绝（那只在收藏册里是只读行），
    //    而不是"到点上才解不开"—— 那会让收藏册给一只装不出来的宠物画上强调条。
    for what in manifest
        .sheet_files()
        .into_iter()
        .chain(std::iter::once("polygon.json"))
    {
        if !dir.join(what).is_file() {
            return Err(format!("拒绝本体 {}：目录里没有 {what}", manifest.slug));
        }
    }
    // ④ 轮廓解得开（`Regions::load` 自己会交叉核对 polygon 的格尺寸 vs 清单）。
    let polygon = dir.join("polygon.json");
    let regions = Regions::load(
        polygon.to_str().ok_or("本体 polygon 路径不是 UTF-8")?,
        manifest.grid.w,
        manifest.grid.h,
        SURFACE_MARGIN,
    )
    .map_err(|e| format!("{}: {e}", polygon.display()))?;
    Ok(BodyCandidate {
        // 身份 = **目录名**，与配件侧同一条裁定（理由见 `Candidate::id` 那段）。
        id: dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} 的目录名不是合法 UTF-8 ⇒ 无法与库存对齐", dir.display()))?
            .to_string(),
        slug: manifest.slug.clone(),
        dir: dir.to_path_buf(),
        grid: manifest.grid,
        regions,
        leaked: None,
        manifest,
    })
}

/// 启动时扫一遍本体库 ⇒ 收藏册里**点得动**的那些宠物行（元数据 + 轮廓，一像素未解）。
///
/// 与 [`discover_accessories`] 同一条最要紧的语义：**这里一个 `Err` 都不出**。库里一只坏本体的
/// 后果是"少一只可换"，不是"宠物起不来"（硬约束 3），每条跳过打一行点名它错在哪道的 stderr。
///
/// ⚠️ 起点本体**不要求**在这张表里：`--manifest` 仍是硬要求（`cli.rs:80` 不动），它退化成
/// "库里没那一行时用什么"（拍板③）。所以这张表空 ⇒ 收藏册的宠物行全部只读，那是**正确行为**。
pub fn discover_pets(root: &std::path::Path, cur: &Grid) -> Vec<BodyCandidate> {
    let mut found: Vec<BodyCandidate> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!(
                "pets: 库 {} 打不开（{e}）⇒ 收藏册里的宠物行点了不会有效果",
                root.display()
            );
            return found;
        }
    };
    // 排序理由与配件侧逐字相同：`read_dir` 的顺序由文件系统给，两次启动可能不同。
    let mut dirs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("pet.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        match check_body(&dir, cur) {
            Ok(cand) => {
                // 与配件侧同一行 ⚠（`:1557`）：不一致就是"库存按 A 记账、屏上叫 B"，而
                // "为什么这只我抽到了却不让我换"问不出来。**拒绝文案点的是 `fits` 或档，不是这个。**
                if cand.id != cand.slug {
                    eprintln!(
                        "pets:   ⚠ 目录名 {} 与清单 slug {} 不一致 ⇒ 库存按 {} 记账、屏上叫 {}",
                        cand.id, cand.slug, cand.id, cand.slug
                    );
                }
                eprintln!(
                    "pets:   {}（{}）换得上去 · {}×{} {}列×{}行 · 轮廓 {} 格 · sheet 未解（点上才读）",
                    cand.id,
                    cand.slug,
                    cand.grid.w,
                    cand.grid.h,
                    cand.grid.cols,
                    cand.grid.rows,
                    cand.regions.frame_count()
                );
                found.push(cand);
            }
            Err(err) => eprintln!(
                "pets: 跳过 {}（{}）⇒ 这一只在收藏册里是只读行，其余不受影响",
                dir.display(),
                err
            ),
        }
    }
    eprintln!(
        "pets: 库 {} ⇒ {} 只可换{}",
        root.display(),
        found.len(),
        if found.is_empty() {
            "（宠物行全部只读；把 `tools/pack.py` 的三件套放进这里）"
        } else {
            ""
        }
    );
    found
}

/// 启动那一步读库里的 `body` 行（拍板③ 唯一的读数）。
///
/// 2026-11-25（M7-d）从 `overlay-wayland/src/app.rs` 搬上来，**函数体一字未改**：Windows 那侧
/// 的起点本体也要读同一行，而"库里那一行能不能当起点本体"是一条判据 ⇒ 判据不许有第二个实现
/// （`overlay-report` 这个 crate 存在的唯一理由，同 `Gacha` 那一族）。
///
/// **只在 `--db` 给了的时候**开库，读完这个 `Store` 就随函数返回一起 drop ⇒ 无 `--db` 那一轮
/// 一个文件字节都不碰（与"演示态一律放行、也不记本体这一笔"同一条口径）。
///
/// 代价明写：带 `--db` 的这一轮库会被开**两次**（这里一次只读、各家 `ensure_gacha` 那一次）。
/// 选它而不是选"半个初始化的 `App`"：`App` 的构造点排在 surface/窗口之后，而起点本体必须排在
/// **解第一张 sheet 之前**（峰值 = 两只本体 ≈147 MB，它落在 ≤200 MiB 门禁内 ⇒ 什么都不会红，
/// 唯一的读数就是这段代码本身）。让本体去等 surface 就得把清单字段换成 `Option` 并给每一处补
/// 一份"还没有清单"的形状 —— 那比一次 sqlite 打开大得多。**以后若有第三个读取者，先问它能不能
/// 并入 `ensure_gacha`。**
///
/// 三条出口合成一个 `Option`：**没有那一行**与**读失败**都是 `None`（= 老库、行为逐字节不变），
/// 而读失败**必须**先点名一行 ⇒ 静默退化成兜底就是假门禁（`--cell` 假开关那一族）。
///
/// ⚠️ 它读的是 `Store::equipped_slot` 那一条 SQL（`WHERE profile_id = ?1 AND slot = ?2`），
/// 而 `overlay_common::dress::split_body_row` 读的是 `Store::equipped` 的整表 ⇒ 同一张表两个
/// 读法。它们对"多于一条 `body` 行"给同一个答案（两条都按 `ORDER BY slot, item_id` 取首条），
/// 所以这不是两个事实源；合成一条读法的代价是"启动多开一次库"或"恢复侧先跑起来才能拿到本体"
/// 那条循环。
pub fn db_body_row(db: Option<&str>) -> Option<String> {
    let path = db?;
    match Store::open(std::path::Path::new(path))
        .and_then(|s| s.equipped_slot(petstore::DEFAULT_PROFILE, BODY_SLOT))
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!(
                "body: 读 `equipped` 的 {BODY_SLOT} 行失败（{e}）⇒ 这一轮用 `--manifest` 那只（兜底）"
            );
            None
        }
    }
}

/// [`plan_body_override`] 的结论。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyChoice {
    /// 覆盖成立 = 起点本体换成**启动扫盘交出来的表**里那一格（调用方拿它去 `bodies[i]`）。
    ///
    /// ⚠️ 判据来自发现表 ⇒ **覆盖天然同档**（[`check_body`] 第 ② 道已经按 `--manifest` 的
    /// `Grid` 过滤过）。所以"启动允许跨档"这一句今天是**协议层面的陈述**（surface/窗口尚未建，
    /// 改尺寸合法），不是这条路径的读数：跨档的那一只在这里落到 `Keep(理由)` 而屏上从来
    /// 不需要改尺寸。真要支持跨档，改的是发现那道过滤（把它做成启动/运行时两套），不是这里。
    Override(usize),
    /// 不覆盖。`Keep(None)` = 库里压根没有那一行（老库、以及一切没换过本体的库）⇒ **一个字
    /// 都不打**，因为那是常态而启动噪音要留给"出事了"。`Keep(Some(理由))` 必须点名。
    Keep(Option<&'static str>),
}

/// "库里那一行能不能当起点本体"的**纯**判定（拆出来的唯一理由 = 让那条读数在 headless 里也
/// 量得到：外层要开库、要解码、要 compositor，这里三样都不需要）。
///
/// 只认**发现表里有的**那一只，而且只按 `id` = 目录名认（库存与 `equipped` 记账用的就是它）。
/// 那句 `Keep` 的文案要说清"为什么不覆盖"有两个不同的原因（目录不存在 / 档不同被扫盘挡掉），
/// 而**不**把它们合并成"读不到"：这一行报的是"你上次换的那只现在这只机器上装不上"，
/// 用户下一步该去找产物，不是去查数据库。
///
/// ⚠️ `start_id` 那一条是 11-07 真机 E 轮实测出来的：库里那一行写的就是 `--manifest` 已经给的
/// 那一只时，旧版无条件走 `Override` ⇒ stderr 打出「起点本体从 cat_shiro 换成这只」而**什么也没
/// 换**。拍板③ 的原话是"**覆盖发生时必须打一行点名**"，读到的那一行不等于覆盖发生 ⇒ 这一臂今天
/// 走 `Keep(None)`（静默 = 常态），而它的读数是 `plan_body_override_wins_only_when_the_row_names_a_discovered_pet`
/// 那张真值表里的第 ③⑤ 两格（后者是"发现表为空而起点正是它"那一面 —— 判据必须排在 `position()`
/// 之前，否则那台机器上会平白多一行假点名）。
pub fn plan_body_override(row: Option<&str>, pet_ids: &[String], start_id: &str) -> BodyChoice {
    let Some(id) = row else {
        return BodyChoice::Keep(None);
    };
    // **先问"是不是本来就在身上"**，再问它在不在发现表里：那一行就是起点那只时这一轮什么都没
    // 换，而"没有本体库 / `--test-rect`"的机器上发现表是空的 ⇒ 把这条判据放在 `position()`
    // 之后会得到一句假点名（"上次换的那只装不上"）—— 它装得上，它就是身上这只。
    if id == start_id {
        return BodyChoice::Keep(None);
    }
    match pet_ids.iter().position(|p| p == id) {
        Some(i) => BodyChoice::Override(i),
        None => BodyChoice::Keep(Some(
            "库里那一行指的本体不在启动扫盘交出来的表里（目录不存在、清单或产物坏了，或格子档与 `--manifest` 那只不同）",
        )),
    }
}
