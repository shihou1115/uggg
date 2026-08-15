//! ステージのドック (spec §4.1.6)。
//!
//! ウインドウは「モニタ作業領域の全幅 × 高さ 600 (logical) の透明ステージ」として
//! 作業領域下端に固定する。キャラの足元が常にタスクバー上端に乗り、ユーザーが
//! ウインドウ自体を動かす手段は無い (キャラは stage/charpos.ts がステージ内で X 移動)。
//!
//! - 起動時: **明示選択 (`monitor_pref`) があればそのモニタ**、無ければ保存位置
//!   (`window_pos`) を含むモニタへドック。どちらも決まらなければ主モニタ
//! - 1 秒間隔の監視: モニタ構成・解像度・タスクバー高さの変更を検知して再ドック
//! - ドック位置を `window_pos` に保存 (選択が無いときのモニタ記憶に使う)
//!
//! **モニタの決定は `resolve_target_monitor` の 1 箇所に集約する** (M13, spec §4.1.6)。
//! 監視ループは「現在位置からモニタを逆算」するため、選択を別扱いにしないと
//! 毎秒のポーリングがユーザーの選択を上書きしてしまう。

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Monitor, PhysicalPosition, PhysicalSize};

use crate::state::AppState;

const WINDOW_POS_KEY: &str = "window_pos";
/// ユーザーが明示選択した表示モニタ (spec §4.1.6)。`window_pos` (前回位置) とは別物。
const MONITOR_PREF_KEY: &str = "monitor_pref";
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

/// ユーザーが選んだ表示モニタ (spec §4.1.6)。
///
/// **恒久 ID は持たない**。OS が返すモニタ名 (Windows では `\\.\DISPLAY1` 等の GDI
/// デバイス名) は接続順で別の物理モニタに付け替わりうるため、「同じ物理モニタに必ず戻る」
/// ことは保証できない。spec のとおり **「表示構成が選択時と同じなら戻る」**だけを実装し、
/// 構成が変わっていたら主モニタへ退避する (誤ったモニタに固定するより安全)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorPref {
    /// `Monitor::name()`。
    pub name: Option<String>,
    /// 選択時の物理座標。**name と合わせて「構成が同じか」の判定に使う**。
    pub x: i32,
    pub y: i32,
}

impl MonitorPref {
    fn matches(&self, m: &Monitor) -> bool {
        let p = m.position();
        pref_matches(self, m.name().map(|s| s.as_str()), p.x, p.y)
    }
}

/// 選択とモニタの同一性判定 (純関数、テスト対象)。
///
/// **name と position の両方が一致したときだけ同一とみなす** (spec §4.1.6)。
/// name だけで判定すると、同型 2 枚構成でデバイス名が入れ替わったときに
/// 「選んでいない物理モニタ」に固定されてしまう。位置も見ることで、
/// 構成が変わった場合は「解決できない」= 主モニタへ退避 に倒す。
fn pref_matches(pref: &MonitorPref, name: Option<&str>, x: i32, y: i32) -> bool {
    pref.name.as_deref() == name && pref.x == x && pref.y == y
}

pub fn load_pref(state: &Arc<AppState>) -> Option<MonitorPref> {
    match state.db.get_setting(MONITOR_PREF_KEY) {
        Ok(Some(v)) => serde_json::from_str(&v).ok(),
        _ => None,
    }
}

/// 選択を保存する。`None` で選択解除 (自動 = 前回位置に戻す)。
pub fn save_pref(state: &Arc<AppState>, pref: Option<&MonitorPref>) -> Result<(), String> {
    let json = match pref {
        Some(p) => serde_json::to_string(p).map_err(|e| format!("{e}"))?,
        // 空文字は JSON として不正なので load_pref が None を返す (= 選択なし)。
        None => String::new(),
    };
    state
        .db
        .set_setting(MONITOR_PREF_KEY, &json)
        .map_err(|e| format!("モニタ選択の保存に失敗しました: {e}"))
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
    let pref = load_pref(state);
    let Some(monitor) = resolve_target_monitor(&window, pref.as_ref(), stored) else {
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
            // 選択があればそれを最優先で解決する (現在位置からの逆算はしない)。
            let pref = load_pref(&state);
            let Some(monitor) =
                resolve_target_monitor(&window, pref.as_ref(), Some(StoredPos { x: pos.x, y: pos.y }))
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

/// **ステージを置くモニタを決める唯一の決定点** (spec §4.1.6)。
///
/// 優先順位:
/// 1. 明示選択 (`pref`) が解決できればそれ — **ユーザーの選択が常に勝つ**
/// 2. 選択が無いときだけ、保存位置 / 現在位置からの逆算 (`pick_monitor`)
/// 3. どちらも決まらなければ主モニタ
///
/// **選択がある場合は現在位置を一切見ない**。監視ループ (`spawn_dock_keeper`) は毎秒
/// この関数を呼ぶので、ここで位置を見てしまうと選択が上書きされる。
/// 選択が解決できない (そのモニタが今は無い) 場合は主モニタへ退避するが、
/// **`monitor_pref` は消さない**ため、構成が戻れば次の tick で自動的に復帰する。
fn resolve_target_monitor(
    window: &tauri::WebviewWindow,
    pref: Option<&MonitorPref>,
    stored: Option<StoredPos>,
) -> Option<Monitor> {
    if let Some(pref) = pref {
        if let Ok(monitors) = window.available_monitors() {
            if let Some(m) = monitors.into_iter().find(|m| pref.matches(m)) {
                return Some(m);
            }
        }
        // 選択はあるが今の構成では見つからない → 主モニタへ退避 (選択は保持)
        return window.primary_monitor().ok().flatten();
    }
    pick_monitor(window, stored)
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

/// **順序が重要**: 先に移動 (`set_position`) してから寸法 (`set_size`) を決める。
/// 逆順だと、まだ旧 DPI のモニタ上にいるウインドウへ新モニタ基準の物理サイズを与えることに
/// なり、移動に伴う DPI 変更で OS 側に再スケールされうる (spec §4.1.6「DPI が異なる
/// モニタへ移しても正しく表示される」/ foundation-design §2.6)。
fn apply_dock(window: &tauri::WebviewWindow, monitor: &Monitor) {
    let (pos, size) = dock_rect(monitor);
    let _ = window.set_position(pos);
    let _ = window.set_size(size);
}

/// 1 台分のモニタ情報 (設定 UI の選択肢用)。
#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub is_primary: bool,
    /// 今ステージが乗っているモニタか。
    pub is_current: bool,
    /// 現在の選択 (`monitor_pref`) がこのモニタに解決されるか。
    pub is_selected: bool,
}

/// 設定 UI へ返すモニタ一覧 + 選択の状態。
#[derive(Debug, Clone, Serialize)]
pub struct MonitorList {
    pub monitors: Vec<MonitorInfo>,
    /// 選択が保存されているか (false = 自動)。
    pub has_pref: bool,
    /// **選択はあるが、今つながっているモニタの中に見つからない**（主モニタへ退避中）。
    /// UI はこのとき「選択中のモニタが見つかりません」と示す (選択は保持されている)。
    pub pref_unresolved: bool,
}

pub fn list_monitors(app: &AppHandle, state: &Arc<AppState>) -> MonitorList {
    let Some(window) = app.get_webview_window("main") else {
        return MonitorList { monitors: Vec::new(), has_pref: false, pref_unresolved: false };
    };
    let pref = load_pref(state);
    let current = window.outer_position().ok();
    let primary_name = window.primary_monitor().ok().flatten().and_then(|m| m.name().cloned());
    let monitors: Vec<MonitorInfo> = window
        .available_monitors()
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            let is_current = current
                .map(|c| {
                    c.x >= p.x
                        && c.x < p.x + s.width as i32
                        && c.y >= p.y
                        && c.y < p.y + s.height as i32
                })
                .unwrap_or(false);
            MonitorInfo {
                is_primary: m.name().cloned() == primary_name,
                is_current,
                is_selected: pref.as_ref().map(|pf| pf.matches(&m)).unwrap_or(false),
                name: m.name().cloned(),
                x: p.x,
                y: p.y,
                width: s.width,
                height: s.height,
                scale_factor: m.scale_factor(),
            }
        })
        .collect();
    let pref_unresolved = pref.is_some() && !monitors.iter().any(|m| m.is_selected);
    MonitorList { monitors, has_pref: pref.is_some(), pref_unresolved }
}

/// 選択の保存直後に呼ぶ: その場で解決して移動する (次の 1 秒 tick を待たせない)。
pub fn redock_now(app: &AppHandle, state: &Arc<AppState>) {
    dock(app, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pref(name: Option<&str>, x: i32, y: i32) -> MonitorPref {
        MonitorPref { name: name.map(|s| s.to_string()), x, y }
    }

    #[test]
    fn pref_matches_requires_same_name_and_position() {
        let p = pref(Some(r"\\.\DISPLAY2"), 1920, 0);
        assert!(pref_matches(&p, Some(r"\\.\DISPLAY2"), 1920, 0));
    }

    #[test]
    fn pref_does_not_match_when_name_differs() {
        // 同じ位置でも別のデバイス名なら採用しない。
        let p = pref(Some(r"\\.\DISPLAY2"), 1920, 0);
        assert!(!pref_matches(&p, Some(r"\\.\DISPLAY3"), 1920, 0));
    }

    #[test]
    fn pref_does_not_match_when_position_differs() {
        // 同名でも配置が変わっていれば「表示構成が違う」→ 解決せず主モニタへ退避する。
        // (同型 2 枚構成でデバイス名が別の物理モニタに付け替わるため、名前だけでは信用しない)
        let p = pref(Some(r"\\.\DISPLAY2"), 1920, 0);
        assert!(!pref_matches(&p, Some(r"\\.\DISPLAY2"), 3840, 0));
        assert!(!pref_matches(&p, Some(r"\\.\DISPLAY2"), 1920, 1080));
    }

    #[test]
    fn pref_handles_unnamed_monitors() {
        // name が取れない環境同士でも位置で判定できる。
        let p = pref(None, 0, 0);
        assert!(pref_matches(&p, None, 0, 0));
        assert!(!pref_matches(&p, Some(r"\\.\DISPLAY1"), 0, 0));
        assert!(!pref_matches(&p, None, 100, 0));
    }

    #[test]
    fn pref_survives_json_roundtrip() {
        // app_settings への保存形式が壊れないこと (選択は再起動をまたいで保持される)。
        let p = pref(Some(r"\\.\DISPLAY2"), 1920, -180);
        let json = serde_json::to_string(&p).unwrap();
        let back: MonitorPref = serde_json::from_str(&json).unwrap();
        assert!(pref_matches(&back, Some(r"\\.\DISPLAY2"), 1920, -180));
    }

    #[test]
    fn empty_pref_json_is_treated_as_no_selection() {
        // save_pref(None) は空文字を書く。読み戻しでは「選択なし」になる。
        assert!(serde_json::from_str::<MonitorPref>("").is_err());
    }
}
