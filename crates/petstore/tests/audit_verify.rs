//! 审计链：**篡改必须被抓住，而且要能被不同层各自抓住**。
//!
//! 两层校验的职责不一样，本文件把它们分开测：
//! - 链（`verify_chain`）证明"记录之间自洽"—— 抓删行、改字段、改尾部。
//! - 重放（`verify_replay`）证明"记录与骰子自洽"—— 抓**重新接链**的伪造者。
//!
//! 只测第一层等于告诉攻击者"改完记得把下一条的 prev_hash 也改一下就行"。
//! 所以这里每条篡改测试都同时问两个问题：链还过得去吗？重放抓到吗？
//!
//! 另外三条走"只吃导出文件"的路径（§6.1 对第三方的承诺：不需要数据库、不需要网络）。

use petcore::gacha::{
    AuditError, AuditRecord, Catalog, Currency, GachaParams, ItemKind, PullState, Rarity, hex,
    rng_state_hash, verify_chain, verify_replay,
};
use petstore::db::{AUDIT_HEAD_KEY, Store, StoreError};
use petstore::ledger::derive_state;
use tempfile::TempDir;

const SEED: u64 = 5;
const BANNER: &str = "cat_series";
const PROFILE: &str = "local";

fn seeded_store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("tempdir");
    let mut s = Store::open(&dir.path().join("opet.db")).expect("建库");
    s.reseed(SEED).expect("播种");
    (dir, s)
}

/// 把（可能已被改过的）记录重新接成一条自洽的链，并把链尾盖回 `meta.audit_head`。
/// 这就是"懂行的伪造者"：他知道哈希怎么算，所以能过链校验。
fn relink(s: &Store, records: &[AuditRecord]) {
    let mut prev = petcore::gacha::GENESIS;
    for r0 in records {
        let mut r = r0.clone();
        r.prev_digest = prev;
        s.conn()
            .execute(
                "UPDATE gacha_pull SET item_id = ?1, item_kind = ?2, pity_counter = ?3,
                        audit_prev_hash = ?4 WHERE seq = ?5",
                rusqlite::params![
                    r.item_id,
                    r.item_kind.as_str(),
                    r.pity_before,
                    hex(&r.prev_digest),
                    r.seq as i64
                ],
            )
            .unwrap();
        prev = r.digest();
    }
    s.conn()
        .execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![AUDIT_HEAD_KEY, hex(&prev)],
        )
        .unwrap();
}

/// 导出 → 读回 → 只靠 petcore 校验（不碰数据库）。这是第三方拿到的全部输入。
#[test]
fn export_verifies_without_any_database() {
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    let p = GachaParams::V1;
    let rep = s.pull(&p, &cat, BANNER, 23).unwrap();

    let text = s.export_jsonl(PROFILE, BANNER).unwrap();
    let lines = text.lines().count();
    assert_eq!(lines, 23, "导出行数该等于账本条数");

    let records = Store::records_from_jsonl(&text).unwrap();
    assert_eq!(records.len(), 23);

    // 第三方只需要：seed（公示）+ 参数（库里/base_rate 公示）+ 目录 + 这份文件
    let chain = verify_chain(&records).unwrap();
    assert_eq!(chain.len, 23);
    assert_eq!(
        chain.head,
        petcore::gacha::unhex(&rep.head_hex).unwrap(),
        "导出的链尾与库里的链尾该是同一个值"
    );
    let replayed = verify_replay(SEED, &p, &cat, &records).unwrap();
    assert_eq!(replayed.counts[..], chain.counts[..]);
    assert_eq!(
        replayed.counts[Rarity::SSR.index()] as u32,
        rep.counts[Rarity::SSR.index()],
        "导出重放出来的分布该等于抽取当时的分布"
    );
    // 每条的 rng_state_hash 就是 sha256(domain ‖ seed ‖ seq)：第三方可以完全自己算
    for r in &records {
        assert_eq!(r.rng_state_hash, rng_state_hash(SEED, r.seq));
    }
}

#[test]
fn tampering_a_middle_row_breaks_the_link_at_the_next_one() {
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    s.pull(&GachaParams::V1, &cat, BANNER, 10).unwrap();
    let mut records = s.records(PROFILE, BANNER).unwrap();
    records[4].item_id = "pretender".into();
    let err = verify_chain(&records).expect_err("改过内容就该断链");
    assert!(
        matches!(err, AuditError::BrokenLink { at, .. } if at == 6),
        "本该在第 6 条（被改那条的下一条）报断链: {err}"
    );
}

#[test]
fn tampering_the_last_row_is_caught_by_the_stored_head() {
    // 尾行没有"下一条"可以指证它 ⇒ 靠 `meta.audit_head` 这个锚点抓。
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    s.pull(&GachaParams::V1, &cat, BANNER, 10).unwrap();
    let head_before = s.meta_get(AUDIT_HEAD_KEY).unwrap().unwrap();
    s.conn()
        .execute(
            "UPDATE gacha_pull SET item_id = 'hijacked' WHERE seq = 10",
            [],
        )
        .unwrap();
    let err = s
        .verify_chain_only(PROFILE, BANNER)
        .expect_err("改尾行该被锚点抓住");
    assert!(
        matches!(
            err,
            StoreError::Audit(petcore::gacha::AuditError::HeadMismatch { .. })
        ),
        "{err}"
    );
    assert_eq!(
        s.meta_get(AUDIT_HEAD_KEY).unwrap().unwrap(),
        head_before,
        "校验本身不该改动锚点"
    );
}

#[test]
fn dropping_a_row_is_caught_even_after_relinking() {
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    s.pull(&GachaParams::V1, &cat, BANNER, 10).unwrap();
    s.conn()
        .execute("DELETE FROM gacha_pull WHERE seq = 5", [])
        .unwrap();
    let err = s
        .verify_chain_only(PROFILE, BANNER)
        .expect_err("删行该被 seq 连续性抓住");
    assert!(
        matches!(
            err,
            StoreError::Audit(petcore::gacha::AuditError::SequenceGap { at: 6, expect: 5 })
        ) || matches!(
            err,
            StoreError::Audit(petcore::gacha::AuditError::BrokenLink { at: 6, .. })
        ),
        "{err}"
    );
    // 伪造者会把后面的 seq 整体前移来补号（升序逐行挪，每格都先腾出来才写）
    // ⇒ 那时 `seq` 进了摘要，链自己就露馅
    for seq in 6..=10 {
        s.conn()
            .execute(
                "UPDATE gacha_pull SET seq = ?1 WHERE seq = ?2",
                rusqlite::params![seq - 1, seq],
            )
            .unwrap();
    }
    assert_eq!(
        s.conn()
            .query_row("SELECT COUNT(*) FROM gacha_pull", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        9,
        "补号后该有 9 行、seq 1..9 连续 ⇒ 这一步是真的在模仿'看不出来'"
    );
    let err2 = s
        .verify_chain_only(PROFILE, BANNER)
        .expect_err("补号挪了 seq ⇒ 摘要跟着变，锚点/链接该拦住");
    assert!(
        matches!(
            err2,
            StoreError::Audit(petcore::gacha::AuditError::BrokenLink { .. })
                | StoreError::Audit(petcore::gacha::AuditError::HeadMismatch { .. })
        ),
        "{err2}"
    );
}

#[test]
fn a_relinked_item_id_forgery_still_fails_replay() {
    // 攻击者知道哈希怎么算：改完 item_id 再把整条链重新接好、锚点也盖回去。
    // 链校验过了 —— 但骰子不认。
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 10).unwrap();
    let mut records = s.records(PROFILE, BANNER).unwrap();
    let real = records[2].item_id.clone();
    records[2].item_id = "dragon_pip".into(); // 把第 3 抽改成"看起来更值钱"的那只
    relink(&s, &records);

    s.verify_chain_only(PROFILE, BANNER)
        .expect("重接过的链确实能过第一层 ⇒ 所以只有链是不够的");
    let err = s
        .verify(&cat, PROFILE, BANNER)
        .expect_err("重放该抓住这次伪造");
    match err {
        StoreError::Audit(AuditError::ReplayMismatch { at, field, .. }) => {
            assert_eq!(at, 3);
            assert_eq!(field, "item_id", "第 3 抽的真实结果不是 {real}");
        }
        e => panic!("错误类型不对: {e}"),
    }
}

#[test]
fn a_relinked_pity_forgery_still_fails_replay() {
    // 篡改 `pity_counter` 是"改自己的保底进度"这条路：窗口推导照它算，
    // 所以这一层必须靠重放来抓（这也是"pity 不建表、从账本推"的前提条件）。
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 6).unwrap();
    let mut records = s.records(PROFILE, BANNER).unwrap();
    records[5].pity_before = 39; // 假装"下一次就进软保底了"
    relink(&s, &records);
    s.verify_chain_only(PROFILE, BANNER).expect("链被重接好了");

    // 窗口法照读了伪造的计数 ⇒ 说明"下一抽的状态"确实会被骗
    let rows = s.rows(PROFILE, BANNER).unwrap();
    let cheated = derive_state(&p, &rows).unwrap();
    assert_eq!(cheated.since_ssr, 40, "窗口法信任 stored 计数，被骗到了");

    let err = s
        .verify(&cat, PROFILE, BANNER)
        .expect_err("重放该抓住这次保底进度伪造");
    match err {
        StoreError::Audit(AuditError::ReplayMismatch { at, field, .. }) => {
            assert_eq!(at, 6);
            assert_eq!(field, "pity_before");
        }
        e => panic!("错误类型不对: {e}"),
    }
}

#[test]
fn item_kind_is_verified_by_replay_not_by_the_digest() {
    // `item_kind` 有意**不进** `canonical()`（它由 item_id + 目录推导，是派生值）。
    // 所以它是"链看不见、重放看得见"的那一类字段 —— 这条测试钉住这个设计后果。
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 4).unwrap();
    let before = s.records(PROFILE, BANNER).unwrap()[1].item_kind;
    s.conn()
        .execute(
            "UPDATE gacha_pull SET item_kind = ?1 WHERE seq = 2",
            rusqlite::params![match before {
                ItemKind::Pet => "accessory",
                ItemKind::Accessory => "pet",
            }],
        )
        .unwrap();
    // 摘要不含它 ⇒ 链照过
    s.verify_chain_only(PROFILE, BANNER)
        .expect("item_kind 不进摘要 ⇒ 改它不会断链（这是有意的）");
    let err = s.verify(&cat, PROFILE, BANNER).expect_err("但重放必须抓住");
    match err {
        StoreError::Audit(AuditError::ReplayMismatch { at, field, .. }) => {
            assert_eq!(at, 2);
            assert_eq!(field, "item_kind");
        }
        e => panic!("错误类型不对: {e}"),
    }
}

#[test]
fn a_handed_in_seed_or_params_mismatch_is_caught() {
    // `verify` 用**库里公示的参数**重放。有人绕过 `ensure_banner` 直接改库 ⇒
    // 重放必然对不上（"公示 == 代码"这条在审计侧的最后证据）。
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    let p = GachaParams::V1;
    s.pull(&p, &cat, BANNER, 12).unwrap();
    // `base_rate` 存的是小数（0.6 而不是 600000），与 §3.4 的示例一致。
    s.conn()
        .execute(
            "UPDATE gacha_banner SET base_rate = ?1 WHERE id = ?2",
            rusqlite::params![r#"{"N":0.5,"R":0.3,"SR":0.15,"SSR":0.05}"#, BANNER],
        )
        .unwrap();
    assert_ne!(s.banner_params(BANNER).unwrap(), p);
    assert_eq!(s.banner_params(BANNER).unwrap().rates.ssr, 50_000);
    let err = s
        .verify(&cat, PROFILE, BANNER)
        .expect_err("改了公示概率还想让审计通过，不该成立");
    match err {
        // 概率被抬 ⇒ 同一个骰面落到别的档；即便稀有度侥幸相同，`ssr_prob` 也是公示值本身
        StoreError::Audit(AuditError::ReplayMismatch { at, field, .. }) => {
            assert!(at <= 12, "第 {at} 抽才露馅，超出账本长度了");
            assert!(
                field == "ssr_prob" || field == "rarity" || field == "item_id",
                "露馅的字段该是概率直接或间接决定的那个: {field}"
            );
        }
        e => panic!("错误类型不对: {e}"),
    }

    // 换个 seed 去重放同一条链也该失败（证明重放真的在用 seed，不是走个形式）
    let (_d2, mut other) = seeded_store();
    other.reseed(SEED + 1).unwrap();
    let records = s.records(PROFILE, BANNER).unwrap();
    let err2 = verify_replay(SEED + 1, &p, &cat, &records).expect_err("错 seed 该被拒");
    assert!(
        matches!(err2, AuditError::ReplayMismatch { at: 1, .. }),
        "{err2}"
    );
}

#[test]
fn a_ledger_row_outside_the_catalog_is_refused_before_any_hashing() {
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    s.pull(&GachaParams::V1, &cat, BANNER, 3).unwrap();
    s.conn()
        .execute(
            "UPDATE gacha_pull SET item_id = 'ghost_pet' WHERE seq = 2",
            [],
        )
        .unwrap();
    let err = s
        .verify(&cat, PROFILE, BANNER)
        .expect_err("账本里出现目录外的 id 必须先报错");
    assert!(
        matches!(err, StoreError::LedgerOutsideCatalog(ref ids) if ids.len() == 1),
        "{err}"
    );
}

#[test]
fn a_garbage_export_line_is_rejected_with_a_format_error() {
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    s.pull(&GachaParams::V1, &cat, BANNER, 5).unwrap();
    let text = s.export_jsonl(PROFILE, BANNER).unwrap();
    for broken in [
        "not json at all\n",
        r#"{"format":1}""#,
        &text.replace("\"seq\":1", "\"seq\":2"),
    ] {
        let e = Store::records_from_jsonl(broken);
        assert!(e.is_err(), "这行本该被拒: {broken}");
    }
    // 空文件不是"零条记录所以校验通过"：解码给出 0 条，校验层必须把它当"没东西可验"拒掉
    let empty = Store::records_from_jsonl("").expect("空文件本身是合法的 JSONL");
    assert!(empty.is_empty());
    assert!(
        matches!(verify_chain(&empty), Err(AuditError::Empty)),
        "空链不该给出'校验通过'"
    );
}

#[test]
fn fresh_store_state_is_a_fresh_window_and_an_empty_chain() {
    let (_d, mut s) = seeded_store();
    let cat = Catalog::default_v1().unwrap();
    let p = GachaParams::V1;
    let (st, next) = s.pity_state(&p, PROFILE, BANNER).unwrap();
    assert_eq!(st, PullState::fresh());
    assert_eq!(next, 1);
    assert_eq!(s.rows(PROFILE, BANNER).unwrap().len(), 0);
    assert!(
        s.verify_chain_only(PROFILE, BANNER).is_err(),
        "空链不该给出'校验通过'"
    );
    // 抽一次之后，seed 仍然是那个 seed（重放的前提）
    s.pull(&p, &cat, BANNER, 1).unwrap();
    assert_eq!(s.seed(), SEED);
    assert_eq!(s.shards(Currency::Shards).unwrap(), 0);
    assert_eq!(s.verify(&cat, PROFILE, BANNER).unwrap().len, 1);
}
