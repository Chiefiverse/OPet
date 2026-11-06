//! 装包 = 所有权的**第二条路径**（FLOW §3.1：`obtained_from = 'pack:<包 id>'`）。
//!
//! 这一文件钉的是 [`Store::grant_pack_items`] 的四件事，每件事背后都是一个被利用的形状：
//!
//! 1. **写入的确实是库存**，而且 `owned()` 立刻读得到 ⇒ 换装页按 `owned()` 过滤（P2）才有东西可过。
//! 2. **反复装卸不产生碎片**：`duplicates` 永远是 0、`count` 永远不 +1。`duplicates` 是碎片的
//!    来源账（§3.4），而碎片能自选 SSR ⇒ 这条一旦能 +1，"装同一个包两次"就是刷碎片的机器。
//! 3. **包内容不许在随机池里**（§3.11.5 + 用户 2026-11-05 第 1 问"池全免费 + 付费包卖池外内容"）。
//!    ⚠️ 光测"带池内 id 的包被拒"证明不了门禁存在 —— 它可能只是碰巧被别的检查先拦住。
//!    所以配套一条**反向**：同一个 id 在**空目录**下必须放行。两合一才说明拒的是"在池里"这件事
//!    （本项目那条"门禁必须能红"的方法论，第五层元测试的同族）。
//! 4. **拒绝不留半个字**：所有校验都在事务之前，被拒的包一行都不写；而**整包一个事务**，
//!    半包入库比不入库更糟（换装页会列出像素根本不存在的那件 —— §9.2 第 20 条的空列表的反面）。

use petcore::gacha::{CATALOG_FORMAT_V1, Catalog, Currency, ItemKind};
use petstore::{PackGrant, Store, StoreError};

/// 池外内容：真实目录（`cat_series_01.json`，30 件）里没有这两个 id。
const PACK_CAT: &str = "pack_demo_mochi";
const PACK_HAT: &str = "pack_hat_red";
const PACK_HAT2: &str = "pack_hat_blue";

fn store() -> Store {
    Store::in_memory().unwrap()
}

fn items(list: &[(&str, &str)]) -> Vec<(ItemKind, String)> {
    list.iter()
        .map(|(k, id)| (ItemKind::parse(k).unwrap(), (*id).to_string()))
        .collect()
}

fn duplicates_of(store: &Store, id: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT duplicates FROM inventory WHERE item_id = ?1",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
}

fn obtained_from(store: &Store, id: &str) -> String {
    store
        .conn()
        .query_row(
            "SELECT obtained_from FROM inventory WHERE item_id = ?1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
}

#[test]
fn a_granted_pack_shows_up_in_inventory_and_in_owned() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    let g = s
        .grant_pack_items(
            "demo_01",
            &catalog,
            &items(&[("pet", PACK_CAT), ("accessory", PACK_HAT)]),
        )
        .unwrap();
    assert_eq!(
        g,
        PackGrant {
            added: vec![format!("accessory/{PACK_HAT}"), format!("pet/{PACK_CAT}")],
            already: vec![],
        },
        "回执按 'kind/id' 排序 ⇒ 同一包两次安装打出同一行"
    );

    let owned = s.owned().unwrap();
    assert!(owned.contains(PACK_CAT) && owned.contains(PACK_HAT));
    assert_eq!(owned.len(), 2, "包里两件 ⇒ 库存就两件");
    assert_eq!(s.item_count(PACK_CAT).unwrap(), 1);
    assert_eq!(obtained_from(&s, PACK_CAT), "pack:demo_01");
    assert_eq!(
        duplicates_of(&s, PACK_CAT),
        0,
        "装包永不是'重复'，它是明码标价的获得"
    );
    assert_eq!(
        s.pack_items("demo_01").unwrap(),
        vec![
            (ItemKind::Accessory, PACK_HAT.to_string()),
            (ItemKind::Pet, PACK_CAT.to_string()),
        ]
    );
}

#[test]
fn reinstalling_the_same_pack_farms_nothing() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    let one = items(&[("pet", PACK_CAT), ("accessory", PACK_HAT)]);
    s.grant_pack_items("demo_01", &catalog, &one).unwrap();
    let shards_before = s.shards(Currency::Shards).unwrap();

    let again = s.grant_pack_items("demo_01", &catalog, &one).unwrap();
    assert_eq!(again.added, Vec::<String>::new(), "第二遍什么都不该新增");
    assert_eq!(
        again.already,
        vec![format!("accessory/{PACK_HAT}"), format!("pet/{PACK_CAT}")],
        "已有的那两件要**点名**，安装器才能如实说'这个包你已经有了'"
    );

    assert_eq!(s.item_count(PACK_CAT).unwrap(), 1, "count 不随重复装包累加");
    assert_eq!(duplicates_of(&s, PACK_CAT), 0);
    assert_eq!(
        s.shards(Currency::Shards).unwrap(),
        shards_before,
        "碎片一枚没动 ⇒ 装卸循环不是刷碎片的机器"
    );
    assert_eq!(s.owned().unwrap().len(), 2);
}

#[test]
fn a_partially_held_pack_reports_both_halves() {
    // 用户手上已经抽到过 PACK_HAT（这里用直写模拟：路径不同，落库形状相同），
    // 再买整包 ⇒ 已有的进 already、缺的进 added，一条都不写重。
    let mut s = store();
    s.conn()
        .execute(
            "INSERT INTO inventory(kind, item_id, count, duplicates, obtained_from)
             VALUES ('accessory', ?1, 1, 0, 'gacha:cat_series#7')",
            [PACK_HAT],
        )
        .unwrap();
    let catalog = Catalog::default_v1().unwrap();
    let g = s
        .grant_pack_items(
            "demo_01",
            &catalog,
            &items(&[("pet", PACK_CAT), ("accessory", PACK_HAT)]),
        )
        .unwrap();
    assert_eq!(g.added, vec![format!("pet/{PACK_CAT}")]);
    assert_eq!(g.already, vec![format!("accessory/{PACK_HAT}")]);
    assert_eq!(s.item_count(PACK_HAT).unwrap(), 1);
    assert_eq!(
        obtained_from(&s, PACK_HAT),
        "gacha:cat_series#7",
        "抽到的那件不能被装包覆盖成 pack: ⇒ 两条所有权路径各记各的"
    );
}

#[test]
fn a_pack_item_that_is_in_the_gacha_pool_is_rejected() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    let in_pool = catalog.pets().next().unwrap().id.clone();
    let err = s
        .grant_pack_items(
            "demo_01",
            &catalog,
            &items(&[("pet", &in_pool), ("accessory", PACK_HAT)]),
        )
        .unwrap_err();
    match err {
        StoreError::PackItemInGachaPool(ids) => {
            assert_eq!(ids, vec![format!("pet/{in_pool}")], "要点名到具体是哪一件")
        }
        other => panic!("本该是 PackItemInGachaPool，实际 {other}"),
    }
    assert!(
        s.owned().unwrap().is_empty(),
        "整包拒绝 ⇒ 包里那件合规的 {PACK_HAT} 也一个字没写"
    );
}

#[test]
fn the_same_id_off_pool_is_fine_so_the_rejection_above_is_the_pool_check() {
    // 反向半条：上一条里被拒的那个 id 换个名字（同 kind、同结构）必须放行。
    // 没有这条，上一条可能红在别处（比如 kind 解析），那条门禁就是装饰。
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    let in_pool = catalog.pets().next().unwrap().id.clone();
    let renamed = format!("outside_{in_pool}");
    assert!(catalog.find(&renamed).is_none());
    let g = s
        .grant_pack_items("demo_01", &catalog, &items(&[("pet", &renamed)]))
        .unwrap();
    assert_eq!(g.added, vec![format!("pet/{renamed}")]);
}

#[test]
fn the_gate_reads_the_catalog_it_is_given() {
    // 上一条的反面：门禁查的是**传进来的那份目录**，不是一份写死的名单。
    // 空目录 ⇒ 同一个池内 id 放行。这一条红 = 有人把"在不在池里"实现成了"像不像池里的名字"。
    let mut s = store();
    let pool_id = Catalog::default_v1()
        .unwrap()
        .pets()
        .next()
        .unwrap()
        .id
        .clone();
    let no_pool = Catalog {
        format: CATALOG_FORMAT_V1,
        id: "empty".into(),
        banner_id: "empty".into(),
        items: vec![],
    };
    assert!(no_pool.find(&pool_id).is_none());
    let g = s
        .grant_pack_items("demo_01", &no_pool, &items(&[("pet", &pool_id)]))
        .unwrap();
    assert_eq!(g.added, vec![format!("pet/{pool_id}")]);
}

#[test]
fn an_unsafe_item_id_is_rejected_before_the_transaction() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    for bad in ["../etc/passwd", "/abs/path", "a//b", ""] {
        let err = s
            .grant_pack_items("demo_01", &catalog, &items(&[("pet", bad)]))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::BadPack(_)),
            "{bad:?} 本该走 BadPack，实际 {err}"
        );
    }
    assert!(s.owned().unwrap().is_empty());
}

#[test]
fn an_unsafe_pack_id_is_rejected() {
    // 包 id 会被安装器当目录名与 `.installed_by/<id>` 文件名用 ⇒ 同一条规则，写在写入点。
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    for bad in ["../outsider", "/tmp/x", ""] {
        assert!(
            matches!(
                s.grant_pack_items(bad, &catalog, &items(&[("pet", PACK_CAT)]))
                    .unwrap_err(),
                StoreError::BadPack(_)
            ),
            "包 id {bad:?} 本该被拒"
        );
    }
    assert!(s.owned().unwrap().is_empty());
}

#[test]
fn an_empty_pack_is_rejected() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    assert!(
        matches!(
            s.grant_pack_items("demo_01", &catalog, &[]).unwrap_err(),
            StoreError::BadPack(_)
        ),
        "0 件的包卖的是什么"
    );
}

#[test]
fn one_id_under_two_kinds_is_rejected() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    let err = s
        .grant_pack_items(
            "demo_01",
            &catalog,
            &items(&[("pet", PACK_HAT), ("accessory", PACK_HAT)]),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::BadPack(_)), "实际 {err}");
    assert!(s.owned().unwrap().is_empty());
}

#[test]
fn a_duplicate_line_in_one_pack_is_folded_not_rejected() {
    // 同一件列两次是**打包侧的冗余**，不是模糊所有权（kind 相同）⇒ 折叠成一条，不报错。
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    let g = s
        .grant_pack_items(
            "demo_01",
            &catalog,
            &items(&[("pet", PACK_CAT), ("pet", PACK_CAT)]),
        )
        .unwrap();
    assert_eq!(g.added, vec![format!("pet/{PACK_CAT}")]);
    assert_eq!(s.owned().unwrap().len(), 1);
}

#[test]
fn uninstall_removes_only_this_packs_rows() {
    let mut s = store();
    let catalog = Catalog::default_v1().unwrap();
    s.conn()
        .execute(
            "INSERT INTO inventory(kind, item_id, count, duplicates, obtained_from)
             VALUES ('pet', ?1, 1, 0, 'gacha:cat_series#7')",
            ["cat_shiro"],
        )
        .unwrap();
    s.grant_pack_items(
        "demo_01",
        &catalog,
        &items(&[("pet", PACK_CAT), ("accessory", PACK_HAT)]),
    )
    .unwrap();
    s.grant_pack_items("demo_02", &catalog, &items(&[("accessory", PACK_HAT2)]))
        .unwrap();

    assert_eq!(s.revoke_pack_items("demo_01").unwrap(), 2);
    let owned = s.owned().unwrap();
    assert!(owned.contains("cat_shiro"), "抽来的那件一件没动");
    assert!(owned.contains(PACK_HAT2), "另一个包的那件也没动");
    assert!(!owned.contains(PACK_CAT) && !owned.contains(PACK_HAT));
    assert_eq!(s.pack_items("demo_01").unwrap().len(), 0);

    // 重新装包 ⇒ 立刻恢复（撤销 ≠ 删文件；§3.4 的退款语义在账本这一侧的样子）
    let g = s
        .grant_pack_items("demo_01", &catalog, &items(&[("pet", PACK_CAT)]))
        .unwrap();
    assert_eq!(g.added, vec![format!("pet/{PACK_CAT}")]);
    assert_eq!(s.revoke_pack_items("no_such_pack").unwrap(), 0);
}
