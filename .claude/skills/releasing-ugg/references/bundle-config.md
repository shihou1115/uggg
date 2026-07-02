# `tauri.conf.json` の `bundle` 設定 — 各キーの意味と失敗モード

ugg の配布ビルドはこの `bundle` ブロックにほぼ全て依存する。dev 実行 (`tauri dev`) は
このブロックを一切見ないので、ここの誤りは配布版でしか露見しない。過去に実際に踏んだ
失敗モードを添えて説明する。

現在の設定 (`src-tauri/tauri.conf.json`):

```json
"bundle": {
  "active": true,
  "targets": ["nsis"],
  "icon": ["icons/icon.ico", "icons/icon.png"],
  "resources": {
    "python/sidecar.py": "python/sidecar.py",
    "../ghosts/": "ghosts/",
    "../shells/": "shells/"
  },
  "windows": {
    "nsis": {
      "installMode": "currentUser",
      "languages": ["Japanese", "English"]
    }
  }
}
```

## `active`

`true` でないと `tauri build` が **バンドル生成フェーズを丸ごとスキップ**する。
`target/release/ugg.exe` (素の実行ファイル) はできるが、インストーラは作られない。

**失敗モード (v0.1.0 で発生)**: `active: false` のまま `npm run tauri build` を実行 →
成功扱いで終わるが `bundle/nsis/` が空。「ビルドは通ったのにインストーラが無い」時は
まずここを疑う。ビルドログに `Running makensis to produce ...setup.exe` が出ていなければ
バンドルが走っていない。

## `targets`

`["nsis"]` = NSIS インストーラを作る。ugg は NSIS のみ。

## `icon`

インストーラ・実行ファイル・タスクバー・トレイに使われるアイコン。`.ico` は複数解像度を
内包できる (16/24/32/48/64/128/256 px)。

**失敗モード (v0.1.0 で発生しかけ)**: プロジェクト初期に置いた 16×16 のプレースホルダ
(`icon.ico` 104 バイト / `icon.png` 82 バイト) がそのまま出荷されかけた。本物の 256px PNG は
1KB 以上、multi-size ico は十数 KB あるので、極端に小さいファイルはプレースホルダを疑う。

アイコン生成は irodori の embeddable Python (Pillow 入り) を使い捨てスクリプトで行える:
`%APPDATA%\ugg\irodori\python\python.exe`。`Image.save(..., sizes=[(16,16),...,(256,256)])`
で multi-size ico を吐ける。

## `resources` — 最重要・即死の原因

**配布版の実行時に必要なファイルを、実行ファイルの隣に同梱するための設定。**
文字列配列でもマップでも書けるが、リネーム/ディレクトリ配置を明示できるマップ形式が安全:

```json
"resources": {
  "<ソース (tauri.conf.json からの相対)>": "<インストール先 (resource_dir 相対)>"
}
```

ugg の実行時リソースは 3 つ:

- `python/sidecar.py` — Irodori サイドカースクリプト
- `../ghosts/` — ゴースト定義 (ghost.json + 辞書 yaml)
- `../shells/` — シェル画像 (キャラの各ポーズ png)

### なぜ ghosts/shells の漏れが「起動即死」になるか

`src-tauri/src/state.rs` の `resolve_assets_dir` は起動時に以下の順で `ghosts/` + `shells/`
の両方があるディレクトリを探す:

1. `app.path().resource_dir()` (= 配布版で `bundle.resources` が展開される場所)
2. カレントディレクトリ
3. その親ディレクトリ

見つからなければ `AppState::initialize` が `Err` を返し、`main.rs` の `setup` が失敗して
**起動即 panic**する。

- **dev で顕在化しない理由**: `tauri dev` はカレントディレクトリがリポジトリルートなので、
  候補 2 で本物の `ghosts/` `shells/` が見つかってしまう。`bundle.resources` が空でも dev は
  平気で動く。
- **配布版で即死する理由**: インストール版のカレントは `%LOCALAPPDATA%\ugg\` で、そこに
  `ghosts/` `shells/` が展開されていなければ候補 1〜3 全部空振り → panic。

**教訓**: 「dev で使えている実行時ファイル」は全部 `bundle.resources` に入れる。dev で
暗黙に CWD から読めているだけのファイルは配布版で消える。新しい実行時リソース
(新カテゴリのアセット等) を足したら、必ず `bundle.resources` にも追記する。

### ビルド後の確認

```bash
ls src-tauri/target/release/ghosts src-tauri/target/release/shells
```

`default/` が両方に出ていれば OK。ここが空なら配布版が起動即死する。

## `windows.nsis`

- `installMode: "currentUser"` — 管理者権限不要、`%LOCALAPPDATA%\ugg\` にインストール。
  ugg はユーザーデータを `%APPDATA%\ugg\` に置く単一ユーザー向けアプリなので currentUser が適切。
- `languages: ["Japanese", "English"]` — インストーラ UI 言語。

## 関連: 起動時 panic の可視化

`bundle` の話ではないが密接に関わる。`main.rs` は
`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` なので、リリースビルドは
コンソールを持たず panic メッセージがどこにも出ない。`resources` 漏れのような起動時 panic は
「プロセスが一瞬で消える」だけの無言クラッシュになる。

`main.rs` の `install_panic_dialog_hook` が panic payload を MessageBoxW で表示することで、
この無言クラッシュを可読なエラーダイアログに変える。配布版の起動失敗調査はこの dialog に
依存しているので、消さないこと。
