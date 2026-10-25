//! 审计导出：JSONL ⇄ [`AuditRecord`]。
//!
//! 为什么单独一个文件、单独一个 `ExportRecord` 类型而不是直接 `#[derive(Serialize)]`
//! 打在 `AuditRecord` 上：导出格式是**对外契约**（第三方脚本要读它），而
//! `AuditRecord` 是内核的内部形状。两个东西一起改就等于改了两份契约。
//! 所以这里：哈希列导出成 hex 字符串（`AuditRecord` 里是 `[u8; 32]`，serde 会展开成
//! 64 个数字，人读不了、diff 也读不了），字段名与 `gacha_pull` 的列名保持一致。
//!
//! `schema` 常量是导出文件的版本位：将来加字段时，读侧能区分"旧文件缺这个字段"
//! 和"文件坏了"。

use petcore::gacha::{AuditRecord, ItemKind, Rarity, hex, unhex};
use serde::{Deserialize, Serialize};

/// 导出文件格式版本。
pub const EXPORT_SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecord {
    pub schema: u32,
    pub seq: u64,
    pub profile_id: String,
    pub banner_id: String,
    /// 墙钟秒。**不参与摘要**（`canonical` 里没有它）：摘要必须只由可重放的量构成。
    pub ts: i64,
    pub rarity: String,
    pub item_kind: String,
    pub item_id: String,
    pub pity_before: u32,
    pub batch_index: u32,
    pub dice_used: u32,
    pub duplicate: bool,
    pub shards_gained: u32,
    pub ssr_prob: u32,
    pub rng_state_hash: String,
    pub audit_prev_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    Json(String),
    BadHash(String),
    BadEnum {
        field: String,
        value: String,
    },
    /// 文件里的 `schema` 与代码期待的不符。
    Schema {
        found: u32,
    },
    /// seq 不连续 ⇒ 有人删过行或截断过文件。
    Truncated {
        at: u64,
        expect: u64,
    },
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ExportError::*;
        match self {
            Json(e) => write!(f, "JSON 解析失败: {e}"),
            BadHash(s) => write!(f, "非法哈希: {s}"),
            BadEnum { field, value } => write!(f, "{field} = {value:?} 不是合法取值"),
            Schema { found } => write!(
                f,
                "导出文件 schema={found}，本程序只认 {EXPORT_SCHEMA} ⇒ 换对应版本的 opet 再验"
            ),
            Truncated { at, expect } => {
                write!(
                    f,
                    "导出文件在 seq={at} 处不连续（本该是 {expect}）⇒ 被截断或删过"
                )
            }
        }
    }
}

impl std::error::Error for ExportError {}

impl ExportRecord {
    /// `ts` 由调用方给（库里的列），因为 `AuditRecord` 本身不带时间。
    pub fn new(r: &AuditRecord, ts: i64) -> Self {
        Self {
            schema: EXPORT_SCHEMA,
            seq: r.seq,
            profile_id: r.profile_id.clone(),
            banner_id: r.banner_id.clone(),
            ts,
            rarity: r.rarity.as_str().into(),
            item_kind: r.item_kind.as_str().into(),
            item_id: r.item_id.clone(),
            pity_before: r.pity_before,
            batch_index: r.batch_index,
            dice_used: r.dice_used,
            duplicate: r.duplicate,
            shards_gained: r.shards_gained,
            ssr_prob: r.ssr_prob,
            rng_state_hash: hex(&r.rng_state_hash),
            audit_prev_hash: hex(&r.prev_digest),
        }
    }

    pub fn to_record(&self) -> Result<AuditRecord, ExportError> {
        if self.schema != EXPORT_SCHEMA {
            return Err(ExportError::Schema { found: self.schema });
        }
        let rarity = Rarity::parse(&self.rarity).ok_or_else(|| ExportError::BadEnum {
            field: "rarity".into(),
            value: self.rarity.clone(),
        })?;
        let item_kind = ItemKind::parse(&self.item_kind).ok_or_else(|| ExportError::BadEnum {
            field: "item_kind".into(),
            value: self.item_kind.clone(),
        })?;
        Ok(AuditRecord {
            seq: self.seq,
            profile_id: self.profile_id.clone(),
            banner_id: self.banner_id.clone(),
            rarity,
            item_kind,
            item_id: self.item_id.clone(),
            pity_before: self.pity_before,
            batch_index: self.batch_index,
            dice_used: self.dice_used,
            duplicate: self.duplicate,
            shards_gained: self.shards_gained,
            ssr_prob: self.ssr_prob,
            rng_state_hash: unhex(&self.rng_state_hash)
                .ok_or_else(|| ExportError::BadHash(self.rng_state_hash.clone()))?,
            prev_digest: unhex(&self.audit_prev_hash)
                .ok_or_else(|| ExportError::BadHash(self.audit_prev_hash.clone()))?,
        })
    }
}

/// 整个文件的编码。一行一条，末尾带换行（`wc -l` 与 `diff` 都友好）。
pub fn encode(records: &[AuditRecord], ts_of: impl Fn(u64) -> i64) -> Result<String, ExportError> {
    let mut out = String::new();
    for r in records {
        out.push_str(&encode_one(r, ts_of(r.seq))?);
    }
    Ok(out)
}

/// 单行编码。`Store::export_jsonl` 用它，因为 `ts` 只在库里有、`AuditRecord` 不带。
pub fn encode_one(r: &AuditRecord, ts: i64) -> Result<String, ExportError> {
    let line = serde_json::to_string(&ExportRecord::new(r, ts))
        .map_err(|e| ExportError::Json(e.to_string()))?;
    Ok(format!("{line}\n"))
}

/// 解码并**顺手校验 seq 连续**：`verify_chain` 也查这个，但那只吃库内数据；
/// 导出文件被人手工删一行时，这里给出"文件被截断"而不是"哈希不匹配"这种误导性的原因。
pub fn decode(text: &str) -> Result<Vec<AuditRecord>, ExportError> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: ExportRecord = serde_json::from_str(line)
            .map_err(|e| ExportError::Json(format!("第 {} 行: {e}", i + 1)))?;
        let r = rec.to_record()?;
        let expect = i as u64 + 1;
        if r.seq != expect {
            return Err(ExportError::Truncated { at: r.seq, expect });
        }
        out.push(r);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use petcore::gacha::GENESIS;

    fn rec(seq: u64) -> AuditRecord {
        AuditRecord {
            seq,
            profile_id: "local".into(),
            banner_id: "cat_series".into(),
            rarity: Rarity::SR,
            item_kind: ItemKind::Accessory,
            item_id: "cap_blue".into(),
            pity_before: 7,
            batch_index: 3,
            dice_used: 2,
            duplicate: true,
            shards_gained: 25,
            ssr_prob: 20_000,
            rng_state_hash: petcore::gacha::rng_state_hash(3, seq),
            prev_digest: GENESIS,
        }
    }

    #[test]
    fn round_trip_is_lossless() {
        let recs = vec![rec(1), rec(2), rec(3)];
        let text = encode(&recs, |_| 1_700_000_000).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert_eq!(decode(&text).unwrap(), recs);
    }

    #[test]
    fn hash_columns_are_hex_not_byte_arrays() {
        // 导出格式给人和第三方脚本读：64 个 hex 字符，不是 [71,0,19,…]。
        let text = encode(&[rec(1)], |_| 0).unwrap();
        assert!(text.contains("\"rng_state_hash\":\""), "{text}");
        assert!(!text.contains("["), "字节数组形式不该出现在导出里: {text}");
    }

    #[test]
    fn a_missing_line_is_reported_as_truncation() {
        let recs = vec![rec(1), rec(2), rec(3)];
        let mut text = encode(&recs, |_| 0).unwrap();
        // 删掉中间一行的代价必须在"文件层"就说清楚
        text = text
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| format!("{l}\n"))
            .collect();
        assert!(matches!(
            decode(&text),
            Err(ExportError::Truncated { at: 3, expect: 2 })
        ));
    }

    #[test]
    fn wrong_schema_is_refused_not_guessed() {
        let mut e = ExportRecord::new(&rec(1), 0);
        e.schema = 99;
        let line = serde_json::to_string(&e).unwrap();
        assert_eq!(
            decode(&line),
            Err(ExportError::Schema { found: 99 }),
            "读到未来的导出格式时不许'尽量解析'"
        );
    }

    #[test]
    fn bad_enum_and_bad_hash_are_named_in_the_error() {
        let mut e = ExportRecord::new(&rec(1), 0);
        e.rarity = "SSSR".into();
        assert!(
            matches!(&e.to_record(), Err(ExportError::BadEnum { field, .. }) if field == "rarity")
        );
        e.rarity = "SR".into();
        e.rng_state_hash = "zz".repeat(32);
        assert!(matches!(e.to_record(), Err(ExportError::BadHash(_))));
    }

    #[test]
    fn ts_is_not_part_of_the_digest() {
        // 同一个 ts 改成别的值，摘要必须不变 ⇒ 审计重放不受墙钟影响。
        let r = rec(1);
        let a = ExportRecord::new(&r, 1).to_record().unwrap();
        let b = ExportRecord::new(&r, 9_999_999).to_record().unwrap();
        assert_eq!(a.digest(), b.digest());
    }
}
