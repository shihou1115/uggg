//! 入力 (.nar / .zip / 展開済みディレクトリ) をメモリ上のエントリ集合にする。
//!
//! ## なぜ全部メモリに載せるか
//! シェルは数 MB 程度で、変換は 1 回きりの短命プロセス。全部メモリに持てば
//! 以降のモジュールが**ファイルシステムに触らない純関数**になり、テストが
//! `SourceTree::from_entries` だけで書ける (実物の .nar を用意せずに済む)。
//!
//! ## 大文字小文字とパス区切り
//! シェルは Windows で作られるので、`surfaces.txt` の記述と実ファイル名の
//! 大小が食い違う (`element0,base,Body.png,0,0` に対し実体は `body.png`)。
//! また element のファイル名はサブディレクトリを `\` で書く。したがって
//! **参照は必ず正規化キー (小文字化 + `/` 区切り) で引く**。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

/// 入力から読み出したファイル群。キーは正規化済みの相対パス。
pub struct SourceTree {
    /// 正規化キー (小文字・`/` 区切り) → 中身。
    entries: BTreeMap<String, Vec<u8>>,
    /// 正規化キー → 元の表記。警告メッセージで元の名前を見せるために保つ。
    display_names: BTreeMap<String, String>,
    /// シェルディレクトリとして選ばれた接頭辞 (`""` かディレクトリ + `/`)。
    root: String,
}

impl SourceTree {
    /// `.nar` / `.zip` / ディレクトリのいずれかを読み込む。
    ///
    /// 拡張子では判定しない (シェルは `.zip` のまま配られることも普通にある)。
    /// ファイルならマジックバイト `PK\x03\x04` を確認する。
    pub fn from_path(path: &Path) -> Result<Self> {
        let _ = path;
        todo!("SourceTree::from_path")
    }

    /// テスト用。`(相対パス, 中身)` から直接組み立てる。
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Vec<u8>)>,
        S: AsRef<str>,
    {
        let mut tree = Self {
            entries: BTreeMap::new(),
            display_names: BTreeMap::new(),
            root: String::new(),
        };
        for (name, bytes) in entries {
            let raw = name.as_ref().to_string();
            let key = normalize(&raw);
            tree.display_names.insert(key.clone(), raw);
            tree.entries.insert(key, bytes);
        }
        tree
    }

    /// 選択済みシェルルートからの相対パスで引く。大小・パス区切りは無視される。
    pub fn get(&self, rel: &str) -> Option<&[u8]> {
        let key = format!("{}{}", self.root, normalize(rel));
        self.entries.get(&key).map(|v| v.as_slice())
    }

    /// 選択済みシェルルート直下のエントリ名 (正規化キー、ルート接頭辞を除く)。
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .keys()
            .filter_map(|k| k.strip_prefix(self.root.as_str()))
            .filter(|rel| !rel.is_empty())
    }

    /// 警告表示用に、正規化キーから元の表記を引く。
    pub fn display_name(&self, rel: &str) -> String {
        let key = format!("{}{}", self.root, normalize(rel));
        self.display_names.get(&key).cloned().unwrap_or(key)
    }

    /// シェルディレクトリを選ぶ。
    ///
    /// `install.txt` の `type` に頼らず、**「`descript.txt` があり、かつ
    /// `surface*.png` が 1 枚以上あるディレクトリ」を列挙する**のを主経路にする。
    /// install.txt が欠けた配布物や変則配置が実在するため、こちらの方が堅い。
    /// 候補が複数あるときは `shell/master` > `shell/*` > ルート の順。
    pub fn select_shell_root(&mut self) -> Result<()> {
        todo!("SourceTree::select_shell_root")
    }
}

/// 参照キーの正規化。`\` → `/`、小文字化、先頭の `./` を除去。
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_ignores_case_and_separator() {
        let tree = SourceTree::from_entries([("parts/Body.PNG", vec![1, 2, 3])]);
        // surfaces.txt には `parts\Body.png` と書かれているのが普通。
        assert_eq!(tree.get(r"parts\Body.png"), Some(&[1u8, 2, 3][..]));
        assert_eq!(tree.get("PARTS/BODY.PNG"), Some(&[1u8, 2, 3][..]));
        assert_eq!(tree.get("parts/missing.png"), None);
    }

    #[test]
    fn display_name_keeps_original_spelling() {
        let tree = SourceTree::from_entries([("Surface0.PNG", vec![])]);
        assert_eq!(tree.display_name("surface0.png"), "Surface0.PNG");
    }

    #[test]
    fn names_lists_entries() {
        let tree = SourceTree::from_entries([
            ("descript.txt", vec![]),
            ("surface0.png", vec![]),
        ]);
        let mut names: Vec<_> = tree.names().collect();
        names.sort_unstable();
        assert_eq!(names, ["descript.txt", "surface0.png"]);
    }
}
