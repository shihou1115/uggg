---
name: releasing-ugg
description: >-
  ugg (Tauri 2 デスクトップアプリ) のリリース・配布物作成の手順とチェックリスト。
  `npm run tauri build` でインストーラを作る、バージョンを上げる (0.1.0 等)、
  NSIS インストーラ / 配布 exe を用意する、リリースタグを打つ、といった作業の時は
  必ずこの skill を使うこと。「dev では動くのにインストール版が起動しない / 即死する」
  系の不具合を調べる時にも使う。過去に bundle 設定漏れで配布ビルドが繰り返し壊れた
  経緯があり、その罠を再発させないための手順が入っている。tauri build / インストーラ /
  リリース / バージョンアップ / 配布 という語が出たら、明示的に頼まれていなくても参照する。
---

# ugg リリース手順

## この skill が存在する理由

**`npm run tauri dev` で全機能が動いても、インストール版 (`ugg_x.y.z_x64-setup.exe` を
インストールしたもの) は別の理由で壊れることがある。** dev と配布版は実行時のカレント
ディレクトリ・リソースの在処・パニック時の可視性が違うため、dev の緑ランプは配布版の
保証にならない。

v0.1.0 リリースでは、G1〜G6 の実機検証を全部 PASS させたあとで、いざインストーラを配ったら
以下を立て続けに踏んだ:

1. `bundle.active: false` のままで `tauri build` がインストーラを作らず、素の exe で止まった
2. アイコンが 16×16 のプレースホルダ (104 バイト) のまま出荷されかけた
3. `bundle.resources` に `ghosts/` `shells/` が入っておらず、**インストール版が起動即死**
   (タスクバーに一瞬出て消える。トレイに常駐しない)
4. `windows_subsystem = "windows"` のせいで、その起動時 panic が完全に無言だった
   (コンソールが無いのでエラーが一切出ない)

どれも dev では絶対に顕在化しない。この手順書はそれらを最初から潰すためにある。

## 大原則

- **配布物は「実際にインストールして起動」するまで検証済みと呼ばない。** dev で動く・
  `cargo test` が緑・`tauri build` が成功、はすべて前提条件であって完了条件ではない。
  最後に必ずインストール版を起動して数十秒生存とウィンドウ表示を確認する (詳細は下記 step 6)。
- **各チェックは「なぜ」を理解して行う。** チェックリストを機械的に潰すのではなく、
  「この項目が漏れると配布版で何が起きるか」を意識する。今回の 4 罠はいずれも
  「dev には無関係だが配布版だけで効く差分」だった。

## リリースフロー

### Step 1: ビルド前の静的検証 (前提条件)

これらが緑でないなら先に進まない。ただし緑でも「リリースできる」意味ではない (上記大原則)。

```bash
cd src-tauri && cargo test        # 全テストパス
cd src-tauri && cargo check       # 警告 0 が理想
npx tsc --noEmit                  # フロント型検査 (リポジトリルートで)
```

### Step 2: バージョン三点セット + lockfile の同期

ugg のバージョンは 3 箇所に散っている。1 つでもズレると配布物の FileVersion が食い違う。

- `package.json` の `"version"`
- `src-tauri/Cargo.toml` の `version`
- `src-tauri/tauri.conf.json` の `"version"`

上 2 つを変えたら lockfile も追随させる:

```bash
npm install --package-lock-only   # package-lock.json を同期
cd src-tauri && cargo check        # Cargo.lock を同期
```

`git grep -n "旧バージョン文字列"` で README・docs・実装計画に古い版が残っていないかも見る。

### Step 3: `tauri.conf.json` の bundle 設定を確認 (最重要・罠の巣窟)

`references/bundle-config.md` に各キーの意味と失敗モードを詳述してある。**必ず読んでから**
以下を確認する:

- `bundle.active` が `true` か (false だとインストーラが生成されない)
- `bundle.resources` に**実行時に必要なファイルが全部**入っているか
  (`../ghosts/`, `../shells/`, `python/sidecar.py`)。ここが今回の即死の原因
- `bundle.icon` が本物のアイコンを指しているか (プレースホルダでないか)

アイコンの中身が本物か迷ったら実サイズを見る:

```bash
python -c "import os; print(os.path.getsize(r'src-tauri/icons/icon.png'))"
# 数百バイト以下ならプレースホルダを疑う (本物の 256px PNG は 1KB 以上ある)
```

### Step 4: 起動時 panic が可視か確認

`src-tauri/src/main.rs` は `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`
なので、**リリースビルドは panic してもコンソールに何も出ず、ただプロセスが消える。**
`main.rs` に panic hook (MessageBoxW を出す `install_panic_dialog_hook`) が入っていることを
確認する。これが無いと、配布版の起動失敗を「タスクバーに一瞬出て消えた」以上に調査できない。

もし将来この hook を消す変更が入っていたら、リリース前に必ず戻す。配布版のデバッグ可能性は
この 1 個の dialog にかかっている。

### Step 5: リリースビルド

```bash
npm run tauri build
```

成功すると 2 つ確認できる:

- `src-tauri/target/release/ugg.exe` (素の実行ファイル)
- `src-tauri/target/release/bundle/nsis/ugg_<version>_x64-setup.exe` (**これがインストーラ**)

**bundle/nsis/ にインストーラが出ていなければ Step 3 の `bundle.active` を疑う。**
ビルドログ末尾に `Running makensis to produce ...setup.exe` が出ているかも確認する。

配布に含めるべき実行時リソースがステージされたかも見る:

```bash
ls src-tauri/target/release/ghosts src-tauri/target/release/shells
# default/ が無ければ bundle.resources 漏れ → インストール版が起動即死する
```

### Step 6: インストール版で実際に起動確認 (完了条件・絶対に省略しない)

**ここを飛ばして dev の結果でリリースしたのが v0.1.0 の失敗。** 配布する当のバイナリを
インストールして起動するまでは何も保証されていない。

```powershell
# サイレント再インストール (既存インストールに上書き)
$f = "C:\claude\uggg\src-tauri\target\release\bundle\nsis\ugg_<version>_x64-setup.exe"
Start-Process -FilePath $f -ArgumentList "/S" -Wait

# インストール先にリソースが展開されたか
Get-ChildItem "$env:LOCALAPPDATA\ugg"   # ghosts / shells / python / ugg.exe が揃うこと

# インストール版を起動して生存確認
Start-Process "$env:LOCALAPPDATA\ugg\ugg.exe" -WorkingDirectory "$env:LOCALAPPDATA\ugg"
Start-Sleep 30
$p = Get-Process ugg -ErrorAction SilentlyContinue
if ($p) { "ALIVE: MainWindowTitle='$($p.MainWindowTitle)'" } else { "DEAD — 起動即死。panic dialog が出ていないか確認" }
```

`ALIVE` かつ `MainWindowTitle='ugg'` が出て、画面にキャラが表示されれば OK。`DEAD` なら
Step 4 の panic dialog が出ているはず — その本文が原因を教えてくれる。よくある原因は
Step 3 の `bundle.resources` 漏れ (ghosts/shells が配布版に無い)。

**理想はクリーンな Windows 環境 (VM や別マシン) での検証。** 開発機での再インストールは
`%APPDATA%\ugg\` の既存データを引き継ぐので、「初回インストールユーザー」の完全な再現には
ならない (DB や設定が既にある状態でのテストになる)。開発機で通ったら、余力があれば
クリーン環境でも 1 回通す。

### Step 7: 配布物メタデータの記録

`docs/release-notes/<version>.md` に以下を埋める:

```powershell
$f = "...\bundle\nsis\ugg_<version>_x64-setup.exe"
(Get-FileHash -Algorithm SHA256 $f).Hash          # SHA-256
(Get-Item $f).Length                               # バイト数
(Get-Item "...\target\release\ugg.exe").VersionInfo.FileVersion   # タグと一致するか
```

FileVersion / ProductVersion が Step 2 で決めたバージョンと一致することを確認する
(食い違っていたら Step 2 の同期漏れ)。

### Step 8: コミットとタグ

- 変更をコミット (version 三点セット + lockfiles + release-notes + アイコン等)
- `git tag v<version>`。ビルドをやり直してバイナリが変わったら `git tag -f` で打ち直し、
  release-notes の SHA-256 も新しい値に更新する (バイナリと記録の SHA がズレると無意味)
- コミットに意図しないファイル (CLAUDE.md 等) が混ざっていないか `git show --stat` で確認

## 完了判定

以下が全部 ✅ になって初めて「リリースできた」と言える:

- [ ] Step 1 の静的検証が緑
- [ ] バージョン三点セット + lockfile が一致
- [ ] `bundle.active: true` / `bundle.resources` 完備 / 本物アイコン
- [ ] `main.rs` に panic dialog hook がある
- [ ] `bundle/nsis/` にインストーラが生成された
- [ ] **インストール版を起動 → 30 秒生存 + ウィンドウ表示を確認した**
- [ ] SHA-256 / FileVersion を release-notes に記録
- [ ] コミット + タグ (バイナリを作り直したら SHA も打ち直し)

## 関連ドキュメント

- `docs/test-plan.md` §7 — リリース前手順の正本 (この skill はその実務的補足)
- `docs/quality_checklist.md` — 実機で人が確認する項目 (M4c G1〜G6 等)
- `docs/release-notes/<version>.md` — 各リリースの記録先
- `references/bundle-config.md` — `tauri.conf.json` bundle 各キーの意味と失敗モード
