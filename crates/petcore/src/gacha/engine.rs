//! 一次完整抽取：稀有度 → 具体物品 → 是否重复 → 折算多少碎片。
//!
//! 纯函数。扣币、写库、时间戳都在 `crates/petstore`；这里只有 `(参数, 目录, 状态, seed, seq) → 结果`。
//! [`Engine::new`] 一次性校验参数与目录，所以每次抽取不会重复做 30 件物品的结构检查
//! （1M 次模拟里那是白扔的时间），校验失败在建 Engine 时就炸，不会"带着坏参数继续掷"。
//!
//! 固定 **2 次骰**（稀有度 1 次 + 同档内选物品 1 次），硬保底时省掉第一次 ⇒ 1 次。
//! 骰数记进 [`PullOutcome::dice_used`] 并进审计记录：它一变，该抽的骰子序列就变——
//! 而派生流（[`super::stream`]）保证这不会波及别的抽。

use std::collections::BTreeSet;
use std::fmt;

use super::catalog::{Catalog, ItemKind};
use super::params::GachaParams;
use super::pity::{PullState, roll_rarity};
use super::rarity::Rarity;
use super::stream::PullStream;

/// 已拥有集合。**用 `BTreeSet` 而不是 `HashSet`**：硬约束 #8 禁止依赖哈希迭代顺序，
/// 这里虽然只查成员，但把"顺序无关"变成"类型上就无关"更省事。
pub type Owned = BTreeSet<String>;

/// 参数 + 目录，校验过一次之后的抽取入口。
#[derive(Clone, Copy, Debug)]
pub struct Engine<'a> {
    params: &'a GachaParams,
    catalog: &'a Catalog,
}

impl<'a> Engine<'a> {
    pub fn new(params: &'a GachaParams, catalog: &'a Catalog) -> Result<Self, GachaError> {
        params.validate()?;
        catalog.validate()?;
        Ok(Self { params, catalog })
    }

    pub const fn params(&self) -> &'a GachaParams {
        self.params
    }

    pub const fn catalog(&self) -> &'a Catalog {
        self.catalog
    }

    /// 单抽。返回结果与推进后的状态——**状态由调用方保存**
    /// （`petstore` 从 `gacha_pull` 账本推导，见计划"关键决定 3"）。
    pub fn pull(
        &self,
        state: &PullState,
        owned: &mut Owned,
        seed: u64,
        seq: u64,
    ) -> Result<(PullOutcome, PullState), GachaError> {
        let mut stream = PullStream::new(seed, seq);
        self.pull_with(state, owned, seq, &mut stream)
    }

    /// 一批（单抽或整批十连）。`first_seq` 起依次 +1；`owned` 边抽边插入 ⇒
    /// 同一批里抽出两件相同物品，第二件算重复。
    pub fn pull_batch(
        &self,
        state: &PullState,
        owned: &mut Owned,
        seed: u64,
        first_seq: u64,
        count: u32,
    ) -> Result<(Vec<PullOutcome>, PullState), GachaError> {
        let mut cur = *state;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let seq = first_seq + i as u64;
            let mut stream = PullStream::new(seed, seq);
            let (outcome, next) = self.pull_with(&cur, owned, seq, &mut stream)?;
            out.push(outcome);
            cur = next;
        }
        Ok((out, cur))
    }

    fn pull_with(
        &self,
        state: &PullState,
        owned: &mut Owned,
        seq: u64,
        stream: &mut PullStream,
    ) -> Result<(PullOutcome, PullState), GachaError> {
        let rarity_roll = roll_rarity(self.params, state, stream);
        let total = self.catalog.total_weight(rarity_roll.rarity);
        if total == 0 {
            return Err(GachaError::EmptyPool(rarity_roll.rarity));
        }
        let item = self
            .catalog
            .pick_weighted(rarity_roll.rarity, stream.roll_u64(total))
            .ok_or(GachaError::EmptyPool(rarity_roll.rarity))?;

        let duplicate = owned.contains(&item.id);
        let shards_gained = if duplicate {
            self.params.dup_shards[rarity_roll.rarity.index()]
        } else {
            0
        };
        owned.insert(item.id.clone());
        let next = state.advanced(self.params, rarity_roll.rarity);

        Ok((
            PullOutcome {
                seq,
                rarity: rarity_roll.rarity,
                item_id: item.id.clone(),
                item_kind: item.kind,
                duplicate,
                shards_gained,
                ssr_prob: rarity_roll.ssr_prob,
                soft_bonus: rarity_roll.soft_bonus,
                hard_pity: rarity_roll.hard_pity,
                sr_floor: rarity_roll.sr_floor,
                pity_before: state.since_ssr,
                batch_index: state.batch_index,
                pity_after: next.since_ssr,
                dice_used: stream.dice_used(),
            },
            next,
        ))
    }
}

/// 一抽的全部产出，也是审计记录的内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullOutcome {
    pub seq: u64,
    pub rarity: Rarity,
    pub item_id: String,
    pub item_kind: ItemKind,
    /// 抽之前（含同 batch 内先出的）是否已经有过这件。
    pub duplicate: bool,
    /// 重复时的碎片折算（§3.6:591），不重复为 0。
    pub shards_gained: u32,
    /// 判定诊断量，`--dry-run` 与审计都直接打印它们。
    pub ssr_prob: u32,
    pub soft_bonus: u32,
    pub hard_pity: bool,
    pub sr_floor: bool,
    pub pity_before: u32,
    pub pity_after: u32,
    /// 本抽在 batch（十连窗）里的位置，0-indexed。审计重放靠它复现十连地板。
    pub batch_index: u32,
    pub dice_used: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GachaError {
    Params(super::params::ParamsError),
    Catalog(super::catalog::CatalogError),
    /// 公示里有这一档的概率、目录里却选不出物品。结构校验本应挡住，留着是最后一道。
    EmptyPool(Rarity),
}

impl From<super::params::ParamsError> for GachaError {
    fn from(e: super::params::ParamsError) -> Self {
        Self::Params(e)
    }
}

impl From<super::catalog::CatalogError> for GachaError {
    fn from(e: super::catalog::CatalogError) -> Self {
        Self::Catalog(e)
    }
}

impl fmt::Display for GachaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Params(e) => write!(f, "参数非法: {e}"),
            Self::Catalog(e) => write!(f, "目录非法: {e}"),
            Self::EmptyPool(r) => write!(f, "{r} 档选不出物品"),
        }
    }
}

impl std::error::Error for GachaError {}

/// 十连地板是否兑现（§3.6:593）——看一批的聚合，不看单抽。
pub fn batch_has_sr_plus(outcomes: &[PullOutcome]) -> bool {
    outcomes.iter().any(|o| o.rarity.sr_or_better())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gacha::catalog::Catalog;

    fn with_engine<T>(f: impl FnOnce(&GachaParams, &Catalog, &Engine<'_>) -> T) -> T {
        let p = GachaParams::V1;
        let c = Catalog::default_v1().unwrap();
        let e = Engine::new(&p, &c).unwrap();
        f(&p, &c, &e)
    }

    #[test]
    fn same_inputs_replay_bit_identically() {
        with_engine(|_p, _c, e| {
            for seed in [1u64, 999, 1_000_000_007] {
                let mut a = PullState::fresh();
                let mut b = PullState::fresh();
                let mut oa = Owned::new();
                let mut ob = Owned::new();
                for seq in 1..=500 {
                    let (x, na) = e.pull(&a, &mut oa, seed, seq).unwrap();
                    let (y, nb) = e.pull(&b, &mut ob, seed, seq).unwrap();
                    assert_eq!(x, y, "seq={seq} 重放不一致");
                    assert_eq!(na, nb);
                    a = na;
                    b = nb;
                }
            }
        });
    }

    #[test]
    fn dice_count_is_fixed_per_pull_kind() {
        with_engine(|p, _c, e| {
            let mut state = PullState::fresh();
            let mut owned = Owned::new();
            let (o, _) = e.pull(&state, &mut owned, 3, 1).unwrap();
            assert!(!o.hard_pity);
            assert_eq!(o.dice_used, 2);
            // 硬保底：省掉稀有度骰。
            state.since_ssr = p.hard_pity - 1;
            let (o2, _) = e.pull(&state, &mut owned, 3, 2).unwrap();
            assert!(o2.hard_pity);
            assert_eq!(o2.dice_used, 1);
            assert_eq!(o2.pity_after, 0, "出了 SSR 窗口必须归零");
        });
    }

    #[test]
    fn every_ten_pull_contains_sr_or_better() {
        with_engine(|p, _c, e| {
            for seed in 0..2_000u64 {
                let mut state = PullState::fresh();
                let mut owned = Owned::new();
                let mut first_seq = 1u64;
                for round in 0..3u32 {
                    let (batch, next) = e
                        .pull_batch(&state, &mut owned, seed, first_seq, p.batch_size)
                        .unwrap();
                    assert_eq!(batch.len(), 10);
                    assert!(
                        batch_has_sr_plus(&batch),
                        "seed={seed} round={round} 的十连里没有 SR 以上，违反 §3.6:593"
                    );
                    assert_eq!(batch[0].seq, first_seq);
                    assert_eq!(batch[9].seq, first_seq + 9);
                    state = next;
                    first_seq += 10;
                }
            }
        });
    }

    #[test]
    fn batch_path_is_the_same_dice_as_the_single_path() {
        with_engine(|p, _c, e| {
            let state = PullState::fresh();
            let mut owned = Owned::new();
            let (batch, next) = e
                .pull_batch(&state, &mut owned, 5, 101, p.batch_size)
                .unwrap();
            let seqs: Vec<u64> = batch.iter().map(|o| o.seq).collect();
            assert_eq!(seqs, (101..111).collect::<Vec<u64>>());
            // 十连不是另一套随机数：逐抽走同一条路径要一模一样。
            let mut owned2 = Owned::new();
            let mut s2 = PullState::fresh();
            for seq in 101..111u64 {
                let (o, n) = e.pull(&s2, &mut owned2, 5, seq).unwrap();
                let want = batch.iter().find(|x| x.seq == seq).unwrap();
                assert_eq!(&o, want, "seq={seq} 两条路径不一致");
                s2 = n;
            }
            assert_eq!(s2, next);
        });
    }

    #[test]
    fn soft_pity_makes_the_hard_pity_a_decoration() {
        // v1 数值下的事实（pity.rs 的 `soft_ramp_saturates_well_before_hard_pity` 已算出饱和点 49）：
        // SSR 概率在第 49 抽撞上 62% 上限，此后每抽 62% ⇒ 窗口拖到 80 抽的概率约 1e-13。
        // 所以 `hard_pity = 80` 在这组数值里**永不触发**，是纸面兜底。
        // 固定 seed ⇒ 这条是确定性的，不会某天突然变红；真要改，改的是公示数值而不是这里的断言。
        with_engine(|p, _c, e| {
            for seed in 0..50u64 {
                let mut state = PullState::fresh();
                let mut owned = Owned::new();
                let mut worst = 0u32;
                for seq in 1..=5_000u64 {
                    let (o, next) = e.pull(&state, &mut owned, seed, seq).unwrap();
                    worst = worst.max(o.pity_before);
                    assert!(!o.hard_pity, "seed={seed} 竟然走到了硬保底");
                    state = next;
                }
                assert!(
                    worst < p.hard_pity,
                    "seed={seed} 出现 {worst} 抽无 SSR，硬保底不再只是兜底"
                );
            }
        });
    }

    #[test]
    fn duplicates_convert_at_the_published_rate_exactly_once() {
        with_engine(|p, _c, e| {
            let mut state = PullState::fresh();
            let mut owned = Owned::new();
            let (first, next) = e.pull(&state, &mut owned, 7, 1).unwrap();
            assert!(!first.duplicate, "空库存的第一抽不可能重复");
            assert_eq!(first.shards_gained, 0);
            state = next;
            let mut seen_dup = 0;
            for seq in 2..5_000u64 {
                let (o, next) = e.pull(&state, &mut owned, 7, seq).unwrap();
                if o.duplicate {
                    assert_eq!(
                        o.shards_gained,
                        p.dup_shards[o.rarity.index()],
                        "{} 的碎片折算跟稀有度不匹配",
                        o.rarity
                    );
                    seen_dup += 1;
                } else {
                    assert_eq!(o.shards_gained, 0, "首得也给了碎片？");
                }
                state = next;
            }
            assert!(seen_dup > 0, "5000 抽里没有一次重复 ⇒ 目录或随机数有问题");
            // 30 件物品全收齐之后，之后每一抽都必然是重复。
            assert_eq!(owned.len(), 30, "默认目录有 30 件，5000 抽该全收齐");
        });
    }

    #[test]
    fn invalid_params_or_catalog_fail_at_construction() {
        let p_bad = GachaParams {
            hard_pity: 1,
            ..GachaParams::V1
        };
        let c = Catalog::default_v1().unwrap();
        assert!(matches!(
            Engine::new(&p_bad, &c),
            Err(GachaError::Params(_))
        ));
        let p = GachaParams::V1;
        let mut thin = c.clone();
        thin.items.retain(|i| i.rarity != Rarity::SSR);
        assert!(matches!(
            Engine::new(&p, &thin),
            Err(GachaError::Catalog(_))
        ));
    }

    #[test]
    fn pity_moves_the_distribution_the_way_the_design_says() {
        // 不是 §6.1 的门禁（门禁在 CLI 的 1M 上跑），是 10 万量的哨兵：
        // band 顺序或保底被改错时这里先炸，不用等 1M 跑完。
        with_engine(|_p, _c, e| {
            let mut counts = [0u64; 4];
            let mut state = PullState::fresh();
            let mut owned = Owned::new();
            for seq in 1..=100_000u64 {
                let (o, next) = e.pull(&state, &mut owned, 20_003, seq).unwrap();
                counts[o.rarity.index()] += 1;
                state = next;
            }
            let total: u64 = counts.iter().sum();
            assert_eq!(total, 100_000);
            let pct = |v: u64| v as f64 * 100.0 / total as f64;
            // 保底只从 N 扣 ⇒ R/SR 的绝对 band 不变；十连地板把末抽的 N/R 升成 SR，
            // 所以只会削 R、抬高 SR，绝不可能把 R 抬到 28% 以上。
            assert!(pct(counts[Rarity::SSR.index()]) > 2.0, "SSR 没被保底抬起？");
            assert!(pct(counts[Rarity::N.index()]) < 60.0, "N 没被保底吃掉？");
            assert!(
                pct(counts[Rarity::SR.index()]) > 10.0,
                "SR 没被十连地板抬起？"
            );
            let r_pct = pct(counts[Rarity::R.index()]);
            assert!(
                r_pct <= 28.0 && (28.0 - r_pct) < 1.5,
                "R 只可能被地板削掉一点点，实测 {r_pct}"
            );
        });
    }
}
