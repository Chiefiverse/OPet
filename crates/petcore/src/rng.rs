//! 确定性随机数。内核内所有随机都来自这里，保证"同样的 seed ⇒ 同样的行为序列"（§2.4 可复现、可审计）。
//!
//! 采用 splitmix64：状态 64 bit、无分支、统计性质足够支撑行为决策与 M3 的加权抽样。
//! `counter` 只服务行为内核。**M3 的审计哈希不从这里取**：`rng_state_hash` 的唯一定义是
//! `gacha::audit::rng_state_hash` = `sha256(b"OPET-GACHA-RNG-1\n" ‖ u64be(seed) ‖ u64be(seq))`，
//! 每抽一条独立派生的骰子流（见 `gacha::stream`）。写在这里是为了防止再出现第二个定义。

#[derive(Clone, Debug)]
pub struct Rng {
    seed: u64,
    state: u64,
    counter: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            // 避免 seed=0 时状态退化成一个固定常数
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
            seed,
            counter: 0,
        }
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// 已消耗的次数，审计链里的 pull_counter。
    pub const fn counter(&self) -> u64 {
        self.counter
    }

    pub const fn state(&self) -> u64 {
        self.state
    }

    pub fn next_u64(&mut self) -> u64 {
        self.counter += 1;
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// [0, 1)
    pub fn next_f32(&mut self) -> f32 {
        // 24 bit ⇒ 1/2^24 分辨率，对行为决策与概率抽样足够，且能精确表示
        (self.next_u64() >> 40) as f32 * (1.0 / 16_777_216.0)
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }

    /// [lo, hi) 均匀分布；`lo >= hi` 时返回 `lo`。
    pub fn uniform(&mut self, lo: f32, hi: f32) -> f32 {
        if hi <= lo {
            return lo;
        }
        lo + self.next_f32() * (hi - lo)
    }

    /// 闭区间整数均匀分布。
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        let span = (hi - lo + 1) as u64;
        lo + (self.next_u64() % span) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn chance_matches_nominal_rate() {
        let mut rng = Rng::new(7);
        let n = 200_000;
        let hits = (0..n).filter(|_| rng.chance(0.25)).count();
        let rate = hits as f64 / n as f64;
        assert!((rate - 0.25).abs() < 0.01, "chance(0.25) 实测 {rate}");
    }

    #[test]
    fn uniform_stays_in_range() {
        let mut rng = Rng::new(11);
        for _ in 0..10_000 {
            let v = rng.uniform(90.0, 140.0);
            assert!((90.0..140.0).contains(&v), "{v}");
            assert!((0.0..1.0).contains(&rng.next_f32()));
        }
    }

    #[test]
    fn counter_tracks_consumption() {
        let mut rng = Rng::new(1);
        for i in 1..=5 {
            rng.next_u64();
            assert_eq!(rng.counter(), i);
        }
    }
}
