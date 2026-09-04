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
    /// advanced (LLM) 経路のプロンプト調整 (spec §4.2)。省略可。
    #[serde(default)]
    pub prompt: Option<GhostPrompt>,
}

/// ゴースト作者が指定する、LLM への文体指示 (spec §4.2)。
///
/// **ここに書かれたものを読むだけで、生成はしない。** 第三者ゴーストの人格記述を
/// 機械生成して LLM に演じさせる導線は spec §6.4 のレッドラインで禁止している。
#[derive(Debug, Clone, Deserialize)]
pub struct GhostPrompt {
    /// 1 発話あたりの目安文字数。プロンプトに指示として載せるだけで、強制はしない。
    #[serde(default)]
    pub max_chars_per_line: Option<u32>,
    /// 掛け合い全体のトーン指示。
    #[serde(default)]
    pub style_notes: Option<String>,
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
    /// キャラクターの人格記述 (spec §4.2)。advanced 経路の system prompt に載る。
    /// 省略時はキャラ名だけが LLM に渡る。
    #[serde(default)]
    pub persona: Option<String>,
}

impl GhostCharacter {
    /// system prompt に載せる 1 行分の説明。
    /// persona があればそれを、無ければ役割だけの既定文を返す。
    pub fn describe(&self, fallback: &str) -> String {
        match self.persona.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(p) => p.to_string(),
            None => fallback.to_string(),
        }
    }
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
    /// 開口フレーム (spec §4.1.4)。pose 画像の隣に `<stem>_talk.<ext>` があれば
    /// **自動で拾う**（`shell.json` に書かせない）。キーは pose 名。
    ///
    /// `poses` と分けているのは、`poses` のキーが advanced の LLM に提示される
    /// pose 語彙そのものだから。ここに `normal_talk` を混ぜると LLM が選んでしまう。
    pub talk_poses: BTreeMap<String, String>,
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

/// `foo.png` -> `foo_talk.png`。実在する場合のみ Some (spec §4.1.4)。
fn talk_frame_path(pose_abs: &Path) -> Option<std::path::PathBuf> {
    let stem = pose_abs.file_stem()?.to_str()?;
    let ext = pose_abs.extension()?.to_str()?;
    let candidate = pose_abs.with_file_name(format!("{stem}_talk.{ext}"));
    candidate.is_file().then_some(candidate)
}

pub fn build_shell_character(def: &ShellCharacterDef, shell_dir: &Path) -> Result<ShellCharacter> {
    let mut poses = BTreeMap::new();
    let mut talk_poses = BTreeMap::new();
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

        // 開口フレーム: 同じディレクトリの `<stem>_talk.<ext>`。無ければ何もしない
        // (spec §4.1.4「無いシェルは口パクなし」)。読めない場合も黙って諦める
        // ため、talk フレームの不備で起動を壊さない。
        if let Some(talk_abs) = talk_frame_path(&abs) {
            if let Ok(talk_bytes) = std::fs::read(&talk_abs) {
                let tb64 = base64::engine::general_purpose::STANDARD.encode(talk_bytes);
                talk_poses.insert(name.clone(), format!("data:{mime};base64,{tb64}"));
            }
        }
    }
    Ok(ShellCharacter {
        base_size: def.base_size,
        default_pose: def.default_pose.clone(),
        poses,
        talk_poses,
        poke_regions: def.poke_regions,
    })
}

/// 出荷資産と Deserialize 構造体の**契約テスト**。
///
/// v0.4.1 まで `ghost.json` は `persona` / `prompt` を出荷しているのに
/// `GhostManifest` がフィールドを宣言しておらず、**serde が黙って捨てていた**。
/// 同じ型の見落とし (`cost_warning_80` の辞書キー欠落など) が計 12 件あり、
/// いずれも「コンパイルも通るしテストも緑」のまま出荷されていた。
///
/// ここでは **出荷している JSON の実キー集合**を構造体が受け取れるかを検査する。
/// 検査対象は同梱の既定資産に限定する（第三者ゴーストや DnD で入れた資産は
/// 未知キーを持ってよく、ここで落とすと取り込みが壊れる）。
#[cfg(test)]
mod shipped_asset_contract {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// リポジトリ直下からの相対パスを解決する。`CARGO_MANIFEST_DIR` は
    /// `src-tauri/` を指すので 1 つ上へ上がる。
    fn repo_path(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri の親")
            .join(rel)
    }

    fn read_json(rel: &str) -> serde_json::Value {
        let p = repo_path(rel);
        let raw = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("出荷資産を読めない {}: {e}", p.display()));
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("出荷資産が JSON として壊れている {}: {e}", p.display()))
    }

    fn keys(v: &serde_json::Value) -> BTreeSet<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// **構造体が実際に読むキー。** 増やしたらここも増やす。
    /// 「読む」ことの裏は `default_ghost_json_parses_with_persona` が取る
    /// （リストに書いただけで実際は読んでいない、を防ぐ）。
    const GHOST_READ: &[&str] = &[
        "schema_version", "id", "name", "characters", "dictionaries", "prompt",
    ];

    /// **出荷しているが意図的に読まないキー。** 理由を必ず書くこと。
    ///
    /// - `author`: ファイルを人が読むときの表示用。UI にもプロンプトにも出さない
    ///   （出すなら spec に要件が要る）。
    /// - `default_shell` は v0.5.1 で**読むようになった**（ゴースト切替でシェルが追従。
    ///   spec §4.5.6）。ただし `GhostManifest` のフィールドではなく
    ///   `commands::settings::default_shell_of` が切替時にファイルから直接読むため、
    ///   構造体の充填検査（`keys_listed_as_read_are_actually_populated`）の対象外。
    const GHOST_IGNORED: &[&str] = &["author", "default_shell"];

    /// 既定シェルの全 pose に開口フレームがあり、**自動検出できる**こと (spec §4.1.4)。
    ///
    /// v0.4.1 まで `_talk.png` は 8 枚あるのに boot payload に載らず、
    /// `mouth.ts` の呼び出し元もゼロだった（口パクが一度も動いていなかった）。
    #[test]
    fn default_shell_talk_frames_are_detected() {
        let shell_dir = repo_path("shells/default");
        let raw = std::fs::read_to_string(shell_dir.join("shell.json")).unwrap();
        let m: ShellManifest = serde_json::from_str(&raw).unwrap();
        for (slot, def) in [
            ("main", Some(&m.characters.main)),
            ("sub", m.characters.sub.as_ref()),
        ] {
            let Some(def) = def else { continue };
            let ch = build_shell_character(def, &shell_dir).unwrap();
            assert_eq!(
                ch.talk_poses.len(),
                ch.poses.len(),
                "{slot}: 開口フレームが揃っていない (poses={:?} talk={:?})",
                ch.poses.keys().collect::<Vec<_>>(),
                ch.talk_poses.keys().collect::<Vec<_>>()
            );
            for (name, url) in &ch.talk_poses {
                assert!(url.starts_with("data:image/"), "{slot}.{name} が data URL でない");
            }
        }
    }

    /// 開口フレームが無いシェルでも壊れないこと（spec §4.1.4「無いシェルは口パクなし」）。
    #[test]
    fn missing_talk_frames_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("normal.png");
        // 1x1 の最小 PNG
        std::fs::write(
            &img,
            [
                0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D',
                b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0,
                b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
            ],
        )
        .unwrap();
        let def = ShellCharacterDef {
            base_size: BaseSize { width: 1, height: 1 },
            default_pose: "normal".into(),
            poses: [("normal".to_string(), "normal.png".to_string())].into(),
            poke_regions: Default::default(),
        };
        let ch = build_shell_character(&def, dir.path()).unwrap();
        assert_eq!(ch.poses.len(), 1);
        assert!(ch.talk_poses.is_empty(), "無いのに開口フレームが生えた");
    }

    /// 既定ゴーストの JSON が持つキーが、**読む**か**意図的に無視する**かの
    /// どちらかに分類されていること。どちらでもないキーは
    /// 「宣言し忘れて serde が黙って捨てている」状態なので落とす。
    ///
    /// **これがあれば persona の取りこぼしは出荷前に落ちていた。**
    #[test]
    fn default_ghost_json_keys_are_all_accounted_for() {
        let v = read_json("ghosts/default/ghost.json");
        let accounted: BTreeSet<String> = GHOST_READ
            .iter()
            .chain(GHOST_IGNORED.iter())
            .map(|s| s.to_string())
            .collect();
        let shipped = keys(&v);
        let unaccounted: Vec<_> = shipped.difference(&accounted).collect();
        assert!(
            unaccounted.is_empty(),
            "ghost.json が出荷しているのに、読むとも無視するとも決めていないキー: {unaccounted:?}
             serde は未知キーを黙って捨てるので、宣言し忘れても誰も気づけない。
             構造体に足すか、GHOST_IGNORED に理由つきで足すこと。"
        );

        // キャラクター側。persona はここで守られる。
        let chars = v.get("characters").expect("characters が無い");
        for slot in ["main", "sub"] {
            let Some(c) = chars.get(slot) else { continue };
            let known_c: BTreeSet<String> =
                ["name", "persona"].iter().map(|s| s.to_string()).collect();
            let shipped_c = keys(c);
            let ignored_c: Vec<_> = shipped_c.difference(&known_c).collect();
            assert!(
                ignored_c.is_empty(),
                "characters.{slot} の未受領キー: {ignored_c:?}"
            );
        }

        if let Some(p) = v.get("prompt") {
            let known_p: BTreeSet<String> = ["max_chars_per_line", "style_notes"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let shipped_p = keys(p);
            let ignored_p: Vec<_> = shipped_p.difference(&known_p).collect();
            assert!(ignored_p.is_empty(), "prompt の未受領キー: {ignored_p:?}");
        }
    }

    /// 出荷している既定ゴーストが、実際に構造体へパースできること。
    /// かつ **persona が実際に入っている**こと（空の JSON でも上のテストは通るため）。
    #[test]
    fn default_ghost_json_parses_with_persona() {
        let raw = std::fs::read_to_string(repo_path("ghosts/default/ghost.json")).unwrap();
        let g: GhostManifest = serde_json::from_str(&raw).expect("ghost.json のパースに失敗");
        assert_eq!(g.schema_version, 1);
        assert!(
            g.characters.main.persona.is_some(),
            "既定ゴーストの main に persona が無い"
        );
        assert!(
            g.characters.sub.as_ref().and_then(|s| s.persona.as_ref()).is_some(),
            "既定ゴーストの sub に persona が無い"
        );
        let p = g.prompt.as_ref().expect("prompt が無い");
        assert!(p.max_chars_per_line.is_some() && p.style_notes.is_some());
    }

    /// shell.json 側も同じ扱い。`author` は ghost.json と同じ理由で意図的に読まない。
    #[test]
    fn default_shell_json_keys_are_all_accounted_for() {
        let v = read_json("shells/default/shell.json");
        const SHELL_READ: &[&str] = &["schema_version", "id", "name", "characters"];
        const SHELL_IGNORED: &[&str] = &["author"];
        let accounted: BTreeSet<String> = SHELL_READ
            .iter()
            .chain(SHELL_IGNORED.iter())
            .map(|s| s.to_string())
            .collect();
        let shipped = keys(&v);
        let unaccounted: Vec<_> = shipped.difference(&accounted).collect();
        assert!(
            unaccounted.is_empty(),
            "shell.json の未分類キー: {unaccounted:?}"
        );
    }

    /// **`GHOST_READ` に並べたキーが、実際に構造体へ入ること。**
    /// リストに書いただけで実は読んでいない、という嘘を防ぐ。
    #[test]
    fn keys_listed_as_read_are_actually_populated() {
        let raw = std::fs::read_to_string(repo_path("ghosts/default/ghost.json")).unwrap();
        let g: GhostManifest = serde_json::from_str(&raw).expect("ghost.json のパースに失敗");
        // GHOST_READ の各キーに対応する値が実際に入っていること。
        assert_eq!(g.schema_version, 1, "schema_version");
        assert!(!g.id.is_empty(), "id");
        assert!(!g.name.is_empty(), "name");
        assert!(!g.characters.main.name.is_empty(), "characters");
        assert!(!g.dictionaries.is_empty(), "dictionaries");
        assert!(g.prompt.is_some(), "prompt");
        assert_eq!(GHOST_READ.len(), 6, "GHOST_READ を増やしたらここの検査も増やす");
    }
}
