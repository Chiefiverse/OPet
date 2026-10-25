//! **全项目唯一**读墙钟的地方。
//!
//! `petcore` 禁止碰时间（硬约束 #8：同 seed 逐位可重放），但 `gacha_pull.ts`、
//! `inventory.obtained_at`、`session_log.ts` 三列按 §3.4 就是墙钟列。所以它们只允许
//! 从这一个函数取值 ⇒ 将来要换成可注入的假时钟（测试用）或 `CLOCK_MONOTONIC`（省电计时用），
//! 改动点只有这里。
//!
//! 单位 = **秒**（`i64`）。选秒而不是毫秒：§3.4 的 `ts`/`start_at`/`end_at` 是同一口径，
//! 且抽取的先后顺序由 `gacha_pull.seq` 决定、不由 `ts` 决定 ⇒ 秒级精度不影响审计。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 UTC 秒。`SystemTime` 早于 epoch（理论上可能）时钳到 0，而不是 panic——
/// 时间戳只是"给人看的元数据"，它不该让一次抽取失败。
pub fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

/// 把任意 `SystemTime` 换算成同样的秒口径（测试与导入旧库用）。
pub fn secs_of(t: SystemTime) -> i64 {
    i64::try_from(t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_a_plausible_unix_second() {
        let n = now_secs();
        // 2001-09-09（Unix 秒四位数进位）< now < 2100
        assert!((1_000_000_000..4_102_444_800).contains(&n), "{n}");
    }

    #[test]
    fn monotone_within_a_process() {
        let a = now_secs();
        let b = now_secs();
        assert!(b >= a, "同一进程内两次取值不该倒退: {a} → {b}");
    }
}
