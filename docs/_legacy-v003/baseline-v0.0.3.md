# ugga v0.0.3 — プロトタイプ完成版 仕様スナップショット

> 「伺か」コンセプトを Tauri v2 (Rust + TypeScript) で再構築したデスクトップマスコット。
> **v0.0.3 をプロトタイピング完成版**とし、以降の本開発の出発点とする。
> 本書は実装済みの現状を網羅するスナップショットであり、設計の正本は [spec.md](spec.md) /
> モジュール契約の正本は [architecture.md](architecture.md) に置く。本書はそれらへの索引と
> v0.0.3 で確定した内容・残課題のサマリを兼ねる。

---

## 1. プロダクト概要

### 1.1 コンセプト
- メイン/サブ 2 体のキャラクターが透過ウインドウ上で**掛け合い対話**するデスクトップ常駐コンパニオン。
- 「キャラがそこにいる」存在感（時間帯挨拶・放置反応・つつき/撫で・独り言・時事ネタ・ポモドーロ）。
- LLM があると豊かに話す**高度モード**、無くても辞書で**低負荷モード**として動く（API キー無しでも完全動作）。

### 1.2 v0.0.3 の絶対要件（プロトタイプで満たした）
- **無サーバ TTS**: 別アプリ/サーバを起動せず ugga 単体で声が出る（voicevox_core を libloading で実行時ロード・CPU合成）。
- **プライバシー**: 発話テキストは外部送信しない（クラウド LLM 経路は別。TTS は完全ローカル）。
- **クリック透過**: キャラの不透明部分のみ操作対象。設定パネル等は `.solid` で透過対象外。
- **オフラインで最低限動く**: LLM/VOICEVOX 未取得でもアプリは落ちず辞書で会話・声なしで継続。
- **Windows 専用**: macOS/Linux は要件外。OS固有 API 採用可。

### 1.3 対象 OS / ランタイム
- Windows 10/11 x64 のみ。WebView2 ランタイム同梱（Tauri 既定）。
- VOICEVOX 音声合成は CPU で動く（GPU 不要）。

---

## 2. 実装済み機能（v0.0.3 時点）

### 2.1 キャラクター表示
- 全 pose の `<img>`（data URL）を重ね、`visible` クラス付け替えでチラつき無く切替（[characters.ts](../src/characters.ts)）。
- 表示スケール 0.5〜2.0（Settings.display_scale）。`#stage` は `position:fixed; bottom:0` で**全スケールで下端アンカー**。
- アルファマスク方式のクリック透過: フロントが 8px グリッドで不透明セルを送る → 50ms ポーリングで `set_ignore_cursor_events` 制御
  ([alphamask.ts](../src/alphamask.ts) / [window_ctl.rs](../src-tauri/src/window_ctl.rs))。

### 2.2 対話
- **2モード**: `low`（辞書のみ、無料・オフライン）/ `advanced`（LLM 経由）。当月コスト上限超過で自動降格、API エラー連続でも一時降格。
- **プロバイダ**: OpenAI / Anthropic / Grok / LM Studio / Ollama（OpenAI 互換 API 経由）。
- **掛け合いパターン 1–4**: メイン/サブの順番・3ターン目の有無。advanced のみ可変、辞書系は常に pattern 1。
- **辞書 schema v2**（[low_mode.rs](../src-tauri/src/dialogue/low_mode.rs) / [ghosts/default/dic/main.yaml](../ghosts/default/dic/main.yaml)）:
  - `rules`（keyword マッチ + priority）、`fallback`、`random_talk`、`error_talk`、`recall_talk`、`update_talk`、`events`。
  - `events` キー: `first_boot` / `boot` / `quit` / `poke_*` / `poke_rapid` / `nade_*` / `idle` /
    `focus_start` / `focus_end` / `break_end` / `pomodoro_done`。
  - 各台詞に `when`（hour_from/hour_to/date）で時間帯・日付条件を付与可（時間帯挨拶・正月など）。
- **長期記憶**: 名前・好み等を user_profile に保存し system prompt へ注入。会話量に応じて自動要約（ContextSummary）。
- **存在感**:
  - 起動挨拶（初回は `first_boot`／2回目以降は時間帯別 `boot`）、終了挨拶 `quit`。
  - 放置反応（30 分無操作）`idle`。
  - 静音条件 = `quiet_mode` / 非可視 / フルスクリーンアプリ検知 / ポモドーロ集中中。

### 2.3 操作
| 操作 | 効果 | 実装 |
|---|---|---|
| 1クリック | 入力欄トグル | [main.ts](../src/main.ts) wireCharacterInteractions |
| 2–3クリック | つつき（部位別／辞書 events.poke_*） | poke コマンド |
| 4連打以上 | 連打反応 events.poke_rapid | poke コマンド (rapid=true) |
| ボタン無しで往復ホバー | 撫で（往復運動の指標で「通過」と区別） | [nade.ts](../src/nade.ts) → nade コマンド |
| 右クリック | 設定パネル | settings.ts |
| ドラッグ | ウインドウ移動 | startDragging |

撫で判定: 方向反転 ≥2 / 累積移動量 / 局所性（累積距離÷正味変位） / 継続時間 / cooldown / ボタン非押下、の合わせ技。

### 2.4 音声合成 TTS（**スタンドアロン化済み**）
- 合成契約: `synthesize_voice(text, slot)` の1本。声・速度・音量・エンジン種別は settings からバックエンドが解決。
- **3エンジン抽象**（[tts_engine.rs](../src-tauri/src/tts_engine.rs)）:
  | 種別 | 設定値 | 用途 |
  |---|---|---|
  | **埋め込み（既定）** | `voicevox_core` | 別アプリ/サーバ不要・CPU合成・ローカル完結 |
  | 外部接続（後方互換） | `voicevox_http` | 既存 VOICEVOX エディタの HTTP エンジンに接続 |
  | OpenAI 互換（上級） | `openai_compat` | Irodori-TTS 等。`instructions` で VoiceDesign キャプション送出（参照音声クローン未使用＝著作権リスク回避） |
- **埋め込み実装**: 公式プリビルド C API (voicevox_core 0.16.4) を **libloading で実行時ロード**
  ([voicevox_ffi.rs](../src-tauri/src/voicevox_ffi.rs))。`acceleration_mode = CPU` 強制（GPU再init時 AV を回避）。
  合成器は AppState の `Mutex<Option<VoicevoxEngine>>` に保持し再利用。
- **初回自動 DL**: 公式 download ツール (`download-windows-x64.exe` 0.16.4) を取得→規約同意プロンプトに
  stdin で `y\n` を投入→`c_api / onnxruntime / dict / models([0-9]*.vvm)` を `%APPDATA%\ugga\voicevox` に展開
  ([voicevox_download.rs](../src-tauri/src/voicevox_download.rs))。
  - GitHub レート制限対策: PAT を keyring(`provider="github_token"`)で保存し `GH_TOKEN` env で渡す。
  - DLL ロック競合対策: DL前に AppState の合成器を Drop ＋ 既存 dll を `.dll.old-N` にリネーム退避。
  - 進捗: `voicevox-download` イベント（最後の進捗をリングバッファで残し、失敗時もエラー文が画面に残る）。
- **事前 init**: boot 時・設定変更時に `spawn_voicevox_preinit` がバックグラウンドで合成器を温める（初発話のラグ消去）。
- **クレジット表示**: VVM 利用規約に基づき「VOICEVOX:話者名」を**画面下端中央に常時表示**（`.solid + pointer-events:none`）。
  TTS無効 / openai_compat / 取得失敗時は非表示。
- **再生**: 逐次キュー（main→sub が重ならない）、新発話で `interrupt`、再生振幅(AnalyserNode)で口パク駆動。
  WebView2 の自動再生ポリシー対策（`--autoplay-policy=no-user-gesture-required` + ウォッチドッグ）。

### 2.5 音声入力 STT（実装済み・UI非表示）
- OpenAI 互換 `/audio/transcriptions` で文字起こし → 入力欄に流して自動送信（[stt.rs](../src-tauri/src/stt.rs) / [stt.ts](../src/stt.ts)）。
- 現状 `STT_UI_ENABLED = false` で事実上無効（マイクボタン非表示・「音声入力」設定セクション非表示）。
  バックエンド契約・コマンドは温存しているので再有効化はフラグを true に戻すだけ。

### 2.6 周辺機能
- **時事ネタ雑談**（任意・既定オフ。[topics.rs](../src-tauri/src/topics.rs)）: RSS（既定 Google ニュース検索）から
  興味分野の見出しを取り、advanced モードの独り言ついでに織り込む。30 分未満は 30 にクランプ。
  暗い見出しのフィルタ・low モードでの発話抑制あり。**LLM 追加コストは発生しない**（独り言生成のついで）。
- **ポモドーロ**（[pomodoro.rs](../src-tauri/src/pomodoro.rs)）: focus / break / idle の状態機械。
  集中中は `should_stay_quiet` が雑談を止める＝集中モード。`pomodoro` イベントを毎秒 emit してバッジを同期。
  辞書 events: `focus_start` / `focus_end` / `break_end` / `pomodoro_done`。
- **ツール**（任意・既定オフ。advanced モード向け）:
  - 現在日時注入（「今何時?」を即答）
  - リマインダー（「N分後に教えて」→ タイマー発話・静音中も鳴る）
  - クリップボード補助（📋ボタン押下時のみ読み取り、翻訳/要約/説明）
- **更新通知**（[update_check.rs](../src-tauri/src/update_check.rs)）: `update_feed_url` の JSON を起動時に取得し、
  CARGO_PKG_VERSION より新しければ辞書 `update_talk` で一度だけ通知。
- **データエクスポート / 履歴クリア / ゴースト・シェル切替・再読込 / トレイメニュー / 自動起動**。
- **オンボーディング**: 初回起動時に nickname / 興味 / 話し方 / 時事ネタ ON/OFF をフォームで聞き取り（[onboarding.ts](../src/onboarding.ts)）。

### 2.7 口パク（リップシンク）
- pose 画像 `<name>.png` の隣に `<name>_talk.png` があれば開口フレームとして自動使用（シェル定義変更不要）。
- TTS 有効時は再生振幅（AnalyserNode・RMS閾値）に同期。無効時はタイプライター描画中に近似パクパク。

---

## 3. アーキテクチャ概観

### 3.1 全体構成
```
+---------------- Tauri 2 (Rust + WebView2) ----------------+
|  Frontend (Vanilla TS + Vite, src/)                       |
|   ├ main.ts          boot, 操作配線, クレジット表示, etc.  |
|   ├ characters.ts    pose <img> 切替・ImageData 保持        |
|   ├ alphamask.ts     8px グリッド合成 → update_alpha_mask  |
|   ├ balloon.ts       吹き出し描画（タイプライター, TTS 起点）|
|   ├ tts.ts           VoicevoxSpeaker / NoopSpeaker         |
|   ├ input.ts / stt.ts / settings.ts / onboarding.ts        |
|   └ chatlog.ts / modeorb.ts / nade.ts                      |
|                                                            |
|  Backend (Rust, src-tauri/src/)                            |
|   ├ main.rs          Tauri command/event のハブ            |
|   ├ state.rs         AppState（全状態を保持）              |
|   ├ db.rs            SQLite（companion.db）                 |
|   ├ dialogue/        low_mode / advanced / llm / banter /  |
|   │                  monologue / mod (persist_and_speak)  |
|   ├ tts_engine.rs    エンジン抽象（core/http/openai_compat) |
|   ├ voicevox_ffi.rs  libloading + C API FFI (埋め込み)     |
|   ├ voicevox_download.rs  公式 DL ツール起動               |
|   ├ voicevox.rs      HTTP エンジン                          |
|   ├ openai_tts.rs    OpenAI 互換                             |
|   ├ ghost.rs         ghost/shell マニフェスト読み込み       |
|   ├ window_ctl.rs    ウインドウ生成・クリック透過ポーリング |
|   ├ presence.rs      idle 監視・ウインドウ位置永続化       |
|   ├ pomodoro.rs / tasks.rs / topics.rs / cost.rs           |
|   ├ tray.rs / update_check.rs / secrets.rs / stt.rs        |
+------------------------------------------------------------+
        ↑↓
   ghosts/<id>/（ghost.json + dic/*.yaml） — 対話定義（YAML）
   shells/<id>/（shell.json + main/sub の画像群） — 見た目
```

### 3.2 モジュール詳細索引
v0.0.3 の architecture.md / context_index.md を参照（**2026-07-24 の docs 整理で本リポジトリからは削除**。原本はプロトタイプリポジトリ `C:\claude\ugga\docs\` と本リポジトリの git 履歴に現存）。

---

## 4. データ・契約（v0.0.3 確定スナップショット）

### 4.1 Settings（永続化フィールド）
詳細は [architecture.md §共有型](architecture.md#共有型-rust-serde--ts-srctypests--名前形を一致させる) を参照。v0.0.3 で追加されたフィールド:

- `tts_engine: "voicevox_core" | "voicevox_http" | "openai_compat"`（既定 `voicevox_core`）
- `tts_oai_base_url / tts_oai_model / tts_oai_caption_main / tts_oai_caption_sub`（openai_compat 用）

その他、旧フィールド（mode/provider/model/api_base_url/各種閾値/tts_speed/tts_volume/stt_*/tools_enabled/pomodoro_*/topics_*/display_scale 等）は serde default で後方互換。
schema_version=1 据え置き。

### 4.2 Tauri コマンド一覧（v0.0.3 時点 — 全 35）
網羅は [architecture.md §Tauri コマンド契約](architecture.md#tauri-コマンド契約フロント--バック) を参照。
v0.0.3 で追加・変更されたもの:

| カテゴリ | コマンド | 変更点 |
|---|---|---|
| TTS（変更） | `synthesize_voice(text, slot)` | 旧 `(text, speaker)` から **slot 化**。声解決はバックエンドへ |
| TTS（追加） | `voicevox_assets_ready` | 資産有無 |
| TTS（追加） | `voicevox_core_list_voices` | 埋め込み合成器の metas を平坦化（重複 style_id 除外） |
| TTS（追加） | `download_voicevox_assets(agreed, gh_token?)` | 初回自動 DL。進捗は `voicevox-download` イベント |
| TTS（追加） | `set_github_token / has_github_token / delete_github_token` | DL 用 PAT を keyring 保存（provider="github_token"） |
| 操作（追加） | `nade(target, region, xregion)` | 撫で反応 |

### 4.3 イベント一覧（バック → フロント）
| イベント | payload | 用途 |
|---|---|---|
| `dialogue` | `DialogueResponse` | バック起点の発話（ランダムトーク等） |
| `mode-changed` | `{mode, reason}` | UI 表示用 |
| `speak` | `{speaker, text}` | TTS フック（現状フロントは no-op） |
| `thinking` | `{active}` | 「思考中」表示 |
| `open-settings` | なし | トレイ → 設定パネル |
| `settings-changed` | `Settings` | バック起点の設定変更 |
| `pomodoro` | `PomodoroStatus` | 毎秒・節目で発火 |
| `voicevox-download` | `string` | v0.0.3 追加。資産DLの進捗行 |

### 4.4 辞書 schema v2
構造は [architecture.md §events](architecture.md#つつきの部位判定-フロントが押した位置キャラ要素矩形に対する相対-xy-で縦-vheadchestbody横-hleftcenterright) と
[ghosts/default/dic/main.yaml](../ghosts/default/dic/main.yaml) を実例として参照。

### 4.5 ファイル配置（実行時）
| 場所 | 用途 |
|---|---|
| `%APPDATA%\com.ugga.companion\` | tauri-plugin-log のログ |
| `%APPDATA%\ugga\companion.db` | SQLite 本体（会話ログ・要約・設定・キャッシュ） |
| `%APPDATA%\ugga\voicevox\` | 埋め込み TTS 資産（c_api / onnxruntime / dict / models / voicevox-downloader.exe） |
| keyring (`service="ugga-companion"`) | API キー、GitHub PAT |

### 4.6 配布
- **v0.0.3 インストーラ**: `src-tauri/target/release/bundle/nsis/ugga-companion_0.0.3_x64-setup.exe`
  （約 4.76 MB、SHA-256 `A4D26A549D9D1B10FD43B3F1C4F5CAE7595BF992B15EE1C4762202F8BE795D1F`）
- NSIS / `currentUser` インストールモード / 日本語ロケール
- `ghosts/` `shells/` は `bundle.resources` で同梱（インストール時に展開）。
  VOICEVOX 資産は**初回起動後に設定パネルから自動 DL**（規約同意必須）。

---

## 5. ビルド・開発フロー

| コマンド | 用途 |
|---|---|
| `npm run tauri dev` | 開発起動（Vite + cargo run） |
| `cargo check`（src-tauri/） | Rust 型検査 |
| `npx tsc --noEmit` | TypeScript 型検査 |
| `npm run tauri build` | リリースビルド（NSIS インストーラ生成） |

依存（Cargo.toml）: tauri 2 / tokio / rusqlite (bundled) / reqwest (rustls) / keyring / arboard /
serde / serde_yaml / chrono / log / tauri-plugin-(log/autostart/single-instance) /
windows-sys (Win32_*) / **libloading 0.8**（v0.0.3 で追加・voicevox_core FFI）。

`[profile.release]` は `strip=true, lto=true`。release ビルドは 2〜3 分（CI 無し・ローカル前提）。

---

## 6. 既知の制約・残課題（次フェーズへの引き継ぎ）

### 6.1 仕様上の既知の制約
- **VOICEVOX クレジット表示は常時表示・非表示不可**（VVM 利用規約準拠）。
- **撫で判定の感度**: 環境（マウス速度・スケール）で多少ばらつく。閾値は [nade.ts](../src/nade.ts) 冒頭に集約。
- **STT は UI 非表示**（`STT_UI_ENABLED=false`）。実装は温存。
- **openai_compat の参照音声クローン非対応**（著作権リスクを避けるためキャプション(VoiceDesign)のみ採用）。
- **音声モデルの版違い警告**: voicevox_core が同じ話者UUIDで `version` 差異の WARN を吐く（無害・合成は動く）。

### 6.2 環境/運用の既知の事項
- `tauri dev` のウォッチャがこの環境では `.rs` 変更時の自動再ビルドで度々停止する。停止時は手動再起動。
- 旧テスト用に手動配置したエディタ流用資産（`%APPDATA%\ugga\voicevox\engine\` 等）が残っていると重複ロードで起動ログにエラーが出る。
  自動DL分のみ（`c_api/onnxruntime/dict/models`）が正規。

### 6.3 改善余地（要件確定済みだが未実装、または更にやれること）
- list_voices / TTS テスト時の話者プレビュー（現状は「保存してテスト」UX）。
- VOICEVOX 資産のサイズ最適化（`--models-pattern` をユーザーが選べるようにする等）。
- LLM トークン使用量の月次グラフ等の可視化（コスト機能の延長線）。
- 自動起動の検証（`tauri-plugin-autostart` の OS ログイン時起動の動作確認）。
- 多言語対応（現状 日本語固定）。
- 署名付きインストーラ（コード署名証明書が必要・配布時の Defender 警告緩和）。

---

## 7. 関連ドキュメント

v0.0.3 の docs 一式（spec / architecture / context_index / quality_checklist / ai_model_routing / subagent_prompt_templates）は **2026-07-24 の docs 整理で本リポジトリからは削除**した。原本はプロトタイプリポジトリ `C:\claude\ugga\docs\` と本リポジトリの git 履歴（タグ `v0.3.0` 以前）に現存する。

| 役割 | ファイル |
|---|---|
| ゴースト定義の実例 | [../../ghosts/default/ghost.json](../../ghosts/default/ghost.json) / [../../ghosts/default/dic/main.yaml](../../ghosts/default/dic/main.yaml) |
| シェル定義の実例 | [../../shells/default/shell.json](../../shells/default/shell.json) |

---

**v0.0.3 凍結日**: 2026-06-18
**次フェーズ**: 本書をベースに新規機能・改善を加えていく。本書自体は v0.0.3 のスナップショットとして
不変扱い（差分は新ドキュメント or 各 docs/* に追記）。
