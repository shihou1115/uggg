//! ゴースト/シェル切替 UI 用コマンド (M5-F)。
//!
//! `ghosts/<id>/ghost.json` / `shells/<id>/shell.json` を scan して `AssetEntry { id, name }`
//! の配列を返す。manifest がパースできないエントリは skip (壊れたものを UI に出さない)。
//!
//! 切替は `set_settings({ ghost_id, shell_id })` 経由で行い、**再起動が必要** (spec §4.5.6
//! でホットリロード廃止)。フロント側で `commands::lifecycle::quit_app` を呼ぶ動線を用意する。

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::AppHandle;

use crate::ghost::dnd::{self, AssetKind, DndError};
use crate::ghost::manifest::{GhostManifest, ShellManifest};

#[derive(Debug, Clone, Serialize)]
pub struct AssetEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DndInstalled {
    pub id: String,
    pub name: String,
    pub kind: AssetKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct DndConflict {
    pub id: String,
    pub name: String,
    pub kind: AssetKind,
    /// 入力パス (フロントが overwrite=true で再呼び出しするとき同じ値を渡す)。
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DndResult {
    pub installed: Vec<DndInstalled>,
    pub conflicts: Vec<DndConflict>,
    pub errors: Vec<DndItemError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DndItemError {
    pub source: String,
    pub message: String,
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

/// M5-A: ドラッグ&ドロップで受け取ったパス群を ghosts/ shells/ 配下に展開する。
/// `overwrite=false` の場合、既存 id と衝突するエントリは `conflicts` に振り分けて返す。
#[tauri::command]
pub fn dnd_install(
    paths: Vec<String>,
    overwrite: bool,
    app: AppHandle,
) -> Result<DndResult, String> {
    let assets_dir = crate::state::resolve_assets_dir(&app).map_err(|e| format!("{e:#}"))?;
    let mut result = DndResult {
        installed: Vec::new(),
        conflicts: Vec::new(),
        errors: Vec::new(),
    };
    for raw in paths {
        let path = PathBuf::from(&raw);
        match install_one(&path, overwrite, &assets_dir) {
            Ok(InstallOutcome::Installed { id, name, kind }) => {
                result.installed.push(DndInstalled { id, name, kind });
            }
            Ok(InstallOutcome::Conflict { id, name, kind }) => {
                result.conflicts.push(DndConflict {
                    id,
                    name,
                    kind,
                    source: raw,
                });
            }
            Err(err) => {
                result.errors.push(DndItemError {
                    source: raw,
                    message: format!("{err}"),
                });
            }
        }
    }
    Ok(result)
}

enum InstallOutcome {
    Installed {
        id: String,
        name: String,
        kind: AssetKind,
    },
    Conflict {
        id: String,
        name: String,
        kind: AssetKind,
    },
}

fn install_one(
    path: &Path,
    overwrite: bool,
    assets_dir: &Path,
) -> Result<InstallOutcome, DndError> {
    let kind = dnd::detect_asset_kind(path)?;
    let peek = dnd::peek_manifest(path, kind)?;
    let target_dir = match kind {
        AssetKind::Ghost => assets_dir.join("ghosts").join(&peek.id),
        AssetKind::Shell => assets_dir.join("shells").join(&peek.id),
    };
    if target_dir.exists() {
        if !overwrite {
            return Ok(InstallOutcome::Conflict {
                id: peek.id,
                name: peek.name,
                kind,
            });
        }
        std::fs::remove_dir_all(&target_dir).map_err(DndError::from)?;
    }
    if path.is_dir() {
        dnd::install_folder(path, &target_dir, kind)?;
    } else {
        dnd::install_zip(path, &target_dir, kind)?;
    }
    Ok(InstallOutcome::Installed {
        id: peek.id,
        name: peek.name,
        kind,
    })
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
