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

    /// 同梱のすべてのゴースト × すべてのシェルの組み合わせが読み込めること（v0.6.0 項目 5）。
    /// ゴーストとシェルは別々に選べる（設定の `ghost_id` / `shell_id`）ので、`default` どうしだけでは足りない。
    #[test]
    fn loads_every_bundled_ghost_and_shell_pair() {
        let root = super::shipped_asset_contract::repo_root();
        for ghost_id in super::shipped_asset_contract::bundled_ghost_ids(&root) {
            for shell_id in super::shipped_asset_contract::bundled_shell_ids(&root) {
                let pair = format!("ghost '{ghost_id}' × shell '{shell_id}'");
                let bundle = load_bundle(assets_root(), &ghost_id, &shell_id)
                    .unwrap_or_else(|e| panic!("{pair}: 読み込めない: {e:#}"));
                assert_eq!(bundle.ghost.schema_version, 1, "{pair}");
                assert_eq!(bundle.ghost.id, ghost_id, "{pair}");
                assert_eq!(bundle.shell.id, shell_id, "{pair}");
                assert!(
                    bundle
                        .shell
                        .characters
                        .main
                        .poses
                        .contains_key(&bundle.shell.characters.main.default_pose),
                    "{pair}"
                );
                // v3 辞書もロードされている
                assert_eq!(bundle.dictionary.schema_version, 3, "{pair}");
                assert!(!bundle.dictionary.input_match.is_empty(), "{pair}: input_match must exist");
                assert!(bundle.dictionary.events.contains_key("first_boot"), "{pair}");
                assert!(bundle.dictionary.events.contains_key("boot"), "{pair}");
            }
        }
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
/// 検査対象は同梱の資産（リポジトリ直下の `ghosts/` `shells/` にあるすべての組）に限定する
/// （第三者ゴーストや DnD で入れた資産は未知キーを持ってよく、ここで落とすと取り込みが壊れる）。
///
/// **v0.6.0 項目 5: `default` 決め打ちをやめ、`ghosts/` `shells/` を走査して見つかった全組に当てる。**
/// 2 組目を足しても何も見張らない、という穴を塞ぐ。失敗のメッセージにはどの組かを入れる。
/// 形・表情・辞書の検査は `root` を引数に取る関数（`*_violations`）にしてあり、実リポジトリには
/// `bundled_*` のテストが、tempdir に組んだ 2 組目には `catches_*` のテストが同じ関数を当てる
/// （検査が壊れても緑のままにならないことを、常設の変異テストで確かめる）。
#[cfg(test)]
pub(crate) mod shipped_asset_contract {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// リポジトリ直下。`CARGO_MANIFEST_DIR` は `src-tauri/` を指すので 1 つ上へ上がる。
    pub(crate) fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri の親")
            .to_path_buf()
    }

    /// リポジトリ直下からの相対パスを解決する。
    fn repo_path(rel: &str) -> PathBuf {
        repo_root().join(rel)
    }

    /// `dir` 直下のディレクトリ名（昇順）。**空・`default` 不在は panic。**
    /// 走査が空だと、全組を見張るテストが何も見ずに通ってしまう（`default` は辞書の比較の基準でもある）。
    fn subdir_names(dir: &Path) -> Vec<String> {
        let rd = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("同梱の {} を走査できない: {e}", dir.display()));
        let mut out: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        out.sort();
        assert!(
            !out.is_empty(),
            "{} に 1 組も無い（走査が空だと、全組を見張るテストが何も見ずに通る）",
            dir.display()
        );
        assert!(
            out.iter().any(|n| n == "default"),
            "{} に default が無い（辞書の比較の基準）",
            dir.display()
        );
        out
    }

    /// `<root>/ghosts/` 直下のディレクトリ名（= 同梱のゴースト ID。一覧は manifest の id を返すので、
    /// 食い違いは `shape_violations` が見る）。
    pub(crate) fn bundled_ghost_ids(root: &Path) -> Vec<String> {
        subdir_names(&root.join("ghosts"))
    }

    /// `<root>/shells/` 直下のディレクトリ名（= 同梱のシェル ID）。
    pub(crate) fn bundled_shell_ids(root: &Path) -> Vec<String> {
        subdir_names(&root.join("shells"))
    }

    /// そのゴーストの辞書ファイルのパス（`ghost.json` の `dictionaries[0]`。読み込みは 1 件だけを受け付ける）。
    fn ghost_dictionary_path(root: &Path, ghost_id: &str) -> Result<PathBuf, String> {
        let dir = root.join("ghosts").join(ghost_id);
        let v = read_value(&dir.join("ghost.json"))?;
        let rel = v
            .get("dictionaries")
            .and_then(|d| d.get(0))
            .and_then(|d| d.as_str())
            .ok_or_else(|| format!("ghost '{ghost_id}': dictionaries[0] が無い"))?;
        Ok(dir.join(rel))
    }

    /// 同梱のすべてのゴーストの `(ゴースト ID, 辞書のパス)`。読めなければ panic（どのゴーストかを添える）。
    pub(crate) fn bundled_dictionary_paths(root: &Path) -> Vec<(String, PathBuf)> {
        bundled_ghost_ids(root)
            .into_iter()
            .map(|id| {
                let p = ghost_dictionary_path(root, &id).unwrap_or_else(|e| panic!("{e}"));
                (id, p)
            })
            .collect()
    }

    fn read_value(p: &Path) -> Result<serde_json::Value, String> {
        let raw = std::fs::read_to_string(p)
            .map_err(|e| format!("出荷資産を読めない {}: {e}", p.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("出荷資産が JSON として壊れている {}: {e}", p.display()))
    }

    /// 同梱が持つべき表情の名前（spec §6.0 v0.6.0。どのゴーストとシェルを組み合わせても表情が見つかる）。
    const POSES: [&str; 4] = ["normal", "happy", "troubled", "surprised"];

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

    // ===== 検査本体（`root` を引数に取る。実リポジトリにも tempdir の 2 組目にも同じものを当てる） =====

    /// **同梱の形**（v0.6.0 項目 5）。ディレクトリ名 == manifest の `id`・sub がある・`default_shell` の指す先が実在する。
    ///
    /// 一覧（`list_ghosts` / `list_shells`）は manifest の `id` を返し、読み込み（`load_bundle`）はその `id` を
    /// ディレクトリ名として使う。食い違うと、選んだ瞬間に次の起動が失敗する。
    fn shape_violations(root: &Path) -> Vec<String> {
        let mut v = Vec::new();
        for id in bundled_ghost_ids(root) {
            let path = root.join("ghosts").join(&id).join("ghost.json");
            let value = match read_value(&path) {
                Ok(x) => x,
                Err(e) => {
                    v.push(format!("ghost '{id}': {e}"));
                    continue;
                }
            };
            let g: GhostManifest = match serde_json::from_value(value.clone()) {
                Ok(g) => g,
                Err(e) => {
                    v.push(format!("ghost '{id}': GhostManifest としてパースできない: {e}"));
                    continue;
                }
            };
            if g.id != id {
                v.push(format!("ghost '{id}': ghost.json の id '{}' がディレクトリ名と違う", g.id));
            }
            if g.characters.sub.is_none() {
                v.push(format!("ghost '{id}': characters.sub が無い"));
            }
            match value.get("default_shell").and_then(|x| x.as_str()).map(str::trim) {
                None | Some("") => v.push(format!("ghost '{id}': default_shell が無い")),
                Some(shell) => {
                    // 本番の `default_shell_of` と同じ確かめ方（shell.json が実在するか）
                    if !root.join("shells").join(shell).join("shell.json").is_file() {
                        v.push(format!(
                            "ghost '{id}': default_shell '{shell}' の指す先（shells/{shell}/shell.json）が無い"
                        ));
                    }
                }
            }
        }
        for id in bundled_shell_ids(root) {
            let path = root.join("shells").join(&id).join("shell.json");
            match read_json_as::<ShellManifest>(&path) {
                Err(e) => v.push(format!("shell '{id}': {e}")),
                Ok(s) => {
                    if s.id != id {
                        v.push(format!("shell '{id}': shell.json の id '{}' がディレクトリ名と違う", s.id));
                    }
                    if s.characters.sub.is_none() {
                        v.push(format!("shell '{id}': characters.sub が無い"));
                    }
                }
            }
        }
        v
    }

    fn read_json_as<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("読めない {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("パースできない {}: {e}", path.display()))
    }

    /// **シェル側の表情の契約**（v0.6.0 項目 5）。main と sub の両方が 4 つの表情と、それぞれの口パクの絵
    /// （`<表情>_talk.<拡張子>`。検出は本番の `talk_frame_path` そのもの）を持ち、`default_pose` がその 1 つ。
    fn shell_pose_violations(root: &Path) -> Vec<String> {
        let mut v = Vec::new();
        for id in bundled_shell_ids(root) {
            let shell_dir = root.join("shells").join(&id);
            let m = match read_json_as::<ShellManifest>(&shell_dir.join("shell.json")) {
                Ok(m) => m,
                Err(e) => {
                    v.push(format!("shell '{id}': {e}"));
                    continue;
                }
            };
            for (slot, def) in [("main", Some(&m.characters.main)), ("sub", m.characters.sub.as_ref())] {
                let Some(def) = def else {
                    v.push(format!("shell '{id}': {slot} が無い"));
                    continue;
                };
                for pose in POSES {
                    let Some(rel) = def.poses.get(pose) else {
                        v.push(format!("shell '{id}' {slot}: 表情 '{pose}' が poses に無い"));
                        continue;
                    };
                    let abs = shell_dir.join(rel);
                    if !abs.is_file() {
                        v.push(format!("shell '{id}' {slot}: 表情 '{pose}' の絵が無い（{rel}）"));
                    } else if talk_frame_path(&abs).is_none() {
                        v.push(format!(
                            "shell '{id}' {slot}: 表情 '{pose}' の口パクの絵（{pose}_talk.<同じ拡張子>）が無い"
                        ));
                    }
                }
                if !POSES.contains(&def.default_pose.as_str()) {
                    v.push(format!(
                        "shell '{id}' {slot}: default_pose '{}' が 4 つの表情のどれでもない",
                        def.default_pose
                    ));
                }
            }
        }
        v
    }

    /// 辞書の中の `pose:` をすべて集める（`pose` キーの値が文字列でも null でもないものも拾う）。
    /// 返り値は 表情名 → 最初に見つけた場所。
    fn walk_poses(v: &serde_yaml::Value, path: &str, bad: &mut std::collections::BTreeMap<String, String>) {
        match v {
            serde_yaml::Value::Mapping(m) => {
                for (k, val) in m {
                    let key = k.as_str().unwrap_or("?");
                    let here = format!("{path}.{key}");
                    if key == "pose" {
                        match val {
                            serde_yaml::Value::String(s) if POSES.contains(&s.as_str()) => {}
                            serde_yaml::Value::String(s) => {
                                bad.entry(s.clone()).or_insert(here);
                            }
                            serde_yaml::Value::Null => {}
                            other => {
                                bad.entry(format!("{other:?}")).or_insert(here);
                            }
                        }
                    } else {
                        walk_poses(val, &here, bad);
                    }
                }
            }
            serde_yaml::Value::Sequence(s) => {
                for (i, val) in s.iter().enumerate() {
                    walk_poses(val, &format!("{path}[{i}]"), bad);
                }
            }
            _ => {}
        }
    }

    /// **辞書側の表情の契約**（v0.6.0 項目 5）。同梱のすべての辞書が使う `pose:` が 4 つの表情に収まる。
    /// ゴーストとシェルは別々に選べるので、収まっていないと組み合わせによっては表情が見つからない。
    fn dictionary_pose_violations(root: &Path) -> Vec<String> {
        let mut v = Vec::new();
        for id in bundled_ghost_ids(root) {
            let doc = ghost_dictionary_path(root, &id).and_then(|p| {
                let raw = std::fs::read_to_string(&p)
                    .map_err(|e| format!("辞書を読めない {}: {e}", p.display()))?;
                serde_yaml::from_str::<serde_yaml::Value>(&raw)
                    .map_err(|e| format!("辞書が YAML として壊れている {}: {e}", p.display()))
            });
            match doc {
                Err(e) => v.push(format!("ghost '{id}': {e}")),
                Ok(doc) => {
                    let mut bad = std::collections::BTreeMap::new();
                    walk_poses(&doc, "", &mut bad);
                    for (name, at) in bad {
                        v.push(format!(
                            "ghost '{id}': 辞書が 4 つの表情の外の pose '{name}' を使っている（最初の場所 {at}）"
                        ));
                    }
                }
            }
        }
        v
    }

    /// **辞書キーの集合の比較**（v0.6.0 項目 5）。同梱のどの辞書も、`default` の辞書のキーの集合を含む。
    /// events・system_messages の各キー（かつ、そのキーの候補が空でない）、input_prompt / menu_prompt の
    /// main・sub、recall・monologue・入力への応答・空振りの応答が空でない。
    fn dictionary_violations(root: &Path) -> Vec<String> {
        fn load(root: &Path, id: &str) -> Result<dict::Dictionary, String> {
            let p = ghost_dictionary_path(root, id)?;
            dict::load_dictionary(&p).map_err(|e| format!("{e:#}"))
        }
        let reference = match load(root, "default") {
            Ok(d) => d,
            Err(e) => return vec![format!("基準の ghosts/default の辞書を読めない: {e}")],
        };
        let mut v = Vec::new();
        for id in bundled_ghost_ids(root) {
            let d = match load(root, &id) {
                Ok(d) => d,
                Err(e) => {
                    v.push(format!("ghost '{id}': 辞書を読めない: {e}"));
                    continue;
                }
            };
            for (section, base, mine) in [
                ("events", &reference.events, &d.events),
                ("system_messages", &reference.system_messages, &d.system_messages),
            ] {
                let mut missing: Vec<&String> = base.keys().filter(|k| !mine.contains_key(*k)).collect();
                missing.sort();
                if !missing.is_empty() {
                    v.push(format!("ghost '{id}': {section} に default のキーが無い: {missing:?}"));
                }
                let mut empty: Vec<&String> = base
                    .keys()
                    .filter(|k| mine.get(*k).is_some_and(|list| list.is_empty()))
                    .collect();
                empty.sort();
                if !empty.is_empty() {
                    v.push(format!("ghost '{id}': {section} のキーはあるが候補が空: {empty:?}"));
                }
            }
            let answering_rules = d.input_match.iter().filter(|r| !r.responses.is_empty()).count();
            for (name, len) in [
                ("recall", d.recall.len()),
                ("monologue", d.monologue.len()),
                ("input_match（入力への応答。応答を持つ規則）", answering_rules),
                ("fallback（空振りの応答）", d.fallback.len()),
                ("input_prompt.main", d.input_prompt_main.len()),
                ("input_prompt.sub", d.input_prompt_sub.len()),
                ("menu_prompt.main", d.menu_prompt_main.len()),
                ("menu_prompt.sub", d.menu_prompt_sub.len()),
            ] {
                if len == 0 {
                    v.push(format!("ghost '{id}': {name} が空"));
                }
            }
        }
        v
    }

    /// 同梱のすべてのゴースト・シェルの形（v0.6.0 項目 5）。
    #[test]
    fn bundled_ghosts_and_shells_have_a_consistent_shape() {
        let v = shape_violations(&repo_root());
        assert!(v.is_empty(), "同梱の形の契約違反:\n{}", v.join("\n"));
    }

    /// 同梱のすべてのシェルが 4 つの表情と口パクの絵を持つ（v0.6.0 項目 5）。
    #[test]
    fn bundled_shells_have_the_four_poses_with_talk_frames() {
        let v = shell_pose_violations(&repo_root());
        assert!(v.is_empty(), "同梱のシェルの表情の契約違反:\n{}", v.join("\n"));
    }

    /// 同梱のすべての辞書が使う表情が 4 つに収まる（v0.6.0 項目 5）。
    #[test]
    fn bundled_dictionaries_use_only_the_four_poses() {
        let v = dictionary_pose_violations(&repo_root());
        assert!(v.is_empty(), "同梱の辞書の表情の契約違反:\n{}", v.join("\n"));
    }

    /// 同梱のどの辞書も `default` の辞書のキーの集合を含む（v0.6.0 項目 5）。
    #[test]
    fn bundled_dictionaries_cover_the_default_key_set() {
        let v = dictionary_violations(&repo_root());
        assert!(v.is_empty(), "同梱の辞書のキー網羅の契約違反:\n{}", v.join("\n"));
    }

    /// **構造体が実際に読むキー。** 増やしたらここも増やす。
    /// 「読む」ことの裏は `bundled_ghost_json_parses_with_persona` が取る
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

    /// 同梱のすべてのシェルの全 pose に開口フレームがあり、**自動検出できる**こと (spec §4.1.4)。
    ///
    /// v0.4.1 まで `_talk.png` は 8 枚あるのに boot payload に載らず、
    /// `mouth.ts` の呼び出し元もゼロだった（口パクが一度も動いていなかった）。
    /// v0.6.0 項目 5 で、`shells/` を走査して全シェルに当てる形に広げた。
    #[test]
    fn bundled_shell_talk_frames_are_detected() {
        let root = repo_root();
        for id in bundled_shell_ids(&root) {
            let shell_dir = root.join("shells").join(&id);
            let raw = std::fs::read_to_string(shell_dir.join("shell.json")).unwrap();
            let m: ShellManifest = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("shell '{id}': shell.json をパースできない: {e}"));
            for (slot, def) in [
                ("main", Some(&m.characters.main)),
                ("sub", m.characters.sub.as_ref()),
            ] {
                let Some(def) = def else { continue };
                let ch = build_shell_character(def, &shell_dir)
                    .unwrap_or_else(|e| panic!("shell '{id}' {slot}: 絵を読み込めない: {e:#}"));
                assert_eq!(
                    ch.talk_poses.len(),
                    ch.poses.len(),
                    "shell '{id}' {slot}: 開口フレームが揃っていない (poses={:?} talk={:?})",
                    ch.poses.keys().collect::<Vec<_>>(),
                    ch.talk_poses.keys().collect::<Vec<_>>()
                );
                for (name, url) in &ch.talk_poses {
                    assert!(
                        url.starts_with("data:image/"),
                        "shell '{id}' {slot}.{name} が data URL でない"
                    );
                }
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

    /// 同梱のすべてのゴーストの JSON が持つキーが、**読む**か**意図的に無視する**かの
    /// どちらかに分類されていること。どちらでもないキーは
    /// 「宣言し忘れて serde が黙って捨てている」状態なので落とす。
    ///
    /// **これがあれば persona の取りこぼしは出荷前に落ちていた。**
    /// v0.6.0 項目 5 で、`ghosts/` を走査して全ゴーストに当てる形に広げた。
    #[test]
    fn bundled_ghost_json_keys_are_all_accounted_for() {
        for id in bundled_ghost_ids(&repo_root()) {
            let v = read_json(&format!("ghosts/{id}/ghost.json"));
            let accounted: BTreeSet<String> = GHOST_READ
                .iter()
                .chain(GHOST_IGNORED.iter())
                .map(|s| s.to_string())
                .collect();
            let shipped = keys(&v);
            let unaccounted: Vec<_> = shipped.difference(&accounted).collect();
            assert!(
                unaccounted.is_empty(),
                "ghost '{id}': ghost.json が出荷しているのに、読むとも無視するとも決めていないキー: {unaccounted:?}
                 serde は未知キーを黙って捨てるので、宣言し忘れても誰も気づけない。
                 構造体に足すか、GHOST_IGNORED に理由つきで足すこと。"
            );

            // キャラクター側。persona はここで守られる。
            let chars = v
                .get("characters")
                .unwrap_or_else(|| panic!("ghost '{id}': characters が無い"));
            for slot in ["main", "sub"] {
                let Some(c) = chars.get(slot) else { continue };
                let known_c: BTreeSet<String> =
                    ["name", "persona"].iter().map(|s| s.to_string()).collect();
                let shipped_c = keys(c);
                let ignored_c: Vec<_> = shipped_c.difference(&known_c).collect();
                assert!(
                    ignored_c.is_empty(),
                    "ghost '{id}': characters.{slot} の未受領キー: {ignored_c:?}"
                );
            }

            if let Some(p) = v.get("prompt") {
                let known_p: BTreeSet<String> = ["max_chars_per_line", "style_notes"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let shipped_p = keys(p);
                let ignored_p: Vec<_> = shipped_p.difference(&known_p).collect();
                assert!(
                    ignored_p.is_empty(),
                    "ghost '{id}': prompt の未受領キー: {ignored_p:?}"
                );
            }
        }
    }

    /// 同梱のすべてのゴーストが、実際に構造体へパースできること。
    /// かつ **persona が実際に入っている**こと（空の JSON でも上のテストは通るため）。
    #[test]
    fn bundled_ghost_json_parses_with_persona() {
        for id in bundled_ghost_ids(&repo_root()) {
            let raw = std::fs::read_to_string(repo_path(&format!("ghosts/{id}/ghost.json"))).unwrap();
            let g: GhostManifest = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("ghost '{id}': ghost.json のパースに失敗: {e}"));
            assert_eq!(g.schema_version, 1, "ghost '{id}'");
            assert!(
                g.characters.main.persona.is_some(),
                "ghost '{id}': main に persona が無い"
            );
            assert!(
                g.characters.sub.as_ref().and_then(|s| s.persona.as_ref()).is_some(),
                "ghost '{id}': sub に persona が無い"
            );
            let p = g
                .prompt
                .as_ref()
                .unwrap_or_else(|| panic!("ghost '{id}': prompt が無い"));
            assert!(
                p.max_chars_per_line.is_some() && p.style_notes.is_some(),
                "ghost '{id}': prompt に max_chars_per_line / style_notes が揃っていない"
            );
        }
    }

    /// shell.json 側も同じ扱い（全シェル）。`author` は ghost.json と同じ理由で意図的に読まない。
    #[test]
    fn bundled_shell_json_keys_are_all_accounted_for() {
        for id in bundled_shell_ids(&repo_root()) {
            let v = read_json(&format!("shells/{id}/shell.json"));
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
                "shell '{id}': shell.json の未分類キー: {unaccounted:?}"
            );
        }
    }

    /// **`GHOST_READ` に並べたキーが、実際に構造体へ入ること**（全ゴースト）。
    /// リストに書いただけで実は読んでいない、という嘘を防ぐ。
    #[test]
    fn keys_listed_as_read_are_actually_populated() {
        for id in bundled_ghost_ids(&repo_root()) {
            let raw = std::fs::read_to_string(repo_path(&format!("ghosts/{id}/ghost.json"))).unwrap();
            let g: GhostManifest = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("ghost '{id}': ghost.json のパースに失敗: {e}"));
            // GHOST_READ の各キーに対応する値が実際に入っていること。
            assert_eq!(g.schema_version, 1, "ghost '{id}': schema_version");
            assert!(!g.id.is_empty(), "ghost '{id}': id");
            assert!(!g.name.is_empty(), "ghost '{id}': name");
            assert!(!g.characters.main.name.is_empty(), "ghost '{id}': characters");
            assert!(!g.dictionaries.is_empty(), "ghost '{id}': dictionaries");
            assert!(g.prompt.is_some(), "ghost '{id}': prompt");
        }
        assert_eq!(GHOST_READ.len(), 6, "GHOST_READ を増やしたらここの検査も増やす");
    }

    // ===== 検査そのものの効き（2 組目を tempdir に組んで壊す。v0.6.0 項目 5） =====
    //
    // 上の `bundled_*` は実リポジトリの全組に当たるが、いまの同梱は 1 組なので、**2 組目が足されたとき
    // 本当に鳴るか**は実物では確かめられない。そこで `default` を `second` として複製した 2 組目を
    // tempdir に作り、1 点だけ壊して、同じ検査が**どの組かを名指しして**落ちることを確かめる。
    // 壊さなければ緑（対照）、壊し方が空振りなら（置換元が無ければ）その場で panic する。

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dst = to.join(e.file_name());
            if e.path().is_dir() {
                copy_dir(&e.path(), &dst);
            } else {
                std::fs::copy(e.path(), &dst).unwrap();
            }
        }
    }

    /// 置換して書き戻す。**置換元が無ければ panic**（壊したつもりで何も壊れていない空振りを防ぐ）。
    fn edit(root: &Path, rel: &str, from: &str, to: &str) {
        let p = root.join(rel);
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains(from), "変異が空振り: {rel} に {from:?} が無い");
        std::fs::write(&p, raw.replace(from, to)).unwrap();
    }

    /// `default` を `default` と `second` の 2 組として複製した、壊していない同梱の見本。
    fn fixture_with_second() -> tempfile::TempDir {
        let src = repo_root();
        let dir = tempfile::tempdir().unwrap();
        for kind in ["ghosts", "shells"] {
            for to in ["default", "second"] {
                copy_dir(&src.join(kind).join("default"), &dir.path().join(kind).join(to));
            }
        }
        let r = dir.path();
        edit(r, "ghosts/second/ghost.json", "\"id\": \"default\"", "\"id\": \"second\"");
        edit(r, "ghosts/second/ghost.json", "\"default_shell\": \"default\"", "\"default_shell\": \"second\"");
        edit(r, "shells/second/shell.json", "\"id\": \"default\"", "\"id\": \"second\"");
        dir
    }

    /// `violations` のうち 1 行が `needles` をすべて含むこと（どの組のどこが違うかまで名指しされている）。
    fn assert_caught(violations: &[String], needles: &[&str], label: &str) {
        assert!(
            violations.iter().any(|l| needles.iter().all(|n| l.contains(n))),
            "{label}: {needles:?} を名指しする違反が出ていない。出た違反: {violations:#?}"
        );
    }

    /// 対照: 壊していない 2 組（`default` と、その複製の `second`）は全検査を通る。
    /// これが通らないと、下の「壊したら落ちる」が何で落ちたのか分からない。
    #[test]
    fn an_untouched_second_bundle_passes_every_check() {
        let dir = fixture_with_second();
        assert_eq!(bundled_ghost_ids(dir.path()), ["default", "second"]);
        assert_eq!(bundled_shell_ids(dir.path()), ["default", "second"]);
        for (name, v) in [
            ("shape", shape_violations(dir.path())),
            ("shell_pose", shell_pose_violations(dir.path())),
            ("dictionary_pose", dictionary_pose_violations(dir.path())),
            ("dictionary", dictionary_violations(dir.path())),
        ] {
            assert!(v.is_empty(), "{name}: 壊していないのに違反: {v:#?}");
        }
    }

    /// (a) 辞書のキーを 1 つ消す／空にする。events・system_messages・各セクション。
    #[test]
    fn catches_a_second_dictionary_missing_default_keys() {
        const DIC: &str = "ghosts/second/dic/main.yaml";
        for (from, to, needles) in [
            ("  idle:", "  idle_x:", vec!["second", "events", "idle"]),
            ("  first_boot:", "  first_boot_x:", vec!["second", "events", "first_boot"]),
            ("  cost_unknown:", "  cost_unknown_x:", vec!["second", "system_messages", "cost_unknown"]),
            // キーはあるが候補が空（辞書の「キーがある」だけでは、そのキーで黙る）
            ("  idle:", "  idle: []\n  idle_x:", vec!["second", "events", "候補が空", "idle"]),
            ("  mode_recovered:", "  mode_recovered: []\n  mode_recovered_x:", vec!["second", "system_messages", "候補が空", "mode_recovered"]),
            ("\nrecall:", "\nrecall_x:", vec!["second", "recall"]),
            ("\nmonologue:", "\nmonologue_x:", vec!["second", "monologue"]),
            ("\ninput_match:", "\ninput_match_x:", vec!["second", "input_match"]),
            ("\nfallback:", "\nfallback_x:", vec!["second", "fallback"]),
            ("\ninput_prompt:", "\ninput_prompt_x:", vec!["second", "input_prompt"]),
            ("\nmenu_prompt:", "\nmenu_prompt_x:", vec!["second", "menu_prompt"]),
        ] {
            let dir = fixture_with_second();
            edit(dir.path(), DIC, from, to);
            assert_caught(&dictionary_violations(dir.path()), &needles, &format!("{from:?}"));
        }
    }

    /// (b) 表情の名前を崩す（シェル側の `happy` → `smile`、辞書側の `pose: happy` → `pose: smile`）。
    #[test]
    fn catches_a_second_bundle_with_a_broken_pose_name() {
        let dir = fixture_with_second();
        edit(dir.path(), "shells/second/shell.json", "\"happy\":", "\"smile\":");
        assert_caught(
            &shell_pose_violations(dir.path()),
            &["second", "main", "'happy'", "poses に無い"],
            "シェルの happy → smile (main)",
        );
        assert_caught(
            &shell_pose_violations(dir.path()),
            &["second", "sub", "'happy'", "poses に無い"],
            "シェルの happy → smile (sub)",
        );

        let dir = fixture_with_second();
        edit(dir.path(), "shells/second/shell.json", "\"default_pose\": \"normal\"", "\"default_pose\": \"calm\"");
        assert_caught(
            &shell_pose_violations(dir.path()),
            &["second", "default_pose", "'calm'"],
            "default_pose が 4 つの外",
        );

        let dir = fixture_with_second();
        edit(dir.path(), "ghosts/second/dic/main.yaml", "pose: happy", "pose: smile");
        assert_caught(
            &dictionary_pose_violations(dir.path()),
            &["second", "'smile'"],
            "辞書の pose: happy → smile",
        );
        // 対になる default は壊していないので名指しされない
        assert!(
            dictionary_pose_violations(dir.path()).iter().all(|l| !l.contains("'default'")),
            "壊していない default まで違反に数えている"
        );
    }

    /// (c) ディレクトリ名と manifest の `id` をずらす（ゴースト・シェルの両方）。
    #[test]
    fn catches_a_second_bundle_whose_id_differs_from_its_directory() {
        let dir = fixture_with_second();
        edit(dir.path(), "ghosts/second/ghost.json", "\"id\": \"second\"", "\"id\": \"other\"");
        assert_caught(&shape_violations(dir.path()), &["ghost 'second'", "'other'", "ディレクトリ名"], "ghost の id");

        let dir = fixture_with_second();
        edit(dir.path(), "shells/second/shell.json", "\"id\": \"second\"", "\"id\": \"other\"");
        assert_caught(&shape_violations(dir.path()), &["shell 'second'", "'other'", "ディレクトリ名"], "shell の id");
    }

    /// (d) 口パクの絵を 1 枚消す（main と sub のそれぞれ）。
    #[test]
    fn catches_a_second_shell_missing_a_talk_frame() {
        for (rel, slot, pose) in [
            ("shells/second/main/happy_talk.png", "main", "happy"),
            ("shells/second/sub/surprised_talk.png", "sub", "surprised"),
        ] {
            let dir = fixture_with_second();
            std::fs::remove_file(dir.path().join(rel)).unwrap();
            assert_caught(
                &shell_pose_violations(dir.path()),
                &["second", slot, &format!("'{pose}'"), "口パク"],
                rel,
            );
        }
    }

    /// (e) `default_shell` を実在しない名前にする／消す。さらに sub を落とす。
    #[test]
    fn catches_a_second_ghost_pointing_at_a_missing_shell_or_lacking_a_sub() {
        let dir = fixture_with_second();
        edit(dir.path(), "ghosts/second/ghost.json", "\"default_shell\": \"second\"", "\"default_shell\": \"no-such-shell\"");
        assert_caught(&shape_violations(dir.path()), &["ghost 'second'", "default_shell", "no-such-shell"], "実在しない default_shell");

        let dir = fixture_with_second();
        edit(dir.path(), "ghosts/second/ghost.json", "\"default_shell\"", "\"default_shell_x\"");
        assert_caught(&shape_violations(dir.path()), &["ghost 'second'", "default_shell が無い"], "default_shell が無い");

        let dir = fixture_with_second();
        edit(dir.path(), "ghosts/second/ghost.json", "\"sub\":", "\"sub_x\":");
        assert_caught(&shape_violations(dir.path()), &["ghost 'second'", "sub が無い"], "ghost の sub");

        let dir = fixture_with_second();
        edit(dir.path(), "shells/second/shell.json", "\"sub\":", "\"sub_x\":");
        assert_caught(&shape_violations(dir.path()), &["shell 'second'", "sub が無い"], "shell の sub");
    }
}
