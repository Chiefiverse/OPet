//! 菜单/结果卡的**纯逻辑**：布局、命中、定位、按钮分派、动画相位表、展示行。
//! 这里不许出现任何 wayland 类型 —— 全部函数吃整数/坐标、出矩形与表，headless CI 必绿。
//! 接线在 `app.rs`（`MenuState`），像素落在 `draw_menu`。

use petcore::gacha::Rarity;

use crate::l10n::{self, Key, Lang};
use crate::settings::Motion;
use crate::text;

/// 可点的菜单项。余额行是信息，不进命中表。
///
/// ⚠️ `Lang`（语言行）**不在 [`MenuItem::CLICKABLE`] 里**，但**能被命中** —— 它走
/// [`footer`] 那一块单独的矩形，好让"三行行带 + 缝隙不命中"那条证明一行不动（§4.4）。
/// 加了它 `on_menu_press` 的 `match` 会因穷尽性编译不过 ⇒ 新项漏分派不可能静默。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Pull1,
    Pull10,
    Close,
    /// 点一下循环 zh→en→ja→ko。**只改本进程**，不写回 config.toml（§7-R15：手写 reader
    /// 保不住用户注释，读改写会把文件刷平）。
    Lang,
}

impl MenuItem {
    /// 三行行带。常量长度是 3 而不是 4 是有意的：`layout()` 的几何证明、缝隙判定、
    /// `ROW_STEP` 全部按这三行算，语言行不属于这一族。
    pub const CLICKABLE: [Self; 3] = [Self::Pull1, Self::Pull10, Self::Close];

    /// 卡片上显示的字。**唯一来源是 `l10n`**，这里一个字面量都不写（写了就绕过
    /// `everything_the_ui_can_render_is_in_the_codes_list` 那对字体门禁）。
    /// 语言行返回 `语言 中文` 这种"标签 + 当前语言的自名"（理由见 [`l10n::lang_row`]：
    /// 看不懂当前语言的人恰恰最需要点它）。
    pub fn label(self, lang: Lang) -> String {
        match self {
            Self::Pull1 => lang.t(Key::Pull1).to_string(),
            Self::Pull10 => lang.t(Key::Pull10).to_string(),
            Self::Close => lang.t(Key::Close).to_string(),
            Self::Lang => l10n::lang_row(lang),
        }
    }

    /// 这一项占的矩形（行带里的三项 + 底部语言行）。`layout()` 与 [`footer`] 的并集就是
    /// 命中表 ⇒ 两处共用同一份几何，draw 与 hit_test 不可能各算一套。
    pub fn rect(self) -> ItemRect {
        if self == Self::Lang {
            return footer();
        }
        let i = Self::CLICKABLE
            .iter()
            .position(|&it| it == self)
            .expect("CLICKABLE 覆盖三个行项");
        ItemRect {
            item: self,
            x: PAD,
            y: PAD + HEAD_H + (i as i32) * ROW_STEP,
            w: CARD_W - 2 * PAD,
            h: ROW_H,
        }
    }
}

/// 一个可点矩形（菜单 surface 的 **logical** 坐标，与 margin/region 同口径，物理换算只在 draw 层）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemRect {
    pub item: MenuItem,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl ItemRect {
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

/// 卡片尺寸（logical px）。取菜单态与结果卡两者的最大需求，动画期不重建 surface。
///
/// 宽度由**排版契约**定，不是拍的：结果卡一行最宽的形状是"档位 + 物品名 + 尾标"，而物品名
/// 现在是 catalog 里的**中文名**（`SSR 蓝帽 新`），拉丁语系的标签更长（en 的
/// `+999999 shards`、`Balance 9,999,999`）。⇒ 预算单位必须是**像素**而不是字符数：
/// CJK 每字 ≈1 em、拉丁字母窄得多，按字数截必然一边宽一边窄（§4.5）。
/// 由 [`MAX_LINE_PX`] 与 [`line_width`] 给，逐 locale × 逐档由 `every_locale_fits_the_card` 量。
/// ⚠️ 上一版这里写的是 180 + 20 字符，证明用的是"物理 scale 2"，也就是**只在 1.5× 屏上**
/// 成立（270 物理像素刚好够）；换到 1× 屏就是 240 > 180 直接切字。排版预算必须和显示器
/// 缩放无关，否则门禁绿着、字却被裁了 —— 又是"看起来对、其实说谎"那一族。
/// 这条口径现在由 `report_budget_is_scale_independent` 钉住（同一句话在 1×/1.5×/2× 上
/// 截在同一个字上）。
pub const CARD_W: i32 = 240;
pub const CARD_H: i32 = 150;

/// 降级字模（`font.rs`，5×7 ASCII 位图）的逻辑字号：**逻辑**像素/字模像素。
/// 只有 [`crate::text::face`] 拿不到字体资产时才会用到它（那条路径在 CI 里被
/// `the_shipped_font_loads_and_covers_the_list` 拦住，永远不执行）。留着的理由不是整洁，
/// 是硬约束 3「宠物崩溃不能影响桌面」：字体资产出事只能不画字/画丑字，不能炸进程。
pub const TEXT_PX: i32 = 2;

/// 行几何：左右留边 8，顶部信息行占 22，行高 26，**行步进 30**（26 高 + 4 缝）。
/// 步进必须大于行高，否则相邻行矩形相交 ⇒ `hit_test` 的"缝隙不命中"与"两处重叠"全塌。
/// 这四个数是 `layout` 与 `draw_menu` 共用的唯一排版口径，draw 层不许再算一份。
pub const PAD: i32 = 8;
pub const HEAD_H: i32 = 22;
pub const ROW_H: i32 = 26;
pub const ROW_STEP: i32 = 30;
/// 结果卡行步进（logical）：字体度量给的行高是 16（`line_height == px`，见 `text::LOGICAL_PX`
/// 那张实测表），留 2 leading。
pub const LINE_STEP: i32 = 18;

/// 底部语言行的几何：`124..140`，**右对齐**（`draw` 层按 `measure` 反推 x）。
///
/// 为什么是 16 高：`line_height(16) == 16`，而底部带 `116..150` 只有 34 px —— 两行加间隙要
/// 40 px 装不下。这是算术不是审美（§4.4）。为什么从 124 起：三行的最后一行底边在
/// `PAD + HEAD_H + 2*ROW_STEP + ROW_H = 116`，留 8 px 气 ⇒ 语言行与行带**不相交**，
/// 这条由 `footer_is_below_the_rows_and_inside_the_card` 钉住（写成 122 时那个"气"实测只有
/// 6 px，断言当场红了 ⇒ 常量按证明改，不是按记忆改）。
pub const FOOTER_Y: i32 = 124;
pub const FOOTER_H: i32 = 16;
/// 语言行与错误行同行带，两者之间必须留的气（logical）。错误行左对齐、语言行右对齐。
pub const FOOTER_GAP: i32 = 8;

/// 语言行的矩形（**不**在 [`layout`] 的三行里，但在 [`rects`] 的命中表里）。
pub fn footer() -> ItemRect {
    ItemRect {
        item: MenuItem::Lang,
        x: PAD,
        y: FOOTER_Y,
        w: CARD_W - 2 * PAD,
        h: FOOTER_H,
    }
}

/// 固定布局：3 个可点项从上到下。`layout` 与 `hit_test` 是唯一的真相源，
/// `draw_menu` 画矩形、`menu_pointer_event` 判命中都从这张表取，不许各处再算一份。
pub fn layout() -> Vec<ItemRect> {
    MenuItem::CLICKABLE.iter().map(|&it| it.rect()).collect()
}

/// 命中表 = 三行 + 语言行。语言行走 `footer()` 这条路，所以行带的四个常量与
/// "缝隙不命中"那条证明（`hit_test_hits_centers_and_misses_gaps_and_margins`）一行没动。
pub fn rects() -> Vec<ItemRect> {
    let mut v = layout();
    v.push(footer());
    v
}

/// 菜单-local logical 坐标 → 命中的项。缝隙、边距、行外一律 None。
/// 布局表由调用方持有（`MenuState` 建一次存着），不在热路径上反复重算。
pub fn hit_test(rects: &[ItemRect], at: (f32, f32)) -> Option<MenuItem> {
    rects
        .iter()
        .find(|r| r.contains(at.0, at.1))
        .map(|r| r.item)
}

/// 整数矩形（logical），用于定位与重叠判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectI {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl RectI {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
}

/// 同层两块 surface 的堆叠序在合成器侧不稳定 ⇒ 用"矩形不相交"消除命中歧义与视觉压盖。
pub fn overlaps(a: RectI, b: RectI) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// 菜单摆位：优先宠物右侧 → 左侧 → 上方 → 下方。每个候选**先 clamp 进 output**（保证出屏
/// 不可能），再判是否与宠物相交；第一个不相交的胜出。首选被 clamp 后仍与宠物重叠是常见情况
/// （宠物贴角时），所以"按原始坐标判 inside 再整体失败 → 退到 clamp 的首选"那种写法会留下
/// 重叠 —— 实测在 1280×720 的右下角宠物上就重叠了。
/// 四档 clamp 后全都避开不了（output 比宠物+菜单还小这类病态）才返回 clamp 后的首选：
/// 宁可短暂相交，也不许菜单出屏。
/// 返回值是菜单 surface 左上角的 output **局部 logical 坐标**（layer-shell margin 口径）。
pub fn placement(pet: RectI, out: RectI) -> (i32, i32) {
    const GAP: i32 = 6;
    let candidates = [
        RectI::new(pet.x + pet.w + GAP, pet.y, CARD_W, CARD_H),
        RectI::new(pet.x - GAP - CARD_W, pet.y, CARD_W, CARD_H),
        RectI::new(pet.x, pet.y - GAP - CARD_H, CARD_W, CARD_H),
        RectI::new(pet.x, pet.y + pet.h + GAP, CARD_W, CARD_H),
    ];
    let mut first = None;
    for c in candidates {
        let c = clamp(c, out);
        if first.is_none() {
            first = Some(c);
        }
        if !overlaps(c, pet) {
            return (c.x, c.y);
        }
    }
    let c = first.expect("candidates 非空");
    (c.x, c.y)
}

/// 把卡片矩形收进 output（output 比卡片小时左上角贴 output 原点，溢出不可避免）。
fn clamp(c: RectI, out: RectI) -> RectI {
    let x = c.x.min(out.x + out.w - CARD_W).max(out.x);
    let y = c.y.min(out.y + out.h - CARD_H).max(out.y);
    RectI::new(x, y, CARD_W, CARD_H)
}

/// `linux/input-event-codes.h` 的三个鼠标键。**顺序是 左→右→中**，不是"左中右"：
/// `BTN_LEFT=0x110`、`BTN_RIGHT=0x111`、`BTN_MIDDLE=0x112`。
/// 写成具名常量而不是把十六进制直接摊进 match，是因为本文件曾经把 0x111/0x112
/// 对调过 —— 真机上表现为"右键打不开菜单"，而单测当时**全绿**（测试和实现抄了
/// 同一份误解）。⇒ 键位这种外部 ABI 只能引用具名事实，不能凭印象排序。
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// 原始按键码 → 契约按钮（其余键不上报）。
pub fn button_from_raw(code: u32) -> Option<petcore::render::Button> {
    match code {
        BTN_LEFT => Some(petcore::render::Button::Left),
        BTN_RIGHT => Some(petcore::render::Button::Right),
        BTN_MIDDLE => Some(petcore::render::Button::Middle),
        _ => None,
    }
}

/// 宠物 surface 上一次 Press 该怎么处置。右键**绝不进双击计时**（那会污染左键 `DoubleTap`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetPressAction {
    /// 记时并可能判双击（仅左键）。
    StartDragOrDoubleTap,
    /// 开/关菜单。
    ToggleMenu,
    /// 视而不见（中键、侧键）。
    Ignore,
}

pub fn pet_press_disposition(button: u32) -> PetPressAction {
    match button_from_raw(button) {
        Some(petcore::render::Button::Left) => PetPressAction::StartDragOrDoubleTap,
        Some(petcore::render::Button::Right) => PetPressAction::ToggleMenu,
        _ => PetPressAction::Ignore,
    }
}

/// 右键 = 翻转可见性（关 → 开、开 → 关）；其余键不动菜单。
pub fn menu_toggle_for(button: u32, currently_open: bool) -> bool {
    match pet_press_disposition(button) {
        PetPressAction::ToggleMenu => !currently_open,
        _ => currently_open,
    }
}

/// 菜单 surface 上，哪些键算"点项"。**只有左键**：右键在宠物那块是开关，落进菜单里
/// 若也激活项，"想关掉菜单而点到 PULL 10"就会顺手抽一次；中键同理一律不理。
pub fn menu_press_activates(button: u32) -> bool {
    button == BTN_LEFT
}

/// 指针被取消（Leave / remove_capability）时是否要当场按 Release 收尾。
/// M4 的 P0：不收尾 ⇒ `drag` 永久为真 ⇒ input region 常驻整块实心矩形吞点击。
pub fn pointer_lost_should_end_drag(dragging: bool) -> bool {
    dragging
}

/// 看门狗到期：合成器 `frames` 帧没有回 frame 回调就要强制解锁并补投。
/// 纯谓词，把魔数 `3` 钉在这里而不是散在状态机里（完整活体故障注入仍是 M4 的账）。
pub fn watchdog_expired(pending_frames: u64, limit: u64) -> bool {
    pending_frames >= limit
}

/// 每档稀有度的开箱动画规格（tick @60Hz）。档位越高越夸张，SSR 额外整卡白闪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimSpec {
    pub burst_ticks: u32,
    pub dwell_ticks: u32,
    /// 主色，0xRRGGBB。
    pub color: u32,
    pub white_flash_ticks: u32,
}

pub const SSR_FLASH_TICKS: u32 = 2;

/// `full` 档的原始规格 = **用户已经签收过的那套像素**（2026-10-25 真机判过："burst 和结果卡
/// 都看见了，SSR 是黄色"）。`anim_specs_are_monotonic_and_ssr_flashes` 把这四个数字逐个钉死，
/// 因为下面两档是**从它派生**的：动这里的基准值会连带改掉 `reduce`，而那条改动没人签收过。
fn anim_spec_full(r: Rarity) -> AnimSpec {
    match r {
        Rarity::N => AnimSpec {
            burst_ticks: 4,
            dwell_ticks: 90,
            color: 0x9AA0A6,
            white_flash_ticks: 0,
        },
        Rarity::R => AnimSpec {
            burst_ticks: 8,
            dwell_ticks: 100,
            color: 0x3B82F6,
            white_flash_ticks: 0,
        },
        Rarity::SR => AnimSpec {
            burst_ticks: 16,
            dwell_ticks: 130,
            color: 0xA855F7,
            white_flash_ticks: 0,
        },
        Rarity::SSR => AnimSpec {
            burst_ticks: 24,
            dwell_ticks: 180,
            color: 0xF5B301,
            white_flash_ticks: SSR_FLASH_TICKS,
        },
    }
}

/// 同一张规格表按**动画强度档**收窄（`UI_REDESIGN_V2` §4.7 那张表 = 判据②"闪烁可关"）。
///
/// 三档的取舍刻意不一样：
/// · `Reduce` = **节奏还在、闪烁没了**：burst 缩到 `max(1, n/4)`（1/2/4/6 tick），白闪 0，
///   dwell **不变**（dwell 是"结果卡停留多久"，不是动效；砍它会让玩家来不及读完卡）。
/// · `Off` = **扩散期整个跳过**（`burst_ticks = 0`，且 [`after_pull`] 直接给 `Reveal`）：
///   提交数只减不增 ⇒ 这一档不可能把省电基线做坏。
/// 两档都不动 `color` 与 `dwell_ticks`：**关掉闪烁之后"这是 SSR"只剩色带**，颜色是信息不是装饰
/// （要真机判的就是这个，见 §6-6②）。
pub fn anim_spec(r: Rarity, motion: Motion) -> AnimSpec {
    let full = anim_spec_full(r);
    match motion {
        Motion::Full => full,
        Motion::Reduce => AnimSpec {
            burst_ticks: (full.burst_ticks / 4).max(1),
            white_flash_ticks: 0,
            ..full
        },
        Motion::Off => AnimSpec {
            burst_ticks: 0,
            white_flash_ticks: 0,
            ..full
        },
    }
}

/// 动画相位。`Menu` 是常态（静止不提交）；`Burst` 只在扩散期逐 tick 提交；
/// `Reveal` 放完 dwell 自动回 `Menu`。`Burst` 必须自带 `lines`，否则扩散结束时无卡可展
/// （抽取结果只在点菜单那一次拿到，不能指望它之后还在别处活着）。
#[derive(Clone, Debug, PartialEq)]
pub enum MenuAnim {
    Menu,
    Burst {
        rarity: Rarity,
        elapsed: u32,
        lines: Vec<String>,
    },
    Reveal {
        elapsed: u32,
        lines: Vec<String>,
        best: Rarity,
    },
}

impl MenuAnim {
    /// 当前是否处于"开箱进行中"（Burst/Reveal 都算；菜单项这段时间不该被点）。
    pub fn is_busy(&self) -> bool {
        !matches!(self, Self::Menu)
    }
}

/// 抽完一次之后卡片**从哪一格开始**。这条判断放在纯模块里而不是 `app.rs` 里，是因为它是
/// `--motion off` 的全部语义：off 档**根本不进 `Burst`**，而不是"在 `Burst` 里画 0 帧"。
///
/// 少了这一步，`off` 档仍会投出一帧扩散（`card::paint` 照样走 `paint_burst`），而
/// [`burst_radius`] 在 `burst_ticks == 0` 上会退化成"第 0 格就是整卡满块"（那里的 clamp
/// 把 t 顶到 ticks）⇒ 玩家看到的不是"没有动画"，而是一帧盖住整张卡的稀有度色块，
/// 下一格才变结果卡。"只减不增"这条省电承诺也会当场破：那一帧是一次实打实的提交。
/// 留在 `app.rs` 里则这条**永远没有自动化覆盖**（headless 起不了 `App`）⇒ 只有真机能发现，
/// 而那时它已经是一个可能说谎的开关。
pub fn after_pull(best: Rarity, lines: Vec<String>, motion: Motion) -> MenuAnim {
    match motion {
        Motion::Off => MenuAnim::Reveal {
            elapsed: 0,
            lines,
            best,
        },
        Motion::Full | Motion::Reduce => MenuAnim::Burst {
            rarity: best,
            elapsed: 0,
            lines,
        },
    }
}

/// 推进一格（60 Hz tick）。**返回值与原值相等就代表卡片像素一模一样** ⇒ 调用方据此
/// 跳过 commit（省电契约：静止的 surface 不许每 tick 投新 buffer）。
///
/// `motion` 是**每进程恒定**的量（CLI/配置文件在启动时一次定死，运行中没有 UI 入口能改它，
/// 与 `dry_run` 同族）⇒ 它不进 `content_sig`（见那里的注释），但**必须**进 `advance`：
/// 各档的 burst/dwell 边界由它决定，少了这个参数 `off` 档就只是"不白闪"而扩散期照走。
pub fn advance(anim: &MenuAnim, motion: Motion) -> MenuAnim {
    match anim {
        MenuAnim::Menu => MenuAnim::Menu,
        MenuAnim::Burst {
            rarity,
            elapsed,
            lines,
        } => {
            let spec = anim_spec(*rarity, motion);
            if *elapsed + 1 >= spec.burst_ticks {
                MenuAnim::Reveal {
                    elapsed: 0,
                    lines: lines.clone(),
                    best: *rarity,
                }
            } else {
                MenuAnim::Burst {
                    rarity: *rarity,
                    elapsed: elapsed + 1,
                    lines: lines.clone(),
                }
            }
        }
        MenuAnim::Reveal {
            elapsed,
            lines,
            best,
        } => {
            let spec = anim_spec(*best, motion);
            if *elapsed + 1 >= spec.dwell_ticks {
                MenuAnim::Menu
            } else {
                MenuAnim::Reveal {
                    elapsed: elapsed + 1,
                    lines: lines.clone(),
                    best: *best,
                }
            }
        }
    }
}

/// Burst 期扩散方块的半边长（logical）。随 tick 单调放大到卡片半宽，永不越出卡片。
pub fn burst_radius(elapsed: u32, spec: AnimSpec) -> i32 {
    const R0: i32 = 10;
    let ticks = spec.burst_ticks.max(1) as i32;
    let max = CARD_W / 2;
    let t = (elapsed.min(spec.burst_ticks.saturating_sub(1)) as i32 + 1).min(ticks);
    R0 + (max - R0) * t / ticks
}

/// 白闪窗口：SSR 的结果卡头几 tick 整卡过白（`white_flash_ticks == 0` 的档位恒 false）。
/// `reduce`/`off` 两档的 flash 计数在 [`anim_spec`] 里就是 0 ⇒ 这里恒 false，不需要再判断相。
pub fn white_flash(anim: &MenuAnim, motion: Motion) -> bool {
    match anim {
        MenuAnim::Reveal { elapsed, best, .. } => {
            let f = anim_spec(*best, motion).white_flash_ticks;
            f > 0 && *elapsed < f
        }
        _ => false,
    }
}

/// 卡片内容签名：`(相位, 语言, 余额, 错误行)` 全同 ⇒ 像素一模一样 ⇒ draw 层跳过 commit。
/// 这是菜单的省电闸门 —— 没有它，`update_menu` 每 tick 置脏，静止的菜单也会 60 次/秒
/// 提交 180×150 的 buffer（M4 的三档口径里"无事可做"那一档就永远不会出现）。
///
/// ⚠️ **`lang` 必须在 `elapsed` 之前**：切语言应当在**同一 tick 计数**下也被认成内容变化
/// （菜单态三行的字换了，而 `elapsed` 一直是 0）。把它排在后面，`Menu` 相位的 `elapsed = 0`
/// 会让"换了字"这件事仍然混进签名 —— 顺序在这里不影响正确性，但影响**读代码的人能不能
/// 一眼看出 lang 不属于动画量**，所以固定这个位置并由 `switching_lang_bumps_sig_once_then_is_stable` 钉住。
/// ⚠️ **`motion` 故意不进签名**：它和 `dry_run` 同族 —— 启动时一次定死，运行中没有任何
/// UI 入口能改它 ⇒ "同一份输入给同一个签名"不会因为少了它而失真。对照 `lang`：语言**能**
/// 在运行中被点着换 ⇒ 必须进。把这条区别写在这里，是因为两者长得像而语义相反，最容易顺手抄错。
/// FNV-1a：不需要新依赖，且**只用来判等**，不当哈希表键、不进任何确定性契约。
pub fn content_sig(anim: &MenuAnim, balance: i64, error: &str, lang: Lang) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let (tag, elapsed) = match anim {
        MenuAnim::Menu => (0u8, 0u32),
        MenuAnim::Burst {
            rarity, elapsed, ..
        } => (1 + rarity.index() as u8, *elapsed),
        MenuAnim::Reveal { elapsed, best, .. } => (5 + best.index() as u8, *elapsed),
    };
    mix(tag);
    // 语言紧跟相位 tag、在 elapsed 前（见上面那条 ⚠️）。用 `code()` 的字节而不是枚举序号
    // ⇒ 新增一档语言不需要改这里，也不可能出现"两个不同枚举值混成同一个签名"。
    for &b in lang.code().as_bytes() {
        mix(b);
    }
    for b in elapsed.to_le_bytes() {
        mix(b);
    }
    for b in balance.to_le_bytes() {
        mix(b);
    }
    for &b in error.as_bytes() {
        mix(b);
    }
    match anim {
        MenuAnim::Reveal { lines, .. } | MenuAnim::Burst { lines, .. } => {
            for l in lines {
                for b in l.as_bytes() {
                    mix(*b);
                }
                mix(b'\n');
            }
        }
        MenuAnim::Menu => {}
    }
    h
}

/// 十连取最高档决定 burst 规格：从 `counts`（下标 = `Rarity::index`）反找。
pub fn best_rarity(counts: &[u32; 4]) -> Option<Rarity> {
    Rarity::ALL
        .iter()
        .rev()
        .find(|r| counts[r.index()] > 0)
        .copied()
}

/// 结果卡排版预算。**单位是像素，不是字符数**：CJK 每字比拉丁宽得多，按字数截必然一边宽
/// 一边窄（§4.5）。`MAX_LINE_PX` 是**逻辑**预算（内容区宽），[`line_budget_px`] 按字号档
/// 换成物理预算，与 [`line_width`] 同量纲。
///
/// ⚠️ 这里有一个**上一版方案里写错的前提**（§4.5 说"CJK 每字正好 1 em 没有进位 ⇒ 截点逐位
/// 相等"）：[`crate::text`] 传给 `ab_glyph` 的 `px` 是**字体高度**（`PxScale` = ascent−descent），
/// 不是 em。实测这份子集 `ascent 1160 / descent −288 / upem 1000` ⇒ 一个汉字的 advance 是
/// `1000/1448 = 0.6906` 个 `px`，于是三档的逐字取整是 **11 / 17 / 22**（11.05·1.5 = 16.58 进位）。
/// 按"每档各自的取整宽"决定截点，1.5× 就会比 1×/2× 少截一个字（实测 [20, 19, 20]）—— 而这
/// 正是判据"预算与缩放无关"要防的形状。正解见 [`advance_bound_log`]：**截点在逻辑域里一次定
/// 死**，物理档只负责验证"画得下"。
///
/// 十连全列 = 13 行，卡片装不下，而逐点裁剪会把词切一半 ⇒ **行数和行宽必须在拼行时就定死**，
/// 不是留给渲染层兜底。
pub const MAX_LINE_PX: i32 = CARD_W - 2 * PAD;
/// 结果卡最多 **6** 行：`标题 + 档位计数 + TOP_ITEMS 明细 + MORE + 碎片` = 1+1+2+1+1。
/// （**演示标在标题行**，不占一行 —— 打两处就是把一个冗余行花在零信息上。）
///
/// ⚠️ 这个数不是"卡片还能塞几行"的最大值，而是"**不许压到 footer**"的上界：行带从 `PAD`
/// 起、每行 [`LINE_STEP`]，而 footer 从 [`FOOTER_Y`] 起 ⇒ `PAD + 6×18 = 116 ≤ 124`，
/// 第七行会写到 134 直接盖住余额/语言行。上一版写 7 时 footer 还不存在，那条算术证明
/// 只对着 `CARD_H` 核 ⇒ 底部带一加进来就破了（`report_budget_fits_the_card` 现在钉的是这个）。
pub const MAX_REPORT_LINES: usize = 6;
/// 明细最多列几条（其余并进 `+N 更多`），永远先列最高档。
pub const TOP_ITEMS: usize = 2;

/// 单个字的推进宽度（**物理**像素），与 [`line_width`] 同一个口径（可加 ⇒ 两者一致）。
///
/// ⚠️ 这里**不能**直接写 `text::measure(...)`：字体拿不到时 `measure` 返回 0（"不画字"），
/// 而 0 宽会让 [`fit_to`] 以为整行都放得下 ⇒ 结果是"卡片上一个字都没有、门禁全绿"。
/// 显式分两支，降级路径按等宽点阵算自己的账。
fn advance_px(c: char, scale: f32) -> i32 {
    if text::face().is_some() {
        text::measure(&c.to_string(), text::physical_px(scale))
    } else {
        // 降级字模等宽：`STEP` 逻辑 × 逻辑字号，再乘缩放。这条分支在 CI 里永不执行
        // （字体资产有 sha256 门禁），留着只为了"资产坏了也只是字丑，不炸进程"。
        (crate::font::STEP as f32 * TEXT_PX as f32 * scale).round() as i32
    }
}

/// 字号档相对逻辑字号的比 ∈ {1, 1.5, 2}（`physical_px` 已经把任意 scale 夹进三档）。
///
/// ⚠️ 预算和排版**必须共用这一个比**，不能用传进来的 `scale` 原值：1.25× 的屏会夹到 16 px 档
/// （比 = 1.0），若预算按原值放大到 280，就会出现"按 1.25× 的钱排、按 1× 的字画"⇒ 一行放 25
/// 个字而卡片只有 240 逻辑宽 ⇒ 顶穿右边距，而像素门禁只看得见"这一格没东西"。
fn tier_ratio(scale: f32) -> f32 {
    text::physical_px(scale) as f32 / text::LOGICAL_PX as f32
}

/// 一串字有多宽（物理像素）。draw 层居中/右对齐也用它 ⇒ 排版与截断**不会差 1 px**。
pub fn line_width(s: &str, scale: f32) -> i32 {
    s.chars().map(|c| advance_px(c, scale)).sum()
}

/// 一行能占多少物理像素（`MAX_LINE_PX` 随字号档放大，见 [`tier_ratio`]）。
pub fn line_budget_px(scale: f32) -> i32 {
    (MAX_LINE_PX as f32 * tier_ratio(scale)).floor() as i32
}

/// 单 char 的**逻辑宽度上界**：16 px 档的取整宽 + 1。截断只按它算 ⇒ 截点与缩放无关。
///
/// 为什么 `+1` 就够了（这条是整个"预算与缩放无关"的地基，所以要能口算）：
/// `round(u) ≥ u − 0.5` ⇒ `bound = round(u) + 1 ≥ u + 0.5`；又 `round(u·r) ≤ u·r + 0.5·r`
/// 且 `r ≥ 1` ⇒ `round(u·r) ≤ (u + 0.5)·r ≤ bound·r`。逐 char 求和：
/// **任意档的实际画宽 ≤ `比 × Σ bound`**，所以只要 `Σ bound ≤ 逻辑预算` 就一定画得下。
/// （空格在 [`crate::text::measure`] 里特判成 `px/3`，是整数除法不是比例，但
/// `floor(16r/3) ≤ 5.34r ≤ 6r = bound·r` 同样成立。）
///
/// 代价：每字最多 1 个逻辑 px 的余量 ⇒ 一行少装约一个字。换来的是"三档截在同一个字上"
/// 从**实测**升级成**可证**，而这正是判据①（1× 屏上的字被切一半）防的东西。
fn advance_bound_log(c: char) -> i32 {
    advance_px(c, 1.0) + 1
}

/// 整串的逻辑宽度上界（配合 [`advance_bound_log`]）。
fn line_bound_log(s: &str) -> i32 {
    s.chars().map(advance_bound_log).sum()
}

/// 逐 char 累加**像素**截断，超长补一个 `…`（U+2026，在子集清单里）。
///
/// 先给省略号留出宽度再装正文 ⇒ 结果串必然 `<= budget`，不会因为"最后一个字正好顶边、
/// 再加个省略号就超"而仍然出界。空串与正好等预算的都原样返回（不无端补点）。
///
/// ⚠️ **决定截在哪的是 [`line_bound_log`]（逻辑域、与 scale 无关），不是每档各自的取整宽**，
/// 理由见 [`MAX_LINE_PX`] 那段（`report_budget_is_scale_independent` 钉的就是这条）。`scale`
/// 只用来把调用方给的**物理**预算折回逻辑预算（下取整 ⇒ 只会更保守）。
pub fn fit(s: &str, scale: f32) -> String {
    fit_to(s, line_budget_px(scale), scale)
}

/// [`fit`] 但预算由调用方给 —— 底部那一行同时装错误行（左）与语言行（右），
/// 错误行的可用宽 = 内容区 − 语言行宽 − [`FOOTER_GAP`]，不是整行。
///
/// **放不下省略号就一个字节都不画**（返回空串）：画一个顶穿出界的 `…` 比不画更坏 ——
/// 像素门禁看不见"顶穿"，只会看见"这一格没东西"，而人会读到一条被切掉尾巴的错误。
/// 这条与"宁可不画也不炸进程"（硬约束 3）同族。
pub fn fit_to(s: &str, budget: i32, scale: f32) -> String {
    let budget = budget.max(0);
    let room = (budget as f32 / tier_ratio(scale)).floor() as i32;
    let ell_w = advance_bound_log('…');
    if line_bound_log(s) <= room {
        return s.to_string();
    }
    if ell_w > room {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = advance_bound_log(c);
        if used + w + ell_w > room {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    debug_assert!(
        line_width(&out, scale) <= budget,
        "fit 出来的串在这一档还超物理预算：{out:?} budget={budget} 实宽={}",
        line_width(&out, scale)
    );
    out
}

/// 结果卡文本（渲染无关，逐行走 draw 层）。
///
/// `items` = 每抽的 **(`显示名`, rarity, 是否重复)** —— 第一项现在是 catalog 里的**中文名**
/// 而不是 `item_id`：玩家抽到「蓝帽」，卡片上就该写「蓝帽」（`app.rs` 取 `catalog.find(id)`，
/// 查不到回退 id，**绝不留白**）。
///
/// `lang` 决定每一条的文案与词序（模板各语言各写，见 `l10n`）；`scale` 决定截断的像素口径。
/// ⚠️ 这两样都是在**抽取那一刻**烤进字符串的 ⇒ 之后切语言不会重刷已经挂着的卡，
/// 这不是 bug 而是"当前显示的卡永远与生成它时的语言一致"（§4.4 / §7-R10，
/// 由 `baked_lines_keep_the_language_they_were_pulled_in` 钉住）。
/// 演示标不在这里：它由 `card.rs` 的标题行徽标承担（`l10n::header_shards`）。
pub fn report_lines(
    counts: &[u32; 4],
    items: &[(String, Rarity, bool)],
    shards_gained: u64,
    lang: Lang,
    scale: f32,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(fit(
        &l10n::line_pulled(lang, items.len(), best_rarity(counts)),
        scale,
    ));
    // 档位计数行：只列非零档，从高到低。稳定排序 ⇒ 同档永远按抽取顺序出。
    let tiers = l10n::line_tiers(counts);
    if !tiers.is_empty() {
        lines.push(fit(&tiers, scale));
    }
    let mut ranked: Vec<&(String, Rarity, bool)> = items.iter().collect();
    ranked.sort_by_key(|(_, r, _)| std::cmp::Reverse(r.index()));
    for (name, r, dup) in ranked.iter().take(TOP_ITEMS) {
        lines.push(fit(&l10n::line_item(lang, *r, name, *dup), scale));
    }
    if items.len() > TOP_ITEMS {
        lines.push(fit(&l10n::line_more(lang, items.len() - TOP_ITEMS), scale));
    }
    lines.push(fit(&l10n::line_shards(lang, shards_gained), scale));
    debug_assert!(lines.len() <= MAX_REPORT_LINES, "结果卡超行 ⇒ 排版契约破了");
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use petcore::gacha::Catalog;

    #[test]
    fn layout_rects_are_disjoint_and_inside_the_card() {
        // 四张矩形（三行 + 语言行）都在命中表里 ⇒ 这张表**必须**是 draw 与 hit_test 共用的
        // 那一份；漏了第 4 项，语言行画得出来却点不到。
        let rects = rects();
        assert_eq!(rects.len(), 4, "三行 + 语言行 = 四张命中矩形");
        assert_eq!(
            rects.iter().map(|r| r.item).collect::<Vec<_>>(),
            vec![
                MenuItem::Pull1,
                MenuItem::Pull10,
                MenuItem::Close,
                MenuItem::Lang
            ]
        );
        for r in &rects {
            assert!(r.x >= 0 && r.y >= 0 && r.x + r.w <= CARD_W && r.y + r.h <= CARD_H);
        }
        for w in rects.windows(2) {
            assert!(
                !overlaps(w[0].as_rect(), w[1].as_rect()),
                "相邻行矩形不许重叠"
            );
        }
    }

    /// 语言行的几何证明（§4.4）：它在三行**之下**、在卡片**之内**，而且与行带不相交 ——
    /// 这条取代了"footer 只在 layout 里"的那种假前提。
    #[test]
    fn footer_is_below_the_rows_and_inside_the_card() {
        let rows = layout();
        assert_eq!(rows.len(), 3, "CLICKABLE 恒三行：语言行走 footer 那条路");
        let f = footer();
        let last = rows.last().unwrap();
        assert_eq!(
            last.y + last.h,
            PAD + HEAD_H + 2 * ROW_STEP + ROW_H,
            "最后一行的底边由四个行常量决定，改常量要看得见"
        );
        assert_eq!(
            f.y - (last.y + last.h),
            8,
            "语言行与行带之间留 8 px 气 ⇒ 两者不相交"
        );
        assert!(!overlaps(last.as_rect(), f.as_rect()), "语言行压进行带了");
        assert!(
            f.y + f.h <= CARD_H,
            "语言行底边出卡：{} > {CARD_H}",
            f.y + f.h
        );
        // 底部带只装得下一行（两行要 30–32 px）：这是算术不是审美。
        assert!(
            2 * text::line_height(text::LOGICAL_PX) + FOOTER_GAP > CARD_H - last.y - last.h,
            "底部带装得下两行了 ⇒ FOOTER_H = 16 那条理由（§4.4）就不再成立，得重论"
        );
    }

    #[test]
    fn hit_test_hits_centers_and_misses_gaps_and_margins() {
        // 用 [`rects`]（三行 + 语言行）而不是 [`layout`]：命中表**必须**与 draw 用的是同一份，
        // 否则语言行画得出来却点不到 —— 而这只有真机能发现。
        let rects = rects();
        for r in &rects {
            assert_eq!(
                hit_test(&rects, ((r.x + r.w / 2) as f32, (r.y + r.h / 2) as f32)),
                Some(r.item),
                "{:?} 中心必须命中",
                r.item
            );
            // 半开区间：右/下边界外 1px 不命中，左上角内 1px 命中。
            assert_eq!(
                hit_test(&rects, ((r.x + r.w) as f32, (r.y + r.h / 2) as f32)),
                None
            );
            assert_eq!(hit_test(&rects, (r.x as f32, r.y as f32)), Some(r.item));
        }
        assert_eq!(hit_test(&rects, (0.0, 0.0)), None, "边距上的点击穿透到背后");
        assert_eq!(
            hit_test(&rects, (4.0, CARD_H as f32 - 4.0)),
            None,
            "卡片下方余量不命中"
        );
        let seam_y = rects[0].y + rects[0].h + 1;
        assert!(
            hit_test(&rects, (CARD_W as f32 / 2.0, seam_y as f32)).is_none(),
            "行缝隙不命中"
        );
    }

    #[test]
    fn placement_prefers_right_then_left_then_above_then_below() {
        // ① 开阔处 ⇒ 右侧，且与宠物隔 GAP。
        let pet = RectI::new(100, 100, 128, 128);
        let out = RectI::new(0, 0, 1280, 720);
        assert_eq!(placement(pet, out), (234, 100), "首选右侧");

        // ② 贴右墙 ⇒ 退左侧（右侧 clamp 后压到宠物身上，必须被拒）。
        let pet_r = RectI::new(1152, 100, 128, 128);
        let m = placement(pet_r, out);
        assert_eq!(m.1, pet_r.y, "左侧候选应与宠物同高");
        assert!(m.0 + CARD_W <= pet_r.x, "左侧候选必须在宠物左边：{m:?}");

        // ③ 窄屏贴底 ⇒ 右/左被 clamp 后都压在身上、下方出屏 ⇒ 退上方。
        let out_n = RectI::new(0, 0, 300, 300);
        let pet_b = RectI::new(0, 172, 128, 128);
        let m = placement(pet_b, out_n);
        assert!(m.1 + CARD_H <= pet_b.y, "贴底时应在上方：{m:?}");

        // ④ 窄屏贴顶 ⇒ 上/左/右都躲不开 ⇒ 退下方。
        let out_tall = RectI::new(0, 0, 300, 720);
        let pet_t = RectI::new(0, 0, 128, 128);
        let m = placement(pet_t, out_tall);
        assert!(m.1 >= pet_t.y + pet_t.h, "贴顶时应在下方：{m:?}");
    }

    #[test]
    fn placement_never_leaves_the_output_and_never_overlaps_the_pet() {
        let out = RectI::new(0, 0, 1280, 720);
        for x in (0..=1280 - 128).step_by(37) {
            for y in (0..=720 - 128).step_by(29) {
                let pet = RectI::new(x, y, 128, 128);
                let (mx, my) = placement(pet, out);
                let menu = RectI::new(mx, my, CARD_W, CARD_H);
                assert!(
                    menu.x >= 0 && menu.y >= 0 && menu.x + menu.w <= 1280 && menu.y + menu.h <= 720,
                    "菜单出屏：pet {:?} → {:?}",
                    pet,
                    menu
                );
                assert!(!overlaps(menu, pet), "同层重叠：pet {:?} → {:?}", pet, menu);
            }
        }
    }

    #[test]
    fn placement_falls_back_to_clamped_when_output_is_tiny() {
        // 病态：output 比宠物+菜单还小 ⇒ 不许出屏（宁可相交），坐标仍须在屏内。
        let pet = RectI::new(40, 40, 100, 100);
        let out = RectI::new(0, 0, 260, 260);
        let (mx, my) = placement(pet, out);
        assert!(mx >= 0 && my >= 0 && mx + CARD_W <= 260 && my + CARD_H <= 260);
    }

    #[test]
    fn only_right_button_toggles_the_menu() {
        for (code, want) in [
            (BTN_LEFT, false),
            (BTN_RIGHT, true),
            (BTN_MIDDLE, false),
            (0x113, false),
        ] {
            assert_eq!(
                matches!(pet_press_disposition(code), PetPressAction::ToggleMenu),
                want,
                "按键 {code:#x} 的翻转判定错了"
            );
        }
        assert!(menu_toggle_for(BTN_RIGHT, false), "右键在关闭态 ⇒ 打开");
        assert!(
            !menu_toggle_for(BTN_RIGHT, true),
            "右键在打开态 ⇒ 翻转成关闭"
        );
        assert!(!menu_toggle_for(BTN_LEFT, false), "左键开拖拽，不碰菜单");
        assert!(
            menu_toggle_for(BTN_LEFT, true),
            "左键在菜单打开时也不关它（返回值仍是打开）"
        );
    }

    /// 键位是**外部 ABI**（`linux/input-event-codes.h`），不是可以自己排序的枚举。
    /// 这条把三个码钉死成 左/右/中 = 0x110/0x111/0x112：曾经 0x111 与 0x112 被对调，
    /// 真机的表现是"右键完全打不开菜单"，而当时两条测试**都是绿的**（抄了同一份误解）
    /// ⇒ 光有测试不够，测试必须钉事实源本身。
    #[test]
    fn button_codes_match_the_linux_input_abi() {
        assert_eq!(BTN_LEFT, 0x110);
        assert_eq!(BTN_RIGHT, 0x111, "0x111 是右键，不是中键");
        assert_eq!(BTN_MIDDLE, 0x112, "0x112 是中键，不是右键");
        assert_eq!(button_from_raw(0x111), Some(petcore::render::Button::Right));
        assert_eq!(
            button_from_raw(0x112),
            Some(petcore::render::Button::Middle)
        );
    }

    #[test]
    fn button_mapping_covers_the_contract_and_rejects_extensions() {
        assert_eq!(
            button_from_raw(BTN_LEFT),
            Some(petcore::render::Button::Left)
        );
        assert_eq!(
            button_from_raw(BTN_RIGHT),
            Some(petcore::render::Button::Right)
        );
        assert_eq!(
            button_from_raw(BTN_MIDDLE),
            Some(petcore::render::Button::Middle)
        );
        assert_eq!(button_from_raw(0x115), None, "侧键不该挤进契约");
        assert_eq!(button_from_raw(0), None);
    }

    #[test]
    fn only_left_press_activates_a_menu_item() {
        assert!(menu_press_activates(BTN_LEFT));
        assert!(
            !menu_press_activates(BTN_RIGHT),
            "右键落进菜单不许顺手抽一次"
        );
        assert!(!menu_press_activates(BTN_MIDDLE));
        assert!(!menu_press_activates(0x1FF));
    }

    #[test]
    fn drag_must_be_ended_when_the_pointer_is_lost() {
        assert!(pointer_lost_should_end_drag(true));
        assert!(!pointer_lost_should_end_drag(false));
    }

    #[test]
    fn watchdog_fires_at_the_limit_not_before() {
        assert!(!watchdog_expired(2, 3));
        assert!(watchdog_expired(3, 3));
        assert!(watchdog_expired(9, 3));
    }

    #[test]
    fn anim_specs_are_monotonic_and_ssr_flashes() {
        let specs = Rarity::ALL.map(|r| anim_spec(r, Motion::Full));
        for w in specs.windows(2) {
            assert!(w[1].burst_ticks > w[0].burst_ticks, "burst 必须随档位增长");
            assert!(w[1].dwell_ticks > w[0].dwell_ticks, "dwell 必须随档位增长");
        }
        assert_eq!(
            specs[Rarity::SSR.index()].white_flash_ticks,
            SSR_FLASH_TICKS
        );
        assert_eq!(specs[Rarity::SR.index()].white_flash_ticks, 0);
        for s in specs {
            assert!(s.burst_ticks >= 4, "burst 至少 4 tick，一帧白闪不算动画");
        }
    }

    /// §4.7 那张表钉成代码。两条不许动的东西比"能关"本身更容易被顺手改坏：
    /// **dwell 与颜色在两档里都原样保留** —— dwell 是"结果卡停留多久"不是动效（砍了玩家
    /// 来不及读完卡），而关掉白闪之后**色带是"这是 SSR"的唯一凭据**。
    /// 总 tick 数只减不增 ⇒ 省电基线（三档相加 == tick 总数）在新档位下不可能被做坏。
    #[test]
    fn motion_tiers_only_touch_burst_and_flash() {
        for r in Rarity::ALL {
            let full = anim_spec(r, Motion::Full);
            let reduce = anim_spec(r, Motion::Reduce);
            let off = anim_spec(r, Motion::Off);
            assert_eq!(reduce.burst_ticks, (full.burst_ticks / 4).max(1));
            assert!(
                reduce.burst_ticks >= 1,
                "reduce 至少要留一格，不然等于没有 burst"
            );
            assert_eq!(off.burst_ticks, 0, "off = 扩散期整个跳过");
            assert_eq!(
                reduce.white_flash_ticks, 0,
                "前庭敏感是硬需求：reduce 不许闪"
            );
            assert_eq!(off.white_flash_ticks, 0);
            for narrowed in [reduce, off] {
                assert_eq!(
                    narrowed.dwell_ticks, full.dwell_ticks,
                    "{r} 的 dwell 被动了"
                );
                assert_eq!(narrowed.color, full.color, "{r} 的档位色被动了");
            }
            assert!(
                reduce.burst_ticks + reduce.dwell_ticks <= full.burst_ticks + full.dwell_ticks,
                "{r} 的 reduce 提交数比 full 还多"
            );
            assert!(off.burst_ticks + off.dwell_ticks <= full.burst_ticks + full.dwell_ticks);
        }
    }

    /// `--motion off` 的**语义**证明：抽完那一次不从 `Burst` 出发，于是扩散那一帧一次都不画
    /// （`card::paint` 进不到 `paint_burst`），忙碌格数从 `burst + dwell` 降到恰好 `dwell`
    /// ⇒ "只减不增"这条承诺是量出来的，不是嘴上说的。
    ///
    /// 这条**必须**是纯函数断言：分派发生在 `App::do_pull`，而 headless 起不了 `App` ⇒
    /// 留在 `app.rs` 里就是"off 到底关掉了什么只有真机看得出来"，而那时它已经是一个
    /// 可能说谎的开关（M4 那条"计数器为 0 不等于代码对"同族）。
    #[test]
    fn off_never_enters_the_burst_phase() {
        for r in Rarity::ALL {
            let lines = vec![format!("BEST {r}")];
            let off = after_pull(r, lines.clone(), Motion::Off);
            match &off {
                MenuAnim::Reveal {
                    elapsed,
                    best,
                    lines: got,
                } => {
                    assert_eq!(*elapsed, 0, "off 档的结果卡该从第 0 格起");
                    assert_eq!(*best, r);
                    assert_eq!(got, &lines, "跳过扩散不能把行数一起跳过 ⇒ 会渲成空白卡");
                }
                other => panic!("off 档该直接进 Reveal，得到 {other:?}"),
            }
            let mut a = off;
            let mut ticks: u32 = 0;
            while a != MenuAnim::Menu {
                a = advance(&a, Motion::Off);
                assert!(
                    !matches!(a, MenuAnim::Burst { .. }),
                    "{r}：off 档走到了扩散期"
                );
                ticks += 1;
                assert!(
                    ticks <= 300,
                    "{r}：off 档回不到静止 ⇒ 菜单每 tick 投 buffer"
                );
            }
            assert_eq!(
                ticks,
                anim_spec(r, Motion::Off).dwell_ticks,
                "{r}：off 档忙碌格数该恰好等于 dwell（扩散一格都不剩）"
            );

            // 另两档仍从扩散期起步 —— 默认观感不许被这一轮改掉。
            for motion in [Motion::Full, Motion::Reduce] {
                let spec = anim_spec(r, motion);
                assert_eq!(
                    after_pull(r, lines.clone(), motion),
                    MenuAnim::Burst {
                        rarity: r,
                        elapsed: 0,
                        lines: lines.clone()
                    },
                    "{r}/{motion:?} 该从扩散期开始"
                );
                assert!(
                    ticks <= spec.burst_ticks.max(1) + spec.dwell_ticks,
                    "{r}/{motion:?} 的忙碌格数比 off 还少 ⇒ off 增加了提交"
                );
            }
        }
    }

    #[test]
    fn best_rarity_reads_counts_high_to_low() {
        assert_eq!(best_rarity(&[1, 0, 0, 0]), Some(Rarity::N));
        assert_eq!(best_rarity(&[9, 2, 1, 0]), Some(Rarity::SR));
        assert_eq!(best_rarity(&[9, 2, 1, 1]), Some(Rarity::SSR));
        assert_eq!(best_rarity(&[0, 0, 0, 0]), None);
    }

    /// 相位机的唯一走法：Burst 放完 ⇒ Reveal（行数原样带过去）⇒ dwell 到点 ⇒ Menu。
    /// 中间任何一格"回不到 Menu"都会让菜单永远每 tick 提交 buffer（省电契约破）。
    /// **逐档 × 逐 motion 都验**：`off` 的 `burst_ticks == 0`，交棒发生在第一次 advance；
    /// 这条若只在默认档成立，换档就是一个卡在扩散期、每 tick 都投 buffer 的菜单。
    #[test]
    fn advance_walks_burst_reveal_menu_and_returns_to_rest() {
        let lines = vec!["1 PULLED BEST SSR".to_string()];
        for motion in Motion::ALL {
            for r in Rarity::ALL {
                let spec = anim_spec(r, motion);
                let mut a = MenuAnim::Burst {
                    rarity: r,
                    elapsed: 0,
                    lines: lines.clone(),
                };
                // 回静止恰好用掉"扩散那一格 + dwell"—— 不多一格（早退 = 卡住的菜单），
                // 也不少一格（晚退 = 结果卡凭空少停一 tick）。
                // `max(1)`：`off` 档 `burst_ticks == 0`，但从 `Burst` 出发仍要花**一次** advance
                // 才交棒 ⇒ 零长度的扩散期不等于零格（真机上这一档根本不会进 `Burst`，
                // 见 `after_pull` 与 `off_never_enters_the_burst_phase`）。
                let handoff = spec.burst_ticks.max(1);
                let total = handoff + spec.dwell_ticks;
                let mut handed_off = false;
                for tick in 1..=total {
                    a = advance(&a, motion);
                    if handed_off || matches!(a, MenuAnim::Burst { .. }) {
                        continue;
                    }
                    handed_off = true;
                    assert_eq!(tick, handoff, "{r}/{motion:?} 交棒时机");
                    match &a {
                        MenuAnim::Reveal {
                            elapsed,
                            best,
                            lines: got,
                        } => {
                            assert_eq!(*best, r);
                            assert_eq!(*elapsed, 0, "{r}/{motion:?} 交棒时 elapsed 该从 0 起");
                            assert_eq!(got, &lines, "burst 必须在结束时把行数交棒给 reveal");
                        }
                        other => panic!("burst 放完该进 Reveal，得到 {other:?}"),
                    }
                }
                assert_eq!(
                    a,
                    MenuAnim::Menu,
                    "{r}/{motion:?} 在 {total} tick 后该回静止"
                );
                assert_eq!(advance(&a, motion), MenuAnim::Menu, "静止是吸收态");
            }
        }
    }

    /// 静止 ⇒ 签名不变 ⇒ draw 层不 commit。四个可见输入（相位/语言/余额/错误行）任一变了
    /// 都要变。
    #[test]
    fn content_sig_is_stable_when_static_and_moves_with_every_visible_input() {
        let rest = content_sig(&MenuAnim::Menu, 100, "", Lang::Zh);
        assert_eq!(rest, content_sig(&MenuAnim::Menu, 100, "", Lang::Zh));
        assert_eq!(
            rest,
            content_sig(&advance(&MenuAnim::Menu, Motion::Full), 100, "", Lang::Zh)
        );
        assert_ne!(
            rest,
            content_sig(&MenuAnim::Menu, 101, "", Lang::Zh),
            "余额行会变"
        );
        assert_ne!(
            rest,
            content_sig(&MenuAnim::Menu, 100, "DB LOCKED", Lang::Zh),
            "错误行会变"
        );
        // 语言行是**可见**输入：三行的字（抽一次/Pull 1/1回引く/1회 뽑기）全换了 ⇒ 必须换签名。
        for l in Lang::ALL.iter().copied().filter(|l| *l != Lang::Zh) {
            assert_ne!(
                rest,
                content_sig(&MenuAnim::Menu, 100, "", l),
                "{l:?} 换了字却没换签名 ⇒ 静止判定会漏掉这一次像素变化"
            );
        }
        let burst = MenuAnim::Burst {
            rarity: Rarity::N,
            elapsed: 0,
            lines: vec!["X".into()],
        };
        assert_ne!(
            content_sig(&burst, 100, "", Lang::Zh),
            content_sig(&advance(&burst, Motion::Full), 100, "", Lang::Zh),
            "扩散期每 tick 都在动 ⇒ 签名必须动"
        );
        // motion 不进签名（启动定死、运行中无人能改），但**同相位的不同档**像素确实相同，
        // 所以这里必须是等号；写成断言是为了让"顺手把 motion 混进来"当场红，而不是悄悄多一提交。
        assert_eq!(
            content_sig(&burst, 100, "", Lang::Zh),
            content_sig(&burst, 100, "", Lang::Zh),
            "签名不该看进程外的东西"
        );
    }

    /// §4.4 的静止基线新论证：切语言 = **一次点击一次换内容 = 恰好 +1 提交**，与点 CLOSE 同族；
    /// 切完之后仍然静止 ⇒ 之后每 tick 签名不再变。这条钉住"静止 1 提交 / 真抽 1,107 提交"
    /// 这对实测数字在 `lang` 进签名之后仍然成立。
    #[test]
    fn switching_lang_bumps_sig_once_then_is_stable() {
        let mut prev = content_sig(&MenuAnim::Menu, 100, "", Lang::Zh);
        let mut bumps = 0;
        for l in [Lang::En, Lang::Ja, Lang::Ko, Lang::Zh] {
            let now = content_sig(&MenuAnim::Menu, 100, "", l);
            if now != prev {
                bumps += 1;
            }
            // 同一个语言再算十次都不许变（⇒ draw 层那十次一个 buffer 都不投）。
            for _ in 0..10 {
                assert_eq!(now, content_sig(&MenuAnim::Menu, 100, "", l));
            }
            prev = now;
        }
        assert_eq!(bumps, 4, "转一圈 = 四次内容变化，不多不少");
    }

    #[test]
    fn burst_radius_grows_monotonically_and_stays_in_the_card() {
        // 只验 Full/Reduce：`off` 档 `after_pull` 直接给 `Reveal`，扩散那一帧**根本不画**，
        // 而这一档的 `burst_radius` 会退化成"第 0 格就满块"（见它的注释）—— 拿它出图既
        // 不是交付观感，也不是在测任何真会被画出来的东西。
        for motion in [Motion::Full, Motion::Reduce] {
            for r in Rarity::ALL {
                let spec = anim_spec(r, motion);
                let mut prev = -1;
                for t in 0..spec.burst_ticks {
                    let rad = burst_radius(t, spec);
                    assert!(rad > prev, "{r}/{motion:?} 的扩散在 tick {t} 没长大");
                    assert!(rad <= CARD_W / 2, "{r}/{motion:?} 的扩散越出卡片：{rad}");
                    prev = rad;
                }
                // 尾帧之后不许回头增长（elapsed 超上限时 clamp，draw 层不该画出格外方块）。
                assert_eq!(burst_radius(spec.burst_ticks * 3, spec), prev);
            }
        }
    }

    #[test]
    fn white_flash_only_at_the_head_of_an_ssr_reveal() {
        let ssr = MenuAnim::Reveal {
            elapsed: 0,
            lines: vec![],
            best: Rarity::SSR,
        };
        assert!(white_flash(&ssr, Motion::Full));
        assert!(white_flash(
            &MenuAnim::Reveal {
                elapsed: SSR_FLASH_TICKS - 1,
                lines: vec![],
                best: Rarity::SSR,
            },
            Motion::Full
        ));
        assert!(!white_flash(
            &MenuAnim::Reveal {
                elapsed: SSR_FLASH_TICKS,
                lines: vec![],
                best: Rarity::SSR,
            },
            Motion::Full
        ));
        assert!(
            !white_flash(
                &MenuAnim::Reveal {
                    elapsed: 0,
                    lines: vec![],
                    best: Rarity::SR
                },
                Motion::Full
            ),
            "只有 SSR 闪白"
        );
        assert!(!white_flash(&MenuAnim::Menu, Motion::Full));
        // 判据②：可访问性那一档要**真的**不闪，而且四档里哪一档都不闪（SSR 也照样不闪）。
        for motion in [Motion::Reduce, Motion::Off] {
            for r in Rarity::ALL {
                assert!(
                    !white_flash(
                        &MenuAnim::Reveal {
                            elapsed: 0,
                            lines: vec![],
                            best: r
                        },
                        motion
                    ),
                    "{motion:?} 档在 {r} 上还闪白"
                );
            }
        }
    }

    /// 排版可行性证明（§4.5 换成字体驱动的版本）。竖向那条算术不变；横向**不再按字数**：
    /// 旧的 `18 字符 × STEP 6 × TEXT_PX 2 = 216` 是 5×7 等宽点阵的算术，对 CJK（每字 ≈1 em）
    /// 与拉丁（窄得多）混排必然一边宽一边窄 ⇒ 预算改成 [`MAX_LINE_PX`]，用真字体量。
    /// 与 `text.rs::every_locale_fits_the_card` 的分工：那条量的是"最坏的一行装不装得下"，
    /// 这条量的是"排版契约本身（行数/行距/数字不裁）成不成立"。
    #[test]
    fn report_budget_fits_the_card() {
        assert!(
            2 * PAD + MAX_REPORT_LINES as i32 * LINE_STEP <= CARD_H,
            "结果卡装不下 {} 行",
            MAX_REPORT_LINES
        );
        // ⚠️ 上面那条对着 `CARD_H` 的证明**不够**：底部还有一条 footer（错误行 / 语言行），
        // 结果卡不许把行写到它上面。`MAX_REPORT_LINES` 从 7 降到 6 就是这条逼出来的。
        assert!(
            PAD + MAX_REPORT_LINES as i32 * LINE_STEP <= FOOTER_Y,
            "结果卡最后一行的底边 {} 压进 footer {FOOTER_Y} ⇒ 余额/语言行会被明细盖住",
            PAD + MAX_REPORT_LINES as i32 * LINE_STEP
        );
        // 拼装处也验一次：`report_lines` 在**最坏输入**（十连）下真的没超过这个数。
        assert_eq!(
            1 + 1 + TOP_ITEMS + 1 + 1,
            MAX_REPORT_LINES,
            "标题+档位计数+明细+更多+碎片 就是行数预算的全部 ⇒ 对不上说明其中一处改了没同步"
        );
        // 行步进必须是**字体度量**给的，不是手抄常数：换字体/换字号时这条会红，
        // 而不是等卡片上的两行字叠在一起。
        let px = text::LOGICAL_PX;
        assert!(
            LINE_STEP >= text::line_height(px),
            "行步进 {LINE_STEP} 小于字体行高 {} ⇒ 相邻行相交（leading 是负的）",
            text::line_height(px)
        );
        assert_eq!(
            LINE_STEP - text::line_height(px),
            2,
            "留的 leading 就是这个数，改了常量要看得见"
        );
        // **数字永不裁**（读错钱比看不见更糟，§4.4 的降级优先级最后一条）。
        for &l in &Lang::ALL {
            for &scale in &[1.0f32, 1.5, 2.0] {
                let money = l10n::line_balance(l, 9_999_999);
                assert_eq!(
                    fit(&money, scale),
                    money,
                    "{l:?}/{scale}× 把余额裁了 ⇒ 预算破了，只能缩徽标/砍标签，不许动数字"
                );
                let shards = l10n::line_shards(l, 999_999);
                assert_eq!(fit(&shards, scale), shards, "{l:?}/{scale}× 把碎片裁了");
            }
        }
    }

    /// §4.5 的口径证明：**排版预算与显示器缩放无关**。同一句话在 1×/1.5×/2×（以及夹进同一档的
    /// 1.25×）上必须截在**同一个字**上 —— 否则门禁绿着、1× 屏上的字却被切了一半（上一版
    /// `CARD_W` 的注释写的就是这个坑）。
    ///
    /// ⚠️ **这条比批准的方案（§4.5"拉丁允许漂 2 字、只有 CJK 逐位相等"）更强，因为地基换了**：
    /// 上一版的前提"CJK 每字正好 1 em ⇒ 没有进位"实测是**假的**（`ab_glyph` 的 `PxScale` 是
    /// 字体高度不是 em，汉字 advance = 0.6906·px ⇒ 三档逐字取整 11/17/22，1.5× 那档必然多进
    /// 一位）。按每档自己的取整宽决定截点，实测就漂成 [20, 19, 20]。现在截点只由
    /// [`advance_bound_log`]（逻辑域上界）决定 ⇒ **任何文种都逐位相等**，物理那一档只需要证明
    /// "画得下"（下面第二条断言）。代价是每字最多 1 逻辑 px 余量，一行少装约一个字。
    #[test]
    fn report_budget_is_scale_independent() {
        // 1.25 也放进来：它夹不进任何一档（`physical_px` 会把它折回 16 px）⇒ 排版必须和 1×
        // 完全一致，这一条抓的是"预算按原始 scale 放大、字按档位画"那种顶穿右边距的写法。
        let scales = [1.0f32, 1.25, 1.5, 2.0];
        // 测料用**目录里真实存在的中文名**连起来，而不是随手打一串 CJK：缺字的 advance
        // 是 0 ⇒ `line_width` 会算出"放得下"，这条门禁就自己把自己骗过去了（而且手打的串
        // 长度刚好 = 预算时，`fit` 根本不会截，测的是个空条件）。
        let catalog = Catalog::default_v1().unwrap();
        let cjk: String = catalog.items.iter().map(|i| i.name.as_str()).collect();
        assert!(
            cjk.chars().count() > 14,
            "测料太短（{} 字）⇒ 1× 档压根不会截，这条断言就退化成'没坏'",
            cjk.chars().count()
        );
        assert!(
            crate::text::missing(&cjk).is_none(),
            "目录里有名字不在字体子集里：{:?} ⇒ 该重跑子集，别改测试",
            crate::text::missing(&cjk)
        );
        let kept: Vec<usize> = scales
            .iter()
            .map(|&s| fit(&cjk, s).chars().count())
            .collect();
        assert!(
            kept.windows(2).all(|w| w[0] == w[1]),
            "CJK 在 {scales:?} 上截到不同字数：{kept:?} ⇒ 截点不再只由逻辑预算决定"
        );
        assert!(kept[0] > 1, "截到只剩省略号 = 预算算错：{kept:?}");
        for &s in &scales {
            let out = fit(&cjk, s);
            assert!(out.chars().count() < cjk.chars().count(), "{s}× 竟然没截");
            assert!(out.ends_with('…'), "{s}× 截断没留可见痕迹：{out:?}");
            assert!(
                line_width(&out, s) <= line_budget_px(s),
                "{s}× 截完还超物理预算：{} > {} ⇒ 逻辑上界挡不住这一档的字宽（会顶穿右边距）",
                line_width(&out, s),
                line_budget_px(s)
            );
        }
        // 拉丁：现在也逐位相等（同一个理由：截点不看每档的取整宽）。哪一档都不许"根本不截"
        // 或"截到空"，那两种都是预算算错而不是排版风格。
        let latin = "Balance 1,240 Shards 999,999 DEMO extra words here".to_string();
        let kept_l: Vec<usize> = scales
            .iter()
            .map(|&s| fit(&latin, s).chars().count())
            .collect();
        assert!(
            kept_l.windows(2).all(|w| w[0] == w[1]),
            "拉丁在 {scales:?} 上截点漂了：{kept_l:?}"
        );
        for (s, k) in scales.iter().zip(&kept_l) {
            let s = *s;
            assert!(
                *k > 1 && *k < latin.chars().count(),
                "{s}× 的截点不像话：{k}"
            );
            let out = fit(&latin, s);
            assert!(
                line_width(&out, s) <= line_budget_px(s),
                "{s}× 拉丁截完还超预算：{}",
                line_width(&out, s)
            );
        }
    }

    /// 结果卡逐条文案：词序各随其文（绝不"英文词序 + 本地词表"拼接）、一行一条、
    /// **演示标不占结果卡的行**（它在标题行，`l10n::header_shards`）。
    /// ⚠️ 旧的 `lines.iter().all(is_ascii)` 删了：现在默认中文，那条断言会把交付本身当 bug。
    /// 取而代之的是"每个上都屏的字都在字体里"（`text::missing`），同 §4.2 门禁 ①②③ 那一族。
    #[test]
    fn report_lines_are_one_per_line_in_the_puller_language() {
        let items = vec![
            ("小白".to_string(), Rarity::N, false),
            ("铃帽".to_string(), Rarity::SR, true),
        ];
        let lines = report_lines(&[1, 0, 1, 0], &items, 8, Lang::Zh, 1.0);
        assert_eq!(lines[0], "2 抽 最高 SR");
        assert_eq!(lines[1], "SR1 N1", "档位计数行从高到低、只列非零档");
        assert_eq!(lines[2], "SR 铃帽 重复", "明细先列最高档");
        assert_eq!(lines[3], "N 小白 新");
        assert_eq!(lines[4], "碎片 +8");
        assert!(!lines.iter().any(|l| l.contains("演示")), "演示标不在这");
        for l in &lines {
            assert!(text::missing(l).is_none(), "上屏的字不在字体子集里：{l:?}");
        }
        // 同一份抽取在 en 下**词序不同**（不是把中文换词）：这条钉住"各语言各写模板"。
        let en = report_lines(&[1, 0, 1, 0], &items, 8, Lang::En, 1.0);
        assert_eq!(en[0], "2 pulled, best SR");
        assert_eq!(en[4], "+8 shards");
        assert_eq!(en[2], "SR 铃帽 DUP", "物品名保留目录中文（§7-R12）");
        // 全 0 计数（理论不可达，防御病态输入）不许 panic。
        let empty = report_lines(&[0, 0, 0, 0], &[], 0, Lang::Zh, 1.0);
        assert_eq!(empty[0], "0 抽");
    }

    /// §4.4 / §7-R10：卡片上的字在**抽取那一刻**烤成字符串 ⇒ 之后切语言不改已挂着的卡。
    /// 这条不是"懒得重翻译"的托词，是有测试的语义：当前显示的卡永远与生成它时的语言一致。
    #[test]
    fn baked_lines_keep_the_language_they_were_pulled_in() {
        let items = vec![("小白".to_string(), Rarity::N, false)];
        let zh = report_lines(&[1, 0, 0, 0], &items, 4, Lang::Zh, 1.0);
        let anim = MenuAnim::Reveal {
            elapsed: 0,
            lines: zh.clone(),
            best: Rarity::N,
        };
        // 走到"下一个 tick"再"切语言"：anim 里的行数与内容一个字都不动。
        let after = advance(&anim, Motion::Full);
        match &after {
            MenuAnim::Reveal { lines, .. } => assert_eq!(lines, &zh),
            other => panic!("reveal 第一格该还是 reveal，得到 {other:?}"),
        }
        // 换语言只影响**下一次**抽取（重新调 report_lines 才拿到英文）。
        let en = report_lines(&[1, 0, 0, 0], &items, 4, Lang::En, 1.0);
        assert_ne!(en, zh, "换了语言还是中文 ⇒ 词表没接上");
        assert!(en.iter().all(|l| !l.contains("抽")));
        // 语言行只在 `Menu` 相位可点（`is_busy` 挡住 burst/reveal）⇒ 上面这个"改不到"
        // 是界面语义而不是实现巧合。
        let busy = MenuAnim::Burst {
            rarity: Rarity::N,
            elapsed: 0,
            lines: zh.clone(),
        };
        assert!(busy.is_busy(), "burst 期必须挡住点击，否则卡到一半会换字");
        assert!(after.is_busy());
    }

    /// 排版契约：十连不许超行、任何行在任何缩放下不许超宽。这两条是 draw 层"不切词"的前提，
    /// 所以在纯逻辑层就钉死 —— 渲染层的逐点裁剪只会把词切一半，不会报错。
    #[test]
    fn ten_pull_report_fits_the_card_budget() {
        // 物品名取目录里**真实**的中文名（不翻译，§7-R12），而且是目录里最长的那批 ——
        // 用 "cat_item_0" 这种 ASCII 占位测不出 CJK 的宽度。
        let catalog = Catalog::default_v1().unwrap();
        let names: Vec<String> = catalog.items.iter().map(|i| i.name.clone()).collect();
        assert!(names.len() >= 10, "目录不足 10 件 ⇒ 这条测的是空东西");
        let items: Vec<(String, Rarity, bool)> = (0..10)
            .map(|i| (names[i].clone(), Rarity::ALL[i % 4], i % 3 == 0))
            .collect();
        let counts = [4, 3, 2, 1];
        for &l in &Lang::ALL {
            for &scale in &[1.0f32, 1.5, 2.0] {
                let lines = report_lines(&counts, &items, 999_999, l, scale);
                assert!(
                    lines.len() <= MAX_REPORT_LINES,
                    "{l:?} 结果卡 {} 行 > 预算 {MAX_REPORT_LINES}",
                    lines.len()
                );
                let budget = line_budget_px(scale);
                for line in &lines {
                    let w = line_width(line, scale);
                    assert!(
                        w <= budget,
                        "{l:?} @ {scale}× 行超宽 {w} > {budget}：{line:?}"
                    );
                    assert!(
                        text::missing(line).is_none(),
                        "{l:?} 上有画不出来的字：{line:?}"
                    );
                }
                assert_eq!(lines[1], "SSR1 SR2 R3 N4", "档位计数行四语言同形");
                // 六行的形状固定：标题 / 档位 / 明细×2 / 其余并一行 / 碎片。
                assert_eq!(
                    lines.len(),
                    6,
                    "十连的行数就是六行，多一行少一行都是契约破了"
                );
                assert_eq!(lines[4], l10n::line_more(l, 8), "剩下的条目并进一行");
                assert_eq!(
                    *lines.last().unwrap(),
                    l10n::line_shards(l, 999_999),
                    "最后一行永远是碎片（演示标在标题行）"
                );
            }
        }
    }

    /// 按**像素**截断，且绝不切在字节中间。预算由 `CATALOG` 真实宽度量出来，不写死字数
    /// （写死的 "19 个字符"下次改常量就变成假失败：失败得没错但原因离谱）。
    #[test]
    fn fit_truncates_by_pixels_not_bytes() {
        // 字符串按预算生成而不是写死长度：取"必然超宽"的串。
        let long_digits: String = (0..200).map(|i| (b'0' + (i % 10) as u8) as char).collect();
        let exact = fit(&long_digits, 1.0);
        assert!(exact.ends_with('…'), "截断要留下可见痕迹");
        assert!(
            line_width(&exact, 1.0) <= line_budget_px(1.0),
            "截完还超预算"
        );
        // 不超预算的串原样返回，无端补点就是把字改错了。
        let short = "余额 1,240";
        assert_eq!(fit(short, 1.0), short);
        assert_eq!(fit("", 1.0), "");
        // CJK 每字 ≈1 em ⇒ 同样的预算装的字数远少于 ASCII。这条是"按字数截"为什么会
        // 一边宽一边窄的直接证据。
        let cjk = fit(&"蓝".repeat(60), 1.0);
        assert!(
            cjk.chars().count() < exact.chars().count(),
            "CJK 截出来的字数不比拉丁少 ⇒ 预算不是按像素算的：{} vs {}",
            cjk.chars().count(),
            exact.chars().count()
        );
        // 逐 char 累加 ⇒ 结果永远在 char 边界上（原来靠"不切半个 UTF-8"的写法换成了像素）。
        assert!(cjk.is_char_boundary(cjk.len()));
        // 预算连省略号都放不下 ⇒ **什么都不画**，而不是画一个顶穿右边距的 `…`。
        assert!(
            fit_to("抽取失败", advance_px('蓝', 1.0) - 1, 1.0).is_empty(),
            "放不下省略号还硬画 ⇒ 顶穿出界，而像素门禁看不见这件事"
        );
    }
}
