//! ウインドウ位置の永続化 (spec §4.1.6)。
//!
//! - 移動を 3 秒デバウンスで保存
//! - 終了時は即時保存
//! - 起動時に復元、モニタ外なら主モニタ中央へフォールバック
//!
//! 位置は app_settings の `window_pos` キーに `{"x":..,"y":..}` JSON で保存。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, PhysicalPosition};

use crate::state::AppState;

const WINDOW_POS_KEY: &str = "window_pos";
const DEBOUNCE_SECS: i64 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct StoredPos {
    x: i32,
    y: i32,
}

/// 起動時に呼ぶ: 保存位置を復元。無ければ何もしない (Tauri 既定位置のまま)。
/// モニタ外ならフォールバックとして主モニタ中央へ寄せる。
pub fn restore(app: &AppHandle, state: &Arc<AppState>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let stored = match state.db.get_setting(WINDOW_POS_KEY) {
        Ok(Some(v)) => serde_json::from_str::<StoredPos>(&v).ok(),
        _ => None,
    };
    let Some(pos) = stored else {
        center_on_primary(&window);
        return;
    };

    if is_fully_visible_on_some_monitor(&window, pos) {
        let _ = window.set_position(PhysicalPosition::new(pos.x, pos.y));
    } else {
        // 保存値が画面外 (モニタ構成変更等) なら DB の値を空にしてフォールバック中央化。
        // 次回起動時に「stored=None」経路に乗って center_on_primary が走る。
        let _ = state.db.set_setting(WINDOW_POS_KEY, "");
        center_on_primary(&window);
    }
}

/// 監視タスクを起動: 1 秒ごとに現在位置を見て、変化があれば pos_dirty_since を更新。
/// デバウンス期間 (3 秒) 変化が無ければ保存する。
pub fn spawn_pos_saver(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<StoredPos> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(window) = app.get_webview_window("main") else {
                continue;
            };
            let Ok(p) = window.outer_position() else {
                continue;
            };
            let cur = StoredPos { x: p.x, y: p.y };

            match last {
                Some(prev) if prev.x == cur.x && prev.y == cur.y => {
                    // 動いていない: デバウンス満了で保存
                    let dirty = state.presence.pos_dirty_since.load(Ordering::SeqCst);
                    if dirty != 0 {
                        let now = chrono::Utc::now().timestamp();
                        if now - dirty >= DEBOUNCE_SECS {
                            persist(&state, cur);
                            state.presence.pos_dirty_since.store(0, Ordering::SeqCst);
                        }
                    }
                }
                _ => {
                    // 動いた: dirty マークを更新
                    state
                        .presence
                        .pos_dirty_since
                        .store(chrono::Utc::now().timestamp(), Ordering::SeqCst);
                    last = Some(cur);
                }
            }
        }
    });
}

/// 終了時の即時保存。
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

/// ウインドウの中心点がいずれかのモニタ内にあるか (= 半分以上が見える)。
/// 「ウインドウ全体が完全内」だと数ピクセルのはみ出しでも off-screen 扱いになるため緩めの判定。
/// 「左上隅のみ」だと逆にウインドウ大半が画面外でも復元してしまう。中心点判定がちょうど良い妥協。
fn is_fully_visible_on_some_monitor(
    window: &tauri::WebviewWindow,
    pos: StoredPos,
) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        return true; // 取得失敗時は復元を試みる
    };
    let Ok(size) = window.outer_size() else {
        return true;
    };
    let cx = pos.x + (size.width as i32) / 2;
    let cy = pos.y + (size.height as i32) / 2;
    for m in monitors {
        let mp = m.position();
        let ms = m.size();
        let (left, top) = (mp.x, mp.y);
        let (right, bottom) = (mp.x + ms.width as i32, mp.y + ms.height as i32);
        if cx >= left && cx < right && cy >= top && cy < bottom {
            return true;
        }
    }
    false
}

fn center_on_primary(window: &tauri::WebviewWindow) {
    let Ok(Some(primary)) = window.primary_monitor() else {
        return;
    };
    let mp = primary.position();
    let ms = primary.size();
    let Ok(win_size) = window.outer_size() else {
        return;
    };
    let x = mp.x + (ms.width as i32 - win_size.width as i32) / 2;
    let y = mp.y + (ms.height as i32 - win_size.height as i32) / 2;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}
