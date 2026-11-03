//! 界面文案的唯一来源：四种语言 × 一张词表（`docs/UI_REDESIGN_V2.md` §4.3）。
//!
//! 纯逻辑 —— 不许出现 wayland 类型，也不许读环境变量/文件（`Lang` 的生效值由 `settings.rs`
//! 按"`--lang` > config.toml > `LC_ALL`/`LANG` > 默认"决定后传进来），headless CI 必绿。
//!
//! ⚠️ **改这里的任何一个非 ASCII 字面量，必须重跑 `python3 tools/subset_font.py` 重裁字体**，
//! 否则卡片上那个字会静默画成缺字形 —— 像素计数断言抓不住"画了但读不出"（上一轮白字金底
//! 就是同族）。两道测试替你记着这件事，它们在 `tests/font_assets.rs`：
//! `everything_the_ui_can_render_is_in_the_codes_list`（会渲染的 ⊆ 清单）与
//! `codes_list_covers_nothing_unrendered`（清单 ⊆ 会渲染的）；再加上 `text.rs` 里
//! `every_listed_codepoint_has_a_glyph_in_the_asset`（清单 ⊆ 真 cmap），三条把"清单 = 字体 = 上屏"夹死。
//!
//! 另一条硬规矩：**渲染层（`menu.rs`/`card.rs`）不许再写任何文案字面量**，只准从这里取。
//! 上一轮的教训是"排版口径散在三处"，文案同理。⚠️ 这条**目前没有自动化**：想扫源码里的
//! 非 ASCII 字面量，会把 `"candidates 非空"` 这类**诊断消息**一起抓进来（它们永不上屏），
//! 正则分不开"文案"和"消息"。兜得住的是**落笔时**查 cmap 的 [`crate::text::missing`] ——
//! 它看的是到达画布的那串字，硬写的中文一旦真画上去就红，比任何源码扫描都准
//! （`card.rs` 换成 `text::draw` 的那一步把它接上，本轮 C2 只有函数）。

use petcore::gacha::Rarity;

/// 界面语言。**默认中文**（`DEFAULT`），四档在菜单底部语言行里循环。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lang {
    Zh,
    En,
    Ja,
    Ko,
}

impl Lang {
    pub const ALL: [Self; 4] = [Self::Zh, Self::En, Self::Ja, Self::Ko];

    /// 没给 `--lang`、config 里没有、环境变量也认不出来时的落点。
    pub const DEFAULT: Self = Self::Zh;

    /// 语言行的循环顺序：zh → en → ja → ko → zh。
    pub const fn next(self) -> Self {
        match self {
            Self::Zh => Self::En,
            Self::En => Self::Ja,
            Self::Ja => Self::Ko,
            Self::Ko => Self::Zh,
        }
    }

    /// `config.toml` / `--lang` / `LANG` 里用的 ISO 子标签。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Ko => "ko",
        }
    }

    /// **每种语言用它自己的文字自称**（中文 / English / 日本語 / 한국어）。
    /// 这不是装饰：语言行是"选语言"这个动作唯一的界面入口，而看不懂当前语言的人恰恰是
    /// 最需要点它的人 —— 翻成当前语言就把它变成了循环按钮，只能一圈一圈试。
    /// ⇒ 四个自名的字形**都得进子集**，与生效语言无关（门禁 `every_ui_string_is_in_the_font` 逐档查）。
    pub const fn self_name(self) -> &'static str {
        match self {
            Self::Zh => "中文",
            Self::En => "English",
            Self::Ja => "日本語",
            Self::Ko => "한국어",
        }
    }

    /// 只认四档，其余返回 `None` 让调用方**出声**（`UI_REDESIGN_V2` §7-R14：静默退回默认
    /// 就是"看起来支持、其实没生效"那一族）。接受 `zh`/`zh_CN`/`zh-Hant`/`zh_CN.UTF-8`
    /// 这类带地区与编码后缀的形式 —— 取第一个 `._-` 之前的子标签，大小写无关。
    ///
    /// ⚠️ 已知限制：`zh_TW`/`zh_Hant` 会落到 `Zh`，因为本轮只有简体文案。
    pub fn parse(raw: &str) -> Option<Self> {
        let tag = raw
            .trim()
            .to_ascii_lowercase()
            .split(['.', '_', '-'])
            .next()
            .unwrap_or("")
            .to_string();
        Self::ALL.into_iter().find(|l| l.code() == tag)
    }

    pub fn t(self, k: Key) -> &'static str {
        translations(k, self)
    }

    /// 本语言**会画上卡片**的全部字面量，一个都不漏 —— 字体门禁用它的返回值，而不是
    /// 用人写的清单（清单会过期，函数不会漏项）。
    pub fn ui_strings(self) -> Vec<String> {
        let mut out: Vec<String> = Key::ALL.iter().map(|&k| self.t(k).to_string()).collect();
        out.extend(Self::ALL.iter().map(|&l| l.self_name().to_string()));
        // 参数化行的固定部分 + 一个真实形状的样例（数字与档位名都是 ASCII，但物品名不是，
        // 它由 catalog 那条门禁管，这里只保证模板自身的字都在）。
        for n in [1usize, 7, 10] {
            for best in Rarity::ALL {
                out.push(line_pulled(self, n, Some(best)));
            }
            out.push(line_pulled(self, n, None));
            out.push(line_more(self, n));
        }
        out.push(line_shards(self, 120));
        out.push(line_balance(self, 1_240));
        out.push(line_tiers(&[0, 1, 2, 1]));
        out.push(line_item(self, Rarity::SSR, "n", true));
        out.push(line_error(self, "x"));
        out.push(header_shards(self, 0, true));
        out.push(header_shards(self, 0, false));
        out.push(lang_row(self));
        out
    }
}

/// 词表的所有键。加一个键 = 四列都要填 + 重跑 `ui_strings` 门禁 + 重裁字体。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// 盲盒页的页名。六个页名一律 `Page*` ⇒ 标题条由 `MenuPage::key()` 选，不是常量。
    PageGacha,
    /// 动作页的页名
    PageAction,
    /// 其它页的页名
    PageMisc,
    /// 换装页的页名
    PageDress,
    /// 收藏册页的页名
    PageCollection,
    /// 设置页的页名
    PageSettings,
    Pull1,
    Pull10,
    Close,
    /// 动作页三行。发到内核是 `Command::Go(Action::*)`，**标签是静态的**：客户端不读
    /// 内核状态，所以不做"暂停/唤醒"那种两态字面（画一个客户端读不到的状态就是假按钮）。
    Walk,
    Run,
    Jump,
    Climb,
    /// "暂停"那一行 ⇒ 内核 `Action::Sleep`。叫暂停不叫睡觉是有意的：这一行对用户是
    /// "让它安静一会儿"，而唤醒它 = 点动作页任意一行（内核 `handle_go` 的既有语义）。
    Pause,
    /// 退出宠物进程（`exit = true` ⇒ 走正常收尾，退出摘要照打）。
    Quit,
    /// 换装页第一行 ⇒ 把候选表移到配件库里的下一件。**只在内存里挪一个下标**，所以这一行
    /// 不解码也不释放 ⇒ 它换来的是"这一页能选哪件"，而"穿没穿"仍由下面两行决定。
    /// 反馈只出在 stderr：选中的那件叫什么名字是用户内容，子集字体盖不住 ⇒ 不能上卡片。
    NextOutfit,
    /// 换装页第二行 ⇒ 把当前选中的那件叠上去（内存档 M1 ⇒ 到这一步才解码 sheet）。
    /// **标签静态**，不做"已穿/未穿"两态：两态字面要把 `worn.is_some()` 送进 `content_sig`，而
    /// 进了签名的字段就必须被画师读到（否则就是白投帧），换来的只是把同一行换个词。
    Equip,
    /// 换装页第三行 ⇒ 丢掉配件、把穿透轮廓换回本体那份。
    Unequip,
    /// 语言行的标签
    Lang,
    /// 收藏册第一行 ⇒ 把"每一档各拥有几种 / 共几种 + 重复数 + 碎片"打到 stderr。**这一行不掷骰、
    /// 不花钱、不改库** ⇒ 它是纯读，所以它**不需要**结果卡相位，也就不参与 `content_sig`。
    Owned,
    /// 收藏册第二行 ⇒ 把 catalog 里还没拿到的那些个名字打到 stderr（名字来自 catalog ⇒ 已在子集里）。
    Missing,
    /// 收藏册第三行 ⇒ 把账本最近若干次抽取打到 stderr（读 `gacha_pull` 表，不写）。
    Recent,
    /// 设置第一行 ⇒ 循环 `full → reduce → off`。**只改本进程**，同 [`Self::Lang`] 一个道理：
    /// 手写 reader 保不住注释 ⇒ 不写回 config.toml。
    MotionTier,
    /// 设置第二行 ⇒ 把公示概率打到 stderr。数据是 `rates.json`（引擎推导产物，不掷骰），
    /// 所以这一行读的就是"公示牌上那几个数"，不是"这次抽到了什么"。
    Rates,
    /// 设置第三行 ⇒ 跑一遍审计链复核并把结论打到 stderr。**只读**（`verify` 不写回），
    /// 所以它是"设置页第三行"而不是"某个要确认的操作"。
    Verify,
    /// 新获得标
    New,
    /// 重复获得标
    Dup,
    Shards,
    Balance,
    /// 内存库（没给 `--db`）的徽标
    DryRun,
    /// 扩散期中央牌上的字
    Burst,
    /// 错误行的前缀（正文来自 `petstore` 的 `Display`，本轮不翻译 ⇒ §7-R9）
    ErrorPrefix,
}

impl Key {
    pub const ALL: [Self; 32] = [
        Self::PageGacha,
        Self::PageAction,
        Self::PageMisc,
        Self::PageDress,
        Self::PageCollection,
        Self::PageSettings,
        Self::Pull1,
        Self::Pull10,
        Self::Close,
        Self::Walk,
        Self::Run,
        Self::Jump,
        Self::Climb,
        Self::Pause,
        Self::Quit,
        Self::NextOutfit,
        Self::Equip,
        Self::Unequip,
        Self::Lang,
        Self::Owned,
        Self::Missing,
        Self::Recent,
        Self::MotionTier,
        Self::Rates,
        Self::Verify,
        Self::New,
        Self::Dup,
        Self::Shards,
        Self::Balance,
        Self::DryRun,
        Self::Burst,
        Self::ErrorPrefix,
    ];
}

/// 一张表把四语言摊平在一个 `match` 里：四列并排才看得出谁漏了。漏项不可能静默 ——
/// 这个 `match` 没有 `_` 分支，加键不填全编译不过。
fn translations(k: Key, l: Lang) -> &'static str {
    match (k, l) {
        (Key::PageGacha, Lang::Zh) => "盲盒",
        (Key::PageGacha, Lang::En) => "Gacha",
        (Key::PageGacha, Lang::Ja) => "ガチャ",
        (Key::PageGacha, Lang::Ko) => "가챠",

        (Key::PageAction, Lang::Zh) => "动作",
        (Key::PageAction, Lang::En) => "Actions",
        (Key::PageAction, Lang::Ja) => "動作",
        (Key::PageAction, Lang::Ko) => "동작",

        (Key::PageMisc, Lang::Zh) => "其它",
        (Key::PageMisc, Lang::En) => "Other",
        (Key::PageMisc, Lang::Ja) => "その他",
        (Key::PageMisc, Lang::Ko) => "기타",

        // 页名取 **帽子** 而不是 **换装**：这一页第一行是「换一件」，两个词共用「换」⇒ 同一屏上
        // 出现两个"看着像同一件事"的字，而它们一个改下标、一个写内存 —— 是两种不同的活。
        // 页名换成这一页此刻唯一真有货的那件配件（`head` 槽 = catalog 里最多的一档，而量到的
        // 挂点与烘出的配件都是帽子）⇒ 名字互不重复这条由
        // `menu::tests::no_two_menu_names_are_the_same_word_in_any_locale` 钉住。
        // ⚠️ 代价已量明：`帽 U+5E3D` 已在清单（catalog 的 `name` 里有），`子 U+5B50` 不在 ⇒
        // **这一改是一次真字体重裁**（+1 码位），不是改一个字符串。
        (Key::PageDress, Lang::Zh) => "帽子",
        (Key::PageDress, Lang::En) => "Outfit",
        (Key::PageDress, Lang::Ja) => "服装",
        (Key::PageDress, Lang::Ko) => "의상",

        // 收藏册与设置。en 用 `Album` 而不是 `Collection`：这块卡片宽 240 logical px，
        // 标题条还要在同一行放页名 + 页码 + 右侧碎片/演示 ⇒ 短一支是一个像素的余量，
        // 而"册子"这个含义在抽卡语境里本来就靠图鉴而不是收藏这个词承担。
        (Key::PageCollection, Lang::Zh) => "收藏",
        (Key::PageCollection, Lang::En) => "Album",
        (Key::PageCollection, Lang::Ja) => "図鑑",
        (Key::PageCollection, Lang::Ko) => "도감",

        (Key::PageSettings, Lang::Zh) => "设置",
        (Key::PageSettings, Lang::En) => "Settings",
        (Key::PageSettings, Lang::Ja) => "設定",
        (Key::PageSettings, Lang::Ko) => "설정",

        (Key::Pull1, Lang::Zh) => "抽一次",
        (Key::Pull1, Lang::En) => "Pull 1",
        (Key::Pull1, Lang::Ja) => "1回引く",
        (Key::Pull1, Lang::Ko) => "1회 뽑기",

        (Key::Pull10, Lang::Zh) => "十连",
        (Key::Pull10, Lang::En) => "Pull 10",
        (Key::Pull10, Lang::Ja) => "10連",
        (Key::Pull10, Lang::Ko) => "10연속",

        (Key::Close, Lang::Zh) => "关闭",
        (Key::Close, Lang::En) => "Close",
        (Key::Close, Lang::Ja) => "閉じる",
        (Key::Close, Lang::Ko) => "닫기",

        // 动作页与退出。日语动词用辞书形（歩く/走る）而不是"歩き"：这一行是**命令**，
        // 而菜单里其余各行（引く/閉じる/終了）也全是辞书形，同一张表里不许两种体。
        (Key::Walk, Lang::Zh) => "走路",
        (Key::Walk, Lang::En) => "Walk",
        (Key::Walk, Lang::Ja) => "歩く",
        (Key::Walk, Lang::Ko) => "걷기",

        (Key::Run, Lang::Zh) => "跑步",
        (Key::Run, Lang::En) => "Run",
        (Key::Run, Lang::Ja) => "走る",
        (Key::Run, Lang::Ko) => "달리기",

        (Key::Jump, Lang::Zh) => "跳跃",
        (Key::Jump, Lang::En) => "Jump",
        (Key::Jump, Lang::Ja) => "ジャンプ",
        (Key::Jump, Lang::Ko) => "점프",

        (Key::Climb, Lang::Zh) => "爬墙",
        (Key::Climb, Lang::En) => "Climb",
        (Key::Climb, Lang::Ja) => "壁登り",
        (Key::Climb, Lang::Ko) => "등반",

        (Key::Pause, Lang::Zh) => "暂停",
        (Key::Pause, Lang::En) => "Pause",
        (Key::Pause, Lang::Ja) => "一時停止",
        (Key::Pause, Lang::Ko) => "일시 정지",

        (Key::Quit, Lang::Zh) => "退出",
        (Key::Quit, Lang::En) => "Quit",
        (Key::Quit, Lang::Ja) => "終了",
        (Key::Quit, Lang::Ko) => "종료",

        // 换装页三行。后两行是一对反义动作 ⇒ 四语言都成对（穿上/脱下、着る/脱ぐ、착용/해제），
        // 而 ja 那两支守上面那条"动词用辞书形"；第一行是**选择**而不是动作对（ja 取名词
        // `着替え`），因为这一页做的事 = 从配件库里挑一件再穿上。"回第一页"交给标题条
        // （`Dress.next() == Gacha`）⇒ 恒三行逼出来的那第三格现在被真活占了，不是凑数。
        (Key::NextOutfit, Lang::Zh) => "换一件",
        (Key::NextOutfit, Lang::En) => "Change",
        (Key::NextOutfit, Lang::Ja) => "着替え",
        (Key::NextOutfit, Lang::Ko) => "교체",

        (Key::Equip, Lang::Zh) => "穿上",
        (Key::Equip, Lang::En) => "Wear",
        (Key::Equip, Lang::Ja) => "着る",
        (Key::Equip, Lang::Ko) => "착용",

        (Key::Unequip, Lang::Zh) => "脱下",
        (Key::Unequip, Lang::En) => "Remove",
        (Key::Unequip, Lang::Ja) => "脱ぐ",
        (Key::Unequip, Lang::Ko) => "해제",

        (Key::Lang, Lang::Zh) => "语言",
        (Key::Lang, Lang::En) => "Language",
        (Key::Lang, Lang::Ja) => "言語",
        (Key::Lang, Lang::Ko) => "언어",

        // 收藏册三行。三行**都是读**而不是写（读 owned / 读 catalog−owned / 读账本尾 N 条），
        // 所以都不需要确认步骤，也不进 `content_sig`（画面一个字不变，变的只是 stderr）。
        // 标签取"这一行读出什么"这一族名词，不取动词：菜单里唯一会改状态的动作行（穿上/脱下）
        // 已经在换装页用动词，这一页混进动词就会让人以为点它会抽卡。
        (Key::Owned, Lang::Zh) => "已获得",
        (Key::Owned, Lang::En) => "Owned",
        (Key::Owned, Lang::Ja) => "入手",
        (Key::Owned, Lang::Ko) => "획득",

        (Key::Missing, Lang::Zh) => "还缺",
        (Key::Missing, Lang::En) => "Missing",
        (Key::Missing, Lang::Ja) => "未入手",
        (Key::Missing, Lang::Ko) => "미수집",

        (Key::Recent, Lang::Zh) => "最近",
        (Key::Recent, Lang::En) => "Recent",
        (Key::Recent, Lang::Ja) => "最近",
        (Key::Recent, Lang::Ko) => "최근",

        // 设置三行。`MotionTier` 这一行是**循环器**（full → reduce → off → full），标签是它循环的
        // 那个**维度名**而不是当前值：把当前值写进标签就是两态字面 ⇒ 要进签名、要人读，
        // 而这一行按下的瞬间画面本来就不变（`Menu` 相的 frame 恒 0）。值只出 stderr。
        (Key::MotionTier, Lang::Zh) => "动画",
        (Key::MotionTier, Lang::En) => "Motion",
        (Key::MotionTier, Lang::Ja) => "動き",
        (Key::MotionTier, Lang::Ko) => "움직임",

        (Key::Rates, Lang::Zh) => "概率",
        (Key::Rates, Lang::En) => "Rates",
        (Key::Rates, Lang::Ja) => "確率",
        (Key::Rates, Lang::Ko) => "확률",

        (Key::Verify, Lang::Zh) => "复核",
        (Key::Verify, Lang::En) => "Verify",
        (Key::Verify, Lang::Ja) => "検証",
        (Key::Verify, Lang::Ko) => "검증",

        (Key::New, Lang::Zh) => "新",
        (Key::New, Lang::En) => "NEW",
        (Key::New, Lang::Ja) => "新規",
        (Key::New, Lang::Ko) => "신규",

        (Key::Dup, Lang::Zh) => "重复",
        (Key::Dup, Lang::En) => "DUP",
        (Key::Dup, Lang::Ja) => "重複",
        (Key::Dup, Lang::Ko) => "중복",

        (Key::Shards, Lang::Zh) => "碎片",
        (Key::Shards, Lang::En) => "Shards",
        (Key::Shards, Lang::Ja) => "かけら",
        (Key::Shards, Lang::Ko) => "조각",

        (Key::Balance, Lang::Zh) => "余额",
        (Key::Balance, Lang::En) => "Balance",
        (Key::Balance, Lang::Ja) => "残高",
        (Key::Balance, Lang::Ko) => "잔액",

        (Key::DryRun, Lang::Zh) => "演示",
        (Key::DryRun, Lang::En) => "DEMO",
        (Key::DryRun, Lang::Ja) => "デモ",
        (Key::DryRun, Lang::Ko) => "데모",

        (Key::Burst, Lang::Zh) => "开箱中…",
        (Key::Burst, Lang::En) => "Opening…",
        (Key::Burst, Lang::Ja) => "開封中…",
        (Key::Burst, Lang::Ko) => "개봉 중…",

        // 冒号形状各随其文：中日全角、英韩半角。两种都要进子集（`：` 不是 ASCII）。
        (Key::ErrorPrefix, Lang::Zh) => "抽取失败：",
        (Key::ErrorPrefix, Lang::En) => "Pull failed: ",
        (Key::ErrorPrefix, Lang::Ja) => "抽出失敗：",
        (Key::ErrorPrefix, Lang::Ko) => "뽑기 실패: ",
    }
}

// ───────────────────────── 参数化行 ─────────────────────────
//
// 每语言各写一条 `format!`，**绝不"英文词序 + 本地词表"拼接**：中/英/日/韩的
// `数量·单位·动词` 顺序互不相同（`碎片 +120` vs `+120 shards`），拼出来的句子在
// 任一非源语言里都是坏话。

/// 结果卡标题行。`best = None` 时不提档位（全 N 之外抽空的情况不会发生，但函数不许骗人）。
pub fn line_pulled(l: Lang, n: usize, best: Option<Rarity>) -> String {
    match (l, best) {
        (Lang::Zh, Some(b)) => format!("{n} 抽 最高 {}", b.as_str()),
        (Lang::Zh, None) => format!("{n} 抽"),
        (Lang::En, Some(b)) => format!("{n} pulled, best {}", b.as_str()),
        (Lang::En, None) => format!("{n} pulled"),
        (Lang::Ja, Some(b)) => format!("{n}回 最高{}", b.as_str()),
        (Lang::Ja, None) => format!("{n}回"),
        (Lang::Ko, Some(b)) => format!("{n}회 최고 {}", b.as_str()),
        (Lang::Ko, None) => format!("{n}회"),
    }
}

/// 档位计数行：只列非零档，从高到低。**四语言同形**（`N/R/SR/SSR` 是系统专名，不译）。
pub fn line_tiers(counts: &[u32; 4]) -> String {
    Rarity::ALL
        .iter()
        .rev()
        .filter(|r| counts[r.index()] > 0)
        .map(|r| format!("{}{}", r.as_str(), counts[r.index()]))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 明细行。词序四语言一致（档位 + 名字 + 尾标），只有尾标变字。
/// ⚠️ `name` 来自 catalog，**本轮不翻译**（目录只有单语 `name`，加 `name_en` 要动
/// `petcore::gacha::catalog` 的 `deny_unknown_fields` schema）⇒ 英文界面配中文物品名是
/// 明写的取舍，不是遗漏（`UI_REDESIGN_V2` §7-R12）。
pub fn line_item(l: Lang, r: Rarity, name: &str, dup: bool) -> String {
    let tag = l.t(if dup { Key::Dup } else { Key::New });
    format!("{} {name} {tag}", r.as_str())
}

/// 被省略的其余条目。
pub fn line_more(l: Lang, n: usize) -> String {
    match l {
        Lang::Zh => format!("+{n} 更多"),
        Lang::En => format!("+{n} more"),
        Lang::Ja => format!("+{n} 他"),
        Lang::Ko => format!("+{n} 외"),
    }
}

/// 本次拿到的碎片。注意词序：中/日/韩是"标签 数量"，英是"数量 标签"。
pub fn line_shards(l: Lang, gained: u64) -> String {
    match l {
        Lang::Zh => format!("碎片 +{gained}"),
        Lang::En => format!("+{gained} shards"),
        Lang::Ja => format!("かけら +{gained}"),
        Lang::Ko => format!("조각 +{gained}"),
    }
}

/// 余额行。金额走 `group` 的千分位，**永不截断**（读错钱比看不见更糟）。
pub fn line_balance(l: Lang, balance: i64) -> String {
    let amount = group(balance);
    match l {
        Lang::Zh => format!("余额 {amount}"),
        Lang::En => format!("Balance {amount}"),
        Lang::Ja => format!("残高 {amount}"),
        Lang::Ko => format!("잔액 {amount}"),
    }
}

/// 标题行右端：`碎片 余额 [演示]`。**这里刻意不带"余额"标签** —— 标题行同时要放
/// 徽标，而 en 的 `Shards 1,240 Balance 1,240 DEMO` 一定超排版预算（§7-R13），
/// 超了就永远读得到碎片、读不到演示。徽标的位置比第二个标签值钱。
pub fn header_shards(l: Lang, balance: i64, dry_run: bool) -> String {
    let head = format!("{} {}", l.t(Key::Shards), group(balance));
    if dry_run {
        format!("{head} {}", l.t(Key::DryRun))
    } else {
        head
    }
}

/// 错误行 = 本地化前缀 + `petstore` 的原文。正文不译（那是内核侧文案，会牵动它的测试）。
pub fn line_error(l: Lang, tail: &str) -> String {
    format!("{}{tail}", l.t(Key::ErrorPrefix))
}

/// 语言行：`语言 中文` —— 标签用**当前**语言，自名也用**当前**语言（它已经写成自己的文字）。
/// 标签翻成当前语言是有意的：看得懂卡片的人一定看得懂"语言"这个词，而自名换文字这一步
/// 才是"下一次点下去会变成什么"的信息。
pub fn lang_row(l: Lang) -> String {
    format!("{} {}", l.t(Key::Lang), l.self_name())
}

/// 千分位分组（`,`，四语言共用）。不做 locale 数字格式（阿拉伯数字 + 逗号在四地都读得懂，
/// 而 `1.240` / `1,240` 的差别会让"金额永不截断"那条变成"金额会被读错"）。
pub fn group(v: i64) -> String {
    let digits = v.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if v < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_non_empty_value_in_every_lang() {
        for &l in &Lang::ALL {
            for &k in &Key::ALL {
                assert!(!l.t(k).is_empty(), "{l:?}/{k:?} 是空串");
            }
        }
    }

    #[test]
    fn lang_row_cycles_through_all_four_and_wraps() {
        let mut l = Lang::Zh;
        for _ in 0..4 {
            l = l.next();
        }
        assert_eq!(l, Lang::Zh, "转一圈必须回到起点，否则语言行会点进 None");
    }

    #[test]
    fn self_names_are_written_in_their_own_script() {
        // 这条断言防的是"把日本語翻成 Current language"那一类倒退：自名必须含非 ASCII，
        // 除了 English（它就是拉丁字母），而 English 至少不能等于其它三档。
        assert_eq!(Lang::Zh.self_name(), "中文");
        assert_eq!(Lang::Ja.self_name(), "日本語");
        assert_eq!(Lang::Ko.self_name(), "한국어");
        assert_eq!(Lang::En.self_name(), "English");
        let mut names: Vec<&str> = Lang::ALL.iter().map(|l| l.self_name()).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "四档自名必须互不相同");
    }

    #[test]
    fn parse_accepts_region_and_encoding_suffixes() {
        for (raw, want) in [
            ("zh", Some(Lang::Zh)),
            ("EN", Some(Lang::En)),
            ("ja_JP.UTF-8", Some(Lang::Ja)),
            ("ko-KR", Some(Lang::Ko)),
            ("zh_CN", Some(Lang::Zh)),
            ("  ", None),
            ("", None),
            ("fr", None),
            // 认不出来的必须返回 None 让调用方出声，不能静默当 Zh。
            ("C", None),
            ("POSIX", None),
        ] {
            assert_eq!(Lang::parse(raw), want, "parse({raw:?})");
        }
    }

    #[test]
    fn grouping_is_locale_neutral_and_handles_edges() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_240), "1,240");
        assert_eq!(group(1_000_000), "1,000,000");
        assert_eq!(group(-1_234), "-1,234");
        assert_eq!(group(i64::MIN), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn word_order_differs_per_lang_so_no_template_is_shared() {
        // 参数化行必须**各语言各写**；这条钉的是"碎片/余额"的词序，防止有人顺手抽一个
        // 公共模板 —— 那会让英文界面出现 `碎片 +120`。
        assert_eq!(line_shards(Lang::Zh, 120), "碎片 +120");
        assert_eq!(line_shards(Lang::En, 120), "+120 shards");
        assert_eq!(line_shards(Lang::Ja, 120), "かけら +120");
        assert_eq!(line_shards(Lang::Ko, 120), "조각 +120");
        assert_eq!(line_balance(Lang::En, 1_240), "Balance 1,240");
        assert_eq!(line_balance(Lang::Zh, 1_240), "余额 1,240");
    }

    #[test]
    fn tiers_line_is_ascii_only_tier_names() {
        assert_eq!(line_tiers(&[3, 1, 0, 1]), "SSR1 R1 N3");
        assert_eq!(line_tiers(&[0, 0, 0, 0]), "");
    }

    #[test]
    fn ui_strings_covers_every_key_and_self_name() {
        for &l in &Lang::ALL {
            let s = l.ui_strings();
            for &k in &Key::ALL {
                assert!(
                    s.iter().any(|x| x == l.t(k)),
                    "{l:?} 的 ui_strings 漏了 {k:?} 的值 {:?}",
                    l.t(k)
                );
            }
            for &other in &Lang::ALL {
                assert!(
                    s.iter().any(|x| x == other.self_name()),
                    "{l:?} 的 ui_strings 漏了自名 {:?}",
                    other.self_name()
                );
            }
        }
    }

    #[test]
    fn error_prefix_keeps_the_engine_message_verbatim() {
        let e = line_error(Lang::Zh, "no such table: pets");
        assert_eq!(e, "抽取失败：no such table: pets");
        // 前缀之外的部分必须是原文逐字（截它 = 把唯一的人类可读诊断信息扔掉）
        assert!(e.ends_with("no such table: pets"));
    }

    #[test]
    fn default_lang_is_zh_and_is_in_the_list() {
        assert_eq!(Lang::DEFAULT, Lang::Zh);
        assert!(Lang::ALL.contains(&Lang::DEFAULT));
    }
}
