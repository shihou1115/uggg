//! バックグラウンドの自発挙動タスク (spec §4.4.3 / §4.4.4)。
//!
//! - ランダムトーク: monologue_interval_min 分ごとに独り言。0 で無効。
//! - 放置監視: 60 秒チェック、最終操作から 30 分で 1 回 idle。
//!
//! いずれも busy / 静音 (quiet) のときは発火を持ち越す。
//! advanced モードの LLM 生成 + キャッシュは M2 のスコープ外として、M3 では low 辞書選択のみ。
//! (advanced キャッシュ補充は将来課題。advanced モードでも当面は辞書 monologue を使う。)

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
