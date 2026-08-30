//! 変換の「決定」と「警告」。変換経路の全モジュールが共有する語彙。
//!
//! ## なぜ警告だけ型で、エラーは anyhow なのか
//! エラーは終端で、CLI が良いメッセージを出せれば十分 (anyhow の context で
//! 「どのファイルで失敗したか」は積める)。対して警告は**データ**として扱う:
//! 件数を数える、推測で埋めた pose を列挙する、テストで内容を検証する、といった
//! 消費者が実際にいる。だから警告だけ構造を持たせる。
//!
//! ただし警告を variant の多い enum にはしない。処置が分岐しない (全部表示する
//! だけの) 分類を enum にすると、未実装機能のための variant が増えて「対応済みに
//! 見える」だけの飾りになる。位置情報 (file / line) だけ構造化し、内容は文字列。

use std::fmt;

/// ugg の `characters` の枠。伺か側の sakura (本体) / kero (相方) に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    Main,
    Sub,
}

impl Slot {
    /// 出力ディレクトリ名。`shell.json` からの相対パスの先頭になる。
    pub fn dir(self) -> &'static str {
        match self {
            Slot::Main => "main",
            Slot::Sub => "sub",
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir())
    }
}

/// pose をその surface に割り当てた根拠。
///
/// 「推測で当てた」ことを黙って隠さないために型で持つ。伺かには表情番号の
/// 標準が無く、`sakura.surface.alias` も命名は作者の自由なので、alias 一致
/// 以外の割り当ては全部あてずっぽうである。利用者はそれを知る権利がある。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoseBasis {
    /// surface 0 (本体) / 10 (相方)。伺か本体の実装が保証する唯一の固定 ID。
    Fixed,
    /// `sakura.surface.alias` / `kero.surface.alias` の名前と一致した。
    Alias(String),
    /// CLI の `--pose` で利用者が明示指定した。
    UserSpecified,
    /// 慣例からの推測。文字列は根拠 (例: "旧慣例 surface2=驚き")。
    Guessed(String),
}

impl PoseBasis {
    /// 推測で埋めたか。変換ログで印を付けるために使う。
    pub fn is_guess(&self) -> bool {
        matches!(self, PoseBasis::Guessed(_))
    }
}

/// 「どの surface をどの pose にしたか」1 件。convert / list の両方で表示する。
#[derive(Debug, Clone)]
pub struct Decision {
    pub slot: Slot,
    pub pose: String,
    pub surface_id: u32,
    pub basis: PoseBasis,
}

/// 解釈できなかったもの・捨てたもの。変換自体は続行する。
#[derive(Debug, Clone)]
pub struct Warning {
    /// 元シェル内の相対パス。シェル全体に対する警告なら None。
    pub file: Option<String>,
    /// `file` 内の 1 始まりの行番号。行を特定できないなら None。
    pub line: Option<u32>,
    pub message: String,
}

impl Warning {
    /// ファイルにも行にも紐づかない警告。
    pub fn general(message: impl Into<String>) -> Self {
        Self { file: None, line: None, message: message.into() }
    }

    /// ファイル単位の警告。
    pub fn in_file(file: impl Into<String>, message: impl Into<String>) -> Self {
        Self { file: Some(file.into()), line: None, message: message.into() }
    }

    /// 行を特定できる警告。`surfaces.txt` の未対応記法などはこれ。
    pub fn at_line(file: impl Into<String>, line: u32, message: impl Into<String>) -> Self {
        Self { file: Some(file.into()), line: Some(line), message: message.into() }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.file, self.line) {
            (Some(file), Some(line)) => write!(f, "{file}:{line}: {}", self.message),
            (Some(file), None) => write!(f, "{file}: {}", self.message),
            (None, _) => f.write_str(&self.message),
        }
    }
}

/// 変換の過程で積み上がる記録。成功しても失敗しても利用者に見せる。
#[derive(Debug, Default)]
pub struct Report {
    pub decisions: Vec<Decision>,
    pub warnings: Vec<Warning>,
}

impl Report {
    pub fn warn(&mut self, w: Warning) {
        self.warnings.push(w);
    }

    pub fn decide(&mut self, d: Decision) {
        self.decisions.push(d);
    }

    /// 推測で埋めた pose。利用者に `--pose` での指定を促すために使う。
    pub fn guessed(&self) -> impl Iterator<Item = &Decision> {
        self.decisions.iter().filter(|d| d.basis.is_guess())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_display_includes_location() {
        let w = Warning::at_line("surfaces.txt", 142, "blend-multiply は未対応");
        assert_eq!(w.to_string(), "surfaces.txt:142: blend-multiply は未対応");
        let w = Warning::in_file("descript.txt", "charset 行がありません");
        assert_eq!(w.to_string(), "descript.txt: charset 行がありません");
        assert_eq!(Warning::general("相方なし").to_string(), "相方なし");
    }

    #[test]
    fn guessed_lists_only_guesses() {
        let mut r = Report::default();
        r.decide(Decision {
            slot: Slot::Main,
            pose: "normal".into(),
            surface_id: 0,
            basis: PoseBasis::Fixed,
        });
        r.decide(Decision {
            slot: Slot::Main,
            pose: "surprised".into(),
            surface_id: 2,
            basis: PoseBasis::Guessed("旧慣例 surface2=驚き".into()),
        });
        let guessed: Vec<_> = r.guessed().map(|d| d.pose.as_str()).collect();
        assert_eq!(guessed, ["surprised"]);
    }
}
