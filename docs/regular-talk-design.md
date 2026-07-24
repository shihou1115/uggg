# 定例会話と天気（v0.3）アーキテクチャ設計書

**対象**: spec.md §4.7 定例会話と天気・生活情報（v0.3 スコープ）＝ 朝・夜の定例会話（§4.7.1）+ 天気・生活情報（§4.7.2）
**位置付け**: spec §4.7（要件の正本）を実装可能な契約・構造へ具体化する Phase 2 成果物。docs/daily-support-design.md と同列の設計文書。
**状態**: 設計 v1 / **M11・M12 実装済み（2026-07-24、ブランチ `feat/v0.3-regular-talk`）**。cargo test 248 / tsc green・ライブ API 検証済み・architecture.md v1.6 追随済み。残: 実機 UI 目視確認・v0.3 リリース。
**作成日**: 2026-07-23

---

## 0. 本書の使い方

- 本書は「どう作るか」を定義する。「何を作るか」は spec §4.7。
- **既存コードの実態を正とする**（daily-support-design §0 と同じ運用）。本書は現行ソース（v0.2.0、schema v8、SpeechCategory 9 カテゴリ）を基準に設計し、architecture.md（v1.4）は実装時に v1.5 として追随改訂する。
- 各機能に **DB / コマンド / イベント / 設定 / 辞書 / ロジック / UI** の 7 面で契約を与える。
- 「実装時に確定」と注記した箇所は擬似コード／方針までとし、細部は実装 PR で詰める（§13.1 に集約）。

---

## 1. 設計原則（spec §4.7 の原則を設計制約へ）

§4.6 の横断原則（AI 非依存 / キャラクターが配達する / 邪魔をしない）はそのまま適用（spec §4.7 冒頭）。本 v で特に効く制約:

| spec の原則 | 設計制約 |
|---|---|
| **AI 非依存** | 材料の集約・発話可否・集約文の組み立ては low = ローカル決定論で完結する。advanced（LLM）は「言い回しの整形」と「会話材料への注入」のみで、**失敗時は常に low の定型文へフォールバック**する（LLM 障害で定例会話が欠けない）。 |
| **邪魔をしない** | 定例会話・降雨の一言はすべて既存の単一ゲート（`deliver_event` → `governance::can_deliver`）を通す **Ambient**。ハード静音・夜間静音に従い、抑制中は同日内で再試行する（§5.2）。 |
| **二重告知の禁止（吸収）** | 朝の定例会話が有効な間、§4.6.2 朝の件数告知と §4.7.2 降雨の一言は**単独発火させない**（定例会話の項目として吸収）。吸収の排他は単一 watcher 内で判定し、レース窓を作らない（§5.5）。 |
| **プライバシー最小化** | 外部送信は気象 API への座標・地名クエリのみ。**座標は保存時点で小数 1 桁（≒11km）に丸め**、丸め前の値をどこにも持たない（§6）。設定行為 = 同意（spec §3.3 / §4.7.2）。 |

### 1.1 スコープの境界（過剰設計の非採用）

spec §4.7 / §6.6 の非採用を設計でも固定する:

- 天気は**単一地点のみ**。複数地点・週間予報の読み上げ・気象警報・気温差アラートは持たない（§6.6）。
- 「生活情報」の実体は天気のみ。ニュース等を本経路に足さない（§4.7.2）。
- 天気キャッシュのために**新 DB テーブルを作らない**（§3.2。単一地点 × 2 日分の小さな JSON で足りる）。schema は v8 のまま。
- 定例会話のために**新 watcher を作らない**（§5.1。既存 `spawn_daily_watcher` に統合する）。

---

## 2. 天気 API 契約（選定裁定）

### 2.1 裁定: Open-Meteo を採用する

**予報 = Open-Meteo Forecast API、地名検索 = Open-Meteo Geocoding API** を採用する。キー不要・無料・地名検索まで単一サービスで閉じる唯一の候補であり、spec §4.7.2 の「キー不要・無料の公開気象 API」を満たす。

**非商用制約の裁定**: Open-Meteo 無料枠は「非商用利用のみ」（terms 明文: *"You may only use the free API services for non-commercial purposes."*）。ugg は**無料配布・広告なし・課金なしの個人開発アプリ**で、API 呼び出しは各ユーザーの端末から本人の個人利用のために行われる。これは terms が非商用の例として挙げる personal projects / home automation の類型に合致し、**利用可と判断する**。ただし次の将来条件を明記する:

- **ugg を有償化・広告掲載・商用製品への組み込みへ転換する場合は、Open-Meteo の有償 API プラン契約か API 差し替えが必須**（本節の再裁定を伴う）。
- spec §4.7.2 の退路（規約・可用性を満たせなければ無効のまま出荷）は保険として維持する。

**レート制限**: 無料枠 600 回/分・5,000 回/時・10,000 回/日。本設計の呼び出しは 1 ユーザーあたり最大でも十数回/日（§3.3）で、各ユーザーの端末から個別に発行されるため実質無関係。

**代替候補（不採用理由の記録）**:
- 気象庁 非公式 JSON（`www.jma.go.jp/bosai/forecast/data/forecast/<area>.json`）: 公共データ利用規約で商用可・出典「気象庁ホームページ」だが、**API としての公式規約・安定性保証が存在しない**（予告なく変更・削除されうると明記）。地名→エリアコード解決も自前実装が要る。将来 Open-Meteo が使えなくなった場合の第一候補として記録に留める。
- MET Norway Locationforecast 2.0: 規約良好（商用可・CC-BY 4.0）だが **User-Agent によるアプリ識別が義務**（現行 HTTP 実装は UA 未設定 §11-4）で、地名検索 API を持たないため geocoding だけ別サービスが要る。不採用。

### 2.2 エンドポイントとリクエスト契約

**予報取得**（`system/weather.rs::fetch_forecast`）:

```
GET https://api.open-meteo.com/v1/forecast
  ?latitude={lat}&longitude={lon}          … 保存済みの丸め値（小数 1 桁）
  &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max
  &timezone=auto                           … 地点ローカルの日付境界で daily を切る
  &forecast_days=2                         … 今日 + 明日のみ（夜の「明日の天気」まで賄う最小）
```

**地名検索**（`commands::daily::search_location`）:

```
GET https://geocoding-api.open-meteo.com/v1/search
  ?name={query}&count=8&language=ja&format=json
```

- HTTP は既存実装（calendar/topics）と同型: `reqwest`、**タイムアウト 15 秒**、`error_for_status()`、User-Agent 指定なし（Open-Meteo は UA を要求しない。§11-4）。
- 取得失敗（ネットワーク・HTTP エラー・パース失敗）は `eprintln!` して既存キャッシュ維持（calendar の先例 `fetch_source_into_cache` と同じ）。リトライはしない（次の定期取得 tick に委ねる）。
- `language=ja` の日本語地名ヒット精度は一次情報で未確認 → 実装時に実クエリで確認し、不足なら表示側でフォールバック（§13.1）。

### 2.3 出典表示（CC-BY 4.0 の義務）

- 取得データは CC-BY 4.0。**帰属表記 + ライセンスへのリンク**を行う（spec §4.7.2 の「VOICEVOX クレジットと同型の義務」）。
- 実装は §9.4: ステージ常設の `#weather-credit`（`#tts-credit` と同型、天気有効時のみ表示、文言「天気: Open-Meteo.com」）+ 設定パネル天気節に正式表記「Weather data by Open-Meteo.com (CC BY 4.0)」とライセンス URL。

---

## 3. 天気基盤: `system/weather.rs`（新設モジュール）

### 3.1 責務

1. 予報の取得と鮮度管理（`ensure_fresh`）
2. 定例会話・降雨の一言・advanced 会話への**材料提供**（`today_material` / `tomorrow_material`）
3. WMO weather code → 日本語ラベル変換（§3.4）

### 3.2 キャッシュ: app_settings 単一キー JSON（新テーブルなし）

**裁定**: `app_settings` のキー `weather_cache` に JSON 文字列で保存する。calendar_cache のような専用テーブルは作らない。

- 理由: データは単一地点 × 2 日分の固定小構造で、行単位のクエリ・インデックス・notified 状態管理（calendar_cache がテーブルである理由）が一切ない。読み書きは「丸ごと読む・丸ごと書く」のみで、`fetched_ts` を JSON 内に持てば鮮度判定も閉じる。**schema v9 を消費しない = マイグレーションなし**。Settings 本体（単一キー JSON）と同じ前例に乗る。
- 将来、複数地点（§6.6）を採用する時にテーブル化を再検討する（spec 改訂を伴う）。

```rust
// system/weather.rs（Serialize + Deserialize。DB 行型と違い双方向 — JSON キャッシュの読み戻しに必要）
pub struct WeatherCache {
    pub fetched_ts: i64,          // 取得時刻 UTC 秒
    pub latitude: f64,            // 取得に使った丸め座標（設定変更の検知用）
    pub longitude: f64,
    pub daily: Vec<DailyWeather>, // [今日, 明日]（地点ローカル日付）
}
pub struct DailyWeather {
    pub date: String,             // "YYYY-MM-DD"（API の timezone=auto が返す地点ローカル日付）
    pub weather_code: u8,         // WMO code
    pub temp_max: f64,
    pub temp_min: f64,
    pub precip_prob_max: u8,      // 0-100
}
```

### 3.3 鮮度と取得タイミング

| 定数 | 値 | 意味 |
|---|---|---|
| `WEATHER_FETCH_INTERVAL_SECS` | 3 * 3600 | 定期取得の間隔（daily watcher の tick 内で判定） |
| `WEATHER_STALE_SECS` | 6 * 3600 | これを超えたキャッシュは**材料として使わず天気項目を省く**（spec §4.7.2 既定 6 時間） |
| `RAIN_PROB_THRESHOLD` | 50 | 降雨判定: `precip_prob_max >= 50` または weather_code が雨系（§3.4） |

- **定期取得**: `spawn_daily_watcher` の 60 秒 tick 内で「天気有効 && 前回取得から `FETCH_INTERVAL` 経過」なら取得（カレンダー watcher が 60 秒 tick 内で 30 分間隔 fetch を判定するのと同型）。専用 watcher は作らない。
- **オンデマンド**: 定例会話の組み立て直前に `ensure_fresh(state)` を呼ぶ。キャッシュが `STALE_SECS` 以内ならそれを返す。超えていれば 1 回取得を試み、成功なら新キャッシュ、失敗なら `None`（天気項目を省く）。
- 座標が設定と不一致のキャッシュ（地域変更直後）は stale 扱いで捨てる。
- 材料の取り出しは **date 文字列の一致**で引く（`today_material` = ローカル今日の日付と `daily[].date` が一致する要素）。深夜跨ぎで `daily[0]` が昨日になっていても誤配しない。一致がなければ `None`（省略）。
- 並行呼び出し（watcher tick と `get_weather` コマンドの同時実行）で二重 fetch が起きても `set_setting` の後勝ちで壊れない。ロックは持たない（許容。§13.1 で実測確認）。

### 3.4 WMO weather code → 日本語ラベル（確定）

| code | ラベル | 雨系（降雨判定に含む） |
|---|---|---|
| 0 | 快晴 | |
| 1 | 晴れ | |
| 2 | 晴れ時々くもり | |
| 3 | くもり | |
| 45, 48 | 霧 | |
| 51, 53, 55, 56, 57 | 霧雨 | ✔ |
| 61, 63, 65, 66, 67 | 雨 | ✔ |
| 71, 73, 75, 77 | 雪 | |
| 80, 81, 82 | にわか雨 | ✔ |
| 85, 86 | にわか雪 | |
| 95, 96, 99 | 雷雨 | ✔ |
| その他 | （ラベルなし → 天気項目を省く） | |

雪系を降雨判定に含めない理由: 「傘・外出注意の一言」の趣旨は降雨であり、雪は将来の警報高度化（§6.6）の領分。

---

## 4. 発話ガバナンス拡張（SpeechCategory 9 → 12）

### 4.1 新カテゴリ 3 種（M11 で一括追加、発火元は M11/M12 が結線）

M7 の先例（未構築 variant を許容し後続 M が結線する）に従い、**M11 で 3 variant を一括追加**する。

| variant | as_str | Priority | 段 3 判定（`enabled()`） | 発火元 |
|---|---|---|---|---|
| `SituationRain` | `situation_rain` | Ambient | `daily_support_enabled && situation_rain_enabled` | M11（daily watcher） |
| `RegularMorning` | `regular_morning` | Ambient | `daily_support_enabled && regular_morning_enabled` | M12（daily watcher） |
| `RegularEvening` | `regular_evening` | Ambient | `daily_support_enabled && regular_evening_enabled` | M12（daily watcher） |

- `COUNT` 9 → **12**。`index()` / `as_str()` / `parse()` / `ALL_CATEGORIES` / `last_by_category` / `backoff` 配列を機械的に拡張。
- 定例会話 2 枠を `daily_support_enabled`（日常支援マスタ）と AND する理由: 定例会話は日常支援の第 2 弾で、材料（ToDo・リマインダー・カレンダー）も Tier S データ。マスタ OFF で Tier S 系がすべて止まる現行の意味論を維持する。
- `is_situation()`: **`SituationRain` のみ true に追加**（段 4/5 の間隔・連投回避と線形バックオフの対象）。`RegularMorning` / `RegularEvening` は **false のまま**（spec §4.7.1 裁定: 1 日 1〜2 回の定例に間隔バックオフは無意味）。既存の段 4/5 が `is_situation()` ゲートである構造のおかげで、**この 1 述語だけで裁定どおりの挙動になる**。
- **コンパイル順の含意（M11/M12 の分担）**: 上表の `enabled()` が `RegularMorning` について `regular_morning_enabled` を参照するため、**M11 で SpeechCategory を 12 へ拡張する時点で、§6 の Settings 11 フィールドを全て追加する**（`regular_*` 6 個を含む。フィールドが無いと M11 がコンパイルできない）。`regular_*` の既定は false なので M11 では inert（`enabled()` が false → 発火せず、配達がないので 🔕 も起きない）。**M12 が結線するのは `regular_*` の発火ロジックと設定 UI のみ**で、Settings フィールド自体は M11 で追加済み。SpeechCategory の配列（`index`/`as_str`/`parse`/`ALL_CATEGORIES`/2 配列）を 2 度触らずに済ませる（M7 の「未構築 variant を許容」先例と同型）。

### 4.2 🔕 フィードバックの適用対象を拡張（`is_situation` → `feedback_target`）

spec §4.7.1 は定例会話にも 🔕（3 回で当該枠のトグル OFF）を要求する。現行は `deliver.rs` の `feedback_allowed = category.is_situation()` が対象を決めているため、この述語を分離する:

```rust
/// 🔕 フィードバック（backoff カウント + 3 回でトグル OFF）の対象。
/// Situation* は加えて段 5 の間隔延長も受けるが、Regular* はカウントのみ
/// （間隔延長は is_situation() ゲートの段 5 にしか無いので、構造上自然にそうなる）。
pub fn feedback_target(self) -> bool {
    self.is_situation()
        || matches!(self, SpeechCategory::RegularMorning | SpeechCategory::RegularEvening)
}
```

- `is_situation()` ゲートは **2 箇所**あり、両方を `feedback_target()` に差し替える（反証レビュー確定指摘。片方だけだと 🔕 ボタンは表示されるのにクリックが黙って捨てられる最悪の UX になる）:
  1. `deliver.rs` の `feedback_allowed = category.is_situation()`（🔕 ボタンの表示可否）
  2. `commands/daily.rs` の `feedback_speech` コマンド入口の `if !cat.is_situation() { return Ok(()); }`（🔕 クリックの受理可否。record_feedback / disable_toggle より手前で弾く二重防御）
- `record_feedback` / `BACKOFF_OFF_THRESHOLD = 3` / `disable_toggle` の既存機構はそのまま使う。`disable_toggle` に 3 カテゴリを追加（`situation_rain_enabled` / `regular_morning_enabled` / `regular_evening_enabled` を落とす）。
- **backoff リセット**: `situation_reenabled` を `feedback_reenabled` に改名し、対象を feedback_target 全カテゴリ（Situation* 4 + Rain + Regular* 2 = 7）に拡張する。リセットしないと、🔕×3 で OFF になった枠を再有効化した直後の 🔕 1 回で即 OFF に落ちる（counter が 3 のまま残るため）。呼び出し元 `set_settings` の変更は関数名と対象リストのみ。

### 4.3 変更しないもの

- 判定表の 5 段構造・Notice/Ambient の意味論・`can_deliver`/`record_delivered` の分離と呼び出し規約（deliver_event 内のみ）は一切変えない。
- `GovernanceState` の構造も配列長（`COUNT`）以外は不変。

---

## 5. 定例会話: ロジック設計

### 5.1 watcher: `spawn_daily_watcher` に統合（新 watcher なし）

**裁定**: 定例会話（朝/夜）・朝告知の吸収・降雨の一言・天気の定期取得を、すべて既存 `spawn_daily_watcher`（60 秒 tick、BOOT_DELAY 30 秒）に統合する。

- 理由: ① 朝の定例会話は §4.6.2 朝告知の吸収先であり、**同一ループ内なら排他が単一の if で閉じてレース窓がない**。② daily watcher は日付変更検知・朝帯判定・app_settings の per-day dedup をすべて既に持つ。③ 新 watcher は BOOT_DELAY の追加調整と spawn 登録を増やすだけで得るものがない（§1.1）。
- tick 内の処理順（1 tick で複数条件が同時成立した場合も、`deliver_event` の busy 直列化が衝突を防ぐ。順序は「定例 > 単独告知」で吸収を先に判定）:

```text
spawn_daily_watcher の 60 秒 tick（擬似コード）:
 1. 日課の復活（既存。日付変更検知）
 2. 天気の定期取得: weather 有効 && last_fetch + 3h 経過 → weather::fetch_forecast → キャッシュ更新
 3. 朝の定例会話の判定（§5.2）→ 発火したら 4/5/6 はこの tick でスキップ（1 tick 1 枠まで。朝夜の窓をユーザーが重ねて設定した場合も、夜枠は次 tick 以降に直列化される）
 4. 朝の件数告知（既存）: ただし regular_morning_enabled == true の間は丸ごとスキップ（吸収 §5.5）
 5. 降雨の一言（§5.6）: ただし regular_morning_enabled == true の間は丸ごとスキップ（吸収 §5.5）
 6. 夜の定例会話の判定（§5.2）
```

### 5.2 発火条件（spec §4.7.1 の発火規則を式に）

枠 slot ∈ {morning, evening} それぞれについて、60 秒 tick ごとに次の**全条件 AND** で 1 回だけ配達する:

```text
fire(slot) =
     settings.daily_support_enabled
  && settings.regular_{slot}_enabled
  && weekday_bit(today_local) ∈ settings.regular_{slot}_days     … bit0=月..bit6=日（reminder と同一定義）
  && minutes_of_day(now_local) >= settings.regular_{slot}_time   … 設定時刻を過ぎている
  && minutes_of_day(now_local) <  min(settings.regular_{slot}_time + REGULAR_SLOT_EXPIRE_MIN, 1440)
                                                                 … 失効窓（当日中のみ。日跨ぎは自然に失効）
  && app_settings["regular_{slot}_date"] != today_local          … 当日未配達（per-day dedup）
  && os_idle_secs() < REGULAR_ACTIVE_IDLE_SECS                   … 「PC を使っている」判定
```

| 定数 | 値 | 根拠 |
|---|---|---|
| `REGULAR_ACTIVE_IDLE_SECS` | 5 * 60 | 既存 `IDLE_BOUNDARY_SECS`（連続利用セッションの境界 = 5 分）と同じ意味論。「直近 5 分以内に OS 入力があった = 使っている」 |
| `REGULAR_SLOT_EXPIRE_MIN` | 360（6 時間） | **失効窓**（反証レビュー確定指摘の対応）。上限がないと「終日 PC を触らず 22 時に開いた日」に朝枠が『おはよ』を夜に配達し、直後に夜枠と二連発する。既定設定では朝 8:00→14:00 失効・夜 21:00→24:00（日跨ぎで自然失効）となり、週末の遅起き（昼過ぎの初操作）は拾いつつ、夜の『おはよ』を防ぐ。spec §4.7.1 の発火規則に失効の存在を追記済み（v1.2.1、確定値は本書） |

- **連続利用中でも設定時刻経過後の最初の判定 tick で発火する**（spec 裁定。アイドル→アクティブの遷移待ちをしない。この式は現在値の閾値比較のみなので自然に満たす）。
- `os_idle_secs()` が `None`（`GetLastInputInfo` 失敗・非 Windows ビルド）の場合は**アクティブ扱いで発火する**。判定不能のために枠が永久に死ぬより、時刻経過で配る方が要件の趣旨（1 日の始まり/終わりに話しかける）に近い。
- `ContextState` / `update_session`（連続利用セッション計測）は**流用しない**。あれは「アイドル境界を超えたらセッション終了」という逆向きのモデルで、ここでは `os_idle_secs()` の現在値比較だけで足りる。
- dedup キーは `regular_morning_date` / `regular_evening_date`（app_settings、値は `YYYY-MM-DD` ローカル日付。`todo_morning_date` と同一パターン）。**日付が変われば自動的に未配達扱い → 前日分は消化される**（spec: 翌朝に昨日の朝の分を言わない、が date 比較だけで成立）。失効窓を過ぎた当日分も date キーを書かずに自然消滅する（翌日は通常どおり）。
- **失効窓内での枠有効化は即時発火を許容する**（例: 13:00 に朝枠を有効化 → 次 tick で『おはよ』）。設定変更の追跡状態を持たない代償として受け入れる（有効化直後に 1 回鳴るのは機能が動いた確認としても機能する）。窓の外（15:00 に朝枠を有効化）なら鳴らない。
- 配達結果の消化規約（既存 watcher と同一）: `Ghost` → date キー更新（消化）。`Deferred`（静音・busy）→ **date キーを進めず次 tick 再挑戦 = 同日内再試行**（spec 裁定）。`Failed`（辞書なし等）→ date キー更新（今日はもう試さない。毎分の空振り辞書引き防止、`todo_morning` と同じ）。fallback は `None`（Ambient は toast に落とさない）。
- 夜間静音帯内に設定時刻を置いた枠は、tick ごとに gate 段 2 で `Deferred` になり続け、その日は配達されない（spec 織り込み済みの挙動。UI 警告 §9.3 が対応）。空振り tick のコストは can_deliver 1 回で無視できる。

### 5.3 材料の集約（low・ローカル完結）

発火が決まった tick で材料を集める。**取得失敗した材料は項目ごと省く**（全体を失敗させない）。

**朝**（`build_morning_materials`）:

| 項目 | 取得 | 絞り |
|---|---|---|
| 今日の予定 | `db.list_calendar(今日 0:00, 明日 0:00)`（ローカル日境界 → UTC 秒） | 開始順。先頭 1 件 + 「ほか N-1 件」 |
| 今日の ToDo 件数 | `db.count_open_todos(Some("today"))` | 0 件なら省く |
| 未完了リマインダー | `db.list_reminders(Active)` → `pending == true` のみ | **due_ts 降順（= 直近に発火したもの優先）で 1 件 + 「ほか N-1 件」**（spec の「直近の数件に絞る」の確定。全件読み上げ禁止） |
| 今日の天気 | `weather::ensure_fresh` → `today_material` | 天気無効・stale・日付不一致なら省く |

**夜**（`build_evening_materials`）:

| 項目 | 取得 | 絞り |
|---|---|---|
| 今日の完了実績 | `db.count_done_todos_since(今日 0:00 ローカルの UTC 秒)` — **新規 Db メソッド**（§10.4） | 0 件なら省く（「0 件」と責めない） |
| 未完了の持ち越し | `db.count_open_todos(Some("today"))` | 0 件なら省く |
| 明日の予定 | `db.list_calendar(明日 0:00, 明後日 0:00)` | 開始順。先頭 1 件 + 「ほか N-1 件」 |
| 明日の天気 | `weather::ensure_fresh` → `tomorrow_material` | 同上 |

- 材料が**全部空でも発火は成立**する（{body} が空文字 → 辞書の導入だけで短いあいさつになる。§8）。
- リマインダーの「未完了」= `list_reminders(Active)` の `pending` 導出列（ack='fired' ログが残っている）。**新 DB クエリ不要**（既存導出列の再利用）。

### 5.4 集約文の組み立て（Rust 定型文 + 辞書の導入/締め）

**裁定**: 材料 → 本文は Rust 側の定型文テンプレート（`build_morning_script` / `build_evening_script`）で組み、**キャラクター性は辞書側の導入・締め（`{body}` の前後）で出す**。項目文まで辞書化するとプレースホルダが項目数ぶん爆発し、空項目の省略ロジックが辞書側に漏れるため。

- 項目文は**中立の簡潔体**（です/ます・キャラ口調を入れない）。「。」区切りで連結し、空項目はスキップ。文例（確定。細かな言い回しは M12 実装 PR で微調整可 → §13.1）:

```text
朝: 「今日の予定は『{summary}』（{HH:MM}）ほか{N-1}件。」   … 1 件なら「ほか」なし。終日予定は時刻省略
    「ToDo は{n}件。」
    「未完了のリマインダーは『{text}』ほか{N-1}件。」
    「天気は{label}、最高{max}℃。降水確率{p}%。」
夜: 「今日終わった ToDo は{n}件。」
    「残りは{n}件。」
    「明日の予定は『{summary}』（{HH:MM}）ほか{N-1}件。」
    「明日は{label}、最高{max}℃／最低{min}℃。」
```

- 配達: `deliver_event(app, state, RegularMorning|RegularEvening, Ambient, "regular_morning"|"regular_evening", &[("body", script)], None)`。既存のプレースホルダ機構 1 個（`{body}`）に収める。
- **advanced 上乗せ（言い回し整形）**: mode=advanced かつ LLM 利用可のとき、low が組んだ script を LLM に 1 回渡して会話調に整形してから配達する。**材料の選定と発話可否は上の low ロジックが確定済み**（spec §4.6.3 原則: LLM は言い回しのみ）。タイムアウト・失敗・降格中は low の script をそのまま使う。プロンプト・トークン上限・タイムアウト値は実装 PR で確定（§13.1）。

### 5.5 吸収規則（二重告知の禁止）

| 吸収されるもの | 条件 | 実装 |
|---|---|---|
| §4.6.2 朝の件数告知（`todo_morning`） | `regular_morning_enabled == true` の間 | daily watcher の朝告知ブロックを丸ごとスキップ。1 日 1 回管理は定例会話側（`regular_morning_date`）が担う |
| §4.7.2 降雨の一言（`weather_rain`） | 同上 | 同上（天気は定例の 1 項目として届く） |

- **定例会話（朝）が当日配達されなかった日（曜日対象外・終日静音・PC 不使用）は、吸収された告知も出ない**（spec 裁定: 許容）。「有効な間は吸収」であり「配達された日だけ吸収」ではない — 条件は設定値のみで判定し、配達実績を見ない（判定を単純に保つ）。
- `regular_morning_enabled == false` に戻せば従来どおり単独発火する（`todo_morning_date` / `weather_rain_date` の dedup は独立キーなので状態が混ざらない）。

### 5.6 降雨の一言（M11。定例会話より先に単独形を実装）

```text
fire(rain) =
     settings.daily_support_enabled && settings.situation_rain_enabled
  && weather 有効（weather_enabled && 座標設定済み）
  && !settings.regular_morning_enabled                    … 吸収（M12 で結線。M11 時点では常に単独）
  && 朝帯（5:00 <= hour < 11:00。todo_morning と同じ帯）
  && app_settings["weather_rain_date"] != today
  && weather::ensure_fresh → today_material が降雨（§3.3 の判定: prob >= 50 or 雨系 code）
```

- 辞書キー: 当日に**時刻付き予定**（`list_calendar(今日)` で `all_day == false`）があれば `weather_rain_outing`（言い回し強め）、なければ `weather_rain`。プレースホルダ: `{label}`（天気ラベル）, `{p}`（降水確率）。
- カテゴリは `SituationRain`（Ambient・段 4/5 とバックオフ適用・🔕 対象）。消化規約は Situation* と同一（Ghost|Failed → date キー消化、Deferred → 再挑戦）。
- 降雨でない朝は date キーを消化する（昼から雨に変わっても当日は鳴らさない。判定は朝 1 回）。

---

## 6. 設定フィールド（Settings 拡張・11 フィールド）

```rust
// state.rs Settings に追加（すべて #[serde(default...)] で後方互換）
pub regular_morning_enabled: bool,   // 既定 false（オプトイン。spec §4.7.1）
pub regular_morning_time: u16,       // 既定 480（8:00。night_quiet と同じ「ローカル 0:00 からの分」）
pub regular_morning_days: u8,        // 既定 127（毎日。bit0=月..bit6=日、reminder weekday_mask と同一定義）
pub regular_evening_enabled: bool,   // 既定 false
pub regular_evening_time: u16,       // 既定 1260（21:00）
pub regular_evening_days: u8,        // 既定 127
pub weather_enabled: bool,           // 既定 false（地域設定 = 同意とセットで UI が有効化）
pub weather_latitude: Option<f64>,   // 既定 None。保存時点で小数 1 桁に丸め（丸め前値をどこにも持たない）
pub weather_longitude: Option<f64>,  // 既定 None。同上
pub weather_place_name: String,      // 既定 ""。表示専用（「東京都 千代田区」等、geocoding の name + admin1）
pub situation_rain_enabled: bool,    // 既定 false（spec §4.7.2）
```

- `clamp()` への追加: `regular_*_time` は 0..=1439、`regular_*_days` は `& 0x7F`、`weather_latitude` は -90.0..=90.0 かつ **小数 1 桁へ丸め**（`(v * 10.0).round() / 10.0`）、`weather_longitude` は -180.0..=180.0 かつ同丸め。bool は clamp 対象外（既存慣習）。
- 丸めを clamp に置く理由: `set_settings` 経路のどこから来ても（UI・将来の別経路）保存前に必ず丸まる。**送信時丸めではなく保存時丸め**にすることで、丸め前座標がプロセス外（DB・ログ）に出る経路を構造的に断つ。
- 天気の有効条件は `weather_enabled && weather_latitude.is_some() && weather_longitude.is_some()`（ヘルパー `Settings::weather_ready()` を用意）。UI は地名検索で座標が決まったときに `weather_enabled` を立てる（§9.2）。
- フロント `src/types.ts` の `Settings` interface に同名フィールドを追加（1:1 同期）。

---

## 7. コマンド契約（新規 2 件・イベント追加なし）

```rust
// commands/daily.rs に追加
#[tauri::command]
async fn search_location(query: String) -> Result<Vec<LocationHit>, String>
// Geocoding API を呼び、候補を返す（設定は書かない。選択・保存はフロントが set_settings で行う）
// LocationHit { name: String, admin1: Option<String>, country: Option<String>, latitude: f64, longitude: f64 }
// クエリ空・2 文字未満は Err。0 件ヒットは Ok(vec![])。

#[tauri::command]
async fn get_weather(state) -> Result<Option<WeatherSnapshot>, String>
// ensure_fresh を通したキャッシュを返す（設定 UI の「いま取得」検証・現在値表示用）。
// weather_ready でなければ Ok(None)。取得失敗かつキャッシュなしも Ok(None)（Err はコマンド実行自体の失敗のみ）。
// WeatherSnapshot { fetched_ts: i64, daily: Vec<DailyWeather> }
```

- **新規イベントは追加しない**。天気キャッシュ更新は UI へ push しない（設定パネルの表示は `get_weather` の戻り値で足り、他に天気を常時表示する UI が無い）。設定変更は既存 `settings-changed` が飛ぶ。
- `search_location` は state 不要（設定を読まない・書かない純 API 呼び出し）。座標の丸めは保存時（clamp）に行うため、候補表示は API の生値のままでよい（保存された時点で丸まる）。

---

## 8. 辞書キー（events v3 に 4 キー追加）

| キー | 用途 | プレースホルダ | 発火元 |
|---|---|---|---|
| `regular_morning` | 朝の定例会話（導入 + `{body}` + 締め） | `{body}` = 集約文 | M12 |
| `regular_evening` | 夜の定例会話（ねぎらい + `{body}` + 締め） | `{body}` | M12 |
| `weather_rain` | 降雨の一言（単独発火時） | `{label}`, `{p}` | M11 |
| `weather_rain_outing` | 同・外出予定がある日の強め版 | `{label}`, `{p}` | M11 |

- 命名は既存の snake_case 短命名（`todo_morning` 等）に揃える。カテゴリ `as_str`（`regular_morning` / `regular_evening`）と辞書キーを同名にし、突合を単純にする。
- **`{body}` 空文字で自然に読める形に書く**規約（材料全空の朝 = 「おはよ！」だけで成立させる。spec §4.7.1）。プレースホルダ空置換は deliver.rs の既存挙動（未知/未渡しは空文字）をそのまま使う。例:

```yaml
regular_morning:
  - main: { text: "おはよ！{body}", pose: happy }
    sub:  { text: "朝の報告は以上。今日もぼちぼちやれ", pose: normal }
regular_evening:
  - main: { text: "今日もおつかれさま。{body}", pose: normal }
    sub:  { text: "あとは休むだけだ。いい夜を", pose: normal }
weather_rain:
  - main: { text: "今日は{label}みたい。降水確率{p}%、傘があると安心かも", pose: normal }
weather_rain_outing:
  - main: { text: "今日は出かける予定があるのに{label}だって。降水確率{p}%、傘は忘れずに！", pose: troubled }
    sub:  { text: "濡れて帰ってきても知らんぞ", pose: normal }
```

- サブ主体の原則（spec §4.6 冒頭: 通知・催促はサブ）に従い、sub の合いの手を基本形に含める（サブ無しシェルでは既存機構が自動で落とす）。
- 実装側の呼び出しキー文字列と辞書キーの一致は grep 突合をマイルストーン完了条件に含める（§12。辞書はホワイトリスト検証がなくタイポが静かに Failed になるため — deliver-dict 調査の指摘）。

---

## 9. UI 設計（設定パネル「日常支援」ページに 2 節追加）

### 9.1 定例会話節（`index.html` の `data-page="daily"` 内）

- 朝・夜それぞれ: 有効トグル（`checkbox-row`）+ 時刻（`<input type="time" step="60">`、`minutesToHHMM`/`hhmmToMinutes` の既存ヘルパーで分⇔HH:MM 変換）+ 曜日選択。
- **曜日選択 UI（新設・先例なし）**: 「月火水木金土日」の 7 個の横並びトグルボタン群（`<button class="weekday-toggle" data-bit="0">月</button>` …）。クリックで on/off、見た目は `.active` クラス。値は bit0=月..bit6=日 の u8 に合成（エンコードは新規、デコード表示は `daily.ts` の `weekdayNames()` と同じビット定義）。チェックボックス 7 個より狭い面積で収まり、リマインダー一覧の曜日表示と同じ略字で揃う。
- 保存は既存の一括 `onSave()` に乗せる（`Inputs` interface / `collectInputs` / `applySettingsToForm` / `onSave` の 4 箇所同時変更パターン）。

### 9.2 天気節（同ページ）

カレンダーソース節の「入力 → 追加（即保存）→ いま取得で検証」パターンを踏襲:

1. 地名入力 + 「検索」ボタン → `search_location` → 候補リスト（`name / admin1 / country`）を表示
2. 候補クリック → `set_settings` で `weather_latitude/longitude/place_name` を保存し **`weather_enabled = true` を同時に立てる**（**設定行為 = 同意**。ボタン脇に「地名・座標は天気予報の取得のため Open-Meteo に送信されます（市区町村単位に丸め）」の注記を常設 — spec §3.3/§4.7.2）
3. 設定済み表示: 「{place_name}（{lat}, {lon}）」+ 「解除」ボタン + 「いま取得」ボタン（`get_weather` → 今日/明日の天気を表示して検証）
   - **「解除」= 同意の撤回**として、`weather_enabled = false` + 座標 None + `weather_place_name = ""` に戻し、app_settings の `weather_cache` も削除する（設定行為 = 同意の対称として、解除で地名・キャッシュを残さない）
4. 降雨の一言トグル（`situation_rain_enabled`。天気未設定時は disabled + 「先に地域を設定してください」ヒント）
5. 正式な出典表記: 「Weather data by [Open-Meteo.com](https://open-meteo.com/)（CC BY 4.0）」

- 結果表示・エラーは既存様式の節内メッセージヘルパー（`showWeatherMessage`、`showCalendarMessage` と同型の個別実装）。

### 9.3 夜間静音帯の重なり警告（保存は止めない）

- 対象: 朝/夜の各時刻入力。`night_quiet_enabled && 帯内(regular_*_time)` のとき、時刻入力の下に警告文「この時刻は夜間静音（{from}〜{to}）の中のため、この枠は配達されません」を表示する。**保存はブロックしない**（spec: UI 警告。帯を跨いだ運用をユーザーが意図的に選べる余地を残す）。
- 判定はフロントの純関数（分単位、`from > to` 日跨ぎ・`from == to` 終日を governance.rs の `night_quiet_active` と同一意味論で実装）。フォーム変更のたびに再計算。Rust 実装との突合はユニットテストの対応表で担保（§13.1）。

### 9.4 出典表示（ステージ常設）

- `index.html` に `#weather-credit`（`#tts-credit` の隣、同じ `.solid` + `.visible` トグル様式）を静的配置。文言「天気: Open-Meteo.com」、title 属性に「Weather data by Open-Meteo.com (CC BY 4.0)」。
- `src/tts/credit.ts` と同型の小モジュール `src/weather/credit.ts`: `weather_ready` のとき表示、`settings-changed` で再計算（`main.ts` の既存フックに 1 行追加）。

---

## 10. 契約サマリ（architecture.md v1.5 へ反映予定）

### 10.1 新規コマンド（2）
`search_location(query) -> Vec<LocationHit>` / `get_weather() -> Option<WeatherSnapshot>`（§7）

### 10.2 新規イベント
なし（既存 `settings-changed` のみ使用）

### 10.3 新規設定フィールド（11）
`regular_morning_enabled/time/days`, `regular_evening_enabled/time/days`, `weather_enabled`, `weather_latitude`, `weather_longitude`, `weather_place_name`, `situation_rain_enabled`（§6）

### 10.4 DB
- **schema 変更なし（v8 のまま）**。新テーブルなし。
- app_settings 新キー: `weather_cache`（JSON）, `regular_morning_date`, `regular_evening_date`, `weather_rain_date`（per-day dedup）, `governance_backoff:situation_rain` / `:regular_morning` / `:regular_evening`（既存機構の自動拡張）
- 新規 Db メソッド: `count_done_todos_since(since_ts: i64) -> Result<u64>`（`status='done' AND done_ts >= ?` の件数。夜の完了実績用）

### 10.5 新規辞書キー（4）
`regular_morning`, `regular_evening`, `weather_rain`, `weather_rain_outing`（§8）

### 10.6 新規モジュール
- `src-tauri/src/system/weather.rs`（取得・キャッシュ・材料・WMO ラベル）
- `src/weather/credit.ts`（出典表示）
- 変更: `governance.rs`（12 カテゴリ + feedback_target + feedback_reenabled）, `tasks.rs`（daily watcher 統合）, `deliver.rs`（feedback_allowed 述語）, `commands/daily.rs`（新コマンド 2 件 + **feedback_speech 入口ゲートの feedback_target() 差し替え** §4.2）, `state.rs`（Settings）, `settings.ts` / `daily` ページ UI

---

## 11. 既存資産との整合・注意（調査で判明した実態）

1. **辞書スキーマの実態は v3**（`dict.rs` は `schema_version: 3` 固定で他を拒否）。CLAUDE.md の「v2 形式」表記は誤りで、本書は v3 を正とする（CLAUDE.md は別途訂正）。
2. 辞書 events にはキー名のホワイトリスト検証が無く、実装側キー文字列とのタイポは**静かに Failed になる**。新キー 4 件は grep 突合を完了条件に含める（§12）。
3. `list_reminders(Active)` の `pending` 導出列（ack='fired' の EXISTS）が「未完了リマインダー」の唯一の横断手段。reminder_log の横断クエリは無いが、本設計はこれで足りる（新クエリ不要）。
4. 既存 HTTP 実装（calendar/topics）は reqwest 都度生成・15 秒タイムアウト・**User-Agent 未設定**で統一されている。weather も同型に揃える（3 つ目の同型実装になるが、共通ヘルパー化は「繰り返すと分かった時点で」の原則に従い本 v ではしない。将来 MET Norway 等 UA 必須 API へ切り替える場合は UA 対応が前提になる）。
5. `weekday_mask` のビット定義（bit0=月..bit6=日）は `tools/reminder.rs` の定義を正とし、`regular_*_days` も同一定義にする（カレンダー RRULE 側の `weekday_code` と値は一致するが実装は別物 — 混同注意）。
6. 設定の永続化は単一キー JSON（`app_settings["settings"]`）のため、`#[serde(default)]` を付ける限り**マイグレーション不要**で後方互換。
7. `Settings` 追加時は `impl Default` の初期値追加がコンパイルエラーで強制されるが、`clamp()` とフロント `types.ts` / `settings.ts` の 4 箇所は強制されない — M11/M12 のレビュー観点に含める。
8. エラーログ（`eprintln!`）に取得 URL（丸め座標を含む）が乗りうるが、ローカル stderr のみで外部送信はなく、座標は丸め済みのため許容する。

---

## 12. 実装マイルストーン

| M | 内容（spec） | 主な成果物 |
|---|---|---|
| **M11 天気基盤** | §4.7.2 | `system/weather.rs`（fetch/キャッシュ/材料/WMO ラベル）、**Settings 11 フィールド全て**（weather_* + situation_rain_enabled + regular_* 6 個。データモデルを M11 で完成させる — 理由 §4.1 コンパイル順）+ clamp、SpeechCategory **12 へ一括拡張** + `feedback_target` + `feedback_reenabled` + **🔕 二重ゲート差し替え（deliver.rs と commands/daily.rs feedback_speech の両方** §4.2。governance 単体テストはコマンド層のゲートを踏まないため、feedback_speech 経路のテストを別途含める）、コマンド `search_location`/`get_weather`、UI 天気節 + `#weather-credit`、辞書 `weather_rain`/`weather_rain_outing`、降雨の一言（daily watcher 統合・単独形）、advanced 会話材料注入 |
| **M12 定例会話** | §4.7.1 | daily watcher へ朝/夜判定統合 + 朝告知・降雨の吸収、材料集約 + `build_*_script`、Db `count_done_todos_since`、（Settings フィールドは M11 で追加済み）**regular_* の発火ロジック + 設定 UI**、辞書 `regular_morning`/`regular_evening`、UI 定例会話節（曜日トグル新設・夜間静音重なり警告）、advanced 言い回し整形（low フォールバック必須） |

- 依存: M12 は M11 の weather 材料・SpeechCategory 拡張に依存する。M11 単体でも出荷可能な増分（天気設定 + 降雨の一言）。
- 各 M の完了条件: `cargo check` / `cargo test` / `npx tsc --noEmit` / 実機確認（`scripts/dev-ready.ps1` 同期待ち）/ reviewer 反証、に加えて **辞書キー・カテゴリ文字列の grep 突合**（§11-2）と **test-plan.md §5 への項目追記**（Tier S 分が未追記のまま残っている実態を v0.3 で繰り返さない）。
- 実装完了後: architecture.md v1.5（§1.2/1.3, §2, §3, §4, §5, §6.2, §11.4, §14.1, §16）と spec §4.7 の実装済み表記、CLAUDE.md ロードマップ行を追随改訂。

---

## 13. 未決事項

### 13.1 実装 PR で確定（設計の骨格に影響しない詳細）

1. Geocoding `language=ja` の日本語ヒット精度の実確認と、候補表示のフォールバック → **M11 で確定**（実測: 日本語ラベルは返るが GeoNames の表記ゆれで 0 件あり ―「東京」→0 件・「東京都」→OK、「渋谷」→OK・「渋谷区」→0 件。候補を都道府県付き複数表示で曖昧解消しつつ、検索ヒント + 0 件時の言い換え案内を追加した。commit `bcfb9b5`）
2. 集約文の項目文言の微調整（§5.4 の文例が正、構造は変えない）→ M12 で確定
3. advanced 言い回し整形のプロンプト・トークン上限・タイムアウト値（low フォールバック必須のみ確定済み）→ **M12 で確定**（タイムアウト 20 秒、トークン上限は設定なし〔既存 `ChatRequest` に `max_tokens` フィールドが無く、追加は契約外のため見送り〕。降格中・API エラー・タイムアウト・空応答で low へフォールバック）
4. 吹き出し・TTS が集約文（最大 150 字程度）を破綻なく表示/読み上げできるかの実機確認 → M12 で確定
5. 夜間静音重なり判定の TS/Rust 突合テストの形（対応表ユニットテスト）→ M12 で確定
6. weather の並行 fetch（watcher tick と get_weather 同時）が実運用で問題を起こさないことの確認 → M11 で確定
7. **advanced 整形のコスト上限の扱い → M12 で裁定（受容）**: `polish_script` は LLM コストを `api_usage` に記録するが、`monthly_limit_usd` の閾値チェック（80% 警告・自動降格）はチャット経路（`degraded_until` を立てる）でしか発火しない。チャットをほぼ使わず定例会話だけ使うユーザーは上限チェックを経ずに整形が走るが、**1 日最大 2 回の短い言い換えで実コスト ≈ 月 $0.004** と無視できる水準のため現状受容。厳密なコスト保護が必要になれば `polish_script` に「月次上限超過なら整形をスキップして low へ」の軽量チェックを足す（将来判断）。

### 13.2 将来（本 v ではやらない）

- 複数地点・週間予報・気象警報・気温差アラート（spec §6.6）。採用時に weather_cache のテーブル化を再検討。
- 項目文テンプレートの辞書化（ゴーストごとに項目文の口調を変える）。辞書スキーマ拡張を伴うため将来判断。
- HTTP 共通ヘルパー化（calendar/topics/weather の 3 実装が揃った今、次の HTTP 利用が現れた時点で検討）。

### 13.3 本 v で確定済み（覆す場合は本書 + spec の改訂を伴う）

1. **天気 API = Open-Meteo（forecast + geocoding）**。非商用制約は現行の無料配布形態で適合と裁定。有償化・広告掲載時は再裁定必須（§2.1）
2. **weather キャッシュ = app_settings 単一キー JSON**。新テーブル・schema v9 なし（§3.2）
3. **watcher 新設なし** — 定例会話・降雨・天気定期取得は `spawn_daily_watcher` に統合（§5.1）
4. **SpeechCategory は M11 で 12 へ一括拡張**。Regular* は `is_situation() == false`（間隔バックオフ非適用）のまま 🔕 対象にするため `feedback_target()` 述語を新設（§4）
5. **アクティブ判定 = `os_idle_secs() < 300`**。None はアクティブ扱い。**失効窓 = 設定時刻 + 6 時間（当日中のみ）**、1 tick 1 枠、窓内の有効化直後発火は許容（§5.1/§5.2。spec §4.7.1 v1.2.1 に失効の存在を追記済み）
6. **集約文 = Rust 定型文 + 辞書は導入/締め（`{body}` 1 プレースホルダ）**（§5.4）
7. **吸収は設定値のみで判定**（`regular_morning_enabled == true` の間、朝告知・降雨は単独発火しない。配達実績は見ない）（§5.5）
8. **座標は clamp() で保存時丸め**（小数 1 桁。丸め前値を保持しない）（§6）
9. 降雨判定 = `precip_prob_max >= 50` or 雨系 weather_code（雪系は含まない）。朝帯 5:00-11:00・1 日 1 回（§3.3/§5.6）
10. 出典表示 = ステージ常設 `#weather-credit` + 設定パネル正式表記（§2.3/§9.4）

---

## 14. 参照

- [docs/spec.md](spec.md) §4.7 — 要件の正本（v1.2）
- [docs/daily-support-design.md](daily-support-design.md) — 様式・共通基盤（ガバナンス §4 / 配達 §3 / TZ 契約 §2.5）の正本
- [docs/architecture.md](architecture.md) v1.4 — 実装後に v1.5 へ追随改訂
- Open-Meteo: https://open-meteo.com/en/docs / https://open-meteo.com/en/docs/geocoding-api / https://open-meteo.com/en/terms

---

## 15. 改訂履歴

| 日付 | 版 | 内容 |
|---|---|---|
| 2026-07-23 | v1 | 初版（Phase 2 設計。API 選定裁定・キャッシュ方式・watcher 統合・ガバナンス 12 カテゴリ・M11/M12 切り）。反証レビュー（4 レンズ 16 指摘）を裁定し反映: 🔕 の is_situation ゲートは deliver.rs + feedback_speech の 2 箇所（§4.2）／定例会話に失効窓 6h・1 tick 1 枠（§5.1/§5.2、spec v1.2.1 連動）／天気「解除」のクリア範囲（§9.2） |
