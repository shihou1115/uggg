# 変更仕様書: テキスト読み上げツール 台本形式対応 (Script Reader)

**日付**: 2026-07-04
**状態**: レビュー待ち (承認後に spec.md / architecture.md / text-reader-spec.md §7 へ反映して実装)
**改訂 1**: 2026-07-04 外部レビュー 3 件を反映 (バリデーション規則の fail-fast 統一・caption 有効判定・
slot 未設定検証・フェンス抽出規則の厳密化・pause 解決順の一元化・テスト計画増強)
**改訂 2**: 2026-07-04 再レビュー 3 件を反映 (caption 注記と実行時フォールバックの矛盾解消・
`irodori_assets_ready` の出所明記・ms 変換丸め規則・話者 ID 検証・エラー優先順位・互換性の逃げ道)
**背景**: v0.1.1 のテキスト読み上げツール (docs/text-reader-spec.md) は「将来対応 §7」として
台本形式を記録した。本書はそれを具体化する。プレーン .txt では表現できない、
対話・朗読劇レベルの演出制御 (話者切替・行ごとの速度・声質指示・間) が目的。
upstream Irodori-TTS が採用する台本形式 (Markdown 内 JSON コードブロック) を基礎とする。

---

## 1. 課題整理

### 機能課題

| # | 課題 | 方針 |
|---|---|---|
| S1 | 台本ファイルの受け取り | 読み上げパネル表示中の DnD に **`.md`** を追加受理 (.txt と同じ規約: 非表示時は無視、ghost/shell 経路に流さない、1MB 上限)。台本形式でない .md には専用エラー (§2.3) |
| S2 | 話者切替 | 台本の話者 ID を ugg のスロット (main / sub) にマッピングする **`slot` キー規約** (§2.3)。行ごとに `synthesize_voice(slot=...)` を切替 |
| S3 | 行ごとの速度 | speed オフセットを既存の再生側 playbackRate に合成 (§2.4)。両エンジン一律・sidecar 変更不要 |
| S4 | 行ごとの声質指示 (caption) | sidecar 合成 API に caption を配線 (§3.3)。Irodori 実モデル時のみ有効 (§2.5) |
| S5 | 行間の間 (pause) | チャンクごとの `pause_after_ms` に一元化 (§2.6)。既定値解決はロード時に Rust 側で完結 |
| S6 | 不正な台本 | **ロード時に全行検証し fail-fast** (§2.8)。部分的に読めても再生を開始しない。エラーは種別 + 位置 (`lines[i].key`) 付き |
| S7 | 使用 slot の声が未設定 | **再生開始前に検証** (§2.7)。Irodori 実モデル時、台本が使う slot の参照音声が未生成なら再生を開始しない |

### 技術課題

| # | 課題 | 方針 |
|---|---|---|
| T1 | Markdown パース | 依存追加しない。フェンスの行スキャン抽出 (規則は §2.3 で厳密に定義) + `serde_json` (既存)。full Markdown パーサは不要 |
| T2 | caption を渡す経路が存在しない | 現状 `/v1/audio/speech` は caption を受けず、`RealModelBackend.synthesize` は `caption=None` 固定 (sidecar.py:312)。Rust `SpeechRequest` / Python `SpeechRequest` / `synthesize` に caption を追加配線する (§3.3) |
| T3 | Irodori 実モデルは speed を無視 | `_ = speed` で握りつぶし duration_scale=1.0 固定 (sidecar.py:308)。**本改訂では触らない** — 速度は従来どおり再生側 playbackRate で適用し、行オフセットもそこに合成する。duration_scale 配線 (ピッチ非破壊) は将来課題 §7 |
| T4 | チャンク構造の拡張 | `reader_load_text` の戻り値を `Vec<String>` → `Vec<ReadingChunk>` (§2.9) に破壊的変更。影響範囲の根拠: `invoke("reader_load_text")` の呼び出しは src/panels/reader.ts の 1 箇所のみ (grep 確認済み)。フロントに TS テストは無く、進捗表示・停止処理はチャンクを不透明に扱っている。回帰条件は §5.3 |
| T5 | 台本行が 120 字超 | 行内テキストを既存 `split_reading_chunks` でさらに分割 (絵文字クラスタ非分断は既存テスト済みロジックを流用)。slot / speed_offset / caption は分割後の全断片へ複製、`pause_after_ms` は**最終断片のみ元の値、中間断片は 0**（一文の途中に不自然な間を作らない） |
| T6 | フロントで生 JSON を扱わない前例 | パースはすべて Rust 側 (`reader_load_text` 内)。フロントは従来どおり invoke の型付き戻り値のみ扱う |

---

## 2. 変更仕様 (spec.md へ反映する内容)

### 2.1 機能概要

- 読み上げパネル表示中に **`.md` (台本形式)** を DnD すると、台本の行単位に話者・速度・
  声質指示・間を制御しながら読み上げる。`.txt` は従来どおりプレーン読み (完全従来互換)。
- 台本は upstream Irodori-TTS 形式: Markdown 内に **必須の ` ```json speakers ` /
  ` ```json lines `** と**任意の ` ```json defaults `** コードブロックを記述する。
- エンジンは設定の `tts_engine` に従う。縮退表 (§2.10) のとおり、caption は
  Irodori 実モデル時のみ有効。voicevox でも話者切替・速度・間は有効。
- 停止・クローズ・自発発話抑制・失敗チャンクスキップ・進捗表示は .txt 読みと共通。

### 2.2 対象外 (この改訂では実装しない)

- main / sub 以外の第 3 スロット追加、話者マッピング UI (`slot` キー規約のみ)
- Irodori sidecar の duration_scale 配線 (行速度のピッチ非破壊適用) → 将来課題 §7
- `cfg_scale_caption` の外部制御 (sidecar 内 3.0 固定を維持)
- upstream `speakers.ref_wav` (任意 WAV 参照) のサポート — 指定時は明示エラー (§2.3)
- TTS capability を返す専用コマンドの新設 — caption 有効判定は既存情報で行う (§2.5)
- 一時停止/再開・シーク・行スキップ (v0.1.1 の対象外を引き継ぐ)
- 通常 Markdown 文書の読み上げ (台本ブロックの無い .md はエラー、プレーン読みしない)

### 2.3 台本形式の仕様

````
```json defaults
{ "default_pause_seconds": 0.3, "speed": -0.1 }
```
```json speakers
{ "host": { "slot": "main" }, "guest": { "slot": "sub" } }
```
```json lines
[
  { "speaker": "host",  "text": "ねえ、聞いた？" },
  { "speaker": "guest", "text": "知ってるわ。", "pause_after": 0.6, "speed": 0.1 },
  { "speaker": "host",  "text": "えええ！！", "caption": "驚いて大声で" }
]
```
````

**前処理**: ファイル先頭の UTF-8 BOM は除去。改行は LF / CRLF 両対応 (行分割時に `\r` を除去)。

**フェンス抽出規則** (実装差を出さないため厳密に定義):

1. 各行を前後 trim した文字列が ` ```json defaults ` / ` ```json speakers ` / ` ```json lines `
   のいずれかに**完全一致**した行を開始フェンスとする。**大文字小文字は区別する**
   (` ```JSON lines ` や ` ```json lines extra ` は台本ブロックとして扱わない)。
   行頭・行末の空白は trim で吸収するため、**末尾スペース付きのフェンスは受理される**。
2. 閉じフェンスは trim 後に ` ``` ` のみの行。閉じフェンスに info string は付けない
   (CommonMark 準拠。` ```json ` で閉じた場合は閉じフェンスと認識されず、
   未閉じフェンスエラー (§2.8) として検出される)。
3. 開始フェンスから閉じフェンスまでの間の行を当該ブロックの本文とし、`serde_json` でパースする。
4. **未閉じフェンスはエラー**。**同名ブロックの重複はエラー**。
5. フェンス外の Markdown 本文は無視する (自由なメモ欄)。ブロック本文中に台本と無関係の
   ` ``` ` が現れるケースは JSON として不正になるため、パースエラーとして検出される。
6. `speakers` / `lines` ブロックが見つからない場合は専用エラー:
   「**台本形式ではありません (speakers / lines ブロックが必要です)。通常の Markdown の
   読み上げには対応していません**」— 一般の .md を誤って DnD したユーザーが原因を理解できる文言にする。

**speakers (必須)**: `{ "<話者ID>": { "slot": "main" | "sub" } }`
- ugg 拡張規約。話者 ID は台本内で自由、値の `slot` で ugg の声にマッピングする。
- `slot` が main/sub 以外 → エラー。
- `ref_wav` キーが存在 → 明示エラー: 「**ugg では外部 WAV の参照 (ref_wav) をサポートして
  いません。代わりに slot キー ("main" または "sub") を指定してください**」。
- 話者 ID は **`id == id.trim()` かつ非空**であること。空文字・空白のみ・前後空白付きの
  ID はエラー (`InvalidSpeakerId`)。
- その他の未知キーは無視 (upstream 前方互換)。

**lines (必須)**: 行オブジェクトの配列。

| キー | 型 | 必須 | 検証 (範囲外・違反はロードエラー) | 意味 |
|---|---|---|---|---|
| `speaker` | String | ✅ | speakers に定義済みの ID であること | 話者 |
| `text` | String | ✅ | `trim()` 後に非空であること | 読み上げテキスト |
| `speed` | f64 | — | **[-1.0, +1.0]** | 速度オフセット (§2.4)。省略時 defaults.speed、それも無ければ 0 |
| `caption` | String | — | **Unicode scalar value で最大 200 文字** (`chars().count()` で検証)。空文字・空白のみは「caption なし (None)」扱い (エラーにしない) | 声質・演技指示 (§2.5) |
| `pause_after` | f64 (秒) | — | **[0.0, 10.0]** | この行の後の間 (§2.6) |
| (未知キー) | — | — | 無視 | upstream 前方互換 |

**defaults (任意)**: `{ "default_pause_seconds": f64, "speed": f64 }`。
検証は lines と同じ範囲 (`default_pause_seconds` は [0, 10]、`speed` は [-1, +1])。未知キーは無視。

**バリデーション方針の統一**: 台本はユーザー編集物なので、範囲外の値は**黙って丸めず
ロードエラーにする** (fail-fast)。丸めが起きると台本作成者がミスに気付けない。
clamp を行うのは実効再生レートの最終合成 (§2.4) のみで、これは台本値ではなく
「設定値との合成結果」の安全弁である。

### 2.4 速度の意味論

- 設定の `tts_speed` は**再生レート係数** (1.0 = 等速、範囲 [0.5, 2.0]、既存仕様)。
  台本の speed は upstream 準拠の**加算オフセット** (基準 0、範囲 [-1.0, +1.0])。
- `line.speed` は `defaults.speed` を**上書き** (加算しない)。
- 実効再生レート = `clamp(tts_speed + オフセット, 0.5, 2.0)`。
  適用は既存どおり再生側 playbackRate (両エンジン一律、sidecar 変更なし)。
- 音量は従来どおり `tts_volume` 一律 (台本からの制御なし)。

### 2.5 caption の意味論と有効判定

- `caption` は Irodori 実モデル (500M-v3) の caption 条件付けに渡す
  (`cfg_scale_caption` は sidecar 内の既存値 3.0 のまま)。
- **有効判定は再生開始前にフロントで行う**。専用コマンドは追加せず、**既存 Tauri コマンド
  `irodori_assets_ready` (資産の存在確認、architecture.md §4.7) と設定 `tts_engine` の組合せ**
  で判定する: `caption 有効 = (tts_engine == "irodori") && irodori_assets_ready()`。
  この判定は「Irodori 実モデルが利用可能である可能性が高い」ことの**事前判定であり、
  合成の成功を保証しない**。実行時に合成失敗した場合は既存フォールバック経路に従う。
- **注記の表示条件**: caption 行を含む台本を、**事前判定で caption 非対応と分かった環境**で
  再生する場合に限り、エラーにはせず、読み上げパネル内に注記ラベルを
  **再生開始から停止/完了まで常時表示**する:
  「※ 演出指示 (caption) は現在の音声エンジンでは無視されます」。
  caption 行の無い台本・.txt・caption 有効と判定された環境では表示しない。
- **実行時フォールバック** (Irodori 合成失敗 → voicevox 代替) により、caption 有効と判定した
  後でも個々のチャンクで caption が適用されないことがある。このケースは既存の絵文字
  アノテーション縮退と同等の挙動として**許容し、UI では逐次通知しない** (このとき注記
  ラベルも表示されない — 事前判定で有効だった環境のため。仕様として意図的な割り切り)。

### 2.6 pause の解決規則 (一元化)

pause はロード時に Rust 側で解決し、`ReadingChunk.pause_after_ms` に確定値として格納する。
フロントの定数 `CHUNK_PAUSE_MS` は廃止し、再生ループは `chunk.pause_after_ms` のみ参照する
(既定値 500ms は Rust 側の定数に移動)。
秒 (f64) からミリ秒 (u32) への変換は **`(seconds * 1000.0).round() as u32`** で行う
(例: 0.333 → 333ms。値はロード時に [0, 10] 検証済みのためオーバーフローしない)。

解決順:
1. `line.pause_after` (指定時)
2. `defaults.default_pause_seconds` (指定時)
3. 既定 0.5 秒

例外:
- 台本 1 行が長行分割された場合、**中間断片は 0ms**、元の値は最終断片のみ (T5)。
- `.txt` は全チャンク一律 0.5 秒 (v0.1.1 と同一挙動)。
- 最終チャンクの後の pause は再生ループ側で適用しない (既存挙動)。

### 2.7 再生開始前の slot 検証

台本が使用する slot 集合を、再生開始前に現在の TTS 状態と照合する:

- **Irodori 実モデル時**: 使用 slot それぞれに参照音声が生成済みか `voice_ref_list` で確認。
  未生成の slot があれば再生を開始せず、エラー表示:
  「**slot=sub を使用していますが、sub の参照音声が未生成です
  (設定 → 音声 → 参照音声で生成してください)**」。
  ※ 文言は slot 基準とする。`ReadingChunk` は話者 ID を保持しない (§2.9) ため、
  再生前検証の時点で話者名は復元できない (改訂 2 の文言例から P5-2 監査で修正)。
  ※ 現行実装では参照音声欠落は合成時の `VoiceRefMissing` → チャンク単位スキップになり、
  台本の片側の声だけ丸ごと無音になる。fail-fast 方針に沿い事前に止める。
- **voicevox 時**: slot ごとの話者設定は常に既定値を持つため検証不要。
- `.txt` (main 固定) は従来挙動のまま (回帰対象、§5.3)。

### 2.8 エラー報告の仕様

- Rust 側にエラー種別を定義し、テストで種別を検証できるようにする (UI 表示は文字列化):

```rust
enum ScriptError {
    NotAScript,          // speakers / lines ブロック欠落 (§2.3 専用文言)
    UnclosedFence,
    DuplicateBlock(&'static str),
    InvalidJson { block: &'static str, line_in_block: usize, file_line: usize, detail: String },
    UnsupportedRefWav { speaker_id: String },
    InvalidSlot { speaker_id: String, slot: String },
    UnknownSpeaker { index: usize, speaker: String },   // 表示: lines[index].speaker
    EmptyText { index: usize },
    OutOfRange { index: Option<usize>, key: &'static str, value: f64 }, // speed / pause_after / defaults
    CaptionTooLong { index: usize, len: usize },
    InvalidSpeakerId { id: String },     // 空・空白のみ・前後空白付きの話者 ID
    EmptyLines,                          // lines 配列が空
}
```

- **位置の表記**: lines の意味エラーは `lines[i].key` 形式 (配列 index)、defaults のエラーは
  `defaults.key` 形式で表示する。JSON 構文エラーのみ、フェンススキャン時に保持したブロック
  開始行オフセットから**元ファイルの行番号を併記**する (serde_json のエラー位置はブロック内
  相対のため換算できる)。
- **エラーの優先順位**: 認識済みの台本ブロックが 1 つ以上存在し、その JSON が不正な場合は
  `InvalidJson` を優先する。台本ブロックが 1 つも認識できない場合に `NotAScript` とする
  (speakers の JSON が壊れ lines が欠落しているファイルは `InvalidJson`)。
- 検証はロード時に行い、**最初のエラーで停止して返す** (部分再生しない)。

### 2.9 コマンド契約の変更 — spec 改訂事項

| コマンド | 変更 | 内容 |
|---|---|---|
| `reader_load_text` | **戻り値変更 (破壊的)** | `Vec<String>` → `Vec<ReadingChunk>`。旧形式は廃止。`.txt` は従来分割の各チャンクに既定メタ (slot=main, speed_offset 0, caption None, pause 500ms) を付けて返す。`.md` は台本パース (§2.3) + 長行の追加分割 (T5)。**影響範囲の根拠**: フロントの invoke 呼び出しは reader.ts の 1 箇所のみ (grep 確認済み)。回帰条件: `.txt` 入力時の読み上げ順・速度・間・停止動作が v0.1.1 と一致すること (§5.3) |
| `synthesize_voice` | **引数追加** | `caption: Option<String>` を追加。**互換性要件**: 既存フロント呼び出し (speaker.ts) が `caption` キーを送らない場合も `None` として成功すること。Tauri の Option 引数デシリアライズ (キー欠落 → None) を前提とするが、前提が崩れていないことを §5.3 の回帰で保証する。**前提が成り立たない場合は、command 引数を `#[serde(default)]` 付きの payload struct に変更して互換性を担保する** (実装時の逃げ道として固定)。空文字列は None と同義に正規化する。Irodori 実モデル経路のみ使用、他経路は無視 |

`ReadingChunk` / `VoiceSlot` (Rust、serde でフロントへ。不正値の混入余地を型で塞ぐ):

```rust
#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum VoiceSlot { Main, Sub }

#[derive(Serialize)]
struct ReadingChunk {
    text: String,
    slot: VoiceSlot,
    speed_offset: f64,       // 0 = 設定速度のまま。ロード時に [-1, +1] 検証済み
    caption: Option<String>, // ロード時に長さ検証・空白正規化済み
    pause_after_ms: u32,     // §2.6 で解決済みの確定値
}
```

```ts
export type VoiceSlot = "main" | "sub";
export interface ReadingChunk {
  text: string;
  slot: VoiceSlot;
  speed_offset: number;
  caption: string | null;
  pause_after_ms: number;
}
```

**直列化の規約**: `ReadingChunk.caption` は**常にキーを含め**、None は `null` として出力する
(`skip_serializing_if` を付けない — TS 側に `undefined` を考慮させない)。`skip_serializing_if`
を使うのは irodori.rs の `SpeechRequest` (旧 sidecar 互換のためキー自体を省略) のみ。

イベント追加なし。設定 (Settings) 追加なし。DB 変更なし。新規依存なし。

### 2.10 エンジン別縮退表

| 要素 | Irodori 実モデル | Irodori フォールバック時 / voicevox |
|---|---|---|
| 話者切替 (slot) | ✅ slot の参照音声 (§2.7 で事前検証) | ✅ slot の voicevox 話者設定 |
| 行速度 (playbackRate) | ✅ | ✅ |
| 間 (pause_after) | ✅ | ✅ |
| caption | ✅ | ❌ 無視 (パネルに常設注記 §2.5) |
| 絵文字アノテーション | ✅ (既存機構) | ❌ 無視 (既存挙動) |

---

## 3. アーキテクチャ変更 (architecture.md へ反映する内容)

### 3.1 新規モジュール

```
src-tauri/src/tts/script.rs   -- フェンス抽出 + 台本パース + 検証 + ScriptError (pure、テスト対象の中心)
```

### 3.2 変更ファイル

| ファイル | 変更 |
|---|---|
| `src-tauri/src/tts/reader.rs` | `ReadingChunk` / `VoiceSlot` 定義。`.md` 分岐 → `script::parse_script`。長行分割時のメタ複製 + 中間断片 pause=0 (T5)。既定 pause 定数 (500ms) をここへ移動 |
| `src-tauri/src/commands/reader.rs` | `reader_load_text` 戻り値型変更。拡張子で .txt/.md 分岐 |
| `src-tauri/src/commands/tts.rs` | `synthesize_voice` に `caption: Option<String>` (tts.rs:69)。空文字→None 正規化。Irodori 経路の `SpeechRequest` へ透過 |
| `src-tauri/src/tts/irodori.rs` | `SpeechRequest` に `caption: Option<String>` (irodori.rs:349)。None 時は JSON にフィールドを含めない (`skip_serializing_if`) — 旧 sidecar との互換を保つ |
| `src-tauri/python/sidecar.py` | `SpeechRequest` に `caption: Optional[str] = None` (sidecar.py:73)。`RealModelBackend.synthesize` の `caption=None` 固定 (sidecar.py:312) を引数透過に変更 — **caption=None 時は現行と完全に同一の呼び出しになる** (None 経路は無変更、回帰リスク最小)。`_make_request` は変更不要 (cfg_scale_caption 3.0 維持)。フォールバックバックエンドは caption を参照しない |
| `src/panels/reader.ts` | 再生ループを `ReadingChunk` 対応: slot="main" ハードコード (reader.ts:110) を chunk.slot に、`CHUNK_PAUSE_MS` 定数 (reader.ts:30) を chunk.pause_after_ms に置換、playbackRate に speed_offset を合成 (reader.ts:204)。再生開始前の slot 検証 (§2.7) と caption 注記ラベル (§2.5)。停止トークンによる pause 中断は既存 `sleepCancellable` を流用 (停止後に残り pause が実行されないこと) |
| `src/dnd.ts` | `.txt` 判定 (dnd.ts:41) を `.txt` / `.md` の 2 拡張子に。受理規約は共通 |
| `src/types.ts` | `ReadingChunk` / `VoiceSlot` 型追加 |

### 3.3 caption 配線 (今回唯一の sidecar 変更)

```
reader.ts ── invoke synthesize_voice(text, slot, caption)
  → commands/tts.rs ── Irodori 経路のみ SpeechRequest.caption に設定
    → sidecar.py /v1/audio/speech ── SpeechRequest.caption (Optional, 既定 None)
      → RealModelBackend.synthesize(caption=...)  ← None 固定を解除
        → SamplingRequest(caption=..., cfg_scale_caption=3.0 既存値)
```

voicevox 経路・フォールバック経路は caption を参照しない (シグネチャ互換のみ確保)。

### 3.4 再生ループの変更点 (reader.ts)

v0.1.1 の構造 (先読み 1・停止トークン・失敗スキップ) は不変。変わるのは
「チャンク=文字列」→「チャンク=ReadingChunk」の置換と、チャンクごとの
slot / playbackRate / pause の 3 パラメタ適用、再生開始前の slot 検証、caption 注記の 5 点。

---

## 4. 実装計画

| Step | 内容 | 検証 |
|---|---|---|
| 1 | spec.md §4.5.8 / architecture.md (§4.7 契約表・モジュール図) / text-reader-spec.md §7 へ本仕様を反映 | 文書レビュー |
| 2 | `tts/script.rs`: フェンス抽出 + パース + 検証 + `ScriptError` + ユニットテスト (§5.1) | `cargo test` |
| 3 | `tts/reader.rs`: `ReadingChunk`/`VoiceSlot` + .txt 経路のメタ付与 + 長行分割のメタ複製・中間 pause=0 | `cargo test` (既存テスト改修込み) |
| 4 | `commands/reader.rs` / `commands/tts.rs` / `irodori.rs` / `sidecar.py` の contract 変更 + `SpeechRequest` 直列化テスト | `cargo check` / `cargo test` |
| 5 | `reader.ts` / `dnd.ts` / `types.ts` (slot 検証・注記ラベル込み) | `npx tsc --noEmit` |
| 6 | 手動テスト S1〜S12 (§5.2) | 実機 |
| 7 | `docs/manual.md` (台本形式の書き方) / `quality_checklist.md` 更新、コミット | — |

## 5. テスト計画

### 5.1 ユニットテスト (Rust、engine 不要)

`script::parse_script` (エラー系は `ScriptError` の種別まで検証):
1. 正常系: 3 ブロック → 行ごとの slot / speed / caption / pause が期待どおり
2. defaults 省略 → speed 0 / pause 500ms
3. line.speed が defaults.speed を上書き (加算されない)
4. speakers 未定義の話者 ID → `UnknownSpeaker` (lines[i] の index 付き)
5. `ref_wav` 指定 → `UnsupportedRefWav` (slot を使う旨の文言)
6. slot が main/sub 以外 → `InvalidSlot`
7. speakers / lines ブロック欠落 → `NotAScript` (専用文言)、同名ブロック重複 → `DuplicateBlock`
8. text 空文字列・**空白のみ** → `EmptyText`。未知キー (speakers / lines / defaults) → 無視で通る
9. `pause_after` / `default_pause_seconds` 範囲外 ([0,10] 外) → `OutOfRange` (clamp しない)
10. `speed` 範囲外 ([-1,+1] 外) → `OutOfRange`
11. caption 201 文字 → `CaptionTooLong`。caption 空文字・空白のみ → None として通る
12. フェンス外の Markdown 本文が無視される。` ```JSON lines ` / ` ```json lines extra ` は
    ブロック扱いされない (→ `NotAScript` に到達)
13. 未閉じフェンス → `UnclosedFence`
14. JSON 構文エラー → `InvalidJson` にブロック名 + 元ファイル行番号が入る
15. UTF-8 BOM 付き・CRLF 改行の台本が正常にパースできる
16. 話者 ID の異常系: 空文字 / 空白のみ / 前後空白付き → `InvalidSpeakerId`
17. lines 配列が空 → `EmptyLines`。ブロック本文が空 (フェンスのみ) → `InvalidJson`
18. エラー優先順位: speakers の JSON 破損 + lines 欠落 → `NotAScript` ではなく `InvalidJson`
19. pause の ms 変換丸め: 0.333 → 333 / 10.0 → 10000 / 0.001 → 1

`reader.rs` (改修分):
20. .txt → 全チャンクが既定メタ (slot=Main, offset 0, caption None, pause 500ms)
21. 台本の 120 字超行 → 分割後全断片に slot/speed_offset/caption 複製、
    **中間断片の pause_after_ms が 0**、最終断片のみ元の値
22. 絵文字クラスタ非分断 (既存テストの `ReadingChunk` 対応改修)

`irodori.rs`:
23. `SpeechRequest` の JSON 直列化: caption=Some → フィールドあり、None → フィールドなし
    (`skip_serializing_if` の確認。旧 sidecar 互換の根拠)。`ReadingChunk` は逆に
    None でも `caption: null` を常に出力すること

### 5.2 実機手動テスト (quality_checklist へ S 節として追加)

**前提**: S4・S9 は Irodori 実モデル導入済み環境で行う。未導入環境では代替として
sidecar ログで `SamplingRequest.caption` に値が透過されることを確認する (S4')。

| # | 手順 | 期待 |
|---|---|---|
| S1 | 台本 .md を DnD (voicevox) | host 行=main の声、guest 行=sub の声で交互に読む |
| S2 | speed 指定行 (±0.15) | 該当行だけ速度が変わる。実効レートは [0.5, 2.0] に収まる |
| S3 | pause_after 0.6 の行 | 行の後の間が明確に長い |
| S4 | **Irodori 実モデル + caption 行** (「驚いて大声で」等) | 演技が音声に乗る。sidecar ログの SamplingRequest.caption に値が入る |
| S5 | voicevox で caption 入り台本 | エラーにならず読む + パネルに注記が**再生終了まで**表示。caption なし台本、および Irodori 実モデル可判定の環境では注記が出ない |
| S6 | 不正台本 (未定義話者 / ref_wav / JSON 破損 / speed 範囲外 / 通常の Markdown 文書) | 再生開始せず、種別ごとの文言 (§2.3/§2.8) で原因が分かる |
| S7 | プレーン .txt (回帰) | v0.1.1 と同一挙動 (順序・速度・間・停止) |
| S8 | 台本読み上げ中の停止/クローズ/自発発話抑制 (回帰) | .txt 読みと同一挙動。**pause 待機中に停止しても残りの pause・次チャンクが実行されない** |
| S9 | Irodori 実モデルで sub の参照音声を削除してから sub 使用台本を DnD | 再生開始前にエラー (§2.7 の文言)。1 チャンクも再生されない |
| S10 | 長い 1 行 (300 字) を含む台本 | 行の途中に不自然な間が入らない (中間断片 pause=0) |
| S11 | Irodori 実モデルで caption なし台本 | v0.1.1 相当の合成品質 (caption=None 透過の回帰) |
| S12 | 台本読み上げ中にゴーストへチャット (回帰) | 応答発話がブロックされない (既存仕様) |

### 5.3 回帰確認

- **`synthesize_voice` の caption 省略互換**: 既存呼び出し (speaker.ts、caption キーなし) が
  両エンジンで従来どおり成功する (S12 と通常チャット発話で確認)
- ゴースト/シェル zip の DnD 導入が従来通り動く (.md 追加の影響がない)
- `.txt` + main 参照音声未生成時の挙動が v0.1.1 と同一 (§2.7 は台本のみの追加検証)
- `cargo test` 全件 / `npx tsc --noEmit`

---

## 6. 既定値として仕様に採り込んだ判断 (レビューポイント)

| 判断 | 採用値 | 代替案 |
|---|---|---|
| 台本フォーマット | upstream 互換 (.md + JSON ブロック) | 独自 YAML (serde_yaml 既存) / 独自テキスト記法 |
| 台本の拡張子 | `.md` + 非台本には専用エラー文 | `.script.md` 等の専用拡張子 |
| フェンス判定 | trim のみ許容、大文字小文字は区別、完全一致 | 大小非区別などの寛容化 (fail-fast 方針に反するため不採用) |
| 話者マッピング | speakers ブロック内 `slot` キー規約 (UI なし) | マッピング UI / ref_wav 対応 |
| ref_wav 指定時 | 明示エラー (対処法入り文言) | 警告して main で読む |
| 話者数 | main / sub の 2 声まで | コマンド層の slot 固定解除 (第 3 スロット) |
| 行速度の実現 | 再生側 playbackRate 合成 (ピッチ変動あり・両エンジン一律) | sidecar duration_scale 配線 (Irodori のみピッチ非破壊) |
| speed の合成 | line が defaults を上書き、実効 = clamp(tts_speed + offset) | defaults と line の加算 |
| 台本値の検証 | **全項目 fail-fast** (speed/pause/caption 長/空 text は範囲外エラー、丸めない) | 寛容 clamp (作成者がミスに気付けないため不採用) |
| caption 有効判定 | 既存情報 (tts_engine + irodori_assets_ready) で再生開始前に判定 | capability API 新設 (コマンド最小化の規律から不採用。将来課題 §7) |
| 実行時フォールバックの caption 脱落 | 絵文字縮退と同等に許容 (常設注記が包含) | 合成結果ごとの backend 通知 |
| caption 無効環境 | 常設注記ラベルを表示して読む | エラーで止める / トースト 1 回 |
| 戻り値の互換 | `reader_load_text` を破壊的に変更 (呼び出し元 1 箇所の根拠を T4 に明記) | 新コマンド並設 (契約の二重化を避け不採用) |
| エラー位置の表記 | `lines[i].key` (JSON 構文エラーのみ元ファイル行番号併記) | 全エラーで元ファイル行番号 (serde_json では過剰実装になるため不採用) |

## 7. 将来対応 (本改訂でも残すもの)

- **duration_scale 配線**: 行速度をピッチ非破壊で効かせる (sidecar `_make_request` の可変化)。
  playbackRate 方式の品質が実測で不足したら着手。
- **TTS capability API**: バックエンド実状態 (実モデル / fallback) の問い合わせ。第 3 スロット
  対応や caption 以外の条件付けが増えた時点で `slot` 検証 (§2.7) ごと集約する。
- **第 3 スロット以降**: voice_ref 層 (DB/ファイル) は任意 slot 対応済み。コマンド層の
  main/sub 固定 (`synthesize_voice` ほか 4 コマンド) の解除と管理 UI が必要。
- **cfg_scale_caption の行指定** / defaults の他パラメタ透過。
- **台本のキュー化・プレイリスト** (複数 DnD は先頭 1 件のままとする)。
