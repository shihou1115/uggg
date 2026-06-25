//! 時刻注入 (M5-B)。
//!
//! LLM (advanced モード) の system prompt に「いまの日時」を埋め込んで、
//! 「今何時?」「今日何曜日?」に正しく答えられるようにする。
//! `Settings.tools_enabled = true` のときのみ `dialogue/advanced.rs` から呼ぶ。

use chrono::{DateTime, Datelike, Local};

/// 「2026-06-22 (土) 14:30 JST」のような人間可読な時刻文字列を返す。
pub fn now_jp_label() -> String {
    let now: DateTime<Local> = Local::now();
    format_jp_label(now)
}

fn format_jp_label(t: DateTime<Local>) -> String {
    const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
    // chrono::Weekday::num_days_from_monday() = 月曜日 0 .. 日曜日 6
    let weekday_idx = t.weekday().num_days_from_monday() as usize;
    let weekday = WEEKDAYS.get(weekday_idx).copied().unwrap_or("?");
    t.format("%Y-%m-%d")
        .to_string()
        .chars()
        .chain(format!(" ({weekday}) ").chars())
        .chain(t.format("%H:%M").to_string().chars())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    #[test]
    fn jp_label_includes_weekday_kanji() {
        // 2026-06-22 (Mon) 14:30 JST
        let dt = Local.with_ymd_and_hms(2026, 6, 22, 14, 30, 0).unwrap();
        let label = format_jp_label(dt);
        assert!(label.contains("2026-06-22"));
        assert!(label.contains("(月)"));
        assert!(label.contains("14:30"));
    }

    #[test]
    fn jp_label_other_weekday() {
        // 2026-06-27 (Sat)
        let dt = Local.with_ymd_and_hms(2026, 6, 27, 9, 0, 0).unwrap();
        let label = format_jp_label(dt);
        assert!(label.contains("(土)"));
    }
}
