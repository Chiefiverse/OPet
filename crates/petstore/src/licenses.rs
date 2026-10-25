//! 权益与内容包：**v1 只有 `Noop` 实现，一行 HTTP 都不发**（D2、硬约束 #4）。
//!
//! 这一层的存在有两个理由：
//!
//! 1. **命令位先留好**（§10.2 `opet license sync --dry-run` 打印 "everything free"），
//!    M7/M8 接 itch/Steam/MoR 时只换 [`LicenseService`] 的实现，不动 schema、不动调用方。
//! 2. **红线 1 的结构层**：[`ContentPack`] 是付费内容包的清单类型，它**没有** `rarity`
//!    与 `weight` 字段，并且 `#[serde(deny_unknown_fields)]` ⇒ 带 `"weight"` 的包 JSON
//!    在解析期就失败。"付费只能买明码标价的确定性内容"因此不是靠自觉，而是靠类型（§3.11.4 第 2 条）。
//!
//! 撤销语义（退款 ⇒ `Revoked`）单独钉了测试：撤销**不删文件、不删库存**，
//! 重新购买即恢复 —— 因为"删文件"在用户看来自创损失，而解锁判定本来就是君子锁（硬约束 #6：不做 DRM）。

use rusqlite::{Connection, OptionalExtension, params};

/// 权益状态。与 `entitlements.status` 的 CHECK 三值一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LicenseState {
    Active,
    Revoked,
    Expired,
}

impl LicenseState {
    pub const ALL: [Self; 3] = [Self::Active, Self::Revoked, Self::Expired];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Active)
    }
}

impl std::fmt::Display for LicenseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条权益记录（`entitlements` 行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entitlement {
    pub id: i64,
    pub pack_id: String,
    /// `'purchase:itch#order' | 'redeem:code' | 'free' | 'gacha'`
    pub source: String,
    pub status: LicenseState,
    /// 订单号。**只存引用，不存任何支付凭证明细**（硬约束 #4 的隐私口径）。
    pub external_ref: Option<String>,
    pub acquired_at: i64,
}

/// 付费内容包的清单。
///
/// ⚠️ 这张结构体是红线：**不许**加 `rarity` / `weight` / `pool` / `drop_rate` 之类的字段。
/// 明码标价的包卖的是"确定性内容"，一旦能携带权重，"花钱改变概率"就从数据层进来了。
/// `deny_unknown_fields` 让旧版本或第三方伪造的包 JSON 直接解析失败，而不是"忽略掉"。
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPack {
    pub format: u32,
    pub id: String,
    pub title: String,
    /// `pets | accessories | bundle`
    pub kind: String,
    /// 明码标价（分）。`None` = 免费包。
    #[serde(default)]
    pub price_cents: Option<i64>,
    #[serde(default)]
    pub currency_code: Option<String>,
    /// `itch | steam | mor | free`
    pub store: String,
    pub pack_path: String,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub release_notes: Option<String>,
    #[serde(default = "default_true")]
    pub is_dlc: bool,
    /// 包里的确定性内容（就是这一袋东西，买了就有）。
    pub items: Vec<String>,
}

const fn default_true() -> bool {
    true
}

impl ContentPack {
    pub const FORMAT_V1: u32 = 1;

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// 结构自检：`kind`/`store` 必须落在 DDL 的 CHECK 里，格式版本必须是 1。
    /// （SQLite 的 CHECK 只在写入时生效，这里是"解析后立刻"的第二道。）
    pub fn validate(&self) -> Result<(), String> {
        if self.format != Self::FORMAT_V1 {
            return Err(format!(
                "包格式版本 {} 不被支持（只认 {}）",
                self.format,
                Self::FORMAT_V1
            ));
        }
        if !matches!(self.kind.as_str(), "pets" | "accessories" | "bundle") {
            return Err(format!("kind = {:?} 不在 CHECK 白名单", self.kind));
        }
        if !matches!(self.store.as_str(), "itch" | "steam" | "mor" | "free") {
            return Err(format!("store = {:?} 不在 CHECK 白名单", self.store));
        }
        if self.store == "free" && self.price_cents.unwrap_or(0) != 0 {
            return Err("store=free 的包不能带价格".into());
        }
        if self.store != "free" && self.price_cents.unwrap_or(0) <= 0 {
            return Err("付费包必须给出正的价格（明码标价，不许'0 元抽奖'这种灰区）".into());
        }
        if self.items.is_empty() {
            return Err("包里一件东西都没有 ⇒ 卖的是什么？".into());
        }
        Ok(())
    }
}

/// 联网权益服务。**v1 唯一实现是 [`NoopLicenseService`]。**
///
/// `sync` 的返回类型刻意是"人类可读的一行"而不是结构化差异：v1 它只会打印
/// "noop: everything free"（§10.2:959），做不出结构来；将来接真渠道时再定形状。
pub trait LicenseService {
    /// 干跑还是真同步。`dry_run` 时**不写库、不发请求**。
    fn sync(&self, conn: &Connection, dry_run: bool) -> Result<String, String>;
}

/// v1 的实现：什么都不做，全部视为已拥有。
pub struct NoopLicenseService;

impl LicenseService for NoopLicenseService {
    fn sync(&self, _conn: &Connection, _dry_run: bool) -> Result<String, String> {
        Ok("noop: everything free".into())
    }
}

// ---- 读写路径 ----

/// 记一条包购买/兑换。**幂等**：同一个 `external_ref` 再来一次只把 `Revoked` 恢复成
/// `Active`，不会攒出第二份权益（重新购买即恢复，§3.4 的退款语义）。
pub fn record_entitlement(
    conn: &Connection,
    pack_id: &str,
    source: &str,
    external_ref: Option<&str>,
    ts: i64,
) -> Result<Entitlement, String> {
    if let Some(existing) = find_by_external_ref(conn, external_ref)? {
        if existing.status != LicenseState::Active {
            conn.execute(
                "UPDATE entitlements SET status = 'active', last_sync_at = ?1 WHERE id = ?2",
                params![ts, existing.id],
            )
            .map_err(|e| e.to_string())?;
            return get(conn, existing.id);
        }
        return Ok(existing);
    }
    conn.execute(
        "INSERT INTO entitlements(pack_id, source, status, external_ref, acquired_at, last_sync_at)
         VALUES (?1, ?2, 'active', ?3, ?4, ?4)",
        params![pack_id, source, external_ref, ts],
    )
    .map_err(|e| format!("写入权益失败（pack_id={pack_id} 在 content_packs 里吗？): {e}"))?;
    let id = conn.last_insert_rowid();
    get(conn, id)
}

/// 退款 ⇒ `Revoked`。**不删任何文件、不改库存**（硬约束 #6：没有 DRM）。
pub fn revoke(conn: &Connection, entitlement_id: i64, ts: i64) -> Result<LicenseState, String> {
    let n = conn
        .execute(
            "UPDATE entitlements SET status = 'revoked', last_sync_at = ?1 WHERE id = ?2",
            params![ts, entitlement_id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("没有 id={entitlement_id} 这条权益"));
    }
    Ok(LicenseState::Revoked)
}

pub fn mark_expired(conn: &Connection, entitlement_id: i64, ts: i64) -> Result<(), String> {
    conn.execute(
        "UPDATE entitlements SET status = 'expired', last_sync_at = ?1 WHERE id = ?2",
        params![ts, entitlement_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 这个包当前可用吗（有且只有一条 `active` 就算可用）。
pub fn is_unlocked(conn: &Connection, pack_id: &str) -> Result<bool, String> {
    Ok(conn
        .query_row(
            "SELECT COUNT(*) FROM entitlements WHERE pack_id = ?1 AND status = 'active'",
            params![pack_id],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?
        > 0)
}

pub fn list(conn: &Connection) -> Result<Vec<Entitlement>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, pack_id, source, status, external_ref, acquired_at
             FROM entitlements ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.map(|t| {
        let t = t.map_err(|e| e.to_string())?;
        let status = LicenseState::parse(&t.3)
            .ok_or_else(|| format!("entitlements.status = {:?} 不在 CHECK 白名单", t.3))?;
        Ok(Entitlement {
            id: t.0,
            pack_id: t.1,
            source: t.2,
            status,
            external_ref: t.4,
            acquired_at: t.5,
        })
    })
    .collect()
}

fn get(conn: &Connection, id: i64) -> Result<Entitlement, String> {
    let row = conn
        .query_row(
            "SELECT pack_id, source, status, external_ref, acquired_at FROM entitlements WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(Entitlement {
        id,
        pack_id: row.0,
        source: row.1,
        status: LicenseState::parse(&row.2).ok_or_else(|| format!("非法 status {:?}", row.2))?,
        external_ref: row.3,
        acquired_at: row.4,
    })
}

fn find_by_external_ref(
    conn: &Connection,
    reference: Option<&str>,
) -> Result<Option<Entitlement>, String> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    let found = conn
        .query_row(
            "SELECT id FROM entitlements WHERE external_ref = ?1 ORDER BY id LIMIT 1",
            params![reference],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match found {
        Some(id) => Ok(Some(get(conn, id)?)),
        None => Ok(None),
    }
}

/// `license_state` 的读写（M7 才会用到：`mor_endpoint`、缓存签名、离线宽限期）。
pub fn license_state_set(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO license_state(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn license_state_get(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    // `.flatten()`：`license_state.value` 是可空列，但本模块的写入路径从不写 NULL，
    // 所以"行不存在"与"值是 NULL"在这里合并成 None —— 调用方只关心"有没有值"。
    Ok(conn
        .query_row(
            "SELECT value FROM license_state WHERE key = ?1",
            params![key],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten())
}

/// 把 [`ContentPack`] 写进 `content_packs`（`items` 不落这张表，它属于包的资产清单）。
pub fn register_pack(conn: &Connection, pack: &ContentPack) -> Result<(), String> {
    pack.validate()?;
    conn.execute(
        "INSERT INTO content_packs(id, title, kind, price_cents, currency_code, store, pack_path, signature, release_notes, is_dlc)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, price_cents = excluded.price_cents,
           pack_path = excluded.pack_path, signature = excluded.signature,
           release_notes = excluded.release_notes, is_dlc = excluded.is_dlc",
        params![
            pack.id,
            pack.title,
            pack.kind,
            pack.price_cents,
            pack.currency_code,
            pack.store,
            pack.pack_path,
            pack.signature,
            pack.release_notes,
            pack.is_dlc as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate::migrate;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c.execute(
            "INSERT INTO content_packs(id, title, kind, store, pack_path) VALUES ('cat_series_01','猫咪系列','pets','itch','packs/cat_series_01.petpack')",
            [],
        )
        .unwrap();
        c
    }

    fn pack_json(extra: &str) -> String {
        format!(
            r#"{{"format":1,"id":"cat_series_01","title":"猫咪系列","kind":"pets",
                "price_cents":990,"currency_code":"USD","store":"itch",
                "pack_path":"packs/cat_series_01.petpack","is_dlc":true,
                "items":["cat_shiro","cap_blue"]{extra}}}"#
        )
    }

    #[test]
    fn a_pack_with_weights_does_not_parse() {
        // 红线 1 的结构层：付费包携带概率权重 ⇒ 解析期失败，不是"解析后忽略"。
        for bad in [
            r#","weight":{"SSR":0.02}"#,
            r#","rarity":"SSR""#,
            r#","drop_rate":0.5"#,
            r#","pool":"ssr_only""#,
        ] {
            let err = ContentPack::from_json(&pack_json(bad)).err();
            assert!(err.is_some(), "带 {bad} 的包 JSON 本该被拒绝");
        }
        assert!(
            ContentPack::from_json(&pack_json("")).is_ok(),
            "正常包不该被拒"
        );
    }

    #[test]
    fn validate_catches_store_price_contradictions() {
        let mut p = ContentPack::from_json(&pack_json("")).unwrap();
        assert_eq!(p.validate(), Ok(()));
        p.store = "free".into();
        assert!(p.validate().unwrap_err().contains("free"));
        p.store = "itch".into();
        p.price_cents = Some(0);
        assert!(p.validate().unwrap_err().contains("正的价格"));
        // 逐条查 ⇒ 每步只留一个错，否则前一个错会先返回，后面的断言其实是空过。
        p.price_cents = Some(990);
        assert_eq!(p.validate(), Ok(()), "修回价格后应重新合法");
        p.items.clear();
        assert!(p.validate().unwrap_err().contains("一件东西都没有"));
    }

    #[test]
    fn refund_revokes_without_deleting_and_repurchase_restores() {
        let c = db();
        let e = record_entitlement(
            &c,
            "cat_series_01",
            "purchase:itch#A1",
            Some("itch#A1"),
            100,
        )
        .unwrap();
        assert_eq!(e.status, LicenseState::Active);
        assert!(is_unlocked(&c, "cat_series_01").unwrap());

        revoke(&c, e.id, 200).unwrap();
        assert_eq!(get(&c, e.id).unwrap().status, LicenseState::Revoked);
        assert!(!is_unlocked(&c, "cat_series_01").unwrap());

        // 重新购买：同一个订单引用 ⇒ 恢复 active，且**不新增第二行**
        let again = record_entitlement(
            &c,
            "cat_series_01",
            "purchase:itch#A1",
            Some("itch#A1"),
            300,
        )
        .unwrap();
        assert_eq!(again.id, e.id, "重新购买不该攒出第二条权益");
        assert_eq!(again.status, LicenseState::Active);
        assert!(is_unlocked(&c, "cat_series_01").unwrap());
        assert_eq!(list(&c).unwrap().len(), 1);
    }

    #[test]
    fn revoked_is_not_the_same_as_expired() {
        // 撤销与过期在语义上不是一回事：过期可以靠续订恢复，撤销是交易被收回。
        let c = db();
        let e = record_entitlement(&c, "cat_series_01", "redeem:code", None, 100).unwrap();
        mark_expired(&c, e.id, 150).unwrap();
        assert_eq!(get(&c, e.id).unwrap().status, LicenseState::Expired);
        assert!(!is_unlocked(&c, "cat_series_01").unwrap());
    }

    #[test]
    fn free_packs_still_get_an_entitlement_row() {
        // 免费包也要走同一条路径，否则"有没有权益"在两类包上语义不同，M7 一定要踩。
        let c = db();
        record_entitlement(&c, "cat_series_01", "free", None, 1).unwrap();
        record_entitlement(&c, "cat_series_01", "free", None, 2).unwrap();
        assert_eq!(list(&c).unwrap().len(), 2, "无订单号的免费包每次都记一行");
        assert!(is_unlocked(&c, "cat_series_01").unwrap());
    }

    #[test]
    fn noop_service_says_everything_free_and_writes_nothing() {
        let c = db();
        let before = list(&c).unwrap().len();
        let msg = NoopLicenseService.sync(&c, true).unwrap();
        assert_eq!(msg, "noop: everything free");
        assert_eq!(list(&c).unwrap().len(), before);
    }

    #[test]
    fn license_state_round_trips_and_allows_null_value() {
        let c = db();
        assert_eq!(license_state_get(&c, "mor_endpoint").unwrap(), None);
        license_state_set(&c, "mor_endpoint", "https://example.invalid").unwrap();
        assert_eq!(
            license_state_get(&c, "mor_endpoint").unwrap().as_deref(),
            Some("https://example.invalid")
        );
        license_state_set(&c, "mor_endpoint", "").unwrap();
        assert_eq!(
            license_state_get(&c, "mor_endpoint").unwrap().as_deref(),
            Some("")
        );
    }

    #[test]
    fn register_pack_rejects_a_bad_kind_before_the_database_does() {
        let c = db();
        let mut p = ContentPack::from_json(&pack_json("")).unwrap();
        p.kind = "surprise_box".into();
        assert!(
            register_pack(&c, &p).is_err(),
            "CHECK 之外的 kind 不该写进去"
        );
        p.kind = "bundle".into();
        assert_eq!(register_pack(&c, &p), Ok(()));
    }

    #[test]
    fn state_strings_match_the_ddl_check() {
        // DDL 的 CHECK 三值与枚举必须一字不差，否则写入永远成功、读取永远失败。
        for s in LicenseState::ALL {
            assert!(matches!(s.as_str(), "active" | "revoked" | "expired"));
            assert_eq!(LicenseState::parse(s.as_str()), Some(s));
        }
        let c = db();
        let e = record_entitlement(&c, "cat_series_01", "free", Some("x"), 1).unwrap();
        assert!(
            c.query_row(
                "SELECT 1 FROM entitlements WHERE id = ?1 AND status = ?2",
                params![e.id, e.status.as_str()],
                |_| Ok(()),
            )
            .is_ok()
        );
    }
}
