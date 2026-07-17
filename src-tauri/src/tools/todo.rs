//! ToDo・日課管理のドメインロジック (M8、spec §4.6.2 / daily-support-design §7.2)。
//!
//! - bucket / priority / recurring の正規化・検証 (DB は文字列のまま持つ)
//! - 日課復活の境界計算: daily = 今日のローカル 0:00、weekly = 今週月曜のローカル 0:00。
//!   done_ts が境界より前の done な日課を open へ戻す (`Db::reset_recurring_todos`)。
//!   TZ 変換は §2.5 契約に従い `reminder::local_to_utc_ts` を再利用する。

use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::{Datelike, Duration, NaiveDateTime};

use crate::state::AppState;
use crate::tools::reminder::local_to_utc_ts;

pub const BUCKETS: [&str; 3] = ["today", "week", "someday"];

/// bucket の検証。無効値はエラー (黙って today に倒さない)。
pub fn validate_bucket(bucket: &str) -> Result<&str> {
    if BUCKETS.contains(&bucket) {
        Ok(bucket)
    } else {
        Err(anyhow!("bucket は today / week / someday のいずれかです: {bucket}"))
    }
}

/// priority の検証 (0=普通, 1=高 の 2 段階のみ、spec §4.6.2)。
pub fn validate_priority(priority: i32) -> Result<i32> {
    if priority == 0 || priority == 1 {
        Ok(priority)
    } else {
        Err(anyhow!("priority は 0 (普通) か 1 (高) です: {priority}"))
    }
}

/// recurring の検証。None | 'daily' | 'weekly'。
pub fn validate_recurring(recurring: Option<&str>) -> Result<Option<&str>> {
    match recurring {
        None => Ok(None),
        Some("daily") => Ok(Some("daily")),
        Some("weekly") => Ok(Some("weekly")),
        Some(other) => Err(anyhow!("recurring は daily / weekly のいずれかです: {other}")),
    }
}

/// 日課復活の境界 (UTC 秒)。(今日のローカル 0:00, 今週月曜のローカル 0:00)。
pub fn recurring_cutoffs(now_local: NaiveDateTime) -> (i64, i64) {
    let today = now_local.date();
    let day_start = today.and_hms_opt(0, 0, 0).expect("midnight");
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let week_start = monday.and_hms_opt(0, 0, 0).expect("midnight");
    (local_to_utc_ts(day_start), local_to_utc_ts(week_start))
}

/// 日課の復活を実行する (起動時・日付変更時)。戻り値は open へ戻した件数。
pub fn reset_recurring(state: &Arc<AppState>) -> Result<u64> {
    let now_local = chrono::Local::now().naive_local();
    let (daily_cutoff, weekly_cutoff) = recurring_cutoffs(now_local);
    state.db.reset_recurring_todos(daily_cutoff, weekly_cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn validators() {
        assert!(validate_bucket("today").is_ok());
        assert!(validate_bucket("week").is_ok());
        assert!(validate_bucket("someday").is_ok());
        assert!(validate_bucket("tomorrow").is_err());
        assert!(validate_priority(0).is_ok());
        assert!(validate_priority(1).is_ok());
        assert!(validate_priority(2).is_err());
        assert!(validate_recurring(None).is_ok());
        assert!(validate_recurring(Some("daily")).is_ok());
        assert!(validate_recurring(Some("weekly")).is_ok());
        assert!(validate_recurring(Some("monthly")).is_err());
    }

    #[test]
    fn cutoffs_day_and_monday() {
        // 2026-07-16 は木曜。週境界は 7/13 (月) 0:00。
        let now = NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let (daily, weekly) = recurring_cutoffs(now);
        let day_start = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap().and_hms_opt(0, 0, 0).unwrap();
        let week_start = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap().and_hms_opt(0, 0, 0).unwrap();
        assert_eq!(daily, local_to_utc_ts(day_start));
        assert_eq!(weekly, local_to_utc_ts(week_start));
        // 月曜自身の週境界はその日の 0:00
        let mon = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap().and_hms_opt(0, 0, 0).unwrap();
        let (_, weekly_on_monday) = recurring_cutoffs(mon);
        assert_eq!(weekly_on_monday, local_to_utc_ts(week_start));
    }
}
