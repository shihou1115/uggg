# アーキテクチャ設計書（実装契約）

この文書は Fable5 が管理する**実装契約**。コマンド名・イベント名・型・スキーマを変更する場合は必ず Fable5 の判断を仰ぐこと。

## ディレクトリ構造

```
ugga/
├── index.html / src/ (フロントエンド TS) / dist/
├── src-tauri/        (Rust バックエンド)
│   ├── Cargo.toml / tauri.conf.json / build.rs
│   ├── capabilities/default.json
│   └── src/
│       ├── main.rs        … エントリ。State 初期化、コマンド登録、タイマー起動、apply_settings/apply_safe_mode
│       ├── state.rs       … AppState 定義
│       ├── window_ctl.rs  … ウインドウ生成抽象化・セーフモード・クリック透過ポーリング・位置復元
│       ├── secrets.rs     … keyring ラッパー
│       ├── db.rs          … SQLite (rusqlite) ラッパー
│       ├── ghost.rs       … ghost/shell マニフェスト読み込み・schema_version 互換処理
│       ├── cost.rs        … API 使用量記録・月次集計・上限リミッター
│       ├── tasks.rs       … バックグラウンドキュー（busy ゲート付き低優先度ワーカー）
│       ├── tray.rs        … システムトレイ（表示切替・モード・静音・セーフモード・終了）
│       ├── presence.rs    … 静音判定（quiet/フルスクリーン/非表示）・idle 監視・位置永続化
│       ├── update_check.rs… 更新通知（update_feed_url の JSON を起動時確認）
│       ├── tts.rs               … TtsSink トレイト（発話確定箇所のフック点）
│       ├── tts_engine.rs        … TTSエンジン抽象（VoicevoxCore/VoicevoxHttp/OpenAiCompat の振り分け）
│       ├── voicevox.rs          … 外部 VOICEVOX エンジン（HTTP）への合成・/speakers
│       ├── voicevox_ffi.rs      … 埋め込み voicevox_core C API への libloading FFI（無サーバ合成）
│       ├── voicevox_download.rs … 公式ダウンローダ取得 + 規約同意自動応答 + 進捗イベント
│       ├── openai_tts.rs        … OpenAI 互換 /v1/audio/speech（VoiceDesign は instructions で送信）
│       └── dialogue/
│           ├── mod.rs        … DialogueEngine（モード制御・コンテキスト移行）
│           ├── low_mode.rs   … YAML 辞書マッチング
│           ├── advanced.rs   … LLM 呼び出し・JSON パース・フォールバック
│           ├── llm.rs        … LlmProvider トレイト + OpenAI/Anthropic/Ollama 実装
│           └── monologue.rs  … 独り言キャッシュ管理・ランダムトークタイマー
├── ghosts/<id>/ghost.json, dic/*.yaml
├── shells/<id>/shell.json, main/*.png, sub/*.png
└── docs/
```

ghosts/ と shells/ は開発中はプロジェクトルート直下（実行ファイルからの相対 `../../ghosts` ではなく、
`std::env::current_dir()` 起点 + 環境変数 `UGGA_DATA_DIR` で上書き可とする）。
SQLite DB は `dirs::data_dir()/ugga/companion.db`。

## 定義ファイルスキーマ（ステップ1 成果物）

実例が正: [ghosts/default/ghost.json](../ghosts/default/ghost.json), [shells/default/shell.json](../shells/default/shell.json), [ghosts/default/dic/main.yaml](../ghosts/default/dic/main.yaml)

- すべて `schema_version: 1`（整数）必須。
- 読み込み時の互換処理: `schema_version` が現行 `CURRENT_*_SCHEMA`（定数）より大きい → エラー（クラッシュせず警告ログ + デフォルトゴーストへフォールバック）。小さい → バージョンごとの migrate 関数を通して現行構造体に変換（v1 が初版なので現状はパススルー、関数の枠だけ用意）。

### dic/*.yaml 構造

現行 `CURRENT_DIC_SCHEMA = 2`。v2 で `events` / `update_talk` を追加（v1 は全フィールド省略可のため
そのまま読める。migrate はパススルー）。

```yaml
schema_version: 2
rules:               # キーワードマッチ規則
  - id: greeting
    keywords: ["こんにちは", "こんにちわ", "やあ"]
    priority: 10     # 大きいほど優先。同点はランダム
    responses:       # 複数あればランダム選択
      - main: { text: "...", pose: happy }
        sub:  { text: "...", pose: normal }
fallback:            # マッチなし時の応答（複数からランダム）
  - main: {...}
    sub: {...}
random_talk:         # 低負荷モードのランダムトーク兼、キャッシュ枯渇時フォールバック
  - main: {...}
    sub: {...}
error_talk:          # API エラー時専用台詞（spec 3.3）
  - main: {...}
    sub: {...}
recall_talk:         # 要約反映用。{summary} プレースホルダーを要約文で置換
  - main: { text: "そういえばさっき、{summary}って話してたよね", pose: normal }
    sub:  {...}
update_talk:         # 更新通知用（v2）。{version} を新バージョン番号で置換
  - main: {...}
    sub: {...}
events:              # イベント台詞（v2）。「イベント名 → 台詞リスト」のマップ。各キーは省略可。
                     # Rust 側は HashMap<String, Vec<EventResponse>>（low_mode.rs）なので、
                     # ゴーストは任意のキー（地域別つつき等）を自由に追加できる。
  first_boot: [...]  # 初回起動オンボーディング（無ければ boot にフォールバック）
  boot: [...]        # 起動挨拶
  quit: [...]        # 終了挨拶（トレイ「終了」時に再生してから exit）
  poke_main: [...]   # つつき（ダブル〜3 連クリック）。地域別が無いときのフォールバック
  poke_sub: [...]
  poke_main_head: [...]   # 頭をつついたとき（縦。無ければ poke_main へ）
  poke_main_chest: [...]  # 胸部をつついたとき（縦）
  poke_main_left: [...]   # 左側をつついたとき（横。任意。X 閾値を設定したシェル向け）
  poke_main_right: [...]  # 右側（横）。poke_main_head_left のような 2D 結合キーも可
  poke_sub_head: [...]    # クロの頭（縦。無ければ poke_sub へ）
  poke_sub_chest: [...]   # クロの胸部（縦）
  poke_rapid: [...]  # 連打（4 回以上。無ければ地域別→poke_* にフォールバック）
  nade_main: [...]   # 撫で（キャラ上をボタン無しで往復）。地域別が無いときのフォールバック
  nade_sub: [...]
  nade_main_head: [...]   # 頭を撫でたとき（縦。無ければ nade_main へ）。chest/left/right も poke と同形
  nade_sub_head: [...]    # クロの頭を撫でたとき（縦。無ければ nade_sub へ）
  idle: [...]        # 30 分無操作で 1 回（操作でリセット）
  focus_start: [...] # ポモドーロ集中開始（v2 追加）
  focus_end: [...]   # 集中終了→休憩へ
  break_end: [...]   # 休憩終了→次の集中へ
  pomodoro_done: [...]# 全ラウンド完了
```
つつきの部位判定: フロントが押した位置（キャラ要素矩形に対する相対 X/Y）で縦 v(head/chest/body)・横 h(left/center/right)
に分類し `poke` へ渡す。**しきい値はシェル作者が shell.json の各キャラに
`poke_regions: { head_max, chest_max, left_max, right_min }` で指定可能**:
- 縦: `ny < head_max`→head / `< chest_max`→chest / それ以外→body（既定 0.40 / 0.62）
- 横: `nx < left_max`→left / `nx >= right_min`→right / それ以外→center（既定 left_max 0 / right_min 1 ＝常に center＝横無効）
イベント探索順（poke コマンド）: poke_<t>_<v>_<h> → poke_<t>_<v> → poke_<t>_<h> → poke_<t>。
center や未定義は自然にフォールバックされる（横を使わないゴーストは縦のみで従来どおり）。
ShellCharacterDef → BootPayload の ShellCharacter.poke_regions を経て CharacterView.regionAt が使う。
バックエンドで 0<=head<chest<=1 / 0<=left<=right<=1 に sanitize。

撫で（nade コマンド）も同じ `regionAt`（ジェスチャ重心の位置で判定）と同じ部位しきい値を流用する。
探索順は nade_<t>_<v>_<h> → nade_<t>_<v> → nade_<t>_<h> → nade_<t>（連打概念は無い）。
撫でと「ただのカーソル通過」の区別はフロント（`src/nade.ts`）が担う: クリック透過により mousemove は
実質キャラ不透明部分の上だけ届くことを前提に、①方向反転回数 ②累積移動量 ③局所性（累積距離/正味変位）
④最低継続時間 を満たしたときだけ発火し、ボタン押下中（クリック・ドラッグ）は除外、発火後は cooldown を挟む。

events の各エントリは `main`/`sub` に加えて任意の `when` 条件を持てる:
`when: { hour_from: 18, hour_to: 5 }`（ローカル時刻の半開区間。from > to は深夜跨ぎ）、
`when: { date: "01-01" }`（MM-DD）。選択は「条件一致のうち特異度最大（date 2 + hour 1）の
候補群からランダム」。片方だけの hour 指定は設定ミスとして除外する。

低負荷モードのマッチング: ユーザー入力に対し rules の keywords を部分一致で走査し、priority 最大の規則の responses から 1 つ選ぶ。マッチなし → 直近の context_summary があり、入力が要約のキーワード（summary_keywords カラム）に部分一致すれば recall_talk（{summary} 置換）。それも無ければ fallback。

## DB スキーマ（SQLite）

```sql
CREATE TABLE IF NOT EXISTS conversation_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,                -- ISO8601 UTC
  mode TEXT NOT NULL,              -- 'low' | 'advanced'
  role TEXT NOT NULL,              -- 'user' | 'main' | 'sub' | 'system'
  text TEXT NOT NULL,
  pose TEXT
);
CREATE TABLE IF NOT EXISTS context_summary (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  summary TEXT NOT NULL,
  summary_keywords TEXT NOT NULL DEFAULT ''   -- カンマ区切り
);
CREATE TABLE IF NOT EXISTS monologue_cache (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  payload TEXT NOT NULL,           -- DialogueResponse の main/sub 部分の JSON
  used INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS api_usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  provider TEXT NOT NULL, model TEXT NOT NULL,
  prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL,
  est_cost_usd REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS app_settings (
  key TEXT PRIMARY KEY, value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS user_profile (   -- ユーザー長期記憶
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  content TEXT NOT NULL UNIQUE              -- 上限 50 件（超過時は古い順に削除）
);
```

app_settings の予約キー: `settings`（Settings JSON）/ `window_pos`（`{"x":..,"y":..}` 物理 px）/
`first_boot_done` / `update_notified_version`。

db.rs: `Db` 構造体が `Arc<Mutex<rusqlite::Connection>>` を保持（std::sync::Mutex）。
すべての公開メソッドは `async fn` とし、内部で `tokio::task::spawn_blocking` + Mutex lock で実行。
CRUD: ログ追加/直近 N 件取得、要約追加/最新取得、独り言 push/pop（未使用最古を used=1 に更新して返す）/未使用数 count、usage 追加/当月合計、settings get/set。

## 共有型（Rust: serde / TS: src/types.ts — 名前・形を一致させる）

```ts
type Pose = string;                       // shell.json の poses キーに依存
interface CharacterLine { text: string; pose: Pose; }
interface DialogueResponse {
  main: CharacterLine;
  sub: CharacterLine;
  mode: "low" | "advanced";
  source: "llm" | "dictionary" | "cache" | "error_fallback" | "recall";
  pattern?: number;        // 掛け合いパターン 1-4（既定 1）。後方互換のため optional/serde default=1
  extra?: CharacterLine | null;  // パターン3/4の3ターン目（最初の話者の2言目）。無ければ null
}
interface Settings {
  schema_version: number;                 // 1
  mode: "low" | "advanced";
  provider: "openai" | "anthropic" | "grok" | "lmstudio" | "ollama";
  model: string;
  api_base_url: string | null;            // null=プロバイダ既定。Grok 等 OpenAI 互換はこれで対応
  monthly_limit_usd: number;
  random_talk_interval_sec: number;       // 0 で無効
  safe_mode: boolean;
  ghost_id: string;
  shell_id: string;
  autostart: boolean;                     // OS ログイン時の自動起動（serde default: false）
  quiet_mode: boolean;                    // 静音モード（default: false）
  auto_quiet_fullscreen: boolean;         // 全画面アプリ中は自動静音（default: true）
  update_feed_url: string | null;         // 更新情報 JSON の URL（default: null=無効）
  display_scale: number;                  // 表示スケール 0.5〜2.0（serde default: 1.0。set_settings で clamp）
  topics_enabled: boolean;                // 時事ネタ雑談（web取得）の有効化（serde default: false）
  topics_interval_min: number;            // web取得間隔（分。serde default: 180。enabled 時 30 未満は 30 に clamp）
  topics_feed_url: string | null;         // 取得元RSSテンプレ（{query} を含む）。null=内蔵 Google ニュース検索RSS
  tts_enabled: boolean;                   // 音声読み上げの有効化（serde default: false）
  tts_engine: "voicevox_core"|"voicevox_http"|"openai_compat"; // TTSエンジン選択（serde default: "voicevox_core"=埋め込み・無サーバ）
  tts_base_url: string | null;            // voicevox_http の接続先。null=既定 http://localhost:50021
  tts_speaker_main: number;               // メインキャラの話者(style)ID（serde default: 2）
  tts_speaker_sub: number;                // サブキャラの話者(style)ID（serde default: 3）
  tts_speed: number;                      // 話速 speedScale（serde default: 1.0。set_settings で 0.5〜2.0 clamp）
  tts_volume: number;                     // 音量 volumeScale（serde default: 1.0。0.0〜2.0 clamp）
  tts_oai_base_url: string | null;        // openai_compat の接続先。null=既定 http://localhost:8880/v1
  tts_oai_model: string;                  // openai_compat のモデル名（serde default: "irodori-tts-500m-v2-voicedesign"）
  tts_oai_caption_main: string;           // openai_compat メインの VoiceDesign キャプション（instructions に送る）
  tts_oai_caption_sub: string;            // openai_compat サブの VoiceDesign キャプション（同上）
  stt_enabled: boolean;                   // 音声入力（STT）の有効化（serde default: false）
  stt_base_url: string | null;            // OpenAI互換 /audio/transcriptions のベースURL。null=既定 https://api.openai.com/v1
  stt_model: string;                      // 文字起こしモデル（serde default: "whisper-1"）
  stt_language: string;                   // 言語ヒント（serde default: "ja"）
  tools_enabled: boolean;                 // ツール（現在日時注入・リマインダー・クリップボード補助）の有効化（serde default: false）
  pomodoro_work_min: number;              // ポモドーロの集中（分。serde default: 25）
  pomodoro_break_min: number;             // ポモドーロの休憩（分。serde default: 5）
  pomodoro_rounds: number;                // 集中の回数（serde default: 4）
}
interface PomodoroStatus { phase: "focus" | "break" | "idle"; remaining_sec: number; round: number; rounds: number; }
interface InterestTopic { id: number; topic: string; enabled: boolean; }
interface VoiceOption { id: number; name: string; }   // VOICEVOX 話者一覧（id=styleID, name="キャラ名(スタイル名)"）
// 後方互換: 新フィールドはすべて serde default 付きで追加（schema_version は据え置き）
interface ProfileEntry { id: number; content: string; }
interface LogEntry { id: number; ts: string; mode: string; role: string; text: string; pose: string | null; }
interface UsageSummary { month_usd: number; limit_usd: number; limited: boolean; }
interface ShellCharacter {
  poses: Record<string, string>;          // pose名 → data URL (バックエンドが base64 化して返す)
  default_pose: string;
  width: number; height: number;
}
interface BootPayload {
  settings: Settings;
  ghost_name: string;
  characters: { main: ShellCharacter; sub: ShellCharacter };
  recent_log: LogEntry[];                 // 直近 20 件
  pose_names: string[];                   // main/sub 共通の利用可能 pose 一覧
  onboarded: boolean;                     // profile_onboarded フラグ。false なら初回オンボーディングを表示
}
```

## Tauri コマンド契約（フロント → バック）

| コマンド | 引数 | 戻り値 | 備考 |
|---|---|---|---|
| `get_boot_payload` | なし | `BootPayload` | 画像は data URL（プリロード兼チラつき防止の源泉） |
| `send_user_message` | `text: String` | `DialogueResponse` | モード判定・フォールバック込み。処理中は busy ゲート ON |
| `set_settings` | `settings: Settings` | `()` | mode が adv→low に変わったら要約タスクをキューへ |
| `get_settings` | なし | `Settings` | |
| `set_api_key` | `provider, key: String` | `()` | keyring 保存。service="ugga-companion", user=provider |
| `has_api_key` | `provider: String` | `bool` | |
| `delete_api_key` | `provider: String` | `()` | |
| `get_usage` | なし | `UsageSummary` | |
| `get_chat_log` | `limit: u32` | `Vec<LogEntry>` | 新しい順 |
| `update_alpha_mask` | `mask: AlphaMask` | `()` | 下記クリック透過参照 |
| `set_safe_mode` | `enabled: bool` | `()` | 設定保存 + ウインドウ再生成 |
| `frontend_ready` | なし | `()` | boot 完了通知。起動挨拶（初回は first_boot）+ 更新チェック起動。greeted ガードで再ロード時は no-op |
| `poke` | `target: "main"\|"sub", region(縦): "head"\|"chest"\|"body", xregion(横): "left"\|"center"\|"right", rapid: bool` | `DialogueResponse \| null` | つつき反応。探索順 (rapid なら poke_rapid →) poke_<t>_<v>_<h> → poke_<t>_<v> → poke_<t>_<h> → poke_<t>。無ければ null |
| `nade` | `target: "main"\|"sub", region(縦): "head"\|"chest"\|"body", xregion(横): "left"\|"center"\|"right"` | `DialogueResponse \| null` | 撫で反応。探索順 nade_<t>_<v>_<h> → nade_<t>_<v> → nade_<t>_<h> → nade_<t>。撫で/通過の判定はフロント（nade.ts）。無ければ null |
| `get_profile` | なし | `ProfileEntry[]` | 長期記憶一覧（古い順） |
| `add_profile` | `content: String` | `ProfileEntry[]` | 追加後の一覧を返す |
| `delete_profile` | `id: i64` | `ProfileEntry[]` | 削除後の一覧を返す |
| `clear_history` | `include_profile: bool` | `()` | 会話ログ・要約・独り言キャッシュを全削除（true なら記憶も） |
| `list_ghosts` | なし | `Vec<{id, name}>` | ghosts/ を走査。読めない定義は警告ログでスキップ |
| `list_shells` | なし | `Vec<{id, name}>` | shells/ を走査。同上 |
| `reload_assets` | なし | `()` | 現在の ghost/shell/dict をディスクから再読込して AppState を差し替え。フロントは続けて get_boot_payload で再構築 |
| `open_log_dir` | なし | `()` | app_log_dir を OS のファイラで開く（explorer / open / xdg-open） |
| `export_data` | `include_profile: bool` | `String` | 会話ログ・要約・(任意で記憶)・api_usage を JSON でダウンロードフォルダへ書き出し、保存パスを返す |
| `get_interests` | なし | `InterestTopic[]` | 興味分野の一覧（追加順） |
| `set_interests` | `topics: String[]` | `InterestTopic[]` | 興味分野を一括置換（重複・空白除去、最大 20 件）。置換後の一覧を返す |
| `complete_onboarding` | `nickname: String\|null, interests: String[], talk_style: String\|null, topics_enabled: bool` | `()` | 初回設定の確定。nickname/talk_style を user_profile へ、interests を interest_topics へ、topics_enabled を settings へ反映し、`profile_onboarded` を立てる。topics_enabled なら即時 fetch を起動 |
| `skip_onboarding` | なし | `()` | オンボーディングをスキップ（`profile_onboarded` を立てるだけ） |
| `fetch_topics_now` | なし | `()` | 時事ネタを今すぐ取得（設定の「今すぐ取得」用。topics_enabled でなくても手動実行可） |
| `synthesize_voice` | `text: String, slot: "main"\|"sub"` | `String` | 合成して WAV を base64 で返す。slot に応じて settings から声・速度・音量・エンジン種別をバックエンドが解決（フロントは引数で何も渡さない）。`tts_engine == "voicevox_core"` のときは埋め込み合成（`spawn_blocking` で実行・AppState 保持の合成器を遅延 init/再利用）、その他は HTTP 合成。失敗（未起動/未DL）は Err でフロントは黙殺 |
| `list_voices` | なし | `VoiceOption[]` | 外部 VOICEVOX エンジン (`tts_engine == "voicevox_http"`) の `/speakers` を取得し style を平坦化（埋め込み用は `voicevox_core_list_voices`） |
| `voicevox_assets_ready` | なし | `bool` | 埋め込みエンジンの資産（dll/onnxruntime/辞書/`.vvm`）が `dirs::data_dir()/ugga/voicevox` 配下に揃っているか |
| `voicevox_core_list_voices` | なし | `VoiceOption[]` | 埋め込み合成器の `voicevox_synthesizer_create_metas_json` を平坦化（同一 style_id は重複除外）。未init なら必要時に init する |
| `download_voicevox_assets` | `agreed: bool, gh_token: String\|null` | `()` | 公式ダウンローダ(`download-windows-x64.exe` 0.16.4)を取得→`-o <asset_dir> --c-api-version 0.16.4 --devices cpu --exclude additional-libraries --models-pattern [0-9]*.vvm` で実行。VOICEVOX 利用規約に対話的同意するため stdin に `y\n` を投入。`agreed=false` は即エラー。`gh_token` が空のときは keyring 保存値を使う（GitHub API レート制限緩和）。DL 前に AppState の合成器を破棄＋既存 dll を `.dll.old-N` に退避（Windows のファイルロック対策）。進捗は `voicevox-download` イベント |
| `set_github_token` / `has_github_token` / `delete_github_token` | `token: String` / なし / なし | `()` / `bool` / `()` | DL 用 GitHub PAT を keyring に保存/有無確認/削除（provider 名は `"github_token"`、既存 `set_api_key` 等とは別系統） |
| `transcribe_audio` | `audio_b64: String, mime: String` | `String` | 録音音声を OpenAI互換 /audio/transcriptions で文字起こしし本文を返す。key は keyring "stt"（base_url がローカルなら任意） |
| `send_with_clipboard` | `text: String` | `DialogueResponse` | クリップボード本文を「# クリップボードの内容」として注入したうえで通常応答。tools_enabled かつ advanced 時のみ注入（それ以外は send_user_message と同等） |
| `read_clipboard` | なし | `String` | クリップボードのテキストを返す（プレビュー用。2000字で切詰め）。空/非テキストは空文字 |

（STT の API キーは既存の `set_api_key`/`has_api_key`/`delete_api_key` を provider 名 `"stt"` で再利用する。）

エラーはすべて `Result<T, String>`（ユーザー提示可能な日本語メッセージ）。
設定適用の本体は `apply_settings` / `apply_safe_mode`（main.rs）に集約し、
設定パネル（コマンド）とトレイの両方から同じ経路を通す。

## イベント契約（バック → フロント、`emit`）

| イベント名 | payload | 用途 |
|---|---|---|
| `dialogue` | `DialogueResponse` | ランダムトーク等、バックエンド起点の発話 |
| `mode-changed` | `{ mode: "low"\|"advanced", reason: "user"\|"api_error"\|"cost_limit"\|"recovered" }` | UI 表示用 |
| `speak` | `{ speaker: "main"\|"sub", text: string }` | **TTS フック**。現状フロントは no-op で受信のみ |
| `thinking` | `{ active: boolean }` | 「思考中」表示の ON/OFF |
| `open-settings` | なし | トレイ「設定を開く」→ フロントが設定パネルを開く |
| `settings-changed` | `Settings` | トレイ等バックエンド起点の設定変更をパネルへ反映 |
| `voicevox-download` | `string` | 埋め込みTTS資産DLの進捗行（ダウンローダの stderr を ANSI 除去・UTF-8 で読んで1行ずつ emit）。終了時のみ `"__done__"` を emit。UI はリングバッファで保持し失敗時はメッセージを残す |

## クリック透過（spec 3.1 — 設計確定済み、変更不可）

1. フロントは現在の表示状態（キャラ画像のアルファ + 吹き出し/パネル等 `.solid` 要素の矩形）から
   ウインドウ全体の **不透明グリッド**（セル 8px 相当、`cols×rows`、各セル 0/1）を合成し、
   pose 変更・UI 表示変更・リサイズのたびに `update_alpha_mask({cols, rows, data})` を送る。
   `data` は 1 セル 1 バイト (0|1) を base64 化した文字列。
2. Rust 側 window_ctl.rs が 50ms 間隔の tokio タスクでグローバルカーソル位置
   （`window.cursor_position()`）とウインドウ矩形を取得し、カーソル下のセルを判定:
   - 不透明セル → `set_ignore_cursor_events(false)`
   - 透明セル / ウインドウ外 → `set_ignore_cursor_events(true)`
   - 状態が変わるときのみ呼ぶ（毎 tick 呼ばない）
   - セーフモード時はポーリング停止・常に false
   - 理由: ignore 中はフロントに mousemove が届かないため、フロント主導では再入検知不能。
3. ウインドウ生成は window_ctl.rs の `create_main_window(app, safe_mode)` に集約:
   - 通常: transparent, decorations=false, always_on_top, shadow=false, resizable=false
   - セーフモード: 不透過・decorations=true・click-through 無効
   - macOS 透過差異は tauri.conf.json `app.macOSPrivateApi: true` で吸収（抽象レイヤーはこの関数）
   - セーフモード切替の再生成は「新ウインドウを先に生成 → 旧を destroy()」。ラベルは "main" / "main-alt" を交互に使用
     （同一ラベル即時再生成の衝突と、全ウインドウ消滅によるアプリ終了の両方を回避するため。capabilities は両ラベルを登録）。
     旧の破棄に close() を使わないこと: CloseRequested を経由して「閉じる→非表示」変換に拾われる
   - ウインドウの CloseRequested は prevent_close + hide に変換（終了はトレイのみ）。位置は Moved イベントで
     AppState に記録し、presence の 3 秒デバウンスタスクが app_settings("window_pos") へ保存。
     起動時に復元し、モニタ外（重なり 50px 未満）なら center() にフォールバック。
     セーフモード切替時は旧ウインドウの現在位置を優先して引き継ぐ

## LLM シングルコール（spec 2.2 / 2.4）

システムプロンプト（advanced.rs で構築）に含める: ghost.json の persona、利用可能 pose 一覧、
**1 セリフ最大 60 文字**の制限、直近対話ログ（最大 10 件）+ 最新 context_summary、
そして次の出力 JSON 仕様:

```json
{
  "main": {"text": "...", "pose": "happy"},
  "sub":  {"text": "...", "pose": "normal"},
  "remember": ["ユーザーの呼び名はシホ"],
  "monologues": [
    {"main": {"text": "...", "pose": "..."}, "sub": {"text": "...", "pose": "..."}}
  ]
}
```

- `remember` = ユーザー長期記憶の自動抽出（Chat タスクのみ）。名前・好み・環境などの新事実を
  短文で返させ、user_profile へ保存（1 件 80 文字で切り詰め、UNIQUE で重複無視、上限 50 件）。
  json_schema では required 指定（grammar 制約下の省略対策。空配列可）。
- プロンプトには user_profile の直近 20 件を「# ユーザーについて覚えていること」として常時注入
  （Chat / MonologueStock 共通。advanced.rs `load_profile_facts`）。

- `monologues`（2 件依頼）= 独り言ストックの「ついで生成」。受信後 monologue_cache へ保存。
  **ついで生成はクラウドプロバイダのみ**。ローカル（lmstudio/ollama）はコスト 0 で節約の意味がなく、
  小型モデルは同時生成で本題の応答品質が落ちるため、単独の RestockMonologues 呼び出しに分離する
  （mod.rs の piggyback 判定）。
- プロンプトは advanced.rs `build_system_prompt(ghost, poses, recent, summary, &PromptTask)` で構築。
  PromptTask は Chat{monologues} / MonologueStock。小型ローカル LLM 対策として
  「必ず日本語」明記・履歴をキャラ名表記・見出しによる構造化・few-shot 例 1 つ・temperature 0.7 を採用。
- 独り言補充は失敗後 5 分間バックオフ（tasks.rs）。接続障害中の失敗リクエスト連打を防ぐ。
- パース: 応答からコードフェンス除去 → 最初の `{`〜最後の `}` を抽出して serde_json。失敗時は error_talk フォールバック（リトライしない）。
- pose が pose 一覧に無い場合は default_pose に矯正。text は 120 文字で切り詰め。

### LlmProvider トレイト (llm.rs)
```rust
pub struct LlmRequest {
    pub system: String,
    pub messages: Vec<(String /*role*/, String)>,
    pub max_tokens: u32,
    pub schema: Option<serde_json::Value>, // 期待出力の JSON Schema（LM Studio の json_schema 用。None=自由文）
}
pub struct LlmUsage { pub prompt_tokens: u32, pub completion_tokens: u32 }
pub struct LlmResult { pub text: String, pub usage: LlmUsage }
#[derive(Debug, thiserror::Error)] pub enum LlmError { Unauthorized, RateLimited, Timeout, Network(String), Parse(String), Other(String) }
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(&self, req: LlmRequest) -> Result<LlmResult, LlmError>;
}
```
実装 3 種: OpenAiProvider（api_base_url 上書き可 / response_format json_object）、AnthropicProvider（x-api-key, anthropic-version: 2023-06-01）、OllamaProvider（http://localhost:11434, format:"json", キー不要）。
reqwest timeout: クラウド 60 秒（推論系モデルの思考時間を考慮）/ ローカル（lmstudio, ollama）300 秒
（JIT ロードや大型モデルの推論は分単位かかる）。HTTP 401→Unauthorized, 429→RateLimited。

クラウド推論系モデルのパラメータ非互換（OpenAI o系/gpt-5系: max_tokens 拒否 → max_completion_tokens 要求、
temperature 既定値以外拒否）への対策: モデル名分岐ではなく、400 応答の本文を見て自動修正リトライ（最大2回。
max_tokens 拒否→キー差し替え / temperature 拒否→省略）。将来モデルにも自動追従する。
json_object 利用時は「プロンプトに JSON という語が必須」（OpenAI）— build_system_prompt と要約プロンプトは
全呼び出しでこの語を含むこと（現状すべて満たす。プロンプト改稿時は維持必須）。

response_format の使い分け（OpenAiProvider）:
- クラウド（openai/grok）: `{"type":"json_object"}`（従来どおり）
- lmstudio: json_object 非対応バージョンがある（"must be 'json_schema' or 'text'" を返す）ため、
  LlmRequest.schema があれば `{"type":"json_schema","json_schema":{...}}`、無ければ response_format なし（自由文）。
  スキーマは advanced.rs の `dialogue_schema()` / `monologues_schema()`、要約は tasks.rs にインライン定義。
  grammar 制約下では optional プロパティが省略されがちなので、必要なフィールドは required にする。

### プロバイダ対応表（build_provider）

| provider 値 | 実体 | 既定 base_url | API キー |
|---|---|---|---|
| openai | OpenAiProvider | https://api.openai.com/v1 | 必須 |
| anthropic | AnthropicProvider | https://api.anthropic.com | 必須 |
| grok | OpenAiProvider | https://api.x.ai/v1 | 必須 |
| lmstudio | OpenAiProvider | http://localhost:1234/v1 | 不要（無ければ "lm-studio" を送る） |

lmstudio でモデル名が空のときは必ず `model` を解決して送る（model 無しのリクエストは LM Studio が
「Unexpected endpoint or method」を HTTP 200 で返すため）。解決順序:
1. `/api/v0/models` の `state == "loaded"` な LLM/VLM（JIT ロード有効時、/v1/models はダウンロード済み全モデルを
   返すため先頭採用では巨大モデルの誤ロードを誘発する）
2. ロード済みが無ければエラー（誤ロードさせずユーザーに案内）
3. `/api/v0/models` 自体が無い旧バージョンのみ `/v1/models` の先頭へフォールバック
LM Studio は認証（API トークン）を有効化できるため、キー欄は「任意」として表示する（keyOptional）。
登録済みならそれを Bearer で送り、未登録ならダミー "lm-studio" を送る（認証オフのサーバーはヘッダーを無視する）。
| ollama | OllamaProvider | http://localhost:11434 | 不要 |

Ollama の防御策（LM Studio と同型の問題への予防。実機未検証 — 導入時に要確認）:
- model 空 → /api/ps（実行中）→ /api/tags（取得済み）の順で自動解決。無ければ `ollama pull` 案内のエラー
- schema があれば `format` に JSON Schema（Ollama 0.5+ 構造化出力）、無ければ format なし（自由文）
- 思考型モデル対策: content 空なら message.thinking を読む。さらに extract_json が <think> ブロックを除去
- base_url 末尾の /v1（OpenAI 互換側 URL）は除去して native API へ。404 はモデル未取得の案内に変換

### コスト (cost.rs)
価格表 `&[(provider, model_prefix, in_usd_per_1m, out_usd_per_1m)]` を定数で持ち、前方一致で解決（不明モデルは 0 扱い + ログ警告）。usage を api_usage に記録し、当月合計が monthly_limit_usd 以上なら limited=true。limited 中は send_user_message が low モード動作になり、`mode-changed {reason:"cost_limit"}` を一度 emit。

## モード移行（spec 2.3）

- 低→アド: advanced.rs が conversation_log 直近 10 件をプロンプトに入れる（常時やるので特別処理なし）
- アド→低（set_settings で検知 / API エラー連続 3 回での自動降格時も）: tasks.rs のキューに `GenerateSummary` を投入。ワーカーが直近 advanced ログ 20 件を LLM で 100 文字以内に要約 + キーワード 5 個抽出（この呼び出しも api_usage に記録）し context_summary へ INSERT。失敗時は黙って破棄（要約はベストエフォート）
- API エラー時の自動降格は「一時的」: settings.mode は書き換えず、エンジン内部フラグ `degraded_until`（最後のエラーから 5 分）で表現
- 降格の解除と通知:
  - `set_settings` / `set_api_key` は「今すぐ再試行してよい」の合図として降格を即解除する。
    解除時に実効モードが advanced へ戻る場合は `mode-changed {mode:"advanced", reason:"recovered"}` を emit
  - 期限切れ時も同イベントで自動復帰を通知（降格設定時に遅延タスクを spawn。延長・先行解除の場合は何もしない）
  - 設定パネルは降格中、モード選択を実効モード(low)表示にし説明行を出す。ただしユーザーが選択に触れていない限り、
    保存時は設定上のモードを送る（一時降格のうっかり恒久化を防ぐ）

## バックグラウンドキュー（spec 3.4）

tasks.rs: `enum BgTask { GenerateSummary, RestockMonologues }` を `tokio::sync::mpsc` で受けるワーカーを 1 本 spawn。
busy ゲート: `Arc<tokio::sync::Semaphore>`（permits=1）を send_user_message が処理中保持。ワーカーはタスク実行前に permit 取得→即返却することで「メイン応答中は待機」を実現。
RestockMonologues は未使用キャッシュが 2 未満かつ advanced モードかつ limited でない時のみ実行（単独 LLM コールで monologues を 3 件生成）。

## ランダムトーク（spec 2.4）

monologue.rs: `random_talk_interval_sec` の tokio interval。tick 時に busy なら skip。
さらに presence::should_stay_quiet（静音モード / ウインドウ非表示 / 全画面アプリ検知）に該当する間も skip。
advanced モード（かつ非 limited）→ cache pop → あれば `dialogue` emit + ログ保存 + `speak` emit ×2。
cache 空 or low モード → 辞書 random_talk から選択。消費後、未使用数 < 2 なら RestockMonologues をキューへ。
settings 変更でインターバル再設定（watch チャネル）。

## TTS フック（spec 2.6）

- Rust: `trait TtsSink { fn on_line(&self, speaker: &str, text: &str); }` を tts.rs に定義。`NoopTts` を実装し AppState に `Box<dyn TtsSink>` で保持。発話確定箇所（send_user_message 応答時・ランダムトーク emit 時）で必ず呼ぶ + `speak` イベント emit。
- TS: `interface TtsSpeaker { speak(speaker: "main"|"sub", text: string): void; interrupt?(): void }` を src/tts.ts に定義、実装を main.ts で注入。吹き出し描画開始時に必ず呼ぶ。

## 音声合成 TTS（スタンドアロン化済み）

実発声は**フロント主導**で行う（吹き出し描画と同期するため）。Rust は「合成→WAVを返す」までを担い、
フロントが Web Audio で再生する（Rust 側に音声再生依存を持ち込まない）。
**この経路に全発話（応答・独り言・つつき・挨拶・時事ネタ）が自動で乗る**（balloon の TtsSpeaker.speak が単一の発声起点）。
合成契約は `synthesize_voice(text, slot)` の一本で、声・速度・音量・エンジン種別はすべて **settings からバックエンドが解決**する
（フロントは speaker 番号を持たない）。

### エンジン抽象（`tts_engine.rs`）
- enum `TtsEngine { VoicevoxCore, VoicevoxHttp{...}, OpenAiCompat{...} }`。`from_settings(&Settings)` で実体を選び、
  `synthesize(slot, text) -> Result<Vec<u8>, String>` で WAV を返す。
- 既定 = **VoicevoxCore（埋め込み・無サーバ）**。要件「ugga 単体で動く」の本命。
  CPUのみで動作（GPU 不要）。資産が揃ってない / 未起動なら Err で返しフロントは黙殺（声なしで継続）。
- VoicevoxHttp は後方互換用（既存の `audio_query` → `synthesis` の VOICEVOX HTTP エンジンと同じ）。
- OpenAiCompat はオプション（外部サーバが要るので「ugga 単体」要件は満たさない）。`POST {base}/audio/speech`
  に `{model, input, voice:"alloy", response_format:"wav", speed, instructions: caption}` を投げる
  （Irodori-TTS-Server 互換。voice はダミー、声質は VoiceDesign キャプションで指定＝参照音声クローン未使用）。

### 埋め込み合成（`voicevox_ffi.rs` + `voicevox_download.rs`）
- 公式プリビルド C API（`voicevox_core.dll` 0.16.4）を **`libloading` で実行時ロード**（ソースからビルドしない＝
  アプリのビルドは軽い）。FFI は `voicevox_onnxruntime_load_once` / `voicevox_open_jtalk_rc_new` /
  `voicevox_synthesizer_new`（**`acceleration_mode = CPU` 強制**。GPU/DirectML は再 init 時 AV する事例があり要件にも合致しないため不使用） /
  `voicevox_voice_model_file_open` + `load_voice_model` / `voicevox_synthesizer_tts` / `voicevox_wav_free` /
  `voicevox_synthesizer_create_metas_json`（声一覧・クレジット表示用）。
- 資産は `dirs::data_dir()/ugga/voicevox` 配下に再帰探索で検出（`voicevox_core.dll` / `voicevox_*onnxruntime*.dll` /
  `open_jtalk_dic_*` フォルダ / 任意の `.vvm`）。初回 `init` で合成器を構築し、AppState の `Mutex<Option<VoicevoxEngine>>` に保持して再利用。
- **boot 時・設定変更時に `spawn_voicevox_preinit`** で `tauri::async_runtime::spawn` 経由のバックグラウンドで事前 init
  （tts_enabled && engine == voicevox_core && 資産OK のとき）→ 初回発話の数秒ラグを消す。失敗してもログだけで boot は止めない。
- 初回 DL（`download_voicevox_assets` コマンド）: 公式ダウンローダ `download-windows-x64.exe`(0.16.4)を `reqwest` で取得し
  `dirs::data_dir()/ugga/voicevox/voicevox-downloader.exe` に保存。`Command::new` で起動し
  `-o <asset_dir> --c-api-version 0.16.4 --devices cpu --exclude additional-libraries --models-pattern [0-9]*.vvm`。
  - **規約同意の自動化**: ツールが対話的に `[y,n,r]` プロンプトを出すため stdin pipe に `y\n` を5回投入してから閉じる。
    UI 側で利用規約同意チェックボックスを要求し、`agreed=false` なら即エラー（コンプライアンス保証）。
  - **GitHub レート制限緩和**: 環境変数 `GH_TOKEN` で PAT を渡す。UI から keyring(`provider="github_token"`)に保存可能。
  - **DLL ロック競合（os error 32）対策**: DL 前に AppState の合成器を Drop（dll アンロード）、ついでに既存
    `voicevox_core.dll` / `voicevox_*onnxruntime*.dll` を `.dll.old-N` にリネーム退避（Windows は使用中 DLL も rename は通る）。
  - **UTF-8 出力対応**: ダウンローダの stderr を `Read::read_to_end` でバイト読み→改行分割→`String::from_utf8_lossy`
    （Windows 既定の lossy 変換で日本語化けるのを回避）。ANSI エスケープも除去して `voicevox-download` イベントに流す。
  - **エラー検出**: `API rate limit exceeded` を見つけたら最終メッセージを日本語の対処案内に置き換え（PAT 案内付き）。

### フロント側（`tts.ts`）
- `VoicevoxSpeaker implements TtsSpeaker`。speak(slot,text) で **逐次キュー**に積み、ワーカーが
  `synthesize_voice({text, slot})` → base64 → ArrayBuffer → `AudioContext.decodeAudioData` → 再生 → ended を待って次へ
  （main→sub が重ならない。speaker 番号はフロントが持たない）。
- `interrupt()`: 現在再生を停止しキューを空にする。**balloon.show() の冒頭（新発話の generation 更新時）で `speaker.interrupt?.()` を呼ぶ**
  （新しい発話が来たら古い音声を打ち切る）。
- 自動再生ポリシー対策（**実装済み・必須**）: WebView2 は操作から時間が経つと AudioContext を一時停止し、
  ジェスチャー無しの resume が失敗するため、**独り言など自発発話が無音になる**。
  window_ctl.rs の `create_main_window` で `additional_browser_args` に
  `--autoplay-policy=no-user-gesture-required` を付与して解決。tts.ts は AudioContext を初回 pointerdown で resume + 再生前にも resume を試みる。
- main.ts: settings.tts_enabled に応じて `RuntimeSpeaker` が `VoicevoxSpeaker` / `NoopSpeaker` を委譲先として切替え（settings-changed で差し替え）。
- 失敗時 UX: 合成エラーは黙殺（声なし・吹き出しは出る）。設定の「声のテスト」だけは失敗をトースト表示。

### クレジット表示（VVM 利用規約準拠）
- VOICEVOX 音声モデルの利用規約は「VOICEVOX:キャラ名」のクレジット表記を義務付けている。
- index.html に `#tts-credit .solid` バッジを置き、`updateTtsCredit(settings, el, requestMask)` が `tts_engine` ごとの
  list_voices（埋め込みは `voicevox_core_list_voices` / 外部は `list_voices`）で speaker_main/sub の話者名を引き、
  `VOICEVOX:四国めたん / VOICEVOX:ずんだもん` のように **ステージ下端中央に常時表示**する。`.solid` でアルファマスクに含めて
  誤クリックを防ぎつつ `pointer-events:none` でクリック自体は透過させる。
- 起動時・`settings-changed`・`onSettingsApplied` のいずれでも refresh。声/エンジンの変更でクレジットも追従する。
- TTS無効 / openai_compat（VVM不使用）・取得失敗時は非表示（誤表記を出さない方針）。

## 音声入力 STT（2026-06 追加分の契約）

声で話しかける。**取得はフロント（MediaRecorder）**、**文字起こしは OpenAI互換エンドポイント**（クラウド既定 / ローカル Whisper サーバーも可）。
結果は既存の入力経路（input.ts）へ流し込み、通常のテキスト送信と同じ `send_user_message` に乗せる。

### マイク許可（WebView2）— 現行方針: 組み込みプロンプトに委ねる
- 調査結果（wry 0.55.1 src/webview2/mod.rs:500）: wry の既定 PermissionRequested ハンドラは
  **CLIPBOARD_READ のみ許可**し、マイクは WebView2 既定（State=DEFAULT）になる。
  → 初回 getUserMedia 時に WebView2 組み込みの「マイクの使用を許可しますか?」プロンプトが出て、
    ユーザーが一度 Allow すればオリジン単位で記憶される。**追加の COM コードなしで動く想定**（明示同意 UX としても妥当）。
- **フォールバック（未実装・必要時に追加）**: 透過/枠なしウインドウでこのプロンプトが出ない・壊れる場合、
  window_ctl.rs の `create_main_window` で `with_webview` → WebView2 コントローラの `add_PermissionRequested` に
  Microphone を ALLOW するハンドラを追加する（`#[cfg(windows)]`、webview2-com + windows を直接依存追加。
  wry のハンドラとは別に登録でき併存する）。それでも困難なら Rust 側 cpal 録音へ切替（別設計）。
- 実機検証で挙動を確定してから fallback の要否を決める（過剰実装を避ける）。

### Rust 側（新規 stt.rs）
- `transcribe(base, key, model, language, audio: &[u8], mime: &str) -> Result<String, String>`:
  multipart/form-data で `file`（filename は mime から拡張子推定: audio/webm→.webm, audio/ogg→.ogg, audio/wav→.wav）、
  `model`、`language`、`response_format=json` を POST `{base}/audio/transcriptions`。Bearer key（あれば）。
  応答 `{"text":"..."}` の text を返す。base 既定 `https://api.openai.com/v1`。timeout 60s。401→キー、その他は日本語 Err。
- コマンド `transcribe_audio(audio_b64, mime)`: settings から base_url/model/language、keyring("stt") から key を読み実行。

### フロント側（新規 stt.ts + input.ts 連携）
- `VoiceInput` クラス: マイクボタン押下で getUserMedia(audio)→MediaRecorder 開始（mimeType は `audio/webm;codecs=opus` を優先、無ければブラウザ既定）。
  もう一度押す（またはトグル）で停止→Blob→base64→`transcribe_audio({audioB64, mime})`→得たテキストを
  入力欄に入れて**自動送信**（input.ts の submit 経路を再利用）。録音中は赤い録音インジケータ、文字起こし中は thinking 表示。
- 入力欄（#chat-input-wrap）にマイクボタンを追加。stt_enabled のときだけ表示。失敗（許可拒否・通信・キー無し）はトーストで案内。
- main.ts: stt_enabled に応じてマイクボタンの表示を切替（settings-changed/boot）。

### 設定 UI（settings.ts 新セクション「音声入力」）
- トグル stt_enabled / 接続先 stt_base_url（上級・空欄=既定 OpenAI）/ モデル stt_model / 言語 stt_language /
  APIキー（provider "stt"、set_api_key、ローカルサーバー時は任意）。各項目に「?」ヘルプ
  （OpenAI はクラウド有料・キー必要、ローカル Whisper サーバーは無料だが別途起動、音声がクラウドへ送られる旨のプライバシー注記）。

### 設定 UI（settings.ts「音声（読み上げ）」）
- トグル `tts_enabled` / エンジン選択（既定 `voicevox_core`＝埋め込み・無サーバ）。
- 埋め込み(core)時のみ表示:
  - **音声データの状態**（`voicevox_assets_ready`で✓/未取得を判定）と進捗ペイン（リングバッファで最後の進捗を残し、失敗時はエラー文と並べて維持）
  - **VOICEVOX 利用規約に同意**チェック（DLのゲート。「VOICEVOX:キャラ名」表記が必要である旨を明示）
  - **音声データをDL/再DL**ボタン（`download_voicevox_assets({agreed, ghToken})`）
  - **GitHub トークン**入力＋「保存」「削除」ボタン＋「保存済み/未保存」表示（`set_github_token`/`has_github_token`/`delete_github_token`）。保存時は入力欄をクリア。任意・無くても可
- VOICEVOX(HTTP)時のみ表示: 接続先 `tts_base_url`、話者ドロップダウン（`list_voices`、失敗時「VOICEVOXを起動してください」）
- 埋め込み/HTTP 共通: メイン/サブの話者ドロップダウン（`tts_engine` でコマンド振り分け＝core は `voicevox_core_list_voices`）
- OpenAI互換時のみ表示: 接続先 `tts_oai_base_url`、モデル `tts_oai_model`、メイン/サブのキャプション `tts_oai_caption_main/sub`（VoiceDesignの自然言語声質指定）
- 共通: 話速・音量スライダー、「声のテスト」ボタン（`synthesize_voice` で `slot:"main"` 短文を合成。失敗はトースト）
- プライバシー/コスト: 埋め込み(core)・HTTP の VOICEVOX はローカル動作で無料・オフライン。openai_compat はサーバの方針に依存。**発話テキストは外部送信されない方針**（クラウド AI とは別経路）。

## フロントエンド構成（src/）

- `types.ts` … 上記共有型
- `main.ts` … boot: get_boot_payload → 画像プリロード → イベント listen → タイマー UI 配線
- `characters.ts` … 各 pose の `<img>`（data URL）を全て生成して重ね、`visible` クラス付け替えのみで切替（チラつき防止）。オフスクリーン canvas にも描画してアルファ取得用 ImageData を保持
- `alphamask.ts` … characters の ImageData + `.solid` 要素矩形からグリッド合成 → `update_alpha_mask`。pose 変更/パネル開閉/吹き出し表示変更時に再送（50ms デバウンス）
- `balloon.ts` … 吹き出し。1 文字 30ms のタイプライター描画（描画開始時に TtsSpeaker.speak を呼ぶ = TTS フック）。main → sub の順に逐次表示
- `chatlog.ts` … 簡易ログパネル（開閉式、`.solid`）
- `input.ts` … チャット入力欄（`.solid`）。送信→ thinking 表示 → send_user_message → balloon 表示
- `settings.ts` … 設定パネル（`.solid`）: モード切替、プロバイダ/モデル/base_url、API キー登録（set_api_key、表示は伏せ字）、月額上限と今月消費額（get_usage）、ランダムトーク間隔、セーフモード切替
- `tts.ts` … TtsSpeaker インターフェース + NoopSpeaker
- 操作系: キャラクター click=入力欄トグル / 右クリック=設定パネル / ドラッグ=`startDragging()` でウインドウ移動。`data-tauri-drag-region` は使わず mousedown で判定（透明部の誤ドラッグ防止）

## モード表示オーブ（非テキストのモードインジケーター）

「高度モードで動作中」等の文字表示は没入感を損なうため使わない。代わりに小さな光球（オーブ）`#mode-orb` を
メインキャラの頭上付近に常時表示し、色と発光で現在モードを示す:

- advanced: 金色 + ゆっくり脈動するグロー（CSS アニメーション）
- low: 青灰色・発光なし（静的な点）
- mode-changed の reason が api_error / cost_limit のとき: 一瞬フリッカー（明滅）してから low 表示へ

状態の更新ソースは 3 つ: ①boot 時の settings.mode（get_usage().limited なら low 扱い）、
②mode-changed イベント、③ユーザー起点応答（send_user_message の戻り値）の DialogueResponse.mode。
`dialogue` イベント（起動挨拶・つつき・ランダムトーク等）ではオーブを更新しない:
これらは advanced モード中も辞書（mode:"low"）で再生されることがあり、実モードと食い違う表示になるため。
劣化（degraded）からの自動復帰はイベントが無いため、次の応答の DialogueResponse.mode で復帰表示される（許容）。
title 属性に短い説明（例:「おしゃべりモード: AI接続中」/「省エネモード」）を持たせてよい（ホバー時のみ表示のため没入感を損なわない）。
オーブはクリック透過対象（.solid を付けない）。

## 設定パネルの UX 指針（知識のないユーザー向け）

- プロバイダは `<select>`: OpenAI / Anthropic (Claude) / Grok (xAI) / Ollama（ローカル・無料） / LM Studio（ローカル・無料）
- プロバイダ変更時: モデル欄の placeholder と空欄時の既定値を切替（openai: gpt-4o-mini / anthropic: claude-haiku-4-5-20251001 / grok: grok-3-mini / ollama: llama3.2 / lmstudio: ロード中のモデル名）。
  base_url 欄の placeholder にも既定 URL を表示。ollama / lmstudio では API キー欄を非表示にする
- 各入力項目の横に「?」ヘルプボタン。クリックで短い説明ポップオーバー（やさしい日本語、APIキーの取得先 URL を含む）。
  説明文は settings.ts 内の定数オブジェクトに集約する

## 常駐機能（トレイ / 自動起動 / イベント対話 / 静音）

- **トレイ（tray.rs）**: アイコンは 32×32 RGBA をコード生成（画像アセット不要）。メニュー =
  表示/非表示・モード 2 択（チェック）・静音・セーフモード・設定を開く・終了。
  左クリックで表示/非表示トグル。チェック状態は apply_settings 経由で `sync_tray` が同期。
  「終了」= 位置保存 → quit イベント台詞（静音中はスキップ）→ 2.2 秒後 exit(0)。
- **自動起動**: tauri-plugin-autostart。settings.autostart 変更時と起動時（整合）に enable/disable。
- **起動挨拶/オンボーディング**: フロント boot 完了時の `frontend_ready` で発火。
  app_settings の `first_boot_done` が無ければ first_boot（オンボーディング台詞）、あれば boot。
  AppState.greeted（AtomicBool）でセーフモード切替のページ再ロード時の二重挨拶を防ぐ。
  起動挨拶は静音モードでも再生する（ユーザーが起動した直後のため）。
- **つつき**: フロントでクリック種別を判別（最後のクリックから 250ms で確定。1 回=入力欄トグル /
  2〜3 回=poke / 4 回以上=poke_rapid）→ `poke` コマンド → 辞書 events から応答。
- **撫で**: フロント（nade.ts）がボタン無しのホバー mousemove を監視し、往復運動の指標
  （方向反転回数・累積移動量・局所性・継続時間）で「ただの通過」と区別 → `nade` コマンド → 辞書 events
  から応答。ボタン押下中（クリック・ドラッグ）は除外、発火後は cooldown で連打化を防ぐ。最終操作にも数える。
- **放置（presence.rs）**: 60 秒間隔で監視。最終操作（送信・つつき）から 30 分で events.idle を
  1 回再生。should_stay_quiet 中・busy 中は持ち越し。
- **静音判定 `should_stay_quiet`**: quiet_mode / ウインドウ非表示 / （auto_quiet_fullscreen 時）
  フォアグラウンドの全画面アプリ検知（Win32。Progman/WorkerW は除外、非 Windows は常に false）。
  ランダムトーク・idle・更新通知・終了挨拶が従う。ユーザー起点の応答（送信・つつき）は対象外。
- **更新通知（update_check.rs）**: update_feed_url の JSON（`{"version":"x.y.z"}`）を起動時に取得し、
  CARGO_PKG_VERSION より新しければ辞書 update_talk（{version} 置換）で一度だけ通知
  （app_settings `update_notified_version` で同一バージョンの再通知を抑止）。失敗は黙殺。

## リリース基盤（配布・運用。2026-06 追加分の契約）

### 1. リソース同梱とユーザーデータ展開
- tauri.conf.json: `bundle.active: true`, `targets: ["nsis"]`, `resources: {"../ghosts/": "ghosts/", "../shells/": "shells/"}`
- data_root() の解決順序（ghost.rs）:
  1. `UGGA_DATA_DIR` 環境変数（開発・検証用の明示上書き）
  2. current_dir から上方向に ghosts/ を持つディレクトリを探索（`tauri dev` 用。リポジトリを直接読む）
  3. `dirs::data_dir()/ugga`（インストール版）
- 初回シード `ghost.rs::seed_user_data(app)`: 解決先が 3 になるケースで `data_dir/ugga/ghosts` が
  無ければ、`app.path().resource_dir()` の ghosts/ shells/ を `data_dir/ugga/` へ再帰コピーする。
  **setup フックの最初**（ghost/shell/dict 読み込みより前）に呼ぶ。
- これに伴い ghost/shell/dict の初期読み込みは main() 先頭ではなく setup 内（seed 後）で行い、
  AppState には組み込みデフォルトを初期値として入れておき setup で差し替える。

### 2. 二重起動防止
- `tauri-plugin-single-instance`。**最初に登録するプラグイン**。2 個目の起動はコールバックで
  既存インスタンスのメインウインドウを表示 + フォーカス（トレイの「表示」と同じ経路）して終了。

### 3. ログ
- `tauri-plugin-log` v2: Target = Stdout + LogDir(file_name "ugga")。LevelFilter::Info、
  RotationStrategy KeepOne・max_file_size 5MB。env_logger は廃止。
- `std::panic::set_hook` で panic を log::error に記録（setup 内で登録）。
- `open_log_dir` コマンド: `app.path().app_log_dir()` を OS ファイラで開く。

### 4. DB バージョニングと自動掃除
- `PRAGMA user_version` ベースの移行基盤（db.rs）: `const DB_VERSION: i32 = 1`。
  open 時に CREATE TABLE 群 → user_version を読み、`v < DB_VERSION` の間 migrate_db(v) を順次適用 →
  user_version 更新。v0→1 は初期スキーマ（CREATE IF NOT EXISTS 済みのためパススルー）。
  **以後 DB 構造を変えるときは必ず DB_VERSION を上げて migrate に追記する。**
- 自動掃除 `db.prune()`: conversation_log は新しい順 5000 件残し、monologue_cache の used=1 は
  7 日経過分を削除、api_usage は 400 日残す。setup で 1 回 + 24 時間間隔の tokio タスク。

### 5. ゴースト/シェル選択・再読込
- list_ghosts / list_shells / reload_assets コマンド（契約表参照）。
- 設定パネルに「キャラクター」セクション: ゴースト/シェルのドロップダウン（開いたときに一覧取得）+
  「再読み込み」ボタン。保存で ghost_id/shell_id が変わったとき・再読込ボタンのときは、
  フロントが `onAssetsReloaded()`（main.ts 提供）を呼び、get_boot_payload からステージを再構築する
  （characters の DOM 破棄→再生成、アルファマスク再送、オーブ初期化を含む）。

### 6. 表示スケール
- Settings.display_scale（0.5〜2.0、既定 1.0）。set_settings で clamp。
- バックエンド: ウインドウ生成時 inner_size = (760, 560) × scale。apply_settings で変化時は set_size。
- フロント: `#stage { transform: scale(var(--ugga-scale)); transform-origin: bottom left; }` 方式で
  全 UI を一括スケール（ウインドウも同率で変わるため見た目の比率は不変）。
  boot と settings-changed で CSS 変数を更新。getBoundingClientRect は transform 後の値を返すため
  アルファマスクは追加対応不要。
- 設定 UI はスライダー（50〜200%、ステップ 25）。

### 7. データエクスポート
- `export_data(include_profile)`: conversation_log / context_summary / api_usage 全件 +
  （指定時）user_profile を `{exported_at, app_version, ...}` の JSON にまとめ、
  `dirs::download_dir()`（無ければ data_dir/ugga）へ `ugga_export_YYYYMMDD_HHMMSS.json` で保存し
  パスを返す。API キーと settings は**含めない**。
- 設定パネルのメンテナンス節: 「ログフォルダを開く」「データを書き出す（記憶を含めるチェック付き）」。

## 時事ネタ雑談・興味分野・オンボーディング（2026-06 追加分の契約）

目的: 定期的に web から情報を取得し、雑談に時事ネタを織り込む。取得ジャンルはユーザーの興味分野で選定する。
初回起動時にプロフィール/嗜好を聞き取る。すべて既定 OFF・明示オプトイン。

### データ（DB_VERSION → 2、migrate に追記）
```sql
CREATE TABLE IF NOT EXISTS interest_topics (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  topic TEXT NOT NULL UNIQUE,
  enabled INTEGER NOT NULL DEFAULT 1,
  ts TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS topic_headlines (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  topic TEXT NOT NULL,
  title TEXT NOT NULL,
  source TEXT, url TEXT, published TEXT,
  fetched_at TEXT NOT NULL,
  used INTEGER NOT NULL DEFAULT 0,
  UNIQUE(topic, title)
);
```
app_settings 予約キーに `profile_onboarded` を追加。
prune() に topic_headlines の掃除を追加（各 topic 新しい順 30 件残し / fetched_at 14 日経過削除）。
interest_topics は user_profile と別管理（user_profile の 50 件 FIFO で興味が消えるのを防ぐ。最大 20 件）。

### 取得モジュール topics.rs
- `build_feed_url(template: Option<&str>, query: &str) -> String`:
  template が None なら内蔵既定
  `https://news.google.com/rss/search?q={query}+when:7d&hl=ja&gl=JP&ceid=JP:ja`。
  `{query}` を URL エンコードした興味語で置換。
- `fetch_all(state)`: enabled な interest_topics それぞれについて feed を GET（タイムアウト 15s、逐次・最大 8 topic）、
  feed-rs で RSS/Atom をパースし上位 5 件の title/link/published/source を取得 → topic_headlines へ UNIQUE(topic,title) で
  重複無視 INSERT → prune。失敗は warn ログで黙殺（update_check と同様のベストエフォート）。
- `recent_headlines(db, n) -> Vec<String>`: 未使用（used=0）の見出しを新しい順に最大 n 件返し、used=1 にする
  （複数 topic から散らす）。返すのは「topic: title」形式の文字列。
- `spawn_topics_scheduler(app, state, interval_rx: watch::Receiver<u64>)`: monologue タイマーと同型。
  topics_interval_min（分）を秒に直した interval。tick 時 topics_enabled なら fetch_all。
  起動時に enabled なら 1 回（ただし最後の fetch から十分時間が経っている場合のみ。簡易に起動 1 回でよい）。
  interval 0/未 enabled の扱いは「enabled でなければ何もしない」。watch で間隔・enabled 変更を反映。
- web 送信の注意: 興味語が検索プロバイダ（既定 Google ニュース）に送信される。既定 OFF・オプトインで担保。

### 雑談への統合（コスト増ゼロ）
- 時事ネタは **advanced モードの独り言ストック生成（tasks.rs run_restock）に折り込む**。追加 LLM コールはしない。
- run_restock で topics_enabled なら `topics::recent_headlines(db, 3)` を取得し、build_system_prompt に渡す。
- build_system_prompt のシグネチャに **`interests: &[String]` と `headlines: &[String]` を追加**（呼び出しは
  run_advanced と run_restock の 2 箇所。advanced は interests のみ渡し headlines は空でよい）。
  - interests（enabled な interest_topics の topic 群、advanced/restock 両方で注入）→
    プロンプトに「# ユーザーの興味・関心」ブロック（無ければ出さない）。会話全体の話題選びに反映。
  - headlines（restock のみ・非空のとき）→ MonologueStock タスクに「# 最近の話題（ネットの見出し）」ブロックを追加し、
    次の**安全ガード文**を必ず含める:
    「これらは雑談のきっかけ。明るく軽い話題を最大1つだけ選んで自然に触れてよい。
     事故・災害・事件・死・病気・戦争・重い政治などネガティブな見出しには触れないこと。
     見出しを丸読みせず、自分の言葉で軽い感想を言う程度に。ふさわしい話題が無ければ普通の雑談でよい。
     見出しに書かれていない事実を作らないこと。」
- 低負荷モードでは見出しを発話しない（棒読みは無神経になりうるため）。interests の会話反映も advanced のみ。

### オンボーディング（フロント主導のフォーム + キャラの声かけ）
- BootPayload.onboarded が false のとき、boot 直後に `#onboarding-panel`（.solid.panel）を開く。
  first_boot の起動挨拶（frontend_ready 側）とは独立。両方出てよい（挨拶 → オンボーディング）。
- パネル項目: ニックネーム（任意）/ 興味分野（チップ入力 + 候補ボタン: ニュース・テクノロジー・ゲーム・スポーツ・
  音楽・映画/アニメ・料理・科学・経済 等）/ 話し方の希望（任意・自由文）/ チェック「ネットの話題を取り入れる」。
  ボタン「はじめる」→ complete_onboarding、「スキップ」→ skip_onboarding。
- complete_onboarding の内部: nickname があれば `ユーザーの呼び名は<nick>` を add_profile、talk_style があれば
  `話し方の希望: <...>` を add_profile、interests を set_interests、topics_enabled を settings に反映（set_settings 経由の
  apply_settings で topics スケジューラへ通知）、profile_onboarded を立て、topics_enabled なら fetch_topics_now 相当を起動。
- プロフィール/興味はユーザー単位（ゴースト変更で消えない）。ゴースト切替時の再聞き取りはしない。
- 設定パネルから「プロフィールを設定し直す」で再表示可能（onboarded でも明示的に開ける）。

### 設定パネル追加セクション「ネットの話題（時事ネタ）」
- トグル topics_enabled / 取得間隔 topics_interval_min（分）/ 興味分野の管理（get_interests・set_interests で
  チップ追加削除）/「今すぐ取得」（fetch_topics_now）/（上級）取得元RSSテンプレ topics_feed_url。
- 各項目に「?」ヘルプ（やさしい日本語、プライバシー注記: 興味語が検索サービスに送られる旨）。
- apply_settings: topics_enabled / topics_interval_min 変更時に topics スケジューラの watch を更新（monologue 間隔と同様）。

## ツール実行（実用アシスタント化。2026-06 追加分の契約）

「おしゃべり相手」から「役に立つ同居人」へ。**2 ラウンドのツールコールはしない**（レイテンシ/コスト倍増を避ける）。
既存の単一 JSON コールを拡張し、データの向きで 2 種に分ける。すべて `tools_enabled`（既定 false）で全体 ON/OFF。

### 1. 現在日時の注入（入力コンテキスト・ゼロリスク）
- tools_enabled かつ advanced のとき、build_system_prompt に「# 現在日時\n`chrono::Local::now()` を `YYYY-MM-DD (曜) HH:MM` で」を入れる。
  「今何時?」「今日何曜日?」に単一コールで答えられる。build_system_prompt に `tools_enabled: bool` 引数を追加（呼び出し2箇所更新）。

### 2. リマインダー（出力アクション・モデルが"する"）
- LLM 出力 JSON に任意 `actions` 配列を追加（`remember`/`monologues` と同じ作法）。
  RawOutput に `#[serde(default)] actions: Vec<RawAction>`。RawAction{ `type`(String,別名 r#type), `after_sec`(Option<u64>), `text`(Option<String>) }。
- dialogue_schema（Chat タスク）に tools_enabled のとき `actions` を追加:
  `{"type":"array","items":{"type":"object","properties":{"type":{"type":"string"},"after_sec":{"type":"integer"},"text":{"type":"string"}},"required":["type"]}}`。required に actions を含める（空配列可）。
- Chat プロンプト（tools_enabled）に「# 使えるツール」節:
  「ユーザーが『N分後に/あとで/◯時に教えて』等のリマインドを頼んだときだけ actions に
  {type:"reminder", after_sec: 秒数, text: 通知の一言} を入れる。それ以外は空配列 []。過去や曖昧な依頼には入れない。」
- 実行（dialogue/mod.rs、応答を persist_and_speak した後）: tools_enabled のときのみ。type=="reminder" の各 action を検証
  （after_sec を [10, 86400] に clamp、最大 3 件、text 空はスキップ）→ `tauri::async_runtime::spawn` で sleep 後、
  リマインダー発話を `dialogue` イベント + ログ + speak で再生（main.text=通知文, sub.text=""／空 sub は balloon が非表示）。
  発話は monologue::emit_and_log を再利用。静音/非表示中でも**リマインダーは鳴らす**（ユーザーが明示的に頼んだため）。
- v1 はメモリ上のタイマー（アプリ終了で消える・相対指定のみ）。永続化は将来。AdvancedOutput に `actions: Vec<ReminderAction>` を足す。

### 3. クリップボード補助（入力コンテキスト・都度同意）
- 自動読取はしない。入力欄横のクリップボードボタン（tools_enabled 時のみ表示）を押した次の送信のみ、
  `send_with_clipboard(text)` 経由でサーバ側がクリップボードを読み（arboard。Cargo.toml に `arboard = "3"` 追加可）、
  プロンプトに「# クリップボードの内容\n<最大2000字>」を注入する。これで「翻訳して/要約して/説明して」に対応。
- handle_user_message に `clipboard: Option<String>` を通す内部経路を追加（既存 send_user_message は None のまま不変）。
- `read_clipboard` はUIプレビュー用（ボタン押下時に先頭を見せる等）。

### フロント
- 入力欄に 📋 クリップボードボタン（#clip-button、tools_enabled 時のみ表示。マイクボタンと同様の出し分け）。
  押すと「次の送信にクリップボードを添える」一発フラグを立て、見た目を active に。次の送信で input.ts が
  `send_with_clipboard` を使い、送信後フラグ解除。リマインダーは `dialogue` イベントで自然に表示される（追加UI不要）。
- 設定パネルに「ツール（アシスタント）」セクション: トグル tools_enabled + ヘルプ
  （日時を答えられる/リマインドを頼める/クリップボードの内容を扱える・クリップボードはボタンを押したときだけ読む旨）。

## 設定画面の刷新・吹き出し・掛け合いパターン（2026-06 追加分の契約）

### A. 設定のカテゴリ分け（settings.ts）
- パネル上部に**カテゴリ選択プルダウン**（`<select>`）を置き、選んだカテゴリのコンテナだけ表示（他は hidden）。
  右クリックは従来どおり「設定を開く」のまま（プルダウンの方が発見しやすく1画面完結のため context menu 方式は採らない）。
- カテゴリと内訳（既存セクションの振り分け。**要素は全て生成したまま**＝ syncForm/collectForm は不変、表示だけ切替）:
  1. **基本（AI接続）**: 動作モード+状態行 / プロバイダ / モデル / 接続先 / APIキー / 月額上限 / 使用量
  2. **会話・ふるまい**: ランダムトーク間隔 / 静音 / 全画面自動静音 / ネットの話題(時事ネタ一式) / ツール
  3. **音声**: TTS（音声読み上げ）。STT セクションは従来どおり STT_UI_ENABLED で隠す
  4. **見た目**: ゴースト/シェル選択 / 表示スケール
  5. **記憶**: 長期記憶の管理
  6. **システム**: 自動起動 / セーフモード / 更新URL / メンテナンス（ログ/エクスポート/履歴削除/プロフィール再設定）
- 実装: 各カテゴリの `<div class="settings-category">` を作り、既存の section 構築を該当コンテナへ append。
  プルダウン change で対象コンテナのみ表示。open() 時は先頭カテゴリ（基本）を表示。

### B. 保存で閉じる（settings.ts）
- save() 成功時（set_settings 等が成功）に `this.close()` を呼ぶ。失敗時は閉じない（トーストのまま）。

### C. 吹き出しの動的表示時間＋TTS同期＋単一消去タイマー（balloon.ts / tts.ts）
- **全ターンの描画（とTTS発話）が終わってから、単一タイマーで全吹き出しを一括消去**する
  （現状の slot 別 scheduleHide を廃止）。
- 消去までの保持時間は総文字数に比例: `hold = clamp(HOLD_BASE + HOLD_PER_CHAR*総文字数, HOLD_BASE, HOLD_MAX)`
  目安 HOLD_BASE=1800ms, HOLD_PER_CHAR=70ms, HOLD_MAX=12000ms。
- **TTS同期**: TtsSpeaker に `whenIdle?(): Promise<void>` を追加（キューが空＆再生中でないとき resolve。
  NoopSpeaker は即 resolve、VoicevoxSpeaker は queue 空＆!running で resolve、RuntimeSpeaker は委譲）。
  balloon は全ターン描画後に `await speaker.whenIdle?.()` してから hold タイマー開始（発話中は消えない）。
  世代カウンタで新発話が来たら whenIdle 待ちも hold も破棄。

### D. 掛け合いパターン（pattern 1-4）
- 種類: 1.main→sub / 2.sub→main / 3.main→sub→main / 4.sub→main→sub。
- **吹き出しは2つのまま**（main 用・sub 用）。3ターン目は最初の話者の同じ吹き出しに **`\n\n`（1行空け）** で追記。
- **重要**: 掛け合いの順番は「表示順」ではなく**内容の性質**。表示だけ入れ替えると、main 先行で書かれた内容が
  sub 先行表示と食い違う（例: pattern2 でサブが先に出るのに中身はメインが先に話している）。よって:
  - advanced（run_advanced）: **pattern を LLM 生成の前に抽選**（重み [1:40,2:30,3:18,4:12]、banter::pick_pattern_full）し、
    プロンプトの「# 掛け合いの順番」でその話す順を指定して、その順番で内容を書かせる。
    JSON キーはキャラ固定（main=メイン, sub=サブ）。extra は3ターン目（最初に話したキャラの2言目）。
    pattern∈{3,4} のときのみ dialogue_schema が extra を required にし、プロンプトで extra を必須化。
    パース後 extra(text非空) が無ければ 3→1 / 4→2 に降格。
  - 辞書系（low_mode の match/fallback/error/recall/update、events の poke/boot/idle 等）・独り言（monologue のキャッシュ/
    辞書・restock 生成ペア）・リマインダー: 固定文は main 先行で書かれているため **常に pattern=1**（入れ替え不可）。
    変化（pattern2-4）は内容を生成できる advanced のみ。
  - DialogueResponse に `pattern: u8`(default 1) と `extra: Option<CharacterLine>`(default None)。
    フロントの順序組み立て（下記）は不変で、内容が順番に一致するので表示と食い違わない。
- **フロント描画順序（balloon）**: pattern と (main,sub,extra) から順序付きターン列を構築:
  - 1: [(main,mainLine)], [(sub,subLine)]
  - 2: [(sub,subLine)], [(main,mainLine)]
  - 3: [(main,mainLine)], [(sub,subLine)], [(main,extra)]
  - 4: [(sub,subLine)], [(main,mainLine)], [(sub,extra)]
  各ターンを順に、その話者の吹き出しへタイプライター描画。同一話者の2回目は既存内容に `\n\n` を足して追記。
  ターン描画開始時にその話者の pose をそのターンの pose へ更新し、`speaker.speak(slot, text)` を呼ぶ（TTSは順番に鳴る）。
  全ターン後に C の whenIdle→hold→一括 hide。

## 口パク（リップシンク。2026-06 追加分の契約）

静止画ベースのまま、発話中だけ口を「閉/開」の2フレームで切り替える（連続アニメではない）。
開口フレームが無いシェルは口パクせず従来どおり（完全後方互換）。

### 開口フレームの規約（シェル定義の変更不要）
- pose 画像が `<dir>/<name>.<ext>`（閉口）のとき、同じ場所に `<dir>/<name>_talk.<ext>` があれば**開口フレーム**として扱う。
- ghost.rs `build_boot_characters`: 各 pose について talk 兄弟ファイルがあれば data URL 化して集める。
- BootPayload の ShellCharacter に `talk_poses: Record<pose, dataUrl>` を追加（talk が在る pose のみ。無ければ空）。
  schema_version は据え置き（追加ファイルの有無だけ・既存シェルは talk_poses 空で従来動作）。

### フロント
- types.ts ShellCharacter に `talk_poses: Record<string, string>` 追加。
- characters.ts CharacterView: 各 pose の閉口 `<img>` に加え、talk_poses にある pose は開口 `<img>` も生成して重ねる。
  `setMouth(open: boolean)`: 現在 pose に開口フレームがあれば open に応じて閉/開 img を切替（無ければ no-op）。
  `setPose(pose)`: 口を閉に戻す。`destroy()` で開口 img も破棄。アルファマスク用 ImageData は閉口のものを使う（口の差分は微小なので再計算不要）。
- tts.ts VoicevoxSpeaker: 音声グラフを source→AnalyserNode→destination に変更。再生中 requestAnimationFrame で
  時間領域 RMS 振幅を計算し、しきい値（約 0.04）超で口開。`setMouthCallback(cb: (slot, open)=>void)` を持ち、
  再生中は cb(slot, open) を呼ぶ。各 playOne 終了・interrupt 時は cb(slot, false)。
  `isAudible(): boolean`（Voicevox active=true）を TtsSpeaker に追加（RuntimeSpeaker は委譲、Noop=false）。
  RuntimeSpeaker は mouth callback を保持し Voicevox 実体へ転送。
- balloon.ts: typeLine で `speaker.isAudible?.()` が false のときのみ、その slot の口を約 120ms 間隔でパクパク
  （タイプ中）→ 行終わり/世代交代で口閉。true のときは TTS が口を駆動するので balloon は口に触れない。
  show() の interrupt・hideAll で両 slot の口を閉じる。
- main.ts: `speaker.setMouthCallback?.((slot, open) => chars[slot].setMouth(open))` を登録（chars は再読込で差し替わる
  可変束縛を参照）。balloon は保持する chars 経由で口を駆動。

## ポモドーロ（集中タイマー。2026-06 追加分の契約）

集中(work)→休憩(break)をラウンド数だけ繰り返す。**集中中は雑談（ランダムトーク・放置反応）を止める**＝集中モード。
停止・完了で自動的に通常へ復帰。節目はキャラが声かけ（辞書 events）。

### Rust 側（新規 pomodoro.rs）
- AppState 追加: `pomodoro_focus: AtomicBool`（集中中 true）、`pomodoro_gen: AtomicU64`（キャンセル用世代）。
- `start_pomodoro` コマンド: settings の work/break/rounds を読み、gen を ++ して状態機械タスクを spawn（旧タスクは gen 不一致で終了）。
- `stop_pomodoro` コマンド: gen を ++（実行中タスクを止める）、focus=false、`pomodoro` イベント {phase:"idle"} を emit。
- 状態機械（tokio タスク、1 秒刻みで cancel チェック）:
  round 1..=rounds で: focus=true → 1 秒ごとに残り秒を減らし `pomodoro` {phase:"focus",remaining,round,rounds} emit
  → work 終了で focus=false。round==1 開始時に events.focus_start を再生。
  最終ラウンド以外は focus 終了で events.focus_end → break（同様に毎秒 emit）→ break 終了で events.break_end。
  全 round 後に events.pomodoro_done。各タイミングの台詞は **emit_and_log で should_stay_quiet を無視して再生**
  （ユーザーが始めたタイマーなので静音中も鳴らす）。gen 不一致で即 return（focus は stop 側が false 化）。
- `should_stay_quiet`（presence.rs）に集中チェックを追加: app から AppState を取得し pomodoro_focus が true なら true を返す。
- BootPayload に現在状態は載せない（起動時は idle 前提。必要なら get_pomodoro_status コマンドで取得）。
  `get_pomodoro_status() -> PomodoroStatus` も用意（フロント再読込時の同期用）。
- tray.rs: メニューに「ポモドーロ開始」「ポモドーロ停止」を追加（apply 経由でコマンドと同じ処理を呼ぶ）。

### イベント
- `pomodoro`: `PomodoroStatus`（phase/remaining_sec/round/rounds）。開始・毎秒・節目・停止で emit。
- 節目の台詞は既存の `dialogue` + `speak` 経路（emit_and_log）で流れる（追加イベント不要）。

### フロント
- types.ts: Settings に pomodoro_* 追加、`PomodoroStatus` 追加。
- index.html: `#pomodoro-badge`（.solid、既定 hidden）をステージ上部に追加。
- 新規 pomodoro.ts or main.ts 内: `pomodoro` イベント listen。phase!="idle" でバッジ表示
  （"🍅 集中 MM:SS" / "☕ 休憩 MM:SS"、round/rounds 併記）、idle で hide。remaining はイベントの値をそのまま表示
  （毎秒来るのでローカルタイマー不要）。**バッジクリックで stop_pomodoro**（停止）。
- settings.ts「会話・ふるまい」カテゴリに「ポモドーロ」: 集中(分)/休憩(分)/回数の数値入力（collectForm/syncForm へ）
  ＋「開始」「停止」ボタン（invoke start_pomodoro/stop_pomodoro）。ヘルプに「集中中は雑談を止めます」。
- main.ts: boot で get_pomodoro_status を取得しバッジ初期化（再読込時の同期）。

## UI 配置（ウインドウ 760×560 内）

下端にキャラ 2 体（main 右・sub 左、各 ~280×420）。吹き出しは各キャラの頭上。
入力欄は下端中央（トグル表示）。設定/ログパネルは中央オーバーレイ。すべて透過背景の上に絶対配置。
