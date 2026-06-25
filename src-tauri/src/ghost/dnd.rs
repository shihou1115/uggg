//! ゴースト/シェル DnD 展開 (M5-A, spec §4.5.6 / architecture §12)。
//!
//! 受け入れ形式:
//! - **zip ファイル** (拡張子 `.zip`): エントリの先頭階層に `ghost.json` / `shell.json` を含むこと
//! - **フォルダ**: 同上、ただしファイルシステム上の実体ディレクトリ
//!
//! セキュリティ (spec §12.3):
//! - zip slip 対策: 展開先パスが目的ディレクトリ配下にあることを正規化後に検証
//! - サイズ上限: 展開後合計 1GB
//! - 拡張子フィルタ: ghost = `.yaml`/`.json`、shell = `.png`/`.jpg`/`.json` のみ (それ以外は拒否)
//! - 再帰深さ: フォルダ DnD は深さ 10 まで (zip 展開も同等)

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ghost::manifest::{GhostManifest, ShellManifest};

/// 展開後合計サイズの上限 (spec §12.3 = 1GB)。設定可能化は将来課題。
const MAX_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
/// フォルダ DnD の再帰深さ上限。
const MAX_DIR_DEPTH: usize = 10;

/// 受け入れたアセットの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Ghost,
    Shell,
}

/// manifest の peek 結果 (id / name のみ、必要最小限)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestPeek {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Error)]
pub enum DndError {
    #[error("対応していない形式です (zip ファイル または フォルダのみ受け付けます)")]
    UnsupportedFormat,
    #[error("ghost.json / shell.json が見つかりません")]
    NoManifest,
    #[error("manifest の解析に失敗しました: {0}")]
    ManifestParse(String),
    #[error("展開後のサイズが上限を超えています ({0} bytes)")]
    Oversize(u64),
    #[error("不正なパスが含まれています (zip slip 検出): {0}")]
    ZipSlip(String),
    #[error("許可されていないファイル種別: {0}")]
    ForbiddenFile(String),
    #[error("再帰深さ {MAX_DIR_DEPTH} を超えました")]
    TooDeep,
    #[error("I/O エラー: {0}")]
    Io(String),
}

impl From<std::io::Error> for DndError {
    fn from(err: std::io::Error) -> Self {
        DndError::Io(err.to_string())
    }
}

impl From<zip::result::ZipError> for DndError {
    fn from(err: zip::result::ZipError) -> Self {
        DndError::Io(err.to_string())
    }
}

/// 入力パスから種別を検出する。
pub fn detect_asset_kind(path: &Path) -> Result<AssetKind, DndError> {
    if path.is_dir() {
        if path.join("ghost.json").is_file() {
            return Ok(AssetKind::Ghost);
        }
        if path.join("shell.json").is_file() {
            return Ok(AssetKind::Shell);
        }
        // 直下に manifest が無ければ、サブディレクトリ 1 階層下も覗く (zip 同梱フォルダ対策)
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                if p.join("ghost.json").is_file() {
                    return Ok(AssetKind::Ghost);
                }
                if p.join("shell.json").is_file() {
                    return Ok(AssetKind::Shell);
                }
            }
        }
        return Err(DndError::NoManifest);
    }
    if has_zip_ext(path) {
        detect_zip_kind(path)
    } else {
        Err(DndError::UnsupportedFormat)
    }
}

/// zip ファイル内の manifest を peek (種別だけ判定)。
fn detect_zip_kind(zip_path: &Path) -> Result<AssetKind, DndError> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name();
        if name.ends_with("ghost.json") {
            return Ok(AssetKind::Ghost);
        }
        if name.ends_with("shell.json") {
            return Ok(AssetKind::Shell);
        }
    }
    Err(DndError::NoManifest)
}

/// 入力 (zip or フォルダ) の manifest を読み取って id/name を返す。
pub fn peek_manifest(path: &Path, kind: AssetKind) -> Result<ManifestPeek, DndError> {
    let bytes = read_manifest_bytes(path, kind)?;
    parse_manifest(&bytes, kind)
}

fn read_manifest_bytes(path: &Path, kind: AssetKind) -> Result<Vec<u8>, DndError> {
    let target_name = match kind {
        AssetKind::Ghost => "ghost.json",
        AssetKind::Shell => "shell.json",
    };
    if path.is_dir() {
        let direct = path.join(target_name);
        if direct.is_file() {
            return Ok(std::fs::read(direct)?);
        }
        // 1 階層下を探す
        for e in std::fs::read_dir(path)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                let inside = p.join(target_name);
                if inside.is_file() {
                    return Ok(std::fs::read(inside)?);
                }
            }
        }
        return Err(DndError::NoManifest);
    }
    if has_zip_ext(path) {
        let file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            if entry.name().ends_with(target_name) {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                return Ok(buf);
            }
        }
        return Err(DndError::NoManifest);
    }
    Err(DndError::UnsupportedFormat)
}

fn parse_manifest(bytes: &[u8], kind: AssetKind) -> Result<ManifestPeek, DndError> {
    match kind {
        AssetKind::Ghost => {
            let m: GhostManifest = serde_json::from_slice(bytes)
                .map_err(|e| DndError::ManifestParse(format!("ghost.json: {e}")))?;
            Ok(ManifestPeek {
                id: m.id,
                name: m.name,
            })
        }
        AssetKind::Shell => {
            let m: ShellManifest = serde_json::from_slice(bytes)
                .map_err(|e| DndError::ManifestParse(format!("shell.json: {e}")))?;
            Ok(ManifestPeek {
                id: m.id,
                name: m.name,
            })
        }
    }
}

/// zip を target_dir に展開する。zip slip / サイズ上限 / 拡張子フィルタを適用。
/// target_dir は呼び出し側で `ghosts/<id>` または `shells/<id>` を渡す前提。事前に
/// 親ディレクトリは作成されているか、`overwrite=true` で既存ディレクトリは削除済みであること。
pub fn install_zip(zip_path: &Path, target_dir: &Path, kind: AssetKind) -> Result<(), DndError> {
    std::fs::create_dir_all(target_dir)?;
    // canonicalize は Windows で `\\?\` prefix を付け、未作成の candidate との
    // starts_with 判定が誤検知になるので使わない。normalize_path で文字列レベルに揃える。
    let normalized_target = normalize_path(target_dir);
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // 最も浅い階層に置かれた manifest があるかどうかで「ルートを剥がすかどうか」を決める。
    // ルートディレクトリが 1 つだけの zip (例: `ghosts/<id>/ghost.json`) を受け取った場合、
    // 先頭ディレクトリを target_dir 配下に展開し直す。
    let strip_prefix = find_strip_prefix(&mut archive, kind)?;
    let allowed_exts = allowed_extensions(kind);

    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_string();
        let trimmed_name = match &strip_prefix {
            Some(prefix) => match raw_name.strip_prefix(prefix.as_str()) {
                Some(rest) => rest,
                None => continue, // 別ルート配下は無視 (まず無いが念のため)
            },
            None => raw_name.as_str(),
        };
        if trimmed_name.is_empty() {
            continue;
        }
        let trimmed_name = trimmed_name.trim_start_matches('/');
        if trimmed_name.is_empty() {
            continue;
        }

        // zip slip: target_dir の外に出ようとするエントリを拒否
        let candidate = target_dir.join(sanitize_zip_path(trimmed_name)?);
        let candidate_normalized = normalize_path(&candidate);
        if !candidate_normalized.starts_with(&normalized_target) {
            return Err(DndError::ZipSlip(raw_name));
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&candidate)?;
            continue;
        }

        // 拡張子フィルタ
        let ext = candidate
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !allowed_exts.contains(ext.as_str()) {
            return Err(DndError::ForbiddenFile(raw_name));
        }

        // サイズ
        let size = entry.size();
        total = total.saturating_add(size);
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(DndError::Oversize(total));
        }

        if let Some(parent) = candidate.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&candidate)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// フォルダ src を target_dir にコピー。再帰深さ上限と拡張子フィルタを適用。
/// src の直下に manifest がない場合、1 階層下のサブディレクトリ (manifest を含むもの) を root として扱う。
pub fn install_folder(src: &Path, target_dir: &Path, kind: AssetKind) -> Result<(), DndError> {
    std::fs::create_dir_all(target_dir)?;
    let manifest_name = match kind {
        AssetKind::Ghost => "ghost.json",
        AssetKind::Shell => "shell.json",
    };
    let effective_root = if src.join(manifest_name).is_file() {
        src.to_path_buf()
    } else {
        // 1 階層下
        let mut found = None;
        for e in std::fs::read_dir(src)?.flatten() {
            let p = e.path();
            if p.is_dir() && p.join(manifest_name).is_file() {
                found = Some(p);
                break;
            }
        }
        found.ok_or(DndError::NoManifest)?
    };
    let allowed_exts = allowed_extensions(kind);
    let mut total: u64 = 0;
    copy_recursive(
        &effective_root,
        target_dir,
        0,
        &allowed_exts,
        &mut total,
    )
}

fn copy_recursive(
    src: &Path,
    dst: &Path,
    depth: usize,
    allowed_exts: &HashSet<&'static str>,
    total: &mut u64,
) -> Result<(), DndError> {
    if depth > MAX_DIR_DEPTH {
        return Err(DndError::TooDeep);
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let p = e.path();
        let name = e.file_name();
        let dest = dst.join(&name);
        if p.is_dir() {
            copy_recursive(&p, &dest, depth + 1, allowed_exts, total)?;
        } else if p.is_file() {
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if !allowed_exts.contains(ext.as_str()) {
                return Err(DndError::ForbiddenFile(p.display().to_string()));
            }
            let size = std::fs::metadata(&p)?.len();
            *total = total.saturating_add(size);
            if *total > MAX_UNCOMPRESSED_BYTES {
                return Err(DndError::Oversize(*total));
            }
            std::fs::copy(&p, &dest)?;
        }
    }
    Ok(())
}

// ---------- 補助 ----------

fn has_zip_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}

fn allowed_extensions(kind: AssetKind) -> HashSet<&'static str> {
    match kind {
        AssetKind::Ghost => HashSet::from(["yaml", "yml", "json", "md"]),
        AssetKind::Shell => HashSet::from(["png", "jpg", "jpeg", "json"]),
    }
}

/// zip エントリ名を `PathBuf` に変換する際、絶対パスや `..` を含むとエラー。
fn sanitize_zip_path(name: &str) -> Result<PathBuf, DndError> {
    let p = Path::new(name);
    for c in p.components() {
        match c {
            Component::Normal(_) => {}
            Component::CurDir => {}
            _ => return Err(DndError::ZipSlip(name.to_string())),
        }
    }
    Ok(p.to_path_buf())
}

/// `..` を含まないパスとして正規化 (canonicalize は存在前提なので使えない)。
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// archive 内で「最も浅い manifest」の親ディレクトリ名を `strip_prefix` として返す。
/// `<id>/ghost.json` 構造の zip では `<id>/` を剥がして展開できるようにする。
/// manifest が直下 (`ghost.json` のみ) なら `None`。
fn find_strip_prefix<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    kind: AssetKind,
) -> Result<Option<String>, DndError> {
    let target = match kind {
        AssetKind::Ghost => "ghost.json",
        AssetKind::Shell => "shell.json",
    };
    let mut best: Option<(usize, String)> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name();
        if !name.ends_with(target) {
            continue;
        }
        // depth 0 = manifest 直下、より浅いものを優先
        let depth = name.matches('/').count();
        let prefix_end = name.len() - target.len();
        let prefix = name[..prefix_end].to_string();
        match &best {
            Some((d, _)) if *d <= depth => continue,
            _ => best = Some((depth, prefix)),
        }
    }
    Ok(best.and_then(|(_, p)| if p.is_empty() { None } else { Some(p) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_zip(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new()
            .suffix(".zip")
            .tempfile()
            .expect("temp zip");
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

    #[test]
    fn detect_zip_with_ghost_json() {
        let zip = write_temp_zip(&[(
            "myghost/ghost.json",
            br#"{"schema_version":3,"id":"x","name":"X","characters":{"main":{"name":"m"}},"dictionaries":["d.yaml"]}"#,
        )]);
        let kind = detect_asset_kind(zip.path()).unwrap();
        assert_eq!(kind, AssetKind::Ghost);
    }

    #[test]
    fn detect_folder_with_shell_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shell.json"), b"{}").unwrap();
        let kind = detect_asset_kind(dir.path()).unwrap();
        assert_eq!(kind, AssetKind::Shell);
    }

    #[test]
    fn no_manifest_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("random.txt"), b"hi").unwrap();
        let err = detect_asset_kind(dir.path()).unwrap_err();
        assert!(matches!(err, DndError::NoManifest));
    }

    #[test]
    fn reject_path_traversal_in_zip() {
        let zip = write_temp_zip(&[
            ("ghost.json", b"{}"),
            ("../escape.txt", b"x"),
        ]);
        let target = tempfile::tempdir().unwrap();
        let err = install_zip(zip.path(), target.path(), AssetKind::Ghost).unwrap_err();
        assert!(matches!(err, DndError::ZipSlip(_)));
    }

    #[test]
    fn reject_forbidden_extension() {
        let zip = write_temp_zip(&[
            ("ghost.json", b"{}"),
            ("evil.exe", b"x"),
        ]);
        let target = tempfile::tempdir().unwrap();
        let err = install_zip(zip.path(), target.path(), AssetKind::Ghost).unwrap_err();
        assert!(matches!(err, DndError::ForbiddenFile(_)));
    }

    #[test]
    fn allowed_extensions_match_spec() {
        let ghost = allowed_extensions(AssetKind::Ghost);
        assert!(ghost.contains("yaml"));
        assert!(ghost.contains("json"));
        assert!(!ghost.contains("png"));
        let shell = allowed_extensions(AssetKind::Shell);
        assert!(shell.contains("png"));
        assert!(shell.contains("jpg"));
        assert!(shell.contains("json"));
        assert!(!shell.contains("yaml"));
    }

    #[test]
    fn sanitize_rejects_absolute_and_parent() {
        assert!(sanitize_zip_path("/abs/path").is_err());
        assert!(sanitize_zip_path("a/../b").is_err());
        assert!(sanitize_zip_path("a/b/c.txt").is_ok());
    }
}
