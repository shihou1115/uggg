//! 定例会話: 材料集約 + 定型文組み立て + advanced 言い回し整形
//! (M12、spec §4.7.1 / regular-talk-design §5.3・§5.4)。
//!
//! 材料選定・発話可否・文組み立ては low = ローカル決定論で完結する (AI 非依存)。
//! `build_*_materials` (async, DB/天気アクセスあり) と `build_*_script` (純関数) を分離し、
//! 文組み立てはテスト容易にしてある。発火条件・配達・dedup は呼び出し側
//! (`tasks::spawn_daily_watcher`) が持つ。
//!
//! advanced 上乗せ (`polish_script`) は low が組んだ script を LLM に 1 回渡して
//! 言い回しのみを整形する (材料選定・発話可否には触れない、spec §4.6.3 原則)。
//! mode != advanced・降格中・API エラー・タイムアウトのいずれでも script をそのまま返す
//! (low フォールバック必須)。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::db::{ApiUsageRow, ReminderFilter};
use crate::dialogue::llm::{estimate_cost_usd, ChatMessage, LlmClient};
use crate::state::{AppState, DialogueMode};
use crate::system::{secrets, weather};
use crate::tools::reminder::local_to_utc_ts;

/// advanced 整形の呼び出しタイムアウト (§5.4「実装時に確定」)。
/// 対話チャット (180 秒) より短くする: 背景配達がいつまでも詰まらないように。
const REGULAR_LLM_TIMEOUT_SECS: u64 = 20;

// ===== 材料 (§5.3) =====

/// 予定 1 件分の要約 (先頭 1 件 + 総件数)。
pub struct CalendarSummary {
    pub summary: String,
    /// None = 終日 (時刻省略)。
    pub time_label: Option<String>,
    /// 表示窓内の総件数 (1 件なら「ほか」なし)。
    pub total: usize,
}

/// 未完了リマインダーの要約 (先頭 1 件 + 総件数)。
pub struct ReminderSummary {
    pub text: String,
    pub total: usize,
}

/// 天気 1 日分の要約 (ラベル組み立て済み)。
pub struct WeatherSummary {
    pub label: &'static str,
    pub temp_max: f64,
    pub temp_min: f64,
    pub precip_prob_max: u8,
}

pub struct MorningMaterials {
    pub calendar: Option<CalendarSummary>,
    pub todo_count: u64,
    pub reminder: Option<ReminderSummary>,
    pub weather: Option<WeatherSummary>,
}

pub struct EveningMaterials {
    pub done_count: u64,
    pub open_count: u64,
    pub calendar: Option<CalendarSummary>,
    pub weather: Option<WeatherSummary>,
}

fn weather_summary(d: &weather::DailyWeather) -> Option<WeatherSummary> {
    weather::weather_label(d.weather_code).map(|label| WeatherSummary {
        label,
        temp_max: d.temp_max,
        temp_min: d.temp_min,
        precip_prob_max: d.precip_prob_max,
    })
}

fn format_hhmm(start_ts: i64) -> String {
    use chrono::Timelike;
    let dt = chrono::DateTime::from_timestamp(start_ts, 0)
        .map(|d| d.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);
    format!("{}:{:02}", dt.hour(), dt.minute())
}

/// [from_date, to_date_exclusive) の予定から開始順の先頭 1 件 + 総件数を取り出す (§5.3)。
/// 取得失敗は項目ごと省く (None)。
fn calendar_summary(
    state: &Arc<AppState>,
    from_date: chrono::NaiveDate,
    to_date_exclusive: chrono::NaiveDate,
) -> Option<CalendarSummary> {
    let from_ts = local_to_utc_ts(from_date.and_hms_opt(0, 0, 0)?);
    let to_ts = local_to_utc_ts(to_date_exclusive.and_hms_opt(0, 0, 0)?);
    let events = state.db.list_calendar(from_ts, to_ts).unwrap_or_else(|err| {
        eprintln!("[regular_talk] list_calendar failed: {err:#}");
        Vec::new()
    });
    let first = events.first()?;
    let time_label = if first.all_day {
        None
    } else {
        Some(format_hhmm(first.start_ts))
    };
    Some(CalendarSummary {
        summary: first.summary.clone(),
        time_label,
        total: events.len(),
    })
}

/// 未完了リマインダー (`list_reminders(Active)` の `pending == true`) を due_ts 降順
/// (= 直近に発火したもの優先) で先頭 1 件 + 総件数にする (§5.3)。
fn pending_reminder_summary(state: &Arc<AppState>) -> Option<ReminderSummary> {
    let all = state.db.list_reminders(ReminderFilter::Active).unwrap_or_else(|err| {
        eprintln!("[regular_talk] list_reminders failed: {err:#}");
        Vec::new()
    });
    let mut pending: Vec<_> = all.into_iter().filter(|r| r.pending).collect();
    if pending.is_empty() {
        return None;
    }
    pending.sort_by(|a, b| b.due_ts.cmp(&a.due_ts));
    let total = pending.len();
    Some(ReminderSummary {
        text: pending[0].text.clone(),
        total,
    })
}

/// 朝の材料集約 (§5.3)。取得失敗した材料は項目ごと省く (全体を失敗させない)。
pub async fn build_morning_materials(state: &Arc<AppState>) -> MorningMaterials {
    let today = chrono::Local::now().date_naive();
    let tomorrow = today + chrono::Duration::days(1);
    let calendar = calendar_summary(state, today, tomorrow);
    let todo_count = state.db.count_open_todos(Some("today")).unwrap_or_else(|err| {
        eprintln!("[regular_talk] count_open_todos failed: {err:#}");
        0
    });
    let reminder = pending_reminder_summary(state);
    let weather = match weather::ensure_fresh(state).await {
        Some(cache) => weather::today_material(&cache).and_then(weather_summary),
        None => None,
    };
    MorningMaterials {
        calendar,
        todo_count,
        reminder,
        weather,
    }
}

/// 夜の材料集約 (§5.3)。
pub async fn build_evening_materials(state: &Arc<AppState>) -> EveningMaterials {
    let today = chrono::Local::now().date_naive();
    let tomorrow = today + chrono::Duration::days(1);
    let day_after = tomorrow + chrono::Duration::days(1);
    let done_count = match today.and_hms_opt(0, 0, 0) {
        Some(midnight) => state
            .db
            .count_done_todos_since(local_to_utc_ts(midnight))
            .unwrap_or_else(|err| {
                eprintln!("[regular_talk] count_done_todos_since failed: {err:#}");
                0
            }),
        None => 0,
    };
    let open_count = state.db.count_open_todos(Some("today")).unwrap_or_else(|err| {
        eprintln!("[regular_talk] count_open_todos failed: {err:#}");
        0
    });
    let calendar = calendar_summary(state, tomorrow, day_after);
    let weather = match weather::ensure_fresh(state).await {
        Some(cache) => weather::tomorrow_material(&cache).and_then(weather_summary),
        None => None,
    };
    EveningMaterials {
        done_count,
        open_count,
        calendar,
        weather,
    }
}

// ===== 定型文組み立て (§5.4。中立簡潔体・「。」区切り・空項目 skip) =====
//
// 各項目は既に「。」で終わる完成文として組み立て、空でない項目をそのまま連結する
// (項目文自体が文末の句点を持つため、追加の区切り文字は要らない)。

/// 朝の集約文。材料が全部空なら空文字を返す (辞書側の導入だけで短いあいさつになる)。
pub fn build_morning_script(m: &MorningMaterials) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(cal) = &m.calendar {
        parts.push(calendar_line("今日の予定は", cal));
    }
    if m.todo_count > 0 {
        parts.push(format!("ToDo は{}件。", m.todo_count));
    }
    if let Some(r) = &m.reminder {
        parts.push(reminder_line(r));
    }
    if let Some(w) = &m.weather {
        parts.push(format!(
            "天気は{}、最高{}℃。降水確率{}%。",
            w.label,
            fmt_temp(w.temp_max),
            w.precip_prob_max
        ));
    }
    parts.concat()
}

/// 夜の集約文。同上。
pub fn build_evening_script(m: &EveningMaterials) -> String {
    let mut parts: Vec<String> = Vec::new();
    if m.done_count > 0 {
        parts.push(format!("今日終わった ToDo は{}件。", m.done_count));
    }
    if m.open_count > 0 {
        parts.push(format!("残りは{}件。", m.open_count));
    }
    if let Some(cal) = &m.calendar {
        parts.push(calendar_line("明日の予定は", cal));
    }
    if let Some(w) = &m.weather {
        parts.push(format!(
            "明日は{}、最高{}℃／最低{}℃。",
            w.label,
            fmt_temp(w.temp_max),
            fmt_temp(w.temp_min)
        ));
    }
    parts.concat()
}

/// 「{prefix}『{summary}』（{HH:MM}）ほか{N-1}件。」。1 件なら「ほか」なし、終日は時刻省略。
fn calendar_line(prefix: &str, cal: &CalendarSummary) -> String {
    let time_part = cal
        .time_label
        .as_deref()
        .map(|t| format!("（{t}）"))
        .unwrap_or_default();
    if cal.total <= 1 {
        format!("{prefix}『{}』{}。", cal.summary, time_part)
    } else {
        format!("{prefix}『{}』{}ほか{}件。", cal.summary, time_part, cal.total - 1)
    }
}

/// 「未完了のリマインダーは『{text}』ほか{N-1}件。」。1 件なら「ほか」なし。
fn reminder_line(r: &ReminderSummary) -> String {
    if r.total <= 1 {
        format!("未完了のリマインダーは『{}』。", r.text)
    } else {
        format!("未完了のリマインダーは『{}』ほか{}件。", r.text, r.total - 1)
    }
}

/// 気温を四捨五入して整数表記にする (「最高28.4℃」ではなく「最高28℃」と読ませる)。
fn fmt_temp(v: f64) -> String {
    format!("{:.0}", v)
}

// ===== advanced 上乗せ (言い回し整形、§5.4) =====

/// low が組んだ script を advanced (LLM) で会話調に整形する。
/// 材料選定・発話可否は呼び出し側 (build_*_script) で確定済み — ここでは言い回しのみ変える。
/// mode != advanced・降格中・script が空・API エラー・タイムアウトのいずれでも
/// script をそのまま返す (low フォールバック必須)。
pub async fn polish_script(state: &Arc<AppState>, script: &str) -> String {
    if script.is_empty() {
        return String::new();
    }
    let settings = state.settings.lock().expect("settings poisoned").clone();
    if !matches!(settings.mode, DialogueMode::Advanced) {
        return script.to_string();
    }
    // 対話チャットと同じ一時降格状態を尊重する (dialogue::mod の degrade と同じフィールド)。
    let until = state.dialogue.degraded_until.load(Ordering::SeqCst);
    if until != 0 && Utc::now().timestamp() < until {
        return script.to_string();
    }
    let api_key = match secrets::get_api_key(&settings.llm_provider) {
        Ok(k) => k,
        Err(err) => {
            eprintln!("[regular_talk] get_api_key failed: {err:#}");
            return script.to_string();
        }
    };
    let client = LlmClient::new(settings.llm_base_url.clone(), api_key);
    let messages = vec![
        ChatMessage::system(
            "次の一言を、意味と情報量を変えずに自然な会話調へ短く言い換えてください。\
             前置き・後置き・引用符・改行・マークダウンは付けず、言い換えた本文だけを返してください。",
        ),
        ChatMessage::user(script.to_string()),
    ];
    let result = tokio::time::timeout(
        Duration::from_secs(REGULAR_LLM_TIMEOUT_SECS),
        client.chat(&settings.llm_model, messages),
    )
    .await;
    match result {
        Ok(Ok(resp)) => {
            let prompt_tokens = resp.usage.map(|u| u.prompt_tokens).unwrap_or(0);
            let completion_tokens = resp.usage.map(|u| u.completion_tokens).unwrap_or(0);
            let cost = estimate_cost_usd(&settings.llm_model, prompt_tokens, completion_tokens);
            let _ = state.db.append_api_usage(&ApiUsageRow {
                provider: settings.llm_provider.clone(),
                model: settings.llm_model.clone(),
                prompt_tokens: prompt_tokens as i64,
                completion_tokens: completion_tokens as i64,
                cost_usd: cost,
                ts: Utc::now().timestamp(),
            });
            match resp.choices.first().map(|c| c.message.content.trim().to_string()) {
                Some(t) if !t.is_empty() => t,
                _ => script.to_string(),
            }
        }
        Ok(Err(err)) => {
            eprintln!("[regular_talk] advanced 整形 API エラー、low の script を使用: {err:#}");
            script.to_string()
        }
        Err(_) => {
            eprintln!("[regular_talk] advanced 整形がタイムアウト、low の script を使用");
            script.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(summary: &str, time_label: Option<&str>, total: usize) -> CalendarSummary {
        CalendarSummary {
            summary: summary.to_string(),
            time_label: time_label.map(|s| s.to_string()),
            total,
        }
    }

    fn weather(label: &'static str, max: f64, min: f64, prob: u8) -> WeatherSummary {
        WeatherSummary {
            label,
            temp_max: max,
            temp_min: min,
            precip_prob_max: prob,
        }
    }

    fn empty_morning() -> MorningMaterials {
        MorningMaterials {
            calendar: None,
            todo_count: 0,
            reminder: None,
            weather: None,
        }
    }

    fn empty_evening() -> EveningMaterials {
        EveningMaterials {
            done_count: 0,
            open_count: 0,
            calendar: None,
            weather: None,
        }
    }

    #[test]
    fn morning_script_all_empty_is_empty_string() {
        assert_eq!(build_morning_script(&empty_morning()), "");
    }

    #[test]
    fn evening_script_all_empty_is_empty_string() {
        assert_eq!(build_evening_script(&empty_evening()), "");
    }

    #[test]
    fn morning_script_skips_zero_and_none_items() {
        let m = MorningMaterials {
            calendar: None,
            todo_count: 0,
            reminder: None,
            weather: Some(weather("晴れ", 28.4, 18.0, 10)),
        };
        assert_eq!(build_morning_script(&m), "天気は晴れ、最高28℃。降水確率10%。");
    }

    #[test]
    fn morning_script_joins_present_items_in_order() {
        let m = MorningMaterials {
            calendar: Some(cal("会議", Some("10:00"), 3)),
            todo_count: 2,
            reminder: Some(ReminderSummary { text: "薬".to_string(), total: 1 }),
            weather: Some(weather("くもり", 25.0, 15.0, 20)),
        };
        assert_eq!(
            build_morning_script(&m),
            "今日の予定は『会議』（10:00）ほか2件。ToDo は2件。未完了のリマインダーは『薬』。天気はくもり、最高25℃。降水確率20%。"
        );
    }

    #[test]
    fn calendar_line_omits_hoka_for_single_item() {
        let m = MorningMaterials {
            calendar: Some(cal("歯医者", Some("14:00"), 1)),
            ..empty_morning()
        };
        assert_eq!(build_morning_script(&m), "今日の予定は『歯医者』（14:00）。");
    }

    #[test]
    fn calendar_line_adds_hoka_for_multiple_items() {
        let m = MorningMaterials {
            calendar: Some(cal("歯医者", Some("14:00"), 4)),
            ..empty_morning()
        };
        assert_eq!(build_morning_script(&m), "今日の予定は『歯医者』（14:00）ほか3件。");
    }

    #[test]
    fn calendar_line_omits_time_for_all_day() {
        let m = MorningMaterials {
            calendar: Some(cal("旅行", None, 1)),
            ..empty_morning()
        };
        assert_eq!(build_morning_script(&m), "今日の予定は『旅行』。");
    }

    #[test]
    fn reminder_line_hoka_variants() {
        let single = ReminderSummary { text: "薬".to_string(), total: 1 };
        assert_eq!(reminder_line(&single), "未完了のリマインダーは『薬』。");
        let multi = ReminderSummary { text: "薬".to_string(), total: 3 };
        assert_eq!(reminder_line(&multi), "未完了のリマインダーは『薬』ほか2件。");
    }

    #[test]
    fn evening_script_joins_present_items_in_order() {
        let m = EveningMaterials {
            done_count: 3,
            open_count: 1,
            calendar: Some(cal("出張", Some("9:00"), 2)),
            weather: Some(weather("雨", 22.0, 17.6, 80)),
        };
        assert_eq!(
            build_evening_script(&m),
            "今日終わった ToDo は3件。残りは1件。明日の予定は『出張』（9:00）ほか1件。明日は雨、最高22℃／最低18℃。"
        );
    }

    #[test]
    fn evening_script_skips_zero_counts() {
        let m = EveningMaterials {
            done_count: 0,
            open_count: 0,
            calendar: None,
            weather: None,
        };
        assert_eq!(build_evening_script(&m), "");
    }

    // polish_script (advanced 上乗せ) は AppState を要求する I/O 経由の関数で、
    // このコードベースに AppState を単体テストで構築する前例がない (network/DB/keyring
    // に触れる)。ここでは low = 決定論で完結する build_*_script / calendar_line /
    // reminder_line のみを対象にする (完了条件が求めるのもこの範囲)。
}
