//! LLM コスト集計と閾値判定 (spec §4.2.7)。
//!
//! `api_usage` テーブルの当月分合計を出し、`monthly_limit_usd` に対して
//! 80% / 100% を超えたかを返す。降格・通知の発火は呼び出し側 (dialogue::mod.rs) で行う。

use anyhow::Result;
use chrono::{Datelike, TimeZone, Utc};

use crate::db::Db;

#[derive(Debug, Clone, Copy)]
pub struct CostStatus {
    /// 当月の累計コスト (USD)。設定 UI で表示する想定で保持。
    #[allow(dead_code)]
    pub current_usd: f64,
    /// 当月上限 (USD)。Settings.monthly_limit_usd と同期。
    #[allow(dead_code)]
    pub limit_usd: f64,
    /// 上限が 0 なら無制限扱い。
    pub unlimited: bool,
    /// 現在の使用率 (0.0..1.0+)。UI 表示用。
    #[allow(dead_code)]
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

/// 当月を表すタグ（`YYYY-MM`、UTC）。`month_start_unix` と同じ月境界を使う。
pub fn current_month_tag() -> String {
    let now = Utc::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

/// 「今月ぶんの告知を済ませたか」。
///
/// プロセス内の `AtomicBool` では (1) 再起動で消える (2) **月が替わっても戻らない**
/// ため、翌月の警告が二度と鳴らなかった。当月タグを `app_settings` に保存して比較する
/// （spec §4.2.7「次月リセットで復帰。」）。
pub fn notified_this_month(db: &Db, key: &str) -> bool {
    matches!(db.get_setting(key), Ok(Some(v)) if v == current_month_tag())
}

/// 当月ぶんの告知済みを記録する。
pub fn mark_notified_this_month(db: &Db, key: &str) {
    if let Err(err) = db.set_setting(key, &current_month_tag()) {
        crate::ulog!("[cost] mark_notified_this_month({key}) failed: {err:#}");
    }
}

/// 告知済みフラグの `app_settings` キー。
pub const KEY_WARNED_80: &str = "cost_warned_80_month";
pub const KEY_LIMIT_NOTIFIED: &str = "cost_limit_notified_month";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_tag_matches_month_start() {
        let tag = current_month_tag();
        assert_eq!(tag.len(), 7, "{tag}");
        let start = Utc.timestamp_opt(month_start_unix(), 0).single().unwrap();
        assert_eq!(tag, format!("{:04}-{:02}", start.year(), start.month()));
    }

    /// 告知済みフラグが **月をまたいで正しくリセットされる** こと。
    ///
    /// 以前は非永続の `AtomicBool` で、(1) 再起動で消える (2) 月が替わっても
    /// 戻らないため翌月の警告が二度と鳴らない、という 2 つの穴があった
    /// (spec §4.2.7「次月リセットで復帰。」)。
    #[test]
    fn notification_flag_is_scoped_to_current_month() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.migrate().unwrap();

        assert!(!notified_this_month(&db, KEY_LIMIT_NOTIFIED), "初期状態は未告知");
        mark_notified_this_month(&db, KEY_LIMIT_NOTIFIED);
        assert!(notified_this_month(&db, KEY_LIMIT_NOTIFIED), "同じ月では告知済み");

        // 先月の値が入っていたら「未告知」に戻る = 次月リセット。
        db.set_setting(KEY_LIMIT_NOTIFIED, "2000-01").unwrap();
        assert!(
            !notified_this_month(&db, KEY_LIMIT_NOTIFIED),
            "月が変われば再び告知できなければならない"
        );

        // 80% 警告のキーは上限超過のキーと独立していること。
        assert!(!notified_this_month(&db, KEY_WARNED_80));
        mark_notified_this_month(&db, KEY_WARNED_80);
        assert!(notified_this_month(&db, KEY_WARNED_80));
        assert!(!notified_this_month(&db, KEY_LIMIT_NOTIFIED), "キーは独立");
    }

    /// 告知済みフラグは**上限判定そのものには影響しない**こと。
    /// 「一度告知したら以後は超過扱いを解除する」という以前の挙動
    /// (AtomicBool の swap で再降格しない) に戻らないための固定。
    #[test]
    fn exceeded_stays_true_after_notifying() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.migrate().unwrap();
        db.append_api_usage(&crate::db::ApiUsageRow {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 2.0,
            ts: month_start_unix() + 1,
        })
        .unwrap();
        assert!(check_status(&db, 1.0).unwrap().exceeded);
        mark_notified_this_month(&db, KEY_LIMIT_NOTIFIED);
        assert!(
            check_status(&db, 1.0).unwrap().exceeded,
            "告知しても超過は超過のまま (課金が再開してはいけない)"
        );
    }

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
