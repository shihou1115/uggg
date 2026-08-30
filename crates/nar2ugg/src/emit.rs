//! `Plan` + 画像 → `shell.json` と PNG 群。ugg 側の検証を書き出す前に再現する。
//!
//! ## 出力に置いてよい拡張子は png と json だけ
//! ugg の DnD (`src-tauri/src/ghost/dnd.rs`) は `allowed_extensions(Shell)` を
//! `{png, jpg, jpeg, json}` に限定しており、許可外の拡張子を見つけると
//! **skip ではなく `ForbiddenFile` を返してインストール全体を失敗させる**
//! (zip 経路・ディレクトリ経路の両方)。readme.txt を 1 個混ぜるだけで、
//! 変換物が ugg で一切受け付けられなくなる。
//!
//! したがってライセンス表示や変換ログは**出力ディレクトリに置かず stderr へ出す**。
//!
//! ## 出力 PNG のファイル名は surface ID から作る
//! pose 名からではない。pose 名は alias 由来で日本語や記号を含みうるので、
//! ファイル名に使うとサニタイズと衝突回避が必要になる。surface ID なら
//! `main/s0.png` のように常に安全。`check_slot` は poses の値が指すファイルの
//! 実在しか見ないので、失われるものは無い。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::imaging::Rgba;
use crate::plan::Plan;
use crate::report::Report;

/// ugg の DnD が受け付ける拡張子のうち、このツールが出力するもの。
///
/// jpg/jpeg も ugg 側は許可しているが、変換結果は常に RGBA PNG なので出さない。
pub const ALLOWED_OUTPUT_EXTS: &[&str] = &["png", "json"];

/// 書き出す前の完成形。キーは出力ルートからの相対パス。
#[derive(Debug, Default)]
pub struct OutputBundle {
    pub files: BTreeMap<String, Vec<u8>>,
}

// ---- shell.json のミラー ------------------------------------------------
// ugg の src-tauri/src/ghost/manifest.rs の serde 定義を写したもの。
// workspace 外の独立 crate なので型は共有せず手で写す。ズレは
// manifest_matches_ugg_schema テストで検出する。

#[derive(Debug, Serialize)]
pub struct ShellManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub characters: ShellCharacters,
}

#[derive(Debug, Serialize)]
pub struct ShellCharacters {
    pub main: ShellCharacterDef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub: Option<ShellCharacterDef>,
}

#[derive(Debug, Serialize)]
pub struct ShellCharacterDef {
    pub base_size: BaseSize,
    pub default_pose: String,
    /// BTreeMap なので出力順が安定する。
    pub poses: BTreeMap<String, String>,
    /// 導出できないときは省略する。ugg 側の既定 (0.45 / 0.62) が適用される。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poke_regions: Option<PokeRegions>,
}

#[derive(Debug, Serialize, Clone, Copy)]
pub struct BaseSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Clone, Copy)]
pub struct PokeRegions {
    pub head_max: f64,
    pub chest_max: f64,
}

// -------------------------------------------------------------------------

/// `Plan` と配置済み画像から出力一式を組み立てる。ディスクには触らない。
///
/// **返す前に `check_bundle` で検証する。** manifest と bundle の両方を持つのは
/// この関数だけなので、検証の責任もここにある。
pub fn build(
    plan: &Plan,
    images: BTreeMap<(crate::report::Slot, String), Rgba>,
    report: &mut Report,
) -> Result<OutputBundle> {
    let _ = (plan, images, report);
    todo!("emit::build")
}

/// ugg 側の検証を、**ディスクではなくこれから書く内容に対して**再現する。
///
/// - `check_slot` (manifest.rs): `default_pose` が `poses` のキーに存在し、
///   `poses` の値が指すファイルが bundle に存在すること
/// - `allowed_extensions` (dnd.rs): 許可外の拡張子を 1 つも含まないこと
///
/// 書き出した後に検証すると、失敗時に「既に書いたファイルの後始末」という
/// 宿題が生まれる。書く前に検証すれば、失敗しても何も残らない。
pub fn check_bundle(manifest: &ShellManifest, bundle: &OutputBundle) -> Result<()> {
    let _ = (manifest, bundle);
    todo!("emit::check_bundle")
}

/// 拡張子ホワイトリストの検査。`check_bundle` の一部だが、単体で検証できるよう
/// 切り出してある (この不変条件を破ると変換物が丸ごと受け付けられなくなる)。
pub fn check_extensions(bundle: &OutputBundle) -> Result<()> {
    for name in bundle.files.keys() {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        if !ALLOWED_OUTPUT_EXTS.contains(&ext.as_str()) {
            anyhow::bail!(
                "出力に含められない拡張子です: {name} \
                 (ugg の DnD は shell に {ALLOWED_OUTPUT_EXTS:?} のみ許可し、\
                 許可外が 1 つでもあるとインストール全体が失敗する)"
            );
        }
    }
    Ok(())
}

/// ディスクへ書き出す。`force` が偽で出力先が空でなければ拒否する。
pub fn write(bundle: &OutputBundle, out_dir: &Path, force: bool) -> Result<()> {
    let _ = (bundle, out_dir, force);
    todo!("emit::write")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_with(names: &[&str]) -> OutputBundle {
        OutputBundle {
            files: names.iter().map(|n| (n.to_string(), Vec::new())).collect(),
        }
    }

    #[test]
    fn allows_png_and_json() {
        let b = bundle_with(&["shell.json", "main/s0.png", "sub/s10.png"]);
        assert!(check_extensions(&b).is_ok());
    }

    #[test]
    fn rejects_anything_ugg_would_refuse() {
        // ugg の dnd.rs は許可外拡張子を skip せず ForbiddenFile で全体を失敗させる。
        for bad in ["readme.txt", "LICENSE", "thumbnail.bmp", "notes.md", "surface0.pna"] {
            let b = bundle_with(&["shell.json", "main/s0.png", bad]);
            assert!(check_extensions(&b).is_err(), "{bad} が許可されてしまった");
        }
    }

    #[test]
    fn extension_check_is_case_insensitive() {
        let b = bundle_with(&["shell.json", "main/S0.PNG"]);
        assert!(check_extensions(&b).is_ok());
    }

    #[test]
    fn manifest_serializes_to_ugg_schema() {
        // ugg の shells/default/shell.json と同じ形になることを確認する。
        let manifest = ShellManifest {
            schema_version: 1,
            id: "sample".into(),
            name: "サンプル".into(),
            author: Some("作者".into()),
            characters: ShellCharacters {
                main: ShellCharacterDef {
                    base_size: BaseSize { width: 256, height: 384 },
                    default_pose: "normal".into(),
                    poses: [("normal".to_string(), "main/s0.png".to_string())].into(),
                    poke_regions: None,
                },
                sub: None,
            },
        };
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["characters"]["main"]["default_pose"], "normal");
        assert_eq!(json["characters"]["main"]["poses"]["normal"], "main/s0.png");
        assert_eq!(json["characters"]["main"]["base_size"]["width"], 256);
        // 相方なし・poke_regions 導出不可のときはキーごと消える
        // (ugg 側は Option / serde(default) で受けるので、null ではなく不在が正)。
        assert!(json["characters"].get("sub").is_none());
        assert!(json["characters"]["main"].get("poke_regions").is_none());
    }
}
