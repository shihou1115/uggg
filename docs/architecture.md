# ugg アーキテクチャ設計書（architecture.md v2.40）

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
│  │    stage/ (character/charpos/alphamask/scale)   │   │
│  │    dialogue/ (balloon/input/typewriter)         │   │
│  │    tts/ (speaker/mouth/credit)                  │   │
│  │    panels/ (settings/chatlog/daily/onboarding)  │   │
│  │    menu/ (context-menu)                         │   │
│  │    interaction/ (click/poke/nade)               │   │
│  │    system/ (toast/ghost-speech)                 │   │
│  └────────────────── ↕ Tauri IPC ──────────────────┘   │
│  ┌──── Backend (Rust) ─────────────────────────────┐   │
│  │   src-tauri/src/                                │   │
│  │    main.rs (コマンド/イベント配線のみ・薄い)   │   │
│  │    state.rs (AppState コンテナ)                 │   │
│  │    db.rs                                        │   │
│  │    commands/ (1コマンド1ファイル目安)          │   │
│  │    dialogue/ (low/advanced/llm/banter)          │   │
│  │    ghost/ (manifest/dict/dnd)                   │   │
│  │    tts/ (voicevox/irodori/sidecar/preprocess)   │   │
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
├── tasks.rs                 -- バックグラウンド watcher 群（ランダムトーク / 放置 / リマインダー / daily / context / calendar / topics / update / Irodori 監視）。§11.4
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
│   └── banter.rs            -- 掛け合いパターン制御 (1-4 + 問いかけ 5)
│                             -- ★v0.5.1: 問いかけパターンは spec §4.2.4 のとおり
│                                **advanced の掛け合いパターン 5**として実装した。
│                                辞書の events キーではない（辞書系は常にパターン1）。
│                                発生確率 5%、構造はパターン1 と同じで内容が
│                                ユーザーへの問いかけで終わる。
│                               ※ 独り言は M7 で専用ヘルパを廃し、deliver_event が dict.pick_monologue を直接引く
│                               ※ ★M14 advanced では deliver_event が先に monologue_cache を pop し、
│                                  空・失効・low なら従来どおり dict.pick_monologue へ落ちる
│
├── ghost/                   -- ゴースト/シェル/辞書ロード
│   ├── mod.rs
│   ├── manifest.rs          -- ghost.json / shell.json パース（★v0.5 `characters.*.persona` と
│   │                          `prompt.{max_chars_per_line,style_notes}` を追加。省略可、既定文へフォールバック。
│   │                          出荷資産のキーと構造体の突合は同ファイルの契約テストが担保）
│   ├── dict.rs              -- 辞書スキーマ v3 パース、when 条件評価
│   └── dnd.rs               -- ★ DnD 展開（zip/フォルダ、zip slip 対策・サイズ/深さ上限）
│
├── tts/                     -- TTS
│   ├── mod.rs               -- サブモジュールの宣言のみ（エンジンの振り分けは commands/tts.rs の synthesize_voice）
│   ├── voicevox.rs          -- voicevox_core 埋め込み（libloading + プリビルド C API）
│   ├── irodori.rs           -- Irodori サイドカー HTTP クライアント
│   ├── irodori_download.rs  -- Irodori 資産 DL（Python ランタイム・依存・HF モデル）
│   ├── sidecar.rs           -- サイドカープロセスの起動・停止・監視
│   ├── gpu.rs               -- GPU 検出（Irodori 可否判定）
│   ├── preprocess.rs        -- 漢字→ひらがな変換（voicevox_core の OpenJtalk を流用）
│   ├── reader.rs            -- テキスト読み上げ: .txt 読込 + チャンク分割 + .md 台本対応（text-reader-spec.md / script-reader-spec.md）。★v0.5.6: 子プロセスの出力 **1 行**の読み方（`decode_output_line`。UTF-8 → Shift_JIS）もここに置く（行に組み立てるのは呼ぶ側 = `sidecar.rs` の stderr ポンプと `child_process.rs`）
│   ├── child_process.rs     -- ★v0.5.6 子プロセスの起動と出力の読み取り（行ごとに流す・`\n` と `\r`・色付けの制御文字を落とす・無進捗なら Job Object ごと止める・同期待ちでワーカーを塞がない）。`irodori_download` の `run_python` と `download.rs` のダウンローダが使う。★v0.5.6 項目 4: ugg の寿命に結びつける Job（`tie_to_ugg`。サイドカーと zip の展開を入れる）と、プロセスの開始時刻の問い合わせ（台帳の持ち主の照合）
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
│   ├── mod.rs               -- configure_main_window / start_cursor_watcher
│   ├── mask.rs              -- クリック透過ポーリング（50ms, set_ignore_cursor_events）
│   └── tray.rs              -- タスクトレイ・メニュー
│
├── system/                  -- 共通基盤
│   ├── mod.rs
│   ├── log.rs               -- ★v0.5 ファイルログ（`%APPDATA%\ugg\ugg.log`、2MB で 1 世代退避。spec §5）
│   ├── secrets.rs           -- keyring ラッパ
│   ├── cost.rs              -- LLM コスト追跡・上限警告・自動降格
│   ├── update.rs            -- 更新通知
│   ├── topics.rs            -- 時事ネタ RSS 取得
│   ├── manual.rs            -- 取扱説明書（同梱 manual.md）を初回起動時・メニューから開く
│   ├── notify.rs            -- ★ 統合通知サービス notify()（横断方針 §3.1 ゴースト発話原則）
│   ├── deliver.rs           -- ★M7 通知配達サービス deliver_event（自発発話の単一経路、§11.4）
│   ├── governance.rs        -- ★M7 発話ガバナンス can_deliver / record_delivered（§11.4）
│   ├── calendar.rs          -- ★M10 ICS 取得・自前パース・RRULE near-term 展開（読み取り専用、§4.6.4）
│   ├── weather.rs           -- ★M11 Open-Meteo forecast 取得・app_settings JSON キャッシュ・WMO ラベル・降雨判定（§4.7.2）
│   ├── regular_talk.rs      -- ★M12 定例会話: 材料集約 + 定型文組み立て（low）+ advanced 言い回し整形（§4.7.1）
│   └── monologue.rs         -- ★M14 advanced 独り言のキャッシュ補充: LLM 生成 + 時事ネタ織り込み + 応答パース（§4.4.4/§4.4.6）。消費は deliver.rs 側
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
├── confirm.ts               -- 確認ダイアログ（window.confirm の代替。静的配置の独自モーダル）
├── dnd.ts                   -- ゴースト/シェルの DnD インストール（dnd_install を呼ぶ）
│
├── stage/                   -- ステージ・ウインドウ
│   ├── character.ts         -- キャラ DOM 管理・pose 画像の保持と切替
│   ├── charpos.ts           -- キャラごとの X 位置管理（spec §4.1.6 / §4.3.4）
│   ├── alphamask.ts         -- 8px グリッド合成 → update_alpha_mask
│   └── scale.ts             -- 表示スケール（レイヤー分離方式 §10）
│
├── dialogue/                -- 対話 UI
│   ├── balloon.ts           -- 吹き出し（最大3つ、§10）
│   ├── input.ts             -- チャット入力
│   └── typewriter.ts        -- タイプライター描画（速度可変）
│
├── tts/                     -- TTS フロント
│   ├── speaker.ts           -- TtsSpeaker / NoopSpeaker / EngineSpeaker（全 slot 直列の発声キュー + 先読み 1）
│   ├── mouth.ts             -- 口パク（振幅駆動のみ、§A-4）
│   ├── types.ts             -- 音声選択肢の型（VoiceOption）
│   └── credit.ts            -- VOICEVOX クレジット表示
│
├── weather/
│   └── credit.ts            -- Open-Meteo 天気クレジット表示（CC BY 4.0、spec §4.7.2）
│
├── panels/                  -- UI パネル
│   ├── settings.ts          -- 設定パネル（1 ファイル）
│   ├── chatlog.ts           -- ログパネル
│   ├── reader.ts            -- テキスト読み上げパネル
│   ├── pomodoro.ts          -- ポモドーロパネル
│   ├── daily.ts             -- ★M7/M8 予定・ToDo パネル（リマインダー節 + ToDo 節: 3 バケットタブ・チェック完了・優先度/日課トグル）
│   └── onboarding.ts        -- 初回オンボーディング
│
├── menu/
│   └── context-menu.ts      -- ★ 右クリック→バルーン内メニュー（C-5、spec §4.3.5）。M7 で「予定・ToDo」項目追加
│
├── interaction/             -- 操作
│   ├── click.ts             -- クリック種別判別（ドラッグ判定を含む）
│   ├── poke.ts              -- つつき
│   └── nade.ts              -- 撫で
│
├── __tests__/               -- 操作列テスト（Vitest + happy-dom、test-plan §3.2b）
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
| ★ 設定パネル UI を分割（general/llm/voice/interests/about）※計画のみ。実装は `panels/settings.ts` の 1 ファイル | v0.0.3 の settings.ts は 1500 行超 |
| ★ system/notify.rs 新設 | 横断方針「ゴーストに喋らせる」を 1 箇所集約 |
| ★ ghost/dnd.rs 新設 | DnD 展開（新機能） |
| ★ tts/voice_ref.rs 新設 | Irodori 参照音声管理（新機能） |
| ★ tts/preprocess.rs 新設 | 漢字→ひらがな変換 |
| ★ tts_engine.rs 廃止 → tts/mod.rs に統合 | 3エンジン抽象（v0.0.3）から 2 エンジン trait へ ※trait は実装していない（振り分けは `commands/tts.rs` の `synthesize_voice` の `match`） |
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
| `topics_cache` | 時事ネタ見出しキャッシュ | 〜数百 | advanced 独り言に織り込む材料（M5-C で蓄積、★M14 で消費開始） |
| `monologue_cache` | ★M14 advanced 独り言のストック | 〜20（STOCK_MAX） | LLM 生成済みの独り言を貯めて発話ごとの API 呼び出しを避ける。再起動をまたぐ |
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
- ★M11 天気: `weather_cache`（WeatherCache を JSON。専用テーブルを持たず app_settings に保存。「解除」= 空文字で消去、§4.7.2）, `weather_rain_date`（降雨の一言の 1 日 1 回 dedup。`*_date` 系と同型）
- ★M12 定例会話: `regular_morning_date` / `regular_evening_date`（朝・夜の定例会話の 1 日 1 回 dedup、`*_date` 系と同型）
- ★v0.5 コスト告知: `cost_warned_80_month` / `cost_limit_notified_month`（値は当月タグ `YYYY-MM`。非永続の AtomicBool では再起動で消え、**月が替わっても戻らない**ため翌月の警告が鳴らなかった。spec §4.2.7「次月リセットで復帰。」）
- ★M13 表示モニタ: `monitor_pref`（ユーザーが選んだモニタ = `{name, x, y}` の JSON。**`window_pos`（前回位置）とは別物**で、選択があるときは `window_pos` を参照しない。空文字 = 選択なし）

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
- **1 応答あたりの行数**: 通常は user → main → sub の 3 行。**掛け合いパターン3/4（§10.4）では3ターン目を話者と同じロール（パターン3=main / パターン4=sub）で追記するため最大 4 行**になり、同一 ts・同一 role の行が 2 行並ぶ（スキーマは不変。role に新値は増やさない）

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
    file_path TEXT NOT NULL,     -- %APPDATA%\ugg\irodori\refs\<slot>_<id>.wav
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

#### `monologue_cache`（★M14 v9 新設、foundation-design §3.2）

```sql
CREATE TABLE monologue_cache (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    ghost_id         TEXT    NOT NULL,   -- 生成に使ったゴースト。切替後の持ち越しを断つ鍵
    text             TEXT    NOT NULL,
    pose             TEXT,               -- 検証は消費時（シェルは push→pop の間に変わりうる）
    topic_fetched_ts INTEGER,            -- 織り込んだ見出しの**取得時刻**。時事ネタ無しは NULL
    created_ts       INTEGER NOT NULL
);
CREATE INDEX idx_monologue_cache_ghost ON monologue_cache(ghost_id);
```

- **`topic_fetched_ts` が二段失効の要**。生成時刻（`created_ts`）で判定すると、6 日前の見出しを
  今日生成した文が「新しい」と誤判定される。複数の見出しを渡したバッチには**最も古い見出しの
  取得時刻**を記録する（最も古いネタに引きずられるため、安全側）
- 失効は 2 本立て: 時事ネタ入りは `TOPIC_MAX_AGE_SECS`（7 日、spec §4.4.6）、
  時事ネタ無しは `MONOLOGUE_MAX_AGE_SECS`（30 日）。定数は `db.rs` に 1 箇所だけ置き、
  **織り込み時（`system/monologue.rs`）と発話時（`pop_monologue_cache`）が同じ値を参照する**
  （片方だけずれると「古いネタを喋る／使える材料を捨てる」に化ける）
- `pop_monologue_cache(ghost_id, now)` は**選別 → 取得 → 削除を 1 トランザクション**で行い、
  ghost_id 不一致・失効行はその場で捨てて次を見る。空なら `None` を返し、呼び出し側は辞書へ落ちる
- `count_monologue_cache(ghost_id, now)` は **pop と同じ条件**で数える（失効行を頭数に入れると、
  pop が全部捨てて辞書に落ちているのに「在庫あり」と見えて補充が止まる）。
  設計書 §3.2 の `count(ghost_id)` に `now` を足したのはこの理由
- sub は持たない（独り言は 1 キャラの発話。`DialogueLine{ main, sub: None }` に詰める）
- 消去の契機: 履歴クリア（全件・`include_profile` 非依存、spec §4.5.5）／
  時事ネタ同意の撤回（`topic_fetched_ts IS NOT NULL` の行だけ、foundation-design §3.6）

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
  - v9: `monologue_cache` を追加 (★M14)。`ghost_id` でゴースト切替後の持ち越しを断ち、`topic_fetched_ts`（**見出しの取得時刻**であって生成時刻ではない）で発話時の賞味期限を判定する
- 参照音声 .wav の配置は `%APPDATA%\ugg\irodori\refs\<slot>_<id>.wav` (architecture §2.4)。`voice_refs.file_path` には絶対パスを保存

### 2.4 ファイル資産（DB 外）

| 場所 | 用途 |
|---|---|
| `%APPDATA%\ugg\companion.db` | SQLite 本体 |
| `%APPDATA%\ugg\voicevox\` | voicevox_core 資産（c_api / onnxruntime / dict / models） |
| `%APPDATA%\ugg\irodori\` | Irodori-TTS 資産（python / model / refs） |
| `%APPDATA%\ugg\irodori\installed.json` | **★v0.5.4 導入記録**。`pins`（このビルドが**要求した**固定 URL）と `resolved`（**実際に入った**版）を持つ。2 つ持つのは役割が違うため — `pins` は入れ直しの要否判定に使い、`resolved` は「指定どおりに入るとは限らない」事実（`huggingface_hub==0.27.0` 指定に対し実機 0.36.2）を残す。**導入が全段成功した後にだけ書く**。**★v0.5.5**: `requirements`（配布名 → 要件文字列）と `models`（名前 → `repo@revision`）を追加。初回導入は全部を記録し、更新は入れ直せた分だけ反映する。**欄が空の記録（v0.5.4 が書いたもの・記録そのものが無い環境）は v0.5.4 の固定の基準値が入っているとみなす**（いまのビルドの値で埋めると、要件を変えたビルドで差が出ず更新が届かない）。**★v0.5.6: `models` が読み先の正本**（サイドカーの起動は記録から読み先を決める。取得はいまのビルドの値。更新が成功したときだけ記録が追いつく。決定表は名前ごと: 記録 → 基準値 → いまのビルド）。**書き込みは tmp → rename**（書きかけを読むと「記録なし」に畳まれ、読み先が基準値へ倒れる） |
| `%APPDATA%\ugg\irodori\update-versions.json` | **★v0.5.6 更新の前の版の控え**（`{配布名: 版}`、`importlib.metadata` の全走査。名前は PEP 503 で正規化）。パッケージを入れ替える更新の前に書き、成功したら消す。**失敗して戻せなかった分があれば残し、次の更新の冒頭でもう一度戻す**。`.update-backup` の**外**に置く（中に置くと、冒頭の「退避が残っていたら戻す」がファイルを退避として扱い、以後ずっと更新できない） |
| `%APPDATA%\ugg\irodori\update.lock` | **★v0.5.6 導入・更新の錠**（プロセスをまたぐ。`LockFileEx` で先頭 1 バイトに掛ける。中身は使わず消さない）。**プロセスが落ちたら OS が外す**。導入・更新が握り、合成の側は「試して放す」で、もう 1 つの ugg の更新中かを見る。MSIX の複製で `%APPDATA%` が別物に見えるプロセスとは共有されない |
| `%APPDATA%\ugg\irodori\.update-backup\` | 更新の退避（固定 URL の 3 本。★v0.5.4）。**★v0.5.6**: 退避が最後まで済んだら `<pkg>.aside-complete` の印を置く（戻すとき、済んだものは入れ直しで入った新しい版を先に退け、済んでいないものは site に残る原本を消さない） |
| `%APPDATA%\ugg\irodori\.update-gate\` | **★v0.5.6 1 回合成のゲートの作業場所**（参照音声の写しと、その事前変換の結果。終わったら消す） |
| `%APPDATA%\ugg\irodori\sidecars.json` | **★v0.5.5 起動したサイドカーの台帳**（`[{port, pid, owner?}]`。★v0.5.6 項目 4: `owner` は記録を書いた ugg の `{pid, started}`（開始時刻と組で見分ける — pid は再利用される）。v0.5.5 の記録には無く、持ち主なしとして扱う。**1 件ずつ読む**（読めない記録があっても残りを失わない。以前は 1 件で台帳が丸ごと空になった）。書き込みは一時ファイル → 差し替え。持ち主なしの記録には欄を書かないので v0.5.5 も読める）。起動直後に追記し、**止まったのを見届けてから**ポートと pid の組で消す。次の起動で孤児掃除（`sweep_orphans`）が読み、**持ち主のいない記録だけを**（★v0.5.6 項目 4。この ugg と、生きているほかの ugg の記録には触らない）`/health` の**応答の形**で自分のものと確かめてから止める。導入・更新の入口でも同じ掃除をする（★v0.5.6 項目 4）。**確かめ終えた記録から 1 件ずつ消す**（途中で落ちても未確認の記録を失わない）。接続を拒否された記録は捨て、**つながったのに応答が無い記録は残す**（合成中はイベントループが塞がり `/health` に答えない） |
| `%APPDATA%\ugg\irodori\sidecars.lock` | **★v0.5.6 台帳の錠**（項目 4。プロセスをまたぐ。`update.lock` と同じ `LockFileEx` で、プロセスの中の錠と二段）。台帳を読んで書き戻す間だけ握る。**足すときは 2 秒待って取れなくても書き**（記録を失うと強制終了のとき孤児を追えない）、**消すときは見送る**（錠なしで書くと、ほかの ugg が足した記録を消しうる。消し損ねた記録は次の掃除で片付く）。中身は使わず消さない |
| `%APPDATA%\ugg\irodori\refs\<slot>_<id>.wav` | 参照音声本体（voice_refs.file_path から参照） |
| `%APPDATA%\ugg\irodori\refs\<slot>_<id>.<合成モデル>+<コーデック>.<精度>.<前処理>.latent.pt` | **★v0.5.6 参照音声の事前変換の結果**（spec §6.0 項目 1）。サイドカーが参照 wav の隣に作り（書きかけは `.latent.pt.tmp`）、以後の合成に使い回す。参照 wav より古ければ作り直す。値を決めるものは全部名前に入れる（合成モデルとコーデックの repo@revision・両者の精度・参照の前処理）。**消すのは `voice_ref::delete_file`**（参照音声の削除と作り直しの両経路）で、`<stem>.` で始まり `.latent.pt` / `.latent.pt.tmp` で終わるものを消す。**名前の形・置き場所・`.tmp` の付け方は `sidecar.py` の `ref_latent_path` と揃える約束**（契約テスト `the_latent_name_matches_the_sidecar`） |
| `%APPDATA%\ugg\ugg.log` | アプリログ（`system/log.rs`。追記、2MB で `ugg.log.1` へ 1 世代退避。spec §5） |
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
    pub ghost: Mutex<Result<GhostBundle, String>>,  // ghost/shell/dict（ロード結果を Result のまま保持）
    pub dialogue: DialogueState,                    // 対話進行
    pub presence: PresenceState,                    // 存在感
    pub tts: TtsState,                              // エンジン保持
    pub pomodoro: PomodoroState,                    // ポモドーロ
    pub window: WindowState,                        // ウインドウ
    pub governance: GovernanceState,                // ★M7 発話ガバナンス（インメモリ）
    pub context: ContextState,                      // ★M9 状況検知（インメモリ）
}
```

※ 初版に載っていた `workers: WorkerHandles` は実装されていない（watcher は
`tauri::async_runtime::spawn` の投げ放しで管理し、ハンドル集約は不要だった）。

```rust
// system/governance.rs (★M7、M9 拡張)。backoff のみ app_settings に永続化
pub struct GovernanceState {
    pub last_spoke: AtomicI64,                       // 最後に自発発話が到達した unix 秒
    pub last_by_category: [AtomicI64; 12],           // カテゴリ別（SpeechCategory::index()）。段 5 が参照。★M11 9→12（SituationRain/RegularMorning/RegularEvening）
    pub backoff: [AtomicU32; 12],                    // ★M9 🔕 回数。governance_backoff:<cat> から復元。★M11 12 へ拡張
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
    pub greeted: AtomicBool,                        // 起動挨拶済み
    pub monologue_refill_ts: AtomicI64,             // ★M14 独り言キャッシュを最後に補充しようとした unix 秒
    pub cost_unknown_notified: AtomicBool,          // ★v0.5.3 「当月コストを集計できない」告知をこのプロセスで出したか（DB 異常時も効くよう意図的にプロセス内）
    // コスト上限の告知済みは AtomicBool ではなく app_settings の月次タグ（§2.2 cost_warned_80_month / cost_limit_notified_month）
}

pub struct PresenceState {
    pub idle_fired: AtomicBool,                     // 現放置期間で発火済か
    pub reading: AtomicBool,                        // テキスト読み上げ中（自発発話を抑制、text-reader-spec K6）
}

pub struct TtsState {
    pub voicevox: Mutex<Option<VoicevoxEngine>>,    // 遅延 init
    pub irodori: IrodoriClient,                     // サイドカーの handle・last_used 等はクライアント内部で保持（§8.4）
}

pub struct PomodoroState {
    pub focus: AtomicBool,                          // 静音判定で参照
    pub gen: AtomicU64,                             // タスクキャンセル用世代
    pub phase: AtomicU32,                           // 0=idle, 1=focus, 2=break
    pub remaining: AtomicU32,
    pub round: AtomicU32,
    pub rounds: AtomicU32,
    pub paused: AtomicBool,                         // 一時停止中（spec §4.4.5）
}

pub struct WindowState {
    pub alpha_mask: Mutex<DecodedMask>,             // クリック透過判定
}

pub struct GhostBundle {
    pub ghost: GhostManifest,
    pub shell: ShellManifest,
    pub shell_dir: PathBuf,
    pub dictionary: Dictionary,
}
```

### 3.3 ライフサイクル

```
[boot]（詳細は §14.1）
  ├─ install_panic_dialog_hook（起動時 panic を MessageBox で見せる）
  ├─ tauri::Builder に tauri_plugin_autostart を登録
  └─ setup フックで:
       ├─ AppState::initialize（log 初期化 → DB open・migrate → settings 読み込み（"settings" キー）→ ghost/shell/dict ロード）
       ├─ app.manage(state.clone())
       ├─ system::governance::load_backoff
       ├─ ウインドウ設定 + クリック透過ポーリング起動（window::configure_main_window / start_cursor_watcher）
       ├─ presence::window_pos::dock / spawn_dock_keeper
       ├─ tasks::spawn_*（ランダムトーク・放置・Irodori のアイドル/ヘルス・更新・時事ネタ・リマインダー・日課・状況・カレンダー）
       ├─ window::tray::install
       ├─ commands::tts::spawn_preinit（tts_enabled のとき。voicevox の事前 init）
       └─ tts::sidecar::install_sidecar_script / sweep_orphans（孤児サイドカーの掃除）

[通常運用]
  ├─ Tauri コマンド → commands/* → 各サブ状態 / DB
  ├─ バックグラウンドタスクの発話は system::deliver の単一ゲート（静音・busy 直列化）を通る
  └─ notify(kind, args) でゴースト発話 or トースト

[終了]（詳細は §14.3）
  ├─ トレイ・右クリックメニューの「終了」（★v0.5.6 同じ経路）→ 見えていれば quit / todo_quit を発話して待つ → 位置の即時保存 → サイドカー停止（POST /shutdown）→ exit
  │   （隠している・最小化しているときと、待っている間の 2 回目は、発話せずにすぐ）
  └─ 終了シグナルを受ける処理は無い（強制終了ではサイドカーは Job Object で一緒に終わる ★v0.5.6。それより前の版が残したものは次の起動の sweep_orphans が止める）
```

---

## 4. Tauri コマンド契約

エラーはすべて `Result<T, String>`（ユーザー提示可能な日本語メッセージ）。

### 4.1 boot / lifecycle

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_boot_payload` | なし | `BootPayload` | キャラ画像（data URL）、settings、`char_positions`（保存済みキャラ X 位置。無ければ null）等 |
| `frontend_ready` | なし | `()` | boot 完了通知。起動挨拶（first_boot or boot）+ 更新チェック起動 |
| `quit_app` | なし | `()` | 右クリックメニュー「終了」。**★v0.5.6 トレイの「終了」と同じ `lifecycle::quit_with_farewell` を通る**（見えていれば終了前の確認かあいさつ → 位置の保存 → サイドカー停止 → exit。隠しているときはすぐ）。終了の処理を始めたら戻る（exit を待たない） |
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
| `send_user_message` | `text: String` | `DialogueResponse` | モード判定・降格制御。**掛け合いパターン3/4 の3ターン目 `extra: SpeechTurn` が付くのはこの戻り値のみ**（optional・advanced のみ・安全縮退時は省略。§10.4） |

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
| `get_cost_status` | なし | `CostStatusView` | ★v0.5 当月の LLM 利用額 / 上限 / 比率 / 80%・超過フラグ / 料金表に載っているか（`pricing_unknown_remote` = 未掲載かつ base_url がリモート = **上限が発動しない構成**）。spec §4.2.7 |
| `get_profile` | なし | `ProfileEntry[]` | |
| `add_profile` | `content: String` | `ProfileEntry[]` | origin="manual" |
| `delete_profile` | `id: i64` | `ProfileEntry[]` | |

### 4.6 log / data

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_chat_log` | `limit: u32` | `LogEntry[]` | 新しい順 |
| `clear_history` | `include_profile: bool` | `ClearResult` | |
| `export_data` | `include_profile: bool` | `String` | 保存パス返却。**★v0.5.1: キャッシュ 3 つ（`calendar_cache` / `topics_cache` / `monologue_cache`）を除く全 9 テーブル**（schema `ugg-export-v2`）。除外したことは payload の `omitted_caches` に明記。**★v0.5.3: 部分救出**。`build_export_payload` へ切り出し、テーブルごとに `rescue()` で「出せた / 出せなかった」を振り分ける。失敗しても打ち切らず、`failed_tables` に名前と理由を残して値は `null`。schema `ugg-export-v3` |
| `check_update_now` | なし | `()` | 設定パネル「いますぐチェック」。`update_feed_url` 未設定なら Err、結果は notify 経由で発話 |
| `get_db_health` | なし | `DbIntegrity` | **★v0.5.1**（spec §4.5.5）。起動時 `PRAGMA quick_check` の結果と、破損時に作った退避先（原本コピー / `VACUUM INTO` 救出コピー）を返す。**正常時は何も検知せず退避コピーも作らない。破損しても DB は作り直さず起動も止めない**（データを取り出せる状態を優先）。**保全は破損 1 件につき 1 回**（既存の退避があれば作り直さずそのパスを返す。毎起動コピーは、まさに対象ユーザーのディスクを食い潰す）。原本コピーは `-wal` / `-shm` も同じ規則で運ぶ（本体だけだと未チェックポイント分が抜ける）。**★v0.5.2: 破損時は `migrate()` の失敗を伝播させない**（`AppState::initialize`。健全な DB での失敗は従来どおり致命）。`VACUUM INTO` 失敗時の 0 バイト残骸は削除する。**★v0.5.3: 整合性検査を pragma より前に実行**し、pragma 失敗は健全時のみ致命。既存の退避・救出コピーは `is_usable_preserved` で妥当性（空でない / 救出コピーは `quick_check` 通過）を確認してから採用する |

**注**: 旧設計の `open_log_dir` は不採用（ログ閲覧はアプリ内チャットログパネル + `export_data` で代替）。

### 4.7 tts

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `synthesize_voice` | `text: String, slot: "main"\|"sub", caption: String\|null（省略可）` | `String` | WAV を base64 で返す（slot 基準、エンジン振り分けはバックエンド）。★ `caption` は Irodori 実モデルのみ使用（他経路は無視、空文字は None 正規化。script-reader-spec.md §3.3）。**v3 本体では効いていない**（§7.1。v0.5.7 の差し替えで閉じる予定で、効くことは差し替え時に検証する） |
| `list_voices` | なし | `VoiceOption[]` | 現在エンジンの声一覧 |
| `voicevox_assets_ready` | なし | `bool` | 資産有無 |
| `download_voicevox_assets` | `agreed: bool, gh_token: String\|null` | `()` | 規約同意必須、進捗は `voicevox-download` イベント |
| `set_github_token` | `token: String` | `()` | |
| `has_github_token` | なし | `bool` | |
| `delete_github_token` | なし | `()` | |
| `irodori_check_gpu` | なし | `GpuInfo` | ★ 起動時 GPU 検出（Q3 対応） |
| `irodori_assets_ready` | なし | `bool` | ★ **「使えるか」だけを返す**（★v0.5.4 で意味を明確化）。最新かどうかは混ぜない — 混ぜるとフロントの `canUseReal = gpuOk && assetsOk` が倒れ、古いが動いている環境で `tts_irodori_use_real_model` が黙って false に書き換わって永続化される |
| `update_irodori_runtime` | なし | `String[]`（入れ直せた pin 名） | **★v0.5.4**（spec §6.0 項目 3）。初回導入と**別経路**だが、**同時には走らない**（`IrodoriBusyGuard`。同じ site-packages を 2 経路が触ると退避も復元も守れない）。**実行前に稼働中のサイドカーを停止する**（止めないと合成は旧コードのまま続き、遅延 import するモジュールを差し替えると動いているプロセスが壊れる）。古くなった pin だけを「**退避 → 入れ直し → import の前後比較 → 悪化が無ければ退避を捨てる**」で入れ替える（失敗したら戻す。戻せなければ退避を残してログに場所を出す）。**`python` は対象外** — `ensure_python_embeddable` は `python.exe` があれば skip し、稼働中のインタプリタを安全に差し替える方法が無いため、その旨を返して止まる。**ただし `outdated` に `python` を入れるのは実物の版が違うときだけ** — 記録の欠落で混ぜると更新経路が丸ごと止まる。成功後は**入れ直せた分だけ**記録（`installed.json`）を書き換える（実物の版が pin と一致していることを確認できたときは `python` も記録する）。**記録の書き込みは更新処理の一部**で、コマンド層では行わない。**★v0.5.6（spec §6.0 項目 3）: 1 つのトランザクションにした** — ① 入口で**プロセスをまたぐ錠**（`update.lock`）を取り、自分のサイドカーを止め、**起動時の孤児掃除の終わりを待ち**、記録にあるサイドカーで**生きているものが残っていれば始めない**（torch の DLL を掴まれていると入れ替えも全戻しも失敗する。**★項目 4 で、数える前に持ち主のいない孤児を止める**ようにした — ほかの ugg が使っているものは止めず、断る説明を持ち主ごとに分ける）② 前回の更新が途中で止まっていれば先に戻す ③ パッケージを入れ替えるなら全配布の版を控える ④ **固定の段取り**: 名前付き要件（torch 系を先に CUDA の index から、それ以外を 1 回の pip で torch を縛って）→ 固定 URL の 3 本（依存の順）→ モデル → **1 回合成して確かめる**（`sidecar.py --synth-once`。試すのは取得したいまのビルドの値）⑤ **どこで失敗しても全部戻す**（固定 URL は退避から、名前付きの配布は控えの版を `--no-deps` で入れ直す＝通信が要る、モデルは戻さない）。ゲートが不合格なら戻したあとに**元の状態でもう一度試し**、「更新が原因」か「元から合成できない環境」かを言い分ける。VRAM 不足は別の案内で戻す。GPU は**差分**で見る（前は使えたのに使えなくなったら戻す。元から見えない環境は確かめずに成立）。参照音声が無ければ確かめずに成立（記録に残す） |
| `get_irodori_status` | なし | `IrodoriStatus { present, has_record, up_to_date, outdated[], resolved{} }` | **★v0.5.4**（spec §6.0 項目 2）。導入記録（`installed.json`）と、いまのビルドが要求する pin を突き合わせる（**★v0.5.5 で名前付き要件とモデルも**。欄が空の記録は v0.5.4 の固定の基準値で読む）。`present` は `irodori_assets_ready` と同じ意味、`up_to_date` は**別の信号**。**記録が無い環境（v0.5.4 より前の導入）は `up_to_date: false`** — 分からないものを「最新」とは言わない。**`outdated` は記録が無くても名指しする**（入れ直せば済むものは分かる）。**`python` だけは記録ではなく実物（`python.exe --version`）で判定**し、聞けなければ「古い」とは言わない。呼び出し側は `outdated` をそのまま対象にする |
| `download_irodori_assets` | `agreed: bool` | `()` | ★ 進捗 `irodori-download`。**★v0.5.6**: 更新と同じ入口を通る（プロセスをまたぐ錠・自分のサイドカーを止める・起動時の孤児掃除を待つ・持ち主のいない孤児を止める・生きているサイドカーが残っていれば始めない）。**自分のサイドカーを止めていなかった**（更新の経路だけ止めていた） |
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
| `complete_onboarding` | `nickname, interests, talk_style, topics_enabled` | `()` | 聞き取った 4 項目をそれぞれの保存先へ投入（spec §4.2.5）: nickname / talk_style / 興味 → `user_profile`（origin=onboarding）、興味 → `interest_topics`（時事ネタ RSS のキーワード。空白・重複を除き上限 20）、`topics_enabled` → **`Settings.topics_enabled`（時事ネタの明示同意。spec §3.3/§4.4.6「既定オフ・オンボーディング同意必須」）**。同意時のみ設定を書き換えて `settings-changed` を emit |
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
| `feedback_speech` | `speech_id: String, category: String` | `()` | 🔕「いまのは邪魔」。**最新のタグ付き発話と一致したときだけ**適用（誤適用は黙って無視）。backoff +1 を `governance_backoff:<category>` に永続化し gate 段 5 の間隔を線形延長、3 回でカテゴリトグルを OFF（settings 永続化 + settings-changed）。対象は `feedback_target()`（Situation* 5 種〔`SituationRain` 含む〕+ ★M11 `RegularMorning` / `RegularEvening`）で、それ以外は no-op。Regular* は段 5 の間隔延長が掛からず（段 5 は Situation* のみ）、回数と 3 回での枠 OFF だけが効く |

★M10 カレンダー（spec §4.6.4、読み取り専用）。変更系は `calendar-changed` を emit。

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `get_calendar_events` | `days?: u32`（省略時 2＝今日明日） | `CalendarEvent[]` | 表示窓の予定を開始順で |
| `refresh_calendar` | — | `usize`（取得件数） | 全ソースを今すぐ再取得 |
| `add_calendar_source` | `source: CalendarSource`（`{kind:"file",path}`\|`{kind:"url",url}`） | `CalendarSource[]` | 追加。source_id は index ベースのため**キャッシュを全 clear**して再取得 |
| `remove_calendar_source` | `index: usize` | `CalendarSource[]` | 削除。同上でキャッシュ全 clear |

★M11 天気（spec §4.7.2）。新規イベントなし（天気キャッシュ更新は UI へ push しない）。

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `search_location` | `query: String` | `LocationHit[]` | Open-Meteo Geocoding（language=ja）。2 文字未満は Err、0 件は空配列。設定は書かない（選択・保存はフロントが `set_settings`） |
| `get_weather` | — | `WeatherSnapshot \| null` | `ensure_fresh` 経由の今日/明日予報。`weather_ready` でなければ null |

### 4.10 window

| コマンド | 引数 | 戻り値 | 説明 |
|---|---|---|---|
| `update_alpha_mask` | `mask: AlphaMask` | `()` | クリック透過用 |
| `set_char_positions` | `main: f64 \| null, sub: f64 \| null` | `()` | キャラごとの X 位置（ステージ内 CSS px、視覚ボックス左端）を `app_settings.char_pos` に保存。ドラッグ終了時に呼ぶ（spec §4.1.6 / §4.3.4） |
| `list_monitors` | — | `MonitorList` | ★M13 表示モニタの選択肢（`monitors[]` + `has_pref` + **`pref_unresolved`** = 選択はあるが今の構成に見つからない）。モニタの知識は `presence/window_pos.rs` に集約し、フロントは `@tauri-apps/api` の window API を使わない |
| `set_monitor_pref` | `pref: MonitorPref \| null`（null = 自動） | `()` | ★M13 表示モニタを選ぶ。`app_settings.monitor_pref` に保存し、**その場で再ドック**する（1 秒監視を待たせない） |

---

## 5. イベント契約

| イベント | payload | 用途 |
|---|---|---|
| `dialogue` | `DialogueResponse` | バック起点の発話（ランダムトーク・notify 経由の system 発話）。★M9: deliver 経由の発話には `speech_id` / `category` / `priority` / `feedback_allowed` が付く（🔕 用。ユーザー応答には付かない。TS 型は optional）。**このイベント経路は常に `banter::pattern_1` で組むため `extra`（3ターン目）は載らない** — 3ターン目が付くのは `send_user_message` の戻り値だけ（§4.3・§10.4） |
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
- ★M11 天気は新規イベントを追加しない。天気キャッシュ更新は UI へ push せず、設定 UI は `get_weather` の戻り値で表示する。地域設定・解除は既存 `settings-changed` が飛ぶ。
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
- **★v0.5.1 実装**: `Dictionary::pick_recall`。`low::reply` が `pick_reply` より
  **先に**評価する（想起が通常応答より優先）。一致が無ければ従来どおり。
  - **`source_keywords` は `ghost::dict::extract_keywords` が記憶本文から自動生成**する
    （`insert_profile` の全 5 呼び出しが通る）。形態素解析器は入れず、漢字・カタカナ・
    英数の連なりを 2 文字以上・最大 8 語まで拾う素朴な方式。取りこぼすが、拾ったものは
    概ね名詞なので想起のトリガーとして実用になる。1 文字語は誤爆するので拾わない。
    **キーワードはユーザーが入力した語からのみ作る**（`complete_onboarding` は定型文ではなく nickname / talk_style / interests の値を渡す）。定型文ごと渡すと「ユーザー」「希望」「興味」が想起のトリガーになり、`pick_recall` が `pick_reply` より先に評価される以上、**low モードの通常応答を無関係な入力まで奪う**（v0.5.1 の監査で検出）。
  - v0.4.1 までは `pick_recall` が存在せず、`insert_profile` の全呼び出しが
    `source_keywords` に `None` を渡していたため、**契約が 4 箇所に揃っているのに
    一度も発火しなかった**（`#[allow(dead_code)]` が警告も消していた）。
  - **low モードで効く**のが要点。記憶は advanced が貯めるが、想起は無料・
    オフラインの既定モードでも起きる（spec §4.2.1 の二モード）。

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

  # 問いかけ（B-4）は events キーではない
  #   → advanced の掛け合いパターン 5（spec §4.2.4・★v0.5.1）。辞書系は常にパターン1

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

  # ★M11 降雨の一言（spec §4.7.2。SituationRain=Ambient・朝帯 1 日 1 回・既定 OFF）
  weather_rain: [ ... ]         # {label} = 天気ラベル、{p} = 降水確率%
  weather_rain_outing: [ ... ]  # 当日に時刻付き予定があるときの強め版。{label} {p}
  # ★M12 定例会話（spec §4.7.1。RegularMorning/RegularEvening=Ambient・1 日 1 回・既定 OFF）
  regular_morning: [ ... ]      # 朝の導入 + {body}（Rust 集約文 §5.4）+ 締め。{body} 空でも成立
  regular_evening: [ ... ]      # 夜のねぎらい + {body} + 締め
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
  probability: 0.05                          # events の when 条件用（低確率発生）
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
| `cost_unknown` ★v0.5.3 | **当月コストを集計できない**（上限が有限なら止める） | 同上 |
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

### 7.0 再生の前提: WebView2 の自動再生ポリシー（★v0.5.1）

ユーザー操作を伴わない発話（起動挨拶・ランダムトーク・リマインダー）では
`AudioContext` が `suspended` のままになりうる。この状態で `start()` を呼んでも
**例外を投げず Promise も解決する**ため、`speaker.ts` が decode/play の失敗を
`console.warn` で握り潰すこととあわせて、**「正常に喋ったように見えて音だけ出ない」**
という形でしか現れない。

対策は 2 段:

1. `tauri.conf.json` の `app.windows[].additionalBrowserArgs`。
   **この値は wry の既定引数を「追加」ではなく「置換」する**
   （`wry::webview2` は `additional_browser_args.unwrap_or_else(既定を組み立てる)`）。
   既定は `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection` に、
   `WebViewAttributes::autoplay` が既定 `true`（Tauri は上書きしない）のため
   `--autoplay-policy=no-user-gesture-required` を足したもの。
   **つまり自動再生ポリシーは元から効いており**、ここに自前の値だけを書くと
   得るものが無いまま既定の抑止だけを失う。**必ず既定分も併記する。**
2. `speaker.ts` / `reader.ts` の `ensureAudioCtx` で `state === "suspended"` なら
   `resume()` を試みる（**待たない**。待つと発話が遅れ、失敗時に無音なのは変わらない）。



### 7.1 全体フロー

> §7.1〜§7.4 の図とコードは **Phase 2 時点の素案**。実装に `TtsEngine` trait・`IrodoriEngine`・`needs_kana_preprocess` は無い。
> `commands/tts.rs` の `synthesize_voice` が `settings.tts_engine` の値で振り分け、voicevox 経路は `TtsState.voicevox` の
> `VoicevoxEngine`、Irodori 経路は `preprocess_for_irodori` で漢字をかなに変えてから `IrodoriClient::synthesize`（`tts/irodori.rs`）を呼ぶ。

```
   synthesize_voice(text, slot, caption?)   ★ caption は Irodori 実モデルのみ使用
                                            ★v0.5.4 訂正: **v3 では効いていない**
                                            (本体 checkpoint が use_caption_condition:false)。
                                            v0.5.7 の差し替え（MF）で効くかを確かめる
                                            （MF の use_caption_condition は未確認。spec §6.0 の spike）
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

> 下のコードは **Phase 2 時点の素案**。実装に `KanaPreprocessor` 型は無く、`tts/preprocess.rs` の関数
> （`to_hiragana_preserving_emoji` など）が、`TtsState.voicevox` の VoicevoxEngine が持つ OpenJtalk を借りて変換する
> （Irodori 経路の合成が送信前に `commands/tts.rs` の `preprocess_for_irodori` を呼ぶ。下の「初期化コスト」と §8.8）。

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
- 初期化コスト: 専用の OpenJtalkRc は持たず、`TtsState.voicevox` の VoicevoxEngine が持つ OpenJtalk を流用する（未初期化なら先に初期化する。失敗したら元のテキストのまま合成する）

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
├── python\              -- M1: 公式 Embeddable Python 3.11.9（Windows x64, 約 10MB）
│   ├── python.exe
│   ├── python311.dll
│   ├── Lib\site-packages\  -- pip install で配置（torch, fastapi 等、~2GB）
│   └── ... (標準ライブラリ)
├── model\               -- Irodori-TTS モデル（HF から DL、数GB。リポジトリと revision ごとに別フォルダで、旧版を上書きしない）
├── refs\                -- 参照音声 wav 格納
│   ├── main_<id>.wav
│   ├── main_<id>.<合成モデル>+<コーデック>.<精度>.<前処理>.latent.pt  -- ★v0.5.6 事前変換の結果（§2.4）
│   └── sub_<id>.wav
├── sidecar.py           -- FastAPI エントリポイント
├── installed.json       -- ★v0.5.4 導入記録（§2.4）。★v0.5.6 モデルの読み先の正本
├── sidecars.json        -- ★v0.5.5 起動したサイドカーの台帳（§2.4）。★v0.5.6 持ち主の欄
├── sidecars.lock        -- ★v0.5.6 台帳の錠（プロセスをまたぐ。§2.4）
├── update.lock          -- ★v0.5.6 導入・更新の錠（プロセスをまたぐ。§2.4）
├── update-versions.json -- ★v0.5.6 更新の前の版の控え（失敗して戻せなかったときだけ残る。§2.4）
├── .update-backup\      -- 更新の退避（固定 URL の 3 本。更新の間だけ）
├── .update-gate\        -- ★v0.5.6 1 回合成のゲートの作業場所（更新の間だけ）
└── ready.json           -- サイドカーが待ち受けポートと pid を書き出す（起動のたびに作り直す）
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
- 規約同意のチェックや同意文言は無い（spec §4.5.1 が規約同意を求めるのは voicevox_core のみ）。「ランタイムをダウンロード」押下時に、取得物（Python ランタイム・PyTorch (CUDA 12.8)・実モデル実行時ランタイム）と通信量（約 2〜3 GB）・所要時間（10〜20 分）を示す確認ダイアログを出し、OK なら `download_irodori_assets` を `agreed: true` で呼ぶ
- DL 進捗は `irodori-download` イベント。**行が届いたときに流す**（★v0.5.6 項目 2。以前は子プロセスが
  終わってからまとめて流しており、数 GB の取得中は画面が 1 行のまま固まった）
- **パイプ越しでは pip も huggingface_hub も取得中の進捗を出さない**（★v0.5.6 で判明。pip の rich は
  端末でないと描かず、hub の tqdm は `disable=None` の既定で端末でないと出ない）。そこで pip には
  `--progress-bar raw`（pip 24.1 以降。入っている版を見てから付ける）を渡し、`Progress N of M` の行を
  「取得中 N / M MB（P%）」へ直して割合が変わったときだけ流す。`sidecar.py` は `--download-only` の
  取得の間だけ hub の判定（`is_tqdm_disabled`）を「自動なら出す」に差し替える
- 失敗したときは**理由**（pip の `ERROR:` の行・例外の行）をエラーに添え、直前の 20 行を `ugg.log`
  （`[irodori:python]`）に残す。進捗の上書きの行は残さない
- **出力も読み書きも CPU も 5 分無ければ止める**（`child_process`。Job Object の集計で孫まで見る）
- **★v0.5.6 読み先と取得先を分ける**（spec §6.0 項目 3a）: `model_args_for_fetch()`（いまのビルド。`--download-only`）と
  `model_args_for_read(asset_root)`（導入記録から名前ごとに決める。サイドカーの起動）。起動時に「読み先をどこから
  決めたか」を `ugg.log` に 1 行残す（`[irodori] モデルの読み先を記録から決めました: …`）
- **★v0.5.6 一発合成のモード**（spec §6.0 項目 3b）: `sidecar.py --synth-once --voice-ref <wav>`。HTTP は立てない
  （`--download-only` と同じく、ポート確保と `--ready-file` の検査より前で分岐）。stdout に目印付きの 2 行
  （`UGG_SYNTH_ONCE_START {cuda, torch_cuda, vram_free_mb, vram_total_mb}` と `UGG_SYNTH_ONCE {ok, ms, bytes | kind, error}`）、
  終了コードは 0 = 合格 / 1 = 失敗 / 2 = VRAM 不足 / 3 = GPU が見えない。VRAM 不足では例外にならずプロセスごと
  落ちることがあるので、モデルを読む前に空き VRAM を出す。締め切り 10 分（`run_streaming_until`）
- モデル取得では `local_dir_use_symlinks` を渡さない（★v0.5.6。hub 0.23 以降は無視、1.x では TypeError）

### 8.4 プロセス管理（O2）

> 下のコードのうち `SidecarHandle` の定義は実装どおり（v0.5.6）。続く `ensure_sidecar_running` / `idle_watcher` は
> **Phase 2 時点の疑似コード**で、実装とは次が違う: ハンドルを守るのは `std::sync::Mutex`、`last_used` は
> `IrodoriClient` 側の `AtomicI64`、起動の完了は `/health` の ping ではなく `ready.json`（起動した子の pid と一致するもの）で知り、
> 停止は `POST /shutdown` のあと必要なら `kill`（`tts/sidecar.rs` / `tts/irodori.rs`）。

```rust
pub struct SidecarHandle {
    asset_root: PathBuf,           // 台帳 sidecars.json の位置（止めたら記録を消す）
    pub port: u16,                 // 動的割当（sidecar.py が ready.json に書き出す）
    pub pid: u32,                  // 台帳の記録をポートと pid の組で消すため
    pub mock: bool,                // ★v0.5.6 --mock で起動したか（実モデルの ON/OFF を切り替えたら起動し直す）
    pub child: Child,              // tokio::process::Child
}
// last_used はハンドルではなく IrodoriClient 側の AtomicI64（unix 秒、0 = 起動なし）

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
    // 1) Windows DXGI で物理アダプタを列挙し、NVIDIA（VendorId 0x10DE）を探す（nvml は使わない。CUDA の可否はサイドカー側で確かめる）
    // 2) なければ GpuInfo { available: false, ... } を返す
    // 3) 設定 UI で「Irodori-TTS は GPU 環境でのみ利用可能」と表示し DL ボタン無効化
}

// サイドカー稼働中の監視（tasks::spawn_irodori_health_watcher）:
// /health は実モデルモードで GPU が無いと 503 {status: "no_gpu"} を返す（起動の待ち合わせは ready.json で、/health は見ない）
// 30 秒ごとに /health を ping（3 秒タイムアウト・2xx 以外も失敗・未起動なら何もしない）し、3 回連続失敗で
//   shutdown → 20 分は ensure_sidecar_running が即 SidecarStart を返す → 理由を ugg.log へ（通知のゲートより前）
//   → notify(IrodoriUnavailable)（synthesize_voice と共有の 5 分クールダウン）
// 合成の失敗（VoiceRefMissing 以外）は synthesize_voice が理由を ugg.log へ残して voicevox_core で再合成し、
//   成功したときだけ notify(IrodoriUnavailable)（commands::tts::decide_fallback）
```

### 8.7 参照音声管理（R1+R3 ハイブリッド）

```
[参照音声の用意]
   └─ 自動生成はしない（shell.json に既定キャプションを持つ項目は無い）。設定パネルの音声ページでキャプションを入力して生成する
       （未生成のまま Irodori で合成すると VoiceRefMissing を返し、voicevox へはフォールバックしない）

[設定パネル: 音声タブ]
   ├─ メイン/サブ それぞれに参照音声状態を表示
   ├─ 「参照音声を生成 / 再生成」ボタン
   │   └─ クリック → 同じ欄のキャプション入力欄（空なら案内して中止）の値で voice_ref_generate → /v1/voice_ref/generate
   ├─ 「プレビュー再生」ボタン
   │   └─ クリック → /v1/audio/speech で短文合成
   └─ 「参照音声を削除」ボタン
       └─ voice_refs テーブル削除 + ファイル削除（★v0.5.6: 事前変換の結果 `.latent.pt` も一緒に消す。
          作り直しで古い参照 wav を消すときも同じ `voice_ref::delete_file` を通る）
```

`voice_refs` テーブルは MVP では `UNIQUE(slot)` で各 slot 最新1件のみ（複数履歴は将来課題）。

### 8.8 漢字→ひらがな前処理の呼び出し

- Irodori 経路の合成（`commands/tts.rs`）が送信前に `preprocess_for_irodori` を呼び、`TtsState.voicevox` の VoicevoxEngine が持つ OpenJtalk で変換する（`tts/preprocess.rs` の `to_hiragana_preserving_emoji`）
- 専用の OpenJtalkRc インスタンスは持たない。変換中は `TtsState.voicevox` の Mutex を保持する

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

- ウインドウ = **モニタ作業領域の全幅 × 高さ 1024 (logical) の透明ステージ**。作業領域下端に固定（presence/window_pos.rs が起動時ドック + 1 秒監視で再ドック）。高さはスケール上限 2.0 でデフォルトシェルのキャラ (384px→768px) + バルーン/入力欄を収容する値（作業領域が足りなければキャップ）。ユーザーはウインドウをドラッグで動かせない（**表示モニタの選択のみ設定から可能**）
- **★M13 モニタ決定は `resolve_target_monitor` の 1 箇所に集約**（spec §4.1.6）。優先順位は ①明示選択 `monitor_pref` が解決できればそれ ②選択が無いときだけ保存位置/現在位置から逆算 ③主モニタ。**選択がある場合は現在位置を見ない** — 1 秒監視は毎 tick この関数を呼ぶため、ここを分けないとポーリングが選択を上書きする。選択が今の構成で見つからないときは主モニタへ退避するが `monitor_pref` は消さず、構成が戻れば次 tick で復帰する
- **★M13 同一性の判定は `name` + `position` の一致のみ**（`pref_matches`、純関数でテスト済み）。OS のモニタ名（Windows は GDI デバイス名）は接続順で別の物理モニタに付け替わるため、名前だけで採用すると「選んでいないモニタに固定」される。位置も見て、構成が変わっていたら退避に倒す
- **★M13 `apply_dock` は `set_position` → `set_size` の順**。逆順だと旧 DPI のウインドウに新モニタ基準の物理サイズを与えることになり、DPI 変更で再スケールされうる
- 各 `.char-slot` は `position: absolute; bottom: 0; left: <x>px`（x = stage/charpos.ts が管理、CSS px）。**キャラごとに独立して X 移動**し、Y は bottom:0 固定
- `--ugg-scale` は CSS 変数として `:root` に保持し、**各 `.char-slot` に** `transform: scale(var(--ugg-scale))` を適用。`transform-origin: bottom left` のため `left` = 視覚ボックス左端のまま拡縮できる
- 既定配置（char_pos 未保存時）: main はステージ右端、sub は main の左 40px（spec §4.1.1）
- スケール変更・ステージリサイズ時は charpos.ts が全キャラをステージ内に再 clamp する

### 10.3 吹き出し配置計算（キャラ左横・伺か風）

**型の分離**: `SlotName`（"main" | "sub"）はキャラ（pose・TTS 話者・位置の基準・`char-${slot}`）、
`BalloonSlot`（"main" | "sub" | "extra"）は吹き出し枠（DOM）を指す別の型（フロント `src/types.ts`）。
掛け合いパターン3/4 の3ターン目は「1ターン目と同じキャラが、別の吹き出し枠 (`extra`) で喋る」ため、
`SlotName` に `"extra"` は追加しない（追加すると `char-extra` の DOM 参照・`setPose("extra")`・
TTS 話者選択が破綻する）。`balloon.ts` の各吹き出し View は自分が現在基準にしているキャラ
(`charSlot: SlotName`) を保持し、`showBalloon(balloonSlot, charSlot)` で更新する。

```typescript
function reposition(balloonSlot: "main" | "sub" | "extra") {
    const view = views.get(balloonSlot);
    const char = char-${view.charSlot};          // 基準キャラ (main/sub 自身、extra はパターンにより main/sub)
    const rect = char.getBoundingClientRect();    // scale 後の矩形
    // 横: キャラ左端から 24px (しっぽ含む) 空けて右端を合わせる
    let left = rect.left - 24 - balloon.offsetWidth;
    // 左端 8px に収まらない場合はキャラの右横へ反転 (.flip、しっぽも反転)
    if (left < 8) left = rect.right + 24;
    // 縦: キャラ上端 + キャラ高さ × 0.12 (顔の高さ) に上端を置く
    let top = rect.top + rect.height * 0.12;
    // main/sub: 相方の吹き出しと重なる場合、main は相方の上へ、sub は相方の下へ退避
    // extra: main・sub 両方の吹き出しと重なる間、外側 (上方向) へ退避 (§10.4)
    // 最後に上下端 8px で clamp
}
```

- しっぽは吹き出しの側辺（上端から 20px）からキャラ側を向く（通常 = 右辺から右向き、.flip 時 = 左辺から左向き）
- タイプライター進行・キャラのドラッグ移動ごとに再計算（吹き出しの成長とキャラ追従）。ドラッグ追従は
  `charpos.ts` が `repositionAllFor(charSlot)` を呼び、そのキャラを基準にしている吹き出し枠
  （main/sub 自身 + 該当時の extra）をまとめて再配置する
- フォント/border はスケールの影響を受けないため視認性確保

### 10.4 3つ目の吹き出し（A-3 パターン3/4、実装確定）

- パターン3: main → sub → main の **3 ターン目**（話者=main）を `#balloon-extra` に独立表示
- パターン4: sub → main → sub の **3 ターン目**（話者=sub）を `#balloon-extra` に独立表示
- **配置（案A: 話者キャラの横・さらに外側へ退避）**: 3ターン目の話者キャラの左横（既存 main/sub と
  同じ伺か風レイアウト = キャラ左端から 24px 空けて右端合わせ、上端はキャラ上端 + 高さ 12%）に出し、
  main・sub 両方の吹き出しと重なる間は上方向へ追い出す（§10.3 の main/sub 退避ロジックの一般化）。
  3段積んで画面上端に収まらない場合は既存の 8px clamp で詰まる（破綻はしない）
- **バックエンド**: パターン抽選 (`dialogue::banter::pick_advanced_pattern`) は LLM 呼び出し**前**に
  行い、`advanced::system_prompt` がパターン別に出力形式を出し分ける（パターン3/4 のみ JSON に
  `extra: { text, pose }` を要求）。`DialogueResponse.extra: Option<SpeechTurn>`
  (`#[serde(skip_serializing_if = "Option::is_none")]`、フロント型は `extra?: SpeechTurn`) を追加
- **安全縮退**: LLM が `extra` を返さなかった / 空文字だった場合は `banter::assemble_advanced` が
  パターン 3→1・4→2 に縮退し、従来通り2ターンで表示する（辞書経路・サブ無しゴーストは無関係、常に
  パターン1のまま）
- 全ターン描画+発話完了後に一括消去（`hideAllBalloons()`）

---

## 11. ゴースト発話原則の実装（U3）

### 11.1 notify() サービス

実装（`system/notify.rs`）。**★v0.5.6 項目 6 で実装どおりに書き直した**（以前ここにあった severity 付きの素案と二段トーストは
実装されず、spec §3.1 の二段トーストとともに取り下げた）。

```rust
pub enum NoticeOutcome { Shown, Held, Failed }  // reached() は Shown のときだけ真

pub async fn notify(app: &AppHandle, state: &Arc<AppState>, kind: NoticeKind) -> NoticeOutcome {
    ulog!("[notify] {}", kind.fallback_text());          // 理由は必ずログへ（v0.5.5 項目 1）
    if !deliver::window_is_visible(app) { return Held }  // 見えていない間は出さない（§4.6.1 と同じ判定）
    match dictionary.pick_system_message(kind.dict_key()) {
        Some(line) => emit("dialogue", pattern_1("system_message", line)),  // ゴースト発話
        None       => emit("system-toast", kind.fallback_text()),           // spec §3.1 のフォールバック
    }                                                    // 送れたら Shown、送れなければ Failed
}

/// 1 回だけ出す告知: 済んでいなければ出し、届いたとき（Shown）だけ mark する
pub(crate) async fn once_reached(done: bool, show: impl FnOnce() -> Fut, mark: impl FnOnce()) -> Option<NoticeOutcome>
```

- 見えていない間は `Held` を返して出さない。**保留した告知を後から自動で出す仕組みは持たない** — 1 回だけの告知は済みに
  しないので次の機会にもう一度出る。それ以外は出さずに終わる（下の表）
- ★v0.5.6 以前は見えているかを見ずに出し、結果も返さなかった。1 回だけの告知の呼び出し元 4 か所は届いたかを見ずに済みにして
  いた（うち 3 か所は発話より前に）ため、隠している間に出ると、コストの 2 件はその月、集計不能はその起動のあいだ、アプリ更新は
  その版では二度と出なかった

### 11.2 NoticeKind 一覧

| kind | 辞書キー（`system_messages`） | 済みの記録 |
|---|---|---|
| CostWarning80 | `cost_warning_80` | 月 1 回（`cost_warned_80_month`）。**届いたときだけ**記録 |
| CostLimitExceeded | `cost_limit_exceeded` | 月 1 回（`cost_limit_notified_month`）。**届いたときだけ**記録し、続けて ModeDegraded も出す。チャットの turn では返答そのものとして出す |
| CostUnknown ★v0.5.3 | `cost_unknown` | 起動中 1 回（`cost_unknown_notified`）。**届いたときだけ**記録。チャットの turn では返答として出す |
| ModeDegraded | `mode_degraded` | 毎回（降格のたび） |
| ModeRecovered | `mode_recovered` | 毎回 |
| VoicevoxDlComplete / VoicevoxDlFailed | `voicevox_dl_complete` / `voicevox_dl_failed` | 毎回（見えていなければ出さずに終わる。結果は設定パネルにも出る） |
| IrodoriUnavailable | `irodori_unavailable` | 5 分に 1 回（間隔は発話の前に刻む。次の失敗でまた出るので、届いたかは見ない） |
| IrodoriDlComplete / IrodoriDlFailed | `irodori_dl_complete` / `irodori_dl_failed` | 毎回（同上） |
| UpdateAvailable | `update_available` | 版ごとに 1 回（`update_notice_seen:<版>`）。**届いたときだけ**記録 |

辞書キーが既定辞書に実在することは `notify::dict_key_contract` が突き合わせる。
★M7: `ReminderFired` variant は削除（§11.4 の deliver_event 経路へ一本化）。

### 11.3 呼び出し点

- `dialogue/mod.rs`: コストの 80% 警告・上限到達・集計不能（`evaluate_cost_status` / `announce_cost_limit_once` /
  `announce_cost_unknown_once`。どれも `once_reached` を通す）、モードの自動降格・復帰
- `system/update.rs`: 新しい版の検出（`once_reached` を通す）
- `commands/tts.rs`: VOICEVOX・Irodori の資産 DL の完了・失敗、合成に失敗して VOICEVOX へ落ちたとき（`IrodoriUnavailable`）
- `tasks.rs`: Irodori のヘルスチェックが 3 回続けて失敗したとき（`IrodoriUnavailable`）

### 11.4 通知配達サービスと発話ガバナンス（★M7、daily-support-design §3/§4 が正）

M7 で**すべての自発発話**（独り言・idle・リマインダー通知、以降の Tier S 発話）は
`system/deliver.rs::deliver_event` に一本化した。notify()（§11.1）は従来どおり
system_messages 用に残る（コスト・降格・DL 系のシステム告知）。

```rust
// system/deliver.rs
pub enum DeliveryOutcome { Ghost, Toast, Deferred, Failed }  // reached() = Ghost|Toast

pub async fn deliver_event(
    app, state,
    category: SpeechCategory,     // Monologue / Idle / Reminder / Todo / Calendar / Situation* / ★M11 SituationRain / ★M12 RegularMorning・RegularEvening
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
- **daily watcher**（★M8/M11/M12、tasks.rs、60 秒間隔・起動 30 秒後開始）: ①起動時 + ローカル
  日付の変更検知で日課を復活（`tools::todo::reset_recurring`、冪等）し `todos-changed` を
  emit。②朝の時間帯（5:00-11:00）に 1 日 1 回、today の未完了が 1 件以上なら
  `todo_morning`（{count}、Ambient）を配達。告知済み日付は app_settings
  `todo_morning_date` に保持し再起動でも二重告知しない。0 件の朝は告知なしで消化。
  Deferred（静音・busy）は後続 tick で再挑戦、Failed（辞書なし）は当日分を消化。
  リマインダー watcher とは分離（`reminder_notify_enabled` と結合させない）。
  設計書 §7.2 の「既存 watcher にピギーバック」は、リマインダー watcher が通知トグルで
  ループを止める構造のため専用 watcher に変更した（実装判断）。
  ★M11: ③天気の定期取得（`weather_ready` の間 3 時間ごと `weather::refresh_cache`、失敗は
  既存キャッシュ維持）。④降雨の一言（朝帯 5:00-11:00・1 日 1 回、`weather::ensure_fresh` の
  今日材料が降雨なら `weather_rain`/`weather_rain_outing`（当日の時刻付き予定の有無で出し分け）を
  `SituationRain`=Ambient で配達。`weather_rain_date` で dedup。`regular_morning_enabled` 有効時は
  ④降雨・②朝告知とも吸収され単独発火しない（§5.5、判定は設定値のみ）。
  ★M12: tick を §5.1 順に再配置（日課→天気→朝定例→朝告知→降雨→夜定例）。⑤朝・⑥夜の定例会話は
  `regular_slot_due`（純関数: 設定時刻以降・失効窓 6h・曜日マスク・`os_idle_secs()`<5 分、None は
  アクティブ扱い）で 1 日 1 回配達（`regular_{morning,evening}_date` で dedup、消化規約は todo_morning と同型）。
  材料集約・定型文は `system/regular_talk.rs`（low 完結・空項目省略）、mode=advanced は LLM で
  言い回しのみ整形し失敗/降格/タイムアウトは low へフォールバック。**1 tick 1 枠**（朝が発火した tick は夜をスキップ）。
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
- **終了前確認**（★M9、spec §4.6.2 後半・2026-07-17 裁定）: 「終了」時に未完了の
  today ToDo があれば `events.todo_quit`（{count}）を `quit` の代わりに再生（ユーザー起点
  につきゲート非対象）。**★v0.5.6 項目 5 でトレイと右クリックメニューの両方が同じ経路を通る**（以前はメニューの
  「終了」が M3 判断のまま即 exit で、この確認が一度も出なかった）。見えていないときは出さずに終了する。
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

- **zip slip 対策**: zip エントリ名の絶対パス・ドライブ指定・`..` を拒否（`sanitize_zip_path`）したうえで、展開先パスが目的ディレクトリ配下にあることを文字列レベルの正規化（`normalize_path`）後に starts_with で検証する。`Path::canonicalize` は Windows で `\\?\` が付き未作成のパスと比較できないため使わない。manifest の `id` も単一のフォルダ名に限定する（`validate_asset_id`。区切り文字・ドライブ指定・`.`/`..`・制御文字・前後空白・Windows 予約名・末尾ドット・UTF-8 で 65 バイト以上（日本語だけなら 22 文字以上）を拒否、★v0.4.1）
- **ファイル名検証**: 制御文字・予約名（CON, PRN 等）の除外は manifest の `id`（＝導入先フォルダ名）にだけ掛けている。zip エントリ名・フォルダ内のファイル名には掛けていない（エントリ名は上の zip slip 検査と下の拡張子検査のみ）
- **サイズ上限**: 展開後合計 1 GB 上限（定数 `MAX_UNCOMPRESSED_BYTES`。設定からは変えられない。超過時エラー）。フォルダ導入は再帰深さ 10 まで（`MAX_DIR_DEPTH`）
- **ファイル種別**: shell 用は .png / .jpg / .jpeg + .json、ghost 用は .yaml / .yml / .json / .md のみを許容する。それ以外の拡張子が 1 つでも含まれていれば導入全体を拒否する（`ForbiddenFile`。警告だけで通す経路や options は無い）

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
- **導入は非破壊（★v0.5.3、spec §4.5.6）**: 上の擬似コードは Phase 2 時点のもの。実装（`commands/assets.rs` の `install_one` / `swap_in`）はまず `<assets>/.staging/` に展開し、展開結果の manifest `id` が確認時と一致するかを照合してから差し替える。失敗したら作業ディレクトリだけを消し、旧版には触れない。差し替えでは旧版を `<assets>/.previous-<subdir>-<id>` へ待避してから入れ替え、成功したら待避を消す。入れ替えに失敗したら旧版を戻し、戻せなければ待避先を残して場所を ugg.log に記録する。作業ディレクトリと待避先は `ghosts/` `shells/` の外に置く。v0.5.2 までの「既存を削除してから展開」はしない
- インストール後は **再起動を促す**（reload_assets は提供せず、再起動の動線を notify でゴーストが案内）

### 12.5 UI

- `getCurrentWebviewWindow().onDragDropEvent` の `drop` を受け、`payload.paths` から OS パスを取得（`src/dnd.ts`）
- 設定パネルにファイル選択 UI は置いていない（`dnd_install` の呼び出し元は `dnd.ts` の DnD 経路のみ）

---

## 13. 配布アーキテクチャ

### 13.1 インストーラ構成

- **NSIS**, `currentUser` モード, 日本語ロケール
- **同梱**: アプリ本体 + ghosts/default + shells/default
- **同梱しない**:
  - voicevox_core 資産（初回 DL）
  - Irodori-TTS（初回 DL、確認ダイアログ・GPU 必須）

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
       ├─ 確認ダイアログ（通信量・所要時間）
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
  ├─ install_panic_dialog_hook（起動時 panic を MessageBox で表示。log 初期化後は ugg.log にも残す）
  └─ tauri::Builder
       ├─ .plugin(tauri_plugin_autostart::init(...))
       ├─ .setup(|app| {
       │    ├─ AppState::initialize(app.handle())
       │    │    ├─ system::log::init（%APPDATA%\ugg\ugg.log）
       │    │    ├─ Db::open(companion.db) → db.migrate()（破損を検知しているときだけ失敗しても続行）
       │    │    ├─ Settings 読み込み（app_settings の "settings" キー）
       │    │    └─ ghost::load_bundle（Ghost/Shell/Dictionary 初期ロード。失敗はエラー文字列で保持）
       │    ├─ app.manage(state.clone())
       │    ├─ system::governance::load_backoff(&state)
       │    ├─ window::configure_main_window(app.handle())
       │    ├─ window::start_cursor_watcher(app, state.clone())
       │    ├─ system::manual::open_on_first_run(app, &state)
       │    ├─ presence::window_pos::dock(app, &state)
       │    ├─ presence::window_pos::spawn_dock_keeper(app, state.clone())
       │    ├─ tasks::spawn_random_talk(app, state.clone())
       │    ├─ tasks::spawn_idle_watcher(app, state.clone())
       │    ├─ tasks::spawn_irodori_idle_watcher(state.clone())
       │    ├─ tasks::spawn_irodori_health_watcher(app, state.clone())
       │    ├─ tasks::spawn_update_watcher(app, state.clone())
       │    ├─ tasks::spawn_topics_watcher(state.clone())
       │    ├─ tasks::spawn_reminder_watcher(app, state.clone())
       │    ├─ tasks::spawn_daily_watcher(app, state.clone())
       │    ├─ tasks::spawn_context_watcher(app, state.clone())
       │    ├─ tasks::spawn_calendar_watcher(app, state.clone())
       │    ├─ window::tray::install(app, state.clone())（失敗はログのみ）
       │    ├─ commands::tts::spawn_preinit(state.clone())（tts_enabled のとき）
       │    └─ tts::voice_ref::irodori_root() が取れれば
       │         ├─ tts::sidecar::install_sidecar_script(resource_dir, asset_root)
       │         └─ tts::sidecar::sweep_orphans（非同期。★v0.5.5）
       │  })
       └─ .invoke_handler(...)

二重起動ガード（single-instance）は無い（v0.5.6 でも入れない — 2026-09-19 ユーザー裁定。他のインスタンスの
生きたサイドカーを孤児と取り違えない対策（spec §6.0 項目 4）と、プロセスをまたぐ導入・更新と台帳の錠
（項目 3f・4）だけを入れる）
```

### 14.2 通常運用

- フロント `boot()` で `get_boot_payload` → 画像プリロード → イベント listen → `frontend_ready`
- `frontend_ready` で起動挨拶（first_boot or boot、`greeted` で再ロード時の二重発話防止）
- ユーザー操作 / 自発タスクの応答ループ
- 設定変更時は `apply_settings` で関連サブシステムへ通知

### 14.3 終了

```
[トレイ・右クリックメニューの「終了」]（★v0.5.6 commands::lifecycle::quit_with_farewell。トレイは直接、メニューは quit_app から）
  ├─ 隠している・最小化している（system::deliver::window_is_visible が偽）か、待っている間の 2 回目 → 発話せず下の後片付けへ
  ├─ daily_support_enabled で未完了の today ToDo があれば events.todo_quit、無ければ events.quit を発話
  ├─ 発話があれば (1600 + 60ms × 文字数).min(8000) + 500ms 待つ（最長 8.5s。発話が無ければ待たない）
  ├─ presence::window_pos::persist_now（位置の即時保存）
  ├─ state.tts.irodori.shutdown()（POST /shutdown → 1s で止まらなければ kill。未起動なら即 return）
  └─ app.exit(0)

終了シグナルを受ける処理（RunEvent::ExitRequested 等）は無い。強制終了（Alt+F4 を含む）ではあいさつは無く、サイドカーは
ugg の寿命に結びつけた Job Object で一緒に終わる（★v0.5.6 項目 4）。それより前の版が残したものは次の起動の sweep_orphans が止める
```

---

## 15. 既知のリスクと対策

| リスク | 影響 | 対策 |
|---|---|---|
| voicevox_core C API のバージョン差 | 起動時クラッシュ | バージョン固定（FFI と一致する版を初回 DL） |
| Irodori-TTS モデル DL の中断 | サイドカー起動失敗 | チェックサム検証、再 DL の動線 |
| 2 つの ugg が同時に Irodori を導入・更新する | 退避と復元を互いに壊す・入れ替え途中の site-packages でサイドカーが起動する | **★v0.5.6 項目 3f**: 導入・更新の錠をプロセスをまたぐもの（`update.lock`、`LockFileEx`）にした。プロセスが落ちたら OS が外す。合成の側も「試して放す」でもう 1 つの ugg の更新中を見る。**効かない条件**: MSIX の複製で `%APPDATA%` が別物に見えるプロセスとは錠が共有されない（Claude のツールから dev・更新を実行しない規律が前提） |
| 更新の途中で失敗する | それまで動いていた環境を壊す | **★v0.5.6 項目 3**: 1 つのトランザクションにした（版の控え・固定の段取り・1 回合成のゲート・全戻し）。**名前付きの配布を戻すには通信が要る**（控えの版を入れ直す）ので、戻せなかったものは控えを残して次の更新の前にもう一度戻す。モデルは戻さない |
| 子プロセス（pip・モデル取得・ダウンローダ）が固まる | 錠を握ったまま戻らず、**再起動まで Irodori が使えない** | **★v0.5.6 項目 2**: 出力・読み書き・CPU のどれも 5 分無ければ Job ごと止める（`child_process`）。出力だけで数えると、pip が torch を展開している間（黙って書き続け、CPU もほとんど使わない）に正常な処理を止めるので、Job Object の集計（孫を含む）で読み書きと CPU も見る。通信の側（Python 本体と get-pip.py の取得・ダウンローダの取得・更新の確認）には接続と読み取りの上限を付けた |
| GPU が利用可能 → 利用不能（運転中変化） | サイドカー異常終了 | notify(IrodoriUnavailable) + voicevox_core に自動切り替え |
| 辞書 v3 のパース失敗 | アプリ起動失敗 | バリデータで起動時に警告、デフォルト辞書にフォールバック |
| user_profile の肥大化 | system prompt 肥大化 | モード別容量管理（要約サイクル or 件数上限） |
| zip slip 等の DnD 経由のパス脱出 | 任意ファイル書き込み | zip エントリ名の検査（`sanitize_zip_path`）+ `normalize_path` 後の starts_with 検証 + manifest `id` の検証（`validate_asset_id`）（§12.3） |
| Python サイドカー起動時の文字エンコーディング | stderr の文字化け・読み取りの停止 | **★v0.5.6 原因を確かめて直した**（spec §6.0 項目 2）。ugg が起動したサイドカーの中で観測すると `stderr.encoding=cp932`・`isolated=1`・`utf8_mode=0` だった — CPython はパイプへ書くとき、UTF-8 モードでなければ ANSI コードページで書く。同梱の Python は `._pth` で isolated なので、環境変数（`PYTHONIOENCODING` / `PYTHONUTF8`）では変えられない。**送り側**: `sidecar.py` が依存の import より前に stdout / stderr を `reconfigure(encoding="utf-8")` で切り替える（`-X utf8` は `open()` の既定まで変えてモデル側のコードに影響しうるので使わない）。起動ごとに切り替え前と後の文字コードを `[stdio]` の 1 行で残す。**受け側**: 子プロセスの出力を読む 3 か所（サイドカーの stderr / `run_python` ＝ pip とモデル取得 / VOICEVOX のダウンローダ）は `reader::decode_output_line` で UTF-8 → Shift_JIS の順に読み、どちらでも読めない行は置換文字で流す（pip の出力は中身に手を入れられないので受け側で読む。行頭の BOM では判定しない）。読み取りが止まるときは理由を `ugg.log` に残す（★v0.5.5）。**★v0.5.6 項目 2 の残りで、VOICEVOX のダウンローダの経路だけ直っていなかったのを直した** — 色付けの制御文字を落とす処理が 1 バイトずつ文字に積み直しており、正しく読めた日本語をそのあとで壊していた（`アクセスが拒否されました` が読めない）。制御文字を落とすのは `child_process` に寄せ、UTF-8 の区間はそのまま残す。インストール版での効き目は実機検証で確かめる |
| サイドカーの孤児プロセス化 | リソースリーク（1 つで数 GB の VRAM） | アプリ終了時に `/shutdown` → kill。**強制終了で残ったものは次の起動で掃除する**（台帳 `sidecars.json` ＋ 応答の形で識別、★v0.5.5）。**★v0.5.6 で Job Object による親子連動が全部入った**: `run_python` と VOICEVOX のダウンローダと 1 回合成のゲートは実行ごとの Job（項目 2・3b。無進捗で止めるときに孫まで止め、読み書きと CPU の集計も Job から取る）、**サイドカーと zip の展開は ugg の寿命に結びつける Job**（項目 4。ハンドルは ugg が終わるまで閉じず、ugg が強制終了・Alt+F4・異常終了しても OS が閉じて中身が終わる）。どちらも「閉じたら中身ごと終わらせる」設定。**ugg 自身は入れない**（入れると、あとから起動する子が所属を引き継ぎ、取説を開いたメモ帳まで一緒に閉じる）。**効くのはこの版を入れた後から**（新しい版のインストーラが止めるのは、この版＝Job を持つ版）。当初ここに「実装済み」と書いていたが実装されておらず、2026-09-14 リリース前監査で記述を実態へ改めた経緯がある |
| 2 つの ugg が同時に動く（サイドカーの台帳） | 2 つ目の ugg の孤児掃除が、1 つ目が使っている最中のサイドカーを止める（1 つ目はヘルス監視で 20 分止まり、キャラが「使えません」と告知する）。台帳の書き込みが互いの記録を消す | **★v0.5.6 項目 4**: 台帳の記録に持ち主（ugg の pid と開始時刻）を持たせ、掃除は**持ち主のいないものだけ**を止める。台帳の錠をプロセスをまたぐもの（`sidecars.lock`）にし、書き込みは差し替えにした。**効かない条件**: v0.5.5 と同時に動かすと、v0.5.5 の記録には持ち主が無いので、この版の掃除はそれを孤児として止めうる（v0.5.5 側の掃除はそもそも持ち主を見ない）。持ち主のプロセスを開けないとき（別のユーザーのものなど）は終わっているとみなす。MSIX の複製で `%APPDATA%` が別物に見えるプロセスとは台帳も錠も共有されない |

---

## 16. 改訂履歴

| 日付 | 版 | 内容 |
|---|---|---|
| 2026-06-18 | v1 | Phase 2 対話で確定した全設計を反映、初版 |
| 2026-07-17 | v1.1 | M7（日常支援 Tier S: 共通基盤 + 統合リマインダー、daily-support-design v2 準拠）を反映。DB v6（reminders 拡張 + reminder_log、§2）/ `system/deliver.rs`・`system/governance.rs` 新設（§11.4）/ `commands/daily.rs`（§4.11）/ イベント `reminders-changed`・`system-toast` 受け皿（§5）/ 辞書 events `reminder_fired`・`reminder_snoozed` + プレースホルダ規約（§6.2、`reminder_fired` は system_messages から events へ移動 §6.5）/ Settings に daily_support・夜間静音・ガバナンス系 10 フィールド追加 / AppState に GovernanceState（§3）。付随して実装との既知乖離を一部解消（monologue_cache → topics_cache、migration v4-v6 追記、notify() の severity 未実装注記、WorkerHandles 不採用注記） |
| 2026-07-17 | v1.2 | M8（ToDo・日課管理 §4.6.2）を反映。DB v7 `todos`（§2）/ `tools/todo.rs`（検証 + 日課復活の境界計算）/ ToDo コマンド 6 種 + `todos-changed`（§4.11・§5）/ daily watcher（日課復活 + 朝の件数告知、§11.4）/ 辞書 events `todo_morning`・`todo_done`（発火）+ `todo_follow`・`todo_stale`（キーのみ、発火は M9）（§6.2）/ パネルに ToDo 節（3 バケットタブ） |
| 2026-07-17 | v1.3 | M9（状況発話 + 検知 + ガバナンス完成 §4.6.3）を反映。`presence/context.rs`（OS 検知 + 閾値純関数、windows crate に Power/SystemInformation feature 追加）/ context watcher（休憩・深夜・バッテリー・ToDo フォロー/滞留、§11.4）/ gate 段 5 連投回避 + 🔕 backoff（`feedback_speech` §4.11、`governance_backoff:*` 永続化）/ `DialogueResponse` に speech_id・category・priority・feedback_allowed（§5、バック起点のみ）/ フロント #balloon-mute + 設定「状況に応じた声かけ」セクション / 辞書 `situation_break`・`situation_late_night`・`situation_battery`・`todo_quit`（§6.2。`situation_todo_follow` キーは `todo_follow`/`todo_stale` に統合）/ 終了前確認（tray quit で todo_quit 優先、spec §4.6.2 後半）/ AppState に ContextState（§3） |
| 2026-07-18 | v1.4 | M10（カレンダー参照 §4.6.4、読み取り専用）を反映。DB v8 `calendar_cache`（複合キー + 実装追加列 unsupported、§2）/ `system/calendar.rs`（ICS 自前パース + RRULE near-term 展開 + TZ 簡易解決）/ カレンダーコマンド 4 種 + `calendar-changed`（§4.11・§5）/ calendar watcher（取得 + 開始前通知、§11.4）/ 辞書 `calendar_upcoming`（§6.2）/ Settings に calendar_sources（File\|Url）+ calendar_notify_min / フロント設定カレンダー UI + パネル今日明日表示。ファイル選択ダイアログは見送り（パス手入力、依存を増やさない）。**Tier S 4 機能そろい → v0.2 リリース候補**。 |
| 2026-07-24 | v1.5 | M11（天気基盤 §4.7.2）を反映。`system/weather.rs`（Open-Meteo forecast 取得・`app_settings["weather_cache"]` JSON キャッシュ = 新テーブルなし・schema v8 維持・WMO→日本語ラベル・降雨判定、§1.2/§2.2）/ 天気コマンド `search_location`・`get_weather`（§4.11、新規イベントなし §5）/ daily watcher に天気 3h 定期取得 + 降雨の一言（`weather_rain`/`weather_rain_outing`、§6.2・§11.4）/ SpeechCategory 9→12（`SituationRain` + M12 用 `RegularMorning`/`RegularEvening` を一括追加）+ `feedback_target()` で Regular* を間隔バックオフ非適用のまま 🔕 対象化、🔕 の is_situation ゲートを `deliver.rs` と `feedback_speech` の 2 箇所差し替え（§3.1）/ Settings 11 フィールド（weather_*4・situation_rain・regular_*6）+ 座標の小数 1 桁丸め clamp / 設定に天気節 + `#weather-credit` 出典表示（CC-BY 4.0）。**§4.7.1 定例会話（M12）は未実装**。 |
| 2026-07-24 | v1.6 | M12（朝・夜の定例会話 §4.7.1）を反映し **v0.3 実装完了**。`system/regular_talk.rs`（材料集約 + 定型文組み立て = low 完結 + advanced 言い回し整形、§1.2）/ daily watcher に朝・夜の定例会話を統合（tick §5.1 順・`regular_slot_due` 純関数・失効窓 6h・1 tick 1 枠・吸収 §5.5、§11.4）/ `regular_morning`/`regular_evening` 辞書（§6.2）/ `regular_{morning,evening}_date` dedup キー（§2.2）/ Db `count_done_todos_since`（夜の完了実績、schema v8 維持）/ 設定に定例会話節（朝/夜 有効・時刻・曜日トグル・夜間静音重なり警告）。SpeechCategory/Settings は M11 で追加済みのため無変更。新規コマンド・イベント・DB テーブルなし。既知の割り切り: advanced 整形は月次コスト上限の閾値チェックを経由しない（実コスト ≈ 月$0.004・受容）。 |
| 2026-08-10 | v1.7 | 掛け合いパターン3/4「3つ目の吹き出し」（spec §4.1.3 / §4.2.4）の未実装を解消。`banter::assemble_advanced` の無条件 3→1・4→2 フォールバックを廃止し、パターン抽選 (`pick_advanced_pattern`) を LLM 呼び出し前に前倒し、`advanced::system_prompt` がパターン別に出力形式を出し分け（§10.4）。`DialogueResponse.extra: Option<SpeechTurn>` 追加（§5）。フロントは `BalloonSlot`（"main"\|"sub"\|"extra"）を `SlotName`（"main"\|"sub"）と分離し `#balloon-extra` を静的配置、`balloon.ts` の `reposition` を吹き出し枠 + 基準キャラの一般化に書き換え、`repositionAll()`（main → sub → extra の固定順で全枠を再配置。**extra が常に退避する側**という一方向の規則で循環を避ける）を新設（§10.3・§10.4、配置は案A = 話者キャラの横・さらに外側へ退避）。新規コマンド・イベント・DB テーブルなし。**リリース前レビューの反映**: 安全縮退の条件に `sub` 欠落を追加（3ターン構成が成立しない応答で 3/4 を維持すると spec §4.2.4 違反の表示になる）／パターン4 の3ターン目の pose 語彙を sub の集合に是正（提示と検証の食い違い）／`chat_log` が最大 4 行になる旨を §2.2 に明記。 |
| 2026-08-10 | v1.8 | オンボーディングの聞き取り（spec §4.2.5）の未達を解消。`complete_onboarding` が M2 以降捨てていた `interests` / `topics_enabled` を結線: 興味 → `user_profile`（origin=onboarding）+ `interest_topics`（RSS キーワード、空白・重複を除き上限 20）、`topics_enabled` → `Settings.topics_enabled` を書き換えて `settings-changed` を emit（**時事ネタの明示同意**。spec §3.3/§4.4.6「既定オフ・オンボーディング同意必須」がチェックボックスの捨てられにより機能していなかった）。コマンド引数に `AppHandle` を追加（§4.9）。フロントは onboarding パネルに興味入力（カンマ区切り、設定パネルと同規則）を追加。 |
| 2026-08-10 | v1.9 | **M13 表示モニタ選択**（spec §4.1.6 / foundation-design §2）。`presence/window_pos.rs` にモニタ決定の単一関数 `resolve_target_monitor` を導入し、`dock` と 1 秒監視の両方をそこ経由に（**明示選択が常に優先、選択時は現在位置を見ない**）。`MonitorPref{name,x,y}` を `app_settings.monitor_pref` に永続化（§2.2）。同一性判定は name+position の一致のみ（`pref_matches` は純関数でテスト 6 件）。選択が解決できないときは主モニタへ退避しつつ選択は保持。`apply_dock` を `set_position` → `set_size` の順に変更（DPI 混在対策）。コマンド `list_monitors` / `set_monitor_pref` を追加（§4.10、新規イベント・DB 変更なし）。設定「基本」→「表示」にモニタ選択の select と、退避中を示す注記を追加。 |
| 2026-08-16 | v2.0 | **M14 advanced 独り言 + 時事ネタ織り込み**（spec §4.4.4 / §4.4.6 / foundation-design §3）。長らく未達だった「advanced では LLM 生成 + キャッシュ補充」を解消。**DB v9 `monologue_cache`**（§2.1・§2.2）+ Db メソッド 5（push/pop/count/clear/clear_with_topics）。**新規モジュール `system/monologue.rs`**（補充・プロンプト組み立て・応答パース、§1.2）。消費は `deliver::resolve_line` の Monologue 分岐で、**ghost ロックの外で pop → その後 pose 検証のためにロック**（DB I/O 中のロック保持を避ける）。補充は `spawn_random_talk` の tick の**発話判定の後**に回す（LLM 待ちで発話を遅らせない）。**二段失効**: 織り込み時 7 日（材料選別）+ 発話時 7 日（pop）、時事ネタ無しは 30 日。しきい値は 在庫 3 / バッチ 5 / 最短間隔 30 分 / 上限 20。**補充の前後で月額上限を評価**し、超過はチャット経路と同じ降格・告知（`dialogue::evaluate_cost_status` を `pub(crate)` 化して合流。これが無いと**チャットを使わず常駐するユーザーで上限が素通りする**）。会計は `api_usage` へ advanced 会話と同じ形で記録。`DialogueState.monologue_refill_ts`（§3.2）で最短間隔を管理。無効化契機 2 つ: 履歴クリア（全件、§4.5.5）/ 時事ネタ同意の撤回（該当行のみ）。**新規コマンド・イベント・Settings フィールドなし**。プロンプトに会話履歴・`user_profile` は渡さない（独り言は応答ではない）。LLM 経路の全失敗（low/降格/キー無し/API エラー/タイムアウト/応答破損/上限超過/在庫空/全件失効/ゴースト未読込/DB エラー）は辞書の `pick_monologue` に落ちる（spec §4.2.1 AI 非依存）。**レビューで是正した 5 点**（foundation-design §7.1 に記録）: ① 独り言 OFF（`monologue_interval_min == 0`）でも補充が課金していた → ゲート追加 ② ストックの鍵を `settings.ghost_id` から**読み込み済み `bundle.ghost.id`** へ（ゴースト切替は再起動が要るため、切替直後は settings 側が先行して人格とズレ、旧人格の文を新ゴースト行として積んでいた）③ **降格中は pop せず辞書へ**（spec §4.4.4 の明文）④ 織り込む見出しに**残り寿命 24 時間**を要求（期限ぎりぎりの材料が「積んでは即失効」の有料ループを作る）⑤ 材料を**現に有効な `interest_topics`** に絞る（外した興味の見出しが最大 7 日残って喋られる）。 |
| 2026-09-02 | v2.1 | **v0.5「宣言どおりに動く」**（spec §6.0）。契約表への反映のみで新規設計なし: コマンド `get_cost_status`（§4）/ `app_settings` の月次告知キー 2 つ`cost_warned_80_month`・`cost_limit_notified_month`（§2.2）/ `ghost.json` の `characters.*.persona` と `prompt.{max_chars_per_line,style_notes}`（§6.1）。あわせて配達の可視性判定（`deliver::window_is_visible`。最小化を `is_minimized` で先に見る）とログ基盤 `system/log.rs`（§1.2）を追記。**この行は v0.5.0 のタグ時に書き漏らしていたものを v0.5.1 で補記した**（ヘッダは v2.1 になっていたが履歴が無かった）。 |
| 2026-09-04 | v2.2 | **v0.5.1 — 棚卸しの残り 7 件**。① **問いかけパターンの記述矛盾を解消**: 本書は §6.2/§6.3 で「辞書の events キー `question_curiosity` + `probability: 0.05`」と書いていたが、spec §4.2.4 は「掛け合いパターンの 5 番目」と定めており実装が変わるレベルで食い違っていた。CLAUDE.md が spec を要件の正本としているため **spec に従い本書を訂正**（`banter.rs` の責務・events キー一覧・`probability` の説明）。② **`recall` の実装契約を §6.2 に追記**: `Dictionary::pick_recall` を `low::reply` が `pick_reply` より先に評価する。`source_keywords` は `ghost::dict::extract_keywords` が記憶本文から自動生成（形態素解析器なし・2 文字以上・最大 8 語）。v0.4.1 までは実装が 1 行も無く、契約が 4 箇所に揃っているのに一度も発火しなかった。③ `talk_poses`（口パクの開口フレーム自動検出）/ `get_db_health` / export schema `ugg-export-v2` / `default_shell` 追従を契約に反映。**新規イベント・DB テーブルなし。** |
| 2026-09-05 | v2.3 | **v0.5.1 のリリース前監査を受けた是正**。① `get_db_health` の契約に「保全は破損 1 件につき 1 回」「原本コピーは `-wal`/`-shm` も運ぶ」を追加（監査が「毎起動 2 本ずつ無限に増える」「WAL を含まない＝実機では本体より大きい」を検出）。② §6.2 の `recall` に「トリガー語はユーザー入力語のみ」を追加（アプリの定型文から作ると汎用語が low の応答を奪う）。③ `additionalBrowserArgs` は wry の既定引数を**置換**する旨を **§7.0（新設）** に記録。**契約表の追加・削除なし（挙動の明文化のみ）。** |
| 2026-09-05 | v2.4 | **v0.5.2**。`AppState::initialize` は破損検知時に `migrate()` の失敗を伝播させない（v0.5.1 は `db.migrate()?` がそのまま setup フックへ抜けて panic し、**破損 DB では起動できなかった**）。健全な DB での失敗は従来どおり致命。あわせて `VACUUM INTO` 失敗時の 0 バイト残骸を削除する（残すと `find_preserved` が「救出済み」と誤認して再試行しない）。**契約表の追加・削除なし。** |
| 2026-09-05 | v2.5 | **docs 整理（tidy-docs、v0.5.2 タグ後）**: 本改訂履歴の並びが版の昇順になっていなかったので整列した（v1.3/v1.4、v1.5/v1.6、v2.3/v2.4 が入れ替わり、v1.8/v1.9 が末尾に取り残されていた）。**設計本文・契約表の変更はなし。** |
| 2026-09-08 | v2.6 | **v0.5.3 項目 1（復旧導線）**。① `export_data` を `build_export_payload` + `rescue()` に分け、**部分救出**へ（`State` と保存先に依存しない形にして、壊れたテーブルを含む DB で挙動を固定できるようにした）。schema `ugg-export-v3`、`failed_tables` を追加。② `Db::open` の順序を「整合性検査 → pragma」へ入れ替え、pragma 失敗は健全時のみ致命。③ `find_preserved` に `require_healthy` を追加し `is_usable_preserved` で妥当性を確認。**新規コマンド・イベント・DB テーブルなし。** |
| 2026-09-10 | v2.7 | **v0.5.3 項目 2（更新でデータを失わない）**。① `install_one` を「`assets/.staging/` へ展開 → `verify_staged` で id を再確認 → `swap_in` で差し替え」に。**失敗しても旧版は無傷**。旧版は `assets/.previous-<種別>-<id>` へ待避してから入れ替え、入れ替え失敗時は戻す。**戻せなければ待避先を消さずログに残す**（作業ディレクトリの掃除で巻き添えにしないよう、待避先は staging の外に置く）。② zip 内 manifest の選択を `pick_manifest_entry` に一本化し、`read_manifest_bytes`（確認側）と `find_strip_prefix`（展開側）の両方をそこへ寄せた。規則は「最も浅いもの。同じ深さならエントリ順で先のもの」。`manifest_name` も 1 箇所へ。③ `DndError::IdMismatch` を追加。④ `ps_single_quoted` で PowerShell 単引用符を escape。**新規コマンド・イベント・DB テーブルなし。** |
| 2026-09-10 | v2.8 | **v0.5.3 項目 3（保存したあとの再保存で巻き戻らない）**。① フロントの `ensureAssetSelection` にゴースト・シェル select の値合わせを一本化し、`fillAssetSelect`（一覧を埋める）と `applySettingsToForm`（保存済みの値をフォームへ戻す）の両方から呼ぶ。**値合わせが 2 か所に分かれていたのが、追従が取り消される原因だった。** ② `Db::clear_calendar` を廃し、`Db::save_settings_and_clear_calendar(key, value)` に置き換え。設定 JSON の保存とキャッシュ全消去を 1 トランザクションで行う。**2 つを分けて呼べる限り同じ穴が空くので、単体の `clear_calendar` は残さない。** **新規コマンド・イベント・DB テーブルなし。** |
| 2026-09-10 | v2.9 | **v0.5.3 項目 5（課金の保護を異常時にも効かせる）**。`dialogue::cost_exceeded(-> bool)` を `cost_gate(-> CostGate{Allow,Exceeded,Unknown})` に置換。集計失敗は `Unknown` で**止める**（従来は `false` で通していた）。判定本体は `decide_cost_gate(limit, check)` に分けて `AppState` 抜きでテストできるようにした。告知に `NoticeKind::CostUnknown`（辞書キー `cost_unknown`）を追加し、既定辞書へ 2 パターン追加。告知済みは `DialogueState::cost_unknown_notified`（**プロセス内 AtomicBool**。記録先の DB 自体が疑わしいので月次タグを使わない）。call site 3 経路すべて更新。**新規コマンド・イベント・DB テーブルなし。** |
| 2026-09-10 | v2.10 | **v0.5.3 項目 4（通知を後続の発話で消さない）**。`system/ghost-speech.ts` に通知キューを追加。`renderResponse` は通知なら `noticeQueue` へ積んで即 return し、`pumpNotices` がステージの空きを見て 1 件ずつ `renderNow` する。割り込みは `takeStage()` に一本化し、**打ち切る相手が通知ならキューの先頭へ積み直す**。`renderNow` の後片付けは `currentToken === token`（= ステージの所有者）のときだけ行う — 割り込まれた側が await から戻って解放すると、次の描画中に「空き」と誤認される。判定は `isNotice`（`kind === "system_message"` または `priority === "notice"`）。**契約変更なし**（既存フィールドのみ参照）。 |
| 2026-09-10 | v2.11 | **v0.5.3 項目 6（問いかけを会話として閉じる）**。`advanced::load_recent_history` を実装（v0.5.2 まで `Ok(Vec::new())` 固定）。`list_recent_chat_log` を時系列へ戻し、連続する同種の行を 1 ブロックへまとめ、`MAX_HISTORY_PAIRS = 8` 往復・`MAX_HISTORY_CHARS = 1200` 文字で古い方から落とす。切り出しの起点は「最初に残すユーザー発言の 1 つ前」= 問いかけ。キャラ発話は `<名前>: <台詞>` 形式で assistant に入れる。`ChatMessage::assistant` の `#[allow(dead_code)]` を解除。**呼び出しは `build_messages`（チャット経路）のみ**で、`system::monologue` は自前のプロンプトを組む。**契約変更なし。** |
| 2026-09-10 | v2.12 | **v0.5.3 項目 9（操作列テストの常設）**。フロントに Vitest + happy-dom を導入（`vitest.config.ts` / `npm test` / `src/__tests__/`）。DOM は `index.html` の body を読み込んで作る（手書きダミーだと id のずれに気づけないため）。操作列 4 本の内訳と書き方は docs/test-plan.md §3.2b が正本。**プロダクションコードの構成変更なし。** |
| 2026-09-10 | v2.13 | **v0.5.3 のリリース前監査を受けた是正**。① `Db::list_recent_chat_log_in_mode(mode, limit)` を追加し、`load_recent_history` はこれで **mode="advanced" の行だけ**を読む（バック起点の発話は例外なく mode="low" で記録されるので mode で切り分けられる。取ってから絞ると独り言だけが続いた夜に会話行が LIMIT の窓から押し出されるため **SQL 側で絞る**）。往復が 1 つも無い窓では空を返す。② `monologue::parse_monologue_batch` のエラーから応答本文を外す（項目 7 で `advanced` 側だけを直しており漏れていた）。③ `llm::truncate_for_log` を追加し、HTTP エラーボディを 300 文字で頭打ちにする。**Tauri コマンド・イベント・DB スキーマの変更なし。** |
| 2026-09-11 | v2.14 | **未計上だった契約⇔実装の乖離を 1 件記載**（v0.5.4 のスコープ検討中に発見）。`synthesize_voice` の `caption` は「Irodori 実モデルのみ使用」と書いてあるが、**v3 本体の checkpoint が `use_caption_condition: false` のため一度も効いていない**（`sidecar.py` は渡しており `cfg_scale_caption=3.0` も設定しているが、条件付けに入らず捨てられる）。台本の行ごとの声質指示は Irodori 経路では無効。**v0.5.5 の v4.1-Small 差し替えで閉じる**（`use_caption_condition: true`）。設計・契約の変更はなし。 |
| 2026-09-11 | v2.15 | **v0.5.4 項目 1・2（入っている版を記録する / 「使える」と「最新」を分ける）**。① 導入記録 `%APPDATA%\ugg\irodori\installed.json` を追加（`pins` = 要求した固定 URL、`resolved` = 実際に入った版。**全段成功後にだけ書く**）。② コマンド `get_irodori_status` を追加（`IrodoriStatus`）。**`irodori_assets_ready` の意味は変えない** — 「最新か」を混ぜると、古いが動いている環境でフロントが `tts_irodori_use_real_model` を黙って倒して永続化するため。**記録が無い環境は `up_to_date: false`**（v0.5.4 より前の導入＝この機能の対象そのもの）。**設定フィールド・イベント・DB スキーマの変更なし。** |
| 2026-09-11 | v2.16 | **v0.5.4 項目 3・4（更新の実行経路 / ユーザーに見える形にする）**。① コマンド `update_irodori_runtime` を追加。**退避 → 入れ直し → import 確認 → 成功なら退避を捨てる**（v0.5.3 項目 2 と同じ規律）。退避先は site-packages の**外**（`%APPDATA%\ugg\irodori\.update-backup`）— 中に置くと import されうるうえ pip が dist-info を拾う。**`python` pin は対象外**（`ensure_python_embeddable` が skip するため既存環境に届かないが、稼働中のインタプリタは差し替えられない。全体の入れ直しが要る旨を返す）。② 設定の Irodori セクションに「**導入済み (更新あり)**」表示と「更新する」ボタン。**`assets_ready` は従来どおり「使えるか」だけ**で、更新の有無は別表示。**設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.17 | **v0.5.4 の更新経路のバグ修正（実機検証の直前に発見）**。`get_irodori_status` は記録が無いとき `outdated` を空で返し、`update_irodori_runtime` 側が `current_pins()` から対象を組み立てていた。そこに**入れ直せない `python` が混ざる**ため、記録が無い環境＝**v0.5.4 が対象にしている環境がちょうど 1 つも更新できなかった**。① `outdated` の算出を `status` に一本化し、記録が無くても名指しする。② **`python` の新旧判定を記録から実物（`python.exe --version`）へ移す**（`ensure_python_embeddable` が skip する以上、記録の欠落と版の相違は別物）。③ 版を聞けなかったときは「古い」と言わない。④ 更新成功後、実物が pin と一致していることを確認できたら `python` も記録する（毎回聞き直さないため）。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.18 | **実機検証（test-plan E-7）1 回目の結果を反映**。`update_irodori_runtime` の成否判定を「3 モジュールが全部 import できること」から「**入れ直し前後で悪化したものが無いこと**」へ改めた。実機の `silentcipher` は `pydub` 不在で一度も import できておらず（upstream が任意依存として `ImportError` を握る）、絶対条件では**既存環境が必ずロールバックして更新が一度も成功しない**。あわせて import 失敗時の**理由（例外の最終行）を捨てない**ようにした（握り潰していたため、実機の失敗が `python 異常終了 (code Some(1))` としか分からなかった）。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.19 | **記録の書き込みを `update_irodori_runtime` の中へ移した**。`installed.json` の更新がコマンド層（`commands::tts`）にあったため**テストから到達できず**、「入れ直したのに `up_to_date` が false のまま」を自動で検出できなかった。記録は更新の一部なので下層へ移し、実機検証（E-7）が `installed.json` と `up_to_date` まで確かめられるようにした。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.20 | **リリース前監査の是正 6 件**。① `IrodoriBusyGuard`（プロセス内 `AtomicBool`）で **`download_irodori_assets` と `update_irodori_runtime` を排他**に。記録が無い環境では両方のボタンが押せ、10〜20 分の初回 DL の途中に更新を差し込めた。② **残っている退避を無条件に消さない** — 復元に失敗して残した退避には唯一の旧版が入っている。先に戻し、戻せなければ場所を伝えて止まる。③ `move_package_aside` が途中で失敗したときも復元する（ディレクトリは動いたが dist-info で失敗する経路）。④ 記録の書き込みを `record_after_install` に一本化し、**初回 DL 経路でも `python` は実物が pin と一致したときだけ記録する**（`ensure_python_embeddable` が skip するため「入れた」と「入っている」は一致しない。更新経路だけ直して隣に残していた）。⑤ `update_irodori_runtime` が実行前に `irodori.shutdown()` を呼ぶ（`State<Arc<AppState>>` を受け取る形へ。**JS から見た引数は変わらない**）。⑥ 失敗時の UI 文言から「元の状態のままです」を外す（複数を順に入れ直すので、先行分は戻していない）。**設定フィールド・イベント・DB スキーマの変更なし。** |
| 2026-09-11 | v2.21 | **v0.5.4 インストール版の実機確認で見つけた 2 件**。① **確認ダイアログの文字が読めなかった**。`.panel` は `background: white` と `color-scheme: light` を指定していたが **`color` を指定しておらず**、`:root` の `color-scheme: light dark` がダーク側に解決した文字色がそのまま継承されて**白背景に白文字**になる。個別指定のある要素（`.panel-hint` など）は無事で、素の要素（確認ダイアログの見出しと本文）だけが消えていた。**「更新しますか?」を読めないまま OK を押す形**になっていたので `.panel` に `color` を明示した。同型を v0.4 でボタンに対して踏んでいる（`.panel-footer button` のコメント）。② **子プロセスのコンソール窓が前面に出ていた**。リリース版は `windows_subsystem = "windows"` でコンソールを持たないため、pip / Expand-Archive / サイドカーが自前で窓を割り当てる。`CREATE_NO_WINDOW` を 4 か所に付けた（`irodori_download` の python と powershell、`sidecar` の python、`download` の voicevox downloader）。**取説を開く `notepad.exe` は窓が出るのが目的なので対象外。** |
| 2026-09-11 | v2.22 | **v0.5.5 項目 1（落ちた理由が残る・2 経路とも）**。① サイドカー stderr の**進捗以外を捨てるのをやめ**、`ugg.log` へ 300 文字で残す（`[irodori:py]`）。`--log-level warning` で起動しており、**リクエスト時の例外は `HTTPException` として応答に載り stderr には来ない**ので、ここが運ぶのは起動時の import 失敗とモデル DL の失敗＝平時は静か。② `notify` が**理由をログに残す**。辞書キーが存在すると `fallback_text()` が使われず `reason` がどこにも残らなかった（「Irodori-TTS が利用できません」とだけ出て原因不明）。③ `sanitize_sidecar_error` を追加 — 500 body は `f"Irodori 合成失敗: {exc}"` で**発話テキストを含みうる**ため、送った本文とキャプションを伏せてから `truncate_for_log` に通す（spec §3.3 / v0.5.3 項目 7）。**4 文字未満は伏せない**（無関係な語まで潰れて診断にならない）。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.23 | **v0.5.5 項目 2（孤児サイドカーの掃除）**。① **台帳 `%APPDATA%\ugg\irodori\sidecars.json` を追加**（起動で追記、正常終了で削除）。`ready.json` は起動のたびに上書き削除される**単一スロット**で、孤児が 2 つ以上できると古い方が到達不能になるため（実機で 2 つ同時に走った実績）。② `sweep_orphans` を**起動時・自分のサイドカーを 1 つも立てる前**に走らせる。③ **識別は応答の形で行う** — `/health` は GPU 不在で **503** を返すので成否では判定できず、一方でポートだけを頼りに `/shutdown` を投げると無関係なサービスを撃つ。`status`（文字列）・`mock`（真偽）・`gpu` の 3 キーが揃うことを確認してから止める。**pid は撃たない**（再利用の事故）。④ 掃除の最中に立った自分のサイドカーを止めないよう、**先に台帳を空にしてから、対象ごとに現在の台帳を見て除外する**。⑤ v0.5.5 より前の環境には台帳が無いので、**旧 `ready.json` も 1 度だけ候補に含める**。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.24 | **v0.5.5 項目 4（更新中の発話を止める）**。`IrodoriBusyGuard` を見ているのはコマンド 2 本だけで、`ensure_sidecar_running` は見ていなかった。更新の最中に発話が来ると**半分入れ替わった `site-packages` で新しいサイドカーが起動する**（数十秒だった v0.5.4 では踏みにくいが、数 GB・十数分になるモデル更新では現実的に踏む）。`irodori_download::is_busy()` を追加し、**起動経路だけ**弾く（すでに動いているものは止めない＝走っている発話を切らない）。弾かれると `decide_fallback` が voicevox へ流す。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.25 | **v0.5.5 項目 3 の前半（名前付き pip 要件を更新の対象にする）**。① `InstalledStamp` に `requirements`（配布名 → 要件文字列、`serde(default)`）を追加し、`outdated_list` が **`current_requirements()` との差も返す**。v0.5.4 の判定は `current_pins()`（固定 URL 4 本）としか突き合わせておらず、`transformers<5` を変えても `outdated` は空＝**更新ボタンすら出なかった**。判定は**要件文字列そのものの比較**で行う（版比較はしない。見たいのは「このビルドの要求が変わったか」）。② 入れ直しは `pip install --upgrade <spec>`。固定 URL の 3 本と違い**依存を解決させる必要がある**ので `--no-deps` は付けない。③ **torch 系は `--index-url` で CUDA 12.8 の index から入れる** — 名前だけで入れ直すと **PyPI の CPU 版**が入り GPU 合成が黙って壊れる（`install_torch_cuda` と同じ index）。④ `merged_requirements` で**入れ直せた分だけ**記録する。**守れる範囲の限界**: 退避して戻せるのは「その名前のパッケージ」だけで、**依存の連鎖までは元に戻せない**。「1 回合成できる」までの検証は v0.5.6（major 移行）で入れる。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.26 | **v0.5.5 項目 3 の後半（モデルを更新の対象にする）**。① **モデルの正本を Rust へ移した**（`MODEL_PINS` / `current_models` / `model_args`）。`sidecar.py` は `install_sidecar_script` が**毎起動で無条件に上書きコピー**するのに重みは初回 DL でしか取らないため、ID をあちらに置いたまま変えると「コードだけ新しくなって重みが無い」＝**無言で VOICEVOX に落ちる**。`sidecar.py` は `--model-*` で受け取り、定数は**渡されなかったとき用の保険**に降格。② `InstalledStamp.models`（名前 → `repo@revision`）を追加し `outdated_list` が差を返す。③ **モデルの置き場所に revision を含める**（`model_dir_name`）— 含めないと revision を上げたとき同じパスへ上書きになり戻れない。`main` のときは従来どおりの名前なので**既存環境のパスは変わらない**。④ `download_models` の**自前の「存在してサイズ > 0」判定を捨て**、`hf_hub_download` / `snapshot_download` の etag 照合に委ねた（途中で切れた DL をサイズでは区別できない。既存環境では再取得も起きない）。⑤ 取得側（`--download-only`）と起動側の両方へ `model_args()` を渡す（**片方だけだと取得先と読む先が食い違う**）。⑥ 跨ぎの噛み合わせを `the_sidecar_defaults_match_the_rust_pins` で見張る。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-11 | v2.27 | **v0.5.5: 記録に欄が無いものを「古い」と扱わない**（実機検証の直前に発見）。項目 3 をそのまま実装すると、**v0.5.4 から上げただけのユーザーの記録には `requirements` / `models` の欄が無い**ため全部が更新対象になり、**torch を含む数 GB の再取得**が走る。固定 URL の 3 本は「記録が無い＝ pin 前の `refs/heads/main` が入っている」と実機で確認できているので対象にしてよいが、**要件とモデルは欄が無いだけで中身が古い証拠にならない**。v0.5.4 の python 判定と同じ原則 — **代償が非対称なら、証拠が無い側へ倒す**。① `outdated_recorded_only` を分け、記録にある名前だけを突き合わせる。② `status()` が `backfill_baseline` で**基準値を書き足して永続化**する（これが無いと欄が空のままで**次に要件を変えても永久に届かない** ＝ v0.5.5 が直した穴の再発）。書いてよい根拠は**v0.5.5 が要件もモデルも変えていない**こと。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-13 | v2.28 | **v0.5.5 実機検証（E-8）で発覚した 2 件**。① **古い `ready.json` を信じて死んだポートへ投げていた** — `try_read_port` が pid を見ずに port だけを返しており、起動前の `remove_file` が効かない状況（ファイルロック・ウイルス対策・仮想化されたプロファイル）で、新しい子が書くより先に**前回の子の記録**を読んでいた。実機では前回セッションで止めた孤児のポート 60005 へ起動時の挨拶を投げて失敗した。`wait_for_ready_file` に `expected_pid` を足し、`ready_belongs_to`（pid 一致。取得できず 0 のときは不一致）を満たす記録だけ受け入れる。孤児掃除側の `try_read_port` は古い記録を拾うのが目的なので絞らない。② **合成失敗の理由が 2 回目以降ログから消えていた** — 項目 1 で足した理由のログは `notify()` の中にあり、通知は 5 分に 1 回へ絞られている（`should_notify_unavailable`）。実機では起動時の失敗が 1 回通知した後、更新中の「進行中です」がログから完全に消えた。`commands::tts` の合成フォールバックと、開発方針 7 の掃討で見つけた `tasks` のヘルス監視の 2 か所で、理由を**レート制限より前に** `ulog!` する。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-13 | v2.29 | **v0.5.5 実機検証（E-8）で発覚: 孤児掃除が、確かめる前に記録を消していた**。v2.23 ④ の「先に台帳を空にしてから、対象ごとに現在の台帳を見て除外する」では、**掃除の途中でアプリが落ちると記録が 1 件も確かめられないまま全部消える**（実機では起動直後に dev が落ち、ダミーを含む 2 件が着信 0 件のまま消えた）。本物の孤児なら GPU を掴んだまま二度と止められない。① `sweep_orphans` は**確かめ終えた記録から 1 件ずつ**消す。自分のものなのに `/shutdown` が届かなかった記録は**残して次の起動で再試行**する。② 掃除中に立った自分の子の除外を「台帳に同じポートがあるか」から「**同じポートを別の pid が持っているか**」（`taken_by_a_new_child`）へ改めた — 空にしない以上、候補自身の記録と区別する必要がある。③ `ledger_remove` は**ポートと pid の組**で消す（同じポートを後から取った新しい子の記録を消さない）。**開発方針 7 の掃討で同じ形を 2 つ**: ④ `shutdown_sidecar` も冒頭で記録を消していた → **止まったのを見届けてから**消す（kill の失敗や止める途中の強制終了で、生きているサイドカーの記録だけが消えていた）。⑤ 台帳の読み→書き戻しに排他が無かった（起動直後の掃除と、挨拶によるサイドカー起動が重なりうる）→ `LEDGER_LOCK` で直列化。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-14 | v2.30 | **v0.5.5 リリース前監査（release-audit）の指摘を反映**。① **欄が空の記録の基準値を固定値にした** — `backfill_baseline` がいまのビルドの値を書いており、要件を変えたビルドで差が出ず更新が届かなかった（v0.5.5 単体では両者が一致して鳴らない）。欄が空の記録は v0.5.4 の基準値（`V054_BASELINE_*`）で読み（`outdated_section`）、欄があって名前が無いものは後から増えた要件として対象にする。初回導入は要件とモデルも全部記録する。② **モデルを、取得した revision の置き場所から読む** — 取得側だけ revision を見ており、読み込み側は常に `main` を読んでいた（`_build_runtime` / `_codec_location`。`main` のときの挙動は変えない）。③ **孤児掃除が「応答が遅い」を「死んでいる」と同じに扱っていた** — 合成中はサイドカーのイベントループが塞がり `/health` に答えない。接続を拒否されたら捨て、つながったのに答えないなら残す（`Probe`）。確かめる時間は 5 秒（Windows は閉じたポートの拒否に約 2 秒かかり、以前の 800ms では死んだ記録もタイムアウトで判定されていた）。④ **起動の途中で更新が始まると、起動したサイドカーが居座る** — 保存と同じ錠の中で busy を見直す（`adopt_sidecar`）。⑤ 伏字が JSON でエスケープされた発話に効かなかった。⑥ busy を奪い合うテスト 2 本を直列化。⑦ 記述の追随（`installed.json` の欄、`sidecars.json` の行、`get_irodori_status`、リスク表の Job Object）。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-14 | v2.31 | **v0.5.5 インストール版の実環境で、孤児掃除が死んだ記録を「応答しない」と取り違えていた**（v2.30 ③の修正が実環境で成立していなかった）。何も待ち受けていないポート（旧 `ready.json` の記録）を、起動のたびに「応答しません（記録を残します）」と判定していた（dev では 2 秒の拒否で正しく捨てていた）。原因: reqwest の `PendingRequest::poll` は**全体のタイムアウトを先に見てから**通信の結果を見る。起動直後の混雑で「接続拒否の知らせ」と「5 秒のタイマー」が同時に処理待ちになると、拒否されていてもタイムアウトと判定される。**対処: HTTP の前に、TCP の接続だけを待機スレッド（`spawn_blocking` ＋ `connect_timeout`、10 秒）で確かめる**（`tcp_reach`）。拒否なら死んでいる、つながったら従来どおり HTTP で応答の形を見る、時間内に結果が出なければ決めつけずに残す。待機スレッドでの接続の結果はタイマーと先着を争わない。あわせて、掃除の開始と 1 件ごとの所要時間をログに出す（今回の起動から判定まで 22 秒の原因を、ログから切り分けられなかったため）。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-14 | v2.32 | **v0.5.5 タグ後の docs 整理（tidy-docs）**。§7.1 全体フローの `caption` の注記が「v0.5.5 の v4.1-Small 差し替えで初めて効く」のままだった（v0.5.4 のスコープ確定時の記述で、v0.5.5 のスコープ確定で差し替えを v0.5.6 へ再分割したときに追随していなかった）。v0.5.6 へ訂正。あわせて改訂履歴の v2.29 の行を版の順へ並べ直した（中身は変えていない）。**契約・設計の変更なし。** |
| 2026-09-17 | v2.33 | **v0.5.6 前の docs 整理（外部レビューの検証）**。実装と突き合わせて、Phase 2 の設計のまま残っていた記述を是正した。① 構造体のコードブロックを実装へ（`DialogueState` の旧 `cost_limited_emitted` を外して `cost_unknown_notified` を追加、`AppState` / `PresenceState` / `TtsState` / `PomodoroState` / `WindowState` / `GhostBundle` / `SidecarHandle`。実装に無い `WorkerHandles` を削除）② 構成図（§1.1〜§1.4）を実ファイルへ（実在しない `pose.ts` / `drag.ts` / `panels/settings/` の分割 / `asset_dnd.rs` / `dialogue/monologue.rs` / `tts/ (engine)` / `trait TtsEngine` / `create_main_window` を訂正し、抜けていたファイルを追加）③ ファイル資産表と §8.1 の図（ログは `%APPDATA%\ugg\ugg.log`、参照音声は `refs\<slot>_<id>.wav`、Python は 3.11.9、`installed.json` / `sidecars.json` / `ready.json`、site-packages の位置）④ §4.11 `feedback_speech` の対象に定例会話、`caption` が v3 本体で効かない注記、メニュー項目名「予定・ToDo」 ⑤ §8.3 / §8.6 / §8.7 / §13（GPU 不在は稼働中のヘルス監視が扱う、`voice_caption_default` とキャプション入力モーダルは無い、Irodori に規約同意は無く確認ダイアログだけ）⑥ §12 DnD 導入（`canonicalize` を使わない zip slip 検査、定数の上限、許可外拡張子の拒否、ファイル名検証の範囲、v0.5.3 の非破壊導入、ファイル選択 UI は無い）⑦ §3.3 / §14 の起動と終了の流れ（存在しないプラグイン・関数名を実名へ、終了経路が 2 本あること）⑧ §15 の「UTF-8 強制」は未実装 ⑨ §7.1〜§7.5 と §8.4 の疑似コードに、Phase 2 の素案で実装と違う点を注記。**契約・設計判断の変更はなし。** |
| 2026-09-19 | v2.34 | **v0.5.6 スコープ確定（spec v1.11）に伴う参照先の訂正と注記**。モデルの差し替えが v0.5.7 へ分割されたので、caption の時期の記述 2 か所（契約表の `synthesize_voice`・§7.1 全体フロー）を v0.5.7 へ直した（§7.1 は、MF の `use_caption_condition` が未確認なので「効くかを確かめる」にとどめた）。§14 の二重起動ガードと §15 のリスク表の Job Object を、裁定の結果（完全な single-instance は入れず、台帳の所有者とプロセスをまたぐ錠だけ入れる／Job Object は v0.5.6 で入れる）へ直した。**スコープの検証で分かった事実を 2 か所に注記した**: 契約表の `update_irodori_runtime` の「失敗したら戻す」は途中の失敗では成り立っていない（それより前に成功した分の退避も消す。名前付き要件は戻せない）／§11 の severity 二段トーストは取り下げ。**契約・設計の変更なし**（設計の変更は各項目の実装時に行う）。 |
| 2026-09-19 | v2.35 | **v0.5.6 項目 1（速くする）と項目 2 の文字コードの実装に伴う改訂**。§2.4 の資産表と §8.1 の構成図に、参照音声の事前変換の結果（`refs\<slot>_<id>.<合成モデル>+<コーデック>.<精度>.<前処理>.latent.pt`）を足し、作る側（サイドカー）・消す側（`voice_ref::delete_file`）と、名前の形を 2 つの言語で揃える約束を書いた。§8.7 の参照音声の削除に、変換結果も一緒に消えることを足した。§15 のリスク表の文字コードの行を、2026-09-19 の観測で確定した原因（パイプへは ANSI コードページで書く・isolated のため環境変数は効かない）と、送り側（`reconfigure`）・受け側（UTF-8 → Shift_JIS）の対策へ書き換えた（「UTF-8 の強制は実装していない」「理由はまだわかっていない」は事実でなくなった）。§1 のモジュール表の reader.rs に、子プロセスの出力の読み方を共有するようになったことを足した。**契約（コマンド・イベント・設定・DB）の変更なし。** |
| 2026-09-20 | v2.36 | **v0.5.6 項目 2 の残り（進捗を行ごとに・無進捗の中断・stderr の伏字）の実装に伴う改訂**。① §1 のモジュール表に `tts/child_process.rs` を追加（子プロセスの起動・行ごとの読み取り・無進捗の中断・Job Object）。`reader.rs` の行は「行に組み立てるのは呼ぶ側」に改めた。② §8.3 に取得の進捗の出し方を追記（**パイプ越しでは pip も huggingface_hub も進捗を出さない**ので、pip は `--progress-bar raw`、hub はモデル取得の間だけ判定を差し替える。失敗したら理由の行をエラーに添え、直前の 20 行を `ugg.log` に残す）。③ §15 のリスク表: 文字コードの行を実装に合わせ、**VOICEVOX のダウンローダの経路は v2.35 の時点では直っていなかった**（色付けの制御文字を落とす処理が 1 バイトずつ文字に積み直しており日本語が化けた）ことを明記。Job Object の行を「6 か所のうち 2 か所は v0.5.6 項目 2 で入れた（残りはサイドカーと zip の展開 ＝ 項目 4）」に改めた。子プロセスが固まる行を追加。**契約・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-23 | v2.37 | **v0.5.6 項目 3（更新を 1 つのトランザクションにする）の実装に伴う改訂**。§2.4 の資産表: `installed.json` がモデルの**読み先の正本**になったこと（書き込みは tmp → rename）、`update-versions.json`（版の控え）・`update.lock`（プロセスをまたぐ錠）・`.update-backup\` の退避の印・`.update-gate\`（ゲートの作業場所）を追加。契約表: `update_irodori_runtime` の「★2026-09-19: 途中の失敗では戻せていない」の注記を、実装した形（入口の備え → 前回の後始末 → 控え → 固定の段取り → 1 回合成のゲート → 全戻し）へ書き換え、`download_irodori_assets` も同じ入口を通ることを書いた（**初回導入は自分のサイドカーを止めていなかった**）。§8.1 の構成図、§8.3（読み先と取得先・一発合成のモード・`local_dir_use_symlinks` を渡さないこと）、§15 のリスク表（2 つの ugg の同時更新・更新の途中失敗）。**コマンド・イベント・設定フィールド・DB スキーマの変更なし**（`sidecar.py` の起動引数 `--synth-once` / `--voice-ref` を足した。子プロセスとして使うだけで、HTTP の契約は変えていない）。 |
| 2026-09-24 | v2.38 | **v0.5.6 項目 4（孤児と二重起動）の実装に伴う改訂**。§1 の `child_process.rs` の行（ugg の寿命に結びつける Job・プロセスの開始時刻）。§2.4 の資産表: `sidecars.json` に持ち主の欄（`owner: {pid, started}`。1 件ずつ読む・差し替えで書く・v0.5.5 も読める）、`sidecars.lock`（台帳の錠。足すときは錠が取れなくても書き、消すときは見送る）を追加。§8.1 の構成図、§8.4 の `SidecarHandle`（`mock`）。契約表: 導入・更新の入口で**持ち主のいない孤児を止めてから**生きているものを数える（項目 3e で「所有者を見分けられるのは項目 4 から」と止めずにいたもの）。§15 のリスク表: 孤児の行を「Job Object が全部入った」へ、2 つの ugg の台帳の行を追加。**コマンド・イベント・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-24 | v2.39 | **v0.5.6 項目 5（終了のあいさつを揃える）の実装に伴う改訂**。§14.3 の終了経路を 1 本（`commands::lifecycle::quit_with_farewell`）へ: トレイと右クリックメニュー（`quit_app`）の両方が通り、見えていれば終了前の確認かあいさつをしてから、隠している・最小化しているとき（`deliver::window_is_visible`）と待っている間の 2 回目はすぐ終了する。起動・終了の概観、契約表の `quit_app`（引数と戻り値は変えない）、終了前確認の節も揃えた。**コマンド・イベント・設定フィールド・DB スキーマの変更なし。** |
| 2026-09-24 | v2.40 | **v0.5.6 項目 6（告知は「届いた」と確かめてから済みにする）の実装に伴う改訂**。§11.1〜§11.3 を実装どおりに書き直した: `notify()` は見えていなければ出さずに `Held` を返し、出したら `Shown`（`NoticeOutcome`）。1 回だけ出す告知 4 か所は `once_reached`（届いたときだけ済みにする）を通す。§11.2 の表を「severity 既定」から「辞書キーと済みの記録」へ（severity の素案と二段トーストは実装されないまま取り下げた）。§11.3 の呼び出し点を実在の場所へ直した（`system/cost.rs` は呼んでいない、`tts/irodori.rs` ではなく `commands/tts.rs` と `tasks.rs`）。**コマンド・イベント・設定フィールド・DB スキーマの変更なし。** |
