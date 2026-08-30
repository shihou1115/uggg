//! `descript.txt` + `surfaces.txt` → `ShellDef`。**伺かの語彙はここで終わる。**
//!
//! 下流 (`plan` / `imaging` / `emit`) は SERIKO も さくらスクリプト も知らない。
//! この境界を守るため、SERIKO のアニメーション定義は `ShellDef` に**持たない**
//! (読み飛ばして警告するだけ)。「せっかく読んだから」で保持し始めると、
//! 使われないフィールドが仕様のように見え始める。
//!
//! ## 読むキーはこれだけ
//! descript.txt: `name` / `craftman`(`craftmanw`) / `seriko.use_self_alpha` /
//!               `sakura.surface.alias`・`kero.surface.alias` (surfaces.txt 側にもある)
//! surfaces.txt: `elementN` / `collisionN` / `name` / alias ブレス
//!
//! それ以外 (balloon offset, point.*, icon.rect, menu, dpi, zorder, animation*) は
//! ugg に対応概念が無いか静止画に効かないので、構造体に置かない。

use std::collections::BTreeMap;

use anyhow::Result;

use crate::report::{Report, Slot};
use crate::source::SourceTree;

/// 変換に必要な範囲だけに絞ったシェル定義。
#[derive(Debug, Default)]
pub struct ShellDef {
    /// `descript.txt` の `name`。日本語のまま保持する (shell.json の name に入る)。
    pub name: Option<String>,
    /// `craftman` / `craftmanw`。
    pub author: Option<String>,
    /// `seriko.use_self_alpha`。真なら PNG のアルファチャンネルを尊重する。
    pub use_self_alpha: bool,
    /// surface ID → 定義。`surfaceN.png` が実在するだけの surface も含む。
    pub surfaces: BTreeMap<u32, Surface>,
    /// alias。**pose 名の唯一の信頼できる出所**であり、10 番以外で
    /// 本体/相方を判別する唯一の手掛かりでもある。
    pub aliases: Vec<Alias>,
    /// `sakura.seriko.defaultsurface` / `kero.seriko.defaultsurface`。
    /// シェル単体 .nar には `ghost/master/descript.txt` が無いので大抵空になる。
    pub default_surface: BTreeMap<Slot, u32>,
}

/// 1 つの surface。
#[derive(Debug, Clone, Default)]
pub struct Surface {
    pub id: u32,
    /// `surfaceN.png` の相対パス。element0 があると使われない。
    pub file: Option<String>,
    /// `elementN` 行。**element0 があると `surfaceN.png` は破棄される**。
    pub elements: Vec<Element>,
    /// `collisionN` 行。`poke_regions` の導出元。
    pub collisions: Vec<Collision>,
}

/// `elementN,描画メソッド,ファイル名,X,Y`。
#[derive(Debug, Clone)]
pub struct Element {
    pub order: u32,
    /// `base` / `overlay` / `replace` / `interpolate` / `asis` のみ対応。
    /// blend 系は未対応で、`overlay` に倒して警告する。**enum にはしない**
    /// (variant を足すだけで対応済みに見えてしまうため、文字列のまま持つ)。
    pub method: String,
    pub file: String,
    pub x: i32,
    pub y: i32,
}

/// `collisionN,X1,Y1,X2,Y2,ID`。ID は `Head` / `Face` / `Bust` などの慣用名。
#[derive(Debug, Clone)]
pub struct Collision {
    pub name: String,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

/// `sakura.surface.alias` / `kero.surface.alias` の 1 エントリ。
#[derive(Debug, Clone)]
pub struct Alias {
    pub slot: Slot,
    pub name: String,
    /// 複数 ID はランダム選択だが、変換では先頭を採る。
    pub ids: Vec<u32>,
}

/// シェル定義を読む。解釈できない記法は警告して読み飛ばし、エラーにしない
/// (SSP 拡張は随時増えるので、未知記法での失敗は誤検出になる)。
pub fn parse(tree: &SourceTree, report: &mut Report) -> Result<ShellDef> {
    let _ = (tree, report);
    todo!("shell_def::parse")
}

/// `surface0.png` / `surface000.png` / `SURFACE0000.PNG` → `Some(0)`。
///
/// **ファイル名の surface ID は前方をいくら 0 で埋めても同じ ID** なので、
/// 文字列一致ではなく数値としてパースする。`.pna` は対象外 (別扱い)。
pub fn surface_id_from_file_name(name: &str) -> Option<u32> {
    let name = name.rsplit('/').next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("png") {
        return None;
    }
    let digits = stem.strip_prefix("surface").or_else(|| {
        stem.get(..7)
            .filter(|p| p.eq_ignore_ascii_case("surface"))
            .and_then(|_| stem.get(7..))
    })?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_padded_surface_ids_are_equal() {
        assert_eq!(surface_id_from_file_name("surface0.png"), Some(0));
        assert_eq!(surface_id_from_file_name("surface00.png"), Some(0));
        assert_eq!(surface_id_from_file_name("surface0000.png"), Some(0));
        assert_eq!(surface_id_from_file_name("surface010.png"), Some(10));
    }

    #[test]
    fn surface_file_name_is_case_insensitive() {
        assert_eq!(surface_id_from_file_name("SURFACE0.PNG"), Some(0));
        assert_eq!(surface_id_from_file_name("Surface10.Png"), Some(10));
    }

    #[test]
    fn non_surface_files_are_rejected() {
        // .pna はアルファマスクなので surface そのものではない。
        assert_eq!(surface_id_from_file_name("surface0.pna"), None);
        assert_eq!(surface_id_from_file_name("descript.txt"), None);
        assert_eq!(surface_id_from_file_name("surface.png"), None);
        assert_eq!(surface_id_from_file_name("surfaceA.png"), None);
        assert_eq!(surface_id_from_file_name("body.png"), None);
        // element 用のパーツ画像がサブディレクトリにある場合も拾わない。
        assert_eq!(surface_id_from_file_name("parts/body.png"), None);
    }

    #[test]
    fn surface_in_subdirectory_is_still_recognized() {
        assert_eq!(surface_id_from_file_name("shell/master/surface0.png"), Some(0));
    }
}
