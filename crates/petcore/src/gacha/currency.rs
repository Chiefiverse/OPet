//! 抽盲盒用的货币。**这个枚举的存在就是红线测试的第一层**：真钱在这里没有对应变体，
//! 所以"付费 ⇒ 随机"在类型上写不出来（§3.11.4 第 1 条、硬约束 #1）。
//!
//! 字符串形式必须与 `gacha_banner.currency` 的 `CHECK (currency IN
//! ('companion_time','shards'))`（§3.4:456-457）逐字符一致：两边不一致时，
//! schema 红线测试会红。

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
pub enum Currency {
    /// 陪伴时长产出的软通货（v1 唯一免费来源）。
    #[serde(rename = "companion_time")]
    CompanionTime,
    /// 重复转化来的碎片，也可用于自选。
    #[serde(rename = "shards")]
    Shards,
}

impl Currency {
    pub const ALL: [Self; 2] = [Self::CompanionTime, Self::Shards];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompanionTime => "companion_time",
            Self::Shards => "shards",
        }
    }

    /// 只认这两个词。`"real_money"`、`"cash"`、`"usd"` 一律 `None`——
    /// CLI 把它翻译成 `SCHEMA_ERROR`（§10.2:960）。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_currencies_exist() {
        assert_eq!(
            Currency::ALL.map(|c| c.as_str()),
            ["companion_time", "shards"]
        );
    }

    #[test]
    fn every_money_flavoured_name_is_refused() {
        for bad in [
            "real_money",
            "money",
            "cash",
            "usd",
            "cny",
            "credit",
            "iap",
            "steam",
            "itch",
            "",
            "Shards",
            "companion_time ",
        ] {
            assert!(Currency::parse(bad).is_none(), "本该拒绝 {bad:?}");
        }
        assert_eq!(
            Currency::parse("companion_time"),
            Some(Currency::CompanionTime)
        );
    }
}
