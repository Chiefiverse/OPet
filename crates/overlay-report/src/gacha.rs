//! 抽卡那一层的读数生产者：连库、抽取、四本册子与复核的行。
//!
//! 2026-11-24（M7-b 前置）整段从 `overlay-wayland/src/app.rs` 搬上来，**函数体一字未改**。
//! 搬的理由与 `overlay-common/src/ui/mod.rs` 顶上那段同族：Windows 的面板（M7）要上第二块屏，
//! 而"屏上是这一串字、stderr 打到的是另一串"这件事只许有一份实现。第二批时 Windows 侧
//! `overlay-win32/src/dress.rs` 里的 `Gacha` 是照着这边手写对齐的（它自己的注释写着"与 Wayland
//! `Gacha::pull` 同一份语义，逐条对齐"）—— 那种对齐是人做的事，本轮把它换成编译器做的事。
//!
//! 这一层**不含任何平台类型**（`menu.rs`/`card.rs` 那族约束的延伸）：好处不是整洁，而是
//! "落库这条路"能在 headless 里被打红 —— 抽取的入口是鼠标，而我绝不注入点击。
//!
//! 宽度纪律全在这里立：`missing_lines` / `verify_lines` / `album_rows` 都问
//! [`overlay_common::menu::fit_to`] 本体，不许拿 `line_width` 自己算（生产者与排版各有各的预算
//! 是这一族唯一的病根）。`scale` 因此是这些函数的**入参**：生产行的那一刻就知道自己要上哪块屏。

use overlay_common::l10n::{self, Key, Lang};
use overlay_common::{menu, panel};
use petcore::gacha::{Catalog, Currency, GachaParams, ItemKind, Owned, Rarity};
use petstore::Store;

use crate::album::{AlbumEntry, AlbumTarget};
use overlay_common::panel::CollectionTab;

/// 库与目录成对出现（`connect` 一起建、一起失败），所以没有"有库没目录"的中间态。
///
/// 这一层**不含任何 wayland 类型**（和 `menu.rs`/`card.rs` 同族约束）：好处不是整洁，
/// 而是"落库这条路"能在 headless 里被测 —— 抽取的入口是鼠标，而我绝不注入点击。
pub struct Gacha {
    pub store: Store,
    pub catalog: Catalog,
}

impl Gacha {
    /// 连库 + 装目录。`None` = 内存库（一条文件都不产生，UI 全程打 DRYRUN 标）。
    ///
    /// 不做 canonicalize：它要求文件已存在，第一次运行必然失败，反而把"能建库"这条正路
    /// 堵死。绝对路径的要求由 `Store::open` 自己把关（相对路径取决于 cwd，而宠物可能被
    /// systemd user service 起），口径与 CLI 一致。
    pub fn connect(db: Option<&str>) -> Result<Self, String> {
        let mut store = match db {
            None => Store::in_memory().map_err(|e| format!("内存库建不起来: {e}"))?,
            Some(p) => {
                Store::open(std::path::Path::new(p)).map_err(|e| format!("打不开存档 {p}: {e}"))?
            }
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

    /// 抽 `count` 次并落库，返回菜单要的三样：结果卡行、最高档、抽完的余额。
    /// 错误只是一条给人看的字（进菜单错误行），调用方不许因为它而停环。
    ///
    /// `lang`/`scale` 都在**这一刻**烤进字符串：文案由 `l10n` 按语言选，截断按当前缩放
    /// 换算的像素预算算（§4.5）。⇒ 之后切语言/换显示器都不会重刷已经挂着的卡，这是语义
    /// 而不是缺陷（"当前显示的卡永远与生成它时的语言一致"，§7-R10）。
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
        // 显示名 = catalog 里的**中文**名（`蓝帽`），查不到回退 `item_id` ⇒ **绝不留白**。
        // 四 locale 一律显示目录中文，英文界面配中文物品名是明写的取舍（§7-R12：给目录加
        // `name_en` 要动 `petcore::gacha::catalog` 的 `deny_unknown_fields` schema，本轮不做）。
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

    /// 「已获得」那一屏的行：每档各拿到几种 / 该档共几种，最后一行是总数 + 重复 + 余额。
    ///
    /// **纯读**：不掷骰、不花钱、不改库 —— 这是它能进菜单而 `redeem_ssr`（花掉 120 碎片、
    /// 不可回退，而这块 surface 没有对话框，D14）不能进的分界。
    ///
    /// 分母取 **catalog** 而不是库存行数 ⇒ "还缺多少"这件事在这一行就读得出来（`n/total`），
    /// 而库存自己只能回答"有什么"。重复数按 `count − 1` 累加：第一件不算重复，哪怕它躺在
    /// 库存里 —— 这是 §3.6 的折算口径（第 2 件起才折碎片），不是我在省一次查询。
    ///
    /// ⚠️ **每一串字都只用「字体子集里已有的字」**（`l10n` 标签 + ASCII + catalog 名）：
    /// 这一批的字现在**同时**是屏上那一行和 stderr 那一行（`App::gacha_panel` 一条路径出两边），
    /// 而 32 px 档的字号竞技场只剩 ≈7 个字形。代价**明写而不是藏起来**：原先那句
    /// 「共 3/30 · 重复 5 · 碎片 120」的中文散文没了，读者要认 `DUP` / `Shards` 这类标签；
    /// 换来的是"屏幕上读到的与日志里打到的是同一串字"，一个口径。
    pub fn collection_lines(&self, lang: Lang) -> Result<Vec<String>, String> {
        let owned = self.store.owned().map_err(|e| format!("读库存失败: {e}"))?;
        let mut lines = Vec::with_capacity(Rarity::ALL.len() + 1);
        let mut dup = 0u64;
        let mut kinds = 0usize;
        let mut total = 0usize;
        for r in Rarity::ALL {
            let mut have = 0usize;
            let mut all = 0usize;
            for item in self.catalog.items_of(r) {
                all += 1;
                if owned.contains(&item.id) {
                    have += 1;
                    let n = self
                        .store
                        .item_count(&item.id)
                        .map_err(|e| format!("读 {} 的件数失败: {e}", item.id))?;
                    dup += n.saturating_sub(1);
                }
            }
            kinds += have;
            total += all;
            lines.push(format!("{} {have}/{all}", r.as_str()));
        }
        lines.push(format!(
            "{kinds}/{total} {} {dup} {} {}",
            lang.t(Key::Dup),
            lang.t(Key::Shards),
            self.balance()
        ));
        Ok(lines)
    }

    /// 「已获得」那一本的**逐件**行（P4 = 收藏册点名 → 直接穿上）。
    ///
    /// ⚠️ **遍历的是 `catalog ∩ owned`（稀有度序 + catalog 内序），不是 `candidates`** —— 后者
    /// 只有磁盘上真的有产物那几件（裸开发树 30 件里只有 1 件），拿它当列表而汇总行读的是
    /// `catalog ∩ owned`，屏上就是"汇总说 12 件而列表只有 1 行"，两个事实自己打自己。
    /// 同一个集合还有一条好处：名字一律来自 catalog，所以不需要 `recent_lines` 那种"catalog
    /// 不认就退化成 id"的分支（那一条读的是账本，账本可以记着目录里没有的 id）。
    ///
    /// 三档**可点/只读**由 [`AlbumEntry::target`] 一格表示，而它的判据是别人给的（不是这里自己算的）：
    /// `ids` = `App::candidates` 的目录名表，`visible` = `App::dress_view` 跑完库存那道之后
    /// 的可见下标集。⇒ "抽到但磁盘没产物""磁盘有但库存不认""本体不匹配"三种形状都落在
    /// `target = None`，屏上那一行**没有强调条**（画师读的正是可点性，见 [`card::paint_panel`]），
    /// 用户看得见地能分辨，而我不用为这三种形状各造一个新字形或新标记。
    ///
    /// ⚠️ P5 把"宠物永远只读"这一条**换成了第二个判据源**：`pet_ids` / `pet_visible` 是
    /// `App::bodies` 的目录名表与 `App::pet_view` 的可见集，形状与上面那一对**完全同构**（同一
    /// 个 [`DressView::select`]，只是喂的表不同）。所以本体现在有三档路由：配件格 ⇒
    /// [`AlbumTarget::Accessory`]、宠物格 ⇒ [`AlbumTarget::Body`]、两表都查不到 ⇒
    /// [`AlbumTarget::None`]。**"没有强调条"这一档对三类原因仍然只有一句实现**（未拥有 / 磁盘没
    /// 产物 / 跨档出局），跨档那一只在 [`check_body`] 第 ② 道就被扫盘挡掉 ⇒ 它压根不在
    /// `pet_ids` 里 ⇒ 落到同一支 `None`（= 只读，而不是"点了被拒"，拍板② 的验收形状）。
    ///
    /// `dup` 的折算口径与 [`Self::collection_lines`] **同一个**（`item_count − 1`：第一件不算
    /// 重复），否则逐件行加起来与汇总那一行对不上，就成了"两本账"。
    ///
    /// `owned` 由调用方给（`App::album_screen` 一次点击只读一遍库存 ⇒ 逐件那几张行与
    /// 汇总行问的是**同一份**答案，不是"各问了一遍然后祈祷它们相同"）。
    pub fn album_entries(
        &self,
        owned: &Owned,
        ids: &[String],
        visible: &[usize],
        pet_ids: &[String],
        pet_visible: &[usize],
    ) -> Result<Vec<AlbumEntry>, String> {
        let mut out = Vec::new();
        for r in Rarity::ALL {
            for item in self.catalog.items_of(r) {
                if !owned.contains(&item.id) {
                    continue;
                }
                let kind_pet = item.kind == ItemKind::Pet;
                // **两张表各查各的**，而且"查得到 + 过得了那道闸"两个条件都要成立。
                // 反查一次而不是把下标带进来看起来像"顺手"，其实是要紧的：**id 是唯一入口**，
                // 下标在这里是查出来的结果 ⇒ catalog 的顺序与磁盘的顺序谁变了都不影响路由。
                let target = if kind_pet {
                    pet_ids
                        .iter()
                        .position(|c| c == &item.id)
                        .filter(|i| pet_visible.contains(i))
                        .map_or(AlbumTarget::None, AlbumTarget::Body)
                } else {
                    ids.iter()
                        .position(|c| c == &item.id)
                        .filter(|i| visible.contains(i))
                        .map_or(AlbumTarget::None, AlbumTarget::Accessory)
                };
                let n = self
                    .store
                    .item_count(&item.id)
                    .map_err(|e| format!("读 {} 的件数失败: {e}", item.id))?;
                out.push(AlbumEntry {
                    target,
                    name: item.name.clone(),
                    rarity: item.rarity,
                    slot: item.slot,
                    kind_pet,
                    dup: n.saturating_sub(1),
                });
            }
        }
        Ok(out)
    }

    /// 「还缺」那一屏的行：catalog 里还没拿到的**名字**，按档分组，**每档最多一行**。
    ///
    /// 名字来自 catalog ⇒ 这些字**已经在字体子集里**（`everything_the_ui_can_render_is_in_the_
    /// codes_list` 那条门禁把 catalog 也纳进去了），所以这一屏是换装页那条"配件名盖不住"的反面：
    /// 它真的能上卡片。
    ///
    /// ⚠️ **宽度是量出来的，不是猜的**（这条是这一批新加的，而它本来就该在产行侧）：一行的预算
    /// = [`panel::ROW_TEXT_W`]（只读行让位后的矩形）经 [`menu::budget_for`] 换算，装不下的名字
    /// 折进尾标 [`l10n::line_more`]（`+N 更多`，四语言都已经在子集里 ⇒ 零新字形）。
    /// **不靠 `card` 的 `fit` 兜底**：那一层截出来的是一个 `…`，读者会以为"缺的就这些"，
    /// 而真正缺的还有 9 件 —— 一行只读报表把数量说错，和把金额说错是同一条红线。
    /// 精确计数另有一行（`have/all`），这一屏给的是"缺哪些名字"的线索 + 几个没列出。
    ///
    /// ⚠️ **"放不下"必须问 [`menu::fit_to`] 本身，不能拿 [`menu::line_width`] 自己算**（这条
    /// 是新门禁实测逼出来的，别再改回去）：`fit_to` 决定截点用的是**逐 char +1 px 的逻辑上界**
    /// （`menu::advance_bound_log`，为了"三档截在同一个字上"可证），`line_width` 量的是**实际**
    /// 画宽 ⇒ 按后者产行、按前者截行 = 生产者和排版各有各的预算，实测在 1× 少一个字，
    /// 产出的行被排版截成 `…`，正好落进上面那条红线。问同一个函数 ⇒ 这一屏的结论**按构造**
    /// 就是屏幕上会被原样画出来的那行。
    pub fn missing_lines(&self, lang: Lang, scale: f32) -> Result<Vec<String>, String> {
        let owned = self.store.owned().map_err(|e| format!("读库存失败: {e}"))?;
        let budget = menu::budget_for(panel::ROW_TEXT_W, scale);
        let fits = |s: &str| menu::fit_to(s, budget, scale) == s;
        let mut lines = Vec::new();
        for r in Rarity::ALL {
            let names: Vec<&str> = self
                .catalog
                .items_of(r)
                .filter(|i| !owned.contains(&i.id))
                .map(|i| i.name.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            let all = format!("{} {}", r.as_str(), names.join(" "));
            if fits(&all) {
                lines.push(all);
                continue;
            }
            // 从"少折一个"往下减，第一个 `fits` 的就收 —— 名字位数最多影响上界里一个 char，
            // 而这里量的是**成品整行**，所以不存在"列完才发现尾标挤不下"那种改判。
            let mut line = format!("{} {}", r.as_str(), l10n::line_more(lang, names.len()));
            for k in (1..names.len()).rev() {
                let cand = format!(
                    "{} {} {}",
                    r.as_str(),
                    names[..k].join(" "),
                    l10n::line_more(lang, names.len() - k)
                );
                if fits(&cand) {
                    line = cand;
                    break;
                }
            }
            lines.push(line);
        }
        if lines.is_empty() {
            lines.push(format!("0 {}", lang.t(Key::Missing)));
        }
        Ok(lines)
    }

    /// 「最近」那一屏的行：账本最后 `n` 次抽取。**只读 `gacha_pull`，一个字都不写**。
    ///
    /// ⚠️ **不印时间**：`ts` 是 unix 秒，而这个 crate 没有任何日期格式化依赖（引一个只为把
    /// 一串数字变好读，代价是 `Cargo.lock` 与 D8 那条许可证核对）。印 `seq` 而不是 `ts`：
    /// 序号是账本自己的顺序、可重放、可和 `audit verify` 的 `len` 对上，而墙钟秒数在屏幕上
    /// 也读不出"几点"。
    ///
    /// 一次全表读（`rows` 没有 LIMIT）换来的是"这一条路径与 `audit verify` 走的是同一个
    /// 查询" ⇒ 少一个口径。点击驱动、不是每 tick 轮询（§4 那条禁止）。
    pub fn recent_lines(&self, lang: Lang, n: usize) -> Result<Vec<String>, String> {
        let rows = self
            .store
            .rows(petstore::DEFAULT_PROFILE, petstore::DEFAULT_BANNER)
            .map_err(|e| format!("读账本失败: {e}"))?;
        if rows.is_empty() {
            return Ok(vec![format!("0 {}", lang.t(Key::Recent))]);
        }
        let from = rows.len().saturating_sub(n);
        Ok(rows[from..]
            .iter()
            .map(|r| {
                let name = self
                    .catalog
                    .find(&r.item_id)
                    .map(|i| i.name.as_str())
                    .unwrap_or(r.item_id.as_str());
                let dup_tag = if r.duplicate {
                    format!(" {}", lang.t(Key::Dup))
                } else {
                    String::new()
                };
                format!(
                    "#{seq} {rar} {name}{dup_tag} +{shards}",
                    seq = r.seq,
                    rar = r.rarity.as_str(),
                    shards = r.shards_gained,
                )
            })
            .collect())
    }

    /// 「复核」那一屏的两行：审计链全链校验 + 用**库里的** seed 与公示参数重放。
    ///
    /// `Store::verify` 只读 ⇒ 这一行不需要确认步骤（同 [`Self::collection_lines`] 那条分界）。
    /// 报红的时候**只报错**，不报"通过了一半"：一条链要么自洽要么不自洽，中间态不存在。
    ///
    /// 档位计数那行借 [`l10n::line_tiers`]：它与结果卡用的是同一个生产者 ⇒ 同一批字，
    /// 而 `line_tiers` 只列非零档 ⇒ 空账本那行是光秃秃一个 `OK`，这是事实而不是排版事故。
    ///
    /// ⚠️ **链头在这块屏上只能是前缀，而前缀必须自己说它是前缀**（这条同样是新门禁逼出来的：
    /// 原先那行 `复核 {len} {64 位十六进制}` 实测在 1× 被 `fit_to` 截成
    /// `复核 10 d2427b64ac70b454da557…` —— 一个被悄悄切短的哈希比不印哈希更坏，读者会拿它去
    /// 和 CLI 的输出逐字比，而"少了 39 位"这件事一行像素门禁都看不见）。所以这里
    /// **先数出装得下的位数、把它写进正文**（`{k}/64`，纯 ASCII ⇒ 零新字形），再按同一口径
    /// 用 [`menu::fit_to`] 挑最长的那一版。完整的 64 位不在屏上也不在日志里：权威出处是
    /// `opet audit verify --db … --account …`（它打的就是同一个 `head_hex`）。
    /// k=0（行窄到一位都装不下）就整个字段不印 ⇒ 那一行只剩 `复核 {len}`，是少说，不是说错。
    pub fn verify_lines(&self, lang: Lang, scale: f32) -> Result<Vec<String>, String> {
        let report = self
            .store
            .verify(
                &self.catalog,
                petstore::DEFAULT_PROFILE,
                petstore::DEFAULT_BANNER,
            )
            .map_err(|e| format!("复核不过: {e}"))?;
        let budget = menu::budget_for(panel::ROW_TEXT_W, scale);
        let fits = |s: &str| menu::fit_to(s, budget, scale) == s;
        let head = report.head_hex();
        let base = format!("{} {}", lang.t(Key::Verify), report.len);
        let mut shown = String::new();
        for k in (1..=head.len()).rev() {
            let cand = format!("{base} {k}/{} {}", head.len(), &head[..k]);
            if fits(&cand) {
                shown = cand;
                break;
            }
        }
        if shown.is_empty() {
            shown = base;
        }
        Ok(vec![
            shown,
            format!("OK {}", l10n::line_tiers(&report.counts)),
        ])
    }

    /// 收藏册三个 tab 的**唯一**行生产者：菜单那三行、面板里点标题带换 tab、切语言后重建行
    /// 三条路都从这里过 ⇒ 不存在"屏上是这一个 tab 的字、stderr 打的是另一个"。
    ///
    /// `Recent` 的条数 = [`panel::MAX_ROWS`] 减一（一屏正好装下，见
    /// [`panel::collection_rows`] 那条"正解在调用方"的注释）；给多了不报错而是截断补
    /// 「+N 更多」，那条分支是保险，不是主路径。
    ///
    /// `scale` 只有 `Missing` 那一支读（[`Gacha::missing_lines`] 按宽度折名字），另外两支
    /// 的行与缩放无关。它和 [`Gacha::pull`] 的 `scale` 是同一个口径：**生产行的那一刻就知道
    /// 自己要上哪块屏**，而不是画的时候再猜。
    pub fn tab_lines(
        &self,
        tab: CollectionTab,
        lang: Lang,
        scale: f32,
    ) -> Result<Vec<String>, String> {
        match tab {
            CollectionTab::Owned => self.collection_lines(lang),
            CollectionTab::Missing => self.missing_lines(lang, scale),
            CollectionTab::Recent => self.recent_lines(lang, panel::MAX_ROWS - 1),
        }
    }
}
