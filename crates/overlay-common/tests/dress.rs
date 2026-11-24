//! `overlay_common::dress` 那五条纯判据的读数。
//!
//! 这一批测试**不是新写的** —— 它们原来住在 `overlay-wayland/src/app.rs` 的 `mod tests` 里
//! （P2/P3/P5 那几轮一条条长出来的），2026-11-23（M5e，M5 第二批）随判据本身搬到这里。
//! 搬的理由与 `dress` 上提的理由是同一条：**判据只有一份语义，读数也就只该有一份**；Windows
//! 客户端将来跑的是同一个 `plan_restore`，若测试留在 wayland 那一侧，Win32 报的每一句
//! "这一行是手工塞的"就从来没有被测过。
//!
//! ⚠️ 守恒：这九条是**移动**不是新增 ⇒ 全工作区测试总数不变。留在 `app.rs` 的那几条
//! （`demo_mode_writes_nothing_while_a_real_store_keeps_the_row`、`the_ui_writes_the_very_file_
//! the_next_process_opens`、`unworn_clears_the_row_and_demo_has_nothing_to_clear`、
//! `the_row_payload_is_the_same_index_the_dress_page_cycles`、
//! `pixels_and_the_ledger_are_written_by_one_function_only`）全都碰 `Store` / `Gacha` / `App`
//! 那三样 IO 或源级形状，搬过来就要在本 crate 里链 `petstore`（`tests/no_platform_deps.rs`
//! 不许）⇒ 它们的家仍在客户端那一侧。

use petcore::gacha::Owned;

use overlay_common::dress::{
    DressView, PickWear, dress_exempt_for, plan_pick_wear, plan_restore, split_body_row,
};

/// P2 的三条读数 = [`DressView::select`] 的三档输入，各打各的。
///
/// 为什么这三条值得单独存在，而不是"真机点一下看看"：整条 `App::dress_view` 要连库要扫盘，
/// 只有真机跑得起来 ⇒ "未拥有的件不出现"这一句就变成只有人眼能验。拆出纯函数后，判定这部分
/// 由下面三条钉住，屏上那个 `*` 由 `card::tests::dress_page_star_marks_only_the_dress_page`
/// （在 `overlay-wayland`）钉住，两边都能红。
#[test]
fn unowned_accessory_never_appears_while_the_owned_one_does() {
    let ids: Vec<String> = ["hat_a", "hat_b", "hat_c"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let owned: Owned = ["hat_b"].into_iter().map(String::from).collect();
    let v = DressView::select(false, Some(&owned), &ids);
    // 可见的是**下标**而不是克隆 ⇒ `picked` 仍是"选中哪件"的唯一事实
    assert_eq!(v.visible, vec![1], "只有库存里那一件能出现");
    assert_eq!(v.withheld, 2, "出局数要说得出漏了几件（stderr 那一句读它）");
}

#[test]
fn exempt_or_demo_makes_nothing_withheld() {
    let ids: Vec<String> = ["hat_a", "hat_b"].iter().map(|s| s.to_string()).collect();
    let v = DressView::select(true, None, &ids);
    assert_eq!(v.visible, vec![0, 1]);
    assert_eq!(
        v.withheld, 0,
        "放行这一档不许报出局数（那会把两句不同的话混成一句）"
    );
}

/// **失败关闭**那一档（`owned = None` = 库存这道**没读过**：连不上库 / 读失败）。
///
/// 光断"什么都不给"抓不到两种坏实现：① 把 `None` 合并成"读过而答案是空集"（那 `--db` 打错
/// 路径与"确实没抽到东西"就在屏上分不开了）；② 更坏的**永远返回空**。所以这条真正起作用的是
/// 后半 —— **库存里有名字时必须一件不少地放行**，它证明这道门是读出来的，不是常量。
#[test]
fn unreadable_store_closes_the_door_while_a_real_set_opens_it() {
    let ids: Vec<String> = ["hat_a", "hat_b"].iter().map(|s| s.to_string()).collect();
    let closed = DressView::select(false, None, &ids);
    assert!(closed.visible.is_empty(), "没验过拥有就一件都不给");
    assert_eq!(closed.withheld, 2);
    let all: Owned = ["hat_a", "hat_b"].iter().map(|s| s.to_string()).collect();
    let open = DressView::select(false, Some(&all), &ids);
    assert_eq!(
        open.visible,
        vec![0, 1],
        "库存说都有 ⇒ 这道门必须真的放行，否则上面那些断言全在测一个常量"
    );
    assert_eq!(open.withheld, 0);
}

/// 豁免真值表四格（[`dress_exempt_for`]）。
///
/// 第三格 = 用户 2026-11-06 专门拍板的那一格：给了 `--allow-unowned-accessories` 而**没给**
/// `--db` ⇒ 放行（照 DRYRUN 那套），但帽子页**不**点 `*`（右端已有「演示」标，同一件事不给
/// 两个记号）。这一格以前只有"起进程看屏幕"能验，而 `App` 离开 compositor 构造不起来。
#[test]
fn exemption_needs_the_flag_and_a_real_store() {
    assert!(
        dress_exempt_for(true, false),
        "开关 + `--db` ⇒ 这才是豁免态"
    );
    assert!(
        !dress_exempt_for(false, false),
        "没给开关，普通运行轮不许豁免"
    );
    assert!(
        !dress_exempt_for(true, true),
        "演示态放行但**不点星**：屏上已有「演示」标，两个记号 = 两个事实源"
    );
    assert!(
        !dress_exempt_for(false, true),
        "演示态没给开关也是放行（DRYRUN 那一套），但同样不许点星"
    );
}

/// 手工往 `equipped` 塞一行 ⇒ **报一句并出局**，不许静默穿上（J2 的后半）。
///
/// 两种手工形状都测：未拥有的那件（磁盘有、库存没有）与槽位不符的那行（清单说 `neck`
/// 而库里写 `head`）。只测前者会留下后者：后者更阴 —— 像素照解照画，帽子挂错部位，
/// 而屏幕上没有任何一处会说"这一行是我自己塞的"。
#[test]
fn a_hand_made_equipped_row_is_refused_and_named() {
    let ids: Vec<String> = ["hat_a", "hat_b"]
        .iter()
        .copied()
        .map(String::from)
        .collect();
    let slots: Vec<String> = ["head".into(), "head".into()].to_vec();
    // 库存只有 hat_a ⇒ visible 只有 0（这一档直接沿用 P2 那次判定的输出）
    let unowned = plan_restore(
        &[("head".to_string(), "hat_b".to_string())],
        &ids,
        &slots,
        &[0],
    );
    assert!(unowned.wear.is_none(), "未拥有的那一件绝不被恢复穿上");
    assert_eq!(unowned.notes.len(), 1);
    assert!(
        unowned.notes[0].contains("hat_b") && unowned.notes[0].contains("库存里没有这一件"),
        "出局要点名是哪一件、错在哪一道：{:?}",
        unowned.notes
    );
    // 槽位不符：库里说 head，清单说 neck ⇒ 这一行不是这套代码写出来的
    let crossed = plan_restore(
        &[("head".to_string(), "hat_a".to_string())],
        &ids,
        &["neck".into(), "head".into()],
        &[0, 1],
    );
    assert!(crossed.wear.is_none());
    assert!(
        crossed.notes[0].contains("手工"),
        "要说清这是手工行，不然用户会去查库存而那儿根本没毛病：{:?}",
        crossed.notes
    );
    // 正向对照：同一批输入把槽位改对 ⇒ 必须真的穿上那一格。
    // 没有这一句，上面四条断言全可能是在测一个"永远返回 None"的函数。
    let ok = plan_restore(
        &[("neck".to_string(), "hat_a".to_string())],
        &ids,
        &["neck".into(), "head".into()],
        &[0, 1],
    );
    assert_eq!(ok.wear, Some(0), "四道全过就该穿上（门禁不能只会说不）");
    assert!(ok.notes.is_empty());
}

/// 上次穿的帽子文件没了（被删、或门禁没过而被启动扫盘跳掉）⇒ 点名，不静默。
///
/// 这条**不需要**恢复代码自己再跑一遍门禁：库里那一行要能穿，前提是它出现在 `ids` 里，
/// 而 `ids` 只由启动扫盘（wayland 侧的 `discover_accessories`）活着交出来 ⇒
/// "不在磁盘上/不过门禁"这两句是同一条。
#[test]
fn a_vanished_item_is_named_instead_of_silently_not_worn() {
    let ids: Vec<String> = ["hat_a".into()].to_vec();
    let slots: Vec<String> = ["head".into()].to_vec();
    let plan = plan_restore(
        &[("head".to_string(), "gone_hat".to_string())],
        &ids,
        &slots,
        &[0],
    );
    assert!(plan.wear.is_none());
    assert!(
        plan.notes[0].contains("gone_hat") && plan.notes[0].contains("找不到这一件"),
        "「上次穿的 X 现在装不上」要带上 X 叫什么：{:?}",
        plan.notes
    );
}

/// 库里记着好几格 ⇒ 只回第一件（M1 = 一次一件），其余**只报告、不删**。
///
/// "第一件"能重放的前提是行序由库给（`Store::equipped` 的 `ORDER BY slot, item_id`）⇒
/// 这一条同时是那次排序的消费方：排序坏了这里就是随机一顶。
#[test]
fn only_the_first_of_several_rows_comes_back_and_the_ledger_stays_intact() {
    let ids: Vec<String> = ["back_pack".into(), "cap".into()].to_vec();
    let slots: Vec<String> = ["back".into(), "head".into()].to_vec();
    let visible: Vec<usize> = vec![0, 1];
    let plan = plan_restore(
        &[
            ("back".to_string(), "back_pack".to_string()),
            ("head".to_string(), "cap".to_string()),
        ],
        &ids,
        &slots,
        &visible,
    );
    assert_eq!(
        plan.wear,
        Some(0),
        "取行序第一件（back 排在 head 前，因为那个 ORDER BY）"
    );
    assert_eq!(plan.notes.len(), 1);
    assert!(
        plan.notes[0].contains("M1") && plan.notes[0].contains("留着"),
        "要说「这一件留在库里没删」，删掉等于丢用户没让我丢的东西：{:?}",
        plan.notes
    );
}

/// [`plan_pick_wear`] 的**全划分**：8 格真值表每一格都落进三档之一，三档各被至少一格打到，
/// 而"先比后写"那条次序就住在这张表里。
///
/// 抄 [`exemption_needs_the_flag_and_a_real_store`] 那种"四格真值表逐格点名"的写法。
/// 两条最容易写反的格子单独钉：② `worn=false` 时下标相等**不是**"已经穿着它"（那是"没穿
/// 任何东西而选中格恰好指着它"⇒ 必须真装）；④ 如果调用方（两家客户端的 `pick_and_wear`）
/// 先写 `picked` 再比，这一格（最常见的那一次"换一件别的"）就变成 `AlreadyWorn` ⇒ 静默不动 +
/// 一句谎话。
#[test]
fn plan_pick_wear_partitions_the_three_outcomes() {
    let mut refused = 0;
    let mut already = 0;
    let mut wear = 0;
    for gate in [false, true] {
        for worn in [false, true] {
            for (picked, want) in [(3usize, 3usize), (3, 4)] {
                match plan_pick_wear(gate, worn, picked, want) {
                    PickWear::Refused => refused += 1,
                    PickWear::AlreadyWorn => already += 1,
                    PickWear::SelectAndWear => wear += 1,
                }
            }
        }
    }
    assert_eq!(refused + already + wear, 8, "八格真值表有格子没落进三档");
    assert_eq!(
        (refused, already, wear),
        (4, 1, 3),
        "三档的分布变了 ⇒ 有人动了判定次序或把某档合并了"
    );

    // ① 闸门没过 ⇒ 无论身上有什么、下标等不等，一律不动。这是"失败关闭"那一侧。
    for worn in [false, true] {
        for (picked, want) in [(3usize, 3usize), (3, 4)] {
            assert_eq!(
                plan_pick_wear(false, worn, picked, want),
                PickWear::Refused,
                "库存没过还去装载 = 换装页那道闸在这里被绕过"
            );
        }
    }
    // ② 身上没东西 ⇒ 哪怕选中的就是它，也得真装一次（否则"第一次点收藏册"什么都不发生）。
    assert_eq!(
        plan_pick_wear(true, false, 3, 3),
        PickWear::SelectAndWear,
        "未装备态点同一格被读成'已经穿着它'"
    );
    // ③ 穿着的就是它 ⇒ 不解像素、不重投帧，但必须说话。
    assert_eq!(plan_pick_wear(true, true, 3, 3), PickWear::AlreadyWorn);
    // ④ 穿着别的那件 ⇒ 换。
    assert_eq!(plan_pick_wear(true, true, 3, 4), PickWear::SelectAndWear);
}

/// **`body` 那一行不是配件的账**（P5 给启动恢复补的那道剥壳，J3/M6 的读数）。
///
/// 这条不测"剥得对"，测的是**不剥会说出哪句假话** —— 那句在 headless 里除了这里没有别的读数，
/// 而它是真机上会被当成"功能没生效"的误导：`磁盘上找不到这一件（已删除，或门禁没过）`
/// 说的是一行**根本不是配件**的账，而且**每一轮启动都打**。
///
/// ⚠️ 明写这条**不**测的那一件：不剥**不会**丢帽子。[`plan_restore`] 取的是"第一条过了四道
/// 的行"，而本体行在第 1 道就被 `continue` 掉，一次都不占 `wear`。下面那两条 `wear` 断言因此
/// 是**同一件事的两面**（剥与不剥都穿上那一件）—— 它守的是"剥壳别把配件行也剥掉"，不是"少剥
/// 就丢装备"。这条剥壳的全部意义是**不说假话**。
#[test]
fn a_body_row_is_not_an_accessory_note() {
    let rows = vec![
        ("body".to_string(), "tiger".to_string()),
        ("head".to_string(), "ballcap".to_string()),
    ];
    let ids: Vec<String> = ["ballcap"].iter().map(|s| s.to_string()).collect();
    let slots: Vec<String> = ["head"].iter().map(|s| s.to_string()).collect();
    let visible = vec![0usize];

    // 剥壳本身：`(本体 id, 只剩配件的行)`，两条都不许丢。
    let (body, accessories) = split_body_row(&rows);
    assert_eq!(body.as_deref(), Some("tiger"), "body 那一行没被认出来");
    assert_eq!(
        accessories,
        vec![("head".to_string(), "ballcap".to_string())],
        "剥壳把配件那一行也剥掉了 ⇒ 帽子从此不会恢复"
    );

    // ✅ 剥完之后：帽子是帽子、本体是本体，两句噪音都不该有。
    let plan = plan_restore(&accessories, &ids, &slots, &visible);
    assert_eq!(plan.wear, Some(0), "配件行被 body 行的存在影响到了");
    assert!(
        plan.notes.is_empty(),
        "剥干净了却还有说明：{:?}",
        plan.notes
    );

    // ❌ 不剥（这是 M6 那一刀）：那句假话必须**当场**打得出来，否则这条门禁就是在念注释。
    let bad = plan_restore(&rows, &ids, &slots, &visible);
    assert_eq!(
        bad.wear,
        Some(0),
        "不剥反而丢了装备 ⇒ `plan_restore` 第 4 条那条取首条的规则变了，上面那段\n\
         「本体行不会把帽子挤掉」的论证作废，这条得重新想（这是本轮唯一能读到它变没变的地方）"
    );
    assert!(
        bad.notes
            .iter()
            .any(|n| n.contains("磁盘上找不到这一件") && n.contains("tiger")),
        "不剥却没有那句假话：{:?}",
        bad.notes
    );

    // 只有 body 行（上次只换了本体、没戴帽子）⇒ 配件侧**一行说明都不许有**。
    let only_body = vec![("body".to_string(), "tiger".to_string())];
    let (b, acc) = split_body_row(&only_body);
    assert_eq!(b.as_deref(), Some("tiger"));
    let plan = plan_restore(&acc, &ids, &slots, &visible);
    assert_eq!(plan.wear, None, "只剩本体行却还要穿一件");
    assert!(
        plan.notes.is_empty(),
        "只剩本体行却给出了配件说明：{:?}",
        plan.notes
    );
}
