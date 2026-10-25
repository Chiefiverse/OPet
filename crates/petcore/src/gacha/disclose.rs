//! 概率公示文件的生成器：**公示是推导产物，不是抄来的字符串**。
//!
//! 硬约束 #2 的两难在这里被正面解决：只要保底生效，实际出金率就不等于基础概率 2%。
//! 那就把两件事分开公示（§3.6 双列口径）：
//!
//! | 列 | 含义 | 怎么验 |
//! |---|---|---|
//! | `base` | 原始骰面的四档概率（[`GachaParams::rates`]） | 只在"无保底污染"子集上验，[`super::sim::BaseCheck`] |
//! | `effective` | 含软/硬保底与十连地板的**综合概率** | 100 万抽实测 + χ²，见 [`super::sim::Fit`] |
//!
//! `effective` 的值来自 [`super::sim::effective_rates`]（状态机的精确期望），
//! **不来自任何一次模拟的输出** ⇒ "实测 vs 公示"是两条独立路径的比对，不是自证。
//!
//! 文件里不放时间戳：内容只由 `(参数, 目录, 视界)` 决定，所以同一个文件字节应当永远相同。
//! 谁改了参数没重跑公示，`rates_file_on_disk_is_the_engines_own_output` 会立刻红。

use serde::Serialize;

use super::catalog::Catalog;
use super::params::{GachaParams, Rates, SCALE};
use super::rarity::Rarity;
use super::sim::{ItemRate, effective_rates, item_rates};

/// 公示文件格式版本位（与 [`super::audit::REC_FORMAT`]、[`super::catalog::CATALOG_FORMAT_V1`] 同族：
/// 改了字段布局就升版本，别让第三方按旧布局解新文件）。
pub const RATES_FORMAT: u32 = 1;

/// `method` 字段固定写的这句话：让读文件的人知道综合概率是**算**出来的，不是跑出来的样本。
pub const EFFECTIVE_METHOD: &str = "markov_expectation_over_pity_and_batch(no dice rolled)";

/// 仓库里那份公示文件的**内嵌副本**（`include_str!`，与 [`Catalog::DEFAULT_JSON`] 同族）。
///
/// 为什么嵌进二进制而不是运行时按 cwd 去找：`opet gacha-sim` 可能从任意目录跑，
/// 而"gacha-sim 的参照列"必须与单测逐字节比对的是**同一份公示**。按 cwd 找会得到
/// "找到了⇒验的是公示 / 没找到⇒验的是参数现算值"两种语义，后者是自证循环，静默退化更糟。
/// 文件缺失时编译期就失败，不留这条缝。
pub const DEFAULT_RATES_JSON: &str = include_str!("../../assets/gacha/rates.json");

/// 那份文件在仓库里的路径（相对 `petcore` 包根）。只用于**提示文案**：
/// 读公示一律走 [`DEFAULT_RATES_JSON`]，不按这个路径去找文件（见上面那段）。
pub const RATES_ASSET: &str = "assets/gacha/rates.json";

#[derive(Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct RateBlock {
    pub n: u32,
    pub r: u32,
    pub sr: u32,
    pub ssr: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct PercentBlock {
    pub n: String,
    pub r: String,
    pub sr: String,
    pub ssr: String,
}

#[derive(Serialize)]
pub struct ParamBlock {
    pub soft_start: u32,
    pub soft_step: u32,
    pub hard_pity: u32,
    pub ssr_cap: u32,
    pub ten_pull_sr_floor: bool,
    pub batch_size: u32,
    pub dup_shards: [u32; 4],
    pub ssr_redeem_cost: u32,
}

#[derive(Serialize)]
pub struct ItemBlock {
    pub id: String,
    pub kind: String,
    pub rarity: &'static str,
    pub weight: u32,
    pub prob_millionths: u32,
    pub percent: String,
}

/// 一份完整公示。字段声明顺序就是 JSON 里的键顺序（`serde_json` 按声明顺序输出），
/// 所以整份文件可逐字节比对。
#[derive(Serialize)]
pub struct RatesFile {
    pub format: u32,
    pub scale: u32,
    pub banner_id: String,
    pub catalog_id: String,
    /// `effective` 的推导视界（抽数）。见 [`super::sim::EFFECTIVE_HORIZON`]。
    pub basis_pulls: u64,
    pub method: &'static str,
    pub params: ParamBlock,
    pub base: RateBlock,
    pub effective: RateBlock,
    /// 同一批数字的可读口径，纯由百万分位整数格式化（不引入浮点）。
    pub base_percent: PercentBlock,
    pub effective_percent: PercentBlock,
    pub items: Vec<ItemBlock>,
}

/// 百万分位 → 百分数字符串，4 位小数（`600_000 ⇒ "60.0000"`）。
///
/// 走整数而不是 `v as f64 / 1e4`，是为了让公示字符串**可逐字符复现**：
/// 浮点格式化在某些平台上给 `57.0147` 结尾多个 0/少一个 0，就会把字节比对变成噪声源。
pub fn percent_str(millionths: u32) -> String {
    format!("{}.{:04}", millionths / 10_000, millionths % 10_000)
}

fn block(rates: &Rates) -> RateBlock {
    RateBlock {
        n: rates.n,
        r: rates.r,
        sr: rates.sr,
        ssr: rates.ssr,
    }
}

fn percent_block(rates: &Rates) -> PercentBlock {
    PercentBlock {
        n: percent_str(rates.n),
        r: percent_str(rates.r),
        sr: percent_str(rates.sr),
        ssr: percent_str(rates.ssr),
    }
}

fn item_block(rate: &ItemRate, catalog: &Catalog) -> ItemBlock {
    let kind = catalog
        .find(&rate.id)
        .map(|i| i.kind.as_str())
        .unwrap_or("unknown");
    ItemBlock {
        id: rate.id.clone(),
        kind: kind.to_string(),
        rarity: rate.rarity.as_str(),
        weight: rate.weight,
        prob_millionths: rate.prob_millionths,
        percent: percent_str(rate.prob_millionths),
    }
}

/// 由参数与目录生成公示内容。`params` 先过 `validate`，目录先过 `validate`，
/// 所以坏数值不会静默变成一份"看起来正常"的公示文件。
pub fn build(params: &GachaParams, catalog: &Catalog, basis: u64) -> Result<RatesFile, String> {
    params.validate().map_err(|e| e.to_string())?;
    catalog.validate().map_err(|e| e.to_string())?;
    let effective = effective_rates(params, basis);
    if effective.sum() != SCALE {
        return Err(format!(
            "综合概率四档之和 = {}，应为 {SCALE}",
            effective.sum()
        ));
    }
    let items: Vec<ItemBlock> = item_rates(&effective, catalog)
        .iter()
        .map(|r| item_block(r, catalog))
        .collect();
    Ok(RatesFile {
        format: RATES_FORMAT,
        scale: SCALE,
        banner_id: catalog.banner_id.clone(),
        catalog_id: catalog.id.clone(),
        basis_pulls: basis,
        method: EFFECTIVE_METHOD,
        params: ParamBlock {
            soft_start: params.soft_start,
            soft_step: params.soft_step,
            hard_pity: params.hard_pity,
            ssr_cap: params.ssr_cap(),
            ten_pull_sr_floor: params.ten_pull_sr_floor,
            batch_size: params.batch_size,
            dup_shards: params.dup_shards,
            ssr_redeem_cost: params.ssr_redeem_cost,
        },
        base: block(&params.rates),
        effective: block(&effective),
        base_percent: percent_block(&params.rates),
        effective_percent: percent_block(&effective),
        items,
    })
}

impl RatesFile {
    /// 稳定、可读、键序固定的 JSON 文本（结尾带换行，方便直接落盘与 `diff`）。
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("公示结构里没有不可序列化的字段");
        s.push('\n');
        s
    }

    /// `base` 列的 `gacha_banner.base_rate` 写法（小数字符串），
    /// 用来把"公示文件"与"库里那行"钉成同一批数字的两种表示。
    pub fn base_rate_json(&self) -> String {
        Rates {
            n: self.base.n,
            r: self.base.r,
            sr: self.base.sr,
            ssr: self.base.ssr,
        }
        .to_base_rate_json()
    }

    /// 反向读出文件里的两列与它的推导口径，用于"文件 == 引擎输出"的逐字段比对。
    ///
    /// `basis_pulls` 必须一起读出来：综合概率是**按那个视界**算的（暂态随视界略变），
    /// 比对时拿别一个视界现算的值去比，就把一条真话判成了谎。
    pub fn read(text: &str) -> Result<Published, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("公示文件解析失败: {e}"))?;
        let read = |key: &str| -> Result<Rates, String> {
            let obj = v
                .get(key)
                .and_then(|x| x.as_object())
                .ok_or_else(|| format!("公示文件缺 {key} 对象"))?;
            let mut out = [0u32; 4];
            for (rarity, slot) in Rarity::ALL.iter().zip(out.iter_mut()) {
                *slot = obj
                    .get(rarity.as_str())
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u32)
                    .ok_or_else(|| format!("{key}.{} 不是整数", rarity.as_str()))?;
            }
            Ok(Rates {
                n: out[0],
                r: out[1],
                sr: out[2],
                ssr: out[3],
            })
        };
        let num = |key: &str| -> Result<u64, String> {
            v.get(key)
                .and_then(|x| x.as_u64())
                .ok_or_else(|| format!("公示文件缺 {key}"))
        };
        Ok(Published {
            format: num("format")? as u32,
            scale: num("scale")? as u32,
            basis_pulls: num("basis_pulls")?,
            method: v
                .get("method")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            base: read("base")?,
            effective: read("effective")?,
        })
    }
}

/// [`RatesFile::read`] 的返回：**公示文件声称的内容**，与生成它的那台引擎解耦。
///
/// 校验方（CLI 与单测）只需回答一个问题：这份声明是否仍等于引擎按同一口径算出的值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub format: u32,
    pub scale: u32,
    pub basis_pulls: u64,
    pub method: String,
    pub base: Rates,
    pub effective: Rates,
}

impl Published {
    /// 把一份公示声明与"按 `params` 现算的引擎输出"整份比对，返回**可用于判定的综合概率列**。
    ///
    /// 三条红线各自对应一句话，绝不"宽容处理"：
    /// - `format`/`scale` 不对 ⇒ 文件是别的版本/别的口径生成的，拒；
    /// - `base` 与代码不符 ⇒ 有人改了基础概率没重跑公示（**这就是公示在说谎**）；
    /// - `effective` 与引擎按 `basis_pulls` 现算的值不符 ⇒ 改了保底参数没重跑公示。
    pub fn check(&self, params: &GachaParams) -> Result<Rates, String> {
        if self.format != RATES_FORMAT {
            return Err(format!(
                "公示文件 format={}，本版本只认 {RATES_FORMAT}",
                self.format
            ));
        }
        if self.scale != SCALE {
            return Err(format!(
                "公示文件 scale={}，本版本概率一律百万分位（{SCALE}）",
                self.scale
            ));
        }
        if self.method != EFFECTIVE_METHOD {
            return Err(format!(
                "公示文件 method={:?}，与引擎的推导口径 {:?} 不同 ⇒ 那一列不是马氏链期望算出来的",
                self.method, EFFECTIVE_METHOD
            ));
        }
        if self.base != params.rates {
            return Err(format!(
                "公示的 base 列 {:?} 与代码里的基础概率 {:?} 不一致 \
                 ⇒ 改了参数没重跑 `opet catalog rates`（硬约束 #2）",
                self.base, params.rates
            ));
        }
        let from_code = effective_rates(params, self.basis_pulls);
        if self.effective != from_code {
            return Err(format!(
                "公示的综合概率列 {:?} 与参数按 {} 抽视界现算的 {:?} 不一致 \
                 ⇒ 改了保底/地板参数没重跑公示（硬约束 #2）",
                self.effective, self.basis_pulls, from_code
            ));
        }
        Ok(self.effective)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gacha::sim::EFFECTIVE_HORIZON;

    fn default_file() -> RatesFile {
        build(
            &GachaParams::V1,
            &Catalog::default_v1().unwrap(),
            EFFECTIVE_HORIZON,
        )
        .unwrap()
    }

    /// **门禁的门禁**：仓库里那份文件必须逐字节等于引擎现在生成的那份。
    #[test]
    fn rates_file_on_disk_is_the_engines_own_output() {
        let on_disk = DEFAULT_RATES_JSON;
        let fresh = default_file().to_json();
        assert_eq!(
            on_disk, fresh,
            "公示文件与引擎输出不一致 ⇒ 有人改了参数/目录没重跑 `opet catalog rates`"
        );
    }

    #[test]
    fn base_column_is_the_code_that_the_dice_actually_use() {
        let f = default_file();
        assert_eq!(f.base.n, GachaParams::V1.rates.n);
        assert_eq!(f.base.ssr, GachaParams::V1.rates.ssr);
        assert_eq!(f.base.sr, GachaParams::V1.rates.sr);
        assert_eq!(f.base.r, GachaParams::V1.rates.r);
        assert_eq!(f.base.n + f.base.r + f.base.sr + f.base.ssr, SCALE);
        assert_eq!(
            f.effective.n + f.effective.r + f.effective.sr + f.effective.ssr,
            SCALE
        );
        assert_eq!(f.scale, SCALE);
        assert_eq!(f.format, RATES_FORMAT);
    }

    #[test]
    fn two_columns_disagree_in_the_right_direction() {
        // 若两列一模一样，双列公示就是装饰：保底必须真的把 SSR 抬起来、把 N 压下去。
        let f = default_file();
        assert!(f.effective.ssr > f.base.ssr);
        assert!(f.effective.n < f.base.n);
        assert!(f.effective.sr > f.base.sr);
        assert!(f.effective.r < f.base.r);
    }

    #[test]
    fn percent_strings_are_the_integers_readably() {
        assert_eq!(percent_str(600_000), "60.0000");
        assert_eq!(percent_str(20_000), "2.0000");
        assert_eq!(percent_str(570_147), "57.0147");
        assert_eq!(percent_str(1), "0.0001");
        assert_eq!(percent_str(0), "0.0000");
        let f = default_file();
        assert_eq!(f.base_percent.n, "60.0000");
        assert_eq!(
            f.effective_percent.ssr,
            percent_str(f.effective.ssr),
            "可读列与整数列不是同一个数"
        );
    }

    #[test]
    fn every_item_is_disclosed_and_adds_up() {
        let catalog = Catalog::default_v1().unwrap();
        let f = default_file();
        assert_eq!(f.items.len(), catalog.items.len());
        for rarity in Rarity::ALL {
            let tier = match rarity {
                Rarity::N => f.effective.n,
                Rarity::R => f.effective.r,
                Rarity::SR => f.effective.sr,
                Rarity::SSR => f.effective.ssr,
            };
            let sum: u32 = f
                .items
                .iter()
                .filter(|i| i.rarity == rarity.as_str())
                .map(|i| i.prob_millionths)
                .sum();
            assert_eq!(sum, tier, "{rarity} 档逐件概率之和对不上档位");
            assert!(
                f.items
                    .iter()
                    .filter(|i| i.rarity == rarity.as_str())
                    .all(|i| i.prob_millionths > 0),
                "{rarity} 档里有概率为 0 的物品，却仍占着公示档位"
            );
        }
        // 宠与配件都要标 kind（§3.4 的 `inventory.kind` 取值）
        assert_eq!(f.items.iter().filter(|i| i.kind == "pet").count(), 10);
        assert_eq!(f.items.iter().filter(|i| i.kind == "accessory").count(), 20);
    }

    #[test]
    fn build_refuses_params_or_catalog_that_the_engine_would_reject() {
        let c = Catalog::default_v1().unwrap();
        let bad = GachaParams {
            hard_pity: 1,
            ..GachaParams::V1
        };
        assert!(build(&bad, &c, EFFECTIVE_HORIZON).is_err());
        let mut thin = c.clone();
        thin.items.retain(|i| i.rarity != Rarity::SSR);
        assert!(build(&GachaParams::V1, &thin, EFFECTIVE_HORIZON).is_err());
    }

    #[test]
    fn json_round_trips_both_columns() {
        let f = default_file();
        let text = f.to_json();
        let p = RatesFile::read(&text).unwrap();
        assert_eq!(p.format, RATES_FORMAT);
        assert_eq!(p.scale, SCALE);
        assert_eq!(p.basis_pulls, EFFECTIVE_HORIZON);
        assert_eq!(p.base.n, f.base.n);
        assert_eq!(p.base.ssr, f.base.ssr);
        assert_eq!(p.effective.n, f.effective.n);
        assert_eq!(p.effective.ssr, f.effective.ssr);
        // 引擎自己那份公示必须通过自己的校验（否则 `check` 里有一条恒真）。
        assert_eq!(p.check(&GachaParams::V1).unwrap(), p.effective);
        // 公示的 base 列必须能与 `gacha_banner.base_rate` 的那个小数字符串对上（同一批数字两种写法）。
        assert_eq!(
            Rates::from_base_rate_json(&f.base_rate_json()).unwrap(),
            GachaParams::V1.rates
        );
        assert!(RatesFile::read("{}").is_err());
        assert!(RatesFile::read("not json").is_err());
    }

    #[test]
    fn check_rejects_a_publication_that_no_longer_matches_the_engine() {
        // `check` 是"公示 == 代码"的运行时落点，它必须真的能拒掉四种篡改。
        let good = RatesFile::read(DEFAULT_RATES_JSON).unwrap();
        let cases = [
            (
                "format",
                Published {
                    format: 99,
                    ..good.clone()
                },
                "format",
            ),
            (
                "scale",
                Published {
                    scale: 100,
                    ..good.clone()
                },
                "scale",
            ),
            (
                "method",
                Published {
                    method: "hand written".into(),
                    ..good.clone()
                },
                "method",
            ),
            (
                "base 悄悄改成 61%",
                Published {
                    base: Rates {
                        n: 610_000,
                        ..good.base
                    },
                    ..good.clone()
                },
                "base",
            ),
            (
                "effective 抄了旧参数",
                Published {
                    effective: Rates {
                        ssr: good.effective.ssr + 500,
                        ..good.effective
                    },
                    ..good.clone()
                },
                "综合概率",
            ),
        ];
        for (what, tampered, needle) in cases {
            let err = tampered
                .check(&GachaParams::V1)
                .expect_err(&format!("{what} 被改了却还通过校验"));
            assert!(err.contains(needle), "{what} 的报错没提{needle}：{err}");
        }
    }

    #[test]
    fn embedded_publication_is_the_default_catalogs_own_output() {
        // 内嵌那份要能用默认目录与 V1 参数过 check：这是 `gacha-sim` 参照列的来源，
        // 一旦它和 `Catalog::default_v1` 漂移，CLI 就在验一份过期的公示。
        let p = RatesFile::read(DEFAULT_RATES_JSON).unwrap();
        assert_eq!(p.check(&GachaParams::V1).unwrap(), p.effective);
        assert_eq!(p.base, GachaParams::V1.rates);
        let c = Catalog::default_v1().unwrap();
        assert_eq!(
            item_rates(&p.effective, &c)
                .iter()
                .map(|r| r.prob_millionths as u64)
                .sum::<u64>(),
            SCALE as u64,
            "逐件概率之和不等于 100% ⇒ 公示的某一档没有对应的物品"
        );
    }

    #[test]
    fn changing_a_knob_moves_the_published_number() {
        // 反证：如果公示值与参数无关，`rates_file_on_disk_...` 那条门禁就是假绿。
        let c = Catalog::default_v1().unwrap();
        let soft_only = GachaParams {
            ten_pull_sr_floor: false,
            ..GachaParams::V1
        };
        let with_floor = default_file();
        let without = build(&soft_only, &c, EFFECTIVE_HORIZON).unwrap();
        assert_ne!(
            with_floor.effective.sr, without.effective.sr,
            "关掉十连地板，公示的 SR 一点没动 ⇒ 地板没进综合概率"
        );
        let no_pity = GachaParams {
            soft_start: GachaParams::V1.hard_pity,
            ..GachaParams::V1
        };
        let np = build(&no_pity, &c, EFFECTIVE_HORIZON).unwrap();
        assert!(
            np.effective.ssr < with_floor.effective.ssr,
            "取消软保底，综合 SSR 反而没降 ⇒ 保底没进期望"
        );
    }
}
