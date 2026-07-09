//! 初回起動時に取扱説明書 (manual.md) を一度だけ開く。
//!
//! 配布版は `bundle.resources` で resource_dir に `manual.md` として同梱される。
//! dev はリポジトリの `docs/manual.md` を使う。開いたかどうかは app_settings の
//! `manual_shown` キーで管理し、2 回目以降は開かない (既存ユーザーが本機能を含む版へ
//! 更新した直後は 1 度だけ開く)。

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::state::{resolve_assets_dir, AppState};

const MANUAL_SHOWN_KEY: &str = "manual_shown";

/// 初回起動なら manual.md を OS 標準の関連付けで開き、フラグを立てる。
/// フラグ済み、または manual.md が見つからない場合は何もしない。
pub fn open_on_first_run(app: &AppHandle, state: &Arc<AppState>) {
    if matches!(state.db.get_setting(MANUAL_SHOWN_KEY), Ok(Some(v)) if v == "1") {
        return;
    }
    let Some(path) = resolve_manual_path(app) else {
        // 見つからないときはフラグを立てない (将来のビルドで開けるよう温存)。
        return;
    };
    // 既定の関連付けで開く (ダブルクリック相当)。explorer.exe は GUI プロセスなので
    // windows_subsystem = "windows" のリリース版でもコンソールが出ない。
    let _ = std::process::Command::new("explorer.exe").arg(&path).spawn();
    let _ = state.db.set_setting(MANUAL_SHOWN_KEY, "1");
}

/// manual.md の場所を解決する。
/// - 配布版: `resource_dir/manual.md` (bundle.resources で同梱)
/// - dev: `<assets_dir>/docs/manual.md` (assets_dir はリポジトリルート)
fn resolve_manual_path(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("manual.md"));
    }
    if let Ok(base) = resolve_assets_dir(app) {
        candidates.push(base.join("manual.md")); // prod: base=$INSTDIR
        candidates.push(base.join("docs").join("manual.md")); // dev: base=repo root
    }
    candidates.into_iter().find(|p| p.is_file())
}
