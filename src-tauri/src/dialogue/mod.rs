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
}

/// バックエンド起点の発話を chat_log に保存しつつフロントへ emit する共通ヘルパ。
/// ランダムトーク・放置反応・ポモドーロ・起動/終了挨拶など、ユーザー入力を伴わない発話で使う。
pub fn persist_and_speak(app: &AppHandle, state: &Arc<AppState>, resp: &DialogueResponse) {
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
        eprintln!("[persist_and_speak] dialogue emit failed: {err}");
    }
}

// ===== オーケストレーション =====
//
// send_user_message から呼ばれる: モード判定・降格チェック・busy ゲート・
// 失敗時 fallback ・ chat_log 永続化を 1 か所に集約する。

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
    let want_advanced = matches!(settings.mode, DialogueMode::Advanced)
        && !is_degraded(&state.dialogue);

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
                eprintln!("[advanced] error_streak={streak}: {err:#}");
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

async fn evaluate_cost_status(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
) {
    let status = match cost::check_status(&state.db, settings.monthly_limit_usd) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[cost] check_status failed: {err:#}");
            return;
        }
    };
    if status.unlimited {
        return;
    }
    if status.exceeded {
        if !state
            .dialogue
            .cost_limited_emitted
            .swap(true, Ordering::SeqCst)
        {
            degrade(&state.dialogue);
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
    } else if status.reached_80
        && !state
            .dialogue
            .cost_warning_80_emitted
            .swap(true, Ordering::SeqCst)
    {
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
    let api_key = secrets::get_api_key(&settings.llm_provider)?;
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
