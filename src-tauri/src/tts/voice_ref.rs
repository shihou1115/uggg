//! Irodori 参照音声 (.wav) のファイル管理 (architecture §8.7)。
//!
//! 配置は `%APPDATA%\ugg\irodori\refs\<slot>_<id>.wav`。DB 側 (`voice_refs` テーブル) の
//! `file_path` カラムに絶対パスを保存し、本モジュールはディレクトリ作成とパス組み立て、
//! ファイル削除のみを担う。HTTP I/O は [`crate::tts::irodori`] 側。

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// `%APPDATA%\ugg\irodori\` (irodori 資産ディレクトリ全体)。
/// Python サイドカー / モデル / 参照音声がここに集約される (architecture §8.1)。
/// Phase D 以降で voice_ref 生成コマンドから呼ばれる。
#[allow(dead_code)]
pub fn irodori_root() -> Result<PathBuf> {
    Ok(crate::state::resolve_app_data_dir()?.join("irodori"))
}

/// 参照音声配置ディレクトリ。存在しなければ作成する。
#[allow(dead_code)]
pub fn refs_dir() -> Result<PathBuf> {
    let dir = irodori_root()?.join("refs");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create refs dir: {}", dir.display()))?;
    Ok(dir)
}

/// `<refs>/<slot>_<id>.wav` を返す (ファイル作成はしない)。
/// 既定ディレクトリ (`%APPDATA%\ugg\irodori\refs\`) は作成される。slot は "main" / "sub" を想定。
#[allow(dead_code)]
pub fn ref_path_for(slot: &str, id: i64) -> Result<PathBuf> {
    let dir = refs_dir()?;
    ref_path_in_dir(&dir, slot, id)
}

/// `ref_path_for` の純粋部分 (ディレクトリ指定 + slot バリデーション + ファイル名組立)。
/// 環境変数 (`APPDATA`) に依存せずテスト可能。
#[allow(dead_code)]
pub fn ref_path_in_dir(dir: &Path, slot: &str, id: i64) -> Result<PathBuf> {
    if slot.is_empty() || slot.contains(['/', '\\', '\0', '.']) {
        return Err(anyhow!("slot に不正な文字が含まれています: {slot}"));
    }
    Ok(dir.join(format!("{slot}_{id}.wav")))
}

/// 既存参照音声ファイルを削除する。存在しない場合は Ok を返す。
pub fn delete_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(path)
        .with_context(|| format!("delete voice ref file: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ref_path_format_is_slot_id_wav() {
        let dir = PathBuf::from("C:/refs");
        let p = ref_path_in_dir(&dir, "main", 42).expect("path");
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("main_42.wav"));

        let p2 = ref_path_in_dir(&dir, "sub", 7).expect("path");
        assert_eq!(p2.file_name().and_then(|n| n.to_str()), Some("sub_7.wav"));
    }

    #[test]
    fn ref_path_rejects_traversal() {
        let dir = PathBuf::from("C:/refs");
        assert!(ref_path_in_dir(&dir, "../evil", 1).is_err());
        assert!(ref_path_in_dir(&dir, "main/sub", 1).is_err());
        assert!(ref_path_in_dir(&dir, "main\\sub", 1).is_err());
        assert!(ref_path_in_dir(&dir, "", 1).is_err());
        assert!(ref_path_in_dir(&dir, "main.dot", 1).is_err()); // 拡張子混入も拒否
    }
}
