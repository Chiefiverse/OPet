//! 保底（pity）与稀有度判定——§3.6 伪代码 :568-580 的落地。
//!
//! 三个概念各自独立，别再混：
//! - **硬保底**：`since_ssr + 1 >= hard_pity` ⇒ 本次必出 SSR。
//! - **软保底**：从第 `soft_start` 抽起，每次把 SSR 概率抬 `soft_step`；**抬的量只从 N 的份额里扣**
//!   （见 [`GachaParams::ssr_cap`]），这样 R/SR 的基础概率在任何子集上都不被污染。
//! - **十连地板**：一个 batch 打到最后还没 SR 以上 ⇒ 最后一抽强制升到 SR。
//!
//! 保底状态 [`PullState`] 由调用方持有。`crates/petstore` 从 `gacha_pull` 账本**推导**它
//! （不建 pity 表，见计划"关键决定 3"），所以引擎本身仍是纯函数。

use super::params::{GachaParams, SCALE};
use super::rarity::Rarity;
use super::stream::PullStream;

/// 一次抽取所需的保底状态。全部是 `u32` 计数，可从账本无损重算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullState {
    /// 本 pity 窗口内**已经**抽过的次数（不含本次）。出 SSR 后归零。
    pub since_ssr: u32,
    /// 本次是 batch 内的第几抽（0-indexed）。
    pub batch_index: u32,
    /// 本 batch 目前出现过的最高稀有度；`None` = 还没抽。
    pub batch_best: Option<Rarity>,
}

impl Default for PullState {
    fn default() -> Self {
        Self::fresh()
    }
}

impl PullState {
    /// 全新窗口 + 全新 batch。
    pub const fn fresh() -> Self {
        Self {
            since_ssr: 0,
            batch_index: 0,
            batch_best: None,
        }
    }

    /// 本次是窗口内的第几抽（1-indexed，与 §3.6 表格的"第 40 抽"同一口径）。
    pub const fn pull_no(&self) -> u32 {
        self.since_ssr + 1
    }

    /// 该不该触发"十连必含 ≥1 SR"：batch 的最后一抽、且前面没出过 SR 以上。
    pub fn sr_floor_due(&self, params: &GachaParams) -> bool {
        params.ten_pull_sr_floor
            && self.batch_index + 1 == params.batch_size
            && !self.batch_best.is_some_and(Rarity::sr_or_better)
    }

    /// 抽完一次之后推进状态（`pub`：`petstore` 与十连循环都靠它，别让两处各写一遍）。
    ///
    /// ⚠️ batch 到头换新窗时，`batch_best` 必须**清空**而不是延续——
    /// 延续会让地板在第二批以后永远判定"前面已经有 SR 了"，于是十连保证整条失效。
    /// （实测：不清空时 seed=1 的第 3 批 10 抽里没有一件 SR 以上。）
    pub fn advanced(&self, params: &GachaParams, got: Rarity) -> Self {
        let wrap = self.batch_index + 1 >= params.batch_size;
        Self {
            since_ssr: if got == Rarity::SSR {
                0
            } else {
                self.since_ssr + 1
            },
            batch_index: if wrap { 0 } else { self.batch_index + 1 },
            batch_best: if wrap {
                None
            } else {
                Some(match self.batch_best {
                    Some(best) if best >= got => best,
                    _ => got,
                })
            },
        }
    }
}

/// 稀有度判定的结果 + 它**为什么**是这个结果。`--dry-run` 和单测都读这些字段，
/// 不靠日志猜。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RarityRoll {
    pub rarity: Rarity,
    /// 本次实际用的 SSR 概率（百万分位）。硬保底时等于 `SCALE`。
    pub ssr_prob: u32,
    /// 本次抬升的份额（百万分位），`ssr_prob = rates.ssr + soft_bonus`（硬保底时不适用）。
    pub soft_bonus: u32,
    pub hard_pity: bool,
    pub sr_floor: bool,
    /// 本次判定用了几次骰（硬/软保底命中时是 0——不用掷）。
    pub dice: u32,
}

/// 软保底抬升量。`pull_no` 是窗口内第几抽（1-indexed）。
pub const fn soft_bonus(params: &GachaParams, pull_no: u32) -> u32 {
    if pull_no < params.soft_start {
        return 0;
    }
    // saturating：抬过上限也无所谓，roll_rarity 会 clamp 到 ssr_cap。
    (pull_no - params.soft_start + 1).saturating_mul(params.soft_step)
}

/// 掷稀有度。**band 顺序 = SSR, N, R, SR**，这是审计契约的一部分：
/// 换顺序会改变"同一个骰面 → 同一结果"的映射，必须升 `audit::REC_FORMAT`。
pub fn roll_rarity(params: &GachaParams, state: &PullState, stream: &mut PullStream) -> RarityRoll {
    let pull_no = state.pull_no();
    let hard = pull_no >= params.hard_pity;
    let uncapped = params.rates.ssr.saturating_add(soft_bonus(params, pull_no));
    let ssr_prob = if hard {
        SCALE
    } else {
        uncapped.min(params.ssr_cap())
    };
    let soft_bonus_v = if hard { 0 } else { ssr_prob - params.rates.ssr };

    let (rarity, dice) = if hard {
        (Rarity::SSR, 0)
    } else {
        // N 的份额吸收全部抬升 ⇒ 四档之和恒等于 SCALE。
        let n_band = SCALE - ssr_prob - params.rates.r - params.rates.sr;
        let roll = stream.roll_scale();
        let r = if roll < ssr_prob {
            Rarity::SSR
        } else if roll < ssr_prob + n_band {
            Rarity::N
        } else if roll < ssr_prob + n_band + params.rates.r {
            Rarity::R
        } else {
            Rarity::SR
        };
        (r, 1)
    };

    let sr_floor = rarity == Rarity::N || rarity == Rarity::R;
    let sr_floor = sr_floor && state.sr_floor_due(params);
    RarityRoll {
        rarity: if sr_floor { Rarity::SR } else { rarity },
        ssr_prob,
        soft_bonus: soft_bonus_v,
        hard_pity: hard,
        sr_floor,
        dice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gacha::params::SCALE;

    fn st(since_ssr: u32, batch_index: u32, best: Option<Rarity>) -> PullState {
        PullState {
            since_ssr,
            batch_index,
            batch_best: best,
        }
    }

    #[test]
    fn bands_partition_scale_exactly_at_every_pity_level() {
        let p = GachaParams::V1;
        // 硬保底那一抽根本不掷骰（见下面 hard_pity 那条测试），band 只在 1..hard_pity 上有意义。
        for pull_no in 1..p.hard_pity {
            let s = st(pull_no - 1, 0, None);
            let mut stream = PullStream::new(1, pull_no as u64);
            let roll = roll_rarity(&p, &s, &mut stream);
            assert!(
                roll.ssr_prob <= p.ssr_cap(),
                "第 {pull_no} 抽 ssr_prob={} 越过上限",
                roll.ssr_prob
            );
            let n_band = SCALE - roll.ssr_prob - p.rates.r - p.rates.sr;
            assert_eq!(
                roll.ssr_prob + n_band + p.rates.r + p.rates.sr,
                SCALE,
                "第 {pull_no} 抽的 band 之和不是 SCALE"
            );
            if roll.sr_floor {
                assert_eq!(roll.rarity, Rarity::SR);
            }
        }
    }

    #[test]
    fn no_pity_subset_reproduces_the_published_base_rates() {
        // §3.6 双列公示口径的第一列：pity=0 时四档就是 60/28/10/2，band 边界不许有舍入。
        let p = GachaParams::V1;
        let n = 100_000u64;
        let mut counts = [0u64; 4];
        for seq in 0..n {
            let mut stream = PullStream::new(1234, seq);
            let roll = roll_rarity(&p, &PullState::fresh(), &mut stream);
            assert!(!roll.hard_pity && !roll.sr_floor);
            assert_eq!(roll.ssr_prob, p.rates.ssr, "第 1 抽不该有抬升");
            counts[roll.rarity.index()] += 1;
        }
        // 二项标准差：SSR 那档最窄，σ = sqrt(p(1-p)/n) ⇒ 6 倍 σ 容差。
        for (rarity, got) in Rarity::ALL.iter().zip(counts) {
            let want = p.rates.weight(*rarity) as u64 * n / SCALE as u64;
            let sigma = (((want as u128) * (n as u128 - want as u128)) / n as u128) as f64;
            let tol = (6.0 * sigma.sqrt()).ceil() as u64;
            assert!(
                (got as i64 - want as i64).unsigned_abs() <= tol,
                "{rarity} 实测 {got}，期望 {want}±{tol}"
            );
        }
    }

    #[test]
    fn soft_pity_is_monotone_and_starts_on_schedule() {
        let p = GachaParams::V1;
        assert_eq!(soft_bonus(&p, p.soft_start - 1), 0);
        assert_eq!(soft_bonus(&p, p.soft_start), p.soft_step);
        let mut prev = 0;
        for pull_no in 1..=p.hard_pity {
            let got = soft_bonus(&p, pull_no).min(p.ssr_cap() - p.rates.ssr);
            assert!(got >= prev, "SSR 概率随 pity 单调不减");
            prev = got;
        }
    }

    #[test]
    fn hard_pity_fires_and_needs_no_dice() {
        let p = GachaParams::V1;
        let s = st(p.hard_pity - 1, 0, None);
        let mut stream = PullStream::new(5, 5);
        let roll = roll_rarity(&p, &s, &mut stream);
        assert_eq!(roll.rarity, Rarity::SSR);
        assert!(roll.hard_pity);
        assert_eq!(roll.dice, 0);
        assert_eq!(stream.dice_used(), 0);
        assert_eq!(roll.ssr_prob, SCALE);
    }

    /// ⚠️ 设计书数值本身有问题的一条，钉成测试而不是修代码：
    /// `soft_start=40 + soft_step=+6%/抽` 让 SSR 概率在**第 49 抽**就撞上 `ssr_cap`，
    /// 所以 pity 窗口最长 49 抽，`hard_pity=80` 永不触发（形同装饰）。
    /// 已量到实测综合概率前，公示只认引擎产出；改不改这两个旋钮等用户拍板。
    #[test]
    fn soft_ramp_saturates_well_before_hard_pity() {
        let p = GachaParams::V1;
        let cap = p.ssr_cap();
        let first_saturated = (1..=p.hard_pity)
            .find(|&n| p.rates.ssr.saturating_add(soft_bonus(&p, n)) >= cap)
            .expect("按 +6%/抽 必然撞上限");
        assert_eq!(first_saturated, 49);
        assert!(
            first_saturated < p.hard_pity,
            "硬保底本该是兜底，别让它成为唯一出金路径"
        );
    }

    #[test]
    fn ten_pull_floor_fires_only_on_the_last_slot() {
        let p = GachaParams::V1;
        // 前 9 抽：地板条件不成立（还没到头）。
        for idx in 0..9 {
            let mut stream = PullStream::new(7, idx as u64);
            let roll = roll_rarity(&p, &st(0, idx, Some(Rarity::N)), &mut stream);
            assert!(!roll.sr_floor, "batch_index={idx} 不该触发地板");
        }
        // 已经有 SR 以上 ⇒ 到头也不该再动。
        let mut stream = PullStream::new(7, 9);
        let roll = roll_rarity(&p, &st(0, 9, Some(Rarity::SR)), &mut stream);
        assert!(!roll.sr_floor);
        // 到头且窗口内无 SR 以上：不变式 = "被标记的必须变成 SR，没被标记的必须保持原档"。
        let mut forced = 0;
        for seq in 0..500u64 {
            let mut stream = PullStream::new(13, seq);
            let roll = roll_rarity(&p, &st(0, 9, Some(Rarity::N)), &mut stream);
            if roll.sr_floor {
                assert_eq!(roll.rarity, Rarity::SR, "地板触发却没升档");
                forced += 1;
            } else {
                assert_ne!(roll.rarity, Rarity::N);
                assert_ne!(roll.rarity, Rarity::R, "该触发却没触发");
            }
        }
        assert!(
            forced > 0,
            "500 次里一次都没被地板救回来 ⇒ 地板形同不存在（N+R 基础概率 88%）"
        );
    }

    #[test]
    fn state_advances_per_the_two_windows() {
        let p = GachaParams::V1;
        let s = st(3, 2, Some(Rarity::R));
        let n = s.advanced(&p, Rarity::N);
        assert_eq!(
            (n.since_ssr, n.batch_index, n.batch_best),
            (4, 3, Some(Rarity::R))
        );
        // batch 到头：索引归零、best 清空（本抽属于旧窗，不属于新窗）。
        let wrapped = st(3, 9, Some(Rarity::N)).advanced(&p, Rarity::SSR);
        assert_eq!(
            (wrapped.since_ssr, wrapped.batch_index, wrapped.batch_best),
            (0, 0, None),
            "SSR 要清 pity 窗，换 batch 要清 best"
        );
        let wrapped2 = st(0, 9, None).advanced(&p, Rarity::N);
        assert_eq!((wrapped2.since_ssr, wrapped2.batch_index), (1, 0));
        assert_eq!(wrapped2.batch_best, None, "新窗还没开始，不该有 best");
        // 新窗的第一抽把 best 立起来。
        let first = wrapped2.advanced(&p, Rarity::SR);
        assert_eq!((first.batch_index, first.batch_best), (1, Some(Rarity::SR)));
    }

    #[test]
    fn default_state_is_fresh() {
        assert_eq!(PullState::default(), PullState::fresh());
    }
}
