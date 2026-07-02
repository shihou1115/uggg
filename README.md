# ugg — デスクトップ常駐コンパニオン

「伺か」コンセプトを **Tauri v2 (Rust + TypeScript)** で再構築したデスクトップマスコット。透過ウインドウ上でメイン/サブ 2 体のキャラクターが掛け合い対話する。Windows 専用。

> v0.0.3 プロトタイプ (`C:\claude\ugga`) を経て、本リポジトリは **本開発** (`C:\claude\uggg`)。M0〜M6 完了、**v0.1.0 リリース済** (タグ `v0.1.0`)。
>
> **使い方は [取扱説明書 (docs/manual.md)](docs/manual.md) を参照。** 以下は開発者向けの情報。

## 特徴

- **二モード対話**: low (辞書、無料・オフライン) / advanced (OpenAI 互換 LLM)
- **常駐 TTS**: voicevox_core を libloading で実行時ロード — 別アプリ/別サーバ不要
- **オプション TTS**: Irodori-TTS Python サイドカー同梱 (GPU 必須・キャプション → 参照音声クローン)
- **クリック透過**: 8px グリッドのアルファマスクで形を維持したまま背面操作
- **存在感**: ランダムトーク、放置反応、ポモドーロ、撫で/つつき、静音モード、フルスクリーン自動静音
- **補助ツール**: チャットログ、エクスポート、ゴースト/シェル切替、自動起動、更新通知、時事ネタ RSS、DnD 配信、リマインダー、クリップボード補助

詳細は [docs/spec.md](docs/spec.md) と [docs/implementation-plan.md](docs/implementation-plan.md) を参照。

## ビルド・実行

要件:
- Windows 10/11 x64
- Rust 1.77+
- Node.js 20+ + npm

開発起動:

```pwsh
npm install
npm run tauri dev
```

リリースビルド (NSIS インストーラ):

```pwsh
npm run tauri build
# → src-tauri/target/release/bundle/nsis/ugg_0.1.0_x64-setup.exe
```

検証:

```pwsh
cd src-tauri && cargo check && cargo test
cd .. && npx tsc --noEmit
```

## ディレクトリ構成

```
src/                  # フロントエンド (Vanilla TypeScript + Vite)
  panels/             # 設定 / チャットログ / オンボーディング等
  dialogue/           # 吹き出し / 入力欄 / タイプライター
  tts/                # 音声合成スピーカー / クレジット / 口パク
  interaction/        # 撫で / つつき / ドラッグ
src-tauri/            # バックエンド (Rust + Tauri 2)
  src/
    commands/         # フロントから invoke されるコマンド群
    dialogue/         # low / advanced / banter / LLM クライアント
    ghost/            # 辞書 / マニフェスト / DnD 展開
    presence/         # 静音 / 放置 / ウインドウ位置
    system/           # コスト / 通知 / シークレット / 時事 / 更新
    tasks.rs          # 自発挙動 (ランダムトーク / リマインダー / 監視類)
    tools/            # M5-B: 時刻 / リマインダー / クリップボード
    tts/              # voicevox_core / Irodori サイドカー / GPU / 前処理
  python/sidecar.py   # Irodori-TTS FastAPI サイドカー (M4c)
ghosts/default/       # 同梱ゴースト (辞書 v3)
shells/default/       # 同梱シェル (画像 + 配置定義)
docs/                 # spec / architecture / implementation-plan 等
```

## データ配置

- 実行ファイル: `%LOCALAPPDATA%\ugg\`
- SQLite / TTS 資産 / リファレンス音声 / Python サイドカー: `%APPDATA%\ugg\`
  - `companion.db` (DB v5)
  - `voicevox\` (voicevox_core 0.16.4 資産、初回 DL ~数百 MB)
  - `irodori\` (Irodori-TTS Python ランタイム + モデル、オプション、初回 DL ~数 GB)
- API キー: Windows Credential Manager (keyring)

## ライセンス

ugg 本体: MIT (予定)。同梱ゴースト/シェル/辞書の権利は同梱資産の `LICENSE` に従う。VOICEVOX 音声モデルおよび Aratako/Irodori-TTS は別ライセンス (利用規約遵守、本体起動時に同意フロー)。
