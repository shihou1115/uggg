pub mod advanced;
pub mod banter;
pub mod llm;
pub mod low;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use tauri::AppHandle;

use crate::db::ChatRole;
use crate::ghost::dict::SpeechTurn;
use crate::state::{AppState, DialogueMode};
use crate::system::cost;
use crate::system::notify::{self, DegradeReason, NoticeKind};
use crate::system::secrets;

/// フロントへの発話 1 ターン分。
#[derive(Debug, Clone, Serialize)]
pub struct DialogueResponse {
    /// "reply" (ユーザー入力に対する応答) / "event" (起動挨拶等) / "system_message" (notify 経由)
    pub kind: &'static str,
    /// "low" / "advanced"
    pub mode: &'static str,
    /// 掛け合いパターン 1..=4。M2 初期は常に 1、M2-J で 2-4 拡張。
    pub pattern: u8,
    pub main: SpeechTurn,
    pub sub: Option<SpeechTurn>,
    /// 掛け合いパターン3/4 の3ターン目 (spec §4.2.4)。パターン3は main の再発話、
    /// パターン4は sub の再発話。`#balloon-extra` に独立表示する (spec §4.1.3)。
    /// パターン1/2、または LLM が3ターン目を返さなかった場合は None
    /// (その場合 pattern は 3→1・4→2 に縮退済み、`dialogue::banter::assemble_advanced` 参照)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<SpeechTurn>,
    // === M9: バック起点発話のメタ (🔕 フィードバック用、daily-support-design §4.3/§8.2) ===
    // `system::deliver::deliver_event` だけが付与する。ユーザー起点の応答
    // (`send_user_message` の戻り値) には付けない (None のままシリアライズから消える)。
    /// 発話ごとの一意 id (連番文字列)。フロントは表示中発話の id を保持し、
    /// 🔕 クリック時に `feedback_speech(speech_id, category)` で送り返す (誤適用防止)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_id: Option<String>,
    /// `SpeechCategory::as_str()` の値。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<&'static str>,
    /// "notice" | "ambient"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<&'static str>,
    /// 🔕 を表示してよい発話か (Situation* の Ambient のみ true、§4.3)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_allowed: Option<bool>,
}

/// バックエンド起点の発話を chat_log に保存しつつフロントへ emit する共通ヘルパ。
/// ランダムトーク・放置反応・ポモドーロ・起動/終了挨拶など、ユーザー入力を伴わない発話で使う。
/// 戻り値は発話 (dialogue emit) が成立したか。M7 の通知配達 (`system::deliver`) が
/// トーストフォールバック判定に使う。既存呼び出しは無視してよい。
pub fn persist_and_speak(app: &AppHandle, state: &Arc<AppState>, resp: &DialogueResponse) -> bool {
    use tauri::Emitter;
    let now = Utc::now().timestamp();
    let _ = state
        .db
        .append_chat(now, resp.mode, ChatRole::Main, &resp.main.text, resp.main.pose.as_deref());
    if let Some(sub) = &resp.sub {
        let _ = state
            .db
            .append_chat(now, resp.mode, ChatRole::Sub, &sub.text, sub.pose.as_deref());
    }
    if let Err(err) = app.emit("dialogue", resp) {
        crate::ulog!("[persist_and_speak] dialogue emit failed: {err}");
        return false;
    }
    true
}

// ===== オーケストレーション =====
//
// send_user_message から呼ばれる: モード判定・降格チェック・busy ゲート・
// 失敗時 fallback ・ chat_log 永続化を 1 か所に集約する。

/// M7 (spec §4.6.1): `tools::reminder::parse_reminder` が抽出した予定を DB へ登録し、
/// 確認台詞を main 単独の発話として返す。LLM は呼ばない (常時ローカル、advanced 非依存)。
/// chat_log には user と main を保存する。
fn handle_reminder_request(
    app: &AppHandle,
    state: &Arc<AppState>,
    user_text: &str,
    parsed: &crate::tools::reminder::ParsedReminder,
) -> Result<DialogueResponse, String> {
    // 本文が省略された場合は元の発話をそのまま使う (例「5分後」)
    let default_body = format!("「{user_text}」より");
    crate::tools::reminder::register(state, parsed, &default_body)
        .map_err(|e| format!("リマインダー登録に失敗: {e:#}"))?;
    {
        use tauri::Emitter;
        let _ = app.emit("reminders-changed", ());
    }

    let body = if parsed.body.is_empty() {
        default_body
    } else {
        parsed.body.clone()
    };
    let now = Utc::now().timestamp();
    let now_local = chrono::Local::now().naive_local();
    let confirm_text = format_confirmation(&parsed.schedule, now_local, &body);
    let _ = state.db.append_chat(now, "low", ChatRole::User, user_text, None);
    let _ = state
        .db
        .append_chat(now, "low", ChatRole::Main, &confirm_text, None);
    Ok(DialogueResponse {
        kind: "reply",
        mode: "low",
        pattern: 1,
        main: SpeechTurn {
            text: confirm_text,
            pose: None,
        },
        sub: None,
        extra: None,
        speech_id: None,
        category: None,
        priority: None,
        feedback_allowed: None,
    })
}

fn format_confirmation(
    schedule: &crate::tools::reminder::Schedule,
    now_local: chrono::NaiveDateTime,
    body: &str,
) -> String {
    use crate::tools::reminder::{weekday_mask_names, Schedule};
    let fmt_tod = |tod: i32| format!("{}:{:02}", tod / 3600, (tod % 3600) / 60);
    match schedule {
        Schedule::Offset { secs } => {
            let (n, unit) = if *secs >= 3600 && secs % 3600 == 0 {
                (secs / 3600, "時間")
            } else if *secs >= 60 {
                (secs / 60, "分")
            } else {
                (*secs, "秒")
            };
            format!("{n}{unit}後に「{body}」を覚えておくね")
        }
        Schedule::AtTime { local } => {
            use chrono::{Datelike, Timelike};
            let day = (local.date() - now_local.date()).num_days();
            let day_label = match day {
                0 => "今日".to_string(),
                1 => "明日".to_string(),
                2 => "明後日".to_string(),
                _ => format!("{}月{}日", local.month(), local.day()),
            };
            format!(
                "{day_label}の{}:{:02}に「{body}」を覚えておくね",
                local.hour(),
                local.minute()
            )
        }
        Schedule::Daily { time_of_day } => {
            format!("毎日{}に「{body}」を覚えておくね", fmt_tod(*time_of_day))
        }
        Schedule::Weekly { weekday_mask, time_of_day } => {
            format!(
                "毎週{}曜の{}に「{body}」を覚えておくね",
                weekday_mask_names(*weekday_mask),
                fmt_tod(*time_of_day)
            )
        }
    }
}

/// 連続 API エラーがこの回数に達したら一時降格する。
const ERROR_STREAK_THRESHOLD: i64 = 3;
/// 一時降格の保持時間 (秒)。経過後に再度 advanced を試みる。
const DEGRADE_HOLD_SECS: i64 = 300;

pub async fn handle_user_message(
    app: AppHandle,
    state: &Arc<AppState>,
    user_text: &str,
) -> Result<DialogueResponse, String> {
    let trimmed = user_text.trim();

    // 同時実行を 1 件に絞る (busy ゲート)
    let permit = state
        .dialogue
        .busy
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| format!("busy semaphore: {e}"))?;
    state
        .dialogue
        .last_interaction
        .store(Utc::now().timestamp(), Ordering::SeqCst);
    // ユーザー操作で放置カウンタをリセット。
    crate::presence::idle::reset(state);

    // 降格期限が切れていれば復帰通知をまず出す。
    if recover_if_due(&state.dialogue) {
        notify::notify(&app, state, NoticeKind::ModeRecovered).await;
    }

    let result = run_dispatch(&app, state, trimmed).await;
    drop(permit);
    result
}

async fn run_dispatch(
    app: &AppHandle,
    state: &Arc<AppState>,
    user_text: &str,
) -> Result<DialogueResponse, String> {
    let settings = state.settings.lock().expect("settings poisoned").clone();

    // M7 (spec §4.6.1): daily_support_enabled なら予定表現をパースしてリマインダー登録を
    // 試みる。tools_enabled・advanced から独立した常時ローカル動作 (§4.2.1 不変条件)。
    // LLM は呼ばずに即時返事するので高速・低コスト。
    if settings.daily_support_enabled {
        let now_local = chrono::Local::now().naive_local();
        if let Some(parsed) = crate::tools::reminder::parse_reminder(user_text, now_local) {
            return handle_reminder_request(app, state, user_text, &parsed);
        }
    }

    // 上限超過は LLM を呼ぶ「前」に弾く (spec §4.2.7)。
    // 以前は try_advanced の成功後にしか判定しておらず、超過後も呼び続けていた。
    let over_limit = cost_exceeded(state, &settings);
    if over_limit && !cost::notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED) {
        // **この turn の返答そのものを告知にする。**
        // emit で別発話として流すと、直後に返る low 応答が同じ吹き出しへ描画され
        // (フロントの listen コールバックは並行する)、月 1 回しか出ない告知が
        // 視認前に消えうる。しかも告知済みフラグは立つので二度と出ない。
        // 返答として返せば必ず表示される。
        if let Some(resp) = cost_limit_reply(state) {
            cost::mark_notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED);
            return Ok(resp);
        }
        // 辞書にキーが無いゴースト向けの保険 (既定辞書には v0.5 で追加済み)。
        announce_cost_limit_once(app, state, &settings).await;
    }
    let want_advanced = matches!(settings.mode, DialogueMode::Advanced)
        && !is_degraded(&state.dialogue)
        && !over_limit;

    if want_advanced {
        match try_advanced(state, user_text).await {
            Ok(resp) => {
                state.dialogue.error_streak.store(0, Ordering::SeqCst);
                // 成功直後にコスト判定 (api_usage が増えた直後)。
                evaluate_cost_status(app, state, &settings).await;
                return Ok(resp);
            }
            Err(err) => {
                let streak = state.dialogue.error_streak.fetch_add(1, Ordering::SeqCst) + 1;
                crate::ulog!("[advanced] error_streak={streak}: {err:#}");
                if streak >= ERROR_STREAK_THRESHOLD {
                    degrade(&state.dialogue);
                    notify::notify(
                        app,
                        state,
                        NoticeKind::ModeDegraded {
                            reason: DegradeReason::ApiError,
                        },
                    )
                    .await;
                }
            }
        }
    }
    // low へフォールバック
    fallback_low(state, user_text)
}

/// 当月コストを評価し、80% 警告 / 上限超過 (降格 + 告知) を月内一度きりで出す。
/// M14: advanced 独り言の補充 (`system::monologue`) も**同じ関数**を通す
/// (背景処理だけが上限を素通りする穴を作らない、foundation-design §3.5)。
/// 上限超過の告知を **その月に 1 回だけ** 出す。
///
/// 以前は非永続の `AtomicBool` を使っており、(1) 再起動で消える
/// (2) **月が替わっても戻らない**ため翌月の警告が二度と鳴らない、という 2 つの穴があった。
/// 当月タグを `app_settings` に保存して判定する（spec §4.2.7「次月リセットで復帰。」）。
///
/// **降格タイマーは張らない。** 超過の判定は `cost_exceeded` が毎回 DB を見て行うので、
/// タイマーで解除されると「5 分後に課金が再開する」という以前の穴に戻る。
pub(crate) async fn announce_cost_limit_once(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
) {
    if cost::notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED) {
        return;
    }
    cost::mark_notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED);
    notify::notify(
        app,
        state,
        NoticeKind::CostLimitExceeded {
            provider: settings.llm_provider.clone(),
        },
    )
    .await;
    notify::notify(
        app,
        state,
        NoticeKind::ModeDegraded {
            reason: DegradeReason::CostLimit,
        },
    )
    .await;
}

/// 80% 到達の警告。呼び出し後のコスト記録を見て、その月に 1 回だけ出す。
///
/// 上限超過そのものの判定は `cost_exceeded` ゲートが LLM 呼び出しの前に行うので、
/// ここは 80% 警告と、超過に「今まさに乗った」場合の告知だけを担当する。
pub(crate) async fn evaluate_cost_status(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
) {
    let status = match cost::check_status(&state.db, settings.monthly_limit_usd) {
        Ok(s) => s,
        Err(err) => {
            crate::ulog!("[cost] check_status failed: {err:#}");
            return;
        }
    };
    if status.unlimited {
        return;
    }
    if status.exceeded {
        announce_cost_limit_once(app, state, settings).await;
    } else if status.reached_80 && !cost::notified_this_month(&state.db, cost::KEY_WARNED_80) {
        cost::mark_notified_this_month(&state.db, cost::KEY_WARNED_80);
        notify::notify(
            app,
            state,
            NoticeKind::CostWarning80 {
                provider: settings.llm_provider.clone(),
            },
        )
        .await;
    }
}

/// 月額上限に達しているか。**LLM を呼ぶ前に必ず通す唯一のゲート** (spec §4.2.7)。
///
/// `cost::check_status` は毎回 `api_usage` の当月分を DB から集計するので、
/// プロセスを跨いでも月が替わっても正しい。以前は「超過 → 300 秒の一時降格」
/// だったため、実質「5 分の一時停止」でその月ずっと課金が続いていた。
/// 降格タイマー (`degraded_until`) は **API エラー由来専用**とし、コスト超過は
/// このゲートで毎回判定する。
pub(crate) fn cost_exceeded(state: &Arc<AppState>, settings: &crate::state::Settings) -> bool {
    if settings.monthly_limit_usd <= 0.0 {
        return false;
    }
    match cost::check_status(&state.db, settings.monthly_limit_usd) {
        Ok(st) => st.exceeded,
        Err(err) => {
            // 集計できないときに課金を止めるのは過剰なので通す（記録は残す）。
            crate::ulog!("[cost] check_status failed: {err:#}");
            false
        }
    }
}

/// 上限超過の告知を「この turn の返答」として組み立てる。
///
/// 辞書 `system_messages.cost_limit_exceeded` を引く。キーが無ければ None。
fn cost_limit_reply(state: &Arc<AppState>) -> Option<DialogueResponse> {
    let guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = guard.as_ref().ok()?;
    let ctx = crate::ghost::dict::WhenContext::now();
    let line = bundle
        .dictionary
        .pick_system_message("cost_limit_exceeded", &ctx, bundle.sub_available())?;
    Some(banter::pattern_1("event", "low", line))
}

fn degrade(d: &crate::state::DialogueState) {
    let until = Utc::now().timestamp() + DEGRADE_HOLD_SECS;
    d.degraded_until.store(until, Ordering::SeqCst);
}

fn recover_if_due(d: &crate::state::DialogueState) -> bool {
    let until = d.degraded_until.load(Ordering::SeqCst);
    if until == 0 {
        return false;
    }
    let now = Utc::now().timestamp();
    if now >= until {
        d.degraded_until.store(0, Ordering::SeqCst);
        d.error_streak.store(0, Ordering::SeqCst);
        true
    } else {
        false
    }
}

async fn try_advanced(
    state: &Arc<AppState>,
    user_text: &str,
) -> anyhow::Result<DialogueResponse> {
    let settings = {
        let s = state.settings.lock().expect("settings poisoned");
        s.clone()
    };
    let api_key = secrets::get_api_key_async(&settings.llm_provider).await?;
    // std::sync::MutexGuard を await を跨いで保持できないので、ブロックで握り→外す。
    let bundle = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => b.clone(),
            Err(s) => return Err(anyhow::anyhow!("{s}")),
        }
    };

    let reply = advanced::reply(&settings, &bundle, &state.db, api_key, user_text).await?;
    Ok(reply.response)
}

fn fallback_low(
    state: &Arc<AppState>,
    user_text: &str,
) -> Result<DialogueResponse, String> {
    let bundle_guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = bundle_guard.as_ref().map_err(|s| s.clone())?;
    let sub_available = bundle.sub_available();
    let resp = low::reply(&bundle.dictionary, user_text, sub_available);
    let now = Utc::now().timestamp();
    let _ = state.db.append_chat(now, "low", ChatRole::User, user_text, None);
    let _ = state.db.append_chat(
        now,
        "low",
        ChatRole::Main,
        &resp.main.text,
        resp.main.pose.as_deref(),
    );
    if let Some(sub) = &resp.sub {
        let _ = state.db.append_chat(
            now,
            "low",
            ChatRole::Sub,
            &sub.text,
            sub.pose.as_deref(),
        );
    }
    Ok(resp)
}

fn is_degraded(d: &crate::state::DialogueState) -> bool {
    let until = d.degraded_until.load(Ordering::SeqCst);
    if until == 0 {
        return false;
    }
    let now = Utc::now().timestamp();
    now < until
}
