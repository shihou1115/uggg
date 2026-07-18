# ugg アーキテクチャ設計書（architecture.md v1.4）

**フェーズ**: 本開発 Phase 2 確定版
**作成日**: 2026-06-18
**根拠**: [spec.md](spec.md) v1 で確定した要件と、Phase 2 対話で確定した設計判断
**位置付け**: **実装契約の正本**。「何を作るか」は [spec.md](spec.md)、「テスト戦略」は [test-plan.md](test-plan.md)（Phase 3）。

---

## 0. 本書の使い方

- 本書は **「どう作るか」** を定義する。要件レベルの判断は spec.md に。
- **★** は v0.0.3 からの構造変更点。
- すべての設計判断には **理由** を併記する（後追いで「なぜこうしたか」が辿れること）。
- コード例は要所のみ。完全な型/シグネチャの正本は実装と本書の組合せで担保する。

---

## 1. モジュール構成

### 1.1 全体図

```
┌────────────────────── ugg.exe ──────────────────────┐
│                                                        │
│  ┌──── Frontend (WebView, TypeScript) ────────────┐   │
│  │   src/                                          │   │
│  │    main.ts                                      │   │
│  │    types.ts                                     │   │
│  │    stage/ (character/pose/alphamask/scale)      │   │
│  │    dialogue/ (balloon/input/typewriter/chatlog) │   │
│  │    tts/ (speaker/mouth/credit)                  │   │
│  │    panels/ (settings/onboarding)                │   │
│  │    menu/ (context-menu)                         │   │
│  │    interaction/ (click/poke/nade/drag)          │   │
│  │    system/ (toast/ghost-speech)                 │   │
│  └────────────────── ↕ Tauri IPC ──────────────────┘   │
│  ┌──── Backend (Rust) ─────────────────────────────┐   │
│  │   src-tauri/src/                                │   │
│  │    main.rs (コマンド/イベント配線のみ・薄い)   │   │
│  │    state.rs (AppState コンテナ)                 │   │
│  │    db.rs                                        │   │
│  │    commands/ (1コマンド1ファイル目安)          │   │
│  │    dialogue/ (low/advanced/llm/monologue/banter)│   │
│  │    ghost/ (manifest/dict/asset_dnd)             │   │
│  │    tts/ (engine/voicevox/irodori/preprocess)    │   │
│  │    presence/ (idle/quiet/window_pos)            │   │
│  │    window/ (mask/tray)                          │   │
│  │    system/ (secrets/cost/update/topics/notify)  │   │
│  │    tools/ (clock/reminder/clipboard)            │   │
│  └─────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────┘
              ↕ HTTP (FastAPI, /v1/audio/speech 互換)
┌──── サイドカー: irodori (Python) ──────────────────────┐
│   %APPDATA%\ugg\irodori\sidecar.py                     │
│   Portable Python + PyTorch + CUDA + Irodori モデル    │
└────────────────────────────────────────────────────────┘
```

### 1.2 バックエンド ディレクトリ構造

```
src-tauri/src/
├── main.rs                  -- エントリポイント。コマンド/イベント配線 + setup フックのみ。実装ロジックは持たない
├── state.rs                 -- AppState（サブ状態のコンテナ）と各サブ状態の定義
├── db.rs                    -- SQLite 接続・マイグレーション・低レベルクエリ
│
├── commands/                -- 各 Tauri コマンドの実装
│   ├── mod.rs
│   ├── boot.rs              -- get_boot_payload
│   ├── lifecycle.rs         -- frontend_ready, quit_app, hide_window, set_autostart
│   ├── settings.rs          -- set_settings, get_settings
│   ├── secrets.rs           -- set_api_key, has_api_key, delete_api_key
│   ├── dialogue.rs          -- send_user_message
│   ├── interaction.rs       -- poke, nade
│   ├── profile.rs           -- get_profile, add_profile, delete_profile
│   ├── tts.rs               -- synthesize_voice, list_voices, voicevox_assets_ready, download_voicevox_assets, irodori 系, voice_ref 系
│   ├── reader.rs            -- reader_load_text, set_reading_active
│   ├── assets.rs            -- list_ghosts, list_shells, dnd_install
│   ├── pomodoro.rs          -- start_pomodoro, stop_pomodoro, get_pomodoro_status
│   ├── daily.rs             -- ★M7/M8 リマインダー + ToDo 管理 (§4.11)
│   ├── tools.rs             -- read_clipboard_text（リマインダー系は M7 で daily.rs へ移設）
│   ├── data.rs              -- get_chat_log, clear_history, export_data, check_update_now
│   ├── topics.rs            -- get_interests, set_interests, fetch_topics_now
│   ├── onboarding.rs        -- complete_onboarding, skip_onboarding
│   └── window.rs            -- update_alpha_mask
│
├── dialogue/                -- 対話エンジン
│   ├── mod.rs               -- persist_and_speak, モード判定
│   ├── low.rs               -- 辞書ベース
│   ├── advanced.rs          -- LLM 経由
│   ├── llm.rs               -- OpenAI 互換クライアント（プロバイダ抽象なし）
│   ├── monologue.rs         -- 独り言（advanced キャッシュ補充・low 辞書選択）
│   └── banter.rs            -- 掛け合いパターン制御 (1-4 + question_curiosity)
│
├── ghost/                   -- ゴースト/シェル/辞書ロード
│   ├── mod.rs
│   ├── manifest.rs          -- ghost.json / shell.json パース
│   ├── dict.rs              -- 辞書スキーマ v3 パース、when 条件評価
│   └── asset_dnd.rs         -- ★ DnD 展開（zip/フォルダ、zip slip 対策）
│
├── tts/                     -- TTS
│   ├── mod.rs               -- trait TtsEngine, 振り分け
│   ├── voicevox.rs          -- voicevox_core 埋め込み（libloading + プリビルド C API）
│   ├── irodori.rs           -- Irodori サイドカー HTTP クライアント
│   ├── preprocess.rs        -- 漢字→ひらがな変換（voicevox_core の OpenJtalk を流用）
│   ├── reader.rs            -- テキスト読み上げ: .txt 読込 + チャンク分割 + .md 台本対応（text-reader-spec.md / script-reader-spec.md）
│   ├── script.rs            -- ★ .md 台本形式パース + 検証（フェンス抽出・ScriptError。script-reader-spec.md）
│   ├── download.rs          -- 公式ダウンローダ起動（既定 voicevox_core 資産）
│   └── voice_ref.rs         -- ★ Irodori 参照音声管理（生成・保存・削除）
│
├── presence/                -- 存在感系
│   ├── mod.rs
│   ├── idle.rs              -- 30 分無操作 → events.idle
│   ├── quiet.rs             -- 静音モード判定（quiet_mode / フルスクリーン / ポモドーロ集中 / 読み上げ中）
│   ├── context.rs           -- ★M9 OS 状況検知（GetLastInputInfo / GetSystemPowerStatus）+ 閾値判定の純関数
│   └── window_pos.rs        -- ステージのドック（作業領域下端全幅に固定・1秒監視で再ドック・モニタ記憶）
│
├── window/                  -- ウインドウ管理
│   ├── mod.rs               -- create_main_window
│   ├── mask.rs              -- クリック透過ポーリング（50ms, set_ignore_cursor_events）
│   └── tray.rs              -- タスクトレイ・メニュー
│
├── system/                  -- 共通基盤
│   ├── secrets.rs           -- keyring ラッパ
│   ├── cost.rs              -- LLM コスト追跡・上限警告・自動降格
│   ├── update.rs            -- 更新通知
│   ├── topics.rs            -- 時事ネタ RSS 取得
│   ├── notify.rs            -- ★ 統合通知サービス notify()（横断方針 §3.1 ゴースト発話原則）
│   ├── deliver.rs           -- ★M7 通知配達サービス deliver_event（自発発話の単一経路、§11.4）
│   ├── governance.rs        -- ★M7 発話ガバナンス can_deliver / record_delivered（§11.4）
│   └── calendar.rs          -- ★M10 ICS 取得・自前パース・RRULE near-term 展開（読み取り専用、§4.6.4）
│
└── tools/                   -- ツール群
    ├── mod.rs
    ├── clock.rs             -- 時刻注入（tools_enabled 時のみ）
    ├── reminder.rs          -- ★M7 統合リマインダー: 自然文パーサ parse_reminder + 次回計算 + TZ 変換（常時ローカル、tools_enabled から独立）
    ├── todo.rs              -- ★M8 ToDo・日課: bucket/priority/recurring 検証 + 日課復活の境界計算（daily_support 配下）
    └── clipboard.rs         -- クリップボード補助（tools_enabled 時のみ）
```

### 1.3 フロントエンド ディレクトリ構造

```
src/
├── main.ts                  -- boot 配線のみ
├── types.ts                 -- Rust と一致する共有型
│
├── stage/                   -- ステージ・ウインドウ
│   ├── character.ts         -- キャラ DOM 管理
│   ├── pose.ts              -- pose 切替（visible クラス）
│   ├── alphamask.ts         -- 8px グリッド合成 → update_alpha_mask
│   └── scale.ts             -- 表示スケール（レイヤー分離方式 §10）
│
├── dialogue/                -- 対話 UI
│   ├── balloon.ts           -- 吹き出し（最大3つ、§10）
│   ├── input.ts             -- チャット入力
│   ├── typewriter.ts        -- タイプライター描画（速度可変）
│   └── chatlog.ts           -- ログパネル
│
├── tts/                     -- TTS フロント
│   ├── speaker.ts           -- TtsSpeaker / NoopSpeaker / EngineSpeaker（全 slot 直列の発声キュー + 先読み 1）
│   ├── mouth.ts             -- 口パク（振幅駆動のみ、§A-4）
│   └── credit.ts            -- VOICEVOX クレジット表示
│
├── panels/                  -- UI パネル
│   ├── settings/
│   │   ├── index.ts         -- 全体管理（タブ管理）
│   │   ├── general.ts       -- モード・自動起動・スケール等
│   │   ├── llm.ts           -- プロバイダ・モデル・APIキー
│   │   ├── voice.ts         -- TTS 設定（voicevox / irodori）
│   │   ├── interests.ts     -- 時事ネタ・興味分野
│   │   └── about.ts         -- バージョン・ライセンス
│   ├── daily.ts             -- ★M7/M8 予定・ToDo パネル（リマインダー節 + ToDo 節: 3 バケットタブ・チェック完了・優先度/日課トグル）
│   └── onboarding.ts        -- 初回オンボーディング
│
├── menu/
│   └── context-menu.ts      -- ★ 右クリック→バルーン内メニュー（C-5、spec §4.3.5）。M7 で「予定・リマインダー」項目追加
│
├── interaction/             -- 操作
│   ├── click.ts             -- クリック種別判別
│   ├── poke.ts              -- つつき
│   ├── nade.ts              -- 撫で
│   └── drag.ts              -- ドラッグ
│
└── system/
    ├── toast.ts             -- トースト表示（フォールバック用）
    └── ghost-speech.ts      -- ゴースト発話受信（dialogue リスナー薄ラッパ）
```

### 1.4 v0.0.3 からの主な構造変更

| 変更 | 理由 |
|---|---|
| ★ main.rs を「配線のみ」に薄く | v0.0.3 は 1300 行超で肥大化、ロジックを各モジュールへ |
| ★ commands/ ディレクトリ化 | コマンド追加時の影響範囲を限定 |
| ★ dialogue/ tts/ presence/ window/ system/ tools/ をディレクトリ化 | 関連ファイルを近接、横移動削減 |
| ★ 設定パネル UI を分割（general/llm/voice/interests/about） | v0.0.3 の settings.ts は 1500 行超 |
| ★ system/notify.rs 新設 | 横断方針「ゴーストに喋らせる」を 1 箇所集約 |
| ★ ghost/asset_dnd.rs 新設 | DnD 展開（新機能） |
| ★ tts/voice_ref.rs 新設 | Irodori 参照音声管理（新機能） |
| ★ tts/preprocess.rs 新設 | 漢字→ひらがな変換 |
| ★ tts_engine.rs 廃止 → tts/mod.rs に統合 | 3エンジン抽象（v0.0.3）から 2 エンジン trait へ |
| ★ openai_tts.rs 廃止 | openai_compat エンジンを spec で削除 |
| ★ stt.rs 廃止 | STT を spec で削除 |
| ★ secrets.rs / cost.rs / update.rs / topics.rs を system/ 配下に集約 | 共通基盤として明示 |

---

## 2. データモデル (SQLite)

### 2.1 テーブル一覧

| テーブル | 用途 | 行数想定 | 主用途 |
|---|---|---|---|
| `app_settings` | キー/値ストア | 数十 | Settings JSON + 個別キー |
| `chat_log` | 会話ログ | 〜数万 | UI 表示・エクスポート・要約 |
| `user_profile` | 長期記憶 | 〜数百 | system prompt 注入・recall |
| `interest_topics` | 時事ネタ興味分野 | 〜20 | RSS 検索キーワード |
| `api_usage` | LLM コスト追跡 | 〜数万 | 月次集計・上限警告 |
| `topics_cache` | 時事ネタ見出しキャッシュ | 〜数百 | advanced 独り言混入の材料（M5-C。旧記載の monologue_cache は未実装のまま廃案） |
| `reminders` | リマインダーのスケジュール定義 | 〜数十 | active=1 かつ due_ts 到達で発火（★M7 拡張） |
| `reminder_log` | ★M7 リマインダー発火・確認履歴 | 〜500（prune） | 完了/未完了管理・通知履歴・再通知判断 |
| `todos` | ★M8 ToDo・日課 | 〜数百 | 3 バケット・2 段階優先度・日課復活 |
| `calendar_cache` | ★M10 ICS 予定の発生インスタンス | 〜数百 | (source_id,uid,start_ts) 複合キー・near-term 展開 |
| `voice_refs` | ★ Irodori 参照音声メタ | 最大 2（slot 1件ずつ） | クローン合成の元音声 |

### 2.2 各テーブル詳細

#### `app_settings`（v0.0.3 と同形式、追加キー）
```sql
CREATE TABLE app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```
- `"settings"` キーに Settings 構造体全体を JSON で保存（フィールド追加に DDL 不要）
- 個別キー: `window_pos`（{x,y}。ステージのドック先モニタの記憶に使う）, `char_pos`（{main,sub} キャラごとの X 位置 CSS px）, `first_boot_done`（"1"）, `last_update_check`（unix秒）, `profile_onboarded`（"1"）, `update_notice_seen:<version>`（"1"）等

#### `chat_log`
```sql
CREATE TABLE chat_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,         -- unix秒
    mode TEXT NOT NULL,          -- "low" | "advanced"
    role TEXT NOT NULL,          -- "user" | "main" | "sub"
    text TEXT NOT NULL,
    pose TEXT                    -- main/sub のときのみ
);
CREATE INDEX idx_chat_log_ts ON chat_log(ts);
```

#### `user_profile`（★ origin 拡張、source_keywords 追加）
```sql
CREATE TABLE user_profile (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    origin TEXT NOT NULL,        -- "manual" | "onboarding" | "auto"
    source_keywords TEXT,        -- カンマ区切り、recall トリガー用
    ts INTEGER NOT NULL          -- 追加時刻
);
CREATE INDEX idx_user_profile_origin ON user_profile(origin);
```

**容量管理 (B-5/B-6 統合)**:
- **advanced モード時**: 件数 > 上限（例 200）で発火する要約サイクル
  - 古い origin='auto' を LLM で複数件 → 1 件に集約
  - 手動追加 / オンボーディング由来は保護（要約対象外）
- **low モード時**: 件数上限のみ
  - origin='auto' から古いものを単純削除
  - LLM 不可なので要約不可
- 上限値は app_settings の `profile_max_count` で調整可、既定 200
- 詳細実装パラメータ（要約対象件数、トリガー閾値）は実装段階で調整

#### `voice_refs`（★ 新規）
```sql
CREATE TABLE voice_refs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slot TEXT NOT NULL,          -- "main" | "sub"
    caption TEXT NOT NULL,       -- 生成に使ったキャプション
    file_path TEXT NOT NULL,     -- %APPDATA%\ugg\irodori\refs\<id>.wav
    created_ts INTEGER NOT NULL,
    UNIQUE(slot)                 -- MVP は slot ごと最新1件のみ
);
```

#### `reminders`（★M7 v6 拡張）+ `reminder_log`（★M7 新設）

**発火 ≠ 完了**（daily-support-design §2.1）: reminders はスケジュール定義、発火と完了/無視の
履歴は reminder_log に分離する。時刻はすべて UTC 秒、繰り返しの time_of_day は
ローカル 0:00 からの秒（TZ 契約は daily-support-design §2.5）。

```sql
-- v6 で既存 reminders(id, due_ts, text, created_ts) に追加
kind         TEXT    NOT NULL DEFAULT 'once',  -- 'once'|'daily'|'weekly'
weekday_mask INTEGER NOT NULL DEFAULT 0,       -- weekly: bit0=月..bit6=日
time_of_day  INTEGER NOT NULL DEFAULT 0,       -- daily/weekly: ローカル 0:00 からの秒
active       INTEGER NOT NULL DEFAULT 1,       -- 0 = 再発火停止（once の発火到達/完了/無視）
base_due_ts  INTEGER,                          -- スヌーズ前の本来時刻（NULL = スヌーズなし）

CREATE TABLE reminder_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    reminder_id INTEGER NOT NULL,
    fired_ts    INTEGER NOT NULL,                 -- 発火（配達到達）時刻
    ack         TEXT    NOT NULL DEFAULT 'fired', -- 'fired'|'completed'|'dismissed'
    ack_ts      INTEGER,
    delivery    TEXT    NOT NULL DEFAULT 'ghost'  -- DeliveryOutcome: 'ghost'|'toast'|'deferred'|'failed'
);
CREATE INDEX idx_reminder_log_rid ON reminder_log(reminder_id);
```

- **未完了 = ack='fired' の行が残っている**（一覧の pending 導出列・再通知判断の根拠）
- watcher は**到達（ghost|toast）時のみ** log_fire する。未達（deferred|failed）は
  active 維持のまま次ポーリングで再試行し、ログは残さない（10 秒間隔の再試行が
  reminder_log を押し流すのを防ぐ実装判断。設計書 §7.1 の記述より狭い）
- 保持は新しい順 500 行（prune_reminder_log）

#### `todos`（★M8 v7 新設、daily-support-design §2.2）

```sql
CREATE TABLE todos (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    text       TEXT    NOT NULL,
    bucket     TEXT    NOT NULL DEFAULT 'today',   -- 'today'|'week'|'someday'
    priority   INTEGER NOT NULL DEFAULT 0,         -- 0=普通, 1=高（2 段階のみ）
    recurring  TEXT,                               -- NULL|'daily'|'weekly'（日課）
    status     TEXT    NOT NULL DEFAULT 'open',    -- 'open'|'done'
    done_ts    INTEGER,
    created_ts INTEGER NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0          -- 同 bucket 内の並び（追加時 max+1）
);
CREATE INDEX idx_todos_status ON todos(status, bucket);
```

- bucket/status/recurring は文字列のまま持ち、正規化・検証は `tools/todo.rs`（TS の文字列 union と 1:1）
- **日課の復活**: done かつ recurring 非 NULL の行を、daily = 今日のローカル 0:00 より前・
  weekly = 今週月曜のローカル 0:00 より前に done なら open へ戻す
  （`reset_recurring_todos(daily_cutoff, weekly_cutoff)`。cutoff 計算は tools/todo.rs、TZ 契約 §2.5）。
  実行タイミングは daily watcher（起動時 + ローカル日付の変更検知、§11.4）
- 一覧の並び: open 先 → priority 高い順 → sort_order 昇順

#### 既存（変更なし）
- `interest_topics(id, topic, enabled)`
- `topics_cache(id, topic, headline, link, fetched_ts)` UNIQUE(topic, headline)
- `api_usage(id, ts, provider, model, prompt_tokens, completion_tokens, cost_usd)`

#### 廃止
- `context_summaries` ← user_profile (origin='auto') に統合
- STT 関連テーブルは v0.0.3 に存在せず

### 2.3 マイグレーション

- DB スキーマバージョンを `app_settings` の `"db_schema_version"` キーで管理
- 起動時に値を読み、必要なら up マイグレーションを順次適用（v0.0.3 からの DB 移行は提供しない、本開発は新規 DB を使う）
  - v1: `app_settings` のみ (M0)
  - v2: `chat_log` / `user_profile` / `api_usage` を追加 (M2)
  - v3: `voice_refs` を追加 (M4c Phase A)
  - v4: `interest_topics` / `topics_cache` を追加 (M5-C)
  - v5: `reminders` を追加 (M5-B)
  - v6: `reminders` 拡張 5 列 + `reminder_log` を追加 (M7)
  - v7: `todos` を追加 (M8)
  - v8: `calendar_cache` を追加 (M10)。複合キー (source_id,uid,start_ts) + 実装追加列 `unsupported`（展開不能 RRULE の印）
- 参照音声 .wav の配置は `%APPDATA%\ugg\irodori\refs\<slot>_<id>.wav` (architecture §2.4)。`voice_refs.file_path` には絶対パスを保存

### 2.4 ファイル資産（DB 外）

| 場所 | 用途 |
|---|---|
| `%APPDATA%\ugg\companion.db` | SQLite 本体 |
| `%APPDATA%\ugg\voicevox\` | voicevox_core 資産（c_api / onnxruntime / dict / models） |
| `%APPDATA%\ugg\irodori\` | Irodori-TTS 資産（python / model / refs） |
| `%APPDATA%\ugg\irodori\refs\<id>.wav` | 参照音声本体（voice_refs.file_path から参照） |
| `%LOCALAPPDATA%\ugg\logs\` | アプリログ（tauri-plugin-log） |
| keyring `ugg` | API キー（provider 名で索引） |
| `<app>/ghosts/<id>/` | 同梱 + DnD 追加ゴースト |
| `<app>/shells/<id>/` | 同梱 + DnD 追加シェル |

---

## 3. AppState 設計

### 3.1 全体構造

```rust
pub struct AppState {
    pub db: Db,                                    // 共通
    pub settings: Mutex<Settings>,                  // 中央保持
    pub ghost: Mutex<GhostBundle>,                  // ghost/shell/dict
    pub dialogue: DialogueState,                    // 対話進行
    pub presence: PresenceState,                    // 存在感
    pub tts: TtsState,                              // エンジン保持
    pub pomodoro: PomodoroState,                    // ポモドーロ
    pub window: WindowState,                        // ウインドウ
    pub governance: GovernanceState,                // ★M7 発話ガバナンス（インメモリ）
}
```

※ 初版に載っていた `workers: WorkerHandles` は実装されていない（watcher は
`tauri::async_runtime::spawn` の投げ放しで管理し、ハンドル集約は不要だった）。

```rust
// system/governance.rs (★M7、M9 拡張)。backoff のみ app_settings に永続化
pub struct GovernanceState {
    pub last_spoke: AtomicI64,                       // 最後に自発発話が到達した unix 秒
    pub last_by_category: [AtomicI64; 9],            // カテゴリ別（SpeechCategory::index()）。段 5 が参照
    pub backoff: [AtomicU32; 9],                     // ★M9 🔕 回数。governance_backoff:<cat> から復元
    pub speech_seq: AtomicU64,                       // ★M9 speech_id 連番
    pub last_speech: Mutex<Option<(u64, SpeechCategory)>>, // ★M9 最新タグ付き発話（🔕 照合用）
}

// state.rs (★M9)。連続利用セッション等のインメモリ状態（再起動でリセット可）
pub struct ContextState {
    pub session_start: AtomicI64,      // 連続利用セッション開始（0 = アイドル中）
    pub break_prompted_secs: AtomicI64, // このセッションで最後に休憩促しした連続利用秒
    pub battery_notified: AtomicBool,  // バッテリー低下通知済み（AC/20% 超回復で解除）
}
```

### 3.2 サブ状態の詳細

```rust
pub struct DialogueState {
    pub busy: Arc<Semaphore>,                       // permits=1
    pub last_interaction: AtomicI64,
    pub degraded_until: AtomicI64,                  // 一時降格期限（unix 秒）
    pub error_streak: AtomicI64,                    // API エラー連続回数
    pub cost_limited_emitted: AtomicBool,           // 上限超過通知済みフラグ
    pub greeted: AtomicBool,                        // 起動挨拶済み
}

pub struct PresenceState {
    pub idle_fired: AtomicBool,                     // 現放置期間で発火済か
    pub win_x: AtomicI64,
    pub win_y: AtomicI64,
    pub pos_known: AtomicBool,
    pub pos_dirty: AtomicBool,                      // 3秒デバウンス保存用
}

pub struct TtsState {
    pub voicevox: Mutex<Option<VoicevoxEngine>>,    // 遅延 init
    pub irodori: Mutex<Option<IrodoriClient>>,      // 遅延 init（サイドカー起動含む）
    pub openjtalk_for_preprocess: Mutex<Option<OpenJtalkRc>>,  // 漢字→かな専用
}

pub struct PomodoroState {
    pub focus: AtomicBool,                          // 静音判定で参照
    pub gen: AtomicU64,                             // タスクキャンセル用世代
    pub phase: AtomicU32,                           // 0=focus, 1=break, 2=idle
    pub remaining: AtomicU32,
    pub round: AtomicU32,
    pub rounds: AtomicU32,
}

pub struct WindowState {
    pub alpha_mask: Mutex<DecodedMask>,             // クリック透過判定
    pub scale_milli: AtomicI64,                     // display_scale × 1000
    pub tray: std::sync::Mutex<Option<TrayHandles>>,// トレイメニュー同期
}

pub struct WorkerHandles {                          // ★ v0.0.3 では AppState 直下平坦
    pub bg_tx: mpsc::UnboundedSender<BgTask>,
    pub interval_tx: watch::Sender<u64>,            // ランダムトーク間隔通知
    pub topics_tx: watch::Sender<u64>,              // 時事ネタ取得間隔通知
}

pub struct GhostBundle {
    pub manifest: GhostManifest,
    pub shell: ShellManifest,
    pub dictionary: Dictionary,
}
```

### 3.3 ライフサイクル

```
[boot]
  ├─ DB open, settings 読み込み（"settings" キー）
  ├─ ghost/shell/dict ロード（initial 値で GhostBundle 構築）
  ├─ AppState::new() で全サブ状態を初期化
  ├─ Tauri builder.manage(Arc::new(state))
  └─ setup フックで:
       ├─ ウインドウ生成
       ├─ クリック透過ポーリング起動（window/mask）
       ├─ presence::spawn_idle_watcher
       ├─ presence::spawn_dock_keeper
       ├─ tasks::spawn_random_talk (interval_rx)
       ├─ tasks::spawn_topics_fetcher (topics_rx)
       ├─ update::spawn_update_check
       └─ tts::spawn_voicevox_preinit（事前 init）

[通常運用]
  ├─ Tauri コマンド → commands/* → 各サブ状態 / DB
  ├─ バックグラウンドタスクは bg_tx 経由で busy ゲートを参照
  └─ notify(kind, args) でゴースト発話 or トースト

[終了]
  ├─ trayから「終了」→ events.quit 発話を待ってから exit
  ├─ presence::persist_window_pos（即時保存）
  ├─ irodori サイドカーの正常終了（HTTP DELETE /shutdown）
  └─ DB クローズ
```

---

## 4. Tauri コマンド契約

エラーはすべて `Result<T, String>`（ユーザー提示可能な日本語メッセージ）。

### 4.1 boot / lifecycle

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_boot_payload` | なし | `BootPayload` | キャラ画像（data URL）、settings、`char_positions`（保存済みキャラ X 位置。無ければ null）等 |
| `frontend_ready` | なし | `()` | boot 完了通知。起動挨拶（first_boot or boot）+ 更新チェック起動 |
| `quit_app` | なし | `()` | 右クリックメニュー「終了」。Irodori サイドカーを best-effort shutdown 後に exit |
| `hide_window` | なし | `()` | メインウインドウを hide（トレイから再表示） |
| `set_autostart` | `enabled: bool` | `()` | OS 自動起動の切替（tauri-plugin-autostart） |

### 4.2 settings

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `set_settings` | `settings: Settings` | `Settings` | 値の clamp（display_scale, tts_speed/volume 等）+ 永続化 + 後処理。**clamp 後の確定値を返す**（フロントはこれを保存済み値として反映） |
| `get_settings` | なし | `Settings` | |
| `set_api_key` | `provider, key: String` | `()` | keyring 保存 |
| `has_api_key` | `provider: String` | `bool` | |
| `delete_api_key` | `provider: String` | `()` | |

### 4.3 dialogue

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `send_user_message` | `text: String` | `DialogueResponse` | モード判定・降格制御 |

**注**: 旧設計の `send_with_clipboard` / `read_clipboard` は不採用。クリップボード連携は `read_clipboard_text`（§4.9）でフロントが本文を取得して入力欄に貼り付け、通常の `send_user_message` で送信する方式に統合した。

**★M7**: dispatch 冒頭で `daily_support_enabled` のとき `tools::reminder::parse_reminder` を先に試し、予定表現なら LLM を経ずに登録 + 確認応答（`reminders-changed` emit）。従来の `tools_enabled` ゲートは撤廃（spec §4.2.1 不変条件: リマインダーは advanced 非依存の常時ローカル）。

### 4.4 interaction

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `poke` | `target: "main"\|"sub", region: "head"\|"chest"\|"body", rapid: bool` | `DialogueResponse \| null` | C-2 で縦のみ |
| `nade` | `target: "main"\|"sub", region: "head"\|"chest"\|"body"` | `DialogueResponse \| null` | 同上、撫で |
| `input_prompt` | `target: "main"\|"sub"` | `SpeechTurn \| null` | クリック時の入力促し（spec §4.3.1）。辞書 `input_prompt` から抽選し chat_log に記録。**dialogue イベントは emit しない**（フロントが renderPrompt で描画）。辞書未定義・sub 無しゴーストの sub は null |
| `menu_prompt` | `target: "main"\|"sub"` | `SpeechTurn \| null` | 右クリックメニューの前口上（main）/ メインへの誘導（sub）（spec §4.3.5）。抽選・記録・非 emit の挙動は `input_prompt` と同じ（実装も共通ヘルパー） |

### 4.5 profile

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_profile` | なし | `ProfileEntry[]` | |
| `add_profile` | `content: String` | `ProfileEntry[]` | origin="manual" |
| `delete_profile` | `id: i64` | `ProfileEntry[]` | |

### 4.6 log / data

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_chat_log` | `limit: u32` | `LogEntry[]` | 新しい順 |
| `clear_history` | `include_profile: bool` | `ClearResult` | |
| `export_data` | `include_profile: bool` | `String` | 保存パス返却 |
| `check_update_now` | なし | `()` | 設定パネル「いますぐチェック」。`update_feed_url` 未設定なら Err、結果は notify 経由で発話 |

**注**: 旧設計の `open_log_dir` は不採用（ログ閲覧はアプリ内チャットログパネル + `export_data` で代替）。

### 4.7 tts

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `synthesize_voice` | `text: String, slot: "main"\|"sub", caption: String\|null（省略可）` | `String` | WAV を base64 で返す（slot 基準、エンジン振り分けはバックエンド）。★ `caption` は Irodori 実モデルのみ使用（他経路は無視、空文字は None 正規化。script-reader-spec.md §3.3） |
| `list_voices` | なし | `VoiceOption[]` | 現在エンジンの声一覧 |
| `voicevox_assets_ready` | なし | `bool` | 資産有無 |
| `download_voicevox_assets` | `agreed: bool, gh_token: String\|null` | `()` | 規約同意必須、進捗は `voicevox-download` イベント |
| `set_github_token` | `token: String` | `()` | |
| `has_github_token` | なし | `bool` | |
| `delete_github_token` | なし | `()` | |
| `irodori_check_gpu` | なし | `GpuInfo` | ★ 起動時 GPU 検出（Q3 対応） |
| `irodori_assets_ready` | なし | `bool` | ★ |
| `download_irodori_assets` | `agreed: bool` | `()` | ★ 進捗 `irodori-download` |
| `voice_ref_generate` | `slot: String, caption: String` | `VoiceRef[]` | ★ Irodori 参照音声生成（同期完了、進捗イベントなし）。完了後の一覧を返す |
| `voice_ref_list` | なし | `VoiceRef[]` | ★ |
| `voice_ref_delete` | `slot: String` | `VoiceRef[]` | ★ 削除後の一覧を返す |
| `voice_ref_preview` | `slot: String, text: String` | `String` | ★ 既存参照音声でプレビュー（WAV base64） |
| `reader_load_text` | `path: String` | `ReadingChunk[]` | ★ テキスト読み上げ: 拡張子で分岐（.txt=プレーン読み / .md=台本形式。script-reader-spec.md）。演出メタ付きチャンク配列を返す（spec §4.5.8） |
| `set_reading_active` | `active: bool` | `()` | テキスト読み上げ: 読み上げ中フラグ（撫で抑制等に使用） |

### 4.8 assets

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `list_ghosts` | なし | `AssetEntry[]` | |
| `list_shells` | なし | `AssetEntry[]` | |
| `dnd_install` | `paths: String[], overwrite: bool` | `DndResult` | ★ DnD で受けたパスを ghost/shell に展開（§12）。`overwrite=false` で競合を検知して `DndResult.conflicts` に振り分け、ユーザー確認後 `overwrite=true` で再実行する |

**注**: `reload_assets` は提供しない。インストール/切替後は再起動の動線を notify でゴーストが案内する（§12）。

### 4.9 pomodoro / tools / topics

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `start_pomodoro` | なし | `()` | settings の work/break/rounds で開始（idle 時）。GUI パネルの「開始」（spec §4.4.5） |
| `stop_pomodoro` | なし | `()` | idle に戻す（破棄）。GUI パネルの「中断」 |
| `pause_pomodoro` | なし | `()` | 進行中のカウントダウンを一時停止（残り時間を保持）。GUI「停止」 |
| `resume_pomodoro` | なし | `()` | 一時停止から同じ残り時間で再開。GUI「停止」の再押下 |
| `get_pomodoro_status` | なし | `PomodoroStatus` | `phase` / `remaining_sec` / `round` / `rounds` / `paused: bool` |
| `read_clipboard_text` | なし | `String` | クリップボードのテキスト取得（入力欄への貼り付け用）。非テキストは空文字、`tools_enabled = false` なら Err |
| `get_interests` | なし | `InterestTopic[]` | |
| `set_interests` | `topics: String[]` | `InterestTopic[]` | |
| `fetch_topics_now` | なし | `()` | |
| `complete_onboarding` | `nickname, interests, talk_style, topics_enabled` | `()` | |
| `skip_onboarding` | なし | `()` | |

### 4.11 daily（★M7 統合リマインダー、daily-support-design §7.1/§8.1）

戻り値の `ReminderEntry[]` は Active フィルタ（要対応 = active=1 or 未処理発火あり）の一覧。
変更系はすべて `reminders-changed` を emit する。`ReminderEntry` は DB `ReminderRow` と
同フィールド + 導出列 `pending: bool`（TS 型も同期、3 表現の同期）。

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `list_reminders` | `filter?: "active" \| "completed" \| "all"` | `ReminderEntry[]` | 省略時 active |
| `add_reminder_nl` | `text: String` | `ReminderEntry[]` | 自然文登録（会話経路と同じ `parse_reminder`）。解釈不能なら Err |
| `add_reminder` | `text, offset_secs` | `ReminderEntry[]` | 単発の内部 API（M5-B 互換） |
| `complete_reminder` | `id` | `ReminderEntry[]` | 最新の未処理発火を ack='completed'。once は再発火も停止 |
| `dismiss_reminder` | `id` | `ReminderEntry[]` | 同上で ack='dismissed' |
| `snooze_reminder` | `id, mins` | `ReminderEntry[]` | due を延長し base_due_ts に本来時刻を保持。辞書 `reminder_snoozed` で確認発話（ゲート非対象） |
| `delete_reminder` | `id` | `ReminderEntry[]` | reminder_log も削除 |
| `update_reminder` | `id, patch: { text?, due_ts? }` | `ReminderEntry[]` | 部分更新 |
| `get_reminder_log` | `id` | `ReminderLogRow[]` | 通知履歴（新しい順、最大 50） |

★M8 ToDo（spec §4.6.2）。戻り値の `TodoEntry[]` は全 bucket の一覧（open 先・優先度高先。
タブの件数表示のためフロントは全件を受けてクライアント側で bucket 絞り込み）。
変更系はすべて `todos-changed` を emit。

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `list_todos` | `bucket?: "today" \| "week" \| "someday"` | `TodoEntry[]` | 省略時 全件 |
| `add_todo` | `text, bucket, priority, recurring?` | `TodoEntry[]` | 値は tools/todo.rs で検証（無効値は Err） |
| `complete_todo` | `id` | `TodoEntry[]` | done + 労い発話 `todo_done`（deliver 経由・Ambient=集中/静音中は黙る）。二重完了は no-op |
| `reopen_todo` | `id` | `TodoEntry[]` | done → open（チェック解除）。done_ts クリア |
| `delete_todo` | `id` | `TodoEntry[]` | |
| `update_todo` | `id, patch: { text?, bucket?, priority?, recurring? }` | `TodoEntry[]` | 部分更新。recurring は「キー省略=変更なし / null=日課解除 / 'daily'\|'weekly'=設定」の三値 |

★M9 発話ガバナンス（spec §4.6.3）。

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `feedback_speech` | `speech_id: String, category: String` | `()` | 🔕「いまのは邪魔」。**最新のタグ付き発話と一致したときだけ**適用（誤適用は黙って無視）。backoff +1 を `governance_backoff:<category>` に永続化し gate 段 5 の間隔を線形延長、3 回でカテゴリトグルを OFF（settings 永続化 + settings-changed）。Situation* 以外は no-op |

★M10 カレンダー（spec §4.6.4、読み取り専用）。変更系は `calendar-changed` を emit。

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_calendar_events` | `days?: u32`（省略時 2＝今日明日） | `CalendarEvent[]` | 表示窓の予定を開始順で |
| `refresh_calendar` | — | `usize`（取得件数） | 全ソースを今すぐ再取得 |
| `add_calendar_source` | `source: CalendarSource`（`{kind:"file",path}`\|`{kind:"url",url}`） | `CalendarSource[]` | 追加。source_id は index ベースのため**キャッシュを全 clear**して再取得 |
| `remove_calendar_source` | `index: usize` | `CalendarSource[]` | 削除。同上でキャッシュ全 clear |

### 4.10 window

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `update_alpha_mask` | `mask: AlphaMask` | `()` | クリック透過用 |
| `set_char_positions` | `main: f64 \| null, sub: f64 \| null` | `()` | キャラごとの X 位置（ステージ内 CSS px、視覚ボックス左端）を `app_settings.char_pos` に保存。ドラッグ終了時に呼ぶ（spec §4.1.6 / §4.3.4） |

---

## 5. イベント契約

| イベント | payload | 用途 |
|---|---|---|
| `dialogue` | `DialogueResponse` | バック起点の発話（ランダムトーク・notify 経由の system 発話）。★M9: deliver 経由の発話には `speech_id` / `category` / `priority` / `feedback_allowed` が付く（🔕 用。ユーザー応答には付かない。TS 型は optional） |
| `settings-changed` | `Settings` | バック起点の設定変更（トレイ等から） |
| `open-settings` | なし | トレイ → 設定パネル |
| `pomodoro` | `PomodoroStatus` | 毎秒・節目 |
| `voicevox-download` | `string` | 資産DL 進捗行 |
| `irodori-download` | `string` | ★ Irodori DL 進捗行 |
| `system-toast` | `string` | notify() / deliver_event のトーストフォールバック。★M7 でフロント受け皿（`system/toast.ts`、#system-toast 帯）を実装（到達保証 Toast の成立要件） |
| `reminders-changed` | なし | ★M7 リマインダーの変更通知（登録・発火・完了等）。パネルが再取得 |
| `todos-changed` | なし | ★M8 ToDo の変更通知（追加・完了・日課復活等）。パネルが再取得 |
| `calendar-changed` | なし | ★M10 カレンダーの変更通知（取得・通知・ソース編集）。パネルが再取得 |

**注**:
- U3 採用により system 発話は `notify()` 内部で `dialogue` 経由に統合する（§11）。`system-toast` は辞書未定義時の fallback のみ。
- 旧設計の `mode-changed` / `thinking` / `voice-ref-progress` イベントは不採用。モードは `DialogueResponse.mode` で各応答に同梱、思考中表示は送信中の入力欄 disable で代替、参照音声生成は `voice_ref_generate` の同期完了で通知する。

---

## 6. 辞書スキーマ v3

### 6.1 全体構造

```yaml
schema_version: 3

input_match: [ ... ]
fallback: [ ... ]
recall: [ ... ]
monologue: [ ... ]
events:
  first_boot: [ ... ]
  boot: [ ... ]
  # ... (詳細は §6.2)
system_messages:
  cost_warning_80: [ ... ]
  # ... (詳細は §6.5)
input_prompt:          # キャラクリック時の入力促し (詳細は §6.2)
  main: [ ... ]
  sub: [ ... ]
menu_prompt:           # 右クリックメニューの前口上/誘導 (詳細は §6.2)
  main: [ ... ]
  sub: [ ... ]
```

### 6.2 セクション仕様

#### input_match（v2 rules 相当）
```yaml
input_match:
  - id: greeting
    keywords: ["こんにちは", "こんばんは"]
    priority: 10
    responses:
      - main: { text: "...", pose: happy }
        sub: { text: "...", pose: normal }     # サブ任意化対応で省略可
      - main: { text: "..." , pose: happy }
        sub: null                              # 明示的に「サブは喋らない」 (H2)
```

- マッチ: keywords 部分一致 + priority 最大の規則から 1 つ抽選
- pattern は常に 1（advanced 時のみパターン制御）

#### fallback
```yaml
fallback:
  - main: { text: "...", pose: troubled }
    sub: { text: "...", pose: normal }
```

#### recall（v2 recall_talk 相当）
- user_profile の `source_keywords` と入力のキーワード一致時にトリガー
- `{summary}` プレースホルダで user_profile.content を埋める

#### monologue（v2 random_talk 相当）
- 既定 10 分間隔（D-4）、advanced ではキャッシュ補充も併用

#### events
```yaml
events:
  # ライフサイクル
  first_boot:
    - main: { ... }
      sub: { ... }
  boot:
    - when: { hour_from: 5, hour_to: 11 }
      main: { ... }
      sub: { ... }
    - when: { date: "01-01" }
      main: { ... }
      sub: { ... }
    - main: { ... }                          # 無条件
      sub: { ... }
  quit: [ ... ]

  # 操作（縦のみ、横は廃止）
  poke_main: [ ... ]
  poke_main_head: [ ... ]
  poke_main_chest: [ ... ]
  poke_main_body: [ ... ]
  poke_sub: [ ... ]                          # サブ無しゴーストでは未到達
  poke_sub_head: [ ... ]
  poke_sub_chest: [ ... ]
  poke_sub_body: [ ... ]
  poke_rapid: [ ... ]
  nade_main: [ ... ]
  nade_main_head: [ ... ]
  nade_main_chest: [ ... ]
  nade_main_body: [ ... ]
  nade_sub: [ ... ]
  nade_sub_head: [ ... ]
  nade_sub_chest: [ ... ]
  nade_sub_body: [ ... ]

  # 問いかけ（B-4）
  question_curiosity: [ ... ]

  # 存在感
  idle: [ ... ]

  # ポモドーロ
  focus_start: [ ... ]
  focus_end: [ ... ]
  break_end: [ ... ]
  pomodoro_done: [ ... ]

  # ★M7 統合リマインダー（deliver_event 経由・プレースホルダ置換あり）
  reminder_fired: [ ... ]      # {body} = 登録本文。サブ無しシェルの縮退に備え main 側に {body} を含めること
  reminder_snoozed: [ ... ]    # {time} = 「10分後」等

  # ★M8 ToDo（deliver_event 経由・サブ主体・責めない言い回し）
  todo_morning: [ ... ]        # 朝の件数告知。{count} = today の未完了件数
  todo_done: [ ... ]           # 完了の労い。{body} = 完了した本文
  todo_follow: [ ... ]         # 未完了フォロー（M9 で発火: 14-18 時・1 日 1 回）。{body}
  todo_stale: [ ... ]          # 3 日以上滞留の再整理提案（M9 で発火: 18-22 時・1 日 1 回）。{body}

  # ★M9 状況発話（すべて既定 OFF のオプトイン・控えめな言い回し）
  situation_break: [ ... ]      # 休憩促し。{time} = 「90分」等の連続利用時間
  situation_late_night: [ ... ] # 深夜利用の声かけ（23-5 時・1 晩 1 回）
  situation_battery: [ ... ]    # バッテリー低下。{count} = 残り %
  todo_quit: [ ... ]            # ★M9 終了前確認: 未完了 today ToDo があるときの終了挨拶（quit の代替）。{count}

  # ★M10 カレンダー開始前通知（Notice=静音を越える。サブ主体）
  calendar_upcoming: [ ... ]    # {summary} = 予定名、{time} = 開始 HH:MM（終日は「今日」）
```

**★M7 プレースホルダ規約**（daily-support-design §3.3）: `system/deliver.rs` 経由の
events 発話では `{body}` `{count}` `{time}` `{summary}` を置換する。未知の
`{xxx}` は残さず空文字に落とす。従来の `pick_event` 直呼び経路（boot 等）では置換されない。

#### input_prompt / menu_prompt（促し系、spec §4.3.1 / §4.3.5）
```yaml
input_prompt:            # キャラ 1 クリック → 入力欄の導線
  main:
    - { text: "なにか用かな？", pose: happy }
  sub:
    - { text: "……ボクに用か", pose: normal }
menu_prompt:             # キャラ右クリック → バルーン内メニューの導線
  main:                  # メニューの前口上 (この下にメニュー項目が続く)
    - { text: "ご用件はどれかな？", pose: happy }
  sub:                   # サブ右クリック時の「メインに頼め」誘導
    - { text: "用事ならミミに頼んでくれ", pose: normal }
```

- どちらも **単発ターン**（Line ではなく SpeechTurn のリスト。掛け合いにしない）で、main / sub 各リストから無条件で 1 件抽選（when 非対応）
- セクション省略可（旧辞書互換）。input_prompt 無し → 促し無しで入力欄だけ開く。menu_prompt 無し → セリフ無しでメニューのみ表示
- 発話はコマンド戻り値をフロントが描画し、**閉じる操作まで吹き出しを保持**する（通常の hold 時間で消さない）
  - input_prompt → renderPrompt()（入力欄クローズで消える）
  - menu_prompt → renderMenuPrompt()（sub 誘導 → main 前口上の順。メニュークローズで消える）

### 6.3 when 条件（I2: 表現力強化）

```yaml
when:                                        # ① 単純条件（v2 互換）
  hour_from: 18
  hour_to: 5                                 # 跨ぎ可

when:                                        # ② 論理結合
  all_of:
    - hour_from: 22
    - date: "12-24"

when:                                        # ③ OR
  any_of:
    - date: "12-24"
    - date: "12-25"

when:                                        # ④ NOT
  not:
    hour_from: 0
    hour_to: 5

when:                                        # ⑤ 直近 N 回中の出現抑制
  not_in_recent:
    key: "boot_evening"
    count: 3

when:                                        # ⑥ 確率
  probability: 0.05                          # question_curiosity 用、低確率発生
```

評価結果はマッチ可否と「特異度」を返し、特異度の高いものから候補抽選（v2 互換、複合条件は加算）。

### 6.4 sub: null の扱い（H2）

- `sub:` フィールドが**省略**または `null` の場合、その台詞は **main 単独**として処理される
- サブ無しゴースト（shell.json に sub 定義なし）では `sub:` が指定されていても無視
- main も null（あり得る? → 規約として禁止、validator で警告）

### 6.5 system_messages のキー一覧（U3 連携）

| key | トリガー | when パラメータ |
|---|---|---|
| `cost_warning_80` | 月次コスト 80% 到達 | `{ provider: "openai" 等 }` |
| `cost_limit_exceeded` | 上限超過 | 同上 |
| `mode_degraded` | 自動降格 | `{ reason: "api_error" \| "cost_limit" }` |
| `mode_recovered` | 自動復帰 | |
| `update_available` | 新バージョン検出 | `{ version: "x.y.z" }` |
| `voicevox_dl_complete` | 資産DL完了 | |
| `voicevox_dl_failed` | 資産DL失敗 | `{ reason }` |
| `irodori_unavailable` | GPU 不可・サイドカー起動失敗 | `{ reason }` |

各キーは省略可（辞書未定義時はトーストへフォールバック）。
★M7: `reminder_fired` は system_messages から **events へ移動**した（deliver_event +
プレースホルダ経路に一本化。notify() の `ReminderFired` variant は削除）。

---

## 7. TTS パイプライン

### 7.1 全体フロー

```
   synthesize_voice(text, slot, caption?)   ★ caption は Irodori 実モデルのみ使用
            │
            ▼
   ┌────────────────────┐
   │ 現在エンジン選択   │
   │  voicevox_core /   │
   │  irodori           │
   └────────────────────┘
            │
            ├── voicevox_core ──→ そのまま VoicevoxEngine::synthesize
            │
            └── irodori ────────→ 漢字→ひらがな前処理（preprocess）
                                  → IrodoriEngine::synthesize（HTTP）
                                  → WAV
   ↓
   WAV を base64 → フロント → Web Audio
```

### 7.2 trait TtsEngine

```rust
#[async_trait]
pub trait TtsEngine: Send + Sync {
    async fn synthesize(
        &self,
        slot: Slot,
        text: &str,
        speed: f64,
        volume: f64,
    ) -> Result<Vec<u8>, TtsError>;

    fn name(&self) -> &'static str;
    fn needs_kana_preprocess(&self) -> bool;
}
```

### 7.3 VoicevoxEngine（既存知見の流用）

- `voicevox_core.dll` を libloading で実行時ロード（v0.0.3 と同じ方針）
- `acceleration_mode = CPU` 強制（GPU 経路の AV 回避、要件にも合致）
- 合成器を `Mutex<Option<VoicevoxEngine>>` で保持・遅延初期化
- 事前 init（boot 時・設定変更時）で初発話のラグを解消
- `needs_kana_preprocess() = false`（OpenJtalk が内部で読み解析）

### 7.4 IrodoriEngine

```rust
pub struct IrodoriEngine {
    client: reqwest::Client,
    base_url: String,           // http://127.0.0.1:8800
    sidecar_handle: Mutex<Option<SidecarHandle>>,  // プロセス管理
    voice_refs: VoiceRefStore,
}

impl TtsEngine for IrodoriEngine {
    fn needs_kana_preprocess(&self) -> bool { true }

    async fn synthesize(&self, slot, text, speed, volume) -> ... {
        self.ensure_sidecar_running().await?;          // (O2)
        let voice_ref = self.voice_refs.get(slot).await?;   // 参照音声 .wav パス
        let body = OpenAiSpeechRequest {
            model: "irodori-voice-clone",
            input: text,                               // 既に preprocess 済み
            voice: voice_ref.id,
            response_format: "wav",
            speed,
        };
        let wav = self.client.post(format!("{}/v1/audio/speech", base_url))
            .json(&body).send().await?.bytes().await?;
        Ok(wav.to_vec())
    }
}
```

### 7.5 漢字→ひらがな前処理（K1: voicevox_core の Open JTalk 流用）

```rust
// tts/preprocess.rs
pub struct KanaPreprocessor {
    openjtalk: Arc<OpenJtalkRc>,    // voicevox_core C API 由来
}

impl KanaPreprocessor {
    pub fn convert(&self, text: &str) -> Result<String, PreprocessError> {
        // voicevox_open_jtalk_rc_analyze で AccentPhrases JSON を取得
        let json = unsafe { self.openjtalk.analyze(text)? };
        let phrases: Vec<AccentPhrase> = serde_json::from_str(&json)?;
        let mut out = String::new();
        for phrase in phrases {
            for mora in phrase.moras {
                // mora の text フィールドがカタカナで返るのでひらがなに変換
                out.push_str(&katakana_to_hiragana(&mora.text));
            }
            if let Some(pause) = phrase.pause_mora { /* 無音挿入は不要、句読点で代用 */ }
            out.push('、');  // 句切れ
        }
        Ok(out)
    }
}
```

- AccentPhrase JSON 構造は voicevox_core の公開仕様に準拠
- 初期化コスト: OpenJtalkRc を専用に1つ持つ（VoicevoxEngine の synthesizer とは別、軽量）

#### 絵文字アノテーションの保護（Irodori-TTS V3 の感情制御対応）

Irodori-TTS V3 は入力テキスト中の特定絵文字（45 種、upstream `EMOJI_ANNOTATIONS.md`）で
感情・スタイル・効果音を制御する。OpenJtalk 解析は mora を持たない文字（記号・絵文字）を
落とすため、素通しするとアノテーションが消える。対策として前処理を**セグメント分割方式**で行う:

1. `split_emoji_segments(text)` でテキストを「対応絵文字」/「通常テキスト」セグメント列に分割
   - ホワイトリスト（45 絵文字）に対する**最長一致**。`😮‍💨`（ZWJ シーケンス）を接頭辞の `😮` に
     誤マッチさせない。`⏸️` `🌬️` は VS16 付きで 1 絵文字
   - リスト外の絵文字は通常テキスト側に残し、従来通り解析で落とす（Irodori が解釈しない文字を送らない）
2. 通常テキストセグメントだけ従来のかな化を通す
3. 元の順序で再結合（絵文字は無変換で残る）

セグメント単位の解析が 1 つでも失敗したら全体を Err とし、呼び出し側（`preprocess_for_irodori`）の
raw テキストフォールバックに委ねる。

### 7.6 キャッシュ方針（L2: なし）

- 都度合成。実装シンプル、メモリ予測しやすい。
- 必要になれば後から `tts/cache.rs` を追加可能。

---

## 8. Irodori-TTS サイドカー設計

### 8.1 構成

```
%APPDATA%\ugg\irodori\
├── python\              -- M1: 公式 Embeddable Python（Windows x64, 約 10MB）
│   ├── python.exe
│   ├── python313.dll
│   └── ... (標準ライブラリ)
├── packages\            -- pip install で配置（torch, fastapi 等、~2GB）
├── model\               -- Irodori-TTS モデル（HF から DL、数GB）
├── refs\                -- 参照音声 wav 格納
│   ├── main_<id>.wav
│   └── sub_<id>.wav
├── sidecar.py           -- FastAPI エントリポイント
└── version.json         -- インストール済みバージョン情報
```

### 8.2 Python ランタイム（M1）

- 公式 Embeddable Python（Windows x64）を初回 DL 時に取得
- `python._pth` を編集して `Lib\site-packages` を有効化
- `get-pip.py` で pip ブートストラップ → 要件パッケージインストール
  - `torch` (CUDA 12.x)
  - `fastapi` + `uvicorn`
  - Irodori-TTS の依存
  - 漢字→かなは Rust 側で行うので Python 側に同種ライブラリは不要

### 8.3 モデル配布（HF DL）

- HuggingFace `Aratako/Irodori-TTS-*` モデルを初回 DL
- 規約同意は設定 UI でチェック必須（VOICEVOX 同様の同意ゲート）
- DL 進捗は `irodori-download` イベント

### 8.4 プロセス管理（O2）

```rust
pub struct SidecarHandle {
    child: Child,                  // tokio::process::Child
    port: u16,                     // 動的割当（起動時に空きポートを取得）
    last_used: Instant,            // アイドル判定
}

// 起動: 初回 synthesize の手前
async fn ensure_sidecar_running(&self) {
    let mut guard = self.sidecar_handle.lock().await;
    if guard.is_none() {
        // python.exe sidecar.py --port <free_port>
        let child = spawn_python_sidecar(...).await?;
        wait_for_health_check(&child).await?;  // /health を ping
        *guard = Some(SidecarHandle { child, ... });
    }
    if let Some(h) = guard.as_mut() {
        h.last_used = Instant::now();
    }
}

// アイドル監視: バックグラウンドタスクで N 分（例 5 分）アイドル → kill
async fn idle_watcher() {
    loop {
        sleep(Duration::from_secs(60)).await;
        let mut guard = sidecar.lock().await;
        if let Some(h) = guard.as_mut() {
            if h.last_used.elapsed() > Duration::from_secs(300) {
                let _ = h.child.kill().await;
                *guard = None;
            }
        }
    }
}
```

- アイドル判定値 5 分は実装値、設定可能性は将来課題

### 8.5 通信プロトコル（P1: HTTP, OpenAI 互換）

- エンドポイント: `POST /v1/audio/speech`（OpenAI TTS API 互換）
- リクエスト:
  ```json
  {
    "model": "irodori-voice-clone",
    "input": "あいうえお",                   // 既にひらがな前処理済み
    "voice": "main_42",                       // refs/main_42.wav が参照音声
    "response_format": "wav",
    "speed": 1.0
  }
  ```
- レスポンス: `audio/wav` バイナリ
- 参照音声生成は別エンドポイント `POST /v1/voice_ref/generate`:
  ```json
  {
    "caption": "明るく元気な若い女性...",
    "out_path": "refs/main_42.wav"
  }
  ```
- ヘルス: `GET /health` → `{ status: "ok", gpu: "NVIDIA RTX..." }`
- 終了: `POST /shutdown`（クリーン終了）

### 8.6 GPU 検出とフォールバック（Q1+Q3）

```rust
pub async fn irodori_check_gpu() -> GpuInfo {
    // 1) Windows DXGI / nvml で CUDA 対応 GPU 検出
    // 2) なければ GpuInfo { available: false, ... } を返す
    // 3) 設定 UI で「Irodori-TTS は GPU 環境でのみ利用可能」と表示し DL ボタン無効化
}

// サイドカー起動時の保険:
// /health が gpu: null を返したら起動失敗扱い → notify(IrodoriUnavailable)
//   → 自動的に voicevox_core にフォールバック
```

### 8.7 参照音声管理（R1+R3 ハイブリッド）

```
[シェル選択時]
   ├─ shell.json に voice_caption_default があれば → 自動生成
   │   └─ voice_refs テーブルに保存
   └─ なければ → 設定パネルで手動入力を促す

[設定パネル: 音声タブ]
   ├─ メイン/サブ それぞれに参照音声状態を表示
   ├─ 「参照音声を生成 / 再生成」ボタン
   │   └─ クリック → キャプション入力モーダル → /v1/voice_ref/generate
   ├─ 「プレビュー再生」ボタン
   │   └─ クリック → /v1/audio/speech で短文合成
   └─ 「参照音声を削除」ボタン
       └─ voice_refs テーブル削除 + ファイル削除
```

`voice_refs` テーブルは MVP では `UNIQUE(slot)` で各 slot 最新1件のみ（複数履歴は将来課題）。

### 8.8 漢字→ひらがな前処理の呼び出し

- IrodoriEngine::synthesize 内部で TtsState の `openjtalk_for_preprocess` を使い変換
- VoicevoxEngine 内部の Synthesizer とは**別の OpenJtalkRc インスタンス**を持つ（合成中の競合を避ける）

---

## 9. クリック透過（v0.0.3 踏襲）

### 9.1 フロント側合成
- セル 8px、グリッド (cols×rows) を生成
- `.solid` 要素の矩形 + キャラ画像のアルファ（ImageData 由来）で塗りつぶし
- `update_alpha_mask({cols, rows, data})` を 50ms デバウンスで送信
- pose 変更・パネル開閉・吹き出し表示変更・リサイズで再送

### 9.2 Rust 側ポーリング（50ms）
- `window.cursor_position()` でグローバル座標取得
- ウインドウ矩形に変換、対応セルを判定
- 不透明セル → `set_ignore_cursor_events(false)`
- 透明セル / ウインドウ外 → `true`
- 状態変化時のみ呼ぶ
- **左ボタン押下中（GetAsyncKeyState）は透過化への遷移を保留**: キャラドラッグ（spec §4.3.4）中はマスク更新がカーソルに追いつかず、古いマスクの透明セル上で click-through が発動して mousemove/mouseup を取りこぼすため。対話化への遷移は常に即時

### 9.3 レイヤー分離との整合
- `#character-layer` のキャラ画像アルファは scale 後の見かけサイズで合成
- `#ui-layer` の `.solid` 要素は scale なしの矩形のまま合成
- 両者の合成は同一の 8px グリッド上で OR を取る

---

## 10. ウインドウ・レイアウト

### 10.1 レイヤー分離方式（S1）

```html
<div id="stage">
  <div id="character-layer">   <!-- inset:0 の配置基準。scale は各 slot 側 -->
    <div id="char-main" style="left: <x>px; transform: scale(var(--ugg-scale))">...</div>
    <div id="char-sub" style="left: <x>px; transform: scale(var(--ugg-scale))">...</div>
  </div>
  <div id="ui-layer">           <!-- scale なし -->
    <div id="balloon-main"></div>
    <div id="balloon-sub"></div>
    <div id="balloon-extra"></div>   <!-- 3つ目（A-3 パターン3/4） -->
    <div id="chat-input-wrap" class="solid"></div>
    <div id="tts-credit" class="solid"></div>
    <div id="pomodoro-badge" class="solid"></div>
  </div>
</div>
```

### 10.2 ステージ方式とキャラ個別配置（spec §4.1.6 / §4.3.4）

- ウインドウ = **モニタ作業領域の全幅 × 高さ 1024 (logical) の透明ステージ**。作業領域下端に固定（presence/window_pos.rs が起動時ドック + 1 秒監視で再ドック）。高さはスケール上限 2.0 でデフォルトシェルのキャラ (384px→768px) + バルーン/入力欄を収容する値（作業領域が足りなければキャップ）。ユーザーはウインドウを動かせない
- 各 `.char-slot` は `position: absolute; bottom: 0; left: <x>px`（x = stage/charpos.ts が管理、CSS px）。**キャラごとに独立して X 移動**し、Y は bottom:0 固定
- `--ugg-scale` は CSS 変数として `:root` に保持し、**各 `.char-slot` に** `transform: scale(var(--ugg-scale))` を適用。`transform-origin: bottom left` のため `left` = 視覚ボックス左端のまま拡縮できる
- 既定配置（char_pos 未保存時）: main はステージ右端、sub は main の左 40px（spec §4.1.1）
- スケール変更・ステージリサイズ時は charpos.ts が全キャラをステージ内に再 clamp する

### 10.3 吹き出し配置計算（キャラ左横・伺か風）

```typescript
function reposition(slot: "main" | "sub") {
    const rect = char.getBoundingClientRect();        // scale 後の矩形
    // 横: キャラ左端から 24px (しっぽ含む) 空けて右端を合わせる
    let left = rect.left - 24 - balloon.offsetWidth;
    // 左端 8px に収まらない場合はキャラの右横へ反転 (.flip、しっぽも反転)
    if (left < 8) left = rect.right + 24;
    // 縦: キャラ上端 + キャラ高さ × 0.12 (顔の高さ) に上端を置く
    let top = rect.top + rect.height * 0.12;
    // 相方の吹き出しと重なる場合: main は相方の上へ、sub は相方の下へ退避
    // 最後に上下端 8px で clamp
}
```

- しっぽは吹き出しの側辺（上端から 20px）からキャラ側を向く（通常 = 右辺から右向き、.flip 時 = 左辺から左向き）
- タイプライター進行・キャラのドラッグ移動ごとに再計算（吹き出しの成長とキャラ追従）
- フォント/border はスケールの影響を受けないため視認性確保

### 10.4 3つ目の吹き出し（A-3 パターン3/4）

- パターン3: main → sub → main の **3 ターン目**を `#balloon-extra` に独立表示
- パターン4: sub → main → sub の **3 ターン目**を `#balloon-extra` に独立表示
- 配置: main/sub の吹き出しと重ならないよう、上方に積み上げ or 横に並べる（実装で詳細決定）
- 全ターン描画+発話完了後に一括消去

---

## 11. ゴースト発話原則の実装（U3）

### 11.1 notify() サービス

```rust
// system/notify.rs

pub enum NoticeKind {
    CostWarning80 { provider: String, percent: u8 },
    CostLimitExceeded { provider: String },
    ModeDegraded { reason: DegradeReason },
    ModeRecovered,
    UpdateAvailable { version: String },
    VoicevoxDlComplete,
    VoicevoxDlFailed { reason: String },
    IrodoriUnavailable { reason: String },
    ReminderFired { text: String },
}

pub struct NoticeOptions {
    pub severity: Severity,    // Minor | Important | Critical
}

pub enum Severity {
    Minor,        // ゴースト発話のみ（or トースト fallback）
    Important,    // ゴースト発話 + トースト二段表示
    Critical,     // トーストのみ（ゴースト未ロード等の安全マージン）
}

pub async fn notify(
    app: &AppHandle,
    state: &Arc<AppState>,
    kind: NoticeKind,
    opt: NoticeOptions,
) {
    let key = kind.dict_key();    // e.g. "cost_warning_80"
    let args = kind.into_args();   // when 評価用のメタデータ

    let dict = state.ghost.lock().await;
    let resp = dict.dictionary.system_message(&key, &args);  // when 条件評価込み

    match (opt.severity, resp) {
        (Severity::Critical, _) => {
            emit_toast(app, kind.fallback_text()).await;
        }
        (_, Some(resp)) => {
            dialogue::persist_and_speak(app, state, &resp).await;
            if opt.severity == Severity::Important {
                emit_toast(app, kind.fallback_text()).await;  // 二段表示
            }
        }
        (_, None) => {
            // 辞書未定義 → トーストへフォールバック
            emit_toast(app, kind.fallback_text()).await;
        }
    }
}
```

### 11.2 NoticeKind 一覧（§6.5 と対応）

| kind | severity 既定 |
|---|---|
| CostWarning80 | Minor |
| CostLimitExceeded | Important |
| ModeDegraded | Important |
| ModeRecovered | Minor |
| UpdateAvailable | Minor |
| VoicevoxDlComplete | Minor |
| VoicevoxDlFailed | Important |
| IrodoriUnavailable | Important |

※ 実装の notify() は severity 二段トーストを持たない（発話 or トースト fallback の二択）。
★M7: `ReminderFired` variant は削除（§11.4 の deliver_event 経路へ一本化）。

### 11.3 呼び出し点

- `system/cost.rs`: ポストLLM呼び出しでコスト計算 → 80% 検出で `notify(CostWarning80, ...)`
- `dialogue/mod.rs`: モード自動降格時に `notify(ModeDegraded, ...)`
- `system/update.rs`: 新バージョン検出時に `notify(UpdateAvailable, ...)`
- `tts/download.rs`: 資産DL完了/失敗時に `notify(VoicevoxDlComplete, ...)`
- `tts/irodori.rs`: GPU不可検出/サイドカー起動失敗時に `notify(IrodoriUnavailable, ...)`

### 11.4 通知配達サービスと発話ガバナンス（★M7、daily-support-design §3/§4 が正）

M7 で**すべての自発発話**（独り言・idle・リマインダー通知、以降の Tier S 発話）は
`system/deliver.rs::deliver_event` に一本化した。notify()（§11.1）は従来どおり
system_messages 用に残る（コスト・降格・DL 系のシステム告知）。

```rust
// system/deliver.rs
pub enum DeliveryOutcome { Ghost, Toast, Deferred, Failed }  // reached() = Ghost|Toast

pub async fn deliver_event(
    app, state,
    category: SpeechCategory,     // Monologue / Idle / Reminder / (M8-M10: Todo / Calendar / Situation*)
    priority: Priority,           // Notice = 必ず届く（全段免除） / Ambient = 気配り系（全段適用）
    key: &str,                    // 辞書 events キー（Monologue のみ monologue セクション）
    placeholders: &[(&str, &str)],
    fallback: Option<String>,     // 辞書未ヒット/発話失敗時の system-toast 文
) -> DeliveryOutcome;
```

- **直列化**: `dialogue.busy` の try_acquire を permit として保持し check→配達→record を
  1 クリティカルセクションで行う（check-then-act 競合と二重発話の防止）。取れなければ Deferred。
- **ゲートは deliver 内で 1 回だけ**: `governance::can_deliver`（純粋判定）→ 配達成功後に
  `governance::record_delivered`。呼び出し側 watcher はどちらも直接呼ばない（二重ゲート禁止）。
- **判定表**（Ambient のみ適用、Notice は全段免除）: 段1 ハード静音（should_stay_quiet）→
  段2 夜間静音（night_quiet_enabled + from/to、日跨ぎ可・from==to は終日）→
  段3 カテゴリ設定 OFF → 段4 最低間隔 min_speak_interval_min（**Situation\* のみ**。
  monologue/idle は既存の間隔設定を維持）。段5 連投回避は M9 で導入（パラメータ未決）。
- **機能スイッチは発火元**: `reminder_notify_enabled`（+ マスタ `daily_support_enabled`）は
  リマインダー watcher が確認する。OFF 中は発火を保留し、ON に戻すと届く（期限は消えない）。
- **リマインダー watcher**（tasks.rs、10 秒間隔）: 到達時のみ log_fire し、once は
  deactivate（発火済み・未完了）、繰り返しは次回 due へ reschedule。未達は active 維持で再試行。
  起動 20 秒後に一度だけ回収パスを実行し、停止中に過ぎた期限が複数あれば
  「『直近の本文』（ほか N-1 件）」の 1 発話に集約する。
- **daily watcher**（★M8、tasks.rs、60 秒間隔・起動 30 秒後開始）: ①起動時 + ローカル
  日付の変更検知で日課を復活（`tools::todo::reset_recurring`、冪等）し `todos-changed` を
  emit。②朝の時間帯（5:00-11:00）に 1 日 1 回、today の未完了が 1 件以上なら
  `todo_morning`（{count}、Ambient）を配達。告知済み日付は app_settings
  `todo_morning_date` に保持し再起動でも二重告知しない。0 件の朝は告知なしで消化。
  Deferred（静音・busy）は後続 tick で再挑戦、Failed（辞書なし）は当日分を消化。
  リマインダー watcher とは分離（`reminder_notify_enabled` と結合させない）。
  設計書 §7.2 の「既存 watcher にピギーバック」は、リマインダー watcher が通知トグルで
  ループを止める構造のため専用 watcher に変更した（実装判断）。
- **context watcher**（★M9、tasks.rs、60 秒間隔・起動 45 秒後開始）: `presence/context.rs`
  の OS 検知（GetLastInputInfo / GetSystemPowerStatus）で連続利用セッションを計測
  （アイドル 5 分で境界）し、状況発話 4 カテゴリを Ambient で配達する。閾値は
  context.rs の定数が正（daily-support-design §11.1-2）: 休憩促し 90 分ごと /
  深夜 23-5 時・30 分利用・1 晩 1 回（`situation_late_night_date`、0-5 時は前日夜キー）/
  バッテリー 15% 以下・非 AC・1 回（AC or 20% 超回復で解除）/ ToDo フォロー 14-18 時・
  1 日 1 回（`todo_follow_date`）/ ToDo 滞留 18-22 時・1 日 1 回・作成 3 日以上
  （`todo_stale_date`）。カテゴリトグルは発火元で機能スイッチとして確認（gate 段 3 と同値）。
  Ghost/Failed は消化、Deferred は次 tick 再挑戦（M8 と同ポリシー）。
- **gate 段 5（連投回避）**（★M9）: Situation* 同カテゴリの最短間隔 = **120 分 × (1 + backoff)**。
  🔕 フィードバック（`feedback_speech`、§4.11）が backoff を線形に増やし、3 回で
  カテゴリトグル自体を OFF。backoff は `governance_backoff:<category>` に永続化し
  起動時 `governance::load_backoff` で復元する。
- **終了前確認**（★M9、spec §4.6.2 後半・2026-07-17 裁定）: トレイ「終了」時に未完了の
  today ToDo があれば `events.todo_quit`（{count}）を `quit` の代わりに再生（ユーザー起点
  につきゲート非対象）。コンテキストメニューの「終了」は M3 判断（即 exit）のまま。
- **カレンダー watcher**（★M10、tasks.rs、60 秒間隔・起動 25 秒後開始）: `system/calendar.rs`
  で全 ICS ソースを 30 分ごとに取得し `calendar_cache` へ near-term 展開して UPSERT。
  開始前通知は `calendar_notify_min` 分前（終日は当日ローカル 8:00）に達した未通知予定を
  `calendar_upcoming`（Notice）で 1 回。到達で `notified=1`。取得失敗はソース単位でログして
  他を続行し既存キャッシュを維持（オフライン動作）。ソース未設定なら何もしない（既定オフ）。
  **TZ は日本前提の簡易解決**（Z=UTC / 浮動・TZID=ローカル / VALUE=DATE=ローカル 0:00）、
  **RRULE は DAILY/WEEKLY を BYDAY/INTERVAL/UNTIL/COUNT/EXDATE 込みで展開**、
  MONTHLY/YEARLY は同日ステップの best-effort、解釈不能な RRULE は当日分のみ
  `unsupported` 印で残す（daily-support-design §11.1）。
- **ユーザー起点の確認発話**（スヌーズ確認・終了前確認）は `speak_event_now`
  （ゲート・record・🔕 メタなし。発話した `DialogueResponse` を返す）。

## 12. ゴースト/シェル DnD 展開（V1+W1）

### 12.1 受け入れ形式

- **zip ファイル**（拡張子 .zip）
- **フォルダ**（中に ghost.json or shell.json）

### 12.2 検出ロジック

```rust
fn detect_asset_kind(path: &Path) -> Result<AssetKind, DndError> {
    let entries = if path.is_dir() {
        list_dir_top(path)
    } else if has_ext(path, "zip") {
        list_zip_top(path)
    } else {
        return Err(DndError::UnsupportedFormat);
    };

    if entries.iter().any(|e| e.ends_with("ghost.json")) {
        Ok(AssetKind::Ghost)
    } else if entries.iter().any(|e| e.ends_with("shell.json")) {
        Ok(AssetKind::Shell)
    } else {
        Err(DndError::NoManifest)
    }
}
```

### 12.3 セキュリティ対策

- **zip slip 対策**: 展開先パスが目的ディレクトリ配下にあることを正規化後に検証（`Path::canonicalize` → starts_with）
- **ファイル名検証**: 制御文字・予約名（CON, PRN 等）を除外
- **サイズ上限**: 展開後合計 1 GB 上限（設定で調整可、超過時エラー）
- **ファイル種別**: shell 用は画像 (.png, .jpg) + .json のみ、ghost 用は .yaml + .json のみを許容、その他は警告（許容するか拒否するかは options）

### 12.4 上書き処理

```rust
async fn install_asset(
    state: &Arc<AppState>,
    path: &Path,
    kind: AssetKind,
) -> Result<InstallResult, DndError> {
    let manifest = peek_manifest(path, kind)?;
    let id = manifest.id;
    let target = match kind {
        AssetKind::Ghost => ghosts_dir().join(&id),
        AssetKind::Shell => shells_dir().join(&id),
    };

    if target.exists() {
        return Ok(InstallResult::ConflictDetected { id, kind });
    }
    extract_into(path, &target)?;
    Ok(InstallResult::Installed { id, kind })
}
```

- **上書き確認**: フロント側で確認ダイアログ → 確定で `dnd_install(..., overwrite: true)` を再呼び出し
- インストール後は **再起動を促す**（reload_assets は提供せず、再起動の動線を notify でゴーストが案内）

### 12.5 UI

- WebView の `dragover` / `drop` を捕捉、`Tauri` の `getDataTransferFiles` でパスを取得
- 設定パネル → 拡張タブにも「ファイル選択」UI を用意（DnD と同等の処理）

---

## 13. 配布アーキテクチャ

### 13.1 インストーラ構成

- **NSIS**, `currentUser` モード, 日本語ロケール
- **同梱**: アプリ本体 + ghosts/default + shells/default
- **同梱しない**:
  - voicevox_core 資産（初回 DL）
  - Irodori-TTS（初回 DL、規約同意必須・GPU 必須）

### 13.2 初回 DL フロー

```
[初回起動]
  ↓
[オンボーディング]
  ├─ nickname / 興味 / 話し方 入力
  └─ 完了
  ↓
[TTS 設定（任意）]
  ├─ voicevox_core 資産 DL（規約同意 → ダウンローダ起動）
  └─ Irodori-TTS（任意・GPU 検出済の場合のみ）
       ├─ 規約同意
       ├─ Embeddable Python DL
       ├─ pip パッケージ DL（torch 等）
       └─ Irodori モデル DL（HF）
  ↓
[通常運用]
```

### 13.3 keyring 利用

- `service = "ugg"`, `user = provider 名 or "github_token"`
- 各種 API キー（openai 等）、voicevox 資産 DL 用 GitHub PAT
- 暗号化は OS 標準（Windows Credential Manager）

---

## 14. ライフサイクル

### 14.1 boot

```
[main]
  ├─ tauri_plugin_log 初期化
  ├─ tauri_plugin_autostart 初期化
  ├─ tauri_plugin_single_instance（既起動チェック）
  ├─ Db::open_default()
  ├─ Settings 読み込み（app_settings の "settings" キー）
  ├─ Ghost/Shell/Dictionary 初期ロード
  ├─ tokio Runtime 構築
  ├─ AppState::new() で全サブ状態初期化
  └─ tauri::Builder
       ├─ .manage(Arc::new(state))
       ├─ .invoke_handler(...)
       └─ .setup(|app| {
            ├─ create_main_window(app, settings.display_scale)
            ├─ window::mask::spawn_cursor_polling(app, state.clone())
            ├─ presence::idle::spawn_watcher(app, state.clone())
            ├─ presence::window_pos::spawn_dock_keeper(state.clone())
            ├─ workers::spawn_random_talk(app, state.clone(), interval_rx)
            ├─ workers::spawn_topics_fetcher(app, state.clone(), topics_rx)
            ├─ system::update::spawn_check(app, state.clone())
            └─ tts::voicevox::spawn_preinit(state.clone())
          })
```

### 14.2 通常運用

- フロント `boot()` で `get_boot_payload` → 画像プリロード → イベント listen → `frontend_ready`
- `frontend_ready` で起動挨拶（first_boot or boot、`greeted` で再ロード時の二重発話防止）
- ユーザー操作 / 自発タスクの応答ループ
- 設定変更時は `apply_settings` で関連サブシステムへ通知

### 14.3 終了

```
[トレイ「終了」 or プロセス終了シグナル]
  ├─ events.quit 発話キュー投入 → 発話完了待ち（最長 5s）
  ├─ presence::window_pos::persist 即時保存
  ├─ tts::irodori::shutdown_sidecar
  └─ Db ドロップ
```

---

## 15. 既知のリスクと対策

| リスク | 影響 | 対策 |
|---|---|---|
| voicevox_core C API のバージョン差 | 起動時クラッシュ | バージョン固定（FFI と一致する版を初回 DL） |
| Irodori-TTS モデル DL の中断 | サイドカー起動失敗 | チェックサム検証、再 DL の動線 |
| GPU が利用可能 → 利用不能（運転中変化） | サイドカー異常終了 | notify(IrodoriUnavailable) + voicevox_core に自動切り替え |
| 辞書 v3 のパース失敗 | アプリ起動失敗 | バリデータで起動時に警告、デフォルト辞書にフォールバック |
| user_profile の肥大化 | system prompt 肥大化 | モード別容量管理（要約サイクル or 件数上限） |
| zip slip 等の DnD 経由のパス脱出 | 任意ファイル書き込み | `canonicalize` 後の starts_with 検証 |
| Python サイドカー起動時の文字エンコーディング | 出力文字化け | UTF-8 強制（PYTHONIOENCODING） |
| サイドカーの孤児プロセス化 | リソースリーク | アプリ終了時に必ず kill、Job Object（Windows）で親子連動 |

---

## 16. 改訂履歴

| 日付 | 版 | 内容 |
|---|---|---|
| 2026-06-18 | v1 | Phase 2 対話で確定した全設計を反映、初版 |
| 2026-07-17 | v1.1 | M7（日常支援 Tier S: 共通基盤 + 統合リマインダー、daily-support-design v2 準拠）を反映。DB v6（reminders 拡張 + reminder_log、§2）/ `system/deliver.rs`・`system/governance.rs` 新設（§11.4）/ `commands/daily.rs`（§4.11）/ イベント `reminders-changed`・`system-toast` 受け皿（§5）/ 辞書 events `reminder_fired`・`reminder_snoozed` + プレースホルダ規約（§6.2、`reminder_fired` は system_messages から events へ移動 §6.5）/ Settings に daily_support・夜間静音・ガバナンス系 10 フィールド追加 / AppState に GovernanceState（§3）。付随して実装との既知乖離を一部解消（monologue_cache → topics_cache、migration v4-v6 追記、notify() の severity 未実装注記、WorkerHandles 不採用注記） |
| 2026-07-17 | v1.2 | M8（ToDo・日課管理 §4.6.2）を反映。DB v7 `todos`（§2）/ `tools/todo.rs`（検証 + 日課復活の境界計算）/ ToDo コマンド 6 種 + `todos-changed`（§4.11・§5）/ daily watcher（日課復活 + 朝の件数告知、§11.4）/ 辞書 events `todo_morning`・`todo_done`（発火）+ `todo_follow`・`todo_stale`（キーのみ、発火は M9）（§6.2）/ パネルに ToDo 節（3 バケットタブ） |
| 2026-07-18 | v1.4 | M10（カレンダー参照 §4.6.4、読み取り専用）を反映。DB v8 `calendar_cache`（複合キー + 実装追加列 unsupported、§2）/ `system/calendar.rs`（ICS 自前パース + RRULE near-term 展開 + TZ 簡易解決）/ カレンダーコマンド 4 種 + `calendar-changed`（§4.11・§5）/ calendar watcher（取得 + 開始前通知、§11.4）/ 辞書 `calendar_upcoming`（§6.2）/ Settings に calendar_sources（File\|Url）+ calendar_notify_min / フロント設定カレンダー UI + パネル今日明日表示。ファイル選択ダイアログは見送り（パス手入力、依存を増やさない）。**Tier S 4 機能そろい → v0.2 リリース候補**。 |
| 2026-07-17 | v1.3 | M9（状況発話 + 検知 + ガバナンス完成 §4.6.3）を反映。`presence/context.rs`（OS 検知 + 閾値純関数、windows crate に Power/SystemInformation feature 追加）/ context watcher（休憩・深夜・バッテリー・ToDo フォロー/滞留、§11.4）/ gate 段 5 連投回避 + 🔕 backoff（`feedback_speech` §4.11、`governance_backoff:*` 永続化）/ `DialogueResponse` に speech_id・category・priority・feedback_allowed（§5、バック起点のみ）/ フロント #balloon-mute + 設定「状況に応じた声かけ」セクション / 辞書 `situation_break`・`situation_late_night`・`situation_battery`・`todo_quit`（§6.2。`situation_todo_follow` キーは `todo_follow`/`todo_stale` に統合）/ 終了前確認（tray quit で todo_quit 優先、spec §4.6.2 後半）/ AppState に ContextState（§3） |
