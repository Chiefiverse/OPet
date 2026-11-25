//! 逐件收藏册：一条记录、它点下去去哪张表、以及那一行该怎么写。
//!
//! 2026-11-24（M7-b 前置）整段从 `overlay-wayland/src/app.rs` 搬上来，**函数体一字未改**。
//!
//! 这里最要紧的一格是 [`AlbumTarget`] 是**判别式**而不是"`Option<usize>` + 一个布尔"：
//! 收藏册那一面同时有配件与本体两套下标空间，旁表（"这个数字属于哪张表"）要跨
//! `show_panel` / `refresh_panel_rows` / `album_screen` 保持对齐，而本项目为这件事付过学费。
//! 判别式让"哪张表"和"下标"**必须一起写** ⇒ 写错一张表是编译错误，不是运行时的一次错穿。
//! 这条判据与像素无关、与平台无关 ⇒ 它属于共用层，两家客户端只留点击那一路。
//!
//! 文档里凡是写 `App::candidates` / `App::bodies` / `App::swap_body` 的地方，读作
//! "**调用方**的那两张表与那两个动作"：本层只把下标算出来并交回，穿上与换本体都留在各家。

use overlay_common::{menu, panel};
use petcore::gacha::{ItemKind, Rarity, Slot};

/// 逐件收藏册里**一件**要说的事（还没变成能画的行）。
///
/// 它刻意不是 `CatalogItem` 的引用：一件"装包来的池外件"（catalog 不认的 id）也要能进这张表，
/// 而那时只有 id 字符串可说。
///
/// 一行点下去要去**哪张表**（P5 = 这一面第一次同时出现两套下标空间）。
///
/// 为什么不写成 `Option<usize>` 加一个"这是宠物还是配件"的布尔：那样能表示
/// `{ index: Some(3), kind: Accessory }` 而 `candidates[3]` 其实是另一件东西 —— 也就是把"这个数字
/// 属于哪张表"交给**旁表**去说，而旁表要跨 `show_panel` / `refresh_panel_rows` / `album_screen`
/// 保持对齐（[`panel::PanelItem::Wear`] 那条注释立的同一条纪律，本项目为它付过学费）。判别式让
/// "哪张表"和"下标"**必须一起写**，写错一张表就是编译错误而不是运行时的一次错穿。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumTarget {
    /// `App::candidates` 的下标 ⇒ 点它走 `App::pick_and_wear`。
    Accessory(usize),
    /// `App::bodies` 的下标 ⇒ 点它走 `App::swap_body`。⚠️ 与上面那个数字**不是同一个空间**。
    Body(usize),
    /// 只读行：未拥有 / 磁盘没产物 / 不 `fits` 本体 / 跨档在本体侧出局。
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumEntry {
    /// 这一行**此刻**点得动吗、点下去去哪张表。`None` = 只读行，屏上没有强调条（画师读的可点性
    /// 正是这一格，见 [`card::paint_panel`]）。
    pub target: AlbumTarget,
    /// catalog 里的名字；catalog 不认这个 id 时退化成 id 本身（ASCII）—— 与
    /// [`Gacha::recent_lines`] **同一个既有口径**，不是新洞。
    pub name: String,
    pub rarity: Rarity,
    pub slot: Option<Slot>,
    /// `kind == Pet` ⇒ 行尾打一个 ASCII `pet` 标。
    ///
    /// ⚠️ 它**不参与路由**（路由看 [`AlbumEntry::target`]）：P4 那一轮它同时是"为什么这一行永远
    /// 不给下标"的理由（那时 `wear()` 解的是配件三件套，传一个宠物 id 进去在类型上合法而屏上什么
    /// 都没有），P5 之后宠物有了自己的下标空间，这一格就只剩**排版**职责（行尾那个 `pet` 字）。
    /// 留着一个不参与判定的布尔而它恰好和判别式同真是巧合不是约束 ⇒ 所以这里的注释要说清它不管什么。
    pub kind_pet: bool,
    /// 重复数 = `item_count − 1`，与 [`Gacha::collection_lines`] 同一个折算口径。
    pub dup: u64,
}

/// 逐件那一本的行文本 = **一行一件**，宽度纪律立在产行的这一刻（[`Gacha::missing_lines`]
/// 那条同族理由：必须问 [`menu::fit_to`] 本身，不许拿 [`menu::line_width`] 自己算）。
///
/// ⚠️ **预算按可点/只读分两档**：可点行让出左端那条强调条 ⇒ [`panel::ROW_LABEL_W`]（209），
/// 只读行是 [`panel::ROW_TEXT_W`]（216）。**用错一档就是"生产者和排版各有各的账"**：按 216
/// 裁出来的可点行送到 `menu::label_rect` 手里就差出一个汉字，被排版截成 `…`，而"这件叫什么"
/// 是用户点它的唯一依据。
///
/// 装不下时**按 `dup` → `slot` 的顺序丢词**，最后只剩 `{RAR} {name}`，**绝不让 `card` 截成
/// `…`**：丢掉的两个词都是"能从别处再读到"的信息（重复数在汇总那一行、挂点在换装页），
/// 而一个 `…` 说的是"这件的名字我不知道"，那是假信息。
/// 极端情况（名字本身就超预算）无词可丢 ⇒ 只能交给排版，那一条由
/// `every_panel_row_is_sayable_and_never_truncated` 用**真 catalog 真三档缩放**当场量红，
/// 不是靠这句注释保证。
///
/// 这里**不需要 `lang`**：token 全是 ASCII（`Rarity::as_str` / `Slot::as_str` /
/// `ItemKind::as_str` / `x{dup}`）加上本来就在子集里的 catalog 名 ⇒ 逐件那几张行**与语言无关**，
/// 而"签名里有、函数体不读"是本项目反复抓的形状（见 [`panel::collection_rows`] 那条同一纪律）。
pub fn album_rows(entries: &[AlbumEntry], scale: f32) -> Vec<panel::PanelRow> {
    entries
        .iter()
        .map(|e| {
            let actionable = !matches!(e.target, AlbumTarget::None);
            let width = if actionable {
                panel::ROW_LABEL_W
            } else {
                panel::ROW_TEXT_W
            };
            let budget = menu::budget_for(width, scale);
            let slot = e.slot.map(Slot::as_str);
            let dup = if e.dup > 0 {
                Some(format!("x{}", e.dup))
            } else {
                None
            };
            // 从"信息最全"往下退：① 全量 ② 丢重复数 ③ 再丢挂点 ④ 只剩档名 + 名字。
            let render = |with_slot: bool, with_dup: bool| {
                let mut s = String::from(e.rarity.as_str());
                if with_slot {
                    if let Some(sl) = slot {
                        s.push(' ');
                        s.push_str(sl);
                    }
                }
                s.push(' ');
                s.push_str(&e.name);
                if e.kind_pet {
                    s.push(' ');
                    s.push_str(ItemKind::Pet.as_str());
                }
                if with_dup {
                    if let Some(d) = &dup {
                        s.push(' ');
                        s.push_str(d);
                    }
                }
                s
            };
            let ladder = [
                render(true, true),
                render(true, false),
                render(false, false),
            ];
            let mut text = ladder[ladder.len() - 1].clone();
            for cand in &ladder {
                if menu::fit_to(cand, budget, scale) == *cand {
                    text = cand.clone();
                    break;
                }
            }
            panel::PanelRow {
                item: match e.target {
                    AlbumTarget::Accessory(i) => panel::PanelItem::Wear(i),
                    AlbumTarget::Body(i) => panel::PanelItem::Body(i),
                    AlbumTarget::None => panel::PanelItem::Info,
                },
                text,
            }
        })
        .collect()
}
