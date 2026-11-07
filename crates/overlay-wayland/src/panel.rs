//! 面板（收藏册 / 设置）= 第三块 layer-shell surface 的**纯逻辑**部分。
//!
//! 与 [`menu`] 同一条分工：这里只算"该画什么行、哪一行点得着、签名变没变"，**一个 wayland
//! 类型都不出现**（连 [`RectI`] 都复用 `menu` 那份），所以版面能 headless 判定，也能出图给人看。
//! 像素在 [`card`]，surface 生命周期在 [`app`]。
//!
//! ## 为什么另起一块 surface 而不是往菜单加第四页
//!
//! 菜单卡片是 `240×150` 固定的，而那道几何证明（§6.1.6）把可用高度算死了：
//! `ROW_STEP 30 × 3 + HEAD_H 22 + PAD 8 = 116`，footer 从 `124` 起 ⇒ 只剩 8 px，**第四行物理放不下**。
//! 收藏册要读的是"每档几条 / 每个名字"，设置要读的是"每一项此刻的值 + 点下去真的生效"，
//! 两者都不是三行能收的 ⇒ 只能换一块更大的面，而不是把菜单撑高（撑高 = 改 `CARD_H` =
//! 改那块 `wl_region` 与 placement 的全部既有证明）。
//!
//! ## 为什么开面板要先把菜单关掉
//!
//! [`menu::overlaps`] 那条注释立着本项目的一条实测结论：**同层两块 surface 的堆叠序在合成器侧
//! 不稳定**，所以菜单的 placement 用"矩形不相交"消除命中歧义与视觉压盖。第三块面若与菜单同时
//! 可见，就把那道两两判定变成三方判定，而它的收益（少一次点击）不值那个代价。 ⇒ [`app`] 里
//! 打开面板的路径**必须**先 [`app::App::close_menu`]，两块面永不同时在场。

use crate::l10n;
use crate::l10n::{Key, Lang};
use crate::menu::{self, RectI};
use crate::settings::Motion;

/// 宽度**刻意等于菜单**：同一道 `MAX_LINE_PX` 排版预算、同一套 `fit` 截断、同一台相机下的
/// 观感重量都能逐位复用。只有高度变。
pub const CARD_W: i32 = menu::CARD_W;
/// 收藏册内容区的上限。**6 = 5 行内容 + 1 行「关闭」**（见 [`collection_rows`]）。
///
/// 逐件那一本（「已获得」）用的是另一种切法：**4 行内容 + 1 行页码控件 + 1 行「关闭」**
/// ⇒ 见 [`ALBUM_PAGE_ROWS`]。同一张卡、两套切法，共同点只有"总行数不超过 `MAX_ROWS`"，
/// 因为 `MAX_ROWS` 唯一真正钉住的是 [`CARD_H`]（= 那块 `wl_region` 与 placement 的证明）。
pub const MAX_ROWS: usize = 6;
/// 逐件收藏册一屏放得下的**条目**行数 = `MAX_ROWS` 减掉「页码控件」与「关闭」那两行。
///
/// 它是 `4` 而不是"看内容多不多"：页码控件**只在这一本真的超过一页时**占一格（见
/// [`album_page`] 里那句"为什么不超过一页就干脆不设这一格"），所以这里按**最坏情况**留位 ⇒
/// 每屏条目数恒定，翻页时行下标不漂。
pub const ALBUM_PAGE_ROWS: usize = MAX_ROWS - 2;
/// 内容区第一行的上沿 = 上边距 + 标题带，与菜单同一条线（两块的标题在同一个 y 上，
/// 换面时视线不用重新找位置）。
pub const ROWS_TOP: i32 = menu::PAD + menu::HEAD_H;
/// `8 + 22 + 5×30 + 26 + 8 = 214`。上下各留 `PAD`，行间缝 = `ROW_STEP - ROW_H` = 4 px
/// （缝是真的"点不着"，见 [`press`]）。
pub const CARD_H: i32 = ROWS_TOP + (MAX_ROWS as i32 - 1) * menu::ROW_STEP + menu::ROW_H + menu::PAD;

/// **只读行**（[`PanelItem::Info`]）文字能用的逻辑宽度 = 行矩形经 [`menu::row_text`] 让出左右
/// 各半个 `PAD` 之后剩下的那一段（`240 − 3×8 = 216`）。
///
/// 开这个口子的唯一理由：[`app`](crate::app) 的 `Gacha::missing_lines` 要在**产行的那一刻**
/// 决定"这一档还列几个名字、剩下折成尾标"，而它手里只有 `scale`、没有画布 —— 让它去猜一个
/// 字数就是"生产者和排版各有各的预算"那一族（`69fd158` 的兄弟）。
///
/// ⚠️ 改这里必须同时改 [`layout`] 的矩形算术，而**忘了改不会静默破版**：
/// `every_panel_row_is_sayable_and_never_truncated` 量的是真矩形真预算（`menu::row_text`），
/// 两边一漂移那条就红。
pub const ROW_TEXT_W: i32 = CARD_W - 3 * menu::PAD;

/// **可点行**（[`PanelItem::Wear`] 那一格）文字能用的逻辑宽度 = [`ROW_TEXT_W`] 再让出左端那条
/// 强调条（`menu::label_rect` 干的正是这件事，而 `card::paint_panel` 画可点行用的就是它）。
///
/// 开这个口子只为一件事：[`app`](crate::app) 的 `album_rows` 要在**产行的那一刻**决定"这一行
/// 还带不带得上挂点名 / 重复数"，而它手里只有 `scale`、没有画布。可点行的预算比只读行**小 7 px**
/// ⇒ 拿 `ROW_TEXT_W` 去裁可点行就会差出一个汉字，被排版截成 `…`，而"这件叫什么"是用户点它的
/// 唯一依据（`69fd158` 的兄弟：生产者和排版各有各的账）。
///
/// ⚠️ 同 [`ROW_TEXT_W`]：改这里要同时改 [`layout`] 的矩形算术，而漂移会被
/// `every_panel_row_is_sayable_and_never_truncated` 量出来（它量的是真矩形真预算）。
pub const ROW_LABEL_W: i32 = ROW_TEXT_W - (menu::ACCENT_W + menu::ACCENT_GAP);

/// 命中矩形。字段与 [`menu::ItemRect`] 同形而**不共用**：`item` 的类型不同，而把两种
/// item 塞进一个泛型参数会让"这块矩形属于谁"这件事在类型上消失 —— 面板与菜单的点击分流
/// 靠的正是它属于哪张面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRect {
    pub item: PanelItem,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl RowRect {
    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x as f32
            && px < (self.x + self.w) as f32
            && py >= self.y as f32
            && py < (self.y + self.h) as f32
    }

    pub const fn as_rect(self) -> RectI {
        RectI::new(self.x, self.y, self.w, self.h)
    }
}

/// 面板上"可能被点着的东西"。**`Info` 在命中表里但永远不激活**，这不是冗余而是有意的：
/// 收藏册的内容行需要有几何（画它、算它占哪一格），把它们从表里摘掉会让"行的位置"与
/// "可点的位置"变成两套索引 —— 而本项目被这种分家咬过（分页那次 `rects()` 把 footer 留在
/// 下标 3 正是为了不让索引漂）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelItem {
    /// 只读的一行（收藏册的内容行）。
    Info,
    /// 标题带 ⇒ **收藏册切 tab，设置页重载这一面的默认行**（[`app`](crate::app) 的
    /// `on_panel_press` 是唯一解释它的地方）。两面都是控件不是对称美学：设置页被
    /// 「概率」/「复核」借走时，除 `Close` 外**只有这一格能回到那五行**。
    Header,
    /// 设置第一行 ⇒ 循环语言并**写回 `config.toml`**。
    Lang,
    /// 设置第二行 ⇒ 循环动画强度并**写回 `config.toml`**。
    Motion,
    /// 设置第三行 ⇒ 把公示概率画进面板（读的是编进二进制的 `DEFAULT_RATES_JSON`，**不连库**）。
    Rates,
    /// 设置第四行 ⇒ 把账本复核结果画进面板（连库才有；读的是链，不写）。
    Verify,
    /// 最后一行 ⇒ 收掉面板。**宠物继续跑。**
    Close,
    /// 收藏册「已获得」里**逐件**的一行 ⇒ 点它 = 把这一件直接穿上身（[`app`](crate::app)
    /// 的 `pick_and_wear`）。
    ///
    /// 载荷是 **[`app`](crate::app) 的 `candidates` 下标**，不是新造一套"收藏条目 id"。两个理由：
    /// ① `wear()` / `persist_worn()` / `picked` / `DressView::visible` **本来就都说这个下标** ⇒
    /// 点击时无需任何反查，也就不需要一个和 `rows` 平行保存的 `Vec<CollectionEntry>` 旁表
    /// （旁表要跨 `show_panel` / `refresh_panel_rows` / `open_panel_lines` 保持对齐 =
    /// `69fd158` 那一族"命中表与画面分家"的形状，本项目为它付过学费）；
    /// ② 它让这一格保持 `Copy`，于是 [`press`] 的签名一个字不用动。
    ///
    /// ⚠️ **下标进不了 [`content_sig`]**（那里只混判别量），见函数体里那条条件式纪律。
    Wear(usize),
    /// 收藏册里**宠物**那一行 ⇒ 点它 = 当场换成那只（[`app`](crate::app) 的 `swap_body`，P5）。
    ///
    /// 载荷是 **[`app`](crate::app) 的 `bodies` 下标**，与 [`Self::Wear`] 的 `candidates` 下标
    /// 是**两套下标空间**，故意不合成一个 `usize`：合并后"这个数字指哪张表"就只剩旁边一段注释在
    /// 说，而注释不是判据（`the_album_routes_pets_to_the_body_index_space` 那条门禁的全文理由）。
    /// 分开写还有一层：**没决定过的那一臂会编译不过** —— `on_panel_press` 的 `match` 少一支就是
    /// 红的，而不是一个"点上去什么都不发生"的静默空洞（D14 那条假按钮形状的源头）。
    ///
    /// ⚠️ 与 [`Self::Wear`] 同一条纪律：**下标不进签名**（[`content_sig`] 只混判别量 9）。
    /// 当前是哪只本体不上屏 —— 它就是宠物自己那几个像素。
    Body(usize),
    /// 页码控件 ⇒ 这一格**的字**就是页码（`+N 更多 2/5`），点它 = 本 tab 翻下一页、末页回卷。
    ///
    /// 为什么翻页要占一个**行**而不是别的：收藏册逐件后必然超过一屏，而这块面的行数被
    /// [`MAX_ROWS`] 钉死（卡片高度 = 那套 `wl_region` / placement 证明的一部分，撑高就是改证明）。
    /// 折叠行"还有 N 件没显示"和"下一页"说的是同一件事 ⇒ 让它兼任控件，**零新字形**
    /// （`l10n::line_more` 四语早已在子集里，分数全 ASCII）。
    More,
}

impl PanelItem {
    /// 这一行点下去会不会发生任何事。[`press`] 是唯一的执行者。
    ///
    /// 它的形状是"**除只读行外全都能点**"而不是逐格列举 ⇒ 新增一格（如 [`Self::Wear`]、
    /// [`Self::More`]）**自动可点**，不必动这一条。反过来：以后要让某种新行**只读**，
    /// **必须**在这里加一支，而不是指望各调用点自己把关（[`press`] 那条注释立的同一道理）。
    pub const fn actionable(self) -> bool {
        !matches!(self, Self::Info)
    }
}

/// 收藏册的三个 tab = 菜单收藏页那三行**换成了同一个面上的三个视图**，而不是三个新命令：
/// 用户点开哪一行，面板就从哪一 tab 起（[`app`] 侧传初值），标题带再切。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionTab {
    Owned,
    Missing,
    Recent,
}

impl CollectionTab {
    pub const ALL: [Self; 3] = [Self::Owned, Self::Missing, Self::Recent];

    pub const fn index(self) -> usize {
        match self {
            Self::Owned => 0,
            Self::Missing => 1,
            Self::Recent => 2,
        }
    }

    /// 标题用的是**行名**（已获得 / 还缺 / 最近），不是页名「收藏」。理由和换装页那次同名：
    /// 一屏只有一个 tab 可见时，页名对读者没有信息量，而"已获得"就是这一屏在说什么。
    /// ⇒ 标题带的文字与菜单第五页那三行的标签**逐字相同**，零新字。
    pub const fn key(self) -> Key {
        match self {
            Self::Owned => Key::Owned,
            Self::Missing => Key::Missing,
            Self::Recent => Key::Recent,
        }
    }

    /// 顺序 = [`Self::ALL`]，与菜单第五页从上到下那三行同序 ⇒ "点标题带往下切"读起来
    /// 和"往下看第五页"是同一件事。
    pub const fn next(self) -> Self {
        match self {
            Self::Owned => Self::Missing,
            Self::Missing => Self::Recent,
            Self::Recent => Self::Owned,
        }
    }
}

/// 面板此刻是哪一面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Collection {
        tab: CollectionTab,
        /// 「已获得」那一本的**页码**（从 0 起）。另两个 tab 恒 0 —— 它们的行不是逐件产的，
        /// 装不满一屏，也就永远到不了页码控件那一格（见 [`album_page`]）。
        ///
        /// 它为什么在**这里**而不是在 `App` 的某个字段里：这一面"此刻该画哪几条"这件事的
        /// 全部输入就是 (哪一面, 哪一本, 第几页)，而 [`app`](crate::app) 的 `panel_rows`
        /// 是**唯一**的行生产者 ⇒ 另立一个 `self.album_page` 字段就是第二个事实源（换 tab
        /// 忘了归零 = 一屏空白，正是这条纪律反复抓的那族）。
        page: usize,
    },
    /// 设置只有一屏：五行（语言 / 动画 / 概率 / 复核 / 关闭）**刚好等于 [`MAX_ROWS`]**，
    /// 所以它没有 tab。但标题带**照样是控件**（[`PanelItem::Header`]）—— 这一面会被
    /// 「概率」/「复核」借去画五组内容行，那时唯一能回默认五行的入口就是标题带。
    Settings,
}

impl PanelKind {
    pub const fn tab(self) -> Option<CollectionTab> {
        match self {
            Self::Collection { tab, .. } => Some(tab),
            Self::Settings => None,
        }
    }

    /// 当前页码（非收藏册返回 0）。
    pub const fn page(self) -> usize {
        match self {
            Self::Collection { page, .. } => page,
            Self::Settings => 0,
        }
    }

    /// 切 tab = **换一本，所以页码归零**。不设这一步就是"从「已获得」第 4 页切到「还缺」
    /// 而那一本只有 1 页" ⇒ 请求一个不存在的页。
    ///
    /// ⚠️ **翻页不走这里** —— 页码只由 [`app`](crate::app) 在点到 [`PanelItem::More`] 时
    /// `+ 1` 得出，而**归一**（末页回卷）只在 [`album_page`] 一处做。一个页两件事分在两条
    /// 函数里，是为了让"归一"这件事没有第二个实现者。
    ///
    /// 设置页没有第二屏可切 ⇒ [`CollectionTab::next`] 那一步在这里返回自身；点它做什么由
    /// [`app`](crate::app) 的 `on_panel_press` 解释（重载默认行），不是"点了没反应"。
    pub const fn cycle(self) -> Self {
        match self {
            Self::Collection { tab, .. } => Self::Collection {
                tab: tab.next(),
                page: 0,
            },
            Self::Settings => Self::Settings,
        }
    }
}

/// 一行 = 它的身份 + 要画的那串字。**文本在这里就定稿**，`card` 只负责排版与截断，
/// 不再自己拼字（否则"签名比的是哪串字"与"画的是哪串字"会分家）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelRow {
    pub item: PanelItem,
    pub text: String,
}

/// 收藏册一屏的行。`lines` 由调用方给（`petstore` 那几个读库的产物，**必须是当前 tab 的**），
/// **本函数是那道几何约束的唯一执行者**：给多了不报错、不改卡片高度，而是截断并补一行「+N 更多」。
///
/// ⚠️ 这里**不读 tab**，是有意的而不是漏了：tab 只决定标题带那一串字（[`header_text`]），
/// 而行内容来自 `lines`。留一个"签名里有、函数体不读"的参数，等于给调用方一个
/// "传 tab=Owned 而 lines 是 Missing 那份"没人会发现的口子 —— 与本项目那条"进了签名的字段
/// 必须被下游读到"是同一条纪律的反面。
///
/// ⚠️ `line_more` 的 N 在这里是**被省略的行数**而不是条数：调用方给的每一行本身已经聚合过
/// （"一档一行"），所以屏上那句"+2 更多"可能读起来像"还有两件"。正解是调用方只给
/// `MAX_ROWS - 1` 行 ⇒ 这条分支是保险，不是主路径（真要精确报条数得让 `petstore` 那边
/// 也交行数以外的计数，那是另一件事，别在这儿顺手改）。
///
/// ⚠️ 它和 [`album_page`] 里那套折行算术**故意不同**，别"顺手统一"：这一条是**非分页**生产者
/// （「还缺」「最近」）的保险 —— 超出就折成一个尾标，反正没有第二屏可去；那一条是**分页**主路径
/// —— 超出就是下一页，所以它一行的字都不丢。同一个 `MAX_ROWS` 上面着两种语义。
pub fn collection_rows(lang: Lang, lines: &[String]) -> Vec<PanelRow> {
    let capacity = MAX_ROWS - 1;
    let mut rows: Vec<PanelRow> = if lines.len() <= capacity {
        lines
            .iter()
            .map(|l| PanelRow {
                item: PanelItem::Info,
                text: l.clone(),
            })
            .collect()
    } else {
        let kept = capacity - 1;
        let mut v: Vec<PanelRow> = lines[..kept]
            .iter()
            .map(|l| PanelRow {
                item: PanelItem::Info,
                text: l.clone(),
            })
            .collect();
        v.push(PanelRow {
            item: PanelItem::Info,
            text: l10n::line_more(lang, lines.len() - kept),
        });
        v
    };
    rows.push(PanelRow {
        item: PanelItem::Close,
        text: lang.t(Key::Close).to_string(),
    });
    rows
}

/// [`album_page`] 的产物：这一屏的行 + **归一后**真正落到的页码 + 总页数。
///
/// 为什么要带回 `page`：调用方（[`app`](crate::app) 的 `panel_rows`）交出去的是
/// "我想要第几页"，而屏上那句 `2/5` 必须是**实际**第几页。丢了这个返回值就只有两条路 ——
/// 要么在 `App` 里再归一一次（两个事实源），要么让请求越界时显示一个不存在的页号。
#[derive(Clone, Debug)]
pub struct AlbumPage {
    pub rows: Vec<PanelRow>,
    pub page: usize,
    pub pages: usize,
}

/// 逐件收藏册**切出一屏**：`entries` 是这一本的全部条目行（顺序即屏序，由调用方
/// [`app`](crate::app) 的 `album_rows` 定稿 —— 那里的每一格已经带着自己的身份
/// [`PanelItem::Wear`] 或 [`PanelItem::Info`]），这里只做**分页**这一件事。
///
/// 三处判断，都是"只在这里做一次"的那种：
/// 1. **页码归一**（`pages == 0 ⇒ 0`，否则 `page % pages`）⇒ 末页点页码控件 = 回卷到第一页，
///    而调用方可以无条件写 `kind.page() + 1`。别在 `App` 里再夹一次。
/// 2. **`hidden` = 这一屏之外还剩几条**（不是"下一页有几条"）⇒ 只要真的分了页它就 `> 0`，
///    屏上不会出现 `+0 更多`。
/// 3. **页码控件只在这一本超过一页时才占那一格** ⇒ 只有一屏时那一行会写成 `+0 更多 1/1`
///    而点它什么也不换 —— 那正是本项目 D14 判死过的"假按钮"。省掉它的代价是底部空一条
///    缝，而这条代价**实际不可达**：`entries` 尾部恒带那 5 行汇总（见 `Gacha::collection_lines`）
///    ⇒ `pages` 恒 `≥ 2`。
///
/// ⚠️ 它**不碰库、不读名字、不算宽度**：那些都在调用方。这条函数是纯切片，所以分页这件事
/// 能 headless 判定（"每条恰好出现一次"那种覆盖断言写在测试里）。
pub fn album_page(lang: Lang, entries: &[PanelRow], page: usize) -> AlbumPage {
    let per = ALBUM_PAGE_ROWS;
    let pages = entries.len().div_ceil(per);
    let page = if pages == 0 { 0 } else { page % pages };
    let start = page * per;
    let end = (start + per).min(entries.len());

    let mut rows: Vec<PanelRow> = entries[start..end].to_vec();
    if pages > 1 {
        let hidden = entries.len() - rows.len();
        rows.push(PanelRow {
            item: PanelItem::More,
            // `line_more` 四语早已在字体子集里（它就是"折叠尾标"那句话），分数全 ASCII
            // ⇒ 这一格零新字形。顺序 = 先"还有几条"再"第几页"：前者是这一行原本的语义，
            // 后者是它兼任的职责。
            text: format!("{} {}/{}", l10n::line_more(lang, hidden), page + 1, pages),
        });
    }
    rows.push(PanelRow {
        item: PanelItem::Close,
        text: lang.t(Key::Close).to_string(),
    });
    AlbumPage { rows, page, pages }
}

/// 设置一屏的五行。**每行的值 = 本进程此刻真的在用的那个值**（`lang` / `motion` 由调用方
/// 传进来，就是 `App` 里那两个字段），所以这一屏不会读出"屏上写 full 而卡片在按 off 动画"
/// 那种自相矛盾。
///
/// 三件事在这一屏上各不相同，别读成同一类：
/// - **语言 / 动画** = 会改状态并写回 `config.toml`（§9.2 第 19 条那个 writer）；
/// - **概率 / 复核** = 纯读，点它只换这一屏画什么（概率读编进二进制的 `rates.json`，
///   复核读账本链）；
/// - **关闭** = 收面。
///
/// 动画那一档在屏上打的是 `full|reduce|off` 这三个 ASCII 词而不是本地化标签：它们正是
/// `config.toml` 里 `[ui] motion` 接受的**字面值** ⇒ 用户写完文件回来对得上，而译名对不上。
pub fn settings_rows(lang: Lang, motion: Motion) -> Vec<PanelRow> {
    vec![
        PanelRow {
            item: PanelItem::Lang,
            text: l10n::lang_row(lang),
        },
        PanelRow {
            item: PanelItem::Motion,
            text: format!("{} {}", lang.t(Key::MotionTier), motion.code()),
        },
        PanelRow {
            item: PanelItem::Rates,
            text: lang.t(Key::Rates).to_string(),
        },
        PanelRow {
            item: PanelItem::Verify,
            text: lang.t(Key::Verify).to_string(),
        },
        PanelRow {
            item: PanelItem::Close,
            text: lang.t(Key::Close).to_string(),
        },
    ]
}

/// 标题带那一格。它是命中表里**最后一格**（下标 = `rows.len()`），与菜单 `rects()` 把
/// footer / header 追加在尾部同一条约定 ⇒ 行下标不漂。
///
/// 两面的标题带**都是 [`PanelItem::Header`]**（§5.3 的裁定）。几何与 kind 无关，所以这里
/// 留 `kind` 参数只有一个理由：调用方（[`card::paint_panel`] 与 [`rects`]）本来就手里有
/// `kind`，而它唯一还要读 kind 的地方是 [`header_text`] —— 拆成无参会让"这一格叫什么"
/// 与"这一格写什么字"分成两次 kind 匹配，正是本项目那条"两个事实源"的形状。
pub fn header_rect(_kind: PanelKind) -> RowRect {
    RowRect {
        item: PanelItem::Header,
        x: menu::PAD,
        y: menu::PAD,
        w: CARD_W - 2 * menu::PAD,
        h: menu::HEAD_H,
    }
}

pub fn layout(rows: &[PanelRow]) -> Vec<RowRect> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| RowRect {
            item: r.item,
            x: menu::PAD,
            y: ROWS_TOP + i as i32 * menu::ROW_STEP,
            w: CARD_W - 2 * menu::PAD,
            h: menu::ROW_H,
        })
        .collect()
}

/// 命中表 = 行带 ++ 标题带。
pub fn rects(kind: PanelKind, rows: &[PanelRow]) -> Vec<RowRect> {
    let mut v = layout(rows);
    v.push(header_rect(kind));
    v
}

pub fn hit_test(rects: &[RowRect], at: (f32, f32)) -> Option<PanelItem> {
    rects
        .iter()
        .find(|r| r.contains(at.0, at.1))
        .map(|r| r.item)
}

/// **唯一许可分派面板点击的入口**（[`menu::press`] 的同形物）。把关放在命中侧而不是分派侧，
/// 理由那条已经付过学费（2026-10-28 第五条缺陷：静态命中表 × 相位化画面 ⇒ "看着一张结果卡
/// 又能顺手抽掉十次"）：`Info` 这一支若留给每个调用点自己问"这一行能不能点"，迟早有人加
/// 一种行而忘了问。
///
/// 面板没有"忙"相位 ⇒ 这里只过滤 [`PanelItem::Info`]。将来若给面板加"读库中"之类的不确定
/// 状态，**要在这里加，不是在 `app` 的各分支里加**。
pub fn press(rects: &[RowRect], at: (f32, f32)) -> Option<PanelItem> {
    hit_test(rects, at).filter(|item| item.actionable())
}

/// 面板的内容签名。**只混"这一帧真的会变的量"**：是哪一面、哪个 tab、什么语言、错误行、
/// 每行此刻的字。⚠️ **没有 `elapsed`，也没有 tick 计数** —— 这是 `69fd158` 那条缺陷的形状
/// （签名混进画师不读的字段 ⇒ 停留期每 tick 重投一帧像素完全相同的 buffer）反着立起来写的一条：
/// 面板是"开着一动不动"的那类面，静止恒 **1 提交**是它唯一的省电判据。
///
/// 每行**文本**都进签名 ⇒ "进了签名的必须被下游读到"这条在这里天然成立（[`card`] 的画师
/// 读的就是同一份 `rows`）。**收藏册的页码同理**：它不进签名，是它那一行的**文本**
/// （`+16 更多 2/5`）进来的 —— 签名读的是画师读的东西。**`error` 同理但反方向**：它由 [`card::paint_panel`] 画进标题带
/// 右段，而它**不是任何一行** ⇒ 不混进签名就出现"错误行换了字而屏上不重画"，只能靠调用方
/// 在每个写 `gacha_error` 的地方补一句"记得重画"（[`app`] 的 `cycle_page` 那条注释判死过的
/// 形状）。菜单的 `content_sig` 早就混了同一个错误行，这里是把两面拉齐。
///
/// FNV-1a：不需要新依赖，且**只用来判等**，不当哈希表键、不进任何确定性契约。
pub fn content_sig(kind: PanelKind, lang: Lang, error: &str, rows: &[PanelRow]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let tag = match kind {
        PanelKind::Collection { .. } => 0u8,
        PanelKind::Settings => 1,
    };
    mix(tag);
    // tab 紧跟面别：切 tab 是这一面上唯一"用户能点着换"的内容维。
    if let Some(tab) = kind.tab() {
        mix(tab.index() as u8);
    }
    // ⚠️ **`page` 不进来**（`PanelKind::Collection { page }` 那个字段）—— 翻页改变的是
    // 这一屏的**行**（条目换了、页码控件那行的 `2/5` 换了），而那些本来就逐行混在下面。
    // 再混一次页码 = 一个画师不读的字段进了签名（同一个函数里 `Wear` 下标那条注释判死的形状），
    // 而且它和 `rows` 是**同一个事实的两种写法**，以后两者谁没更新都能被签名接受。
    //
    // 语言用 `code()` 的字节而不是枚举序号（同 `menu::content_sig` 的理由：新增一档语言
    // 不需要改这里，也不可能出现"两个不同枚举值混成同一个签名"）。
    for &b in lang.code().as_bytes() {
        mix(b);
    }
    mix(b'|');
    for &b in error.as_bytes() {
        mix(b);
    }
    mix(b'\n');
    for r in rows {
        // item 也要混：`Rates` 那一行点下去后文本可能恰好和上一帧同长同字（概率读不出来时
        // 是一句错误行），只比文本就分不开"同一串字、不同身份"。
        mix(match r.item {
            PanelItem::Info => 0,
            PanelItem::Header => 1,
            PanelItem::Lang => 2,
            PanelItem::Motion => 3,
            PanelItem::Rates => 4,
            PanelItem::Verify => 5,
            PanelItem::Close => 6,
            // ⚠️ **只混判别量，绝不混 `Wear` 的那个下标** —— 这不是省字，是一条行为裁定：
            // 画师读的是 [`PanelItem::actionable`]（可点行有强调条、标签缩进，只读行没有，
            // `card::paint_panel` 那一段），下标在这块面上**没有任何下游读者**。混进去就是
            // `69fd158` 那一族的正脸：签名变而像素不变 ⇒ 停留期每 tick 重投一帧完全相同的
            // buffer，而"静止恒 1 提交"是这一面唯一的省电判据。
            //
            // ⇒ **条件式，记在这里**：哪天要让"当前穿着的那一件"在下标上画得出区别（描一圈、
            // 换个标），必须**同时**把下标混进签名，否则那圈东西永远投不出帧。用户 2026-11-07
            // 拍板的是"选中了什么不上屏、只进日志"，所以今天这一条是**故意的**。
            PanelItem::Wear(_) => 7,
            PanelItem::More => 8,
            // 同一条纪律的第二次应用（P5）：**只混判别量，不混 `bodies` 的下标**。本体这一格
            // 比配件那一格更极端 —— 它连"当前选中是哪只"这个概念都不在上屏（当前是哪只就是宠物
            // 自己那几个像素，再打一行字就是第二个事实源），所以混下标**只会**造出"签名变而
            // 像素不变"的白投帧。以后要让"当前是哪只"上屏，正解是把它放进 `r.text`（文本本来就
            // 进签名，下面那三行），绝不是把它放进这个判别量。
            PanelItem::Body(_) => 9,
        });
        for b in r.text.as_bytes() {
            mix(*b);
        }
        mix(b'\n');
    }
    h
}

/// 面板标题带的那串字：收藏册 = `已获得 1/3`，设置 = `设置`。
/// 页码全 ASCII ⇒ 零字体成本（菜单分页那次同名结论）。
///
/// ⚠️ 那串分数报的是**第几本**（tab），不是第几页 —— 逐件那一本的页码在**页码控件那一行**
/// 上（[`album_page`] 里 `More` 的文本）。两处各报一个维度而标题带**故意不报页**：它高
/// [`menu::HEAD_H`]、排的是"这一面叫什么"，把页号塞进去要和 tab 分数抢同一段宽度，而
/// "还有多少条没显示"这个信息只有在条目行那一栏的宽度里才放得下 `+N 更多` 那半句。
///
/// ⚠️ 分母是 [`CollectionTab::ALL`] 的长度而不是 [`MAX_ROWS`] 那一类版面常量：标题要说的是
/// "一共三个视图"，与一屏能放几行无关。
pub fn header_text(kind: PanelKind, lang: Lang) -> String {
    match kind {
        PanelKind::Collection { tab, .. } => format!(
            "{} {}/{}",
            lang.t(tab.key()),
            tab.index() + 1,
            CollectionTab::ALL.len()
        ),
        PanelKind::Settings => lang.t(Key::PageSettings).to_string(),
    }
}

fn clamp(c: RectI, out: RectI) -> RectI {
    let x = c.x.clamp(out.x, (out.x + out.w - c.w).max(out.x));
    let y = c.y.clamp(out.y, (out.y + out.h - c.h).max(out.y));
    RectI::new(x, y, c.w, c.h)
}

/// 摆放：右 → 左 → 上 → 下四档逐个试，第一块**不与宠物相交**的就用；全相交才宁可短暂相交
/// 也不出屏（[`menu::placement`] 同一条优先级与同一条兜底，所以两块面在极端桌面上会各自
/// 贴边而不是互相躲）。
///
/// 这里**只避宠物**，不避菜单，因为 [`app`] 保证两块面永不同时在场（见模块头那条"为什么开
/// 面板要先把菜单关掉"）。
pub fn placement(pet: RectI, out: RectI) -> (i32, i32) {
    const GAP: i32 = 6;
    let cands = [
        // 右
        RectI::new(pet.x + pet.w + GAP, pet.y, CARD_W, CARD_H),
        // 左
        RectI::new(pet.x - GAP - CARD_W, pet.y, CARD_W, CARD_H),
        // 上
        RectI::new(pet.x, pet.y - GAP - CARD_H, CARD_W, CARD_H),
        // 下
        RectI::new(pet.x, pet.y + pet.h + GAP, CARD_W, CARD_H),
    ];
    for c in cands {
        let c = clamp(c, out);
        if !overlaps(c, pet) {
            return (c.x, c.y);
        }
    }
    let c = clamp(cands[0], out);
    (c.x, c.y)
}

pub fn overlaps(a: RectI, b: RectI) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("N item{i}")).collect()
    }

    /// `menu::RectI` 故意只有 `new`（它不需要别的），所以"完全落在里面"这个谓词
    /// 在测试侧就地写一份 —— 生产代码里没有任何地方读它，把它加进 `RectI` 就是
    /// 给一个没人用的方法。
    fn inside(inner: RectI, outer: RectI) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.w <= outer.x + outer.w
            && inner.y + inner.h <= outer.y + outer.h
    }

    /// 一屏最多 [`MAX_ROWS`] 行，而**最后一行永远是「关闭」**。给 0 行、给满、给超都成立
    /// （超了截断而不是把关闭挤出去：关闭没了 = 这块面再也关不掉）。
    #[test]
    fn rows_are_capped_and_close_is_always_last() {
        for lang in Lang::ALL {
            // 一屏的行与 tab **无关**（tab 只换标题带那串字，见 [`collection_rows`] 的注释），
            // 所以这里遍历的是 `lang × 行数`，而不是把 tab 也套一层循环假装它会变。
            for n in [0usize, 1, 4, 5, 6, 20] {
                let rows = collection_rows(lang, &lines(n));
                assert!(
                    rows.len() <= MAX_ROWS,
                    "{lang:?} 给 {n} 行 ⇒ 产 {} 行 > {MAX_ROWS}",
                    rows.len()
                );
                assert_eq!(
                    rows.last().map(|r| r.item),
                    Some(PanelItem::Close),
                    "给 {n} 行 ⇒ 最后一行不是「关闭」"
                );
                assert!(!rows.iter().any(|r| r.text.is_empty()), "有空行");
            }
            let s = settings_rows(lang, Motion::Full);
            // 设置页是**五行**（语言 / 动画 / 概率 / 复核 / 关闭），而卡片高度按收藏册给满
            // 那一档的六行定 ⇒ 底下空一条行带是设计使然：卡片尺寸是固定几何（`placement`
            // 与 `wl_region` 那两套证明都按它写），为"填满"而凑一行就是假按钮的形状。
            // 写成 `==` 而不是 `<=`：以后谁删掉设置页的一行，这条要红。
            assert_eq!(s.len(), MAX_ROWS - 1, "设置页应当恰好五行");
            assert_eq!(s.last().unwrap().item, PanelItem::Close);
        }
    }

    /// 溢出那一行的 N = **被省略的行数**，而卡片高度一个字不变。这条断言是给
    /// 「正解在调用方那边（只给 5 行）」留的锚，不是给"+N 更多"的语义背书。
    #[test]
    fn overflow_truncates_to_max_rows_and_names_the_omitted_lines() {
        let rows = collection_rows(Lang::Zh, &lines(20));
        assert_eq!(rows.len(), MAX_ROWS);
        let more = &rows[MAX_ROWS - 2];
        assert_eq!(more.item, PanelItem::Info);
        assert_eq!(more.text, l10n::line_more(Lang::Zh, 20 - (MAX_ROWS - 2)));
    }

    /// 逐件那一本的条目行（每条各占一格，下标 = 它自己的序号，当作 `candidates` 下标用）。
    fn album_entries(n: usize) -> Vec<PanelRow> {
        (0..n)
            .map(|i| PanelRow {
                item: PanelItem::Wear(i),
                text: format!("R item{i}"),
            })
            .collect()
    }

    /// **分页的覆盖不变量**：每条条目在全部页里**恰好出现一次**，一屏不超过 [`MAX_ROWS`] 行，
    /// 最后一行永远是「关闭」，而只要真分了页，页码控件那一格就在（每屏至多一个）。
    ///
    /// 抄 [`rows_are_capped_and_close_is_always_last`]（同一族"版面产行的硬约束"），但它查的是
    /// **跨页**那一维 —— 逐件之后这是这块面唯一的新风险：切错一刀就是某件衣服永远点不到，
    /// 而"永远点不到"在屏上读起来和"这一件本来就不能穿"一模一样。
    #[test]
    fn the_album_pages_cover_every_entry_exactly_once() {
        for lang in Lang::ALL {
            for n in [0usize, 1, 4, 5, 9, 20, 21, 22] {
                let entries = album_entries(n);
                let pages = n.div_ceil(ALBUM_PAGE_ROWS);
                let mut seen = vec![0usize; n];
                for p in 0..pages.max(1) {
                    let screen = album_page(lang, &entries, p);
                    assert_eq!(screen.pages, pages, "{lang:?} 给 {n} 条 ⇒ 页数报错");
                    assert_eq!(screen.page, p % pages.max(1), "请求的页没被如实带回");
                    assert!(
                        screen.rows.len() <= MAX_ROWS,
                        "{lang:?} 给 {n} 条第 {p} 页 ⇒ {} 行 > {MAX_ROWS}",
                        screen.rows.len()
                    );
                    assert_eq!(
                        screen.rows.last().map(|r| r.item),
                        Some(PanelItem::Close),
                        "给 {n} 条 ⇒ 最后一行不是「关闭」"
                    );
                    let mut control = 0;
                    for r in &screen.rows {
                        match r.item {
                            PanelItem::Wear(i) => seen[i] += 1,
                            PanelItem::More => control += 1,
                            PanelItem::Close => {}
                            other => panic!("逐件那一屏不该出现 {other:?}"),
                        }
                    }
                    assert!(control <= 1, "一屏两个页码控件");
                    if pages > 1 {
                        assert_eq!(control, 1, "分了页而第 {p} 页没有页码控件 ⇒ 翻不过去");
                    }
                }
                assert!(
                    seen.iter().all(|&c| c == 1),
                    "{lang:?} 给 {n} 条 ⇒ 各页出现次数 {seen:?}"
                );
            }
        }
    }

    /// 页码控件那一行的**字**：`+N 更多 P/T` —— N = 这一屏之外还剩几条（所以真分页时它
    /// 恒 `> 0`，屏上不会出现 `+0 更多`），P/T = 实际页号 / 总页数；而**末页再点 = 回卷**
    /// （[`album_page`] 是唯一做归一的地方）。
    ///
    /// 抄 [`the_dwell_never_bumps_the_signature_but_a_tab_or_value_switch_does`] 里"随维变"
    /// 那一半的写法：把可变的那一维逐值遍历，再断言文本 == 该值对应的那串字。
    #[test]
    fn the_page_control_row_names_its_page_and_wraps() {
        let entries = album_entries(10); // 4 + 4 + 2 ⇒ 三页
        let total = album_page(Lang::Zh, &entries, 0).pages;
        assert_eq!(total, 3);
        for p in 0..total {
            let screen = album_page(Lang::Zh, &entries, p);
            let ctrl = screen
                .rows
                .iter()
                .find(|r| r.item == PanelItem::More)
                .expect("分了页却找不到页码控件");
            let frac = ctrl.text.rsplit(' ').next().unwrap();
            assert_eq!(frac, format!("{}/{}", p + 1, total), "分数报的不是当前页");
            let shown = screen.rows.len() - 2; // 减掉控件与「关闭」自己
            let hidden = entries.len() - shown;
            assert!(hidden > 0, "第 {p} 页已经没有隐藏条目却还挂着控件");
            assert!(
                ctrl.text.starts_with(&l10n::line_more(Lang::Zh, hidden)),
                "控件前半句没把「还剩几条」说对：{:?}",
                ctrl.text
            );
        }
        // 末页 +1 ⇒ 回卷到第一页（调用方因此可以无条件写 `page() + 1`）。
        assert_eq!(album_page(Lang::Zh, &entries, total).page, 0);
        assert_eq!(album_page(Lang::Zh, &entries, 0).page, 0);
        // 空库 ⇒ 第 0 页、零页、只有「关闭」—— 不能因为 `page % 0` 炸在这里。
        let empty = album_page(Lang::Zh, &[], 7);
        assert_eq!((empty.page, empty.pages), (0, 0));
        assert_eq!(empty.rows.len(), 1);
    }

    /// **"选中了什么不上屏"这条决定的技术形态**（用户 2026-11-07 拍板：只进日志）：
    /// [`PanelItem::Wear`] 的**下标**不许进签名，而**身份**必须进。
    ///
    /// 抄 [`an_info_row_is_hit_but_never_activates_while_the_row_below_does`] 那条"逐格列举"
    /// 的写法，但它断言的是签名的两个方向：
    /// - 同文本、不同下标 ⇒ **签名相同**（否则停留期每 tick 重投一帧逐位相同的 buffer，
    ///   而"静止恒 1 提交"是这块面唯一的省电判据 = `69fd158`）；
    /// - 同文本、一格 `Wear` 一格 `Info` ⇒ **签名不同**（可点行有强调条，画师真读得到，
    ///   漏了它屏上就不换）。
    ///
    /// ⚠️ 哪天要把"当前穿着的那一件"画出来（描一圈、加个标），**必须同时**把下标混进
    /// [`content_sig`] —— 否则那圈东西投不出帧。这条测试就是那件事的提醒牌。
    #[test]
    fn the_wear_index_stays_out_of_the_signature_while_the_identity_does_not() {
        let kind = PanelKind::Collection {
            tab: CollectionTab::Owned,
            page: 0,
        };
        let row = |item: PanelItem| {
            Vec::from([PanelRow {
                item,
                text: "R hat".to_string(),
            }])
        };
        let sig = |rows: &[PanelRow]| content_sig(kind, Lang::Zh, "", rows);
        assert_eq!(
            sig(&row(PanelItem::Wear(1))),
            sig(&row(PanelItem::Wear(2))),
            "下标进了签名 ⇒ 画师不读它，却每 tick 重投同样的像素"
        );
        assert_ne!(
            sig(&row(PanelItem::Wear(1))),
            sig(&row(PanelItem::Info)),
            "身份没进签名 ⇒ 只读行和可点行在屏上分不开"
        );
        assert_ne!(
            sig(&row(PanelItem::More)),
            sig(&row(PanelItem::Close)),
            "身份没进签名 ⇒ 页码控件和「关闭」在屏上分不开"
        );
    }

    /// 同上一条的**第二个下标空间**（P5 的 `Body`），四条读数一条不能少。
    ///
    /// 抄 [`the_wear_index_stays_out_of_the_signature_while_the_identity_does_not`] 的写法，因为
    /// 那一条只举了 `Wear` 一例，而 `Body` 是后来加的那一格 —— 变异 M7（把 `Body` 的下标混进
    /// 判别量）**实测让上面那条照旧全绿**：两格是不同的判别量，只测一格等于给另一格留了空档。
    /// 这一族的判据是"**每一个带下标的臂都要有自己的读数**"，不是"有一条签名测试就覆盖了签名"。
    #[test]
    fn the_body_index_stays_out_of_the_signature_while_the_identity_does_not() {
        let kind = PanelKind::Collection {
            tab: CollectionTab::Owned,
            page: 0,
        };
        let row = |item: PanelItem| {
            Vec::from([PanelRow {
                item,
                text: "N cat".to_string(),
            }])
        };
        let sig = |rows: &[PanelRow]| content_sig(kind, Lang::Zh, "", rows);
        assert_eq!(
            sig(&row(PanelItem::Body(1))),
            sig(&row(PanelItem::Body(2))),
            "本体下标进了签名 ⇒ 画师不读它，却每 tick 重投逐位相同的 buffer\
             （「静止恒 1 提交」是这块面唯一的省电判据）"
        );
        assert_ne!(
            sig(&row(PanelItem::Body(1))),
            sig(&row(PanelItem::Info)),
            "本体身份没进签名 ⇒ 可点的宠物行与只读行在屏上分不开"
        );
        assert_ne!(
            sig(&row(PanelItem::Body(1))),
            sig(&row(PanelItem::Wear(1))),
            "两个下标空间共用一个判别量 ⇒ 「这个数字指哪张表」又只剩注释在说，而同一条数字\
             在两套表里是两件不同的事"
        );
    }

    /// 行带两两不相交、全部落在卡内，**而带与带之间那 4 px 缝点不着任何东西**（"缝隙不命中"
    /// 在菜单那道证明里的同一条性质）。正对照 = 每条带中心点得着自己那一行，否则
    /// "整面都点不着"也能让这条绿。
    #[test]
    fn row_bands_are_disjoint_inside_the_card_and_the_seam_hits_nothing() {
        for kind in [
            PanelKind::Collection {
                tab: CollectionTab::Owned,
                page: 0,
            },
            PanelKind::Settings,
        ] {
            let rows = match kind {
                PanelKind::Collection { .. } => collection_rows(Lang::Zh, &lines(5)),
                PanelKind::Settings => settings_rows(Lang::Zh, Motion::Full),
            };
            let rs = rects(kind, &rows);
            let card = RectI::new(0, 0, CARD_W, CARD_H);
            for (i, r) in rs.iter().enumerate() {
                let rr = r.as_rect();
                assert!(
                    rr.x >= card.x
                        && rr.y >= card.y
                        && rr.x + rr.w <= card.x + card.w
                        && rr.y + rr.h <= card.y + card.h,
                    "第 {i} 格 {rr:?} 出了卡片 {card:?}"
                );
                for (j, o) in rs.iter().enumerate() {
                    if j != i {
                        assert!(!overlaps(rr, o.as_rect()), "第 {i} 格与第 {j} 格相交");
                    }
                }
                // 正对照：带中心必须命中自己。
                assert_eq!(
                    hit_test(
                        &rs,
                        (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0)
                    ),
                    Some(r.item),
                    "第 {i} 格的中心点不到自己"
                );
            }
            // 相邻两行的缝：**只量行带之间**。标题带是追加在命中表**尾部**的（向量序 ≠ 空间序），
            // 把它套进 `rs.windows(2)` 量出来的"最后一行 → 标题带"不是任何物理相邻关系。
            for pair in layout(&rows).windows(2) {
                let below = pair[0].y + pair[0].h;
                let seam = (below + pair[1].y) / 2;
                assert!(
                    below < pair[1].y && seam > below && seam < pair[1].y,
                    "行带之间没有缝（{}→{}，中点 {seam}）",
                    below,
                    pair[1].y
                );
                assert!(
                    hit_test(
                        &rs,
                        ((pair[0].x + pair[0].w / 2) as f32, (below + 1) as f32)
                    )
                    .is_none(),
                    "行缝被算进了命中区"
                );
            }
        }
    }

    /// 标题带：**收藏册切 tab，设置页切不动 tab 但照样点得着**（§5.3：那一格是
    /// 「重载这一面默认五行」的入口，`Rates`/`Verify` 借走这一面时它是唯一的回程）。
    /// 所以这里钉的是两件不同的事：**命中**两面都真，**换面**只有收藏册会换。
    #[test]
    fn both_headers_are_hit_while_only_the_collection_one_changes_face() {
        let tab_kind = PanelKind::Collection {
            tab: CollectionTab::Owned,
            page: 2,
        };
        let rows = collection_rows(Lang::Zh, &lines(3));
        let rs = rects(tab_kind, &rows);
        let h = header_rect(tab_kind);
        let center = (h.x as f32 + h.w as f32 / 2.0, h.y as f32 + h.h as f32 / 2.0);
        assert_eq!(press(&rs, center), Some(PanelItem::Header));
        assert_eq!(
            tab_kind.cycle(),
            PanelKind::Collection {
                tab: CollectionTab::Missing,
                // 从 `page: 2` 起步而这里要 0 ⇒ 这条顺带钉住"**切 tab 即归零页码**"：
                // 换一本之后还停在第 3 页，屏上就是一屏空白（另两本只有一屏）。
                page: 0
            }
        );
        // 切满一圈回**同一本**。⚠️ 但回不到原来那一**页**（起点 `page: 2`，一圈回来是 `page: 0`），
        // 而这正是这里要的读数：归零发生在**每一下** `cycle`，不是只在"换到另一本"那一刻 ——
        // 写成 `k == tab_kind` 就是把"页码也跟着转一圈"当成正确行为钉住了。
        let mut k = tab_kind;
        for _ in 0..CollectionTab::ALL.len() {
            k = k.cycle();
        }
        assert_eq!(
            k,
            PanelKind::Collection {
                tab: CollectionTab::Owned,
                page: 0
            }
        );
        assert_eq!(PanelKind::Settings.cycle(), PanelKind::Settings);

        // 设置页的标题带：`item` 是 `Header` ⇒ `press` 放行，而 `cycle()` 返回自身。
        // 这两条同时成立才是"点它 = 重载默认行"（改成 Info 就退化成上一版的死装饰）。
        let s_rows = settings_rows(Lang::Zh, Motion::Full);
        let s_rs = rects(PanelKind::Settings, &s_rows);
        let sh = header_rect(PanelKind::Settings);
        assert_eq!(sh.item, PanelItem::Header);
        assert_eq!(
            press(&s_rs, (sh.x as f32 + 1.0, sh.y as f32 + 1.0)),
            Some(PanelItem::Header),
            "设置页的标题带点不动 ⇒ 「概率」/「复核」借走这一面后没有回程"
        );
        assert_eq!(PanelKind::Settings.cycle(), PanelKind::Settings);
        // 两面几何相同（同一格、同一个 item）⇒ 命中表里"标题带在最后一格"这条不随面别漂。
        assert_eq!(sh.as_rect(), h.as_rect());
    }

    /// **内容行在命中表里但 `press` 永不激活**，正对照 = 紧挨它下面那一行「关闭」点得到。
    /// 少了这个正对照，把 `press` 写成"永远返回 None"也能让前半条绿。
    #[test]
    fn an_info_row_is_hit_but_never_activates_while_the_row_below_does() {
        let rows = collection_rows(Lang::Zh, &lines(5));
        let rs = rects(
            PanelKind::Collection {
                tab: CollectionTab::Owned,
                page: 0,
            },
            &rows,
        );
        let content = rs[0];
        let close = rs[MAX_ROWS - 1];
        assert_eq!(content.item, PanelItem::Info);
        assert_eq!(close.item, PanelItem::Close);
        assert!(
            hit_test(&rs, (content.x as f32 + 1.0, content.y as f32 + 1.0)).is_some(),
            "内容行不在命中表里 ⇒ 排版索引会漂"
        );
        assert_eq!(
            press(&rs, (content.x as f32 + 1.0, content.y as f32 + 1.0)),
            None
        );
        assert_eq!(
            press(&rs, (close.x as f32 + 1.0, close.y as f32 + 1.0)),
            Some(PanelItem::Close)
        );
        for item in [
            PanelItem::Header,
            PanelItem::Lang,
            PanelItem::Motion,
            PanelItem::Rates,
            PanelItem::Verify,
            PanelItem::Close,
            // 逐件收藏册那两格（P4）⇒ 它们**必须**在这一列里：这一条断言就是"`actionable()`
            // 是除 `Info` 外全点得着"那个形状的唯一守门人，漏了它就得靠各调用点自己问。
            PanelItem::Wear(0),
            PanelItem::More,
        ] {
            assert!(item.actionable(), "{item:?} 被当成只读行了");
        }
        assert!(!PanelItem::Info.actionable());
        // 正对照：把一件条目真放进一屏，命中它 ⇒ `press` 交回**同一个下标**的那一格
        // （载荷在命中表里活着 = P4 那条"点一件穿上"唯一的输入）。
        let album = vec![
            PanelRow {
                item: PanelItem::Wear(3),
                text: "R hat".into(),
            },
            PanelRow {
                item: PanelItem::More,
                text: l10n::line_more(Lang::Zh, 1),
            },
            PanelRow {
                item: PanelItem::Close,
                text: Lang::Zh.t(Key::Close).into(),
            },
        ];
        let ar = rects(
            PanelKind::Collection {
                tab: CollectionTab::Owned,
                page: 0,
            },
            &album,
        );
        assert_eq!(press(&ar, center(ar[0])), Some(PanelItem::Wear(3)));
        assert_eq!(press(&ar, center(ar[1])), Some(PanelItem::More));
    }

    /// 一条带子的中心点（"点得着自己"的最小充分坐标；边角 +1 那种写法在缝宽变更小的时候
    /// 会退化成测缝）。
    fn center(r: RowRect) -> (f32, f32) {
        (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0)
    }

    /// 设置页五行点下去**各自是各自的那一件**（不是全落到语言、也不是全落空）。
    #[test]
    fn every_settings_row_dispatches_to_its_own_action() {
        let rows = settings_rows(Lang::Zh, Motion::Reduce);
        let rs = rects(PanelKind::Settings, &rows);
        let want = [
            PanelItem::Lang,
            PanelItem::Motion,
            PanelItem::Rates,
            PanelItem::Verify,
            PanelItem::Close,
        ];
        for (i, w) in want.iter().enumerate() {
            let r = rs[i];
            assert_eq!(
                press(&rs, (r.x as f32 + 1.0, r.y as f32 + 1.0)),
                Some(*w),
                "第 {i} 行不是 {w:?}"
            );
        }
        assert_eq!(
            rows[1].text,
            format!("{} reduce", Lang::Zh.t(Key::MotionTier))
        );
    }

    /// **停留期一个字都不许进签名**（`69fd158` 的镜像形状）：同一面、同一 tab、同一语言、
    /// 同一份行 ⇒ 签名逐位相同，而签名变了的两组必须真的不同。**错误行也在这一条里**：
    /// 它画在标题带右段却不属于任何一行，漏掉它就是"库炸了而屏上还挂着旧的五行"。
    #[test]
    fn the_dwell_never_bumps_the_signature_but_a_tab_or_value_switch_does() {
        for lang in Lang::ALL {
            let a = collection_rows(lang, &lines(5));
            let b = collection_rows(lang, &lines(5));
            assert_eq!(
                content_sig(
                    PanelKind::Collection {
                        tab: CollectionTab::Owned,
                        page: 0
                    },
                    lang,
                    "",
                    &a
                ),
                content_sig(
                    PanelKind::Collection {
                        tab: CollectionTab::Owned,
                        page: 0
                    },
                    lang,
                    "",
                    &b
                ),
                "同样的行算了两个签名 ⇒ 停留期会每 tick 重投"
            );
            let c = collection_rows(lang, &lines(5));
            // ⚠️ `a` 与 `c` 是**同一份行**（行内容不由 tab 决定）⇒ 这两支签名唯一的差
            // 就是 tab 那一格，正好把"tab 进了签名"这件事单独量出来。
            assert_ne!(
                content_sig(
                    PanelKind::Collection {
                        tab: CollectionTab::Owned,
                        page: 0
                    },
                    lang,
                    "",
                    &a
                ),
                content_sig(
                    PanelKind::Collection {
                        tab: CollectionTab::Missing,
                        page: 0
                    },
                    lang,
                    "",
                    &c
                ),
                "切 tab 没换签名 ⇒ 屏上还画旧 tab"
            );
        }
        // 语言、动画值、行身份各自都要能撞开签名。
        let zh = settings_rows(Lang::Zh, Motion::Full);
        let en = settings_rows(Lang::En, Motion::Full);
        let off = settings_rows(Lang::Zh, Motion::Off);
        let sig = |rows: &[PanelRow], lang: Lang| content_sig(PanelKind::Settings, lang, "", rows);
        assert_ne!(sig(&zh, Lang::Zh), sig(&en, Lang::En), "换语言没换签名");
        assert_ne!(sig(&zh, Lang::Zh), sig(&off, Lang::Zh), "换动画档没换签名");
        // 错误行：行一个字没变，只有错误串变了 ⇒ 这两支签名必须分开。
        assert_ne!(
            content_sig(PanelKind::Settings, Lang::Zh, "", &zh),
            content_sig(PanelKind::Settings, Lang::Zh, "打不开存档", &zh),
            "错误行进不了签名 ⇒ 面板会挂着旧错误（或反过来，新错误永远不上屏）"
        );
        assert_ne!(
            sig(&zh, Lang::Zh),
            content_sig(
                PanelKind::Collection {
                    tab: CollectionTab::Owned,
                    page: 0
                },
                Lang::Zh,
                "",
                &collection_rows(Lang::Zh, &[])
            ),
            "面和面之间不签名"
        );
        // `item` 混进签名那条必须有证据：同文本、不同身份 ⇒ 不同签名。
        let same_text_diff_item = [
            PanelRow {
                item: PanelItem::Rates,
                text: "X".to_string(),
            },
            PanelRow {
                item: PanelItem::Verify,
                text: "X".to_string(),
            },
        ];
        // 正对照 = 这两格的文本**必须真的相同**，否则下面那条比的不是身份而是字数。
        assert_eq!(
            same_text_diff_item[0].text, same_text_diff_item[1].text,
            "对照样本自己就不满足「同文本」⇒ 下面那条红得没有意义"
        );
        assert_ne!(
            same_text_diff_item[0].item, same_text_diff_item[1].item,
            "对照样本自己就不满足「不同身份」"
        );
        assert_ne!(
            content_sig(PanelKind::Settings, Lang::Zh, "", &same_text_diff_item[..1]),
            content_sig(PanelKind::Settings, Lang::Zh, "", &same_text_diff_item[1..]),
            "同文本不同身份混成了同一个签名"
        );
    }

    /// 面板的每一串字都必须画得出来（**零新字是这一批的设计前提**，见 `daily/2026-11-03`
    /// 那道字号竞技场算术：32 px 档余量只有 ≈17 字）。有人绕过 `l10n` 硬写一句文案，
    /// 红的就是这里。
    #[test]
    fn the_panel_copy_needs_no_glyphs_outside_the_subset() {
        let mut checked = 0usize;
        for lang in Lang::ALL {
            for n in [0usize, 3, 5, 9] {
                // 行内容与 tab 无关（[`collection_rows`] 不读它）⇒ 行只算一次、查一遍。
                let rows = collection_rows(lang, &lines(n));
                for r in rows.iter().map(|r| &r.text) {
                    assert_eq!(crate::text::missing(r), None, "面板行画不出：{r:?}");
                    checked += 1;
                }
                // 随 tab 变的是**标题带**那一串字 ⇒ 三档逐个查，这才是这条的覆盖面所在。
                for tab in CollectionTab::ALL {
                    let h = header_text(PanelKind::Collection { tab, page: 0 }, lang);
                    assert_eq!(crate::text::missing(&h), None, "标题带画不出：{h:?}");
                    checked += 1;
                }
            }
            for m in Motion::ALL {
                for r in settings_rows(lang, m) {
                    assert_eq!(
                        crate::text::missing(&r.text),
                        None,
                        "设置行画不出：{:?}",
                        r.text
                    );
                    checked += 1;
                }
            }
            assert_eq!(
                crate::text::missing(&header_text(PanelKind::Settings, lang)),
                None
            );
            checked += 1;
        }
        assert!(checked > 100, "样本太少，这条等于没跑");
    }

    /// 页码那串是纯 ASCII ⇒ 字体预算零成本（菜单分页那次的同一条结论换到面板）。
    #[test]
    fn the_page_indicator_is_ascii_only() {
        for lang in Lang::ALL {
            for tab in CollectionTab::ALL {
                let h = header_text(PanelKind::Collection { tab, page: 0 }, lang);
                let tail = h.rsplit(' ').next().unwrap();
                assert!(
                    tail.chars().all(|c| c.is_ascii_digit() || c == '/'),
                    "{lang:?} {tab:?} 的页码不是纯 ASCII：{tail:?}"
                );
                assert_eq!(
                    tail,
                    format!("{}/{}", tab.index() + 1, CollectionTab::ALL.len()),
                    "{lang:?} {tab:?} 的标题带说了别的事 ⇒ 那句分数说的是**哪一本**，不是第几页"
                );
            }
            // ⚠️ 标题带那一处**只报 tab**（上面那句），而"第几页"这件事本轮真的有了 ⇒ 它落在
            // **页码控件那一行的文本**里。这个测试的名字原本只指标题带，现在得把那一格也管进来：
            // 两处分数的形状必须一样是纯 ASCII `P/T`，否则字体门禁（四常数联动）会在"没人改
            // `l10n.rs`"的那一轮静默变成"屏上一个 `?`"。
            let entries = album_entries(10); // 三页，页码控件一定在
            for p in 0..3 {
                let screen = album_page(lang, &entries, p);
                let ctrl = screen
                    .rows
                    .iter()
                    .find(|r| r.item == PanelItem::More)
                    .expect("分了页而控件不在");
                let frac = ctrl.text.rsplit(' ').next().unwrap();
                assert!(
                    frac.chars().all(|c| c.is_ascii_digit() || c == '/'),
                    "{lang:?} 页码控件的分数不是纯 ASCII：{frac:?}"
                );
                assert_eq!(frac, format!("{}/{}", p + 1, screen.pages));
            }
        }
    }

    /// 摆放：四档优先级、每档都在可用区内、且**不与宠物相交**（可用区够大的时候）。
    /// 卡片从 150 高变 214 高 ⇒ 这条是新的几何证明，不能引用菜单那份。
    #[test]
    fn placement_prefers_the_right_side_and_never_leaves_the_available_area() {
        let out = RectI::new(0, 0, 1920, 1080);
        let pet = RectI::new(800, 500, 224, 224);
        let (x, y) = placement(pet, out);
        assert_eq!((x, y), (pet.x + pet.w + 6, pet.y), "右侧够用时没走右侧");
        // 右边放不下 ⇒ 换左；左也放不下 ⇒ 上 ⇒ 下；极端到四面都不行 ⇒ 仍然必须在区内。
        let narrow = RectI::new(0, 0, 240, 214);
        let (nx, ny) = placement(RectI::new(0, 0, 240, 214), narrow);
        assert_eq!((nx, ny), (0, 0), "可用区只够一块面时应该贴原点");
        assert!(
            inside(RectI::new(nx, ny, CARD_W, CARD_H), narrow),
            "卡片被放到可用区外了"
        );
        for (px, py, pw, ph) in [
            (10, 10, 224, 224),
            (1680, 900, 224, 224),
            (1680, 10, 224, 224),
            (10, 900, 224, 224),
            (900, 500, 1900, 700),
        ] {
            let p = RectI::new(px, py, pw, ph);
            let (x, y) = placement(p, out);
            let card = RectI::new(x, y, CARD_W, CARD_H);
            assert!(inside(card, out), "宠物 {p:?} ⇒ 面板 {card:?} 出了 {out:?}");
            assert!(!overlaps(card, p), "宠物 {p:?} ⇒ 面板 {card:?} 与它相交");
        }
    }

    /// 卡片几何：高度由 `MAX_ROWS` 推出来，不是手抄的 `214`。宽度**必须等于菜单**，
    /// 否则那条 `MAX_LINE_PX` / `fit` 的排版预算要重新证一遍。
    #[test]
    fn card_geometry_is_derived_and_the_width_matches_the_menu() {
        assert_eq!(CARD_W, menu::CARD_W);
        assert_eq!(CARD_H, 214);
        assert_eq!(ROWS_TOP, menu::PAD + menu::HEAD_H);
        let rows = collection_rows(Lang::Zh, &lines(MAX_ROWS - 1));
        let rs = layout(&rows);
        assert_eq!(
            rs.last().unwrap().y + rs.last().unwrap().h + menu::PAD,
            CARD_H,
            "最后一行下面不是正好一个 `PAD` ⇒ 上下留白不对称"
        );
        assert_eq!(
            rs[1].y - (rs[0].y + rs[0].h),
            menu::ROW_STEP - menu::ROW_H,
            "行缝不对"
        );
    }
}
