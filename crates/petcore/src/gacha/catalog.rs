//! 抽取目录（banner 里有什么、同档内谁更常见）。
//!
//! **每档内权重必须公示**：§3.6 的概率表只到稀有度，但"抽到 SSR 之后是哪一只"同样是随机数，
//! 所以 [`Catalog`] 里的 `weight` 属于披露范围，`opet catalog rates` 会连同稀有度一起打出来。
//!
//! 一处与设计书的偏离（已回写 §3.4）：ER 图 :394 画了 `GACHA_BANNERS ||--o{ GACHA_POOL`，
//! 但 DDL 里没有 `gacha_pool` 表。v1 只有一个 banner = 整份目录，权重放在目录 JSON 里；
//! M8 真做多 banner（限时池、子集池）时再建表引用 item id。

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;

use super::rarity::Rarity;

/// 目录格式版本位（D4：Petdex 导入推迟到 v1.1，但版本位现在就留）。
pub const CATALOG_FORMAT_V1: u32 = 1;

/// `inventory.kind` 的取值（§3.4:423）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
pub enum ItemKind {
    #[serde(rename = "pet")]
    Pet,
    #[serde(rename = "accessory")]
    Accessory,
}

impl ItemKind {
    pub const ALL: [Self; 2] = [Self::Pet, Self::Accessory];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pet => "pet",
            Self::Accessory => "accessory",
        }
    }

    /// 反查 `inventory.kind` / `gacha_pull.item_kind` 的 CHECK 值。
    /// 未知值返回 `None` 而不是兜底成 `Pet`：库里出现没见过的 kind 说明有人手改过行，
    /// 静默兜底会让审计链的下一环算错摘要。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `accessories.slot` 的取值（§3.4:418 的 CHECK 五值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Head,
    Face,
    Neck,
    Back,
    Hand,
}

impl Slot {
    pub const ALL: [Self; 5] = [Self::Head, Self::Face, Self::Neck, Self::Back, Self::Hand];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Face => "face",
            Self::Neck => "neck",
            Self::Back => "back",
            Self::Hand => "hand",
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogItem {
    pub id: String,
    pub name: String,
    pub kind: ItemKind,
    pub rarity: Rarity,
    /// 同档内相对权重，必须 > 0。等权就全填 1000。
    pub weight: u32,
    /// 配件必须有挂点；宠物必须没有（与 DDL 的 `slot CHECK` 同一条约束）。
    #[serde(default)]
    pub slot: Option<Slot>,
    /// 资产路径。v1 只有 `cat_shiro` 真烘出来了，所以缺文件是常态：
    /// 结构校验不管它，存在性检查归 CLI（`catalog validate --strict-assets`）。
    #[serde(default)]
    pub manifest_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub format: u32,
    pub id: String,
    pub banner_id: String,
    pub items: Vec<CatalogItem>,
}

impl Catalog {
    pub fn from_json(text: &str) -> Result<Self, CatalogError> {
        serde_json::from_str(text).map_err(|e| CatalogError::Json(e.to_string()))
    }

    /// §6.1 交付项里的"10 只宠物 20 件配件"默认目录。
    pub const DEFAULT_JSON: &'static str = include_str!("../../assets/gacha/cat_series_01.json");

    pub fn default_v1() -> Result<Self, CatalogError> {
        let c = Self::from_json(Self::DEFAULT_JSON)?;
        c.validate()?;
        Ok(c)
    }

    pub fn items_of(&self, rarity: Rarity) -> impl Iterator<Item = &CatalogItem> {
        self.items.iter().filter(move |i| i.rarity == rarity)
    }

    pub fn find(&self, id: &str) -> Option<&CatalogItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// 某档的权重总和。空档返回 0（[`Catalog::validate`] 会把它判成错误）。
    pub fn total_weight(&self, rarity: Rarity) -> u64 {
        self.items_of(rarity).map(|i| i.weight as u64).sum()
    }

    /// 结构校验。全部是"公示与实现会不会不一致"级别的检查，不是风格检查：
    /// 每一项都对应一条玩家可见的承诺。
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.format != CATALOG_FORMAT_V1 {
            return Err(CatalogError::UnknownFormat(self.format));
        }
        if self.items.is_empty() {
            return Err(CatalogError::Empty);
        }
        let mut seen = BTreeSet::new();
        for item in &self.items {
            if !seen.insert(item.id.as_str()) {
                return Err(CatalogError::DuplicateId(item.id.clone()));
            }
            if item.weight == 0 {
                return Err(CatalogError::ZeroWeight(item.id.clone()));
            }
            match (item.kind, item.slot) {
                (ItemKind::Pet, Some(s)) => {
                    return Err(CatalogError::SlotMismatch {
                        id: item.id.clone(),
                        reason: format!("宠物不该有挂点 {s}"),
                    });
                }
                (ItemKind::Accessory, None) => {
                    return Err(CatalogError::SlotMismatch {
                        id: item.id.clone(),
                        reason: "配件必须有 slot ∈ head/face/neck/back/hand".into(),
                    });
                }
                _ => {}
            }
        }
        // 公示里写了这一档的概率，目录里却一件都没有 ⇒ 那个概率就是说谎。
        for rarity in Rarity::ALL {
            if self.total_weight(rarity) == 0 {
                return Err(CatalogError::EmptyPool(rarity));
            }
        }
        Ok(())
    }

    /// 同档内按权重选一件。`roll` 必须来自 [`super::stream::PullStream`]，
    /// 且落在 `[0, total_weight)`。band 顺序 = 目录里 `items` 的出现顺序（审计契约的一部分）。
    pub fn pick_weighted(&self, rarity: Rarity, roll: u64) -> Option<&CatalogItem> {
        let total = self.total_weight(rarity);
        if total == 0 {
            return None;
        }
        let mut target = roll % total;
        for item in self.items_of(rarity) {
            if target < item.weight as u64 {
                return Some(item);
            }
            target -= item.weight as u64;
        }
        None // 只在权重溢出 u64 时可能，理论到不了
    }

    pub fn pets(&self) -> impl Iterator<Item = &CatalogItem> {
        self.items.iter().filter(|i| i.kind == ItemKind::Pet)
    }

    pub fn accessories(&self) -> impl Iterator<Item = &CatalogItem> {
        self.items.iter().filter(|i| i.kind == ItemKind::Accessory)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogError {
    Json(String),
    UnknownFormat(u32),
    Empty,
    DuplicateId(String),
    ZeroWeight(String),
    SlotMismatch { id: String, reason: String },
    EmptyPool(Rarity),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(msg) => write!(f, "目录 JSON 解析失败: {msg}"),
            Self::UnknownFormat(v) => write!(f, "目录 format={v}，本版本只认 {CATALOG_FORMAT_V1}"),
            Self::Empty => write!(f, "目录里一件物品都没有"),
            Self::DuplicateId(id) => write!(f, "物品 id 重复: {id}"),
            Self::ZeroWeight(id) => write!(f, "{id} 的权重为 0，永远抽不到"),
            Self::SlotMismatch { id, reason } => write!(f, "{id}: {reason}"),
            Self::EmptyPool(r) => write!(f, "公示里有 {r} 的概率，目录里却没有 {r} 物品"),
        }
    }
}

impl std::error::Error for CatalogError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, rarity: Rarity, weight: u32, slot: Option<Slot>) -> CatalogItem {
        CatalogItem {
            id: id.into(),
            name: id.into(),
            kind: if slot.is_some() {
                ItemKind::Accessory
            } else {
                ItemKind::Pet
            },
            rarity,
            weight,
            slot,
            manifest_path: None,
        }
    }

    fn cat(items: Vec<CatalogItem>) -> Catalog {
        Catalog {
            format: CATALOG_FORMAT_V1,
            id: "t".into(),
            banner_id: "t".into(),
            items,
        }
    }

    #[test]
    fn default_catalog_is_valid_and_is_10_pets_20_accessories() {
        let c = Catalog::default_v1().unwrap();
        assert_eq!(c.items.len(), 30);
        assert_eq!(c.pets().count(), 10, "§6.1 要求 10 只宠物");
        assert_eq!(c.accessories().count(), 20, "§6.1 要求 20 件配件");
        for r in Rarity::ALL {
            assert!(c.items_of(r).count() > 0, "{r} 档空了");
        }
        // SSR 档只有 3 件：碎片商店自选（120 碎片）的候选集就是这个。
        assert_eq!(c.items_of(Rarity::SSR).count(), 3);
    }

    #[test]
    fn each_rejected_shape_is_the_error_you_would_want() {
        let full = || {
            Rarity::ALL
                .iter()
                .enumerate()
                .map(|(i, r)| item(&format!("i{i}"), *r, 10, None))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cat(full()).validate(),
            Ok(()),
            "四档各一件的最小目录应当合法"
        );

        let mut dup = full();
        let first = dup[0].id.clone();
        dup[1].id = first;
        assert_eq!(
            cat(dup).validate(),
            Err(CatalogError::DuplicateId("i0".into()))
        );

        let mut zero = full();
        zero[0].weight = 0;
        assert_eq!(
            cat(zero).validate(),
            Err(CatalogError::ZeroWeight("i0".into()))
        );

        let mut thin = full();
        thin.retain(|i| i.rarity != Rarity::SSR);
        assert_eq!(
            cat(thin).validate(),
            Err(CatalogError::EmptyPool(Rarity::SSR))
        );

        let mut pet_with_slot = full();
        pet_with_slot[0].slot = Some(Slot::Head);
        assert!(matches!(
            cat(pet_with_slot).validate(),
            Err(CatalogError::SlotMismatch { .. })
        ));

        assert_eq!(cat(vec![]).validate(), Err(CatalogError::Empty));
        let mut bad = cat(full());
        bad.format = 7;
        assert_eq!(bad.validate(), Err(CatalogError::UnknownFormat(7)));
    }

    #[test]
    fn unknown_fields_are_refused_not_ignored() {
        // 目录里出现 "rate"/"weight_pct" 这类字段说明有人想绕过 Rates，必须解析失败。
        let text = Catalog::DEFAULT_JSON.replacen(
            "\"weight\": 1000,",
            "\"weight\": 1000, \"drop_rate\": 0.02,",
            1,
        );
        assert!(Catalog::from_json(&text).is_err());
    }

    #[test]
    fn weighted_pick_is_proportional_to_weights() {
        let c = Catalog {
            items: vec![
                item("heavy", Rarity::SR, 9_000, Some(Slot::Head)),
                item("light", Rarity::SR, 1_000, Some(Slot::Neck)),
                item("othershouldnever", Rarity::N, 5, None),
            ],
            ..cat(vec![])
        };
        let total = c.total_weight(Rarity::SR);
        assert_eq!(total, 10_000);
        let mut heavy = 0u64;
        for roll in 0..total {
            if c.pick_weighted(Rarity::SR, roll).unwrap().id == "heavy" {
                heavy += 1;
            }
        }
        assert_eq!(heavy, 9_000, "band 边界不精确");
        // roll 落在别的档的权重上也不该串档
        assert_eq!(c.pick_weighted(Rarity::SR, total).unwrap().id, "heavy");
        assert_eq!(
            c.pick_weighted(Rarity::N, 1).unwrap().id,
            "othershouldnever"
        );
        assert!(
            c.pick_weighted(Rarity::SSR, 0).is_none(),
            "空档必须返回 None"
        );
    }

    #[test]
    fn slot_and_kind_strings_match_the_ddl_check_constraints() {
        // §3.4:418/:423 的 CHECK 值必须是这几个字符串，否则写入即失败。
        assert_eq!(
            Slot::ALL.map(|s| s.as_str()),
            ["head", "face", "neck", "back", "hand"]
        );
        assert_eq!(ItemKind::ALL.map(|k| k.as_str()), ["pet", "accessory"]);
    }
}
