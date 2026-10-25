//! 每一抽一条独立的骰子流。
//!
//! **为什么派生而不是共用一条流**：`pull()` 内部掷几次骰（稀有度、物品、地板补偿）
//! 会随实现演进而变化。若所有抽共用一条流，加一次 reroll 就会平移**之后所有历史抽**的
//! 骰子，第三方拿 `seed + seq` 重放出来的结果和当初写进账本的对不上——§3.6:592
//! "任何第三方脚本可重放验证概率"当场作废。派生流把每抽隔开：改每抽骰数只影响新抽，
//! 历史 `rng_state_hash` 永远不变。

use crate::rng::Rng;

use super::params::SCALE;

/// `(seed, seq) → 该抽专用种子`。**冻结函数**：改了它 = 重放全部历史，必须升审计格式版本。
///
/// 用 splitmix64 的 finalizer 混合两个输入，保证 seq 相邻的种子不相关。
pub const fn derive_seed(seed: u64, seq: u64) -> u64 {
    // 域分隔常数：不让 (seed, seq) 与 (seq, seed) 交换后同值。
    let mut z = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(seq)
        .wrapping_add(0x6A09_E667_F3BC_C8AB);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 一抽的骰子流，顺带记账"这抽用了几次骰"（写进审计记录，见 `audit::REC_FORMAT`）。
#[derive(Clone, Debug)]
pub struct PullStream {
    rng: Rng,
    dice: u32,
}

impl PullStream {
    pub fn new(seed: u64, seq: u64) -> Self {
        Self {
            rng: Rng::new(derive_seed(seed, seq)),
            dice: 0,
        }
    }

    /// `[0, scale)` 上的整数掷骰。`scale` 必须是 `SCALE` 或它的因子分组。
    ///
    /// **不做拒绝采样**：拒绝采样让骰数随结果变化，与上面"派生流"要解决的稳定性相冲突，
    /// 而收益微小——偏差上界见 [`Self::modulo_bias_bound`]。
    pub fn roll(&mut self, scale: u32) -> u32 {
        assert!(scale > 0, "掷骰刻度必须 > 0");
        self.dice += 1;
        (self.rng.next_u64() % scale as u64) as u32
    }

    pub fn roll_scale(&mut self) -> u32 {
        self.roll(SCALE)
    }

    /// `[0, scale)`，`u64` 版：给"同档内权重总和"这类非定刻度的掷骰用。
    pub fn roll_u64(&mut self, scale: u64) -> u64 {
        assert!(scale > 0, "掷骰刻度必须 > 0");
        self.dice += 1;
        self.rng.next_u64() % scale
    }

    pub const fn dice_used(&self) -> u32 {
        self.dice
    }

    /// 取模偏差的**理论上界**（相对值）：`scale / 2^64`。
    /// `SCALE = 1e6` ⇒ `5.4e-11`，比 §6.1 门禁的 `0.15%` 小 6 个数量级。
    pub const fn modulo_bias_bound(scale: u32) -> f64 {
        scale as f64 / (1u128 << 64) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_seed_is_stable_and_order_sensitive() {
        // 冻结值：任何改动都会让历史重放漂移，所以这里钉死而不是只测"看起来分散"。
        assert_eq!(derive_seed(1, 0), 0xFB6F_BBFB_8261_7A41);
        assert_eq!(derive_seed(1, 1), 0xBF8F_8F72_3CDB_820F);
        assert_eq!(derive_seed(42, 137), 0x786C_4BE2_A59F_5F76);
        assert_eq!(derive_seed(0, 0), 0x5501_C075_8FD1_9C5A);
        assert_ne!(derive_seed(1, 0), derive_seed(0, 1));
    }

    #[test]
    fn same_seed_seq_replays_bit_identically() {
        for seq in [0u64, 1, 7, 1_000_000] {
            let mut a = PullStream::new(42, seq);
            let mut b = PullStream::new(42, seq);
            for _ in 0..200 {
                assert_eq!(a.roll_scale(), b.roll_scale());
            }
        }
    }

    #[test]
    fn one_roll_does_not_disturb_other_pulls() {
        // 派生流的核心承诺：给 seq=5 多掷几次，seq=6 的骰子序列一字不变。
        let mut probe = PullStream::new(99, 5);
        let mut baseline = PullStream::new(99, 6);
        let before: Vec<u32> = (0..40).map(|_| baseline.roll_scale()).collect();
        let _ = [
            probe.roll_scale(),
            probe.roll_scale(),
            probe.roll_scale(),
            probe.roll(37),
        ];
        let mut after = PullStream::new(99, 6);
        for want in before {
            assert_eq!(after.roll_scale(), want, "别的抽被污染了");
        }
    }

    #[test]
    fn dice_counter_tracks_consumption() {
        let mut s = PullStream::new(3, 3);
        assert_eq!(s.dice_used(), 0);
        let _ = s.roll_scale();
        let _ = s.roll(10);
        assert_eq!(s.dice_used(), 2);
        assert_eq!(s.rng.counter(), 2);
    }

    #[test]
    fn rolls_stay_in_range_and_bias_bound_is_tiny() {
        let mut s = PullStream::new(7, 11);
        for _ in 0..10_000 {
            assert!(s.roll(1) < 1);
            assert!(s.roll(37) < 37);
            assert!(s.roll_scale() < SCALE);
            assert!(s.roll_u64(30) < 30);
            assert!(s.roll_u64(1_000_000_000_000) < 1_000_000_000_000);
        }
        assert!(PullStream::modulo_bias_bound(SCALE) < 1e-9);
    }

    #[test]
    #[should_panic(expected = "掷骰刻度必须 > 0")]
    fn zero_scale_is_a_programming_error() {
        PullStream::new(1, 1).roll(0);
    }

    #[test]
    #[should_panic(expected = "掷骰刻度必须 > 0")]
    fn zero_scale_is_a_programming_error_for_u64_too() {
        PullStream::new(1, 1).roll_u64(0);
    }
}
