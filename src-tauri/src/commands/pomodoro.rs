//! ポモドーロ (spec §4.4.5)。
//! 集中(work) → 休憩(break) を rounds 回繰り返す状態機械。
//! 毎秒 `pomodoro` イベントを emit、節目で events.focus_start / focus_end / break_end / pomodoro_done を再生。
//! 静音中も再生 (ユーザーが始めたタイマーなので)。
//!
//! 世代カウンタ (`PomodoroState::gen`) で stop / 新規 start による旧タスクを失効させる。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::dialogue::{self, low};
use crate::ghost::dict::WhenContext;
use crate::state::AppState;

const PHASE_IDLE: u32 = 0;
const PHASE_FOCUS: u32 = 1;
const PHASE_BREAK: u32 = 2;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct PomodoroStatus {
    /// "idle" / "focus" / "break"
    pub phase: &'static str,
    pub remaining_sec: u32,
    pub round: u32,
    pub rounds: u32,
}

fn phase_name(phase: u32) -> &'static str {
    match phase {
        PHASE_FOCUS => "focus",
        PHASE_BREAK => "break",
        _ => "idle",
    }
}

fn snapshot(state: &Arc<AppState>) -> PomodoroStatus {
    PomodoroStatus {
        phase: phase_name(state.pomodoro.phase.load(Ordering::SeqCst)),
        remaining_sec: state.pomodoro.remaining.load(Ordering::SeqCst),
        round: state.pomodoro.round.load(Ordering::SeqCst),
        rounds: state.pomodoro.rounds.load(Ordering::SeqCst),
    }
}

#[tauri::command]
pub fn get_pomodoro_status(state: State<'_, Arc<AppState>>) -> PomodoroStatus {
    snapshot(&state)
}

#[tauri::command]
pub fn stop_pomodoro(app: AppHandle, state: State<'_, Arc<AppState>>) {
    stop_internal(&app, state.inner());
}

fn stop_internal(app: &AppHandle, state: &Arc<AppState>) {
    state.pomodoro.gen.fetch_add(1, Ordering::SeqCst);
    state.pomodoro.focus.store(false, Ordering::SeqCst);
    state.pomodoro.phase.store(PHASE_IDLE, Ordering::SeqCst);
    state.pomodoro.remaining.store(0, Ordering::SeqCst);
    state.pomodoro.round.store(0, Ordering::SeqCst);
    state.pomodoro.rounds.store(0, Ordering::SeqCst);
    let _ = app.emit("pomodoro", snapshot(state));
}

#[tauri::command]
pub fn start_pomodoro(app: AppHandle, state: State<'_, Arc<AppState>>) {
    let inner = state.inner().clone();
    let (work_secs, break_secs, total_rounds) = {
        let s = inner.settings.lock().expect("settings poisoned");
        (
            (s.pomodoro_work_min as u32).saturating_mul(60),
            (s.pomodoro_break_min as u32).saturating_mul(60),
            s.pomodoro_rounds,
        )
    };
    let gen = inner.pomodoro.gen.fetch_add(1, Ordering::SeqCst) + 1;
    inner.pomodoro.rounds.store(total_rounds, Ordering::SeqCst);
    inner.pomodoro.round.store(0, Ordering::SeqCst);
    inner.pomodoro.phase.store(PHASE_IDLE, Ordering::SeqCst);
    inner.pomodoro.remaining.store(0, Ordering::SeqCst);

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        run_loop(app_clone, inner, gen, work_secs, break_secs, total_rounds).await;
    });
}

async fn run_loop(
    app: AppHandle,
    state: Arc<AppState>,
    gen: u64,
    work_secs: u32,
    break_secs: u32,
    total_rounds: u32,
) {
    for round in 1..=total_rounds {
        if gen != state.pomodoro.gen.load(Ordering::SeqCst) {
            return;
        }
        state.pomodoro.round.store(round, Ordering::SeqCst);

        // focus フェーズ
        state.pomodoro.phase.store(PHASE_FOCUS, Ordering::SeqCst);
        state.pomodoro.focus.store(true, Ordering::SeqCst);
        speak_event(&app, &state, "focus_start");
        if !run_phase(&app, &state, gen, work_secs).await {
            return;
        }
        state.pomodoro.focus.store(false, Ordering::SeqCst);
        if round == total_rounds {
            // 最終ラウンドの集中終了 = pomodoro_done のみ (break は無し)
            speak_event(&app, &state, "pomodoro_done");
            break;
        }
        speak_event(&app, &state, "focus_end");

        // break フェーズ
        state.pomodoro.phase.store(PHASE_BREAK, Ordering::SeqCst);
        if !run_phase(&app, &state, gen, break_secs).await {
            return;
        }
        speak_event(&app, &state, "break_end");
    }

    // 完了: idle に戻す
    if gen == state.pomodoro.gen.load(Ordering::SeqCst) {
        stop_internal(&app, &state);
    }
}

/// 残り秒をカウントダウン。世代不一致で即抜け。
/// 戻り値 false なら中断 (新規 start / stop で失効)。
async fn run_phase(
    app: &AppHandle,
    state: &Arc<AppState>,
    gen: u64,
    secs: u32,
) -> bool {
    state.pomodoro.remaining.store(secs, Ordering::SeqCst);
    let _ = app.emit("pomodoro", snapshot(state));
    for _ in 0..secs {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if gen != state.pomodoro.gen.load(Ordering::SeqCst) {
            return false;
        }
        let prev = state.pomodoro.remaining.fetch_sub(1, Ordering::SeqCst);
        // 念のため0未満になっていたらリセット (saturating_sub 相当)
        if prev == 0 {
            state.pomodoro.remaining.store(0, Ordering::SeqCst);
        }
        let _ = app.emit("pomodoro", snapshot(state));
    }
    true
}

/// 辞書 events から発話 (静音は無視: ユーザーが始めたタイマー)。
fn speak_event(app: &AppHandle, state: &Arc<AppState>, key: &str) {
    let ctx = WhenContext::now();
    let resp = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => low::event(&b.dictionary, key, &ctx, b.sub_available()),
            Err(_) => None,
        }
    };
    if let Some(resp) = resp {
        dialogue::persist_and_speak(app, state, &resp);
    }
}
