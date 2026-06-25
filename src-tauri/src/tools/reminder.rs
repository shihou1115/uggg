//! リマインダー (M5-B, spec §4.5.3)。
//!
//! - `parse_request("3分後にお茶")` のように **「N 分後」「N 時間後」** の発話を検出する pure 関数
//! - `add` / `list` / `delete` で DB の `reminders` テーブルを CRUD
//! - 発火は `tasks::spawn_reminder_watcher` が `due_reminders(now)` を 10 秒間隔でポーリングし、
//!   `notify::ReminderFired` を経由してゴーストに告知 (静音中も鳴らす特例)。

use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;

use crate::db::ReminderRow;
use crate::state::AppState;

/// 「3分後にお茶を飲む」のような発話から抽出した要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderRequest {
    /// 現時刻からの相対秒数。
    pub offset_secs: i64,
    /// リマインダー本文 (「お茶を飲む」)。空文字なら呼び出し側で扱い方を決める。
    pub body: String,
}

/// 発話文字列から「N 分後 / N 時間後」を抽出する。マッチしなければ None。
/// 数字は半角アラビアのみ対応 (全角は M5-B では未対応、将来課題)。
pub fn parse_request(text: &str) -> Option<ReminderRequest> {
    let bytes = text.as_bytes();
    let mut idx = 0;
    let mut number_start: Option<usize> = None;

    while idx < bytes.len() {
        let ch = bytes[idx];
        if ch.is_ascii_digit() {
            if number_start.is_none() {
                number_start = Some(idx);
            }
            idx += 1;
            continue;
        }
        if let Some(start) = number_start {
            let num_str = &text[start..idx];
            let n: i64 = match num_str.parse() {
                Ok(v) if v > 0 => v,
                _ => {
                    number_start = None;
                    continue;
                }
            };
            // 後続の単位を見る
            let unit_segment = &text[idx..];
            let (unit_secs, unit_byte_len) = if unit_segment.starts_with("分後") {
                (60_i64, "分後".len())
            } else if unit_segment.starts_with("時間後") {
                (3600_i64, "時間後".len())
            } else if unit_segment.starts_with("秒後") {
                (1_i64, "秒後".len())
            } else {
                // この数字は対象外、次へ
                number_start = None;
                continue;
            };
            let offset_secs = n.saturating_mul(unit_secs);
            // 本文 = 「N 分後」より後ろの文字列。「に」「で」「には」等の助詞を 1 つ食う
            let mut after = &text[idx + unit_byte_len..];
            for prefix in ["には", "に", "で", "、", ",", " "] {
                if let Some(rest) = after.strip_prefix(prefix) {
                    after = rest;
                    break;
                }
            }
            let body = after.trim().to_string();
            return Some(ReminderRequest { offset_secs, body });
        }
        idx += 1;
    }
    None
}

pub fn add(state: &Arc<AppState>, text: &str, offset_secs: i64) -> Result<i64> {
    let now = Utc::now().timestamp();
    let due_ts = now.saturating_add(offset_secs);
    state.db.insert_reminder(due_ts, text, now)
}

pub fn list(state: &Arc<AppState>) -> Result<Vec<ReminderRow>> {
    state.db.list_reminders()
}

pub fn delete(state: &Arc<AppState>, id: i64) -> Result<()> {
    state.db.delete_reminder(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minutes() {
        let r = parse_request("3分後にお茶を飲む").unwrap();
        assert_eq!(r.offset_secs, 180);
        assert_eq!(r.body, "お茶を飲む");
    }

    #[test]
    fn parse_hours() {
        let r = parse_request("1時間後に休憩").unwrap();
        assert_eq!(r.offset_secs, 3600);
        assert_eq!(r.body, "休憩");
    }

    #[test]
    fn parse_seconds() {
        let r = parse_request("30秒後にお知らせ").unwrap();
        assert_eq!(r.offset_secs, 30);
        assert_eq!(r.body, "お知らせ");
    }

    #[test]
    fn parse_with_two_digit_minutes() {
        let r = parse_request("10分後で温まる予定").unwrap();
        assert_eq!(r.offset_secs, 600);
        assert_eq!(r.body, "温まる予定");
    }

    #[test]
    fn parse_no_match_returns_none() {
        assert!(parse_request("今何時?").is_none());
        assert!(parse_request("3 後にお茶").is_none()); // 単位なし
        assert!(parse_request("こんにちは").is_none());
    }

    #[test]
    fn parse_zero_is_rejected() {
        // 0 分後 は意味がないので扱わない
        assert!(parse_request("0分後").is_none());
    }

    #[test]
    fn parse_body_can_be_empty() {
        // 本文なしのときは body="" で返す (呼び出し側で「リマインダー」とデフォルト名にする等)
        let r = parse_request("5分後").unwrap();
        assert_eq!(r.offset_secs, 300);
        assert_eq!(r.body, "");
    }
}
