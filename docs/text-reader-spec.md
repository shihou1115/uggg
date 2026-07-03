# 変更仕様書: テキスト読み上げツール (Text Reader)

**日付**: 2026-07-04
**状態**: レビュー待ち (承認後に spec.md / architecture.md へ反映して実装)
**背景**: LLM が絵文字を安定して返さないため、Irodori-TTS V3 絵文字アノテーション (G5+)
の実機検証が困難。任意テキストを読み上げるツールがあれば、絵文字入り .txt を投入して
検証できる。同時に、青空文庫等の長文を「キャラの声で朗読させる」という一般用途としても
価値がある。

---

## 1. 課題整理

### 機能課題

| # | 課題 | 方針 |
|---|---|---|
| K1 | 読み上げの起動導線 | コンテキストメニューに「テキスト読み上げ」を追加 → 専用パネルを開く |
| K2 | .txt の受け取り | 読み上げパネル**表示中のみ** ugg ウインドウへの DnD で受理 (ユーザー要件どおり「ツール起動後に DnD」)。パネル非表示時の .txt ドロップは無視 |
| K3 | 既存 DnD (ゴースト/シェル導入) との競合 | `dnd.ts` の入口で拡張子 `.txt` を分岐。既存経路は zip/フォルダのみ受けるため実害なし |
| K4 | 長文で音声生成が失敗/遅延する | **チャンク分割** (§3.3)。文境界を優先し最大 120 文字で区切り、チャンク単位に合成 |
| K5 | 絵文字アノテーション対応 | 合成に既存 `synthesize_voice` を再利用するため、Irodori 経路の絵文字保護 (f73e566) がそのまま効く。追加実装は「チャンク分割が絵文字を分断しない」ことのみ |
| K6 | 読み上げ中のゴースト自発発話との衝突 | 読み上げ中は自発発話 (独り言・放置反応・時報系) を抑制。`should_stay_quiet` に読み上げ中フラグを追加 |
| K7 | 停止手段 | パネルに停止ボタン。パネルを閉じても停止 |
| K8 | 進捗の可視化 | パネルに「n / m チャンク」+ 現在読み上げ中のチャンク本文を表示 |

### 技術課題

| # | 課題 | 方針 |
|---|---|---|
| T1 | .txt のエンコーディング | UTF-8 (BOM 有無) + Shift_JIS を自動判定 (`encoding_rs` を新規依存に追加)。判定不能なら明示エラー |
| T2 | 巨大ファイル | 1 MB 上限。超過は明示エラー (テキスト 1MB ≒ 50 万字、朗読 10 時間超なので実用上十分) |
| T3 | チャンク分割時の絵文字分断 | 文境界・読点分割では絵文字は分断されない。**強制分割** (1 文が長すぎる場合) のみ、既存 `preprocess::split_emoji_segments` を再利用して絵文字クラスタ (ZWJ/VS16 込み) の途中で切らないことを保証 |
| T4 | 合成と再生のパイプライン | 「全チャンク合成後に再生」は初動が遅すぎる。**先読み 1**: チャンク i を再生中にチャンク i+1 を合成。再生ループは reader 専用 (既存 speaker キューはゴースト発話用に温存) |
| T5 | チャンク合成失敗 | 該当チャンクをスキップして続行し、終了時に「N 件スキップ」を表示。Irodori 失敗時の voicevox fallback は synthesize_voice 内の既存機構に委ねる |
| T6 | WebView2 透過バグ | パネルは index.html に静的配置 (既存パネルと同じ回避策) |

---

## 2. 変更仕様 (spec.md へ反映する内容)

### 2.1 機能概要

- コンテキストメニュー「テキスト読み上げ」で読み上げパネルを開く。
- パネル表示中に `.txt` ファイルを ugg ウインドウへ DnD すると、その内容を**メインキャラの声**で読み上げる。
- エンジンは設定の `tts_engine` に従う (voicevox / Irodori)。Irodori + 実モデル時は絵文字アノテーションが有効。
- 読み上げ速度・音量は既存 `tts_speed` / `tts_volume` に従う。
- 読み上げ中はゴーストの自発発話を抑制する (リマインダーの「静音中も鳴らす特例」は従来通り鳴る)。
- 停止ボタンまたはパネルを閉じると読み上げを即停止する。

### 2.2 対象外 (このリリースでは実装しない)

- 一時停止 / 再開、シーク、チャンク単位のスキップ操作
- sub キャラの声での読み上げ、話者の切替 UI
- .txt 以外の形式 (PDF / EPUB / クリップボード / URL)
- 読み上げテキストの吹き出し表示・口パク同期 (パネル内テキスト表示のみ)
- チャンク长の設定 UI (定数 120 文字。実測後に調整)

### 2.3 チャンク分割仕様

`split_reading_chunks(text) -> Vec<String>` (Rust pure 関数、既定 `MAX_CHUNK_CHARS = 120`):

1. 改行で行に分割し、各行内を文末記号 (`。` `！` `？` `!` `?` `…`) で文に分割
2. 文を順に詰め、120 文字 (chars 数) を超えない範囲で 1 チャンクに結合
3. 単一文が 120 文字超のときは読点 `、` で分割して詰め直す
4. それでも超える断片は 120 文字で強制分割。ただし絵文字クラスタ (Irodori 45 種、ZWJ/VS16 込み) の途中では切らない
5. 空白のみのチャンクは捨てる

### 2.4 追加コマンド (Tauri) — spec 改訂事項

| コマンド | 引数 | 戻り | 説明 |
|---|---|---|---|
| `reader_load_text` | `path: String` | `Vec<String>` (チャンク列) | 拡張子 `.txt` 検証 → 1MB 上限検証 → UTF-8/SJIS 自動判定で読込 → チャンク分割して返す |
| `set_reading_active` | `active: bool` | `()` | 読み上げ中フラグの設定 (自発発話抑制用)。パネルの開始/停止/クローズで呼ぶ |

合成は既存 `synthesize_voice(text=チャンク, slot="main")` をチャンクごとに呼ぶ (新規合成コマンドなし)。これにより漢字→かな前処理・絵文字保護・Irodori fallback・通知クールダウンが自動的に適用される。

### 2.5 状態追加 — spec 改訂事項

- `AppState.presence.reading: AtomicBool` (既定 false)。`quiet::should_stay_quiet` が true 判定条件に加える。設定 (Settings) への追加は無し (永続化しない一時状態)。

### 2.6 新規依存 — spec 改訂事項

- `encoding_rs = "0.8"` (Shift_JIS 判定/変換。pure Rust)

---

## 3. アーキテクチャ変更 (architecture.md へ反映する内容)

### 3.1 新規モジュール

```
src-tauri/src/tts/reader.rs      -- split_reading_chunks / decode_text_file (pure中心、テスト対象)
src-tauri/src/commands/reader.rs -- reader_load_text / set_reading_active
src/panels/reader.ts             -- パネル UI + 逐次再生ループ (先読み 1) + 停止トークン
```

### 3.2 変更ファイル

| ファイル | 変更 |
|---|---|
| `index.html` | `#reader-panel` 静的配置 (ヘッダ / DnD ヒント / ファイル名 / 進捗 / 現在チャンク / 停止ボタン) |
| `src/dnd.ts` | `handleDnd` 冒頭で `.txt` を分離。reader パネル表示中なら reader へ、非表示なら無視 |
| `src/menu/context-menu.ts` | 「テキスト読み上げ」項目追加 (ポモドーロの下) |
| `src/main.ts` | `mountReaderPanel()` 呼び出し |
| `src-tauri/src/state.rs` | `presence.reading: AtomicBool` |
| `src-tauri/src/presence/quiet.rs` | `should_stay_quiet` に reading 判定追加 |
| `src-tauri/src/tts/preprocess.rs` | `split_emoji_segments` / `Segment` を `pub(crate)` へ (reader の強制分割で再利用) |
| `src-tauri/src/main.rs` | コマンド 2 件を invoke_handler へ登録 |
| `src-tauri/Cargo.toml` | `encoding_rs` 追加 |

### 3.3 再生ループ (reader.ts) の設計

```
状態: idle → loading → playing(n/m) → done/stopped/error
1. DnD → reader_load_text(path) → チャンク列 + set_reading_active(true)
2. ループ: synth(i) を await (先読み: 再生開始時に synth(i+1) を発火しておく)
   → base64 wav を Web Audio で再生 (reader 専用 AudioContext、音量 = tts_volume,
      playbackRate = tts_speed。ゴースト発話の speaker キューとは独立)
3. 停止トークン: 停止/クローズで再生ソース stop + 未処理チャンク破棄 + set_reading_active(false)
4. チャンク失敗: スキップして次へ。終了時に「N 件スキップ」表示
```

読み上げ中もゴーストのチャット応答 (ユーザー起点) はブロックしない。同時発声は稀な操作
なので初版では許容する (自発発話のみ抑制)。

---

## 4. 実装計画

| Step | 内容 | 検証 |
|---|---|---|
| 1 | spec.md §4.5 / architecture.md へ本仕様を反映 | 文書レビュー |
| 2 | `tts/reader.rs`: `decode_text_file` (拡張子/サイズ/エンコーディング) + `split_reading_chunks` + ユニットテスト | `cargo test` |
| 3 | `preprocess.rs`: `split_emoji_segments` の可視性変更 (`pub(crate)`) | `cargo test` (既存 12 件回帰なし) |
| 4 | `commands/reader.rs` + `state.rs` reading フラグ + `quiet.rs` 判定 + `main.rs` 登録 | `cargo check` |
| 5 | `index.html` パネル + `panels/reader.ts` + `dnd.ts` 分岐 + `context-menu.ts` 項目 | `npx tsc --noEmit` |
| 6 | 手動テスト R1〜R8 (§5.2) — **R4 で G5+ 絵文字検証を兼ねる** | 実機 |
| 7 | `docs/manual.md` / `docs/quality_checklist.md` 更新、コミット | — |

## 5. テスト計画

### 5.1 ユニットテスト (Rust、engine 不要)

`split_reading_chunks`:
1. 短文 1 つ → 1 チャンク
2. 複数短文が 120 字以内に結合される
3. 121 字目にかかる文は次チャンクへ (文境界優先)
4. 1 文 120 字超 → 読点分割
5. 読点なし 120 字超 → 強制分割、かつ char 境界安全
6. 強制分割が Irodori 絵文字 (ZWJ 😮‍💨) を分断しない
7. 絵文字入り文がチャンク内にそのまま残る
8. 空行・空白のみの行が捨てられる
9. 文末記号 (。！？…) それぞれで区切れる

`decode_text_file`:
10. UTF-8 (BOM 無し) / UTF-8 BOM / Shift_JIS が読める
11. `.txt` 以外の拡張子 → エラー
12. 1MB 超 → エラー

### 5.2 実機手動テスト (quality_checklist へ R 節として追加)

| # | 手順 | 期待 |
|---|---|---|
| R1 | コンテキストメニュー →「テキスト読み上げ」 | パネルが開く |
| R2 | パネル表示中に UTF-8 .txt を DnD (voicevox エンジン) | 先頭から読み上げ、進捗 n/m が進む |
| R3 | 長文 .txt (2,000 字程度) | チャンク間で途切れず最後まで読む。生成失敗しない |
| R4 | **Irodori 実モデル + 絵文字入り .txt** (😊/😭/🤧🤧/👂/🍕 を含む) | エモートが音声に乗る。🍕 は無視。**G5+ をこれで消化** |
| R5 | 読み上げ中に停止ボタン | 即停止、進捗リセット |
| R6 | 読み上げ中に放置 | 独り言が出ない (抑制)。停止後は再開 |
| R7 | Shift_JIS の .txt | 文字化けせず読める |
| R8 | パネル非表示時に .txt を DnD | 何も起きない (ghost/shell 経路にも入らない) |

### 5.3 回帰確認

- ゴースト/シェル zip の DnD 導入が従来通り動く (R8 と併せて)
- 通常チャット発話 (voicevox / Irodori 両エンジン) に変化なし
- `cargo test` 全件 / `npx tsc --noEmit`

---

## 6. 既定値として仕様に採り込んだ判断 (レビューポイント)

| 判断 | 採用値 | 代替案 |
|---|---|---|
| チャンク最大長 | 120 文字 (定数) | 設定化 / エンジン別の長さ |
| 読み上げの声 | main の参照音声・話者固定 | 話者選択 UI |
| パネル非表示時の .txt DnD | 無視 | パネル自動オープンして開始 |
| 失敗チャンク | スキップ + 件数表示 | リトライ 1 回 / 中断 |
| ユーザー発話との同時発声 | 許容 (自発発話のみ抑制) | 読み上げ中は全 TTS を排他 |
| エンコーディング | UTF-8 / SJIS のみ | UTF-16 対応 |
| 複数 .txt の同時 DnD | 先頭 1 件のみ読む | キュー化して連続読み上げ |
| チャンク間ポーズ | 0.5 秒 (定数、最終チャンク後は無し) | 設定化 / 台本形式の pause_after (§7) |

---

## 7. 将来対応: 台本形式 (Script 形式) — 本リリースでは対象外

upstream Irodori-TTS が採用する台本形式 (Markdown 内に 3 つの JSON コードブロック:
`defaults` / `speakers` / `lines`) への対応を将来課題として記録する。プレーン .txt では
表現できない、対話・朗読劇レベルの演出制御が目的。

### 7.1 形式の概要 (実サンプルより)

```
```json defaults
{ "default_pause_seconds": 0.05, "speed": -0.1, ... }
```
```json speakers
{ "A": { "ref_wav": "voices/host_a.wav" }, "B": { "ref_wav": "voices/host_b.wav" } }
```
```json lines
[
  { "speaker": "host",  "text": "ねえ！…", "speed": 0.1 },
  { "speaker": "guest", "text": "知ってるわ。…", "pause_after": 0.25, "speed": -0.15 },
  { "speaker": "host",  "text": "えええ！！", "caption": "驚いて大声で" },
  ...
]
```
```

### 7.2 ugg に取り込む場合の対応方針

| 要素 | 対応方針 |
|---|---|
| **話者指定** (`speaker` / `speakers`) | 台本の話者 ID を ugg の参照音声 (main / sub / 将来の追加スロット) にマッピングする UI または `speakers` ブロック内で ugg 登録名を参照する規約を設ける。行ごとに `synthesize_voice(slot=...)` を切替 |
| **行ごとの速度** (`speed`, `defaults.speed`) | 現在 `SpeechRequest.speed` は送っているが sidecar 側で無視 (duration_scale=1.0 固定、再生側 playbackRate 一律)。台本対応時は sidecar の `SamplingRequest.duration_scale` に行ごとの speed を反映する配線を追加 (upstream の speed は加算オフセット形式である点に注意) |
| **行ごとの caption** (`caption`) | 現在 `RealModelBackend.synthesize` は `caption=None` 固定。`SpeechRequest` に `caption: Option<String>` を追加し、`SamplingRequest.caption` + `cfg_scale_caption` へ通す。VoiceDesign モデルではなく合成モデル (500M-v3) 側の caption 条件付けを使う |
| **行間ポーズ** (`pause_after`, `default_pause_seconds`) | チャンク再生ループのチャンク間 sleep に反映 (フロント側で実装可能、sidecar 変更不要) |
| **ファイル判別** | 拡張子 `.md` + 3 コードブロック構造のパース、または専用拡張子 (`.script.md` 等)。読み上げパネルの DnD 受理対象を拡張 |

### 7.3 前提となる本リリースの設計互換性

- チャンク再生ループは「テキスト列を順に合成して逐次再生」する構造なので、台本の
  lines 配列 (行=チャンク、行内は長ければさらに分割) にそのまま拡張できる。
- `reader_load_text` の戻り値を `Vec<String>` から「チャンク構造体 (text + 演出パラメタ)」の
  配列に拡張する余地を、フロントの再生ループがチャンクを不透明に扱う形で残しておく。
- caption / speed の sidecar 配線は §7.2 の通り追加改修が必要 (本リリースでは見送り)。
