//! 盲盒参数：§3.6 那张概率表的唯一代码事实源。
//!
//! **为什么全用整数**：抽样若走 `f32`，`Rng::next_f32()` 只取高 24 位（`rng.rs:46-49`），
//! 60% 会被舍成非精确值，而 §6.1 的门禁是"与公示概率偏差 < 0.15%"——浮点归约的平台差异
//! 会直接咬进这个判定（硬约束 #8 明确禁止平台相关浮点归约）。所以概率一律表示成
//! **百万分位（millionths）的 u32**，编译期断言四档之和恰为 `SCALE`。

use super::rarity::Rarity;
use std::fmt;

/// 概率的定点刻度：1.0 = 1_000_000。
pub const SCALE: u32 = 1_000_000;

/// 四档稀有度的基础概率（不含保底），单位 = `SCALE` 的份数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rates {
    pub n: u32,
    pub r: u32,
    pub sr: u32,
    pub ssr: u32,
}

impl Rates {
    /// §3.6:584-589 的公示数值：N 60% / R 28% / SR 10% / SSR 2%。
    pub const DEFAULT: Self = Self {
        n: 600_000,
        r: 280_000,
        sr: 100_000,
        ssr: 20_000,
    };

    /// 饱和加：万一有人填错数值，宁可让下面的 `assert` 报"和不等于 SCALE"，
    /// 也不要在 const 求值里先炸一个"整数溢出"的无关错误。
    pub const fn sum(&self) -> u32 {
        self.n
            .saturating_add(self.r)
            .saturating_add(self.sr)
            .saturating_add(self.ssr)
    }

    pub const fn weight(&self, rarity: Rarity) -> u32 {
        match rarity {
            Rarity::N => self.n,
            Rarity::R => self.r,
            Rarity::SR => self.sr,
            Rarity::SSR => self.ssr,
        }
    }
}

/// 四档之和必须是 100%，否则"加权抽样"根本没有确定语义。
const _: () = assert!(Rates::DEFAULT.sum() == SCALE);

/// 一个 banner 的全部可调数值。列集合与 `gacha_banner` 表一一对应（§3.4:452-458），
/// **代码里不许藏魔数**：改数值要同时过 [`GachaParams::validate`] 与公示重跑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GachaParams {
    pub rates: Rates,
    /// 软保底起点（第几抽开始抬升，1-indexed）。§3.6:588 = 40。
    pub soft_start: u32,
    /// 每次抬升的量，单位 same as [`SCALE`]。§3.6:588 的"+6%" = 60_000。
    pub soft_step: u32,
    /// 硬保底：窗口内第几抽必出 SSR（§3.6:589 = 80）。
    pub hard_pity: u32,
    /// 十连必含 ≥1 SR（§3.6:593）。
    pub ten_pull_sr_floor: bool,
    /// 一个 batch 的长度；地板只看这一窗。
    pub batch_size: u32,
    /// 重复转化碎片：`[N, R, SR, SSR]`（§3.6:591 = 2/8/25/100）。
    pub dup_shards: [u32; 4],
    /// 碎片商店自选 SSR 的价（§3.6:591 = 120）。
    pub ssr_redeem_cost: u32,
}

impl GachaParams {
    /// v1 数值（§3.6）。注意：软保底抬的是 **SSR**（设计书 §3.6 伪代码 :572-576 与表格 :588
    /// 原本打架，2026-10-24 定为伪代码口径）。
    pub const V1: Self = Self {
        rates: Rates::DEFAULT,
        soft_start: 40,
        soft_step: 60_000,
        hard_pity: 80,
        ten_pull_sr_floor: true,
        batch_size: 10,
        dup_shards: [2, 8, 25, 100],
        ssr_redeem_cost: 120,
    };

    /// SSR 概率的硬上限：保底只能从 **N** 的份额里拿，绝不能吃掉已公示的 R/SR。
    /// 这样"无保底子集"上四档才精确等于基础概率，§3.6 的双列公示口径才立得住。
    pub const fn ssr_cap(&self) -> u32 {
        SCALE
            .saturating_sub(self.rates.r)
            .saturating_sub(self.rates.sr)
    }

    pub fn validate(&self) -> Result<(), ParamsError> {
        if self.rates.sum() != SCALE {
            return Err(ParamsError::RatesSum {
                got: self.rates.sum(),
            });
        }
        if self.rates.ssr == 0 {
            return Err(ParamsError::ZeroRate(Rarity::SSR));
        }
        if self.hard_pity < 2 {
            return Err(ParamsError::HardPityTooSmall(self.hard_pity));
        }
        if self.soft_start == 0 || self.soft_start > self.hard_pity {
            return Err(ParamsError::SoftStartOutOfRange {
                soft_start: self.soft_start,
                hard_pity: self.hard_pity,
            });
        }
        if self.batch_size == 0 {
            return Err(ParamsError::BatchSizeZero);
        }
        if self.dup_shards.contains(&0) || self.ssr_redeem_cost == 0 {
            return Err(ParamsError::ShardsDisabled);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamsError {
    RatesSum { got: u32 },
    ZeroRate(Rarity),
    HardPityTooSmall(u32),
    SoftStartOutOfRange { soft_start: u32, hard_pity: u32 },
    BatchSizeZero,
    ShardsDisabled,
    BaseRateJson(String),
}

impl fmt::Display for ParamsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RatesSum { got } => write!(f, "四档基础概率之和 = {got}，应为 {SCALE}"),
            Self::ZeroRate(r) => write!(f, "{r} 的基础概率为 0，该档永不出货"),
            Self::HardPityTooSmall(v) => write!(f, "hard_pity={v}，至少要是 2"),
            Self::SoftStartOutOfRange {
                soft_start,
                hard_pity,
            } => write!(
                f,
                "soft_start={soft_start} 必须在 (0, hard_pity={hard_pity}] 内"
            ),
            Self::BatchSizeZero => write!(f, "batch_size 不能为 0"),
            Self::ShardsDisabled => write!(f, "重复转化碎片与自选价都必须 > 0"),
            Self::BaseRateJson(msg) => write!(f, "base_rate JSON 解析失败: {msg}"),
        }
    }
}

impl std::error::Error for ParamsError {}

/// §3.4:454 的 `base_rate` 列写的是 JSON 小数字符串（`{"N":0.60,...}`）。
/// 这里只做**表示层转换**，转换本身允许用浮点：它跑在 DB 适配路径上，
/// 不进抽样回路；解析完立刻回到 u32 百万分位。
impl Rates {
    pub fn to_base_rate_json(&self) -> String {
        let frac = |v: u32| v as f64 / SCALE as f64;
        format!(
            "{{\"N\":{},\"R\":{},\"SR\":{},\"SSR\":{}}}",
            frac(self.n),
            frac(self.r),
            frac(self.sr),
            frac(self.ssr)
        )
    }

    /// 反序列化并检查精度：小数字与定点值必须在 1e-12 内互相对应，
    /// 否则说明公示字符串表达的不是整数百万分位（比如 0.3333333…），拒绝入库。
    pub fn from_base_rate_json(text: &str) -> Result<Self, ParamsError> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ParamsError::BaseRateJson(e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| ParamsError::BaseRateJson("顶层不是对象".into()))?;
        let mut out = [0u32; 4];
        for (rarity, slot) in Rarity::ALL.iter().zip(out.iter_mut()) {
            let key = rarity.as_str();
            let num = obj
                .get(key)
                .and_then(|x| x.as_f64())
                .ok_or_else(|| ParamsError::BaseRateJson(format!("{key} 缺失或不是数字")))?;
            let scaled = num * SCALE as f64;
            if (scaled - scaled.round()).abs() > 1e-9 {
                return Err(ParamsError::BaseRateJson(format!(
                    "{key} = {num} 无法精确落到百万分位"
                )));
            }
            if scaled < 0.0 || scaled > SCALE as f64 {
                return Err(ParamsError::BaseRateJson(format!(
                    "{key} = {num} 超出 [0,1]"
                )));
            }
            *slot = scaled.round() as u32;
        }
        Ok(Self {
            n: out[0],
            r: out[1],
            sr: out[2],
            ssr: out[3],
        })
    }
}

// Rust 的 `f64: Display` 输出最短可回读表示，所以 0.6 就是 `"0.6"`，
// 公示字符串因此可逐字符比对；反读回定点时仍要过精度检查（见上面的 `1e-9`）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_params_are_valid() {
        GachaParams::V1.validate().unwrap();
    }

    #[test]
    fn rates_must_sum_to_scale() {
        let bad = Rates {
            n: 600_000,
            r: 280_000,
            sr: 100_000,
            ssr: 19_999,
        };
        let p = GachaParams {
            rates: bad,
            ..GachaParams::V1
        };
        assert_eq!(p.validate(), Err(ParamsError::RatesSum { got: 999_999 }));
    }

    #[test]
    fn soft_pity_cannot_eat_published_sr_and_r() {
        // 抬到头也拿不走 R/SR 的份额 ⇒ 双列公示里那两档永远是真话。
        assert_eq!(GachaParams::V1.ssr_cap(), 620_000);
    }

    #[test]
    fn base_rate_json_round_trips_and_matches_defaults() {
        let json = Rates::DEFAULT.to_base_rate_json();
        assert_eq!(
            json, r#"{"N":0.6,"R":0.28,"SR":0.1,"SSR":0.02}"#,
            "公示字符串漂移了"
        );
        assert_eq!(Rates::from_base_rate_json(&json).unwrap(), Rates::DEFAULT);
    }

    #[test]
    fn base_rate_json_refuses_non_representable_and_missing() {
        for bad in [
            r#"{"N":0.3333333,"R":0.28,"SR":0.1,"SSR":0.02}"#,
            r#"{"N":0.6,"R":0.28,"SR":0.1}"#,
            r#"{"N":0.6,"R":0.28,"SR":0.1,"SSR":1.5}"#,
            "not json",
        ] {
            assert!(Rates::from_base_rate_json(bad).is_err(), "本该拒绝: {bad}");
        }
    }

    #[test]
    fn hard_pity_and_batch_are_bounded() {
        assert_eq!(
            GachaParams {
                hard_pity: 1,
                ..GachaParams::V1
            }
            .validate(),
            Err(ParamsError::HardPityTooSmall(1))
        );
        assert_eq!(
            GachaParams {
                soft_start: 90,
                ..GachaParams::V1
            }
            .validate(),
            Err(ParamsError::SoftStartOutOfRange {
                soft_start: 90,
                hard_pity: 80
            })
        );
        assert_eq!(
            GachaParams {
                batch_size: 0,
                ..GachaParams::V1
            }
            .validate(),
            Err(ParamsError::BatchSizeZero)
        );
    }
}
