//! `equipped` 那张表从 P3 起**有读有写**（此前全仓库零读零写，见 FLOW §2 与 AGENTS 事实 7）。
//!
//! 这个文件钉的是**库侧**的四件事。屏幕侧（恢复、五道重跑、未拥有出局）在
//! `overlay-wayland` 的 `app::tests`，因为那些判定要同时看到磁盘与库存。
//!
//! 1. **一个槽位只留一件**：换帽子是覆盖，不是加一行 ⇒ "上次穿的"永远只有一个答案。
//! 2. **读出来是确定序**：M1 内存档只容一件，多行时恢复取第一件；没有 `ORDER BY` 的"第一件"
//!    就是同一台机器两次启动穿两顶不同的帽子（硬约束 8）。
//! 3. **写 `equipped` 不给所有权**：`inventory` 一字不动。拥有那道立在**读侧**（恢复时按
//!    `owned()` 出局），不在写侧 —— 写侧加守卫只会多出第二套答案。
//! 4. **落的是库不是连接**：关掉重开还读得回来（J3 在库层的等价物）。

use petcore::gacha::Currency;
use petstore::{DEFAULT_PROFILE, Store};

fn store() -> Store {
    Store::in_memory().unwrap()
}

/// 同槽覆盖、异槽互不相干（PK `(profile_id, slot)` 的两面）。
#[test]
fn one_slot_holds_exactly_one_item_and_the_others_are_untouched() {
    let mut s = store();
    s.equip(DEFAULT_PROFILE, "head", "cap").unwrap();
    s.equip(DEFAULT_PROFILE, "neck", "scarf").unwrap();
    // 换帽子：覆盖
    s.equip(DEFAULT_PROFILE, "head", "top_hat").unwrap();
    assert_eq!(
        s.equipped(DEFAULT_PROFILE).unwrap(),
        vec![
            ("head".into(), "top_hat".into()),
            ("neck".into(), "scarf".into()),
        ],
        "head 只该留最后一件，而 neck 不该被这次覆盖带走"
    );

    // 脱下只删那一格
    assert_eq!(
        s.unequip(DEFAULT_PROFILE, "head").unwrap(),
        1,
        "穿着时脱下该删掉一行"
    );
    assert_eq!(
        s.equipped(DEFAULT_PROFILE).unwrap(),
        vec![("neck".into(), "scarf".into())]
    );
    assert_eq!(
        s.unequip(DEFAULT_PROFILE, "head").unwrap(),
        0,
        "本来没穿 ⇒ 报 0 行，调用方决定要不要出声（不是错误）"
    );
}

/// **确定序**：写入顺序是 `neck/head/back`，读出来必须按 `(slot, item_id)` 升序。
///
/// 这条不是排版洁癖：恢复时"库里有多行而 M1 只容一件"要取第一件，取哪件必须是可重放的。
#[test]
fn the_read_is_ordered_regardless_of_write_order() {
    let mut s = store();
    for (slot, id) in [
        ("neck", "n1"),
        ("head", "h2"),
        ("back", "b1"),
        ("head", "h1"),
    ] {
        // head 连写两件 = 覆盖，最后一件是 h1
        s.equip(DEFAULT_PROFILE, slot, id).unwrap();
    }
    assert_eq!(
        s.equipped(DEFAULT_PROFILE).unwrap(),
        vec![
            ("back".into(), "b1".into()),
            ("head".into(), "h1".into()),
            ("neck".into(), "n1".into()),
        ]
    );
}

/// 两个 profile 各穿各的（恢复只读 `DEFAULT_PROFILE` 那一格）。
#[test]
fn profiles_do_not_see_each_others_clothes() {
    let mut s = store();
    s.equip(DEFAULT_PROFILE, "head", "mine").unwrap();
    s.equip("other", "head", "theirs").unwrap();
    assert_eq!(
        s.equipped(DEFAULT_PROFILE).unwrap(),
        vec![("head".into(), "mine".into())]
    );
    assert_eq!(
        s.equipped_slot("other", "head").unwrap().as_deref(),
        Some("theirs")
    );
    assert_eq!(s.equipped_slot(DEFAULT_PROFILE, "face").unwrap(), None);
}

/// **写 `equipped` 不产生所有权**（J2 的写侧反面：这条表能记着库存里没有的那件，
/// 所以守卫必须立在读侧）。同时 `shards` 也不该动 —— 穿上不是消费。
#[test]
fn wearing_grants_nothing() {
    let mut s = store();
    let owned_before = s.owned().unwrap();
    let shards_before = s.shards(Currency::Shards).unwrap();
    s.equip(DEFAULT_PROFILE, "head", "not_in_inventory")
        .unwrap();
    assert_eq!(
        s.owned().unwrap(),
        owned_before,
        "往 equipped 写一行不该让谁变成已拥有 ⇒ 拥有那道只在库存"
    );
    assert_eq!(
        s.shards(Currency::Shards).unwrap(),
        shards_before,
        "穿上不是消费"
    );
    assert!(
        s.equipped(DEFAULT_PROFILE)
            .unwrap()
            .contains(&("head".into(), "not_in_inventory".into())),
        "写侧不设库存守卫 ⇒ 这一行要真在库里，读侧那道才有东西可拦（否则测的是空集）"
    );
}

/// `equipped.slot` **没有 CHECK**（FLOW §5 第 2 条核实过）。这里把它钉成事实而不是注释：
/// `slot='body'` 写得进去 ⇒ P5「本体换装」的接口位能落在同一张表；而配件的合法五值仍由
/// 清单那一侧的 `Slot` 枚举把关，**不是**由这张表把关。
#[test]
fn the_slot_column_accepts_body_by_design() {
    let mut s = store();
    s.equip(DEFAULT_PROFILE, "body", "tiger")
        .expect("`equipped.slot` 没有 CHECK ⇒ `body` 语法合法（P5 接口位的前提）");
    assert_eq!(
        s.equipped(DEFAULT_PROFILE).unwrap(),
        vec![("body".into(), "tiger".into())]
    );
}

/// **落的是库，不是连接**：同一个文件关掉重开还读得回来。
///
/// J3（"重启后还穿着"）在库层的全部前提就是这一条。它故意不用 `in_memory()`：
/// 内存库能覆盖上面五条而**一条都覆盖不到这条**。
#[test]
fn the_row_survives_reopening_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opet.db");
    {
        let mut s = Store::open(&path).unwrap();
        s.equip(DEFAULT_PROFILE, "head", "cap").unwrap();
    }
    let mut again = Store::open(&path).unwrap();
    assert_eq!(
        again.equipped(DEFAULT_PROFILE).unwrap(),
        vec![("head".into(), "cap".into())],
        "关掉重开就读不到 ⇒ 屏幕上的帽子永远不可能在重启后还在"
    );
    // 恢复完了要能脱下，否则这一格永远删不掉
    assert_eq!(again.unequip(DEFAULT_PROFILE, "head").unwrap(), 1);
    assert!(again.equipped(DEFAULT_PROFILE).unwrap().is_empty());
}
