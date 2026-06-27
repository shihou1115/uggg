//! バックグラウンドの自発挙動タスク (spec §4.4.3 / §4.4.4)。
//!
//! - ランダムトーク: monologue_interval_min 分ごとに独り言。0 で無効。
//! - 放置監視: 60 秒チェック、最終操作から 30 分で 1 回 idle。
//! - Irodori サイドカーのアイドル監視: 60 秒チェック、最終使用から 5 分で自動 shutdown (M4c Phase E)。
//!
//! いずれも busy / 静音 (quiet) のときは発火を持ち越す (idle 監視除く)。
//! advanced モードの LLM 生成 + キャッシュは M2 のスコープ外として、M3 では low 辞書選択のみ。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tauri::AppHandle;

use crate::dialogue::{self, low};
use crate::ghost::dict::WhenContext;
use crate::presence::{idle, quiet};
use crate::state::AppState;

/// ランダムトークタスク。1 分ごとに「前回発話からの経過 >= 設定間隔」を判定。
pub fn spawn_random_talk(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut last_talk = Utc::now().timestamp();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            let interval_min = {
                let s = state.settings.lock().expect("settings poisoned");
                s.monologue_interval_min
            };
            if interval_min == 0 {
                // 無効: タイマーをリセットしておく (有効化後すぐ撃たないように)
                last_talk = Utc::now().timestamp();
                continue;
            }
            let now = Utc::now().timestamp();
            if now - last_talk < (interval_min as i64) * 60 {
                continue;
            }
            // 静音・busy なら持ち越し (last_talk を進めない)
            if quiet::should_stay_quiet(&state) {
                continue;
            }
            if state.dialogue.busy.try_acquire().is_err() {
                continue;
            }
            // try_acquire の permit はスコープを抜けると解放される。発話処理は速いので保持しない。
            if speak_monologue(&app, &state) {
                last_talk = now;
            }
        }
    });
}

fn speak_monologue(app: &AppHandle, state: &Arc<AppState>) -> bool {
    let resp = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => low::monologue(&b.dictionary, b.sub_available()),
            Err(_) => None,
        }
    };
    match resp {
        Some(resp) => {
            dialogue::persist_and_speak(app, state, &resp);
            true
        }
        None => false,
    }
}

/// 放置監視タスク。60 秒ごとにチェックし、30 分無操作で 1 回だけ idle を発火。
pub fn spawn_idle_watcher(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            let now = Utc::now().timestamp();
            if !idle::idle_due(&state, now) {
                continue;
            }
            // この放置期間で既に撃っていればスキップ
            if state.presence.idle_fired.load(Ordering::SeqCst) {
                continue;
            }
            // 静音・busy なら持ち越し (idle_fired を立てない → 次のチェックで再挑戦)
            if quiet::should_stay_quiet(&state) {
                continue;
            }
            if state.dialogue.busy.try_acquire().is_err() {
                continue;
            }
            if speak_idle(&app, &state) {
                state.presence.idle_fired.store(true, Ordering::SeqCst);
            }
        }
    });
}

fn speak_idle(app: &AppHandle, state: &Arc<AppState>) -> bool {
    let ctx = WhenContext::now();
    let resp = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => low::event(&b.dictionary, "idle", &ctx, b.sub_available()),
            Err(_) => None,
        }
    };
    match resp {
        Some(resp) => {
            dialogue::persist_and_speak(app, state, &resp);
            true
        }
        None => false,
    }
}

/// Irodori サイドカーのアイドル監視 (M4c Phase E, architecture §8.4)。
/// 60 秒ごとに `IrodoriClient` の `last_used` を確認し、5 分以上未使用なら shutdown する。
/// メモリ常駐の Python + torch + モデルが大きいので、放置中は積極的に解放する。
pub fn spawn_irodori_idle_watcher(state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 60;
    const IDLE_THRESHOLD_SECS: i64 = 5 * 60;
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
            let now = Utc::now().timestamp();
            // 失敗は無視 (次のティックで再試行)
            let _ = state
                .tts
                .irodori
                .shutdown_if_idle(now, IDLE_THRESHOLD_SECS)
                .await;
        }
    });
}

/// M5-B: リマインダー watcher (10 秒間隔で `due_reminders(now)` をポーリング → 発火 → 削除)。
/// **静音中も鳴らす特例** (spec §4.5.3): `quiet::should_stay_quiet` を見ない。
pub fn spawn_reminder_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 10;
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
            let now = Utc::now().timestamp();
            let due = match state.db.due_reminders(now) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("[reminder] due_reminders failed: {err:#}");
                    continue;
                }
            };
            for r in due {
                fire_reminder(&app, &state, &r);
                if let Err(err) = state.db.delete_reminder(r.id) {
                    eprintln!("[reminder] delete_reminder({}) failed: {err:#}", r.id);
                }
            }
        }
    });
}

fn fire_reminder(app: &AppHandle, state: &Arc<AppState>, r: &crate::db::ReminderRow) {
    use crate::dialogue::{self, DialogueResponse};
    use crate::ghost::dict::SpeechTurn;
    let body = if r.text.is_empty() {
        "リマインダーの時間だよ".to_string()
    } else {
        format!("リマインダー: {}", r.text)
    };
    let resp = DialogueResponse {
        kind: "system_message",
        mode: "low",
        pattern: 1,
        main: SpeechTurn {
            text: body,
            pose: None,
        },
        sub: None,
    };
    dialogue::persist_and_speak(app, state, &resp);
}

/// M5-C: `topics_enabled` が ON の間、1 時間おきに enabled な interest_topics の RSS を取得して
/// topics_cache に蓄積する。topics_enabled=false の周期は何もしない (キャッシュ取得しない)。
pub fn spawn_topics_watcher(state: Arc<AppState>) {
    const PERIOD_SECS: u64 = 60 * 60;
    const FIRST_DELAY_SECS: u64 = 60;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(FIRST_DELAY_SECS)).await;
        loop {
            let enabled = state.settings.lock().expect("settings poisoned").topics_enabled;
            if enabled {
                if let Err(err) = crate::system::topics::fetch_all_into_cache(&state).await {
                    eprintln!("[topics] fetch failed: {err:#}");
                }
            }
            tokio::time::sleep(Duration::from_secs(PERIOD_SECS)).await;
        }
    });
}

/// M5-D: 起動時に 1 回 + 24 時間ごとに更新フィードを確認する。
/// `update_feed_url` が未設定なら check 内で no-op。
pub fn spawn_update_watcher(app: AppHandle, state: Arc<AppState>) {
    const ONCE_DELAY_SECS: u64 = 30;
    const PERIOD_SECS: u64 = 24 * 60 * 60;
    tauri::async_runtime::spawn(async move {
        // 起動直後は他の boot 処理と被らせないため少し待つ
        tokio::time::sleep(Duration::from_secs(ONCE_DELAY_SECS)).await;
        loop {
            if let Err(err) = crate::system::update::check_update_once(&app, &state).await {
                eprintln!("[update] check failed: {err:#}");
            }
            tokio::time::sleep(Duration::from_secs(PERIOD_SECS)).await;
        }
    });
}

/// Irodori サイドカーのヘルスチェック (M4c Phase G)。
/// 30 秒ごとに `/health` を ping し、3 回連続失敗で `IrodoriUnavailable` を通知 + サイドカー shutdown。
/// 次回 `ensure_sidecar_running` で再起動される。サイドカー未起動なら no-op。
pub fn spawn_irodori_health_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 30;
    const FAIL_THRESHOLD: u32 = 3;
    tauri::async_runtime::spawn(async move {
        let mut fails: u32 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
            if state.tts.irodori.health_ping().await {
                fails = 0;
                continue;
            }
            fails += 1;
            if fails < FAIL_THRESHOLD {
                continue;
            }
            // 3 回連続失敗: shutdown + 通知 (5 分クールダウンで連続発話を防ぐ)。
            let _ = state.tts.irodori.shutdown().await;
            if state.tts.irodori.should_notify_unavailable() {
                crate::system::notify::notify(
                    &app,
                    &state,
                    crate::system::notify::NoticeKind::IrodoriUnavailable {
                        reason: format!("ヘルスチェックが {FAIL_THRESHOLD} 回連続失敗"),
                    },
                )
                .await;
            }
            fails = 0;
        }
    });
}
