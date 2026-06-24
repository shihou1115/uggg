//! ゴースト/シェル切替 UI 用コマンド (M5-F)。
//!
//! `ghosts/<id>/ghost.json` / `shells/<id>/shell.json` を scan して `AssetEntry { id, name }`
//! の配列を返す。manifest がパースできないエントリは skip (壊れたものを UI に出さない)。
//!
//! 切替は `set_settings({ ghost_id, shell_id })` 経由で行い、**再起動が必要** (spec §4.5.6
//! でホットリロード廃止)。フロント側で `commands::lifecycle::quit_app` を呼ぶ動線を用意する。

use serde::Serialize;
use tauri::AppHandle;

use crate::ghost::manifest::{GhostManifest, ShellManifest};

#[derive(Debug, Clone, Serialize)]
pub struct AssetEntry {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub fn list_ghosts(app: AppHandle) -> Result<Vec<AssetEntry>, String> {
    let assets_dir = crate::state::resolve_assets_dir(&app).map_err(|e| format!("{e:#}"))?;
    let ghosts_dir = assets_dir.join("ghosts");
    let mut out = scan_dir(&ghosts_dir, "ghost.json", |bytes| {
        let m: GhostManifest = serde_json::from_slice(bytes).ok()?;
        Some(AssetEntry {
            id: m.id,
            name: m.name,
        })
    });
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[tauri::command]
pub fn list_shells(app: AppHandle) -> Result<Vec<AssetEntry>, String> {
    let assets_dir = crate::state::resolve_assets_dir(&app).map_err(|e| format!("{e:#}"))?;
    let shells_dir = assets_dir.join("shells");
    let mut out = scan_dir(&shells_dir, "shell.json", |bytes| {
        let m: ShellManifest = serde_json::from_slice(bytes).ok()?;
        Some(AssetEntry {
            id: m.id,
            name: m.name,
        })
    });
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// `dir` の直下サブディレクトリそれぞれの `manifest_name` を読み込み、`parse` を通せたエントリを集める。
fn scan_dir<F>(dir: &std::path::Path, manifest_name: &str, parse: F) -> Vec<AssetEntry>
where
    F: Fn(&[u8]) -> Option<AssetEntry>,
{
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let manifest = p.join(manifest_name);
        let Ok(bytes) = std::fs::read(&manifest) else {
            continue;
        };
        if let Some(entry) = parse(&bytes) {
            out.push(entry);
        }
    }
    out
}
