# 日常支援（Tier S）アーキテクチャ設計書

**対象**: spec.md §4.6 日常支援（v0.2 スコープ）＝ Tier S 4 機能
**位置付け**: spec §4.6（要件の正本）を実装可能な契約・構造へ具体化する Phase 2 成果物。個別機能の詳細 spec（`text-reader-spec.md` 等）と同列の設計文書。
**状態**: 設計 v2。**M7〜M10 すべて実装済み**（M7 統合リマインダー / M8 ToDo・日課 / M9 状況発話+ガバナンス / M10 カレンダー参照。2026-07-18、architecture.md v1.4 に契約反映済み。実装時の確定判断は §11.1 と architecture §2/§11.4 の注記参照）。**Tier S 4 機能そろい、v0.2 リリース候補**。
**作成日**: 2026-07-12（v2 同日改訂、§13 改訂履歴）

---

## 0. 本書の使い方

- 本書は「どう作るか」を定義する。「何を作るか」は spec §4.6。
- **既存コードの実態を正とする**。architecture.md には実装との乖離が複数あるため（§9 参照）、本書は現行ソースを基準に設計し、architecture.md は実装時に追随改訂する。
- 各機能に **DB / コマンド / イベント / 設定 / 辞書 / ロジック / UI** の 7 面で契約を与える。
- 「実装時に確定」と注記した箇所は擬似コード／方針までとし、細部は実装 PR で詰める。

---

## 1. 設計原則（spec §4.6 の 3 原則を設計制約へ）

| spec の原則 | 設計制約 |
|---|---|
| **AI 非依存** | Tier S の基盤（登録・判定・通知・表示）は low = ローカル決定論で完結する。advanced（LLM）は「解釈・提案」の上乗せ層で、**基盤の呼び出し経路に LLM を挟まない**。LLM 未設定・降格・障害でもリマインダー発火・ToDo 管理・カレンダー通知・状況発話は動く（spec §4.2.1 不変条件）。|
| **キャラクターが配達する** | 通知・催促は独立トーストではなく**ゴースト発話が主経路**（§3 通知配達サービス）。二体構成では**サブが通知・催促・進捗係**、メインが受付・応答。サブ無しゴーストはメインが兼務（spec §4.1.1 縮退規則）。|
| **邪魔をしない** | すべての自発発話（状況発話・通知・催促・独り言・idle）は**単一の発話ガバナンスゲート**（§4）を通す。ユーザー起点の応答はゲートしない。|

### 1.1 スコープの境界（過剰設計の非採用）

spec §4.6 で明示された非採用を設計でも固定する:
- ToDo にサブタスク階層・プロジェクト・タグ・工数管理を持たせない（§4.6.2）。
- カレンダーは **読み取り専用**。書き込み・双方向同期・OAuth は持たない（§4.6.4、将来課題 §6.6）。
- 状況検知は**ローカル完結**。ウインドウ内容・キー入力内容・スクリーン内容は読まない（§4.6.3）。

---

## 2. 共通基盤: DB スキーマ

現行は schema v5（`app_settings / chat_log / user_profile / api_usage / voice_refs / interest_topics / topics_cache / reminders`）。マイグレーションは `db.rs::migrate()` の `if current < N { DDL; db_schema_version=N }` を末尾に追加する逐次パターンに従う。**Tier S で v6→v8 を段階追加**（v6=reminders 拡張＋reminder_log / v7=todos / v8=calendar_cache。機能マイルストーンと対応）。ガバナンス状態は DB 化せずインメモリ（§2.4）。時刻はすべて UTC Unix 秒で保存する（TZ 契約 §2.5）。

### 2.1 v6: reminders 拡張 + reminder_log 新設（§4.6.1）

spec §4.6.1 は「完了 / 未完了の管理」「通知履歴」「未完了の再通知判断」を要求する。**発火（通知した）と完了（ユーザーが済ませた）は別状態**であり、繰り返しでは「次回予定」と「今回分の履歴」を同一行では表現できない（外部レビュー指摘）。よって **reminders＝スケジュール定義、reminder_log＝発火・確認の履歴** に分離する。

```sql
-- v6: 既存 reminders(id, due_ts, text, created_ts) を拡張
ALTER TABLE reminders ADD COLUMN kind         TEXT    NOT NULL DEFAULT 'once';  -- 'once'|'daily'|'weekly'
ALTER TABLE reminders ADD COLUMN weekday_mask INTEGER NOT NULL DEFAULT 0;       -- weekly: bit0=月..bit6=日
ALTER TABLE reminders ADD COLUMN time_of_day  INTEGER NOT NULL DEFAULT 0;       -- daily/weekly: ローカル 0:00 からの秒（§2.5）
ALTER TABLE reminders ADD COLUMN active       INTEGER NOT NULL DEFAULT 1;       -- 1=有効, 0=停止（once 発火済み/完了/dismiss で 0）
ALTER TABLE reminders ADD COLUMN base_due_ts  INTEGER;                          -- スヌーズ前の本来時刻（NULL=なし）
-- idx_reminders_due(due_ts) は既存。due_ts は「次回発火予定」（UTC 秒）

-- v6: 発火・確認の履歴（完了/未完了・通知履歴・再通知判断の根拠）
CREATE TABLE reminder_log (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  reminder_id INTEGER NOT NULL,
  fired_ts    INTEGER NOT NULL,                  -- 発火（配達試行）時刻 UTC 秒
  ack         TEXT    NOT NULL DEFAULT 'fired',  -- 'fired'|'completed'|'dismissed'
  ack_ts      INTEGER,
  delivery    TEXT    NOT NULL DEFAULT 'ghost'   -- 配達結果（§3.1 DeliveryOutcome）: 'ghost'|'toast'|'deferred'|'failed'
);
CREATE INDEX idx_reminder_log_rid ON reminder_log(reminder_id);
```

**状態遷移（発火 ≠ 完了）**:
- **発火（watcher §7.1）**: `reminder_log` に 1 行 INSERT（ack='fired', delivery=結果）。**reminders 側を完了扱いにしない**。`once` は `active=0`（再発火停止。ただし「未完了」のまま）。繰り返しは `active=1` のまま次回 due へ reschedule。
- **ユーザー操作**: 「完了」→ 当該 reminder の最新 fired ログを ack='completed'。「無視」→ ack='dismissed'。
- **未完了 = ack='fired' のログが残っている**（通知したが未処理）。これが「未完了の再通知判断」（§7.1）と一覧の未完表示の根拠。
- **スヌーズ**: `due_ts=now+snooze`, `base_due_ts=元due_ts`, `active=1`。ログは残す。
- **保持**: `reminder_log` は既定 500 行で古い順 prune。`once` で active=0 かつ全ログ completed/dismissed の reminders は一定期間後に物理削除可（上限・期間は §11）。

新規 Db メソッド: `insert_reminder_ex / list_reminders(filter) / due_active_reminders(now) / log_fire(reminder_id, fired_ts, delivery) / set_ack(reminder_id, ack, ack_ts) / reschedule_reminder(id, next_due) / snooze_reminder(id, base, new_due) / deactivate_reminder(id) / list_reminder_log(reminder_id) / prune_reminder_log(keep)`。`ReminderRow`（新列）・`ReminderLogRow` を追加（ともに `Serialize`）。

### 2.2 v7: todos 新設（§4.6.2）

```sql
-- v7
CREATE TABLE todos (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  text       TEXT    NOT NULL,
  bucket     TEXT    NOT NULL DEFAULT 'today',   -- 'today'|'week'|'someday'
  priority   INTEGER NOT NULL DEFAULT 0,         -- 0=普通, 1=高
  recurring  TEXT,                               -- NULL|'daily'|'weekly'（日課）
  status     TEXT    NOT NULL DEFAULT 'open',     -- 'open'|'done'
  done_ts    INTEGER,
  created_ts INTEGER NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_todos_status ON todos(status, bucket);
```
**日課の復活**: `recurring` が非 NULL の todo を `done` にした場合、日次/週次の境界を跨いだ最初の起動または日付変更時に `status='open'`, `done_ts=NULL` へ戻す（`todos_daily_reset` 処理、§4.6.2 ロジック）。Db メソッド: `insert_todo / list_todos(bucket?) / set_todo_status / delete_todo / update_todo / reset_recurring_todos(now)`。

### 2.3 v8: calendar_cache 新設（§4.6.4）

ICS の繰り返し予定は複数の発生回が同一 UID を共有するため、**UID 単独主キーだと発生回が相互上書きされる**（外部レビュー指摘）。複数 ICS ソース（ファイル/URL、§5）間の UID 衝突もある。よって **(source_id, uid, start_ts) の複合キー**にし、発生インスタンス単位で 1 行持つ:

```sql
-- v8
CREATE TABLE calendar_cache (
  source_id     INTEGER NOT NULL,       -- ICS ソース識別（§5 calendar_sources の index）
  uid           TEXT    NOT NULL,       -- ICS UID（無ければ summary+start のハッシュ）
  recurrence_id TEXT,                   -- RECURRENCE-ID（繰り返しの個別回。単発は NULL）
  summary       TEXT    NOT NULL,
  start_ts      INTEGER NOT NULL,       -- UTC 秒（§2.5 で TZ 解決済み）
  end_ts        INTEGER,
  all_day       INTEGER NOT NULL DEFAULT 0,
  status        TEXT    NOT NULL DEFAULT 'confirmed', -- 'confirmed'|'cancelled'（CANCELLED/EXDATE 由来）
  notify_key    TEXT    NOT NULL,       -- summary|start|end のハッシュ。notified 差分検知用
  notified      INTEGER NOT NULL DEFAULT 0,
  fetched_ts    INTEGER NOT NULL,
  PRIMARY KEY (source_id, uid, start_ts)
);
CREATE INDEX idx_calendar_start ON calendar_cache(start_ts);
```

- **notified の差分規則（外部レビュー指摘）**: 取得時 UPSERT で、既存行と `notify_key`（summary/start/end のハッシュ）が**一致すれば `notified` を保持**、**変われば `notified=0` にリセット**（変更後の予定を再通知）。毎回 0 に戻さない（再通知の暴発防止）。
- **RRULE の展開**: パーサが表示窓＋通知窓（今日〜N 日、既定 7 日）**だけ near-term 展開**して発生行を作る（§7.4）。`EXDATE`/`RECURRENCE-ID`/`CANCELLED` を反映（除外回は行を作らない or status='cancelled'）。**対応できない RRULE は黙って欠落させず、その予定を「繰り返し（未対応）」として当日分だけ表示**し UI に印を付ける（§7.4）。
- prune: `end_ts < now - 1日` の過去発生行を削除。ソース削除時は該当 `source_id` を全削除。

### 2.4（撤回）状況発話ログ `speech_log` は作らない

初版は自発発話履歴テーブル `speech_log`（v9）を置いたが、reviewer 反証（YAGNI）を受け**撤回**する。M7 で必要な「発話の最低間隔・直近カテゴリ」判定は **`AppState` のインメモリ Atomic**（`GovernanceState`、§4.2）で足り、DB テーブルは不要（CLAUDE.md「将来のためを入れない」／v0.0.3 の「DB テーブル増で見通し悪化」の教訓）。連投回避の**履歴永続化**や「当日利用の要約（advanced）」が実需になった時点（M9 以降）で永続化を再検討する（§11）。**DB は v6〜v8 の 3 段のみ**にする。

### 2.5 時刻・タイムゾーン契約（骨格。実装 PR に先送りしない）

パーサは `NaiveDateTime`、DB は整数、ICS は多形式を取り得るため、変換規則を設計で固定する（外部レビュー指摘: これは「詳細」ではなく DB/パーサ/watcher に一斉に効く契約）:
- **DB は常に UTC Unix 秒で保存**（reminders.due_ts / reminder_log / calendar_cache.start_ts 等すべて）。
- **表示・判定・繰り返しの基準は OS ローカル TZ**（`chrono::Local`）。「毎日 9:00」「明日の朝」等はローカル TZ で解釈し、保存時に UTC 化する。
- **OS TZ 変更**: 繰り返しの `time_of_day`（ローカル 0:00 からの秒）はローカル基準で保持し、次回 due 計算のたびに現在のローカル TZ で UTC 化する（TZ 変更後は新 TZ の同じ壁時計時刻で鳴る）。
- **DST**: 日本ローカルには無いが ICS 外部予定にはあり得る。存在しないローカル時刻は繰り上げ（+1h）、重複する時刻は先（早い方）を採用。
- **ICS の時刻形式**: `TZID=...`（指定 TZ→UTC）、末尾 `Z`（UTC）、浮動時刻（TZ なし→ローカル TZ 扱い）、`VALUE=DATE`（終日→その日のローカル 0:00 を start）。
- **終日予定の通知時刻**: その日のローカル 8:00（設定の「朝」既定に従う、§7.1 の時間帯マッピングと共有）。

---

## 3. 共通基盤: 通知配達サービス

### 3.1 現状と方針

バック起点発話の唯一の共通出口は `dialogue::persist_and_speak(app, state, &DialogueResponse)`（chat_log 追記 + `dialogue` イベント emit → フロント `ghost-speech.ts` が受信）。現行リマインダー `fire_reminder` は辞書を使わず `DialogueResponse` を直書きしている（`system_messages.reminder_fired` は未使用）。

**設計**: Tier S の通知は**辞書 events を主経路**にし、キャラクター性・サブ主体・ゴースト差し替えを効かせる。低レベルは既存 `low::event` + `persist_and_speak` を再利用し、その上に薄い配達ヘルパーを新設する:

```rust
// system/deliver.rs（新設）
pub enum DeliveryOutcome { Ghost, Toast, Deferred, Failed }  // 到達結果（§7.1 期限超過回収が参照）

/// 通知配達の単一経路。**ガバナンス判定を呼ぶのはここだけ**（呼び出し側は
/// gate を呼ばず category/priority を渡す。二重ゲート禁止）。
pub async fn deliver_event(
    app: &AppHandle, state: &Arc<AppState>,
    category: SpeechCategory, priority: Priority, key: &str,
    placeholders: &[(&str, &str)], fallback: Option<String>,
) -> DeliveryOutcome;
```
手順（**配達は単一クリティカルセクションで直列化**し、複数 watcher の同時発話・二重発話を防ぐ。既存 `dialogue.busy` セマフォ or deliver 専用 Mutex を用いる）:
1. `governance::can_deliver(state, category, priority)` を **1 回だけ**評価（§4.2、純粋判定・副作用なし）。抑制なら `Deferred` を返す。
2. 辞書ヒット時: `low::event` 相当で `DialogueResponse` を作り `{...}` 置換 → `persist_and_speak`。成功なら `governance::record_delivered(state, category, now)` → `Ghost`。`persist_and_speak` がエラーなら 3 のフォールバックへ。
3. 辞書未ヒット or 発話失敗時: `fallback` があれば `system-toast` emit → `record_delivered` → `Toast`。無ければ `Failed`（record しない）。
- **到達保証（外部レビュー指摘）**: gate 通過は「抑制されない」だけで「届いた」保証ではない。実到達は `DeliveryOutcome` で表す。Notice の呼び出し側（リマインダー §7.1）は `Ghost|Toast` 以外（`Deferred|Failed`）を**未達**として扱い、起動時回収・再試行につなぐ。
- **record のタイミング**: 最終発話時刻の更新（間隔会計）は `Ghost|Toast` を返す時だけ。`Deferred|Failed` では更新しない（空振りで間隔が狂わない）。
- **サブ主体**: 辞書 Line は `main` 必須・`sub` 任意（既存構造）。通知系キーは「main が短く受け、sub が本体」を推奨運用としオーサリングガイドに記す（構造変更は不要）。

### 3.2 発話失敗時のフォールバック（spec §3.1 の二段構え）

`persist_and_speak`（dialogue emit）自体の失敗、または Notice の未達時:
- 発話経路失敗 → `system-toast` を試す（`Toast`）。トーストも失敗 → `Failed`。
- **フロントの受信 ack は取らない**（双方向確認は過剰、本 v 非採用）。代わりに Notice の未達は呼び出し側の**起動時回収**（§7.1）で救う: リマインダーは永続なので、アプリ停止中に過ぎた期限や `Deferred|Failed` を次回起動時／次ポーリングで拾い直せる。

### 3.3 プレースホルダ規約

辞書テキストに `{body}`（リマインダー/ToDo 本文）・`{count}`（件数）・`{time}`（時刻）・`{summary}`（予定名）を許可。`deliver_event` が安全に置換（未知プレースホルダは残さず空へ）。

---

## 4. 共通基盤: 発話ガバナンス

### 4.1 現状の問題

静音判定 `presence::quiet::should_stay_quiet`（OR: ポモドーロ集中 / 読み上げ中 / quiet_mode / フルスクリーン自動静音）は**呼び出し側が各自呼ぶ分散方式**で、`spawn_random_talk` と `spawn_idle_watcher` の 2 箇所しか呼んでいない（reminder・poke・notify では未チェック）。Tier S で自発発話が増えると、呼び忘れが静音破りに直結する。

### 4.2 設計: 単一ゲート

**すべての自発発話は `deliver.rs` 経由で `gate` を 1 回通る**（§3.1、二重ゲート禁止）。ユーザー起点の応答（`send_user_message` の戻り値経路）は通さない。

```rust
// system/governance.rs（新設）
pub enum SpeechCategory {
    Monologue, Idle,                       // 既存の自発発話（gate へ集約）
    Reminder, Todo, Calendar,              // 通知系
    SituationBreak, SituationLateNight, SituationBattery, SituationTodoFollow, // §4.6.3
}
pub enum Priority { Notice, Ambient }      // Notice=登録/予定した「必ず届く」通知、Ambient=気配り系

/// 純粋判定（副作用なし）。発話してよいかだけを返す。
pub fn can_deliver(state: &Arc<AppState>, cat: SpeechCategory, prio: Priority) -> bool;
/// 実際に配達した後に呼ぶ（最終発話時刻・カテゴリ別時刻を更新）。deliver_event のみが呼ぶ。
pub fn record_delivered(state: &Arc<AppState>, cat: SpeechCategory, at_ts: i64);
```

**判定と記録を分離（外部レビュー指摘: gate の副作用契約の内部矛盾を解消）**: `can_deliver` は副作用ゼロの純粋判定。実際に発話できたか（辞書ヒット・配達成功）は判定時点で不明なため、記録は配達成功後に `record_delivered` で行う。両者は `deliver_event`（§3.1）が**直列化されたクリティカルセクション内で check→deliver→record** を実行することで、複数 watcher の check-then-act 競合と二重発話を防ぐ（予約トークン/ロールバックは過剰につき非採用。単一ディスパッチの直列化で足りる）。

**状態はインメモリ**（`speech_log` は撤回、§2.4）: `AppState` に `GovernanceState { last_spoke: AtomicI64, last_by_category: [AtomicI64; N] }` を持ち、連投回避が実需化するまで永続化しない。

**Mutex 自己再ロック回避（本プロジェクト実績の罠、db.rs の警告参照）**: `gate` は冒頭で **settings を 1 回スナップショット**（`state.settings.lock().clone()` して即 drop）し、以降はコピーで判定する。`should_stay_quiet` も内部で settings をロックするため、**settings ロックを保持したまま呼ばない**。

**判定表（優先度 × 各段の可否）** — Notice は「必ず届く」を保証し全段を越える。Ambient は全段を適用:

| 段 | 条件 | Notice | Ambient |
|---|---|---|---|
| 1 ハード静音 | `should_stay_quiet`（集中/読み上げ/quiet_mode/フルスクリーン） | 通す | ブロック |
| 2 夜間静音 | `night_quiet` 帯内 | 通す | ブロック |
| 3 カテゴリ OFF | 当該カテゴリ設定が OFF | 免除（通す） | ブロック |
| 4 最低間隔 | 直近発話から `min_speak_interval` 未満 | 免除 | ブロック（下記の適用範囲に注意） |
| 5 連投回避 | 同カテゴリが直近ウィンドウで規定回数超 | 免除 | ブロック |

- **Notice が全段を越える根拠**: spec §4.6.1「静音中も鳴る／必ず届く」。同時刻に複数登録した別リマインダー（例 9:00 に服薬・出発・餌やり）を連投で落とさないため、段 5 も Notice は免除する。
- **`min_speak_interval` の適用範囲（reviewer 指摘の訂正）**: 段 4/5 は **状況発話系（Situation*）Ambient にのみ**適用する。`Monologue`/`Idle` は**既存の間隔（`monologue_interval_min` 既定 10 分・idle 30 分）を維持**し、`min_speak_interval` で上書きしない（初版の「monologue にも乗る・挙動は等価」は撤回。ユーザーの monologue 間隔設定を殺さないため）。

**移行方針**: 既存 `spawn_random_talk`（Monologue）・`spawn_idle_watcher`（Idle）を `should_stay_quiet` 直呼びから `deliver`/`can_deliver` 経由へ差し替える。**変わるのはハード静音＋夜間静音が乗る点のみ**（間隔は既存の `monologue_interval_min`/idle を維持）。M7 で導入し既存 2 経路を接続、以降の Tier S 発話は最初からゲート下。

### 4.3 「邪魔だった」フィードバック（spec §4.6.3 必須要件）

- 発話中バルーンに小さな「🔕」アフォーダンスを出し、クリックで「今のカテゴリは邪魔」を送る。**右クリックは使わない**（右クリック＝バルーンメニュー §4.3.5 と競合するため）。
- 受信で当該カテゴリの実効頻度を下げる（spec「頻度が自動で下がる」の**最小実装**）: そのカテゴリの実効 `min_speak_interval` を 1 段延長する係数を `app_settings`（`governance_backoff:<category>`）に保存。数回で実質 OFF 相当に達したら設定のカテゴリトグルも OFF にする。**指数的学習など凝った適応はしない**（spec 超過の回避）。
- **フロント契約（外部レビュー指摘）**: 🔕 を正しいカテゴリ・正しい発話に適用するため、バック起点発話の `dialogue` イベント payload（`DialogueResponse`）に **`speech_id`（発話ごとの一意 id）・`category`・`priority`・`feedback_allowed`** を追加する。フロントは表示中発話の `speech_id`+`category` を保持し、🔕 クリックで `feedback_speech(speech_id, category)` を送る（**古い発話への誤適用を `speech_id` で防止**）。`feedback_allowed=false`（ユーザー応答など）では 🔕 を出さない。ユーザー起点の応答（`send_user_message` 戻り値）にはこれらを付けない。
- コマンド `feedback_speech(speech_id: String, category: String)`。

---

## 5. 共通基盤: 設定フィールド（Settings 拡張）

`Settings`（`state.rs`）に追加。既存の `clamp()` に範囲丸めを足す。**リマインダー/ToDo/カレンダー表示は `tools_enabled` から独立**（§4.2.1 不変条件）。`tools_enabled` は「クリップボード 📋 + advanced 時刻注入」用途に縮小（§4.5.3 移行）。

```rust
// 追加フィールド（既定値）
daily_support_enabled: bool,          // true（Tier S マスタスイッチ。v0.2 の目玉のため既定 ON。個別カテゴリは既定 OFF）
// --- 発話ガバナンス ---
situation_break_enabled: bool,        // false
situation_late_night_enabled: bool,   // false
situation_battery_enabled: bool,      // false
todo_follow_enabled: bool,            // false（未完了フォロー発話）
min_speak_interval_min: u32,          // 30（状況発話系 Ambient の最低間隔。§4.2）
night_quiet_enabled: bool,            // false（夜間静音の有効化。番兵値を使わず独立フラグに。外部レビュー指摘）
night_quiet_from: u16,                // 1380 = 23:00（0:00 からの純粋な分。0 も有効値=0:00）
night_quiet_to: u16,                  // 420 = 07:00（from>to は日跨ぎ。from==to は「終日」と定義）
// --- カレンダー ---
calendar_sources: Vec<CalendarSource>,// 空（既定オフ、spec §3.3）。CalendarSource = File{path}|Url{url}（§7.4）
calendar_notify_min: u32,             // 15（開始前通知）
// --- リマインダー ---
reminder_notify_enabled: bool,        // true（発火通知の on/off。登録自体は常時）
```
`night_quiet_enabled=true` のときのみ `from..to`（日跨ぎ可・`from==to` は終日）で判定する。時刻値は番兵にせず純粋な分（0=0:00 も有効）。設定 UI は §6 の新ページ「日常支援」。

---

## 6. 共通基盤: UI（パネルとページ）

既存の静的配置 + `.solid .panel` + `mount*` + `.visible` トグル方式を踏襲（ポモドーロパネルが雛形）。

- **リマインダー & ToDo パネル**: index.html に `#daily-panel`（タブ or 2 セクション: リマインダー / ToDo）を静的配置。一覧・追加・完了・削除・スヌーズを提供。登録の主経路は従来どおりチャット自然文で、パネルは確認・編集用。
- **設定「日常支援」ページ**: `settings-page-selector` に `daily` を 1 つ追加（`data-page="daily"`）。ガバナンス各トグル・夜間静音時刻・最低間隔・カレンダー URL/通知分・リマインダー通知 on/off を配置。既存 `data-active-page` + CSS ルールに `daily` を 1 行追加するだけ（settings.ts）。
- 右クリックメニュー（バルーンメニュー §4.3.5）に「予定・ToDo」項目を 1 つ追加し `#daily-panel` を開く。
- バッジ: 未完了 ToDo 件数の常時表示は**任意**（既定 OFF、`#pomodoro-badge` と同型の小バッジ）。過剰なら次段階送り。

---

## 7. 機能別詳細設計

### 7.1 §4.6.1 統合リマインダー（最優先・M7）

#### パーサ（`tools/reminder.rs` 拡張）
現行 `parse_request -> Option<ReminderRequest{offset_secs, body}>` に加え、**構造化リクエスト**を返す新関数:
```rust
pub enum Schedule {
    Offset { secs: i64 },                                   // 既存「N分後」
    AtTime { at_ts: i64 },                                  // 絶対時刻・日付（今日/明日の HH:MM、MM-DD）
    Daily  { time_of_day: i32 },                            // 毎日 HH:MM
    Weekly { weekday_mask: u8, time_of_day: i32 },          // 毎週X曜 HH:MM
}
pub struct ParsedReminder { schedule: Schedule, body: String }
pub fn parse_reminder(text: &str, now: NaiveDateTime) -> Option<ParsedReminder>;
```
対応語彙（決定論・low、確定は実装 PR）:
- 相対: 「N分後/時間後/秒後」（既存踏襲）。
- 絶対時刻: 「HH時」「HH時MM分」「HH:MM」「18時に」→ 今日のその時刻、過ぎていれば翌日。
- 相対日 + 時刻: 「明日」「今日」「明後日」＋時刻、「明日の朝/昼/夕方/夜」→ 時間帯マッピング（朝=8:00 等、設定既定）。
- 日付: 「M月D日」「MM/DD」＋時刻。
- 曜日/繰り返し: 「毎日」「毎朝」「毎週月曜」「月・水・金」→ Daily/Weekly。
- 全角数字の正規化を前段に追加（現行は半角のみ）。
- **曜日条件は辞書 `WhenExpr` ではなくリマインダー側 `weekday_mask` で完結**（辞書 when の曜日欠如 §9 は本機能に影響しない）。

#### 会話経路
`dialogue::run_dispatch` 冒頭の tools 分岐（現行 `tools_enabled && parse_request`）を、`daily_support_enabled && parse_reminder(...)` に置換・拡張。マッチで `handle_reminder_request` が `Schedule` から `due_ts`（or 繰り返しメタ）を計算し `insert_reminder_ex` → 確認文（「毎週月曜 9 時に『X』を覚えておくね」等）を LLM なしで返す。**advanced ゲートは撤廃**（§4.6.1、常時ローカル）。曖昧文（「夕方になったら薬局」）は low パーサが拾えなければ、advanced 時のみ LLM に「予定抽出」を促し提案する（上乗せ、Phase 2 で LLM プロンプト設計）。

#### watcher（`tasks.rs` 拡張）
`spawn_reminder_watcher`（10 秒間隔）の `fire_reminder` を改修（**発火 ≠ 完了**、§2.1）:
1. `due_active_reminders(now)`（active=1 AND due_ts<=now）を取得。
2. 各件 `deliver_event(Reminder, Priority::Notice, "reminder_fired", [("body", &r.text)], fallback=Some("リマインダー: …"))`。**gate は deliver 内で 1 回のみ**（直接 `can_deliver` は呼ばない）。Notice は §4.2 表どおりハード静音・カテゴリ OFF・連投を越える。
3. `log_fire(id, now, outcome)` で発火履歴を記録（outcome=Ghost/Toast/Deferred/Failed）。**未達（Deferred|Failed）は active を維持し次ポーリングで再試行**（Deferred はハード静音明け等で自然に再送）。
4. 到達（Ghost|Toast）したら: `kind=='once'` → `deactivate_reminder(id)`（active=0＝再発火停止。**完了ではなく「発火済み・未完了」**）。繰り返し → `reschedule_reminder(id, next_due)`（§2.5 で TZ 解決）。
5. **起動時回収（外部レビュー指摘）**: 起動直後に一度 `due_active_reminders(now)` を実行し、アプリ停止中に過ぎた期限を拾う。過去分が複数溜まっている場合は**直近 1 件に集約して通知**（大量の連続発話を避ける。集約 or 個別は §11）。
6. 定期 prune。

#### コマンド（`commands/tools.rs` or 新 `commands/daily.rs`）
```
add_reminder_nl(text)            -> ReminderEntry[]   // 自然文（パネルからも登録可）
list_reminders(filter)           -> ReminderEntry[]   // filter: active|completed|all
complete_reminder(id)            -> ReminderEntry[]   // 最新 fired ログを ack='completed'、once は active=0
dismiss_reminder(id)             -> ReminderEntry[]   // ack='dismissed'
snooze_reminder(id, mins)        -> ReminderEntry[]
delete_reminder(id)              -> ReminderEntry[]
update_reminder(id, patch)       -> ReminderEntry[]   // 時刻/本文/繰り返し編集
get_reminder_log(id)             -> ReminderLogRow[]  // 通知履歴
```
既存 `add_reminder(text, offset_secs)` は**内部 API として残す**（パネル互換）。会話経路は `parse_reminder` へ一本化。イベント `reminders-changed` を emit しパネルが再取得。

#### 辞書
`events` に `reminder_fired`（sub 主体・`{body}` 使用）・`reminder_snoozed` を **default 辞書に新設**（現行未定義 §9 を解消）。

#### UI
`#daily-panel` のリマインダー節: 一覧（時刻・本文・繰り返しアイコン・**未完了/完了バッジ**）、完了/スヌーズ/削除ボタン、通知履歴、自然文追加欄。**未完了 = ack='fired' の残**（§2.1）。

---

### 7.2 §4.6.2 ToDo・日課管理（M8）

- **DB**: v7 `todos`。**コマンド**: `add_todo(text, bucket, priority, recurring) / list_todos(bucket?) / complete_todo(id) / delete_todo(id) / update_todo(id, patch)`、イベント `todos-changed`。
- **日課復活**: 起動時と日付変更検知時に `reset_recurring_todos(now)`（daily=毎日 0:00 境界、weekly=月曜 0:00 境界で open へ戻す）。日付変更検知は既存 watcher にピギーバック（前回チェック日を app_settings に保持）。
- **キャラ関与（通知配達 §3 経由・ガバナンス §4 下）**:
  - 朝（初回起動 or 朝の時間帯の最初のアイドル明け）: `deliver_event(Todo, "todo_morning", [("count", n)])` 「今日は n 件あるよ」。
  - 長時間未完了: `todo_follow_enabled` 時、`SituationTodoFollow` カテゴリで**責めない**催促（辞書側の言い回しで担保）。
  - 完了時: `complete_todo` 内で `deliver_event(Todo, "todo_done", ...)` 労い。
  - 複数日滞留: 再整理提案（advanced 上乗せ、low は定型)。
  - **終了前の確認**（spec §4.6.2「起動時・終了前の確認」の後半）: **M9 で扱う**（2026-07-17 裁定）。起動時=朝の件数告知は M8 実装済み。終了前は quit 経路で未完了があれば一言確認する想定（詳細は M9 実装時に確定）。
- **advanced 上乗せ**: 会話からの ToDo 抽出提案・分割・優先順位提案・「今から何を」応答（LLM、基盤の open/done は low）。
- **辞書**: events `todo_morning / todo_follow / todo_done / todo_stale` を新設（サブ主体）。
- **UI**: `#daily-panel` の ToDo 節（今日/今週/いつかの 3 タブ、チェックで完了、優先度トグル）。

---

### 7.3 §4.6.3 状況対応型自発発話 + 発話ガバナンス（M9）

ガバナンス基盤は §4 で M7 に前倒し導入済み。M9 では**状況検知ソースの新設**とカテゴリ実装を行う。

#### 状況検知モジュール（`presence/context.rs` 新設・`#[cfg(windows)]`）
現行に無い OS レベル検知を追加（windows crate に feature 追加が必要）:
```rust
pub fn os_idle_secs() -> u64;          // GetLastInputInfo（OS 全体の無操作秒）
pub fn continuous_use_secs(state) -> u64; // 「最後に os_idle が閾値超え→今」までの連続利用。context.rs が計測
pub fn battery() -> Option<BatteryInfo>;  // GetSystemPowerStatus（%、AC接続）
pub fn system_muted() -> Option<bool>;    // IAudioEndpointVolume（音量/ミュート、任意・後回し可）
```
- **連続利用時間**: `spawn_context_watcher`（新規、60 秒間隔）が `os_idle_secs()` を監視し、アイドル閾値（例 5 分）を跨いだらセッション境界としてリセット、それ以外は加算。値は `AppState` の新フィールドに保持。
- 既存の擬似アイドル（idle 反応 §4.4.3）は **`last_interaction`（`DialogueState`。更新元はチャット送信のみで、poke/nade でも未更新）** を使う。状況発話はこれと**別系統**にし、OS アイドル（`os_idle_secs` = GetLastInputInfo）を用いる。idle 反応 §4.4.3 はそのまま。

#### 発話カテゴリ（`spawn_context_watcher` が判定 → `gate` → `deliver_event`）
| カテゴリ | 条件（low・ローカル） | 既定 | 発話キー |
|---|---|---|---|
| 休憩促し | 連続利用 ≥ 閾値（例 90 分）かつ非フルスクリーン | OFF | `situation_break` |
| 深夜利用 | `night_quiet` 帯 or 深夜時刻に一定時間利用 | OFF | `situation_late_night` |
| バッテリー低下 | `battery.percent ≤ 15` かつ非 AC、1 回のみ | OFF | `situation_battery` |
| 未完了フォロー | ToDo/リマインダーの未完 + 長時間経過 | OFF | `situation_todo_follow` |
- 「提案 #10 PC 利用時間・休憩支援」は独立機能とせず本カテゴリに吸収（spec §4.6.3）。
- 動画/ゲーム中の抑制は `should_stay_quiet` のフルスクリーン判定で既にカバー（`gate` 内）。
- **advanced 上乗せ**: 直近会話・作業傾向を考慮した発話生成・言い回し（LLM）。ただし**発話可否の最終ゲートは常に low の `gate`**（LLM がガバナンスを越えて喋らせない、§4.6.3）。

#### 辞書
events `situation_break / situation_late_night / situation_battery / situation_todo_follow` を新設（サブ主体、控えめな言い回し）。

---

### 7.4 §4.6.4 カレンダー参照（読み取り専用・M10）

- **入力（ファイル / URL 両対応、外部レビュー指摘: spec §4.6.4 は「ファイル/URL」を要求）**: `calendar_sources: Vec<CalendarSource>`（§5）。
  ```rust
  enum CalendarSource { File { path: PathBuf }, Url { url: String } }
  ```
  File は設定パネルのファイル選択（tauri dialog）で追加。更新は `fetched_ts` より新しい mtime を検知して再読込。Url は HTTP GET。取得失敗時は既存キャッシュを表示（オフライン動作 §4.6.4）。
- **取得**: `system/calendar.rs` 新設。`VEVENT` の `SUMMARY/DTSTART/DTEND/UID/RRULE/EXDATE/RECURRENCE-ID/STATUS` をパース（軽量自前 or crate）。時刻は §2.5 の TZ 契約で UTC 化。
- **RRULE 展開（near-term のみ）**: 表示窓（今日〜7 日、既定）＋通知窓ぶんだけ発生インスタンスに展開して `calendar_cache` へ UPSERT（複合キー §2.3）。`EXDATE`/`RECURRENCE-ID`/`CANCELLED` を反映（除外回は行を作らない or status='cancelled'）。**対応できない RRULE は黙って落とさず**、その予定を「繰り返し（未対応）」として当日分のみ表示し UI に印を付ける。完全な RRULE 対応は将来（§11）。
- **キャッシュ/更新**: §2.3 の `notify_key` 差分規則で `notified` を保持/リセット。取得は `spawn_calendar_watcher`（既定 30〜60 分）＋手動 `refresh_calendar`。
- **通知**: watcher が `start_ts - calendar_notify_min*60 <= now < start_ts` かつ `notified=0` を `deliver_event(Calendar, Priority::Notice, "calendar_upcoming", [("summary", …), ("time", …)])` で配達（**gate は deliver 内で 1 回のみ。初版の直接 `gate()` 呼びは廃止**＝二重ゲート解消）。到達（Ghost|Toast）で `notified=1`。終日予定は §2.5 の「朝」時刻に通知。
- **プライバシー（spec §3.3）**: ソースはユーザー明示・**既定空**・外部送信は Url ソースの GET のみ。設定 UI に送信先を明示。
- **advanced 上乗せ**: 「今日の予定は？」「次の予定まで何分？」に応答（キャッシュ参照 + LLM 整形）。空き時間抽出・ToDo 割当は将来（§6.2 以降）。
- **辞書**: events `calendar_upcoming`。
- **当日イベント取得 API**（朝・夜定例 Tier A §6.2 の材料）は「当日分の取得」1 本に絞る（過度な汎用化はしない）。

---

## 8. 契約サマリ（architecture.md へ反映予定）

### 8.1 新規コマンド
| コマンド | 引数 | 戻り | 機能 |
|---|---|---|---|
| `add_reminder_nl` | `text` | `ReminderEntry[]` | 4.6.1 |
| `complete_reminder` / `dismiss_reminder` / `snooze_reminder` / `update_reminder` | `id`(+`mins`/`patch`) | `ReminderEntry[]` | 4.6.1 |
| `list_reminders`(filter) / `delete_reminder` / `get_reminder_log` | `filter` / `id` / `id` | `ReminderEntry[]` / `ReminderLogRow[]` | 4.6.1 |
| `add_todo` / `list_todos` / `complete_todo` / `reopen_todo` / `delete_todo` / `update_todo` | 各種 | `TodoEntry[]` | 4.6.2（reopen_todo は M8 実装で追加: パネルのチェック解除） |
| `get_calendar_events` / `refresh_calendar` / `add_calendar_source` / `remove_calendar_source` | `range?` / — / `source` / `id` | `CalendarEvent[]` / `CalendarSource[]` | 4.6.4 |
| `feedback_speech` | `speech_id, category` | `()` | 4.6.3 ガバナンス |

### 8.2 新規イベント / payload 拡張
- 新規: `reminders-changed` / `todos-changed` / `calendar-changed`（フロント再取得トリガ）。
- **既存 `dialogue` イベント payload（`DialogueResponse`）を拡張**（外部レビュー指摘、🔕 フィードバックのため）: バック起点発話に `speech_id` / `category` / `priority` / `feedback_allowed` を付与。ユーザー応答（`send_user_message` 戻り値）には付けない。TS 型 `DialogueResponse` も同期。

### 8.3 新規設定フィールド
§5 の一覧（ガバナンス: night_quiet_enabled + from/to + min_speak_interval + カテゴリ 4 / カレンダー: calendar_sources + notify_min / リマインダー: reminder_notify / マスタ: daily_support_enabled）。`clamp` に範囲追加、`types.ts` の `Settings` 同期。`CalendarSource` 型も TS と同期。

### 8.4 新規 DB テーブル/列
reminders 拡張 5 列 + `reminder_log`（v6）/ `todos`（v7）/ `calendar_cache`（複合キー、v8）。**ガバナンス状態はインメモリ**（`speech_log` は撤回、§2.4）。すべて UTC 秒（§2.5）。
- **3 表現の同期（reminders/todos/calendar とも）**: DB の `*Row` / コマンド層の `*Entry`（`commands/*.rs`）/ TS の `*Entry`（`types.ts`）の 3 つに同じフィールドを反映する。特に reminders の新列（kind/weekday_mask/time_of_day/active/base_due_ts）と `ReminderLogRow`（fired_ts/ack/delivery）を 3 表現へ揃えないと、パネルが繰り返し・完了状態・通知履歴を表示できない。

### 8.5 新規辞書キー（default 辞書に追加）
`reminder_fired, reminder_snoozed, todo_morning, todo_follow, todo_done, todo_stale, situation_break, situation_late_night, situation_battery, situation_todo_follow, calendar_upcoming`。すべてサブ主体推奨。

### 8.6 新規モジュール
`system/deliver.rs`（通知配達・DeliveryOutcome）/ `system/governance.rs`（can_deliver + record_delivered）/ `presence/context.rs`（OS 状況検知）/ `system/calendar.rs`（ICS 取得・パース・RRULE near-term 展開）/ `commands/daily.rs`（or tools.rs 拡張）/ フロント `panels/daily.ts`。windows crate に `Win32_System_Power`（バッテリー）・`Win32_UI_Input_KeyboardAndMouse`（GetLastInputInfo、既存 feature に含まれる可能性あり要確認）・音量用 `Win32_Media_Audio`（任意 §11）を追加。ICS ファイル選択に tauri dialog。

---

## 9. 既存資産との整合・注意（調査で判明した実態）

設計は**実装の実態**を正とする。architecture.md の以下は実装とずれており、Tier S 実装時に architecture.md 側を追随改訂する:
- DB 節に `monologue_cache`（未実装）が載り、`topics_cache`（実装済み）が無い。
- notify() の `severity`（Minor/Important/Critical）二段トーストは**未実装**（発話 or トーストの二択）。`CostWarning80.percent` も無い。
- `AppState` サブ状態の集約構造（architecture §3.2）は実態と別（`presence::window_pos` 等）。`PomodoroState.paused`・`PresenceState.reading` は実装のみ。

Tier S 実装で特に踏む既存の穴:
- `system_messages.reminder_fired / cost_warning_80` 等は辞書未定義で常時トースト fallback → **本設計で reminder_fired 等を default 辞書に追加**して解消。
- `WhenContext.recent_keys` は常に空で `NotInRecent` はデッド → ガバナンスの連投回避は**辞書 when に相乗りせず**、インメモリ `GovernanceState`（§4.2）で実装（永続化は M9 以降で判断、§2.4）。
- 静音ガードの一元化欠如 → **`governance::gate` に集約**（§4.2）。
- OS アイドル/連続利用/バッテリー/音量は未実装 → **`presence/context.rs` で新設**（§7.3）。
- `WhenExpr` に曜日なし → リマインダー繰り返しは reminders 側 `weekday_mask` で完結（辞書 when は拡張しない）。

---

## 10. 実装マイルストーン

現行 M0〜M6 完了・v0.1.4 リリース済み。Tier S は M7〜M10。各 M の完了条件は cargo check/test・tsc・実機確認・reviewer。

| M | 内容 | 主な成果物 |
|---|---|---|
| **M7** | 共通基盤 + 統合リマインダー（§4.6.1） | DB v6、`deliver.rs`、`governance.rs`（gate 導入 + 既存 monologue/idle を接続）、`parse_reminder` 拡張、watcher 改修、辞書 `reminder_*`、`#daily-panel` リマインダー節、設定「日常支援」ページ骨格 |
| **M8** | ToDo・日課管理（§4.6.2） | DB v7、todo コマンド、日課 reset、キャラ催促（deliver 経由）、辞書 `todo_*`、パネル ToDo 節 |
| **M9** | 状況発話 + 検知（§4.6.3） | `presence/context.rs`（OS アイドル/連続利用/バッテリー）、`spawn_context_watcher`、状況カテゴリ、ガバナンス設定 UI、辞書 `situation_*`、フィードバック導線、`todo_follow`/`todo_stale` の発火結線（§7.2）、終了前の ToDo 確認（§7.2、2026-07-17 裁定） |
| **M10** | カレンダー参照（§4.6.4） | DB v8、`calendar.rs`（ICS 取得/パース）、`spawn_calendar_watcher`、通知、設定 URL、辞書 `calendar_upcoming` |

M7 で「通知配達 + ガバナンス」という 2 つの横断基盤を先に立てるため、M8 以降は基盤の再利用で加速する。ガバナンスを M7 に前倒しするのは、リマインダーが増える時点で静音一元化が無いと退行リスクが高いため。

---

## 11. 未決事項

### 11.1 実装 PR で確定（設計の骨格に影響しない詳細）
1. リマインダー自然文パーサの**語彙・書式の網羅表** → **M7 で確定**: `tools/reminder.rs` のテストを正とする（時間帯 朝8:00/昼12:00/夕方17:00/夜・晩20:00、全角正規化、疑問文は拒否、「今日+過去時刻」はパース全体を拒否、繰り返しの時刻省略は朝 8:00）。時刻が明示されない弱いマッチと時刻のみマッチには**雑談語尾の否定リスト**（ね/かな/た/てる/くらい/状態形容詞 等）を適用し、平叙文の誤登録（→Notice として鳴り続ける幽霊リマインダー化）を抑える。形態素解析は持たないため完全ではない（reviewer 指摘を受けた精度優先のヒューリスティック）。
2. 状況検知の**閾値** → **M9 で確定**（`presence/context.rs` の定数を正とする）: アイドル境界 5 分（連続利用セッションのリセット）/ 休憩促し 90 分ごと（セッション内で繰り返し）/ 深夜帯 23:00-5:00 で連続 30 分利用・1 晩 1 回（0-5 時は前日夜として dedup）/ バッテリー 15% 以下かつ非 AC で 1 回（AC 接続 or 20% 超回復で解除のヒステリシス）/ ToDo フォロー 14-18 時・1 日 1 回 / ToDo 滞留 18-22 時・1 日 1 回・作成から 3 日以上。1 日 1 回系の dedup は app_settings（todo_morning_date と同型）。
3. Situation* の**連投ウィンドウ・回数** → **M9 で確定**: gate 段 5 = 同カテゴリ **120 分 × (1 + backoff)** の最短間隔（回数カウント方式ではなく間隔方式。検知側の 1 日 1 回制御の二重安全網）。🔕 バックオフは線形延長（指数学習はしない、§4.3 どおり）で **3 回でカテゴリトグル自体を OFF**（設定へ永続化 + settings-changed）。**backoff の回復**（M9 reviewer 指摘）: カテゴリトグルを OFF→ON に戻したとき `set_settings` が `governance_backoff:<category>` を 0 にリセットする。これが無いと 🔕 の恒久 throttle が再有効化後も残り、理由の見えないまま間隔が絞られる。
4. 音量/ミュート検知（`system_muted`）→ **M9 で見送り確定**（優先度低、無くても成立。実需が出たら再検討）。
5. ToDo 件数バッジの採否（既定 OFF、次段階送り可）。
6. 起動時回収の**集約 or 個別** → **M7 で確定**: 2 件以上は「『直近の本文』（ほか N-1 件）」の 1 発話に集約し、状態遷移は全件に適用。
7. reminder_log の**保持上限** → **M7 で確定**: 500 行（新しい順）。once 完了 reminders の物理削除は見送り（Completed フィルタで閲覧可、実害が出たら再検討）。
8. **M7 での追加確定**: watcher は未達（Deferred|Failed）をログに残さない（§7.1 の記述より狭い。10 秒間隔の再試行が reminder_log を押し流すため）。通知 OFF（`reminder_notify_enabled=false`）中は発火を保留し、ON に戻すと届く（期限は消えない）。
9. **M8 での追加確定**: 日課復活と朝告知は専用 `spawn_daily_watcher`（60 秒間隔）で行う（§7.2 の「既存 watcher にピギーバック」から変更 — リマインダー watcher は通知トグルでループを止めるため結合を避けた）。朝告知は 5:00-11:00・1 日 1 回（`todo_morning_date` を app_settings に永続化、再起動でも二重告知しない）・today バケットの件数のみ・0 件の朝は告知なしで消化。`todo_follow`/`todo_stale` は辞書キーのみ M8 で整備し、発火（長時間未完了・複数日滞留の判定）は M9 の状況検知と同時に結線する。パネルの ToDo 追加はアクティブなバケットへ priority=0・単発で入れ、優先度・日課は行内トグルで後付けする。
10. **spec §4.6.2「終了前の確認」の割当** → **M9 で扱う**（2026-07-17 ユーザー裁定。M8 reviewer が指摘した「要件と実装計画の未接続」を解消。§7.2・§10 M9 行に反映済み）。
11. **M9 での追加確定**:
    - **辞書キーの統合**: §7.3/§8.5 の `situation_todo_follow` キーは新設せず、M8 整備済みの `todo_follow`（未完了の思い出し）と `todo_stale`（滞留の再整理提案）に統合した（重複回避）。カテゴリはどちらも `SituationTodoFollow`（トグルは todo_follow_enabled）。
    - **終了前確認の実装形**: トレイの「終了」で未完了の today ToDo があれば `events.todo_quit`（{count}）を通常の `quit` の代わりに再生する（ユーザー起点なのでゲート非対象・ブロッキング確認ダイアログは置かない）。コンテキストメニューの「終了」は M3 判断（即 exit・挨拶なし）のまま変更しない。
    - **🔕 の適用範囲**: `feedback_allowed=true` は Situation* カテゴリのみ（§4.3 のバックオフ機構＝段 4/5 の適用対象と一致させる。Notice や独り言には「頻度を下げる」レバーが無いため）。speech_id は deliver の連番で、**最新のタグ付き発話と一致したときだけ**適用（バック側でも照合）。
    - **深夜利用と夜間静音の相互作用**: 深夜の声かけ（Ambient）は夜間静音帯では gate 段 2 にブロックされる。両方を同じ帯で有効にした場合は静音が優先（ガバナンスとして一貫。声かけが欲しい帯は夜間静音から外すこと）。
12. **M10 での追加確定**:
    - **ICS パーサは自前実装**（依存を増やさない）。行 unfolding → VEVENT ブロック → SUMMARY/DTSTART/DTEND/UID/RRULE/EXDATE/RECURRENCE-ID/STATUS。パース・展開・notify_key の網羅は `system/calendar.rs` のテストを正とする。
    - **TZ は日本前提の簡易解決**（§2.5 理想の near-term 実装）: 末尾 `Z`=UTC / `VALUE=DATE`=その日のローカル 0:00 / それ以外（TZID 付き・浮動時刻）=ローカル TZ 扱い。任意 TZID の VTIMEZONE 完全解決は将来（§11.2）。
    - **RRULE は near-term 展開のみ**: DAILY/WEEKLY を INTERVAL/BYDAY/UNTIL/COUNT/EXDATE 込みで、MONTHLY/YEARLY は同日ステップの best-effort。解釈不能な RRULE は**当日分だけ** `unsupported` 印つきで残す（schema に実装追加列 `unsupported`。設計 §2.3 の schema に対する追加）。
    - **表示窓は今日 0:00（ローカル）〜 now + 7 日**。取得は 30 分ごと + 手動 refresh。開始前通知は時刻付き=`start - calendar_notify_min*60`、終日=当日ローカル 8:00。
    - **ソース識別は index ベース**（`calendar_sources` の添字 = `source_id`）。追加/削除で index がずれるため、**ソース変更時はキャッシュを全 clear** して次回取得で作り直す（notified はその周期で失われるが、ソース編集は稀）。tauri のファイル選択ダイアログは見送り、設定パネルで**パス/URL を手入力**（spec §4.6.4「ファイル/URL の読み込み」は満たす。依存を増やさない判断）。
    - **M10 reviewer 反映**（2026-07-18）: ①終日予定の通知候補は「開始前」ではなく**当日中**（start+86400 > now。通知時刻 8:00 が start=0:00 より後のため、開始前条件だと一度も鳴らない）②URL 取得は 15 秒タイムアウト + `error_for_status` + `BEGIN:VCALENDAR` 検証（エラーページ応答でキャッシュを全消去しない = オフライン保証）③カレンダー watcher も他 Tier S watcher と同様 `daily_support_enabled` を機能スイッチとして確認④**RECURRENCE-ID 上書き回は親系列の展開から除外**（EXDATE と同扱い。移動の二重表示・取消の順序依存を解消）⑤YEARLY の 2/29 起点は月末丸めで前進。見送り: ソース編集と取得の TOCTOU 孤児行（稀・自己回復）、複数予定同時刻の通知集約（Notice の設計どおり個別配達）。

### 11.2 将来（M10 以降 or Tier A）
- **完全な RRULE 対応**（複雑な繰り返し・TZ 跨ぎ・例外の網羅）。本 v は near-term 展開＋未対応表示（§7.4）。
- カレンダー**書き込み・双方向同期**（spec §6.6、読み取り専用の外）。
- 連投回避・当日利用要約の**永続化**（実需化時、§2.4）。

### 11.3 本 v で確定済み（2 回の反証を受けた設計判断）
- **発火 ≠ 完了**: reminders(定義)＋reminder_log(発火/確認履歴)に分離。未完了=ack='fired' の残（§2.1）。
- **到達保証**: `DeliveryOutcome{Ghost|Toast|Deferred|Failed}`＋発話失敗トーストフォールバック＋起動時回収。フロント ack は非採用（§3.1/§3.2/§7.1）。
- **ガバナンス**: `can_deliver`(純粋判定)/`record_delivered`(記録) を分離し deliver 内で直列化。gate 可否表・`min_speak_interval` は Situation* のみ・Mutex スナップショット（§3.1/§4.2）。予約トークンは非採用。
- **カレンダー**: (source_id, uid, start_ts) 複合キー・notify_key 差分で notified 制御・RRULE near-term 展開・ファイル/URL 両対応（§2.3/§7.4）。
- **時刻**: DB=UTC 秒、判定/繰り返し=ローカル TZ、DST/ICS 各形式の規則を §2.5 に固定（実装 PR に先送りしない）。
- **`speech_log` テーブルは作らない**（インメモリ、§2.4）。
- **既定値**: `daily_support_enabled = true`（v0.2 の目玉）。状況発話カテゴリ・`night_quiet_enabled` は**既定 OFF** でオプトイン（spec §3.3・アップグレード時の無言化回避）。夜間静音は番兵値でなく独立フラグ（§5）。

---

## 12. 参照

- spec.md §4.6（要件の正本）・§4.2.1（モード境界・不変条件）・§4.4.8/9（静音）・§6.1（Tier S ロードマップ）
- architecture.md §2（DB）・§4（コマンド）・§5（イベント）・§6（辞書）・§11（notify）※実装との乖離は §9 参照
- 既存の類似設計: [text-reader-spec.md](text-reader-spec.md) / [script-reader-spec.md](script-reader-spec.md)

---

## 13. 改訂履歴

| 版 | 日付 | 内容 |
|---|---|---|
| v1 | 2026-07-12 | 初版。共通基盤（DB v6-v8 / deliver / governance / context）+ 4 機能 + 契約サマリ + M7-M10。社内 reviewer 反証で `speech_log` 撤回・gate 単一化・min_speak 適用範囲・Mutex 制約・既定値を反映。 |
| v2 | 2026-07-12 | 外部レビュー（重大 4 / 高 4 / 中 2）を反映。**発火 ≠ 完了**（reminders＋reminder_log 分離、§2.1）／**到達保証**（DeliveryOutcome＋フォールバック＋起動時回収、§3.1/§3.2/§7.1）／**gate を can_deliver＋record_delivered に分離**し直列化（§4.2）／**カレンダー複合キー・notify_key 差分・RRULE near-term 展開・ファイル/URL 両対応**（§2.3/§7.4、二重ゲート解消）／**時刻・TZ 契約を §2.5 に新設**／**🔕 のフロント payload 契約**（speech_id 等、§4.3）／**夜間静音を独立フラグ化**（§5）。過剰設計（フロント ack・予約トークン・完全 RRULE）は非採用として明記。 |
