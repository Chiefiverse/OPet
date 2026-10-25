//! 稀有度：四档枚举 + 与 §3.4 DDL 里 `rarity TEXT` 的字符串形式互转。

use std::fmt;
use std::str::FromStr;

/// 声明顺序就是档位高低：`N < R < SR < SSR`。
/// 保底地板（"十连必含 ≥1 SR"）靠这个序比较，别改声明顺序——改了等于改概率。
///
/// serde 形式 = 变体名本身（`"N"/"R"/"SR"/"SSR"`），与 §3.4 里 `rarity TEXT` 的取值一致；
/// 目录 JSON、`base_rate` 列、`rates.json` 都靠它，改重命名会同时改到公示字符串。
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
pub enum Rarity {
    N,
    R,
    SR,
    SSR,
}

impl Rarity {
    /// 与 [`Rarity`] 的声明顺序一致，数组下标 = [`Rarity::index`]（计数、碎片折算都用它）。
    pub const ALL: [Self; 4] = [Self::N, Self::R, Self::SR, Self::SSR];

    pub const fn index(self) -> usize {
        match self {
            Self::N => 0,
            Self::R => 1,
            Self::SR => 2,
            Self::SSR => 3,
        }
    }

    /// `gacha_pull.rarity` / `pets.rarity` 里存的值（§3.4:410、:461）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::N => "N",
            Self::R => "R",
            Self::SR => "SR",
            Self::SSR => "SSR",
        }
    }

    pub const fn sr_or_better(self) -> bool {
        matches!(self, Self::SR | Self::SSR)
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

impl fmt::Display for Rarity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Rarity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("未知稀有度 {s:?}，应为 N/R/SR/SSR 之一"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_matches_tier_semantics() {
        assert!(Rarity::N < Rarity::R);
        assert!(Rarity::R < Rarity::SR);
        assert!(Rarity::SR < Rarity::SSR);
        assert!(Rarity::SR.sr_or_better() && Rarity::SSR.sr_or_better());
        assert!(!Rarity::R.sr_or_better());
    }

    #[test]
    fn string_form_round_trips() {
        for r in Rarity::ALL {
            assert_eq!(Rarity::parse(r.as_str()), Some(r));
            assert_eq!(r.to_string(), r.as_str());
            assert_eq!(Rarity::from_str(r.as_str()).unwrap(), r);
        }
        for bad in ["n", "ssr", "SSSR", "", "N "] {
            assert!(Rarity::parse(bad).is_none(), "本该拒绝 {bad:?}");
        }
    }

    #[test]
    fn index_is_the_array_order() {
        for (i, r) in Rarity::ALL.iter().enumerate() {
            assert_eq!(r.index(), i);
        }
    }
}
