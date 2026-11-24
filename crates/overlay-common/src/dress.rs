//! 换装那一条判据的**纯**层（D16 路线 B = 运行时逐格叠加的门禁面）。
//!
//! ## 为什么这一层存在
//!
//! 这一模块里的每一个函数都只做一件事：**把已经读出来的事实折成一个结论**。它们不开库、
//! 不扫盘、不解像素、不碰窗口系统 —— 所以它们能在**没有 compositor、没有 Windows、没有
//! 真存档**的机器上被逐条打到红。这不是整洁偏好，是这个项目反复付过学费的那条：
//! `App`（两家的都一样）离开显示服务器构造不起来 ⇒ "屏上 A、记忆里 B"这一族形状在 headless
//! 里**根本没有读数**（三次变异实测各复现一次，见 `overlay-wayland/src/app.rs` 的
//! `pixels_and_the_ledger_are_written_by_one_function_only` 那条源级门禁）。把判定拆成自由函数
//! 之后，"许不许穿""穿哪一件""这句该不该打"三件事各自有独立读数，真机那一轮只剩"接线对不对"。
//!
//! ## 为什么它在 `overlay-common` 而不是各家客户端
//!
//! 判据只有一份**语义**：同一张库、同一个开关、同一批件，两个平台不许给出两个答案。
//! 2026-11-23（M5 第二批 M5e）自 `overlay-wayland/src/app.rs` 整搬而来 —— 搬的判据与
//! [`crate::accounting`] 那条 `classify_tick` 完全同一条：**纯决策逻辑上提，IO 与像素留在各家**。
//! 反面理由也同一条：复制一份会在某一天静默分叉，而分叉的症状是"屏上戴着一顶而库里没记这笔"
//! 这一类**没有任何门禁看得见**的错。
//!
//! ## 谁用哪一条
//!
//! - [`dress_exempt_for`] / [`DressView`] —— 两家客户端的 `dress_view()`（换装页"这一件许不许穿"）
//!   与 Wayland 的 `pet_view()`（收藏册"这一只许不许换"）。后者是**同一条判据喂另一张表**。
//! - [`plan_pick_wear`] —— 只有 Wayland 用（收藏册点名那一件）。Win32 第二批没有收藏册，
//!   留着它不是为了看起来对称，和 [`crate::region`] 只被一家用是同一条理由。
//! - [`split_body_row`] / [`plan_restore`] —— 两家客户端的启动恢复（"上次穿的那顶还在不在"）。
//! - [`BODY_SLOT`] —— 写侧只有 Wayland（P5 换本体），读侧两家都走 [`split_body_row`]。
//!
//! ## 这一层刻意**没有**的东西
//!
//! `write_equip` / `clear_equip` / `WornWrite` 不在这里。它们是"把结论落进 `equipped` 那一行"
//! 的那一步 = IO，而 IO 的家在 `petstore` 那一侧、调用形状在各家客户端（D13 同一条）。
//! 把它们搬进来要嘛让本 crate 依赖 `petstore`（本 crate 的立身之本是不碰任何带状态的层，
//! `tests/no_platform_deps.rs` 的依赖普查钉着那一张表），要嘛造一个只服务于两个调用者的
//! 泛型 —— 两条都比"各家十行 IO 胶水"贵。⚠️ 代价明写：**"演示态一个字都不写"这条策略今天
//! 有两份实现**，它的读数在 Wayland 侧（`demo_mode_writes_nothing_while_a_real_store_keeps_the_row`
//! / `unworn_clears_the_row_and_demo_has_nothing_to_clear`），Win32 侧那份必须自己带上同一条
//! 断言，不许只抄结论不抄测试。

use petcore::gacha::Owned;

/// `equipped.slot` 里代表**本体**那一行的取值。
///
/// 为什么是一个裸字符串而不是 `Slot` 枚举的成员：`Slot` 是**配件的挂点**（`head`/`face`/…六值，
/// `accessory.json` 的 `slot` 字段就归它），而本体不挂在任何挂点上 —— 把 `body` 塞进那个枚举会让
/// "一件配件的槽位是 body"这种组合变得**可以表示**，而它是没有意义的（`schema.sql` 那个 `slot` 列
/// **故意没有 CHECK**，正是为这件事留的接口槽；petstore 的
/// `the_slot_column_accepts_body_by_design` 钉着"写得进去"）。
pub const BODY_SLOT: &str = "body";

/// 开发态豁免那一句的**纯**版本 = 两家客户端各自 `App::dress_exempt()` 的函数体。
///
/// 单拎出来只为一件事：`--allow-unowned-accessories` 而**没有** `--db` 那一格（演示态）是被
/// 专门区分着拍板过的（"照 DRYRUN 放行 + 打「演示」标"⇒ 放行但**不**点帽子页那个 `*`），而
/// `App` 要连显示服务器才构造得起来 ⇒ 留在方法里这条判定就没有任何自动化读数，只能靠"起一次
/// 进程看有没有星号"。拆成自由函数 ⇒ 四格真值表逐格钉住（`exemption_needs_the_flag_and_a_real_store`）。
///
/// ⚠️ 本体侧的 `--allow-unowned-pets` **共用这一条判据函数**而**开关字段分开**：配件那条的屏上
/// 记号是帽子页标题的 `*`，而那一页的列表里根本没有宠物 ⇒ 两条合并必然出现"标打了而没放行"。
pub fn dress_exempt_for(allow_unowned: bool, dry_run: bool) -> bool {
    allow_unowned && !dry_run
}

/// 库存那道跑完之后，换装页手里那张表（各家客户端 `App::dress_view()` 的结果）。
///
/// 为什么不是一个 `Vec<Candidate>` 而是**下标**：各家 `App::picked` 存的就是 `candidates` 的下标，
/// 而"选中哪一件"与"这一件许不许穿"是两件事 —— 复制一份过滤后的表就会出现第二个"当前选中的
/// 是哪件"（本项目反复出现的那一族）。留下标 ⇒ `picked` 仍是唯一下标，`visible` 只是它能不能落
/// 在这几格上。
pub struct DressView {
    /// 通过库存那道判定的 `candidates` 下标，**升序**（"换一件"的循环序 = 目录序）。
    pub visible: Vec<usize>,
    /// 因为"库存里没有这一件"而出局的数量 ⇒ 只进 stderr 那一句，不上屏（用户内容不进子集字体）。
    pub withheld: usize,
}

impl DressView {
    /// 库存那道的**纯**判定 = "这些件叫什么" + "库存说拥有哪些" ⇒ 可见下标与出局数。
    ///
    /// 拆这一层的唯一理由 = 让这条判据能被自动化打到红。外层 `App::dress_view` 要连库、要扫盘，
    /// 整条只有真机跑得起来 ⇒ "未拥有的件不出现"就只能靠人眼看屏幕，而本项目对门禁的一贯要求是
    /// "能证明它会红"。纯函数拿走三档输入 ⇒ 三档各有一条独立读数（`tests/dress.rs` 前三条）。
    ///
    /// - `allow_all` = 这一轮**根本没有**库存那道（开发态豁免 ∪ 演示态；两者屏上记号不同，
    ///   但可见集是同一个，所以在这里合成一个布尔）。
    /// - `owned = None` = 那道**没读过**（连不上库 / 读失败）⇒ 失败关闭，一件都不给。
    ///   这一档必须与 `Some(空集)` 分开：后者是"读到了，答案是都没有"，前者的"都没有"没有任何
    ///   事实支撑过 —— 拿它当后者就是"没验过也当验过"。
    ///
    /// `ids` 传的是**目录名**（各家 `Candidate::id`）而不是清单里的 `slug`：库存那一侧的键从头到尾
    /// 是目录名，而 `slug` 是打包者填的字符串，两者可以不相等（启动扫盘那行 ⚠️ 报的就是这个）。
    pub fn select(allow_all: bool, owned: Option<&Owned>, ids: &[String]) -> Self {
        let visible: Vec<usize> = match (allow_all, owned) {
            // 这一轮没有库存那道 ⇒ 全可见（`ids` 只贡献长度，不参与判定）
            (true, _) => (0..ids.len()).collect(),
            // 那道没读过 ⇒ 全不可见
            (false, None) => Vec::new(),
            // 读过了 ⇒ 逐件问"库存里有没有这个名字"
            (false, Some(owned)) => ids
                .iter()
                .enumerate()
                .filter(|(_, id)| owned.contains(*id))
                .map(|(i, _)| i)
                .collect(),
        };
        let withheld = ids.len() - visible.len();
        Self { visible, withheld }
    }
}

/// 「点中一件」要判的三档结论（方法论同 [`DressView::select`]：`App` 在没有显示服务器的机器上
/// 构造不起来 ⇒ 判定必须先改写成写得出来、也测得出来的形状）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickWear {
    /// 库存那道没过（未拥有 / 库读不出 / 这件不匹配本体 / 下标已失效）⇒ 一个字都不动。
    Refused,
    /// 点的**就是**身上这一件 ⇒ 不解像素、不重投帧（点了什么都没发生是**对的**，但它必须说话）。
    AlreadyWorn,
    /// 先释放身上那件（若有任何一件），再装载这一件。
    SelectAndWear,
}

/// [`PickWear`] 的判定本体。拆出来的唯一理由 = 让"先比后写"这件事有一条 headless 读数。
///
/// ⚠️ `AlreadyWorn` 的比较**必须**用写之前的 `picked` —— 调用方那句"先比再写"的次序就是这条函数
/// 存在的理由：一旦先写 `picked` 再比，`picked == want` 恒真，最常见的那一次点击（换一件别的）
/// 会读成"已经穿着这一件"⇒ 静默不动 + 一句谎话。
pub fn plan_pick_wear(gate_passes: bool, worn_some: bool, picked: usize, want: usize) -> PickWear {
    if !gate_passes {
        return PickWear::Refused;
    }
    if worn_some && picked == want {
        return PickWear::AlreadyWorn;
    }
    PickWear::SelectAndWear
}

/// 把 `equipped` 读回的整表分成"本体那一行"与"配件那几行"（启动恢复要补的那道剥壳）。
///
/// 为什么**必须**先剥：`Store::equipped` 的 `ORDER BY slot, item_id` 里 `body` 排在**所有**配件
/// 槽位之前（字母序），于是那一行是整表的第一条，而 [`plan_restore`] 的第 1 道只问"配件表里有没有
/// 这一件"。不剥会打出三句假话：① 它说的是一件**根本不是配件**的账 ⇒ 用户下一次就去查配件库、
/// 查 `pack.py`，而真正的下一件事是"库里那一行是换本体写的"；② 那句"磁盘上找不到这一件"会盖掉
/// 真正的下一件；③ 第 4 道"一次只容一件"会被本体行占掉一格。
///
/// ⚠️ 明写**不构成**危害的那一条，免得后人把它当成更强的理由：本体行**不会**把帽子挤掉。
/// [`plan_restore`] 取的是"第一条过了四道的行"，而本体行在第 1 道（或第 3 道 `slot != 清单声明的
/// 槽位`，因为 `Slot` 六值里没有 `body`）就被 `continue` 掉了，一次都不占 `wear`。⇒ 这条剥壳
/// 的全部意义是**不说假话**，不是"少剥就丢装备"。
///
/// 返回 `(本体的 id, 只剩配件的行)`。多于一条 `body` 行 = 手工行或旧版本写的 ⇒ **取首条**，
/// 与只读单行的那条读法给的答案一致（同一条 `ORDER BY`）。
pub fn split_body_row(rows: &[(String, String)]) -> (Option<String>, Vec<(String, String)>) {
    let mut body: Option<String> = None;
    let mut accessories = Vec::new();
    for (slot, item_id) in rows {
        if slot == BODY_SLOT {
            if body.is_none() {
                body = Some(item_id.clone());
            }
        } else {
            accessories.push((slot.clone(), item_id.clone()));
        }
    }
    (body, accessories)
}

/// [`plan_restore`] 的结论 = 要穿的那一格（`None` = 一件都不穿）+ 每一行的处置。
pub struct RestorePlan {
    pub wear: Option<usize>,
    /// 给人看的行，**只进 stderr 不上屏**（用户内容不进子集字体，同 [`DressView::withheld`]）。
    /// 每一条都点名"哪一格、哪一件、为什么"。
    pub notes: Vec<String>,
}

/// 启动恢复那一道的**纯**判定（"上次穿的这一轮还穿不穿"）。
///
/// 拆出来同 [`DressView::select`] 一个理由：外层要连库、要解码、要显示服务器，这里三样都不需要
/// ⇒ "手工往 `equipped` 塞一行未拥有的 id 必须出局并报一句"才有读数。
///
/// 四道各管一件事，顺序 = 便宜在前：
/// 1. **磁盘上找不到这一件** —— 已删除，或门禁没过（被启动扫盘那一步跳掉了）。
///    ⚠️ 这一条把"恢复时重跑门禁"整个接住了：恢复能穿的**只能是 discovery 活着交出来的那件**，
///    所以清单坏了 / 不 `fits` 本体的那几道，在恢复这一刻都由同一条打到。单独再跑一遍只会多出
///    第二套答案。（解码那一档不在这里 —— 它是像素，只能穿的那一刻验。）
/// 2. **库存没有这一件** ⇒ 出局并报（这一条拦的正是手工行；可见集 `visible` 来自
///    [`DressView::select`] ⇒ 换装页"许不许穿"与恢复"穿不穿"从头就是同一道判据，不会有
///    "换装页看不见而启动却穿上了"那种分裂）。
/// 3. **这一行记的槽位与清单声明不符** ⇒ 手工行的第二种形状，不穿。槽位的真值在清单
///    （`Slot` 六值），而 `equipped.slot` 没有 CHECK，所以只能这样对。
/// 4. **多于一件**：内存档 M1 只容一件 ⇒ 取第一件，其余只报告、不穿，而**库里那几行原样留着**
///    （删掉等于把用户没让我删的东西丢了；哪天容得下多件，它们自己会恢复）。
///    "第一件"可重放的前提 = 行序由 `Store::equipped` 的 `ORDER BY slot, item_id` 给。
pub fn plan_restore(
    rows: &[(String, String)],
    ids: &[String],
    slots: &[String],
    visible: &[usize],
) -> RestorePlan {
    let mut wear: Option<usize> = None;
    let mut notes: Vec<String> = Vec::new();
    for (slot, item_id) in rows {
        let at = |i: usize| format!("slot={slot} item={item_id} ⇒ {}", ids[i]);
        let Some(i) = ids.iter().position(|id| id == item_id) else {
            notes.push(format!(
                "{slot}={item_id} 现在装不上：磁盘上找不到这一件（已删除，或门禁没过）"
            ));
            continue;
        };
        if slots[i] != *slot {
            notes.push(format!(
                "{} 而清单声明的是 {slots_i} ⇒ 这一行是手工塞的，不穿",
                at(i),
                slots_i = slots[i]
            ));
            continue;
        }
        if !visible.contains(&i) {
            notes.push(format!("{} 出局：库存里没有这一件 ⇒ 未验过拥有不穿", at(i)));
            continue;
        }
        if wear.is_some() {
            notes.push(format!(
                "{} 不恢复：内存档 M1 一次只容一件（库里这一行留着，以后容得下多件会自动穿）",
                at(i)
            ));
            continue;
        }
        wear = Some(i);
    }
    RestorePlan { wear, notes }
}
