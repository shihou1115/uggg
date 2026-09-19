//! Irodori 参照音声 (.wav) のファイル管理 (architecture §8.7)。
//!
//! 配置は `%APPDATA%\ugg\irodori\refs\<slot>_<id>.wav`。その隣にサイドカーが事前変換の結果
//! (`<slot>_<id>.<モデル>.<精度>.<前処理>.latent.pt`) を置く（v0.5.6 項目 1）。消すのは
//! [`delete_file`] で、参照 wav と一緒に消える。DB 側 (`voice_refs` テーブル) の
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

/// 参照音声の事前変換の結果の拡張子（`sidecar.py` の `REF_LATENT_SUFFIX` と揃える）。
const REF_LATENT_SUFFIX: &str = ".latent.pt";

/// 既存参照音声ファイルを削除する。存在しない場合は Ok を返す。
///
/// **事前変換の結果（`<参照 wav の stem>.*.latent.pt`）も一緒に消す**（spec §6.0 v0.5.6 項目 1）。
/// サイドカーが参照 wav の隣に置くもので、名前の形は `sidecar.py` の `ref_latent_path` と揃える。
/// 残すと、参照音声を消しても変換結果だけが溜まり続ける。
pub fn delete_file(path: &Path) -> Result<()> {
    delete_ref_latents(path);
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(path)
        .with_context(|| format!("delete voice ref file: {}", path.display()))
}

/// 参照 wav の隣にある事前変換の結果（書きかけの `.tmp` を含む）を消す。消せなくても
/// 参照音声の削除は止めない（変換結果は次の合成で作り直せる）ので、理由をログに残すだけ。
fn delete_ref_latents(ref_wav: &Path) {
    let (Some(dir), Some(stem)) = (
        ref_wav.parent(),
        ref_wav.file_stem().and_then(|s| s.to_str()),
    ) else {
        return;
    };
    // `main_1.` で絞る（`main_10.…` を巻き込まない）。
    let prefix = format!("{stem}.");
    let tmp_suffix = format!("{REF_LATENT_SUFFIX}.tmp");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(&prefix)
            && (name.ends_with(REF_LATENT_SUFFIX) || name.ends_with(&tmp_suffix))
        {
            if let Err(err) = std::fs::remove_file(entry.path()) {
                crate::ulog!(
                    "[voice_ref] 事前変換の結果を消せません: {} ({err})",
                    entry.path().display()
                );
            }
        }
    }
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

    /// 参照音声を消すと、その事前変換の結果も消える。**他の参照音声の結果は巻き込まない**
    /// （`main_1` を消しても `main_10` は残す）。
    #[test]
    fn deleting_a_voice_ref_also_deletes_its_latents_only() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let touch = |name: &str| std::fs::write(d.join(name), b"x").unwrap();
        touch("main_1.wav");
        touch("main_1.Aratako__Irodori-TTS-500M-v3.fp32.n-16_e1_s30.latent.pt");
        touch("main_1.Other@abc.bf16.n-16_e1_s30.latent.pt.tmp");
        touch("main_10.wav");
        touch("main_10.Aratako__Irodori-TTS-500M-v3.fp32.n-16_e1_s30.latent.pt");
        touch("sub_1.Aratako__Irodori-TTS-500M-v3.fp32.n-16_e1_s30.latent.pt");
        touch("main_1.notes.txt");

        delete_file(&d.join("main_1.wav")).unwrap();

        let mut left: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "main_1.notes.txt",
                "main_10.Aratako__Irodori-TTS-500M-v3.fp32.n-16_e1_s30.latent.pt",
                "main_10.wav",
                "sub_1.Aratako__Irodori-TTS-500M-v3.fp32.n-16_e1_s30.latent.pt",
            ]
        );
    }

    /// 参照 wav がもう無くても、残っている変換結果は消す（途中で落ちた後の片付け）。
    #[test]
    fn latents_are_deleted_even_when_the_wav_is_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let latent = dir.path().join("sub_7.m.fp32.n-16_e1_s30.latent.pt");
        std::fs::write(&latent, b"x").unwrap();
        delete_file(&dir.path().join("sub_7.wav")).unwrap();
        assert!(!latent.exists());
    }

    /// **名前の形が `sidecar.py` と食い違わないこと**（正本が 2 つある形は、噛み合わせを見張る）。
    /// サイドカーが作る名前は `<参照 wav の stem>.<モデル>.<精度>.<前処理>.latent.pt`。
    /// どちらかだけ変えると、参照音声を消しても変換結果が残り続ける。
    #[test]
    fn the_latent_name_matches_the_sidecar() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("python")
            .join("sidecar.py");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("sidecar.py を読めない {}: {e}", path.display()));
        assert!(
            src.contains(&format!("REF_LATENT_SUFFIX = \"{REF_LATENT_SUFFIX}\"")),
            "sidecar.py の拡張子が Rust と違う"
        );
        assert!(
            src.contains("f\"{ref_wav.stem}.{model_key}.{precision}.{prep}{REF_LATENT_SUFFIX}\""),
            "sidecar.py の名前の形（stem で始まる）が Rust の消し方と合わない"
        );
        // 消す側は参照 wav と同じディレクトリしか見ない（`ref_wav.parent()`）。
        assert!(
            src.contains("return ref_wav.with_name("),
            "sidecar.py が変換結果を参照 wav の隣に置いていない（Rust は隣しか消さない）"
        );
        // 消す側は書きかけを `.latent.pt.tmp` として探す。
        assert!(
            src.contains("tmp = path.with_name(path.name + \".tmp\")"),
            "sidecar.py の書きかけの名前が Rust の消し方（.latent.pt.tmp）と合わない"
        );
    }
}
