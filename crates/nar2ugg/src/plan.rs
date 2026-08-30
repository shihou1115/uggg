//! `ShellDef` + CLI 指定 → `Plan`。**どの surface をどの pose にするかを決める。**
//!
//! ここは**画像を一切読まない**。だから `list` は、画像が壊れたシェルでも
//! 中身の一覧を出せる。「変換に失敗したので何も分からない」が最悪の体験
//! なので、一覧の可用性は変換の成否から独立させる。
//!
//! ## pose 割り当ては原理的に自動化できない
//! 伺かには表情番号の標準が無く (surface 0 = 本体・10 = 相方だけが保証)、
//! alias 名も作者の自由。したがって:
//! - 保証されるものは `PoseBasis::Fixed`
//! - alias 一致は `PoseBasis::Alias`
//! - それ以外は `PoseBasis::Guessed` で**推測と明記する**
//! - 利用者は `--pose main.happy=5` で上書きできる (`PoseBasis::UserSpecified`)

use std::collections::BTreeMap;

use anyhow::Result;

use crate::report::{Report, Slot};
use crate::shell_def::ShellDef;

/// 出力する shell の設計図。
#[derive(Debug)]
pub struct Plan {
    /// `validate_asset_id` を通る ID。ディレクトリ名になる。
    pub id: String,
    /// 表示名。日本語のままでよい。
    pub name: String,
    pub main: SlotPlan,
    /// 相方がダミー画像・不在なら None。`characters.sub` を丸ごと省略する。
    pub sub: Option<SlotPlan>,
}

/// 1 スロット分の割り当て。
#[derive(Debug)]
pub struct SlotPlan {
    pub default_pose: String,
    /// pose 名 → surface ID。
    pub poses: BTreeMap<String, u32>,
}

/// CLI から渡る、人手でしか決まらない指定。
#[derive(Debug, Default)]
pub struct PlanOptions {
    /// `--id`。自動生成した ID が不都合なときの逃げ道。
    pub id: Option<String>,
    /// `--pose main.happy=5`。
    pub poses: Vec<PoseSpec>,
    /// `--no-sub`。ダミー判定が外れたときの逃げ道。
    pub no_sub: bool,
}

/// `--pose <SLOT>.<NAME>=<SURFACE_ID>` 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoseSpec {
    pub slot: Slot,
    pub pose: String,
    pub surface_id: u32,
}

impl std::str::FromStr for PoseSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (lhs, id) = s
            .split_once('=')
            .ok_or_else(|| format!("`{s}`: `main.happy=5` の形式で指定してください"))?;
        let (slot, pose) = lhs
            .split_once('.')
            .ok_or_else(|| format!("`{s}`: pose 名に `main.` / `sub.` を付けてください"))?;
        let slot = match slot {
            "main" => Slot::Main,
            "sub" => Slot::Sub,
            other => return Err(format!("`{other}`: スロットは main か sub です")),
        };
        if pose.is_empty() {
            return Err(format!("`{s}`: pose 名が空です"));
        }
        let surface_id: u32 = id
            .parse()
            .map_err(|_| format!("`{id}`: surface ID は 0 以上の整数です"))?;
        Ok(PoseSpec { slot, pose: pose.to_string(), surface_id })
    }
}

/// 割り当てを決める。
pub fn build(def: &ShellDef, opts: &PlanOptions, report: &mut Report) -> Result<Plan> {
    let _ = (def, opts, report);
    todo!("plan::build")
}

/// ugg の `validate_asset_id` (src-tauri/src/ghost/dnd.rs) のミラー。
///
/// **ugg 側と同じ id 集合を弾くこと**が契約。理由コードは返さない
/// (ugg 側が判定順を入れ替えただけで壊れるテストを作らないため)。
pub fn validate_id(id: &str) -> bool {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    // ugg 側は `id.len() > 64`、すなわち UTF-8 の**バイト長**で見ている。
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    if id == "." || id == ".." {
        return false;
    }
    if id.contains(['/', '\\', '\0', ':']) {
        return false;
    }
    if id.chars().any(|c| c.is_control()) {
        return false;
    }
    if id != id.trim() {
        return false;
    }
    let stem = id.split('.').next().unwrap_or(id).to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return false;
    }
    !id.ends_with('.')
}

/// 元シェルの名前から ID を作る。
///
/// 非英数を `-` に潰して ASCII 化する。`validate_asset_id` 自体は非 ASCII を
/// 禁じていないが、ID はディレクトリ名として使われるので ASCII が安全。
/// 潰した結果が空になる (名前が全部日本語) 場合は None を返し、呼び出し側が
/// `--id` を促す。
pub fn sanitize_id(raw: &str) -> Option<String> {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    // 64 バイト上限。ASCII なので文字数と一致する。
    let mut id: String = trimmed.chars().take(64).collect();
    id = id.trim_end_matches('-').to_string();
    if id.is_empty() {
        return None;
    }
    // 予約名は接尾辞で回避する ("con" → "con-shell")。
    if !validate_id(&id) {
        id.push_str("-shell");
    }
    validate_id(&id).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn validate_id_mirrors_ugg_rules() {
        assert!(validate_id("default"));
        assert!(validate_id("my-shell-01"));

        assert!(!validate_id(""));
        assert!(!validate_id("."));
        assert!(!validate_id(".."));
        assert!(!validate_id("a/b"));
        assert!(!validate_id(r"a\b"));
        assert!(!validate_id("C:name"));
        assert!(!validate_id("a\0b"));
        assert!(!validate_id("a\nb"));
        assert!(!validate_id(" pad"));
        assert!(!validate_id("pad "));
        assert!(!validate_id("CON"));
        assert!(!validate_id("con.png"));
        assert!(!validate_id("lpt9"));
        assert!(!validate_id("name."));
        assert!(!validate_id(&"a".repeat(65)));
        assert!(validate_id(&"a".repeat(64)));
    }

    #[test]
    fn sanitize_id_produces_valid_ids() {
        assert_eq!(sanitize_id("My Shell!").as_deref(), Some("my-shell"));
        assert_eq!(sanitize_id("shell_01").as_deref(), Some("shell-01"));
        assert_eq!(sanitize_id("--weird--").as_deref(), Some("weird"));
        // 日本語だけの名前は ASCII 化すると何も残らない。
        assert_eq!(sanitize_id("さくら"), None);
        // 予約名は回避される。
        assert_eq!(sanitize_id("CON").as_deref(), Some("con-shell"));
    }

    /// sanitize_id の後置条件: 何を渡しても、返るなら必ず validate_id を通る。
    #[test]
    fn sanitize_id_output_always_validates() {
        let long = "a".repeat(200);
        let inputs = [
            "さくら",
            "CON",
            "aux.txt",
            "  ",
            "///",
            long.as_str(),
            "日本語 mixed 名前",
            "..",
            "trailing.",
            "\u{0}\u{1}",
        ];
        for raw in inputs {
            if let Some(id) = sanitize_id(raw) {
                assert!(validate_id(&id), "sanitize_id({raw:?}) = {id:?} が無効");
            }
        }
    }

    #[test]
    fn pose_spec_parses() {
        assert_eq!(
            PoseSpec::from_str("main.happy=5").unwrap(),
            PoseSpec { slot: Slot::Main, pose: "happy".into(), surface_id: 5 }
        );
        assert_eq!(
            PoseSpec::from_str("sub.normal=10").unwrap(),
            PoseSpec { slot: Slot::Sub, pose: "normal".into(), surface_id: 10 }
        );
    }

    #[test]
    fn pose_spec_rejects_bad_input() {
        for bad in ["happy=5", "main.happy", "other.happy=5", "main.=5", "main.happy=x"] {
            assert!(PoseSpec::from_str(bad).is_err(), "{bad} が通ってしまった");
        }
    }
}
