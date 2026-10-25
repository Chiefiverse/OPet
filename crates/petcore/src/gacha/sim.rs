//! 分布验证：把"公示的概率"与"引擎真的掷出来的频率"放进同一个整数判定。
//!
//! ## 为什么参照物不是"再跑一次模拟"
//!
//! 硬约束 #2 要的是"公示 == 代码"。如果公示值本身来自某一次模拟，那"实测 vs 公示"就是拿
//! 数据比数据：两次模拟之差是噪声，比出来的"通过"**什么也没证明**（自证循环）。
//! 所以这里的参照物是**保底与十连地板这台状态机的精确期望**：[`expected_counts`] 在
//! `(since_ssr × batch_best)` 的马氏链上逐步累加每档概率，全程只用 [`GachaParams`] 的整数，
//! 不掷一粒骰子。于是
//!
//! - `dev_*`（实测 − 公示）与 `chi2` 比的是**两条独立计算路径**：解析期望 vs 随机数实现；
//! - 随机数有偏、band 边界写错、地板判定漂移都会被抓住，而"两次模拟互比"抓不住。
//!
//! ## 整数门禁
//!
//! §6.1 两句门禁（偏差 < 0.15%、χ² p > 0.05）都写成**纯整数比较**：偏差用百万分位、
//! χ² 用放大 10⁶ 倍的 u128 与临界值 [`CHI2_CRITICAL_MILLIONTHS`] 比大小。
//! [`Fit::p_value`] 里的浮点只用于**打印诊断**，不参与判定（硬约束 #8 禁平台相关浮点归约
//! 决定门禁）。马氏链期望本身是 `f64`，只用来把公示值**舍入到百万分位整数**：
//! 舍入误差 ≤ 5e-7 概率，比 0.15% 门禁小三个数量级，也比 10⁶ 抽的标准差小两个数量级。

use super::catalog::Catalog;
use super::engine::{Engine, GachaError, Owned};
use super::params::{GachaParams, Rates, SCALE};
use super::pity::{PullState, soft_bonus};
use super::rarity::Rarity;

/// §6.1 门禁一：任一档实测频率与公示综合概率的绝对偏差上限 = 0.15%（百万分位）。
pub const MAX_DEV_MILLIONTHS: u64 = 1_500;
/// §6.1 门禁二：df=3、α=0.05 的 χ² 临界值 7.815，放大 10⁶ 倍写成正整数。
pub const CHI2_CRITICAL_MILLIONTHS: u128 = 7_815_000;
/// 无保底子集的容差倍数（6σ）。写成整数不等式，全程不开方。
pub const BASE_SIGMA: u128 = 6;
/// [`effective_rates`] 的默认推导视界 = §6.1 门禁的测量长度，公示与实测同口径。
///
/// 视界不是随便取的自由度：起步若干抽还没进过保底窗（**暂态**），所以短视界的综合概率
/// 会略低于稳态值；`effective_rates_sum_is_exactly_one` 把"视界 ≥ 5 万时漂移 ≪ 门禁"钉住。
pub const EFFECTIVE_HORIZON: u64 = 1_000_000;

/// 一次长模拟的观测汇总。全是计数，没有浮点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    pub pulls: u64,
    /// `[N, R, SR, SSR]`。
    pub counts: [u64; 4],
    /// **无保底污染子集** = 掷骰前判定"本抽的 band 就是基础 band"的那些抽
    /// （`pull_no < hard_pity` 且 `soft_bonus == 0` 且**地板未到窗口**）。
    ///
    /// ⚠️ 第三条件必须问 [`PullState::sr_floor_due`]（状态），不能用结果的 `sr_floor` 标记：
    /// 后者只在"骰面是 N/R 且被抬成 SR"时为真，于是**只把地板抽里的 N/R 删掉、把地板抽里的
    /// SR/SSR 留下**——留下的地板抽 100% 是 SR+，SR 档被系统性抬高（实测 1M 抽上 +2950 百万分位
    /// ≈ 9.4σ，门禁当场判红）。判据要问"规则有没有生效"，不是"结果有没有被改"。
    pub base_counts: [u64; 4],
    pub base_pulls: u64,
    /// 十连地板兑现次数（"十连必含 ≥1 SR"的实测证据）。
    pub floor_hits: u64,
    pub hard_pity_hits: u64,
    pub duplicates: u64,
    pub shards: u64,
    /// 实测最长 pity 窗口：用来证明 `hard_pity` 是兜底还是唯一出金路径。
    pub max_since_ssr: u32,
}

impl Observed {
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }
}

/// 真的掷骰子跑 `pulls` 次（全新状态、全新库存，`seq` 从 1 递增）。
///
/// 走的是 [`Engine::pull`] 本体而不是它的复刻 —— 门禁必须打在玩家实际会走的那条路上。
pub fn simulate(
    params: &GachaParams,
    catalog: &Catalog,
    seed: u64,
    pulls: u64,
) -> Result<Observed, GachaError> {
    let engine = Engine::new(params, catalog)?;
    let mut state = PullState::fresh();
    let mut owned = Owned::new();
    let mut o = Observed {
        pulls,
        counts: [0; 4],
        base_counts: [0; 4],
        base_pulls: 0,
        floor_hits: 0,
        hard_pity_hits: 0,
        duplicates: 0,
        shards: 0,
        max_since_ssr: 0,
    };
    for seq in 1..=pulls {
        // 掷骰**前**判_clean：这三条成立时 band 与基础概率逐档相同，地板也不会介入。
        let clean = state.pull_no() < params.hard_pity
            && soft_bonus(params, state.pull_no()) == 0
            && !state.sr_floor_due(params);
        let (got, next) = engine.pull(&state, &mut owned, seed, seq)?;
        o.counts[got.rarity.index()] += 1;
        if clean {
            o.base_counts[got.rarity.index()] += 1;
            o.base_pulls += 1;
        }
        o.floor_hits += got.sr_floor as u64;
        o.hard_pity_hits += got.hard_pity as u64;
        o.duplicates += got.duplicate as u64;
        o.shards += got.shards_gained as u64;
        o.max_since_ssr = o.max_since_ssr.max(got.pity_before);
        state = next;
    }
    Ok(o)
}

/// 实测 vs 公示的拟合结果。`published` 是整数百万分位，所以判定全是整数比较。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub pulls: u64,
    pub observed: [u64; 4],
    pub published: [u32; 4],
    /// 每档"实测概率 − 公示概率"，百万分位（正 = 实测偏高）。
    pub dev_millionths: [i64; 4],
    /// χ² × 10⁶。与 [`CHI2_CRITICAL_MILLIONTHS`] 直接比大小。
    pub chi2_millionths: u128,
}

impl Fit {
    pub fn new(observed: [u64; 4], pulls: u64, published: &Rates) -> Self {
        let want = [published.n, published.r, published.sr, published.ssr];
        let scale = SCALE as u128;
        let n = pulls.max(1) as u128;
        let mut dev = [0i64; 4];
        let mut chi2 = 0u128;
        for i in 0..4 {
            let got = observed[i] as u128;
            let w = want[i] as u128;
            // 实测概率，百万分位，四舍五入。
            dev[i] = ((got * scale + n / 2) / n) as i64 - want[i] as i64;
            // χ² 一项 = (O−E)²/E，E = n·w/S。通分：(O−E)²/E = (O·S − n·w)² / (S·n·w)。
            // 而门禁把 χ² 放大 S=10⁶ 倍存成整数，放大因子恰好和分母里的 S 约掉：
            //   chi2_millionths += (O·S − n·w)² / (n·w)
            let d = (got * scale) as i128 - (n * w) as i128;
            if w == 0 {
                // 公示为 0 的档一旦出货就是"公示在说谎"，直接判成无穷大。
                if got > 0 {
                    chi2 = u128::MAX;
                }
            } else {
                chi2 += d.unsigned_abs().pow(2) / (n * w);
            }
        }
        Self {
            pulls,
            observed,
            published: want,
            dev_millionths: dev,
            chi2_millionths: chi2,
        }
    }

    pub fn worst_dev(&self) -> u64 {
        self.dev_millionths
            .iter()
            .map(|d| d.unsigned_abs())
            .max()
            .unwrap_or(0)
    }

    /// 门禁一：每档绝对偏差 ≤ 0.15%。
    pub fn dev_ok(&self) -> bool {
        self.worst_dev() <= MAX_DEV_MILLIONTHS
    }

    /// 门禁二：χ²(df=3) ≤ 7.815。
    pub fn chi2_ok(&self) -> bool {
        self.chi2_millionths <= CHI2_CRITICAL_MILLIONTHS
    }

    pub fn chi2(&self) -> f64 {
        self.chi2_millionths as f64 / SCALE as f64
    }

    /// df=3 的 p 值，**只用于打印**；判定看 [`Fit::chi2_ok`] 的整数比较。
    ///
    /// 闭式 `P(X > x) = erfc(√(x/2)) + √(2x/π)·e^(−x/2)`，`erfc` 用 Abramowitz–Stegun
    /// 7.1.26（绝对误差 ≤ 1.5e-7），量级诊断够用。
    pub fn p_value(&self) -> f64 {
        chi2_sf_df3(self.chi2())
    }
}

/// 双列公示的第一列：无保底子集必须**精确**等于基础概率。
///
/// 判据写成整数不等式 `((O·S − n·w)² ≤ 36·n·w·(S−w))`，等价于"|偏差| ≤ 6σ"，
/// 但不开方、不用浮点 ⇒ 容差在任何平台上都是同一个数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseCheck {
    pub subset: u64,
    pub counts: [u64; 4],
    pub lhs: [u128; 4],
    pub rhs: [u128; 4],
}

impl BaseCheck {
    pub fn run(counts: &[u64; 4], subset: u64, base: &Rates) -> Self {
        let want = [base.n, base.r, base.sr, base.ssr];
        let s = SCALE as u128;
        let n = subset as u128;
        let sigma2 = BASE_SIGMA * BASE_SIGMA;
        let mut lhs = [0u128; 4];
        let mut rhs = [0u128; 4];
        for i in 0..4 {
            let w = want[i] as u128;
            let d = (counts[i] as i128) * s as i128 - n as i128 * w as i128;
            let a = d.unsigned_abs();
            lhs[i] = a * a;
            rhs[i] = sigma2 * n * w * (s - w);
        }
        Self {
            subset,
            counts: *counts,
            lhs,
            rhs,
        }
    }

    pub fn ok(&self) -> bool {
        self.lhs.iter().zip(self.rhs).all(|(l, r)| *l <= r)
    }
}

/// `best` 的编码：`0` = 本 batch 还没抽（`PullState::batch_best == None`），
/// `1..=4` = [`Rarity::index`] + 1。声明顺序即稀有度高低，所以 `max` 就是
/// [`super::pity::PullState::advanced`] 里"记住本批最好"的语义。
const BEST_LEVELS: usize = 5;

/// 给定 pity 窗口位置与 batch 窗口，本抽四档的概率（精确，不掷骰）。
///
/// 与 [`super::pity::roll_rarity`] 逐条对应：硬保底 ⇒ SSR 概率 1；否则
/// `ssr_prob = min(rates.ssr + soft_bonus, ssr_cap)` 且**抬升全从 N 的份额里扣**，
/// 最后按十连地板把 N+R 的质量整体搬到 SR。两处口径一旦漂移，
/// `dp_agrees_with_the_engines_own_roll` 会立刻红。
fn tier_probs(params: &GachaParams, since_ssr: u32, batch_index: u32, best: u32) -> [f64; 4] {
    let pull_no = since_ssr + 1;
    if pull_no >= params.hard_pity {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let ssr_prob = params
        .rates
        .ssr
        .saturating_add(soft_bonus(params, pull_no))
        .min(params.ssr_cap());
    let n_band = SCALE - ssr_prob - params.rates.r - params.rates.sr;
    let mut p = [
        n_band as f64,
        params.rates.r as f64,
        params.rates.sr as f64,
        ssr_prob as f64,
    ];
    let best_rarity = best
        .checked_sub(1)
        .and_then(|i| Rarity::ALL.get(i as usize).copied());
    let floor_due = params.ten_pull_sr_floor
        && batch_index + 1 == params.batch_size
        && !best_rarity.is_some_and(Rarity::sr_or_better);
    if floor_due {
        p[2] += p[0] + p[1];
        p[0] = 0.0;
        p[1] = 0.0;
    }
    let s = SCALE as f64;
    [p[0] / s, p[1] / s, p[2] / s, p[3] / s]
}

/// **马氏链精确期望**：跑 `pulls` 抽，每档应当出现多少次。
///
/// 状态 = `(since_ssr, batch_best)`；`batch_index` 由"第几抽"决定（`k % batch_size`），
/// 是周期的确定量，不必编进状态。每抽的骰子流由 `derive_seed(seed, seq)` 派生 ⇒
/// 相邻抽相互独立，"给定状态求概率再累加"才成立（这是 [`super::stream`] 派生流的第二重收益：
/// 它不只让历史不漂移，还让整条链可以用马尔可夫精确解算出来）。
///
/// 收敛后按周期解跳完剩余抽：`hard_pity=80` 时逐步走完 10⁶ 抽要 10⁶×400 次更新，没必要。
pub fn expected_counts(params: &GachaParams, pulls: u64) -> [f64; 4] {
    let s_max = params.hard_pity as usize;
    let bs = params.batch_size as usize;
    let mut cur = vec![0f64; s_max * BEST_LEVELS];
    let mut nxt = vec![0f64; s_max * BEST_LEVELS];
    cur[0] = 1.0; // (since_ssr=0, best=None)
    let mut exp = [0f64; 4];
    let mut phase_marginal = vec![[0f64; 4]; bs];
    let mut prev_state = vec![0f64; cur.len()];

    let mut k = 0u64;
    while k < pulls {
        let batch_index = (k as usize % bs) as u32;
        let wrap = batch_index as usize + 1 >= bs;
        let mut marginal = [0f64; 4];
        step(params, &cur, &mut nxt, batch_index, wrap, &mut marginal);
        for (e, m) in exp.iter_mut().zip(&marginal) {
            *e += m;
        }
        std::mem::swap(&mut cur, &mut nxt);
        phase_marginal[batch_index as usize] = marginal;
        k += 1;

        // 走完整个周期才判稳，这样剩余抽恰好从相位 0 起，外推 = 整周期倍数 + 前缀。
        if (k as usize) % bs == 0 && k >= 2 * bs as u64 {
            // ⚠️ 比的是**状态向量**，不是边际分布。只比边际会在"第 40 抽才起坡"的软保底上
            // 假阳性：起坡前每档概率都与 `since_ssr` 无关，边际逐周期完全相同，而 pity 计数
            // 一直在往上爬 —— 于是出金前的基础概率被当成稳态乘上一万个周期，
            // 公示的 SSR 就永远停在 2.0%（本文件第一版真的这么错过，靠两条对照测试才抓住）。
            let settled = (0..cur.len()).all(|i| (cur[i] - prev_state[i]).abs() < 1e-15);
            prev_state.clone_from(&cur);
            if settled {
                let rest = pulls - k;
                let cycles = rest / bs as u64;
                let rem = (rest % bs as u64) as usize;
                for t in 0..4 {
                    let per_cycle: f64 = phase_marginal.iter().map(|m| m[t]).sum();
                    exp[t] += cycles as f64 * per_cycle
                        + phase_marginal[..rem].iter().map(|m| m[t]).sum::<f64>();
                }
                return exp;
            }
        }
    }
    exp
}

/// 马氏链一步：把 `cur` 按本抽概率推到 `nxt`，并累加本抽边际分布。
fn step(
    params: &GachaParams,
    cur: &[f64],
    nxt: &mut [f64],
    batch_index: u32,
    wrap: bool,
    marginal: &mut [f64; 4],
) {
    let s_max = params.hard_pity;
    nxt.fill(0.0);
    for s in 0..s_max as usize {
        for b in 0..BEST_LEVELS {
            let m = cur[s * BEST_LEVELS + b];
            if m == 0.0 {
                continue;
            }
            let p = tier_probs(params, s as u32, batch_index, b as u32);
            for (t, pt) in p.iter().enumerate() {
                if *pt == 0.0 {
                    continue;
                }
                let add = m * pt;
                marginal[t] += add;
                let ns = if t == Rarity::SSR.index() {
                    0
                } else {
                    (s + 1) as u32
                };
                let nb = if wrap {
                    0
                } else {
                    b.max(t + 1).min(BEST_LEVELS - 1)
                };
                nxt[ns as usize * BEST_LEVELS + nb] += add;
            }
        }
    }
}

/// 公示的**综合概率**（含软/硬保底与十连地板），百万分位整数。
///
/// 舍入余数归给 **N**：保底只从 N 的份额里拿，N 本来就是四档里的吸收项，由它兜住舍入误差，
/// 四档之和恒等于 [`SCALE`]（`effective_rates_sum_is_exactly_one`）。
pub fn effective_rates(params: &GachaParams, horizon: u64) -> Rates {
    let horizon = horizon.max(1);
    let exp = expected_counts(params, horizon);
    let h = horizon as f64;
    let milli = |v: f64| (v / h * SCALE as f64).round().clamp(0.0, SCALE as f64) as u32;
    let r = milli(exp[Rarity::R.index()]);
    let sr = milli(exp[Rarity::SR.index()]);
    let ssr = milli(exp[Rarity::SSR.index()]);
    let n = SCALE.saturating_sub(r + sr + ssr);
    Rates { n, r, sr, ssr }
}

/// 逐件概率：`P(item) = P(该档综合概率) × weight ÷ 该档权重和`。
///
/// §3.6 的表只到稀有度，但"出了 SSR 之后是哪一只"同样是随机数，所以逐件概率也在披露范围里。
/// 舍入余数归给档内权重最大的一件 ⇒ **逐件之和精确等于该档公示值**，不会出现
/// "加起来对不上档位"的空洞。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRate {
    pub id: String,
    pub rarity: Rarity,
    pub weight: u32,
    pub prob_millionths: u32,
}

pub fn item_rates(effective: &Rates, catalog: &Catalog) -> Vec<ItemRate> {
    let mut out = Vec::with_capacity(catalog.items.len());
    for rarity in Rarity::ALL {
        let tier = effective.weight(rarity) as u128;
        let total = catalog.total_weight(rarity).max(1) as u128;
        let mut rates: Vec<ItemRate> = Vec::new();
        let mut assigned = 0u128;
        for item in catalog.items_of(rarity) {
            let p = (tier * item.weight as u128 + total / 2) / total;
            assigned += p;
            rates.push(ItemRate {
                id: item.id.clone(),
                rarity,
                weight: item.weight,
                prob_millionths: p as u32,
            });
        }
        if let Some(biggest) = rates.iter_mut().max_by_key(|x| x.weight) {
            let residue = tier as i64 - assigned as i64;
            biggest.prob_millionths = (biggest.prob_millionths as i64 + residue).max(0) as u32;
        }
        out.extend(rates);
    }
    out
}

/// df=3 的 χ² 右尾概率（诊断用，见 [`Fit::p_value`]）。
fn chi2_sf_df3(x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    let t = (x / 2.0).sqrt();
    erfc(t) + (2.0 * x / std::f64::consts::PI).sqrt() * (-x / 2.0).exp()
}

/// Abramowitz–Stegun 7.1.26，绝对误差 ≤ 1.5e-7。
fn erfc(x: f64) -> f64 {
    let p = 0.3275911;
    let a = [
        0.254829592,
        -0.284496736,
        1.421413741,
        -1.453152027,
        1.061405429,
    ];
    let t = 1.0 / (1.0 + p * x.abs());
    // 注意最外层那个 `t`：公式是 `a1·t + a2·t² + …`，少乘一次会得到 erfc 偏大 ~7%
    // （实测在 χ²=7.815 处给出 p=0.0533 而不是 0.0499，把临界值处的自校验带歪）。
    let poly = t * (a[0] + t * (a[1] + t * (a[2] + t * (a[3] + t * a[4]))));
    let v = poly * (-x * x).exp();
    if x < 0.0 { 2.0 - v } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gacha::pity::PullState;
    use crate::gacha::stream::PullStream;

    fn params() -> GachaParams {
        GachaParams::V1
    }

    fn catalog() -> Catalog {
        Catalog::default_v1().unwrap()
    }

    fn state_of(since_ssr: u32, batch_index: u32, best: u32) -> PullState {
        PullState {
            since_ssr,
            batch_index,
            batch_best: if best == 0 {
                None
            } else {
                Some(Rarity::ALL[(best - 1) as usize])
            },
        }
    }

    /// DP 的每抽概率必须等于引擎自己的判定。这条不成立，下面所有参照物都是空的。
    #[test]
    fn dp_agrees_with_the_engines_own_roll() {
        let p = params();
        // 每个状态跑固定数量的独立骰子流（seq 递增 ⇒ 派生流互不相同），
        // 与 tier_probs 比 6σ 内的频率。挑的状态覆盖：保底前/起点/饱和点/硬保底。
        let states = [
            (0u32, 0u32, 0u32),
            (1, 4, 2),
            (38, 9, 1),
            (39, 9, 1),
            (44, 9, 4),
            (48, 0, 0),
            (49, 5, 3),
            (79, 9, 1),
            (79, 9, 4),
        ];
        let trials = 40_000u64;
        for (s, i, b) in states {
            let want = tier_probs(&p, s, i, b);
            let mut got = [0u64; 4];
            for k in 0..trials {
                let mut stream = PullStream::new(9_824_516, k + (s as u64) * 1_000_000);
                let roll = crate::gacha::roll_rarity(&p, &state_of(s, i, b), &mut stream);
                got[roll.rarity.index()] += 1;
            }
            for t in 0..4 {
                let want_c = want[t] * trials as f64;
                let tol = (6.0 * (want_c * (1.0 - want[t])).sqrt()).ceil() as u64 + 1;
                assert!(
                    (got[t] as f64 - want_c).abs() <= tol as f64,
                    "状态 ({s},{i},{b}) 档 {t}：DP 期望 {want_c}±{tol}，引擎实测 {}",
                    got[t]
                );
            }
        }
    }

    #[test]
    fn expectation_is_a_distribution_over_the_requested_pulls() {
        let p = params();
        for pulls in [1u64, 10, 79, 100, 12_345] {
            let exp = expected_counts(&p, pulls);
            let sum: f64 = exp.iter().sum();
            assert!(
                (sum - pulls as f64).abs() <= 1e-6 * pulls as f64 + 1e-9,
                "{pulls} 抽的期望之和 = {sum}"
            );
            assert!(exp.iter().all(|v| *v >= 0.0), "期望出现负数");
        }
    }

    /// 稳态外推必须与"老老实实逐步走"给同一个数 —— 否则 10⁶ 抽的参照物不可信。
    #[test]
    fn converged_expectation_matches_the_step_by_step_walk() {
        let p = params();
        for horizon in [200u64, 1_000, 5_000, 20_000, 50_000] {
            let fast = expected_counts(&p, horizon);
            let slow = expected_counts_brute(&p, horizon);
            for t in 0..4 {
                assert!(
                    (fast[t] - slow[t]).abs() <= 1e-6 * horizon as f64,
                    "{horizon} 抽档 {t}：外推 {} vs 逐步 {}",
                    fast[t],
                    slow[t]
                );
            }
        }
    }

    /// 关掉稳态跳跃的对照实现。
    fn expected_counts_brute(params: &GachaParams, pulls: u64) -> [f64; 4] {
        let bs = params.batch_size as usize;
        let mut cur = vec![0f64; params.hard_pity as usize * BEST_LEVELS];
        let mut nxt = vec![0f64; cur.len()];
        cur[0] = 1.0;
        let mut exp = [0f64; 4];
        let mut marg = [0f64; 4];
        for k in 0..pulls {
            let batch_index = (k as usize % bs) as u32;
            marg.fill(0.0);
            step(
                params,
                &cur,
                &mut nxt,
                batch_index,
                batch_index as usize + 1 >= bs,
                &mut marg,
            );
            for (e, m) in exp.iter_mut().zip(&marg) {
                *e += m;
            }
            std::mem::swap(&mut cur, &mut nxt);
        }
        exp
    }

    #[test]
    fn effective_rates_sum_is_exactly_one() {
        let p = params();
        let eff = effective_rates(&p, EFFECTIVE_HORIZON);
        assert_eq!(eff.sum(), SCALE, "四档综合概率之和必须等于 SCALE");
        // 机制方向：抬升从 N 扣 ⇒ N 降；地板把 N/R 搬成 SR ⇒ SR 升、R 降；SSR 被保底抬起。
        assert!(eff.ssr > p.rates.ssr, "保底没抬起 SSR？{eff:?}");
        assert!(eff.n < p.rates.n, "N 没被保底吃掉？{eff:?}");
        assert!(eff.sr > p.rates.sr, "十连地板没抬起 SR？{eff:?}");
        assert!(eff.r < p.rates.r, "R 一点没被地板搬走？{eff:?}");
        // 视界 ≥ 5 万之后，公示值随视界的漂移必须远小于 0.15% 门禁，否则"综合概率"
        // 就成了随便挑一个 n 凑出来的数。5 千抽时漂移实测 66 百万分位（暂态权重 30/5000）。
        for h in [50_000u64, 200_000, 2_000_000] {
            let other = effective_rates(&p, h);
            let drift = [eff.n, eff.r, eff.sr, eff.ssr]
                .iter()
                .zip([other.n, other.r, other.sr, other.ssr])
                .map(|(a, b)| (*a as i64 - b as i64).unsigned_abs())
                .max()
                .unwrap_or(0);
            assert!(
                drift < MAX_DEV_MILLIONTHS / 10,
                "视界 {h} 与 {EFFECTIVE_HORIZON} 的公示值差 {drift} 百万分位"
            );
        }
    }

    #[test]
    fn item_rates_add_up_to_their_tier_exactly() {
        let p = params();
        let c = catalog();
        let eff = effective_rates(&p, EFFECTIVE_HORIZON);
        let rates = item_rates(&eff, &c);
        assert_eq!(rates.len(), c.items.len());
        for rarity in Rarity::ALL {
            let sum: u32 = rates
                .iter()
                .filter(|r| r.rarity == rarity)
                .map(|r| r.prob_millionths)
                .sum();
            assert_eq!(
                sum,
                eff.weight(rarity),
                "{rarity} 档逐件概率之和不等于该档公示值"
            );
        }
        assert!(
            rates.iter().all(|r| r.prob_millionths > 0),
            "有物品概率为 0"
        );
    }

    #[test]
    fn simulation_lands_on_the_published_effective_rates() {
        // 8 万抽的单测哨兵：σ(SSR) ≈ 0.18%，0.15% 门禁在这个 n 上会抖，
        // 所以这里查"数量级 + 方向 + 四档齐全"，真正的门禁在 CLI 的 10⁶（固定 seed 可复现）上跑。
        let p = params();
        let c = catalog();
        let obs = simulate(&p, &c, 1, 80_000).unwrap();
        assert_eq!(obs.total(), obs.pulls, "四档计数之和 != 抽数，有档漏计");
        let eff = effective_rates(&p, EFFECTIVE_HORIZON);
        let fit = Fit::new(obs.counts, obs.pulls, &eff);
        assert!(
            fit.worst_dev() < 8 * MAX_DEV_MILLIONTHS,
            "偏差 {:?} 超出数量级容差",
            fit.dev_millionths
        );
        assert!(
            fit.chi2() < 100.0,
            "χ²={} ⇒ 实测与解析期望根本不是一个分布",
            fit.chi2()
        );
    }

    #[test]
    fn unpolled_subset_equals_the_published_base_rates() {
        let p = params();
        let c = catalog();
        let obs = simulate(&p, &c, 3, 100_000).unwrap();
        assert!(
            obs.base_pulls > obs.pulls / 2,
            "无保底子集只有 {} 抽，双列公示的第一列没内容可验",
            obs.base_pulls
        );
        let chk = BaseCheck::run(&obs.base_counts, obs.base_pulls, &p.rates);
        assert!(
            chk.ok(),
            "子集 {} 抽上四档 {:?} 对不上基础概率 {:?}",
            chk.subset,
            chk.counts,
            p.rates
        );
    }

    #[test]
    fn base_check_is_not_vacuous() {
        // 造一份确实被污染的数据，6σ 判据必须判红 ⇒ 上面那条绿不是运气。
        let p = params();
        let n = 100_000u64;
        let mut exact = [0u64; 4];
        for (i, &w) in [p.rates.n, p.rates.r, p.rates.sr, p.rates.ssr]
            .iter()
            .enumerate()
        {
            exact[i] = w as u64 * n / SCALE as u64;
        }
        assert!(BaseCheck::run(&exact, n, &p.rates).ok());
        let mut skewed = exact;
        skewed[Rarity::N.index()] -= 500;
        skewed[Rarity::SSR.index()] += 500;
        assert!(
            !BaseCheck::run(&skewed, n, &p.rates).ok(),
            "搬了 0.5% 还没判红 ⇒ 容差写到天上去了"
        );
    }

    #[test]
    fn fit_gates_reject_a_biased_dice() {
        let p = params();
        let eff = effective_rates(&p, EFFECTIVE_HORIZON);
        let n = 1_000_000u64;
        // 按期望造的数据：χ² ≈ 0，两条门禁都必须绿（门禁不误伤正确实现）。
        let honest: [u64; 4] = expected_counts(&p, n).map(|v| v.round() as u64);
        let fit = Fit::new(honest, n, &eff);
        assert!(
            fit.dev_ok() && fit.chi2_ok(),
            "按期望造的数据被门禁拒了：{fit:?}"
        );
        // 把 1% 的点从 N 搬到 SSR ⇒ 两条门禁都必须红。
        let mut off = honest;
        off[Rarity::N.index()] -= 10_000;
        off[Rarity::SSR.index()] += 10_000;
        let fit2 = Fit::new(off, n, &eff);
        assert!(!fit2.dev_ok(), "偏 1% 却没触发偏差门禁");
        assert!(!fit2.chi2_ok(), "偏 1% 却没触发 χ² 门禁");
    }

    #[test]
    fn p_value_is_monotone_and_brackets_the_critical_value() {
        let mut f = Fit {
            pulls: 1,
            observed: [0; 4],
            published: [0; 4],
            dev_millionths: [0; 4],
            chi2_millionths: 0,
        };
        assert!((f.p_value() - 1.0).abs() < 1e-9, "χ²=0 的 p 应当是 1");
        f.chi2_millionths = CHI2_CRITICAL_MILLIONTHS;
        let at_gate = f.p_value();
        assert!(
            (at_gate - 0.05).abs() < 2e-4,
            "临界值处的 p 应当 ≈ 0.05，实测 {at_gate}"
        );
        f.chi2_millionths = 20_000_000;
        assert!(f.p_value() < at_gate / 10.0, "p 值不随 χ² 单调下降");
    }

    #[test]
    fn floor_pity_and_shard_counters_are_reported() {
        let p = params();
        let c = catalog();
        let obs = simulate(&p, &c, 1, 10_000).unwrap();
        assert!(obs.floor_hits > 0, "十连地板一次都没兑现");
        assert!(obs.duplicates > 0);
        assert!(obs.shards > 0);
        // 软保底在第 49 抽就饱和（pity.rs 钉着这条），所以硬保底 80 应当只是纸面兜底。
        assert_eq!(obs.hard_pity_hits, 0);
        assert!(obs.max_since_ssr < p.hard_pity);
    }

    #[test]
    fn same_seed_same_counts_across_runs() {
        let p = params();
        let c = catalog();
        let a = simulate(&p, &c, 42, 5_000).unwrap();
        let b = simulate(&p, &c, 42, 5_000).unwrap();
        assert_eq!(a, b, "同 seed 的模拟不可重放");
        let d = simulate(&p, &c, 43, 5_000).unwrap();
        assert_ne!(a.counts, d.counts, "换 seed 计数一模一样 ⇒ 随机数没起作用");
    }
}
