# 基盤・完成度（v0.4）アーキテクチャ設計書

**対象**: spec.md §6.0 の v0.4 スコープ ＝ ① 表示モニタ選択（§4.1.6）② advanced 独り言の LLM 生成 + キャッシュ補充（§4.4.4）③ 時事ネタの織り込み（§4.4.6）④ 品質・負債返済
**位置付け**: spec（要件の正本）を実装可能な契約・構造へ具体化する Phase 2 成果物。docs/daily-support-design.md（v0.2）・docs/regular-talk-design.md（v0.3）と同列。
**状態**: 設計 v1（未実装。M13〜M15）
**作成日**: 2026-08-10

---

## 0. 本書の使い方

- 本書は「どう作るか」を定義する。「何を作るか」は spec §4.1.6 / §4.4.4 / §4.4.6 / §6.0。
- **既存コードの実態を正とする**（先行 2 設計書と同じ運用）。現行は v0.3.0（schema v8、SpeechCategory 12）。architecture.md は実装時に追随改訂する。
- 「実装時に確定」と注記した箇所は方針までとし、細部は実装 PR で詰める（§7 に集約）。
- ② と ③ は**新機能ではなく、spec が以前から要求していた未達の解消**。要件の変更ではないので、spec 側の改訂は伴わない。

---

## 1. 設計原則（v0.4 固有）

| 原則 | 制約 |
|---|---|
| **AI 非依存**（spec §4.2.1） | 独り言の**基盤は辞書**。LLM は上乗せで、未設定・降格・障害・キャッシュ空のいずれでも**辞書の独り言に必ず落ちる**。LLM 経路が壊れても独り言が止まらないこと。 |
| **邪魔をしない**（§4.6.3） | 独り言の配達経路（`deliver_event` の単一ゲート）は変えない。LLM 生成は「配達する文の出どころ」を変えるだけで、**発話可否の判定には一切関与しない**。 |
| **鮮度が価値**（§4.4.6） | 時事ネタは古いと逆効果。**織り込み時と発話時の二段**で失効を判定し、疑わしければ黙って辞書へ落とす。 |
| **選択はユーザーのもの**（§4.1.6） | 表示モニタは**ユーザーの明示選択が常に優先**。自動推定（毎秒ポーリング）が選択を上書きしない構造にする。 |

### 1.1 スコープの境界（やらないこと）

- **キャラをドラッグで別モニタへ移す機能は作らない**（spec §4.1.1 のとおり移動範囲はステージ内のまま）。動かすのはステージごと。
- **モニタごとに別々のキャラ X 位置は記憶しない**（spec §6.6 に残件として明記済み）。移動時は新しいステージ幅へ clamp するだけ。
- 独り言の LLM 生成に**会話履歴・長期記憶を注入しない**（独り言は「ひとりごと」であって応答ではない。プロンプトはゴースト設定 + 時事ネタのみ）。

---

## 2. 表示モニタ選択（§4.1.6）

### 2.1 現状と、設計の核心

`presence/window_pos.rs` は次の構造になっている:

- `dock()` — 起動時に 1 回。`window_pos`（前回の物理座標）を `pick_monitor` に渡してモニタを決め、`apply_dock`。
- `spawn_dock_keeper()` — **1 秒ごとに「現在のウインドウ位置」を `pick_monitor` に渡してモニタを決め直し**、期待矩形とズレていれば再ドック。
- `pick_monitor(window, stored)` — 座標を含むモニタを探し、無ければ `primary_monitor()`。

つまり**モニタの決定は常に「今どこにいるか」からの逆算**であり、「ユーザーがどれを選んだか」という概念が存在しない。ここに選択を足すとき、**毎秒のポーリングが選択を上書きしないことが設計の核心**になる。

### 2.2 設計: 選択を第一の真実にする

モニタ決定を**単一の関数に集約**し、`dock` と `spawn_dock_keeper` の両方がそれを使う。

```rust
/// ステージを置くべきモニタを決める（唯一の決定点）。
/// 優先順位:
///   1. 明示選択 (monitor_pref) が解決できればそれ  ← ユーザーの選択が常に勝つ
///   2. 選択が無い場合のみ、保存座標 or 現在位置からの逆算（従来の pick_monitor）
///   3. どちらも決まらなければ primary_monitor
/// **選択がある場合は現在位置を一切見ない**（ポーリングによる上書きを構造的に防ぐ）。
fn resolve_target_monitor(
    window: &WebviewWindow,
    pref: Option<&MonitorPref>,
    stored: Option<StoredPos>,
) -> Option<Monitor>;
```

- `spawn_dock_keeper` は毎 tick この関数を呼ぶ。選択があれば毎回同じモニタに解決されるため、**再ドックは「選択したモニタの作業領域が変わったとき」だけ起きる**（解像度・タスクバー・DPI 変更への追従は従来どおり働く）。
- 選択が解決できない（そのモニタが今は無い）場合は主モニタへ退避するが、**`monitor_pref` は消さない**。再び解決できる構成になれば次の tick で自動的に戻る。

### 2.3 モニタの識別（「表示構成が同じなら戻る」の実装）

tauri が返す `Monitor` から得られるのは `name()`（Windows では GDI デバイス名 `\\.\DISPLAY1` 等）・`position()`・`size()`・`scale_factor()` のみ。**`name()` は物理モニタに固定されず、接続順で入れ替わりうる**（レビューで確認済み）。そこで恒久 ID を追わず、spec の保証（表示構成が同じなら戻る）をそのまま実装する:

```rust
/// app_settings["monitor_pref"] に JSON で保存する選択。
struct MonitorPref {
    name: Option<String>,   // Monitor::name()
    x: i32, y: i32,         // 選択時の position（物理px）
    width: u32, height: u32 // 選択時の size（物理px）
}
```

解決の条件は **1 つだけ**（満たさなければ「解決できない」＝主モニタへ退避）:

> **`name` が一致し、かつ `position` も一致する** — 表示構成が選択時と同じ。

これが spec §4.1.6 の「**表示構成が選択時と同じであれば戻る。構成が変わっていた場合は主モニタに置く**」の直訳。`name`（GDI デバイス名）は接続順で別の物理モニタに付け替わりうるので、**name だけの一致で採用すると「選んでいないモニタに固定される」**という、spec が明示的に避けようとした状態を招く。

- 単に解像度を変えただけでも `position` が変われば選択は解決できなくなり、主モニタへ退避する。**これは仕様どおりの挙動**であり、ユーザーは select で選び直せる（誤ったモニタに固定されるより安全側）。
- 退避しても `monitor_pref` は消さないので、構成が元に戻れば次の tick で自動的に選択モニタへ復帰する。

### 2.4 契約（コマンド 2 件 / イベントなし / DB 変更なし）

```rust
#[tauri::command]
fn list_monitors(window) -> Vec<MonitorInfo>
// MonitorInfo { name: Option<String>, x, y, width, height, scale_factor: f64,
//               is_primary: bool, is_current: bool, is_selected: bool }
// 戻り値には「選択が解決できているか」も含める（未解決なら UI が注記を出す。§2.5）。
// 設定パネルが select の option を組むために使う。

#[tauri::command]
fn set_monitor_pref(pref: Option<MonitorPref>, window, state) -> Result<(), String>
// None = 選択解除（自動に戻す）。保存後、その場で resolve_target_monitor → apply_dock まで行い
// 「選んだ瞬間に移動する」挙動にする（次の tick を待たせない）。
```

- **モニタ列挙を Rust 側に置く理由**: `@tauri-apps/api` は 2.1.1 で `availableMonitors` を持つが、**現行フロントは window 系 API を一切使っていない**（grep 0 件）。モニタの知識は `window_pos.rs` に集約されており、`is_current` の判定もそこにある。フロントに新しい依存を持ち込むより、既存の「Rust がウインドウを扱い、フロントはコマンド越しに触る」構造に揃える。
- 保存先は `app_settings["monitor_pref"]`（spec が名指し）。読み書きは `char_pos` と同じ作法（コマンドで全置換保存、起動時は boot payload 側で読む）。**`Settings` には入れない**（`window_pos`/`char_pos` と同じ「ウインドウの状態」であり、設定パネルの一括保存に混ぜると `set_settings` 経由の往復が増えるため）。
- **イベントは追加しない**。移動はコマンド内で完結する。

### 2.5 UI（設定パネル「基本」→「表示」）

- `index.html` の「表示」セクション（`settings-display-scale` の隣）に `<select id="settings-monitor">` を追加。
- option は `list_monitors()` の結果から動的生成する（**TTS 話者選択が `list_voices` から option を組む既存パターンを踏襲**）。表示は「1: 3840×2160 (主) — 現在」のように、順番・解像度・主モニタ・現在地が分かる形。先頭に「自動（前回の位置）」= 選択解除。
- **選択したモニタが今つながっていないとき**、select は「自動」に見えてはいけない（spec §4.1.6 は選択を保持すると定めている）。`list_monitors` の戻り値に**選択が解決できたかを含め**、解決できていなければ「選択中のモニタが見つかりません（主モニタに表示中）」と注記する。ユーザーは選び直せるし、繋ぎ直せば自動で戻る。
- 選択の反映は `set_monitor_pref` を**即時 invoke**（`calendar_sources` と同じ「即時保存する項目」の扱い。一括 `onSave` には乗せない）。
- モニタが 1 台のときは select を無効化し、「モニタが 1 台のため選択できません」と表示。

### 2.6 DPI が異なるモニタへの移動（spec §4.1.6）

ステージ高さは `dock_rect` が `STAGE_HEIGHT_LOGICAL * monitor.scale_factor()` で算出しており、**移動先モニタの `Monitor` から取った scale で計算する限り値そのものは正しく出る**。問題は適用の順序で、現行 `apply_dock` は `set_size` → `set_position` の順に呼ぶため、**まだ旧 DPI のモニタ上にいるウインドウに新モニタ基準の物理サイズを与える**ことになり、移動に伴う DPI 変更で OS 側に再スケールされうる。

- 対策: `set_position`（移動）→ `set_size`（サイズ確定）の順に変更する。**移動してから寸法を決める**方が DPI 変更の影響を受けない。
- 保険として、1 秒監視が期待矩形とのズレを検知して再ドックするため、順序が効かない環境でも最長 1 秒で収束する（ただし一瞬のちらつきが出るので、順序の修正を本線とする）。
- **DPI 混在構成での切替は M13 の実機確認項目**（§6）。

### 2.7 モニタ切替時にフロント側で起きること

ウインドウのサイズ・位置が変わるため、既存の再計算が連鎖する:

| 対象 | 契機 | 既存実装で足りるか |
|---|---|---|
| クリック透過マスク | `resize` | `alphamask.ts` が `resize` で再合成 → **足りる** |
| キャラ X 位置 | `resize` | `charpos.ts::reclampAll` が新しい幅へ clamp → **足りる**（spec の「新しいステージ幅へ clamp」がこれ） |
| 吹き出し位置 | キャラ位置追従 | `balloon.ts::repositionAll` → **足りる** |

**実機確認が要る前提**: Rust 側の `set_size`/`set_position` で WebView2 に `resize` が発火すること。発火しない場合は `set_monitor_pref` 後に明示のイベントを飛ばして再計算させる（→ §7-1）。

---

## 3. advanced 独り言と時事ネタ（§4.4.4 / §4.4.6）

### 3.1 現状

独り言経路に **low/advanced の分岐が存在しない**。`spawn_random_talk` → `deliver_event(Monologue)` → `resolve_line` が常に `dict.pick_monologue` を引く（`deliver.rs:147`）。時事ネタは `topics_cache` に溜まるだけで読み出す経路が無い（`list_recent_topics` は呼び出し元ゼロ）。

### 3.2 DB v9: `monologue_cache` 新設

```sql
-- v9
CREATE TABLE IF NOT EXISTS monologue_cache (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 生成に使ったゴースト。切替後に前ゴーストの人格の台詞を喋らないための鍵。
    ghost_id        TEXT    NOT NULL,
    text            TEXT    NOT NULL,
    pose            TEXT,
    -- 織り込んだ見出しの取得時刻（UTC 秒）。時事ネタを含まない文は NULL。
    -- 発話時の賞味期限判定（§4.4.6）に使う。
    topic_fetched_ts INTEGER,
    created_ts      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_monologue_cache_ghost ON monologue_cache(ghost_id);
```

- **`topic_fetched_ts` が二段失効の要**。「いつ生成したか」ではなく「**織り込んだ見出しがいつ取得されたか**」を持つ。生成時刻（`created_ts`）で判定すると、6 日前の見出しを今日生成した文が「新しい」と誤判定される。
- **`ghost_id` が必要な理由**: 生成文はゴーストの人格そのもので、`pose` もシェル依存。ストックは再起動をまたぐため、ゴースト/シェルを切り替えると**前のキャラの人格で喋り、存在しない pose で表情が変わらない**。消費時に現在の `settings.ghost_id` と一致しない行は使わずに捨てる。
- **生成時刻による失効（spec §4.4.4）**: 時事ネタを含まない文も**生成から 30 日を超えたら使わない**。人格や状況が変わっている可能性があり、無期限に持ち越す理由がないため（時事ネタ入りは §3.4 の 7 日が先に効く）。
- sub は持たない（独り言は 1 キャラの発話。`DialogueLine{ main, sub: None }` に詰める）。
- 新規 Db メソッド: `push_monologue_cache(ghost_id, text, pose, topic_fetched_ts, created_ts)` / `pop_monologue_cache(ghost_id, now) -> Option<MonologueCacheRow>`（**取り出しと削除を 1 トランザクションで**。ghost_id 不一致・失効した行はその場で捨てて次を見る）/ `count_monologue_cache(ghost_id)` / `clear_monologue_cache()` / `clear_monologue_cache_with_topics()`（時事ネタ同意の撤回時に `topic_fetched_ts IS NOT NULL` の行だけ消す）。
- マイグレーションは `if current < 9 { ... version '9' }` を末尾に追加する既存パターン。

### 3.3 補充と消費

**消費**（`deliver.rs::resolve_line` の Monologue 分岐）:

```text
Monologue の行を決める:
  1. mode == advanced なら:
     a. settings から ghost_id を読む（settings ロックのみ。ghost ロックは取らない）
     b. pop_monologue_cache(ghost_id, now) — ghost_id 不一致・7日超の時事ネタ入り・
        30日超の行はその場で捨てて次を見る（発話時失効・§3.4）
     c. 取れたら ghost をロックして pose をシェルの pose 集合で検証し
        （不正なら pose=None に落とす。既存 validate_pose と同じ考え方）、
        DialogueLine{ main: SpeechTurn{text, pose}, sub: None } を返す
  2. 取れなければ（low / キャッシュ空 / 全部失効）ghost をロックして
     dict.pick_monologue(sub) — 従来どおり
```

**順序が重要**: 現行 `resolve_line` は先頭で `state.ghost` をロックしたまま最後まで進む。キャッシュの pop（DB 書き込みトランザクション）をその内側に置くと、**ゴーストのロックを DB I/O の間ずっと保持する**ことになる。上のように **DB を触ってから ghost をロックする**順にすれば、ロックの重なりを避けられる（`resolve_line` は同期関数のままでよい）。

**配達失敗時の扱い**: ゲート（`can_deliver`）は `resolve_line` より前なので、静音・busy による `Deferred` では pop は起きない。pop 後に `persist_and_speak` が失敗する（`Failed`）ケースだけ 1 件を失うが、**辞書経路が生きているため発話自体は止まらず**、次の補充で埋まる。この稀な損失を避けるために「配達成功後に削除する」二段構えにすると `deliver_event` の契約に行 ID を通す必要があり、単一ゲートの構造を汚すので採らない。

**補充**（`tasks.rs::spawn_random_talk` の tick 内。発話とは独立）:

```text
mode == advanced かつ topics/LLM が使える状態で、
count_monologue_cache() < REFILL_THRESHOLD(3) なら refill を 1 回だけ走らせる:
  - 1 回の LLM 呼び出しで REFILL_BATCH(5) 件生成させ、まとめて push
  - 失敗しても何もしない（次 tick で再試行。辞書経路が生きているので発話は止まらない）
  - 補充の最短間隔 REFILL_MIN_INTERVAL(30 分) を設け、失敗が続いても呼び出しが詰まらないようにする
```

- **発話の直前に補充しない**。補充が LLM 待ちで数秒かかるため、発話タイミングを遅らせない構造にする。
- ストックは DB なので**再起動をまたいで保持**される（spec の要求）。起動直後に補充が走らない。

### 3.4 時事ネタの織り込みと二段失効（§4.4.6）

```text
補充時（織り込み時の失効）:
  topics_enabled かつ 7 日以内の見出しがあれば、数件をプロンプトに材料として渡す
  → 生成した文には、渡した見出しのうち最も古いものの fetched_ts を topic_fetched_ts として記録
  → 見出しが無い / 時事ネタ無効なら topic_fetched_ts = NULL（純粋な独り言）

発話時（発話時の失効）:
  topic_fetched_ts が NULL でなく now - topic_fetched_ts > 7 日 なら、その行は使わず捨てる
  → 次の行へ。全滅なら辞書の独り言へ落とす
```

- 「最も古い見出しの取得時刻」を記録するのは、**複数の見出しを織り込んだ文は最も古いネタに引きずられる**ため（安全側）。
- `list_recent_topics(limit)` は鮮度フィルタを持たないので、**呼び出し側で 7 日超を除外**する（またはシグネチャに `since_ts` を足す。実装 PR で確定 → §7-2）。
- 既存の 7 日 prune（`topics.rs`）は時事ネタ有効時にしか走らないため、**これに依存しない**。

### 3.5 LLM 呼び出しの作法

**`system/regular_talk.rs::polish_script`（v0.3）が最も近い前例**なので、その形を踏襲する（ただし目的が「整形」ではなく「生成」なので関数は新設し、`system/monologue.rs` に置く）:

- `mode != Advanced` / 降格中（`dialogue.degraded_until`）/ API キー無し → **何もしない**（辞書経路が動くので実害なし）
- **月額上限を補充の前に必ず見る**（`system/cost.rs::check_status`）。超過していれば補充しない。
  - **これが無いと spec §4.4.4「コスト管理は advanced 会話と同じ扱い（月額上限に従う）」を満たさない**。実装で上限を判定しているのはチャット経路だけで、そこは「チャットしたとき」にしか動かない。**チャットを使わず常駐しているユーザーは、補充が 30 分ごとに無人で API を叩き続けても上限超過が検知されない**（降格は 5 分で自動復帰するため歯止めにならない）。
  - 呼び出し後も advanced 会話と同じく上限評価を通し、超過に達したらチャット経路と同じ扱い（降格・告知）に合流させる。**背景処理だけが上限を素通りする穴を作らない**。
- `LlmClient::new(base_url, api_key)` → `ChatMessage::system(ゴースト設定 + 時事ネタ材料)` + `user(生成指示)` の 1 往復
- **専用タイムアウト**（`polish_script` の 20 秒に倣う。背景処理が詰まらない値）
- 成功時 `estimate_cost_usd` → `db.append_api_usage(...)` で会計記録（advanced 会話と同じテーブル・同じ関数 = spec の「コスト管理は advanced 会話と同じ扱い」）
- 応答は JSON 配列で受け、`extract_json_blob` で剥がしてからパース。壊れていたら 1 件も push しない
- **`error_streak` による自動降格には関与しない**（読むだけ・書かない。背景処理の失敗でチャットを降格させない。§13.1-7 で v0.3 が採った判断と同じ）

### 3.6 履歴クリアとの接続（§4.5.5）

`commands/data.rs::clear_history` に `clear_monologue_cache()` を追加する。**`include_profile` に依存せず常に消す**（独り言キャッシュは記憶ではなく生成物のキャッシュで、`chat_log` と同格）。

**あわせて、キャッシュを無効化すべき他の契機**（放置すると古い前提の文を喋り続ける）:

| 契機 | 処理 | 実装位置 |
|---|---|---|
| ゴースト / シェルの切替 | 消費時に `ghost_id` 不一致の行を捨てる（§3.2）。切替は再起動を伴うため、能動的な削除は不要 | `pop_monologue_cache` |
| **時事ネタ同意の撤回**（`topics_enabled` を OFF） | `topic_fetched_ts IS NOT NULL` の行を削除する（同意を外したのに時事ネタ入りの文を喋り続けない） | `set_settings`（v0.3 で天気の「解除」時に `weather_cache` を消したのと同じ作法） |

---

## 4. 負債返済

| # | 項目 | 作業 |
|---|---|---|
| D1 | **test-plan §5 に日常支援（spec §4.6）の手動チェックが無い** | **G 節を新設**（A〜F が spec §4.1〜§4.7 に 1 対 1 対応する既存構造に合わせる）。リマインダー（自然文登録・繰り返し・スヌーズ・完了/未完了・起動時回収）/ ToDo・日課（3 バケット・日課復活・朝の件数告知・終了前確認）/ 状況発話（4 カテゴリの個別 ON/OFF・夜間静音・🔕 で頻度低下）/ カレンダー（ICS 読み込み・今日明日表示・開始前通知）の 4 群 |
| D2 | **architecture.md のモジュール一覧に実在しない `dialogue/monologue.rs`** | 該当行を削除（M7 で削除済みのファイル）。あわせて v0.4 で新設する `system/monologue.rs` を追記。ディレクトリ構造の節を実体と全突合する |
| D3 | **text-reader-spec.md / script-reader-spec.md のヘッダが「状態: レビュー待ち」** | 実態（実装済み・実機検証済み・spec/test-plan へ反映済み）に更新。内容は既に整理済みなのでヘッダのみ |
| D4 | **Irodori 上流の版固定が非一貫** | 3 資産のうち silentcipher だけコミット pin で、本体と dacvae は `main` 追随。**本体と dacvae もコミット pin に統一**する（上流の破壊的変更で配布版が突然壊れるのを防ぐ）。pin する版は実装時に現在の main の HEAD を採る |

---

## 5. 契約サマリ（architecture.md へ反映予定）

- **新規コマンド 2**: `list_monitors` / `set_monitor_pref`
- **新規イベント**: なし
- **新規 Settings フィールド**: なし（モニタ選択は `app_settings["monitor_pref"]`）
- **DB**: **schema v9**（`monologue_cache` 新設）。新規 Db メソッド 5（`push` / `pop` / `count` / `clear` / `clear_with_topics`）。**`prune` は作らない** — 失効判定は pop 時に行い、増えすぎはストック上限で抑えるため、用途の無いメソッドを先に生やさない（CLAUDE.md 規律 2「将来のために入れない」）
- **新規 app_settings キー**: `monitor_pref`
- **boot payload**: 起動時にモニタ選択を反映するため `commands/boot.rs` の `BootPayload` に選択状態を足す（フロント/バックの契約なので architecture.md の反映対象）
- **新規モジュール**: `src-tauri/src/system/monologue.rs`（生成・補充・織り込み）
- **変更**: `presence/window_pos.rs`（`resolve_target_monitor` へ集約）/ `system/deliver.rs`（Monologue 分岐にキャッシュ消費）/ `tasks.rs`（補充トリガ）/ `commands/data.rs`（履歴クリア）/ `commands/settings.rs`（時事ネタ撤回時の掃除）/ `commands/window.rs`（コマンド 2 件）/ `commands/boot.rs` / 設定 UI

---

## 6. 実装マイルストーン

| M | 内容 | 主な成果物 |
|---|---|---|
| **M13 表示モニタ選択** | spec §4.1.6 | `resolve_target_monitor` への集約 / `MonitorPref` と `monitor_pref` キー / コマンド 2 件 / 設定 UI の select / 主モニタ退避と復帰 |
| **M14 advanced 独り言 + 時事ネタ** | spec §4.4.4 / §4.4.6 | DB v9 `monologue_cache` / `system/monologue.rs`（生成・補充・織り込み）/ `resolve_line` の消費 / **二段失効** / 履歴クリア接続 |
| **M15 負債返済** | — | D1〜D4 |

- 依存なし（3 つは独立。M13 と M14 は並行可能）。
- 各 M の完了条件: `cargo check` / `cargo test` / `npx tsc --noEmit` / reviewer 反証 / **実機確認**:
  - **M13**: モニタ切替の目視に加え、**① DPI（拡大率）が異なるモニタ間の切替**（spec §4.1.6 が名指しする要件。ステージ高さがそのモニタのスケールで再計算されるか）**② 選択モニタを外したときの主モニタ退避と、繋ぎ直したときの復帰** ③ 切替後にクリック透過・キャラ位置・吹き出しが崩れないこと
  - **M14**: advanced での実発火（キャッシュから喋ること）、時事ネタ入りの文が出ること、**降格中・キャッシュ空で辞書に落ちること**
- 実装完了後: architecture.md（§1.2・§2・§4・§10・§14）と test-plan の追随改訂。

---

## 7. 未決事項（実装 PR で確定）

1. **モニタ切替で WebView2 に `resize` が発火するか**（発火しなければ `set_monitor_pref` 後に明示イベントで再計算させる）→ M13 で実機確認
2. `list_recent_topics` に鮮度フィルタを足すか、呼び出し側で除外するか → M14
3. 補充の閾値・バッチ件数・最短間隔（3 / 5 / 30 分は初期値）→ M14 で実測して調整
4. 独り言生成のプロンプト文面（ゴースト設定の渡し方・時事ネタの見出しの見せ方・JSON 形式）→ M14
5. モニタ select の表示文字列（順番・解像度・主/現在の示し方）→ M13
6. ストック上限（際限なく溜めない上限値。補充バッチと合わせて決める）→ M14
7. `apply_dock` の順序変更（`set_position` → `set_size`）が DPI 混在環境で狙いどおり効くか → M13 で実機確認

---

## 8. 参照

- [docs/spec.md](spec.md) §4.1.6 / §4.4.4 / §4.4.6 / §4.5.5 / §6.0 — 要件の正本（v1.3）
- [docs/daily-support-design.md](daily-support-design.md) / [docs/regular-talk-design.md](regular-talk-design.md) — 先行 2 版の設計書（様式と共通基盤）
- [docs/architecture.md](architecture.md) v1.8 — 実装後に追随改訂

---

## 9. 改訂履歴

| 日付 | 版 | 内容 |
|---|---|---|
| 2026-08-10 | v1 | 初版（Phase 2 設計。モニタ決定の単一化・`MonitorPref` の同一性判定・`monologue_cache` と二段失効・負債 4 件・M13〜M15）。反証レビュー（2 レンズ 11 指摘）を裁定し反映: **モニタの解決条件を spec どおり「name + position の一致」だけに戻す**（位置不一致でも採用する案は spec §4.1.6 の「構成が変わったら主モニタ」に反するため撤回）／**補充前に月額上限を見る**（チャットを使わない常駐ユーザーで上限が素通りする穴）／`monologue_cache` に `ghost_id` と生成時刻 30 日失効を追加（ゴースト切替後に前の人格で喋る・純粋な独り言が永久に残る）／時事ネタ同意の撤回時に該当行を掃除／**pop を ghost ロックの外へ**（DB I/O 中のロック保持を回避）／退避中の選択を UI に出す／boot payload を契約に追加／DPI 移動を §2.6 として明文化し M13 の実機確認項目へ／用途の無い `prune` を契約から削除 |
