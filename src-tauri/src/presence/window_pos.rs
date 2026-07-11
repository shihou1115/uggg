//! ステージのドック (spec §4.1.6)。
//!
//! ウインドウは「モニタ作業領域の全幅 × 高さ 600 (logical) の透明ステージ」として
//! 作業領域下端に固定する。キャラの足元が常にタスクバー上端に乗り、ユーザーが
//! ウインドウ自体を動かす手段は無い (キャラは stage/charpos.ts がステージ内で X 移動)。
//!
//! - 起動時: 保存位置 (`window_pos`) を含むモニタへドック。無ければ主モニタ
//! - 1 秒間隔の監視: モニタ構成・解像度・タスクバー高さの変更を検知して再ドック
//! - ドック位置を `window_pos` に保存 (次回起動のモニタ記憶のためだけに使う)

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Monitor, PhysicalPosition, PhysicalSize};

use crate::state::AppState;

const WINDOW_POS_KEY: &str = "window_pos";
/// ステージの高さ (CSS px)。デフォルトシェルの最大キャラ (384px) を表示スケール上限
/// (scale.ts MAX_SCALE = 2.0 → 768px) で拡大しても頭が切れず、その上のバルーン・入力欄
/// まで収まる高さ。= 384 * 2.0 + 256 (バルーン/入力欄/余白)。作業領域が足りなければ
/// dock_rect が wa.size.height でキャップする (低解像度で物理的に入らない分は不可避)。
/// スケール連動でキャラ頭が切れる回帰 (v0.1.4 監査で検出) を防ぐための値。
const STAGE_HEIGHT_LOGICAL: f64 = 1024.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct StoredPos {
    x: i32,
    y: i32,
}

/// 起動時に呼ぶ: 前回のモニタ (無ければ主モニタ) の作業領域下端へドックする。
pub fn dock(app: &AppHandle, state: &Arc<AppState>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let stored = match state.db.get_setting(WINDOW_POS_KEY) {
        Ok(Some(v)) => serde_json::from_str::<StoredPos>(&v).ok(),
        _ => None,
    };
    let Some(monitor) = pick_monitor(&window, stored) else {
        return;
    };
    apply_dock(&window, &monitor);
    if let Ok(p) = window.outer_position() {
        persist(state, StoredPos { x: p.x, y: p.y });
    }
}

/// 監視タスク: 1 秒ごとに「現在のモニタの期待ドック矩形」と実矩形を比較し、
/// ズレていれば再ドックする (解像度変更・タスクバー高さ変更・モニタ取り外し対応)。
pub fn spawn_dock_keeper(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(window) = app.get_webview_window("main") else {
                continue;
            };
            let Ok(pos) = window.outer_position() else {
                continue;
            };
            let Some(monitor) = pick_monitor(&window, Some(StoredPos { x: pos.x, y: pos.y }))
            else {
                continue;
            };
            let (want_pos, want_size) = dock_rect(&monitor);
            let size_ok = window
                .outer_size()
                .map(|s| s == want_size)
                .unwrap_or(true);
            if pos == want_pos && size_ok {
                continue;
            }
            apply_dock(&window, &monitor);
            if let Ok(p) = window.outer_position() {
                persist(&state, StoredPos { x: p.x, y: p.y });
            }
        }
    });
}

/// 終了時の即時保存 (モニタ記憶)。
pub fn persist_now(app: &AppHandle, state: &Arc<AppState>) {
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(p) = window.outer_position() {
            persist(state, StoredPos { x: p.x, y: p.y });
        }
    }
}

fn persist(state: &Arc<AppState>, pos: StoredPos) {
    if let Ok(json) = serde_json::to_string(&pos) {
        let _ = state.db.set_setting(WINDOW_POS_KEY, &json);
    }
}

/// 保存位置を含むモニタを返す。該当なし・保存なしは主モニタ (それも無ければ None)。
fn pick_monitor(window: &tauri::WebviewWindow, stored: Option<StoredPos>) -> Option<Monitor> {
    if let (Some(pos), Ok(monitors)) = (stored, window.available_monitors()) {
        for m in monitors {
            let mp = m.position();
            let ms = m.size();
            if pos.x >= mp.x
                && pos.x < mp.x + ms.width as i32
                && pos.y >= mp.y
                && pos.y < mp.y + ms.height as i32
            {
                return Some(m);
            }
        }
    }
    window.primary_monitor().ok().flatten()
}

/// モニタの作業領域 (タスクバー除く) から期待ドック矩形を計算する。
/// 幅 = 作業領域全幅、高さ = 600 logical を物理化 (作業領域高さでキャップ)、下端揃え。
fn dock_rect(monitor: &Monitor) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let wa = monitor.work_area();
    let h = ((STAGE_HEIGHT_LOGICAL * monitor.scale_factor()).round() as u32)
        .min(wa.size.height)
        .max(1);
    let pos = PhysicalPosition::new(
        wa.position.x,
        wa.position.y + wa.size.height as i32 - h as i32,
    );
    (pos, PhysicalSize::new(wa.size.width, h))
}

fn apply_dock(window: &tauri::WebviewWindow, monitor: &Monitor) {
    let (pos, size) = dock_rect(monitor);
    let _ = window.set_size(size);
    let _ = window.set_position(pos);
}
