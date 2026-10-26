//! 菜单/结果卡的**纯逻辑**：布局、命中、定位、按钮分派、动画相位表、展示行。
//! 这里不许出现任何 wayland 类型 —— 全部函数吃整数/坐标、出矩形与表，headless CI 必绿。
//! 接线在 `app.rs`（`MenuState`），像素落在 `draw_menu`。

use petcore::gacha::Rarity;

use crate::settings::Motion;

/// 可点的菜单项。余额行与错误行是信息，不进命中表。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Pull1,
    Pull10,
    Close,
}

impl MenuItem {
    pub const CLICKABLE: [Self; 3] = [Self::Pull1, Self::Pull10, Self::Close];

    /// 卡片上的显示文本（字模只认 ASCII，非 ASCII 会被 `font` 画成 `?`）。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pull1 => "PULL x1",
            Self::Pull10 => "PULL x10",
            Self::Close => "CLOSE",
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
/// 宽度由**排版契约**定，不是拍的：结果卡一行最宽的形状是 `"{rarity} {item_id} NEW"`
/// （实测形如 `SSR cat_shiro_01 NEW` = 19 字符），而字号必须是"逻辑 2 px/字模像素"
/// 才在 96 px 宠物旁边读得清（见 [`TEXT_PX`]）。`2*PAD + 18*STEP*TEXT_PX = 232 ≤ CARD_W`
/// 这条算术由 `report_budget_fits_the_card` 钉住。
/// ⚠️ 上一版这里写的是 180 + 20 字符，证明用的是"物理 scale 2"，也就是**只在 1.5× 屏上**
/// 成立（270 物理像素刚好够）；换到 1× 屏就是 240 > 180 直接切字。排版预算必须和显示器
/// 缩放无关，否则门禁绿着、字却被裁了 —— 又是"看起来对、其实说谎"那一族。
pub const CARD_W: i32 = 240;
pub const CARD_H: i32 = 150;

/// 字号：**逻辑**像素/字模像素。`draw` 层乘上生效缩放得到物理字号 ⇒ 同一份排版
/// 在 1× 和 1.5× 上占的 logical 宽度一模一样，预算也就只需要证一次。
pub const TEXT_PX: i32 = 2;

/// 行几何：左右留边 8，顶部信息行占 22，行高 26，**行步进 30**（26 高 + 4 缝）。
/// 步进必须大于行高，否则相邻行矩形相交 ⇒ `hit_test` 的"缝隙不命中"与"两处重叠"全塌。
/// 这四个数是 `layout` 与 `draw_menu` 共用的唯一排版口径，draw 层不许再算一份。
pub const PAD: i32 = 8;
pub const HEAD_H: i32 = 22;
pub const ROW_H: i32 = 26;
pub const ROW_STEP: i32 = 30;
/// 结果卡行步进（logical）：字模 7 px 高 × scale 2 = 14，留 4 leading。
pub const LINE_STEP: i32 = 18;

/// 固定布局：3 个可点项从上到下。`layout` 与 `hit_test` 是唯一的真相源，
/// `draw_menu` 画矩形、`menu_pointer_event` 判命中都从这张表取，不许各处再算一份。
pub fn layout() -> Vec<ItemRect> {
    MenuItem::CLICKABLE
        .iter()
        .enumerate()
        .map(|(i, &item)| ItemRect {
            item,
            x: PAD,
            y: PAD + HEAD_H + (i as i32) * ROW_STEP,
            w: CARD_W - 2 * PAD,
            h: ROW_H,
        })
        .collect()
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

/// 卡片内容签名：`(相位, 余额, 错误行)` 全同 ⇒ 像素一模一样 ⇒ draw 层跳过 commit。
/// 这是菜单的省电闸门 —— 没有它，`update_menu` 每 tick 置脏，静止的菜单也会 60 次/秒
/// 提交 180×150 的 buffer（M4 的三档口径里"无事可做"那一档就永远不会出现）。
///
/// ⚠️ **`motion` 故意不进签名**：它和 `dry_run` 同族 —— 启动时一次定死，运行中没有任何
/// UI 入口能改它 ⇒ "同一份输入给同一个签名"不会因为少了它而失真。对照 `lang`：语言**能**
/// 在运行中被点着换，所以它必须在签名里（下一步接语言行时加，混入位置 = 相位 tag 之后、
/// `elapsed` 之前）。把这条区别写在这里，是因为两者长得像而语义相反，最容易顺手抄错。
/// FNV-1a：不需要新依赖，且**只用来判等**，不当哈希表键、不进任何确定性契约。
pub fn content_sig(anim: &MenuAnim, balance: i64, error: &str) -> u64 {
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

/// 结果卡排版预算。十连全列 = 13 行，卡片（180×150 logical、字模 scale 2 一行约 21 字符）
/// 装不下，`font` 的逐点裁剪又会把词切一半 ⇒ **行数和行宽必须在拼行时就定死**，
/// 而不是留给渲染层兜底。这三条常量是 draw 层的排版契约。
pub const MAX_LINE_CHARS: usize = 18;
/// 结果卡最多 7 行：`标题 + 档位计数 + TOP_ITEMS 明细 + MORE + 碎片 + DRYRUN`。
/// 与 [`LINE_STEP`] 一起构成"卡片装得下"的算术证明（见测试 `report_budget_fits_the_card`）。
pub const MAX_REPORT_LINES: usize = 7;
/// 明细最多列几条（其余并进 `+N MORE`），永远先列最高档。
pub const TOP_ITEMS: usize = 2;

/// 按字符数截断到 [`MAX_LINE_CHARS`]（超长留一个 `.`）。ASCII 预算下 char 与 byte 等价，
/// 但按 char 走，非 ASCII 混进来也不会切在字节中间。
pub fn fit(s: &str) -> String {
    if s.chars().count() <= MAX_LINE_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_LINE_CHARS - 1).collect();
    format!("{head}.")
}

/// 结果卡文本（渲染无关，逐行走 `font::draw_str`）。
/// `items` = 每抽的 `(item_id, rarity, 是否重复)`；`dry_run` 必须打 DRYRUN 标 ——
/// 让用户能一眼分清"这是内存库的演示"还是"真落库了"（与 `--allow-default-archive` 红线同族）。
pub fn report_lines(
    counts: &[u32; 4],
    items: &[(String, Rarity, bool)],
    shards_gained: u64,
    dry_run: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    match best_rarity(counts) {
        Some(best) => lines.push(fit(&format!("{} PULLED BEST {best}", items.len()))),
        None => lines.push(fit(&format!("{} PULLED", items.len()))),
    }
    // 档位计数行：只列非零档，从高到低。稳定排序 ⇒ 同档永远按抽取顺序出。
    let tiers: Vec<String> = Rarity::ALL
        .iter()
        .rev()
        .filter(|r| counts[r.index()] > 0)
        .map(|r| format!("{}{}", r.as_str(), counts[r.index()]))
        .collect();
    if !tiers.is_empty() {
        lines.push(fit(&tiers.join(" ")));
    }
    let mut ranked: Vec<&(String, Rarity, bool)> = items.iter().collect();
    ranked.sort_by_key(|(_, r, _)| std::cmp::Reverse(r.index()));
    for (id, r, dup) in ranked.iter().take(TOP_ITEMS) {
        lines.push(fit(&format!(
            "{r} {id} {}",
            if *dup { "DUP" } else { "NEW" }
        )));
    }
    if items.len() > TOP_ITEMS {
        lines.push(fit(&format!("+{} MORE", items.len() - TOP_ITEMS)));
    }
    lines.push(fit(&format!("+{shards_gained} SHARDS")));
    if dry_run {
        lines.push(fit("DRYRUN"));
    }
    debug_assert!(lines.len() <= MAX_REPORT_LINES, "结果卡超行 ⇒ 排版契约破了");
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rects_are_disjoint_and_inside_the_card() {
        let rects = layout();
        assert_eq!(rects.len(), 3);
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

    #[test]
    fn hit_test_hits_centers_and_misses_gaps_and_margins() {
        let rects = layout();
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

    /// 静止 ⇒ 签名不变 ⇒ draw 层不 commit。三个输入（相位/余额/错误行）任一变了都要变。
    #[test]
    fn content_sig_is_stable_when_static_and_moves_with_every_visible_input() {
        let rest = content_sig(&MenuAnim::Menu, 100, "");
        assert_eq!(rest, content_sig(&MenuAnim::Menu, 100, ""));
        assert_eq!(
            rest,
            content_sig(&advance(&MenuAnim::Menu, Motion::Full), 100, "")
        );
        assert_ne!(rest, content_sig(&MenuAnim::Menu, 101, ""), "余额行会变");
        assert_ne!(
            rest,
            content_sig(&MenuAnim::Menu, 100, "DB LOCKED"),
            "错误行会变"
        );
        let burst = MenuAnim::Burst {
            rarity: Rarity::N,
            elapsed: 0,
            lines: vec!["X".into()],
        };
        assert_ne!(
            content_sig(&burst, 100, ""),
            content_sig(&advance(&burst, Motion::Full), 100, ""),
            "扩散期每 tick 都在动 ⇒ 签名必须动"
        );
        // motion 不进签名（启动定死、运行中无人能改），但**同相位的不同档**像素确实相同，
        // 所以这里必须是等号；写成断言是为了让"顺手把 motion 混进来"当场红，而不是悄悄多一提交。
        assert_eq!(
            content_sig(&burst, 100, ""),
            content_sig(&burst, 100, ""),
            "签名不该看进程外的东西"
        );
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

    /// 排版可行性证明：7 行 × 18 logical 高 + 上下边距 ≤ 卡片高；
    /// 20 字符 × 6 × scale 2 的字模在 1.5× 缓冲（270 物理像素宽）里扣掉边距后放得下。
    /// 这两条一旦不成立，draw 层只能"切掉半个词"—— 像素门禁看不见少字，所以必须在这里红。
    #[test]
    fn report_budget_fits_the_card() {
        assert!(
            2 * PAD + MAX_REPORT_LINES as i32 * LINE_STEP <= CARD_H,
            "结果卡装不下 {} 行",
            MAX_REPORT_LINES
        );
        // 行宽预算：全用 **logical** 口径（字号 [`TEXT_PX`] 也是 logical），所以这条证明
        // 与显示器缩放无关 —— 物理像素只是同步乘 scale，不存在"1.5× 才够宽"。
        let text_w = MAX_LINE_CHARS as i32 * crate::font::STEP * TEXT_PX;
        assert!(
            2 * PAD + text_w <= CARD_W,
            "{MAX_LINE_CHARS} 字符在字号 {TEXT_PX} 下要 {} logical 像素，卡片只有 {CARD_W}",
            2 * PAD + text_w
        );
        assert_eq!(
            2 * PAD + text_w,
            232,
            "排版预算算的是这个数，改了常量要看得见"
        );
    }

    #[test]
    fn report_lines_are_ascii_one_per_line_with_the_dryrun_stamp() {
        let items = vec![
            ("cat_shiro".to_string(), Rarity::N, false),
            ("hat_bell".to_string(), Rarity::SR, true),
        ];
        let lines = report_lines(&[1, 0, 1, 0], &items, 8, true);
        assert_eq!(lines[0], "2 PULLED BEST SR");
        assert_eq!(lines[1], "SR1 N1", "档位计数行从高到低、只列非零档");
        assert_eq!(lines[2], "SR hat_bell DUP", "明细先列最高档");
        assert_eq!(lines[3], "N cat_shiro NEW");
        assert_eq!(lines[4], "+8 SHARDS");
        assert_eq!(lines[5], "DRYRUN");
        assert!(
            lines.iter().all(|l| l.is_ascii()),
            "展示行必须是字模能画的 ASCII"
        );
        let real = report_lines(&[1, 0, 0, 0], &items[..1], 0, false);
        assert!(
            !real.iter().any(|l| l == "DRYRUN"),
            "真落库不许打 DRYRUN 标"
        );
        // 全 0 计数（理论不可达，防御病态输入）不许 panic。
        let empty = report_lines(&[0, 0, 0, 0], &[], 0, true);
        assert_eq!(empty[0], "0 PULLED");
    }

    /// 排版契约：十连不许超行、任何行不许超宽。这两条是 draw 层"不切词"的前提，
    /// 所以在纯逻辑层就钉死 —— 渲染层的逐点裁剪只会把词切一半，不会报错。
    #[test]
    fn ten_pull_report_fits_the_card_budget() {
        let items: Vec<(String, Rarity, bool)> = (0..10)
            .map(|i| {
                (
                    format!("cat_item_{}", i),
                    [Rarity::N, Rarity::R, Rarity::SR, Rarity::SSR][i % 4],
                    i % 3 == 0,
                )
            })
            .collect();
        let counts = [4, 3, 2, 1];
        let lines = report_lines(&counts, &items, 42, true);
        assert!(
            lines.len() <= MAX_REPORT_LINES,
            "结果卡 {} 行 > 预算 {MAX_REPORT_LINES}",
            lines.len()
        );
        for l in &lines {
            assert!(
                l.chars().count() <= MAX_LINE_CHARS,
                "行超宽 {}: {l:?}",
                l.chars().count()
            );
        }
        assert_eq!(lines[1], "SSR1 SR2 R3 N4");
        assert_eq!(lines[lines.len() - 3], "+8 MORE", "剩下的档位并进一行");
        assert_eq!(lines[lines.len() - 2], "+42 SHARDS");
        assert_eq!(lines[lines.len() - 1], "DRYRUN");
    }

    #[test]
    fn fit_truncates_by_chars_not_bytes() {
        // 字符串按预算生成而不是写死长度：卡片宽度是个会被排版契约改动的量，写死的
        // "19 个字符"下次改常量就变成假失败（失败得没错但原因离谱）。
        let digits =
            |n: usize| -> String { (0..n).map(|i| (b'0' + (i % 10) as u8) as char).collect() };
        assert_eq!(fit(&digits(MAX_LINE_CHARS)), digits(MAX_LINE_CHARS));
        let long = fit(&digits(MAX_LINE_CHARS + 6));
        assert_eq!(long.chars().count(), MAX_LINE_CHARS);
        assert!(long.ends_with('.'), "截断要留下可见痕迹");
        // 非 ASCII 按 char 切，绝不切在字节中间（`is_char_boundary` 恒真）。
        let cjk = fit(&"啊".repeat(MAX_LINE_CHARS + 5));
        assert!(cjk.is_char_boundary(cjk.len()) && cjk.chars().count() == MAX_LINE_CHARS);
    }
}
