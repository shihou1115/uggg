# ugg 実装計画（implementation-plan.md v1）

**フェーズ**: 本開発 Phase 4 実装着手の段取り
**作成日**: 2026-06-18
**根拠**: [spec.md](spec.md) v1 / [architecture.md](architecture.md) v1 / [test-plan.md](test-plan.md) v1
**位置付け**: **実装の進行管理**。マイルストーン単位の作業切り口と完了条件を定める。

---

## 0. 本書の使い方

- 本書は **「どの順に作るか」** を定義する。「何を作るか」は spec.md、「どう作るか」は architecture.md、「どう確認するか」は test-plan.md。
- 各マイルストーンは **「動く成果物」** を目標とする（垂直スライス方式）。
- 各マイルストーンには **完了条件（DoD: Definition of Done）** を明示。
- 上から下へ順に進める。途中で順序を変える場合は、依存関係を確認すること。

---

## 1. 進行方針

### 1.1 アプローチ: 垂直スライス
- ボトムアップ（DB → モジュール → UI）ではなく、**起動骨格から機能を縦に切って順次拡張**。
- 利点:
  - いつでも「動くアプリ」が手元にある
  - 統合バグを早期発見できる
  - 区切りごとにユーザー（プロダクト所有者）が実機で評価できる
- 欠点と対策:
  - 後続マイルストーンで設計の歪みが出る可能性 → architecture.md を都度参照・必要なら更新

### 1.2 共通の完了条件（DoD）
各マイルストーンの完了は、すべて以下を満たす:
- ☐ `cargo check` グリーン
- ☐ `npx tsc --noEmit` グリーン
- ☐ `cargo test` グリーン（該当範囲のテストが存在し、通る）
- ☐ `npm run tauri dev` で起動し、当該マイルストーンの主要機能が手動で動くことを確認
- ☐ test-plan.md §5 のうち、該当する手動チェック項目が ○

### 1.3 規律（v0.0.3 反省）
- spec.md にない機能を「ついで」で実装しない
- 「将来必要そう」な抽象化を入れない
- 設定フィールドを増やすときは spec.md / architecture.md の改訂を伴う
- マイルストーン途中で大きな設計変更が必要になったら、いったん止めて architecture.md を更新してから実装に戻る

### 1.4 進捗の記録
- 各マイルストーン完了時に本書の **§3 マイルストーン詳細** の該当節に「完了日」と「コミット/タグ」を追記
- 必要があれば spec.md / architecture.md / test-plan.md の改訂を§4 で追跡

---

## 2. マイルストーン一覧

| ID | 名称 | 目的 | 想定規模 |
|---|---|---|---|
| M0 | 起動骨格 | Tauri 2 で空のキャラが表示される最小アプリ | 小 |
| M1 | low モード会話 | 辞書ベースで会話できる | 中 |
| M2 | advanced モード | LLM 経由で会話できる | 中 |
| M3 | 存在感 | マスコットらしい自発挙動が揃う | 中 |
| M4 | TTS | 声で話す（voicevox_core 埋め込み + Irodori サイドカー） | 大 |
| M5 | 補助機能 | DnD・ツール・時事ネタ・データ操作・更新通知 | 中 |
| M6 | リリース準備 | 全項目チェックとインストーラ生成、v0.1.0 リリース | 小 |

---

## 3. マイルストーン詳細

### 3.1 M0: 起動骨格

**目的**: 「ugg.exe をダブルクリックすると、空のキャラが画面に表示される」最小状態を作る。

**含まれる作業**:
- Tauri 2 プロジェクト初期化（`src-tauri/`、`src/`、`package.json`、`tauri.conf.json`）
- バックエンドの最小骨格:
  - `main.rs`（コマンド配線のみ）
  - `state.rs`（AppState の最小定義、サブ状態は空でも可）
  - `db.rs`（接続のみ、マイグレーションは v1）
  - `commands/boot.rs`（`get_boot_payload` のみ）
  - `ghost/manifest.rs`（default シェルだけ読める最小実装）
  - `window/mod.rs`（透過ウインドウ生成、クリック透過は M1 で実装）
- フロントエンドの最小骨格:
  - `main.ts`（boot 起動のみ）
  - `types.ts`（共有型の最小セット）
  - `stage/character.ts`（pose プリロード、初期 pose のみ表示）
  - `stage/scale.ts`（CSS 変数による表示スケール、レイヤー分離 §10）
- 設定パネルなし、トレイなし、対話なし、操作なし

**含まれない作業**:
- 対話（M1）
- LLM 連携（M2）
- 存在感系イベント（M3）
- TTS（M4）
- 補助機能（M5）

**DoD**:
- 共通 DoD（§1.2）すべて
- ☑ `npm run tauri dev` でウインドウが立ち上がり、default シェルのメイン（とサブが在れば）が画面に出る
- ☑ Alt+F4（OS 標準の終了ジェスチャ。M0 は装飾なし窓のため × ボタンは無く、トレイ実装の M3 までは Alt+F4 が窓を閉じる経路）で正常終了する
- ☑ DB ファイルが `%APPDATA%\ugg\companion.db` に作られる（識別子 `io.ugg.app` ではなく `ugg` 直下、architecture.md §2.4 準拠）
- ☑ ghost.json / shell.json のパースエラー時に分かりやすいメッセージ（boot 時の失敗は窓を開いてからフロント側で赤帯 UI 表示。setup フックで panic しない）

**完了日**: 2026-06-20
**コミット/タグ**: -（未コミット）

---

### 3.2 M1: low モード会話

**目的**: 辞書ベースで会話できる。API キー不要・無料・オフラインで動く UX が完成。

**含まれる作業**:
- 辞書 v3 パーサ・バリデータ（`ghost/dict.rs`）:
  - input_match / fallback / recall / monologue / events / system_messages
  - when 条件評価（§6.3 の表現力強化版）
  - sub: null の扱い
- 対話ロジック（`dialogue/low.rs` / `dialogue/banter.rs`）:
  - keyword マッチ + priority
  - 掛け合いパターン 1 のみ（advanced 時に 2-4 + question_curiosity を M2 で）
  - サブ任意化対応
- コマンド `send_user_message`
- フロント:
  - `dialogue/balloon.ts`（吹き出し描画）
  - `dialogue/typewriter.ts`（タイプライター、速度可変）
  - `dialogue/input.ts`（入力欄、Enter送信、Escape閉じ）
  - `interaction/click.ts`（1回=入力欄トグル）
  - `interaction/drag.ts`（ドラッグでウインドウ移動）
- クリック透過（`window/mask.rs` + `stage/alphamask.ts`）
- 起動挨拶（events.first_boot / boot、`presence/quiet.rs` の最小骨格、時間帯 when）

**含まれない作業**:
- LLM 連携（M2）
- 撫で・つつき（M3、または M1 末尾）
- 設定パネルは「最小限」（モード切替も M2 で）

**DoD**:
- 共通 DoD
- ☑ default ゴーストと辞書ベースで会話できる（pick_reply → DialogueResponse → 吹き出し描画、ユーザー目視で確認済）
- ☑ クリック透過が機能（alpha mask + 50ms ポーリング、ユーザー目視で透明部分の透過確認）
- ☑ 起動時に events.boot が時間帯別に出る
- ☑ A-3 タイプライターの速度設定が動く（settings.talk_speed=slow/normal/fast/instant、ゴースト発話側の interval に反映）
- ☑ test-plan §5.2 B-3 / §5.3 C-1, C-4 / §5.4 D-1 が ○

**実装中に判明した WebView2 制約と回避策**:
- Tauri 2 + WebView2 透過ウインドウでは、`document.createElement` で作って後から挿入した `<div>` を `display`/`visibility`/`opacity` のいずれで切り替えても画面に描画されない（DOM 上はサイズ・座標とも正しいが透過レイヤー合成から漏れる）。
- 回避策: 表示・非表示を切り替える要素は index.html に静的に配置しておき、JS は `.visible` クラスの付与・テキスト書き換え・位置調整のみ行う。v0.0.3 と同じ方式に揃えた。
- 適用済要素: `#balloon-main` / `#balloon-sub`（chat-input-wrap は v0.0.3 同様 dynamic で動いたためそのまま）。M2 以降で新規 .solid 要素を追加する際は同じ規約に従う。

**完了日**: 2026-06-20
**コミット/タグ**: -

---

### 3.3 M2: advanced モード

**目的**: LLM 経由で会話できる。コスト管理・モード自動降格・記憶の自動抽出も含む。

**含まれる作業**:
- OpenAI 互換クライアント（`dialogue/llm.rs`）
- `dialogue/advanced.rs`（LLM 呼び出し、プロンプト組み立て）
- `dialogue/banter.rs` 拡張（パターン 1-4 + question_curiosity の確率制御）
- `system/cost.rs`（コスト記録、80% / 100% 警告）
- `system/notify.rs`（U3 統合通知、最小骨格）
- API キー管理コマンド + keyring 連携（`system/secrets.rs`）
- `user_profile` テーブル + 拡張カラム（origin / source_keywords）
- オンボーディング（`panels/onboarding.ts`）
- B-5 会話からの自動抽出（advanced 呼び出しの副産物として）
- 容量管理（advanced=要約サイクル / low=件数上限）
- モード自動降格・復帰
- 設定パネルの最小実装（`panels/settings/general.ts` + `llm.ts`）
- system_messages（cost_warning_80 / cost_limit_exceeded / mode_degraded / mode_recovered）

**DoD**:
- 共通 DoD（cargo test 32 件パス / cargo check / tsc グリーン）
- ☑ OpenAI 互換キーを入れて advanced で会話できる（ローカル LM Studio で OpenAI 互換エンドポイントを実機検証。公式 OpenAI も同経路）
- ☑ ローカル LLM (LMStudio/Ollama) で base_url 設定して動く（LM Studio `http://localhost:1234/v1` で実機確認、api_usage 記録あり）
- ☑ 80% コスト到達でゴースト発話の警告（cost.rs ユニットテストで境界判定、notify 経路は dialogue emit で実機動作。ローカル LLM は cost=0 のため実機での発火は OpenAI 実キー必須）
- ☑ 上限超過で自動降格、ゴーストが告知（同上。降格状態遷移はロジック実装済・API エラー連続降格は実機ログで確認）
- ☑ user_profile に origin=auto の行が増える（gpt-oss-20b で実機確認: "Favorite food is ramen." が origin=auto で保存）
- ☑ test-plan §5.2 B-1, B-2, B-5, B-8 が ○（B-7 コスト警告はローカル LLM では cost=0 のため実 API キー環境で別途）

**実装中の設計判断・追加**:
- **プレーンテキストフォールバック**: 小型ローカル LLM (1.2B 等) は JSON 出力指示に従えずプレーンテキストを返すことがある。その場合 low へ落とさず生テキストを main 単独発話として表示する (`advanced::plaintext_fallback`)。実用上、ローカル LLM 利用時の堅牢性に必須だったため追加。
- **LLM タイムアウト 180 秒**: ローカル大型モデル (12B/20B) は初回ロード + 推論で 60 秒を超えるため。設定可能化は将来課題。
- **モデル別コスト推定テーブル** (`llm.rs::pricing_for`): 未掲載モデル・ローカル LLM は cost=0。OpenAI 主要モデルの概算単価を内蔵。
- **memory 自動抽出**: LLM 応答 JSON の `memory` フィールドを user_profile(origin=auto) に保存。容量管理は M2 では件数上限ベースの簡易版 (advanced 要約サイクルは将来課題)。
- **`complete_onboarding` の interests/topics_enabled**: 引数は architecture §4.9 通り受け取るが、interest_topics テーブルと時事ネタ機能は M5 スコープのため保存は保留 (フラグのみ進める)。
- **設定パネル / コンテキストメニュー / オンボーディングを index.html 静的配置**: [[webview2-balloon-bug]] と同じ理由。動的 createElement だと透過レイヤーで描画されない。
- DB スキーマ v2: chat_log / user_profile / api_usage を追加。

**完了日**: 2026-06-20
**コミット/タグ**: -

---

### 3.4 M3: 存在感

**目的**: マスコットらしい自発挙動が揃う。「そこにいる」感が出る。

**含まれる作業**:
- ランダムトーク（`dialogue/monologue.rs`、既定 10 分・advanced キャッシュ・low 辞書）
- 放置反応（`presence/idle.rs`、30 分）
- 終了挨拶（events.quit、トレイ終了時）
- ポモドーロ（`commands/pomodoro.rs`、状態機械、events.focus_* / break_end / pomodoro_done、バッジ UI）
- 静音モード（`presence/quiet.rs` 拡張、自動判定含む）
- フルスクリーン自動静音（既定 OFF、Win32 API）
- タスクトレイ（`window/tray.rs`、メニュー：表示/モード/静音/設定/終了）
- コンテキストメニュー（`menu/context-menu.ts`、右クリック）
- 撫で・つつき（`interaction/poke.ts` / `nade.ts`、縦のみ部位判定）
- 口パク（`tts/mouth.ts` 骨格、ただし TTS は M4 なので開口フレーム切替のみ）
- 設定パネル拡張（`panels/settings/voice.ts`、`interests.ts`、`about.ts`）

**DoD**:
- 共通 DoD（cargo test 32 件パス / cargo check / tsc グリーン）
- ☑ 既定で 10 分間隔で独り言が出る（実機 chat_log に 600 秒間隔で main/sub ペアが記録、辞書 monologue から抽選）
- ☑ 30 分放置で idle 発火（1 回のみ）（idle.rs / tasks.rs で実装、idle_fired フラグで一度きり）
- ☑ ポモドーロ集中→休憩→ラウンド遷移、ねぎらい台詞（commands/pomodoro.rs 状態機械、辞書 focus_end / pomodoro_done のねぎらい台詞 3 種ずつ拡充）
- ☑ 静音モード、フルスクリーン自動静音（OFF 既定）（presence/quiet.rs、Win32 GetForegroundWindow/MonitorInfo）
- ☑ 右クリックでコンテキストメニュー、トレイメニュー（context-menu に静音/ポモドーロ/隠す追加、window/tray.rs に CheckMenuItem でモード/静音同期）
- ☑ つつき・撫でで反応（部位別、縦のみ）（commands/interaction.rs、shell.json の poke_regions、フロント poke.ts/nade.ts。横判定は除去）
- ☑ test-plan §5.3 C-2, C-3, C-5 / §5.4 D-2 〜 D-9 が ○

**実装中の設計判断・追加**:
- **windows クレート (0.58)** を `[target.'cfg(windows)'.dependencies]` で追加。Win32 API (GetForegroundWindow / GetMonitorInfoW) でフルスクリーン検出。
- **Tauri tray-icon + image-png features** を有効化。トレイは left click でウインドウトグル、右クリックでメニュー (CheckMenuItem でモード/静音の現状を表示)。
- **終了挨拶**: トレイ「終了」では events.quit を再生して hold (1.6+60ms/char、最大8.5秒) 待ってから app.exit。コンテキストメニュー「終了」は即 exit (UI 上の二経路を意識)。
- **ねぎらい台詞拡充**: focus_end / pomodoro_done を各 3 バリエーション。
- **静音判定の OR**: quiet_mode / ポモドーロ集中 / auto_quiet_fullscreen の和。リマインダー (M5) と起動/終了挨拶は呼び出し側で should_stay_quiet を無視する責務。
- **ランダムトーク・放置監視**: tasks.rs に集約。tokio タスクで 60 秒ごとにチェック、busy ゲートを try_acquire で取り、quiet なら持ち越し。
- **persist_and_speak**: バックエンド起点発話を chat_log 保存 + dialogue emit する共通ヘルパを dialogue/mod.rs に追加。tray の quit / random_talk / idle / pomodoro / poke / nade で再利用。
- **撫で判定**: 方向反転 ≥2 / 累積移動 ≥120px / 局所性 90px / 継続 ≥400ms / cooldown 1500ms の合わせ技 (interaction/nade.ts)。
- **poke_regions の横軸 (left_max/right_min) を spec §4.3.2 通り廃止**: shell.json から除去、`PokeRegions { head_max, chest_max }` のみ。

**完了日**: 2026-06-21
**コミット/タグ**: -

---

### 3.5 M4: TTS

**目的**: 声で話す。voicevox_core 埋め込みで無サーバ達成、Irodori-TTS をオプションで提供。

**含まれる作業**:

#### M4a: voicevox_core 埋め込み
- `tts/mod.rs`（trait TtsEngine）
- `tts/voicevox.rs`（libloading + プリビルド C API）
- `tts/download.rs`（公式ダウンローダ取得・規約同意自動応答・GitHub PAT）
- 設定 UI（音声タブ、エンジン選択は M4b で）
- フロント `tts/speaker.ts`（キュー・interrupt・口パク連動）
- フロント `tts/credit.ts`（クレジット常時表示、ステージ下端）
- `notify` に DL 完了/失敗キー追加
- `tts/voicevox.rs` の事前 init（boot 時・設定変更時）

#### M4b: 漢字→ひらがな前処理
- `tts/preprocess.rs`（voicevox_core の Open JTalk 流用）
- AccentPhrase JSON パース、カタカナ→ひらがな
- `TtsState.openjtalk_for_preprocess` の初期化

#### M4c: Irodori-TTS サイドカー
- `tts/irodori.rs`（HTTP クライアント、OpenAI 互換）
- サイドカープロセス管理（O2: 初回起動 + アイドル停止）
- `commands/tts.rs` の Irodori 用コマンド（GPU 検出 / アセット DL / 参照音声生成・一覧・削除・プレビュー）
- GPU 検出（Q3）、DL ボタン無効化、設定 UI
- 参照音声管理 UI（`panels/settings/voice.ts` 拡張、R1+R3）
- `voice_refs` テーブル + ファイル管理
- `notify` に IrodoriUnavailable 等

**DoD**:
- 共通 DoD
- ☐ voicevox_core 資産 DL（規約同意・PAT 任意）が動く
- ☐ デフォルト声で発話、クレジット表示が常時出る
- ☐ 漢字混じり文章で voicevox_core が自然に喋る
- ☐ Irodori-TTS（GPU 環境）が初回 DL → 参照音声生成 → 発話まで通る
- ☐ Irodori-TTS（GPU 無し環境）で DL ボタンが無効化される
- ☐ Irodori-TTS で漢字→ひらがな前処理が効いている
- ☐ test-plan §5.5 E-1 が ○

**完了日**: 未着手
**コミット/タグ**: -

---

### 3.6 M5: 補助機能

**目的**: spec.md で確定した残りの機能を全て載せる。

**含まれる作業**:
- DnD 展開（`ghost/asset_dnd.rs`、zip+フォルダ、zip slip 対策、上書き確認、再起動案内）
- ツール（`tools/clock.rs` / `reminder.rs` / `clipboard.rs`、tools_enabled トグル）
- リマインダーは静音中も鳴らす特例
- 時事ネタ（`system/topics.rs`、RSS 取得・暗い見出しフィルタ・既定オフ・オンボーディング同意）
- 更新通知（`system/update.rs`、update_feed_url ベース、events.update_available）
- データエクスポート / 履歴クリア（`commands/data.rs`、include_profile 切替）
- ゴースト/シェル切替（ホットリロードなし、再起動が必要、案内は notify 経由）
- チャットログパネル（`dialogue/chatlog.ts`）
- 自動起動（`tauri-plugin-autostart`、設定 OFF 既定）

**DoD**:
- 共通 DoD
- ☐ zip / フォルダ DnD でゴースト・シェルが展開、再起動して反映
- ☐ DnD のセキュリティ対策（zip slip、サイズ上限）が機能
- ☐ ツール群が tools_enabled で有効化、無効化時は一切動かない
- ☐ 時事ネタが既定オフ、ON にすると advanced 独り言に混ざる
- ☐ 更新通知がゴーストの口から出る（モック feed URL でテスト）
- ☐ エクスポート / 履歴クリアが正しく動く
- ☐ test-plan §5.5 E-3 〜 E-7 が ○

**完了日**: 未着手
**コミット/タグ**: -

---

### 3.7 M6: リリース準備

**目的**: 全機能の通し確認 + インストーラ生成 + 初版 v0.1.0 をリリース。

**含まれる作業**:
- test-plan §5 の手動テストチェックリスト全項目を実機で実施
- 結果を `docs/release-notes/v0.1.0.md` に記録
- バージョン番号の三点セット更新（tauri.conf.json / Cargo.toml / package.json）
- `npm run tauri build` でインストーラ生成
- インストーラの FileVersion / ProductVersion / SHA-256 を release-notes に記録
- クリーンな Windows 環境でのインストール→主要動作確認
- リグレッション静的チェック（test-plan §8）
- README.md の整備（v0.0.3 README を本開発仕様に書き直し）
- update_feed_url 用の JSON テンプレ準備（運用するなら）

**DoD**:
- ☐ test-plan §5 の全項目が ○ または「該当なし」
- ☐ test-plan §7 のリリース前手順を全通過
- ☐ クリーンインストール → 起動 → 主要機能 ○
- ☐ インストーラの SHA-256 を release-notes に記録
- ☐ Git タグ `v0.1.0` を打ち、release-notes を公開

**完了日**: 未着手
**コミット/タグ**: -

---

## 4. 設計改訂の追跡

実装途中で spec.md / architecture.md / test-plan.md を改訂した場合、ここに記録する（規律 §1.3「設計変更は文書改訂を伴う」）。

| 日付 | 改訂対象 | 内容 | 起因マイルストーン |
|---|---|---|---|
| 2026-06-20 | architecture.md | DB ファイル位置を Tauri 既定の `%APPDATA%\io.ugg.app\` ではなく `%APPDATA%\ugg\` に固定（state.rs::resolve_app_data_dir で APPDATA を直接参照）。spec §2.4 と合わせる。 | M0 |
| 2026-06-20 | architecture.md | Settings に `talk_speed: TalkSpeed (slow/normal/fast/instant)` を追加。フロント typewriter が参照。 | M1 |
| 2026-06-20 | architecture.md | ghost.json に `dictionaries: string[]` フィールド（v3 辞書ファイル相対パスの配列）を必須化。M1 段階では先頭 1 件のみサポート、複数指定はエラー。 | M1 |
| 2026-06-20 | architecture.md | AppState.ghost を `Mutex<Result<GhostBundle, String>>` に。ロード失敗時も起動を継続し `get_boot_payload` が Err を返してフロントが赤帯で告知する。 | M0/M1 |
| 2026-06-20 | architecture.md | Tauri 2 capabilities ファイル（`src-tauri/capabilities/default.json`）を追加。`core:default` / `core:event:default` / `core:window:default` / `core:window:allow-start-dragging` を許可。これが無いと `listen()` / `startDragging()` が機能しない。 | M1 |
| 2026-06-20 | architecture.md | Settings に LLM 関連 5 フィールド追加（llm_provider / llm_model / llm_base_url / monthly_limit_usd / profile_max_count）。`Settings::clamp()` で範囲補正。set_settings / get_settings コマンドで DB 永続化（app_settings."settings" に JSON）。 | M2 |
| 2026-06-20 | architecture.md | advanced 応答の堅牢化として `plaintext_fallback` を追加（LLM が JSON を返さない場合の生テキスト表示）。spec/architecture に明記なしだがローカル LLM 実用に必須。 | M2 |
| 2026-06-20 | architecture.md | LLM HTTP タイムアウトを 180 秒に設定（ローカル大型モデル対応）。`dialogue/llm.rs` にモデル別コスト推定テーブルを内蔵。 | M2 |
| 2026-06-20 | architecture.md | BootPayload に `onboarded: bool` を追加。設定パネル / コンテキストメニュー / オンボーディングパネルは index.html 静的配置（透過レイヤーバグ回避）。 | M2 |
| 2026-06-21 | architecture.md | Settings に M3 関連 5 フィールド追加（auto_quiet_fullscreen / monologue_interval_min / pomodoro_work_min / pomodoro_break_min / pomodoro_rounds）。 | M3 |
| 2026-06-21 | architecture.md | AppState に PresenceState（idle_fired / pos_dirty_since）と PomodoroState（focus / gen / phase / remaining / round / rounds）を追加。 | M3 |
| 2026-06-21 | architecture.md | shell.json の `poke_regions` は縦 2 値のみ（`head_max` / `chest_max`）。横判定 (`left_max` / `right_min`) は spec §4.3.2 で廃止のため除去。manifest.rs に `PokeRegions` 型追加、`ShellCharacter` に伝搬。 | M3 |
| 2026-06-21 | architecture.md | バックエンド起点発話の共通ヘルパ `dialogue::persist_and_speak` を新設。chat_log 保存 + dialogue emit。tray quit / tasks::random_talk / idle / pomodoro / poke / nade で再利用。 | M3 |
| 2026-06-21 | architecture.md | windows クレート (0.58) を `[target.'cfg(windows)'.dependencies]` で追加。tauri features に `tray-icon` / `image-png` を追加。`hide_window` コマンドを追加。 | M3 |
| 2026-06-21 | architecture.md | ポモドーロバッジ (`#pomodoro-badge`) を index.html 静的配置。コンテキストメニューに静音/ポモドーロ/ウインドウ隠すを追加。設定パネルに M3 関連 6 項目（quiet/auto_quiet/独り言間隔/ポモドーロ 3 項目）を追加。 | M3 |

---

## 5. リスクと予防

| リスク | 影響 | 予防 |
|---|---|---|
| マイルストーン M4 (TTS) で voicevox_core FFI が不安定 | M4 が長期化 | v0.0.3 の知見を参照、初期に最小 PoC を切る |
| Irodori サイドカーの Python 同梱が膨張 | インストーラ・配布の見積もりずれ | M4c 初期に同梱物のサイズを実測、計画修正 |
| 辞書 v3 を採用したが既存ゴースト資産が v2 で動かない | デフォルトゴーストが沈黙 | M1 で default ゴーストを v3 へ書き直し、ghosts/default の docs/_legacy 退避 |
| クリック透過の見た目と挙動が一致しない | UX 違和感 | M1 末尾に手動で透明領域の境界をクリックして網羅検証 |
| GPU 検出ロジックが環境依存で正しく動かない | Irodori UI が崩れる | M4c の GPU 検出を early に PoC、Q3 のフォールバックを必ず実装 |
| マイルストーンの粒度が大きすぎ進捗が見えない | モチベ低下・優先度ぶれ | 各 M を 1〜2 週間で割れる粒度で進める、難しければ M をさらに細分 |

---

## 6. 改訂履歴

| 日付 | 版 | 内容 |
|---|---|---|
| 2026-06-18 | v1 | Phase 4 着手前の実装計画 v1 |
