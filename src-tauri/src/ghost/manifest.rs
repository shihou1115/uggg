use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::ghost::dict::{self, Dictionary};

#[derive(Debug, Clone, Deserialize)]
pub struct GhostManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub characters: GhostCharacters,
    pub dictionaries: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhostCharacters {
    pub main: GhostCharacter,
    #[serde(default)]
    pub sub: Option<GhostCharacter>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhostCharacter {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShellManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub characters: ShellCharacters,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShellCharacters {
    pub main: ShellCharacterDef,
    #[serde(default)]
    pub sub: Option<ShellCharacterDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShellCharacterDef {
    pub base_size: BaseSize,
    pub default_pose: String,
    pub poses: BTreeMap<String, String>,
    /// 縦の部位しきい値 (C-2: 縦のみ、横は廃止)。未指定なら既定値。
    #[serde(default)]
    pub poke_regions: PokeRegions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BaseSize {
    pub width: u32,
    pub height: u32,
}

/// 縦の部位判定しきい値。`ny < head_max`→head / `< chest_max`→chest / それ以外→body。
/// 横判定 (left_max/right_min) は spec §4.3.2 で廃止。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PokeRegions {
    pub head_max: f64,
    pub chest_max: f64,
}

impl Default for PokeRegions {
    fn default() -> Self {
        // architecture.md §4.3.2 の既定値
        Self {
            head_max: 0.45,
            chest_max: 0.62,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellCharacter {
    pub base_size: BaseSize,
    pub default_pose: String,
    pub poses: BTreeMap<String, String>,
    pub poke_regions: PokeRegions,
}

#[derive(Debug, Clone)]
pub struct GhostBundle {
    pub ghost: GhostManifest,
    pub shell: ShellManifest,
    pub shell_dir: PathBuf,
    pub dictionary: Dictionary,
}

impl GhostBundle {
    /// shell.json に sub 定義があるかどうか。
    /// 辞書側 sub の有効化判定はここで決める。
    pub fn sub_available(&self) -> bool {
        self.shell.characters.sub.is_some()
    }
}

pub fn load_bundle(assets_root: &Path, ghost_id: &str, shell_id: &str) -> Result<GhostBundle> {
    let ghost_dir = assets_root.join("ghosts").join(ghost_id);
    let ghost_json = ghost_dir.join("ghost.json");
    let ghost: GhostManifest = read_json(&ghost_json)
        .with_context(|| format!("ゴースト定義の読み込みに失敗しました: {}", ghost_json.display()))?;
    if ghost.schema_version != 1 {
        return Err(anyhow!(
            "ゴースト定義の schema_version が未対応です（期待: 1, 検出: {}）: {}",
            ghost.schema_version,
            ghost_json.display()
        ));
    }
    if ghost.dictionaries.is_empty() {
        return Err(anyhow!(
            "ゴースト定義に dictionaries が 1 件も指定されていません: {}",
            ghost_json.display()
        ));
    }

    let shell_dir = assets_root.join("shells").join(shell_id);
    let shell_json = shell_dir.join("shell.json");
    let shell: ShellManifest = read_json(&shell_json)
        .with_context(|| format!("シェル定義の読み込みに失敗しました: {}", shell_json.display()))?;
    if shell.schema_version != 1 {
        return Err(anyhow!(
            "シェル定義の schema_version が未対応です（期待: 1, 検出: {}）: {}",
            shell.schema_version,
            shell_json.display()
        ));
    }

    validate_poses(&shell, &shell_dir)?;

    // M1 では辞書は 1 ファイルのみ扱う（架構図に「dictionaries[]」とあるが
    // 複数ファイルの合成は MVP の範囲外。将来必要になったら拡張）。
    if ghost.dictionaries.len() > 1 {
        return Err(anyhow!(
            "現在は dictionaries[] を 1 件のみ対応しています（指定数: {}）",
            ghost.dictionaries.len()
        ));
    }
    let dict_path = ghost_dir.join(&ghost.dictionaries[0]);
    let dictionary = dict::load_dictionary(&dict_path)?;

    Ok(GhostBundle {
        ghost,
        shell,
        shell_dir,
        dictionary,
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("ファイルを開けませんでした: {}", path.display()))?;
    let parsed = serde_json::from_str::<T>(&raw)
        .with_context(|| format!("JSON の構文エラーです: {}", path.display()))?;
    Ok(parsed)
}

fn validate_poses(shell: &ShellManifest, shell_dir: &Path) -> Result<()> {
    check_slot("main", &shell.characters.main, shell_dir)?;
    if let Some(sub) = &shell.characters.sub {
        check_slot("sub", sub, shell_dir)?;
    }
    Ok(())
}

fn check_slot(slot: &str, def: &ShellCharacterDef, shell_dir: &Path) -> Result<()> {
    if !def.poses.contains_key(&def.default_pose) {
        return Err(anyhow!(
            "シェルの {slot} に default_pose '{}' が poses に存在しません",
            def.default_pose
        ));
    }
    for (pose, rel) in &def.poses {
        let abs = shell_dir.join(rel);
        if !abs.is_file() {
            return Err(anyhow!(
                "シェル {slot} の pose '{pose}' の画像が見つかりません: {}",
                abs.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn assets_root() -> &'static Path {
        // テストは src-tauri/ から実行される。リポジトリ直下にある ghosts/ shells/ を参照。
        Path::new("..")
    }

    #[test]
    fn loads_bundled_default() {
        let bundle = load_bundle(assets_root(), "default", "default").expect("default bundle");
        assert_eq!(bundle.ghost.schema_version, 1);
        assert_eq!(bundle.ghost.id, "default");
        assert_eq!(bundle.shell.id, "default");
        assert!(bundle
            .shell
            .characters
            .main
            .poses
            .contains_key(&bundle.shell.characters.main.default_pose));
        // v3 辞書もロードされている
        assert_eq!(bundle.dictionary.schema_version, 3);
        assert!(!bundle.dictionary.input_match.is_empty(), "input_match must exist");
        assert!(bundle.dictionary.events.contains_key("first_boot"));
        assert!(bundle.dictionary.events.contains_key("boot"));
    }

    #[test]
    fn missing_ghost_id_returns_user_friendly_error() {
        let err = load_bundle(assets_root(), "does-not-exist", "default").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ゴースト定義の読み込みに失敗しました"),
            "{msg}"
        );
        assert!(msg.contains("does-not-exist"), "{msg}");
    }

    #[test]
    fn missing_shell_id_returns_user_friendly_error() {
        let err = load_bundle(assets_root(), "default", "does-not-exist").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("シェル定義の読み込みに失敗しました"),
            "{msg}"
        );
        assert!(msg.contains("does-not-exist"), "{msg}");
    }
}

pub fn build_shell_character(def: &ShellCharacterDef, shell_dir: &Path) -> Result<ShellCharacter> {
    let mut poses = BTreeMap::new();
    for (name, rel) in &def.poses {
        let abs = shell_dir.join(rel);
        let bytes = std::fs::read(&abs)
            .with_context(|| format!("pose 画像の読み込みに失敗: {}", abs.display()))?;
        let mime = match abs.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            other => {
                return Err(anyhow!(
                    "未対応の画像形式です（{other}）: {}",
                    abs.display()
                ))
            }
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let data_url = format!("data:{mime};base64,{b64}");
        poses.insert(name.clone(), data_url);
    }
    Ok(ShellCharacter {
        base_size: def.base_size,
        default_pose: def.default_pose.clone(),
        poses,
        poke_regions: def.poke_regions,
    })
}
