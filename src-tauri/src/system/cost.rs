//! LLM コスト集計と閾値判定 (spec §4.2.7)。
//!
//! `api_usage` テーブルの当月分合計を出し、`monthly_limit_usd` に対して
//! 80% / 100% を超えたかを返す。降格・通知の発火は呼び出し側 (dialogue::mod.rs) で行う。

use anyhow::Result;
use chrono::{Datelike, TimeZone, Utc};

use crate::db::Db;

#[derive(Debug, Clone, Copy)]
pub struct CostStatus {
    pub current_usd: f64,
    pub limit_usd: f64,
    /// 上限が 0 なら無制限扱い。
    pub unlimited: bool,
    pub ratio: f64,
    pub reached_80: bool,
    pub exceeded: bool,
}

/// 当月集計 + 閾値判定。
pub fn check_status(db: &Db, monthly_limit_usd: f64) -> Result<CostStatus> {
    let month_start = month_start_unix();
    let current = db.sum_cost_since(month_start)?;
    let unlimited = monthly_limit_usd <= 0.0;
    let (ratio, reached_80, exceeded) = if unlimited {
        (0.0, false, false)
    } else {
        let r = current / monthly_limit_usd;
        (r, r >= 0.8, r >= 1.0)
    };
    Ok(CostStatus {
        current_usd: current,
        limit_usd: monthly_limit_usd,
        unlimited,
        ratio,
        reached_80,
        exceeded,
    })
}

/// 今月 1 日 00:00 UTC の unix 秒。
pub fn month_start_unix() -> i64 {
    let now = Utc::now();
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now);
    start.timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_when_zero() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.migrate().unwrap();
        let s = check_status(&db, 0.0).unwrap();
        assert!(s.unlimited);
        assert!(!s.reached_80);
        assert!(!s.exceeded);
    }

    #[test]
    fn flags_at_thresholds() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.migrate().unwrap();
        db.append_api_usage(&crate::db::ApiUsageRow {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 4.0,
            ts: month_start_unix() + 1,
        })
        .unwrap();
        let s = check_status(&db, 5.0).unwrap();
        assert!(s.reached_80, "4/5 = 80%");
        assert!(!s.exceeded);

        db.append_api_usage(&crate::db::ApiUsageRow {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 2.0,
            ts: month_start_unix() + 2,
        })
        .unwrap();
        let s = check_status(&db, 5.0).unwrap();
        assert!(s.exceeded, "6/5 > 100%");
    }
}
