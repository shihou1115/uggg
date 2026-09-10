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

#[derive(Debug)]
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

/// 展開作業用のディレクトリ。`ghosts/` `shells/` の**外**に置く。
/// 中に有効な manifest を持つ一時ディレクトリが入るため、資産一覧の走査対象に
/// 見えてはいけない。
const STAGING_DIR: &str = ".staging";

fn install_one(
    path: &Path,
    overwrite: bool,
    assets_dir: &Path,
) -> Result<InstallOutcome, DndError> {
    let kind = dnd::detect_asset_kind(path)?;
    let peek = dnd::peek_manifest(path, kind)?;
    let subdir = match kind {
        AssetKind::Ghost => "ghosts",
        AssetKind::Shell => "shells",
    };
    let target_dir = assets_dir.join(subdir).join(&peek.id);
    if target_dir.exists() && !overwrite {
        return Ok(InstallOutcome::Conflict {
            id: peek.id,
            name: peek.name,
            kind,
        });
    }

    // **別ディレクトリで展開・検証を終えてから差し替える (v0.5.3)。**
    //
    // v0.5.2 までは上書き承認後に `remove_dir_all(&target_dir)` を先に実行してから
    // 展開していた。そのため展開中に禁止拡張子・zip 破損・容量不足などで失敗すると、
    // **使えていた旧版まで失われた**。悪意ある入力を必要としない、通常の更新操作の
    // 問題である (Codex レビュー 2026-09-06)。
    let staging_root = assets_dir.join(STAGING_DIR);
    // 前回が異常終了して残っていた分を掃除する (DnD は 1 件ずつ順に処理するので競合しない)。
    let _ = std::fs::remove_dir_all(&staging_root);
    let staging = staging_root.join(format!("{subdir}-{}", peek.id));
    std::fs::create_dir_all(&staging).map_err(DndError::from)?;

    let staged = if path.is_dir() {
        dnd::install_folder(path, &staging, kind)
    } else {
        dnd::install_zip(path, &staging, kind)
    }
    .and_then(|()| verify_staged(&staging, kind, &peek.id));

    if let Err(err) = staged {
        // **旧版は無傷のまま**。作業ディレクトリだけ片付けて失敗を返す。
        let _ = std::fs::remove_dir_all(&staging_root);
        return Err(err);
    }

    // 待避先は staging_root の**外**に置く (下の掃除で旧版を巻き添えにしないため)。
    let previous = assets_dir.join(format!(".previous-{subdir}-{}", peek.id));
    let swapped = swap_in(&staging, &target_dir, &previous);
    let _ = std::fs::remove_dir_all(&staging_root);
    swapped?;

    Ok(InstallOutcome::Installed {
        id: peek.id,
        name: peek.name,
        kind,
    })
}

/// 展開結果が「確認したもの」と一致しているか見る。
///
/// 複数の manifest を含む zip では、確認時と展開時で別の manifest を採ると
/// **確認ダイアログに出した id と実際の中身が食い違う**。選択規則は
/// `dnd::pick_manifest_entry` に一本化してあるが、差し替え前にもう一度突き合わせる。
fn verify_staged(staging: &Path, kind: AssetKind, declared_id: &str) -> Result<(), DndError> {
    let staged = dnd::peek_manifest(staging, kind)?;
    if staged.id != declared_id {
        return Err(DndError::IdMismatch(format!(
            "確認時 {declared_id} / 展開結果 {staged}",
            staged = staged.id
        )));
    }
    Ok(())
}

/// 展開済みディレクトリを本番位置へ差し替える。
///
/// 旧版は削除ではなく**待避してから**入れ替え、入れ替えに失敗したら戻す。
/// 待避先を `ghosts/` `shells/` の外に置くのは、有効な manifest を持つ
/// ディレクトリが資産一覧に一瞬でも現れないようにするため。
fn swap_in(staging: &Path, target: &Path, previous: &Path) -> Result<(), DndError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(DndError::from)?;
    }
    if !target.exists() {
        return std::fs::rename(staging, target).map_err(DndError::from);
    }

    let _ = std::fs::remove_dir_all(previous);
    std::fs::rename(target, previous).map_err(DndError::from)?;
    match std::fs::rename(staging, target) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(previous);
            Ok(())
        }
        Err(err) => {
            // 入れ替えに失敗したら旧版を戻す。**戻せなかったときは待避先を消さず**、
            // 場所をログに残す (自動で消すと、まさに守ろうとしたデータを失う)。
            match std::fs::rename(previous, target) {
                Ok(()) => {}
                Err(restore) => crate::ulog!(
                    "[dnd] 差し替えに失敗し、旧版の復元にも失敗しました。旧版は {} に残してあります: {restore}",
                    previous.display()
                ),
            }
            Err(DndError::from(err))
        }
    }
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

#[cfg(test)]
mod install_tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new().suffix(".zip").tempfile().unwrap();
        let mut zw = zip::ZipWriter::new(tmp.reopen().unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
        tmp
    }

    fn ghost_json(id: &str) -> Vec<u8> {
        format!(
            r#"{{"schema_version":1,"id":"{id}","name":"{id} のゴースト","characters":{{"main":{{"name":"m"}}}},"dictionaries":[]}}"#
        )
        .into_bytes()
    }

    /// 既に導入済みの状態を作る。
    fn assets_with_installed(id: &str, marker: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let ghost = dir.path().join("ghosts").join(id);
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(ghost.join("ghost.json"), ghost_json(id)).unwrap();
        std::fs::write(ghost.join("dic.yaml"), marker.as_bytes()).unwrap();
        dir
    }

    fn read(p: std::path::PathBuf) -> String {
        std::fs::read_to_string(p).expect("読めない")
    }

    /// **操作列テスト 1/4「旧版からの更新」** (spec §6.0 項目 9、v0.5.3)。
    ///
    /// 新規導入 → 更新成功 → 更新失敗 → 再更新 を**同じ id へ順に流す**。
    /// 1 回ずつの単機能テストでは「失敗した更新のあと、まだ使えるか」
    /// 「そのあと更新し直せるか」が見えない。v0.5.3 で確定した 14 件は
    /// こういうつなぎ目に集中しており、単機能テストでは 1 件も捕まらなかった。
    #[test]
    fn install_then_update_then_failed_update_leaves_a_usable_ghost() {
        let assets = tempfile::tempdir().unwrap();
        let dir = assets.path().join("ghosts").join("mimi");

        // 1. 新規導入 (まだ何も無いので上書き承認は要らない)
        let v1 = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("dic.yaml", b"V1".as_slice()),
        ]);
        assert!(matches!(
            install_one(v1.path(), false, assets.path()).unwrap(),
            InstallOutcome::Installed { .. }
        ));
        assert_eq!(read(dir.join("dic.yaml")), "V1");

        // 2. 上書き更新 (承認あり)
        let v2 = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("dic.yaml", b"V2".as_slice()),
        ]);
        assert!(matches!(
            install_one(v2.path(), true, assets.path()).unwrap(),
            InstallOutcome::Installed { .. }
        ));
        assert_eq!(read(dir.join("dic.yaml")), "V2");

        // 3. 壊れた更新 (禁止拡張子を含む zip)
        let bad = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("dic.yaml", b"V3".as_slice()),
            ("evil.exe", b"x".as_slice()),
        ]);
        let err = install_one(bad.path(), true, assets.path()).unwrap_err();
        assert!(matches!(err, DndError::ForbiddenFile(_)), "{err}");

        // 4. **直前まで使えていた版がそのまま残っている**
        assert_eq!(read(dir.join("dic.yaml")), "V2", "失敗した更新が旧版を壊した");
        assert!(dir.join("ghost.json").is_file(), "manifest が消えている");
        assert!(!assets.path().join(STAGING_DIR).exists(), "作業用の残骸");
        assert!(
            !assets.path().join(".previous-ghosts-mimi").exists(),
            "待避先の残骸 (戻せているので消えるべき)"
        );

        // 5. そのあと正常な更新をすれば入れ替わる (失敗が経路を詰まらせていない)
        let v4 = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("dic.yaml", b"V4".as_slice()),
        ]);
        assert!(matches!(
            install_one(v4.path(), true, assets.path()).unwrap(),
            InstallOutcome::Installed { .. }
        ));
        assert_eq!(read(dir.join("dic.yaml")), "V4");
    }

    /// **更新に失敗しても旧版が残る (v0.5.3)。**
    ///
    /// v0.5.2 までは上書き承認後に `remove_dir_all` を先に実行してから展開していたため、
    /// 展開中の失敗 (禁止拡張子・zip 破損・容量不足) で**使えていた旧版まで失われた**。
    /// 悪意ある入力を必要としない、通常の更新操作の問題 (Codex レビュー 2026-09-06)。
    #[test]
    fn failed_update_keeps_the_previous_version() {
        let assets = assets_with_installed("mimi", "OLD");
        // ghost では .exe は許可されていないので展開が必ず失敗する。
        let bad = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("evil.exe", b"x".as_slice()),
        ]);

        let err = install_one(bad.path(), true, assets.path()).unwrap_err();
        assert!(
            matches!(err, DndError::ForbiddenFile(_)),
            "想定と違うエラー: {err}"
        );

        // **旧版が無傷であること**が要点。
        let old = assets.path().join("ghosts").join("mimi");
        assert!(old.is_dir(), "旧版のディレクトリごと消えている");
        assert_eq!(
            std::fs::read_to_string(old.join("dic.yaml")).unwrap(),
            "OLD",
            "旧版の中身が失われている"
        );
        // 作業ディレクトリは残さない。
        assert!(!assets.path().join(STAGING_DIR).exists());
    }

    /// 成功したときはちゃんと入れ替わり、待避先も残らない。
    #[test]
    fn successful_update_replaces_and_leaves_no_leftovers() {
        let assets = assets_with_installed("mimi", "OLD");
        let good = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("dic.yaml", b"NEW".as_slice()),
        ]);

        let out = install_one(good.path(), true, assets.path()).unwrap();
        assert!(matches!(out, InstallOutcome::Installed { .. }));

        let dir = assets.path().join("ghosts").join("mimi");
        assert_eq!(std::fs::read_to_string(dir.join("dic.yaml")).unwrap(), "NEW");
        assert!(!assets.path().join(STAGING_DIR).exists());
        assert!(!assets.path().join(".previous-ghosts-mimi").exists());
        // ghosts/ 直下に作業用の残骸を作っていないこと (資産一覧に紛れ込む)。
        let stray: Vec<_> = std::fs::read_dir(assets.path().join("ghosts"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "mimi")
            .collect();
        assert!(stray.is_empty(), "ghosts/ に残骸: {stray:?}");
    }

    /// 上書き承認が無ければ従来どおり Conflict を返し、何も触らない。
    #[test]
    fn conflict_without_overwrite_touches_nothing() {
        let assets = assets_with_installed("mimi", "OLD");
        let good = zip_with(&[
            ("ghost.json", ghost_json("mimi").as_slice()),
            ("dic.yaml", b"NEW".as_slice()),
        ]);
        let out = install_one(good.path(), false, assets.path()).unwrap();
        assert!(matches!(out, InstallOutcome::Conflict { .. }));
        let dir = assets.path().join("ghosts").join("mimi");
        assert_eq!(std::fs::read_to_string(dir.join("dic.yaml")).unwrap(), "OLD");
        assert!(!assets.path().join(STAGING_DIR).exists());
    }

    /// **確認した id と、実際に導入される中身が一致する (v0.5.3)。**
    ///
    /// v0.5.2 までは確認が「最初に一致した manifest」、展開が「最も浅い manifest」で、
    /// 複数 manifest を含む zip で食い違いえた。選択規則を 1 箇所へ寄せて解消した。
    #[test]
    fn deep_manifest_first_does_not_decide_the_id() {
        let assets = tempfile::tempdir().unwrap();
        // **深い方を先に**置く。旧実装はこちらを採用していた。
        let z = zip_with(&[
            ("nested/deep/ghost.json", ghost_json("deep").as_slice()),
            ("ghost.json", ghost_json("shallow").as_slice()),
            ("dic.yaml", b"NEW".as_slice()),
        ]);
        let out = install_one(z.path(), true, assets.path()).unwrap();
        match out {
            InstallOutcome::Installed { id, .. } => assert_eq!(id, "shallow"),
            InstallOutcome::Conflict { .. } => panic!("Conflict が返った"),
        }
        assert!(assets.path().join("ghosts").join("shallow").is_dir());
        assert!(!assets.path().join("ghosts").join("deep").exists());
    }
}
