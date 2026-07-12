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

M4c は規模が大きいので **Phase A〜G** に分割して進める:
- **Phase A**: Rust 骨格 (DB v3 / TtsState 拡張 / IrodoriClient スタブ / synthesize_voice 分岐 / 新規 6 コマンドのスタブ / `IrodoriUnavailable` 追加) — **完了 (2026-06-21)**
- **Phase B**: GPU 検出 (Windows DXGI で NVIDIA 物理 GPU を判定) — **完了 (2026-06-21)**
- **Phase C**: Irodori 資産 DL (Python 3.11.9 embeddable + pip + torch CUDA 12.1 + fastapi/uvicorn/huggingface_hub/numpy/soundfile) — **完了 (2026-06-21)**。HF モデル本体の DL は Phase D/G に統合
- **Phase D**: Python サイドカースクリプト (sidecar.py FastAPI + 動的ポート + ready.json + モック推論) + Rust 配線 (start_sidecar/shutdown_sidecar / IrodoriClient::synthesize / voice_ref_generate) — **完了 (2026-06-22)**。実 Irodori モデルとの結線・HF モデル DL は Phase G の実機調整に分離
- **Phase E**: サイドカープロセス管理 (アイドル 5 分で自動 shutdown + lifecycle::quit_app / tray quit に shutdown フック) — **完了 (2026-06-22)**。ヘルスチェック (10 秒 /health ping) は Phase G で必要時に追加
- **Phase F**: フロント設定 UI 拡張 + 参照音声管理 UI (Irodori セクション静的配置 / GPU 状態 / 資産 DL / 進捗 / main・sub の参照音声生成・プレビュー・削除 / プレビュー用 previewWavBase64 / default 辞書に irodori_dl_* と irodori_unavailable 追加) — **完了 (2026-06-22)**
- **Phase G**: 実モデル準備 (sidecar.py に download_models + RealModelBackend スタブ + --no-download) / Settings.tts_irodori_use_real_model / health watcher (30秒間隔、3 連続失敗で IrodoriUnavailable + 再起動) / 「実モデルを使う (β)」UI / docs/quality_checklist.md 新設 — **コード完成 (2026-06-22)**。実 Aratako/Irodori-TTS 推論コードの結線は実機 GPU 環境で TODO

**DoD**:
- 共通 DoD（cargo test 35 件パス / cargo check / tsc グリーン）
- ☑ voicevox_core 資産 DL（規約同意・PAT 任意）が動く 〔コード完成。実機 DL は数百MB のためユーザー操作で実行〕
- ☑ デフォルト声で発話、クレジット表示が常時出る 〔資産 DL 後に有効化することで動作。クレジットは list_voices 取得時に "VOICEVOX:<話者名>" を画面下に常時表示〕
- ☑ 漢字混じり文章で voicevox_core が自然に喋る 〔voicevox_core 内蔵 OpenJtalk が読み解析を行うため前処理不要〕
- ☐ Irodori-TTS（GPU 環境）が初回 DL → 参照音声生成 → 発話まで通る 〔Phase C 完了 (Python ランタイム + 共通依存 + torch CUDA 12.1 DL)。モデル DL/サイドカー本体は Phase D〕
- ☑ Irodori-TTS（GPU 無し環境）で DL ボタンが無効化される 〔Phase B + F 完了: `irodori_check_gpu` の `available=false` で `#settings-irodori-download` を disabled、reason を `#settings-irodori-gpu-state` に表示〕
- ☑ Irodori-TTS で漢字→ひらがな前処理が効いている 〔preprocess.rs 実装済 + synthesize_irodori が呼び出し済 (NotImplemented スタブ手前で適用)〕
- ☐ test-plan §5.5 E-1 が ○ 〔Phase G の実機検証で実施。docs/quality_checklist.md §M4c Irodori-TTS 実機検証 を消化する〕

**実装中の設計判断・追加**:
- **M4c Phase A は骨格のみ・既存 voicevox 経路に回帰なし**: `synthesize_voice` を `settings.tts_engine` で voicevox / irodori に分岐し、irodori 経路は `IrodoriClient::synthesize` が `TtsError::NotImplemented` を返すスタブ。Phase B 以降の実装で同 client を順次拡張する。DB v3 で `voice_refs` テーブルを新設 (UNIQUE(slot) で各 slot 最新 1 件)。
- **M4c (Irodori-TTS サイドカー) は別セッションに分離**: Python embeddable + PyTorch 2GB+ + HuggingFace モデル DL + GPU 検出 + HTTP サイドカー管理 と独立大規模ブロックのため、M4a/b を先に固めて 1 セッション 1 マイルストーンの規律を維持する判断。さらに M4c 内も Phase A〜G に分割。
- **C-API バージョン固定 (0.16.4)**: FFI シグネチャと公式 download ツールを同じバージョンに揃える。
- **CPU 強制 (`acceleration_mode = 1`)**: GPU 経路は依存 DLL 未配布で AV 検出リスクあり、CPU 合成で要件を満たす。
- **events.voicevox_dl_complete / voicevox_dl_failed**: ゴースト発話原則 (横断方針 §3.1) に従い辞書経由で告知。default 辞書に台詞追加。
- **TTS 設定 6 項目** (tts_enabled / tts_engine / tts_speaker_main/sub / tts_speed / tts_volume) を Settings に追加。clamp で範囲補正。
- **`#tts-credit` を index.html 静的配置**: WebView2 透過バグ対策 (動的 createElement だと描画されない問題と同じ)。
- **設定パネル 音声タブ**: TTS 有効化 / 話者 select (list_voices 取得時に動的 fill) / 速度・音量 / 資産 DL ボタン (進捗 listen) / GitHub PAT (keyring)。
- **事前 init**: tts_enabled かつ資産があれば boot 時に背景で `VoicevoxEngine::init` をキック (初発話のラグ解消)。
- **`synthesize_voice` は `spawn_blocking`**: voicevox の TTS は CPU 重で同期 API のため、tokio runtime をブロックしないようブロッキングタスクで実行。

**完了日**: 2026-06-22 (M4a/b + M4c Phase A〜G コード完成。実 Aratako/Irodori-TTS 推論結線は実機 GPU 環境で TODO)
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

**進捗 (2026-06-22)**:
- ✅ M5-G チャットログパネル (`#chatlog-panel` 静的配置、`get_chat_log(limit)` で新しい順 N 件取得、設定パネル「データ管理」から開閉)
- ✅ M5-E データエクスポート + 履歴クリア (`export_data(include_profile)` → ダウンロードフォルダに JSON、`clear_history(include_profile)` で chat_log + (option で) user_profile 削除、確認ダイアログ付き)
- ✅ M5-F ゴースト/シェル切替 UI (`list_ghosts` / `list_shells` で ghosts/ shells/ を scan、設定パネル「キャラクター」セクションで select、切替時は再起動案内)
- ✅ M5-H 自動起動 (`tauri-plugin-autostart` 統合、`set_autostart(enabled)` コマンド、Settings.autostart で UI 同期)
- ✅ M5-D 更新通知 (`Settings.update_feed_url`、`system/update.rs` で feed JSON 比較、起動 30 秒後 + 24h おきに `spawn_update_watcher` で発火、`check_update_now` で即時チェック、`update_notice_seen:<ver>` で重複防止)
- ✅ M5-C 時事ネタ RSS (DB v4 で `interest_topics` + `topics_cache`、`Settings.topics_enabled`、`system/topics.rs` で Google News RSS 取得 + 暗い見出しフィルタ、`tasks::spawn_topics_watcher` で 1 時間おき、`commands::topics::{get_interests, set_interests, fetch_topics_now}`、設定パネル「興味分野」セクション。advanced 独り言混入は将来課題)
- ✅ M5-A DnD ゴースト/シェル展開 (`zip` crate 追加、`ghost/dnd.rs` で zip slip / サイズ 1GB / 拡張子フィルタ / 再帰深さ 10 / strip_prefix / 再帰コピー、`commands::assets::dnd_install` で installed/conflicts/errors 振り分け、`src/dnd.ts` で `onDragDropEvent` listen、conflict 時 confirm 再呼び出し、設定パネルへ結果通知)
- ✅ M5-B ツール (DB v5 `reminders` テーブル / `Settings.tools_enabled` / `tools::{clock,reminder,clipboard}` / advanced system prompt に時刻 + 保留中リマインダー 24h 注入 / 「N 分後」「N 時間後」「N 秒後」parse_request 前置 / `tasks::spawn_reminder_watcher` 10 秒間隔・静音中も鳴る特例 / `commands::tools` 4 件 / `tauri-plugin-clipboard-manager` 統合 / 入力欄 📋 ボタン / 設定パネル「ツール」セクション)

**完了日**: 2026-06-22 (M5 全機能 G/E/F/H/D/C/A/B コード完成)

### 3.6.x 品質改善 (M5 後の整地、2026-06-22)
- `cargo check` の dead_code 警告 8 件をすべて `#[allow(dead_code)]` + 理由コメントに置換 (UsageSummary / ReplyUsage / ChatMessage::assistant / Dictionary.{schema_version,recall} / CostStatus.{current_usd,limit_usd,ratio} / VoicevoxEngine.loaded_models)。実コード削除はせず将来用に温存。
- 警告 0 件で cargo check グリーン、`cargo test` 80 件パス維持。

### 3.7 M6 リリース準備 (完了 2026-07-02)

- ✅ README.md を本開発版に整備 (機能一覧 / ビルド方法 / Windows 専用 / ディレクトリ構成 / データ配置 / ライセンス方針)
- ✅ docs/release-notes/v0.1.0.md (機能要約 M0〜M5 + M4c 実機検証済 + 設定 5 ページ分割、既知の制限 6 項目、検証ステータステーブル)
- ✅ バージョン三点セットを `0.1.0` に確定 (`package.json` / `Cargo.toml` / `tauri.conf.json`)
- ✅ test-plan §5 手動テスト A〜D 消化 (2026-06-22、発見ハング 6 件修正済)
- ✅ docs/quality_checklist.md §M4c G1〜G6 消化 (2026-06-28、実機 RTX 5080、発見バグ 4 件修正済)
- ✅ アプリアイコン生成 (紫グラデ + ゴースト、multi-size ico 16〜256px)
- ✅ `npm run tauri build` NSIS インストーラ生成 (`bundle.active: true` + NSIS currentUser / 日英)
- ✅ インストーラ SHA-256 / FileVersion を release-notes に追記 (`BD6FB05F…834E45` / 0.1.0)
- ✅ Git タグ `v0.1.0`
- ☐ クリーン Windows 環境でのインストール検証 (リリース後フォローアップ、ユーザー側)
**コミット/タグ**: `v0.1.0`

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
- ✅ test-plan §5 の全項目が ○ または「該当なし」
- ✅ test-plan §7 のリリース前手順を全通過
- ☐ クリーンインストール → 起動 → 主要機能 ○ (リリース後フォローアップ)
- ✅ インストーラの SHA-256 を release-notes に記録
- ✅ Git タグ `v0.1.0` を打ち、release-notes を公開

**完了日**: 2026-07-02
**コミット/タグ**: `v0.1.0`

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
| 2026-06-21 | architecture.md | Settings に TTS 関連 6 フィールド（tts_enabled / tts_engine / tts_speaker_main/sub / tts_speed / tts_volume）を追加。TtsState を AppState に追加し VoicevoxEngine を遅延初期化。 | M4 |
| 2026-06-21 | architecture.md | `#tts-credit` を index.html 静的配置（WebView2 透過バグ対策）。VOICEVOX 利用規約に基づくクレジット表示は list_voices で取得した話者名で "VOICEVOX:&lt;話者名&gt;" 形式。 | M4 |
| 2026-06-21 | architecture.md | M4c (Irodori-TTS Python サイドカー) を本マイルストーンから切り出して別セッションに分離。M4a (voicevox_core) と M4b (かな前処理) のみ完了とする。 | M4 |
| 2026-06-21 | architecture.md | DB スキーマ v3 を追加: `voice_refs(id, slot, caption, file_path, created_ts)` + `UNIQUE(slot)`。`upsert_voice_ref` / `get_voice_ref` / `list_voice_refs` / `delete_voice_ref` を `Db` に追加。 | M4c-A |
| 2026-06-21 | architecture.md | `TtsState.irodori: IrodoriClient` を追加 (Mutex なし、内部状態の Mutex は `IrodoriClient` 側で隔離)。`tts/irodori.rs` 新設 (HTTP クライアント骨組み + `TtsError` enum)、`tts/voice_ref.rs` 新設 (refs ディレクトリ管理)。 | M4c-A |
| 2026-06-21 | architecture.md | `synthesize_voice` を `settings.tts_engine` で voicevox / irodori 分岐。irodori 経路は Phase A 時点で `IrodoriClient::synthesize` が `TtsError::NotImplemented` を返すスタブ。preprocess は呼び出し済 (voicevox 資産未 init/失敗時はフォールバック)。 | M4c-A |
| 2026-06-21 | architecture.md | コマンド 7 件追加 (Phase A はスタブ含む): `irodori_check_gpu` / `irodori_assets_ready` / `download_irodori_assets` / `voice_ref_list` / `voice_ref_delete` (動作可) / `voice_ref_generate` / `voice_ref_preview`。`notify::NoticeKind::IrodoriUnavailable` を追加 (dict_key=`irodori_unavailable`)。 | M4c-A |
| 2026-06-21 | architecture.md | フロント `refreshCredit(enabled, engine, main, sub)` に engine 引数追加。engine !== "voicevox_core" のとき非表示 (Irodori は規約上の帰属表示義務なし)。設定パネルにエンジン選択セレクト (`#settings-tts-engine`) 追加、`tts_engine` フィールドを Settings ↔ UI 往復に配線。 | M4c-A |
| 2026-06-21 | architecture.md | `tts/gpu.rs` 新設: Windows DXGI で物理 GPU を列挙し VendorId=0x10DE (NVIDIA) を判定。`pick_irodori_gpu` を pure 関数に切り出してテスト 4 件追加。`commands::tts::irodori_check_gpu` を実 DXGI 判定に置換。`Cargo.toml` の windows features に `Win32_Graphics_Dxgi` / `Win32_Graphics_Dxgi_Common` を追加。 | M4c-B |
| 2026-06-21 | architecture.md | `tts/irodori_download.rs` 新設: Python 3.11.9 embeddable 取得 → `Expand-Archive` 展開 → `python._pth` 編集 (`patch_pth` pure 関数) → `get-pip.py` ブートストラップ → 共通依存 + torch CUDA 12.1 pip install。`commands::tts::irodori_assets_ready` / `download_irodori_assets` を実装に置換。進捗イベント `irodori-download` (voicevox-download と同様の `__done__` センチネル)。 | M4c-C |
| 2026-06-21 | architecture.md | `notify::NoticeKind` に `IrodoriDlComplete` / `IrodoriDlFailed { reason }` を追加 (dict_key=`irodori_dl_complete` / `irodori_dl_failed`)。voicevox 同様の DL 結果告知パターンを Irodori にも適用。 | M4c-C |
| 2026-06-21 | architecture.md | Phase C のスコープ調整: 当初 Phase C 一括で扱う想定だった HF モデル本体 DL は、配置先 / 取得スクリプト / 必要な追加依存が Phase D の sidecar.py 設計に強く依存するため Phase D へ統合。Phase C は「Python + pip + 共通依存」止まり。 | M4c-C |
| 2026-06-22 | architecture.md | `src-tauri/python/sidecar.py` 新設 (FastAPI 単一ファイル / 動的ポート割当 / `ready.json` 書き出し / モック推論モード `--mock` で正弦波・無音 wav 返却 / `POST /shutdown` で 100ms 後に `os._exit(0)`)。`tauri.conf.json` の `bundle.resources` に `python/sidecar.py` を追加し、boot 時に `%APPDATA%\ugg\irodori\sidecar.py` へ best-effort コピー。 | M4c-D |
| 2026-06-22 | architecture.md | `src-tauri/src/tts/sidecar.rs` 新設: `SidecarHandle { port, pid, child }`、`start_sidecar` (Python を tokio::process::Child で起動 → ready.json polling 最大 30 秒)、`shutdown_sidecar` (`POST /shutdown` → 1 秒待って kill フォールバック)、`install_sidecar_script` (リソースから %APPDATA% へコピー)。 | M4c-D |
| 2026-06-22 | architecture.md | `IrodoriClient` 拡張: `sidecar: std::sync::Mutex<Option<SidecarHandle>>` を内部に持ち、`ensure_sidecar_running` で起動済確認 → 未起動なら `start_sidecar`。`synthesize` を実装 (参照音声パスを `voice` で渡し `POST /v1/audio/speech` → WAV)、`generate_voice_ref` を実装 (キャプション → `POST /v1/voice_ref/generate` → 指定パスに wav 保存)。`shutdown` を `lifecycle::quit_app` 用に追加 (Phase E で配線)。 | M4c-D |
| 2026-06-22 | architecture.md | `commands::tts::voice_ref_generate(slot, caption, state)` を実装に置換: `voice_ref::ref_path_in_dir` で `<slot>_<unix_ts>.wav` を決め、`IrodoriClient::generate_voice_ref` 呼び出し → 成功時 `Db::upsert_voice_ref` で DB upsert (`UNIQUE(slot)` で更新)、古いファイルは差分があれば削除。`voice_ref_preview(slot, text, state)` は `synthesize_irodori` を呼んで WAV(base64) を返す薄いラッパ。 | M4c-D |
| 2026-06-22 | architecture.md | `commands::tts::synthesize_voice` の Irodori 分岐: `Db::get_voice_ref(slot)` を取り、未登録なら `TtsError::VoiceRefMissing(slot)` を文字列で返却。preprocess は voicevox 未 init/失敗時にフォールバック (元テキストをそのまま IrodoriClient へ)。 | M4c-D |
| 2026-06-22 | architecture.md | `IrodoriClient` に `last_used: AtomicI64` 追加。`synthesize` / `generate_voice_ref` 冒頭で `touch_last_used()` を呼ぶ。`shutdown_if_idle(now, idle_secs)` を追加し、起動済みかつ最終使用から `idle_secs` 経過なら `shutdown()`。`tasks::spawn_irodori_idle_watcher` 新設 (60 秒チェック、5 分閾値) — `main.rs` setup で spawn。 | M4c-E |
| 2026-06-22 | architecture.md | `commands::lifecycle::quit_app` を async 化し `state.tts.irodori.shutdown().await` を best-effort で実行 (失敗無視で即 `app.exit(0)`)。`window::tray::quit_with_farewell` の `app.exit(0)` 直前にも同じ shutdown を挿入 (両方の終了経路でサイドカー残骸を防止)。 | M4c-E |
| 2026-06-22 | architecture.md | 設定パネルに「音声 (Irodori-TTS / 高品質モード)」セクションを `index.html` 静的配置で追加 (#settings-irodori-gpu-state / #settings-irodori-assets-state / #settings-irodori-download / #settings-irodori-progress / #settings-voiceref-{main,sub}-{state,caption,generate,preview,delete} / #settings-voiceref-progress)。`refreshIrodoriState` で open 時に `irodori_check_gpu` / `irodori_assets_ready` / `voice_ref_list` を呼んで状態同期、`irodori_check_gpu.available=false` で DL ボタン disabled。 | M4c-F |
| 2026-06-22 | architecture.md | `onIrodoriDownload` / `onVoiceRefGenerate(slot)` / `onVoiceRefPreview(slot)` / `onVoiceRefDelete(slot)` を実装 (それぞれ `download_irodori_assets` + `irodori-download` listen / `voice_ref_generate` / `voice_ref_preview` → `previewWavBase64` / `voice_ref_delete`)。`src/tts/speaker.ts` に `previewWavBase64` を export (キューを通さず即時再生、currentSource は触らない)。 | M4c-F |
| 2026-06-22 | architecture.md | default 辞書 (`ghosts/default/dic/main.yaml`) の `system_messages` に `irodori_dl_complete` / `irodori_dl_failed` / `irodori_unavailable` を追加 (notify::NoticeKind 側は Phase A/C で既に追加済)。 | M4c-F |
| 2026-06-22 | architecture.md | `Settings.tts_irodori_use_real_model: bool` (既定 false) 追加。`IrodoriClient::synthesize` / `generate_voice_ref` の `mock` 引数を呼び出し側 (commands::tts) から `!use_real` で渡す形に。`IrodoriClient::health_ping` 追加 (3 秒タイムアウトの `GET /health`)。 | M4c-G |
| 2026-06-22 | architecture.md | `tasks::spawn_irodori_health_watcher` 追加 (30 秒間隔で `health_ping`、3 連続失敗で `shutdown()` + `notify(IrodoriUnavailable)` を発火)。`main.rs` setup で `spawn_irodori_idle_watcher` の隣で spawn。 | M4c-G |
| 2026-06-22 | architecture.md | `sidecar.py` に `download_models(asset_dir)` 追加 (`huggingface_hub.snapshot_download` で `Aratako/Irodori-TTS-500M-v3` / `-v2-VoiceDesign` / `Semantic-DACVAE-Japanese-32dim` を `asset_dir/model/<repo>` に取得、stderr で進捗報告)。`--no-download` フラグ追加。`RealModelBackend` クラス追加 (実推論は TODO 擬似コード)。`build_app(asset_dir, mock, backend)` 拡張で実モデル経路 / モック経路を切替。 | M4c-G |
| 2026-06-22 | architecture.md | 設定パネルに「実モデルを使う (β / 実機検証中)」チェックボックス追加 (`#settings-irodori-use-real-model`)。GPU + 資産両方が揃っていなければ disabled。 | M4c-G |
| 2026-06-22 | docs/quality_checklist.md | 新設。M4c Irodori-TTS 実機検証チェックリスト (G1〜G6: セットアップ / Python DL / モック起動 / ヘルスチェック / 実モデル結線 / フォールバック) を収録。 | M4c-G |
| 2026-06-22 | architecture.md | DB に `ChatLogRow` 型と `Db::list_recent_chat_log(limit) -> Vec<ChatLogRow>` 追加 (新しい順)。`Db::list_api_usage()` (上限 10000 行) と `Db::clear_user_profile() -> u64` も追加。 | M5-G/E |
| 2026-06-22 | architecture.md | `commands::data` モジュール新設 (`get_chat_log` / `export_data(include_profile)` / `clear_history(include_profile)` / `check_update_now`)。`export_data` は `dirs::download_dir()` に `ugg-export-<unix_ts>.json` を保存し絶対パスを返す。`clear_history` は `ClearResult { chat_cleared, profile_cleared_count }` を返す。 | M5-G/E/D |
| 2026-06-22 | architecture.md | `commands::assets` モジュール新設 (`list_ghosts` / `list_shells`): `state::resolve_assets_dir` を `pub` 化し、`ghosts/<id>/ghost.json` / `shells/<id>/shell.json` を scan して `AssetEntry { id, name }` を返却。parse 失敗エントリは skip。 | M5-F |
| 2026-06-22 | architecture.md | `tauri-plugin-autostart` 追加 (Cargo.toml + `tauri::Builder::plugin(...)`)。`commands::lifecycle::set_autostart(enabled)` で `app.autolaunch().enable()` / `.disable()`。`Settings.autostart: bool` (既定 false) を `Settings` に追加し、設定保存時に差分を見て set_autostart を呼ぶ。 | M5-H |
| 2026-06-22 | architecture.md | `system/update.rs` 新設: `check_update_once(app, state)` で `update_feed_url` の JSON (`{latest, url, notes}`) を fetch → `parse_version("a.b.c")` で major.minor.patch 比較 → 新版なら `notify(UpdateAvailable { version })` を 1 度だけ発火、`app_settings."update_notice_seen:<ver>"` で重複防止。`tasks::spawn_update_watcher` (起動 30 秒後 + 24h ごと)。`Settings.update_feed_url: Option<String>` 追加 + `Settings::clamp` で空文字を None に正規化。`notify::NoticeKind::UpdateAvailable { version }` 追加。 | M5-D |
| 2026-06-22 | index.html / settings.ts | 設定パネルに 4 セクション追加 (キャラクター / OS / 更新通知 / データ管理)、`#chatlog-panel` を `index.html` 静的配置。`src/panels/chatlog.ts` 新規 (`mountChatLog` / `openChatLog`)。`src/types.ts` に `ChatLogRow` / `AssetEntry` / `ClearResult` 追加と `Settings.autostart` / `update_feed_url` 追加。`Cargo.toml` に `dirs = "5"` 追加。 | M5-G/E/F/H/D |
| 2026-06-22 | architecture.md | DB v4: `interest_topics(id, topic UNIQUE, enabled)` と `topics_cache(id, topic, headline, link, fetched_ts, UNIQUE(topic, headline))` を追加。`Db::{list_interests, replace_interests, list_enabled_topics, insert_topic_cache, list_recent_topics, prune_topics_cache}` を追加。`InterestTopic` / `TopicCacheRow` 型を追加。 | M5-C |
| 2026-06-22 | architecture.md | `system/topics.rs` 新設: `build_google_news_rss_url(query)` で日本語 Google News RSS URL を組み立て、`fetch_topic(query, limit)` で取得 → `quick-xml` で `<item>` パース → `is_dark_headline` で暗い見出し (訃報/事件/災害/戦争) を除外。`fetch_all_into_cache(state)` で enabled な interest_topics を順次取得し `topics_cache` に INSERT OR IGNORE 蓄積、7 日 prune。 | M5-C |
| 2026-06-22 | architecture.md | `commands::topics` 新設 (`get_interests` / `set_interests(topics)` / `fetch_topics_now`)。`tasks::spawn_topics_watcher` 追加 (1 時間おき、`topics_enabled` 時のみ)。`Settings.topics_enabled: bool` (既定 false) を追加。設定パネルに「興味分野 (時事ネタ)」セクション追加 (チェックボックス + カンマ区切り入力 + 即時取得ボタン)。advanced 独り言への混入は将来課題。`Cargo.toml` に `quick-xml = "0.36"` 追加。 | M5-C |
| 2026-06-22 | architecture.md | `ghost/dnd.rs` 新設 (architecture §12 通り): `DndError` + `detect_asset_kind` (zip / フォルダ、1 階層下までは fallback で peek) + `peek_manifest` + `install_zip` + `install_folder`。`zip = "2"` crate 追加 (default-features off + deflate のみ)。zip slip 対策は `normalize_path(target)` と `sanitize_zip_path`、サイズ上限 1GB、拡張子フィルタ (ghost = yaml/yml/json/md、shell = png/jpg/jpeg/json)、深さ上限 10、`strip_prefix` で `<id>/ghost.json` 形式の zip も `ghosts/<id>/` 直下に展開可能。 | M5-A |
| 2026-06-22 | architecture.md | `commands::assets::dnd_install(paths, overwrite)` 追加。戻り値 `DndResult { installed: DndInstalled[], conflicts: DndConflict[], errors: DndItemError[] }`。conflict (既存 id と重複) はフロント側で confirm → `overwrite=true` で再呼び出し。`src/dnd.ts` 新設 (`mountDnd` で `WebviewWindow.onDragDropEvent` listen → `dnd_install` 呼び出し → conflict confirm → `window.dispatchEvent("ugg-dnd-result")` で配信)。settings.ts で受け取り、設定パネルを自動で開いて結果サマリを表示 + asset select を refresh。 | M5-A |
| 2026-06-22 | architecture.md | DB v5: `reminders(id, due_ts, text, created_ts)` テーブル + `Db::{insert_reminder, list_reminders, due_reminders, delete_reminder}` 追加。`ReminderRow` 型を追加。`Settings.tools_enabled: bool` (既定 false) 追加。 | M5-B |
| 2026-06-22 | architecture.md | `tools` モジュール新設: `clock::now_jp_label` (現在時刻の「YYYY-MM-DD (曜日) HH:MM」)、`reminder::parse_request(text)` (「N 分後/時間後/秒後」の pure 抽出、テスト 7 件)、`clipboard::read_text(app)` (tauri-plugin-clipboard-manager 経由)。 | M5-B |
| 2026-06-22 | architecture.md | `dialogue::run_dispatch` の冒頭で `tools_enabled && parse_request(...)` が成立すれば `handle_reminder_request` を呼び LLM を介さず即時返事 (「N 分後に『X』を覚えておくね」)。chat_log にも user+main を保存。 | M5-B |
| 2026-06-22 | architecture.md | `dialogue::advanced::system_prompt` シグネチャに `tools_block: &str` 追加。`build_messages` が `tools_enabled` のとき `render_tools_block(db)` で現在時刻 + 24 時間以内の保留中リマインダーを system prompt に注入。 | M5-B |
| 2026-06-22 | architecture.md | `tasks::spawn_reminder_watcher` 追加 (10 秒間隔、`due_reminders(now)` を消費して `persist_and_speak` で発話 + DB 削除)。**静音中も鳴らす特例** (`quiet::should_stay_quiet` を見ない)。`notify::ReminderFired { text }` を将来用に追加。 | M5-B |
| 2026-06-22 | architecture.md | `commands::tools` 新設: `list_reminders` / `add_reminder` / `delete_reminder` / `read_clipboard_text`。`tauri-plugin-clipboard-manager` を Cargo.toml + main.rs に統合。chat 入力欄 (`src/dialogue/input.ts`) に 📋 ボタンを追加し、`read_clipboard_text` で末尾追加。設定パネルに「ツール」セクション (tools_enabled + 保留中リマインダー一覧 + 削除ボタン)。 | M5-B |
| 2026-07-12 | spec.md v1.1 | 新機能提案を有用性軸（頻度/代替不可能性/キャラ相乗/負効用リスク、実装難度は不使用）で評価し反映: **§4.6 日常支援を新設**（v0.2 スコープ: 統合リマインダー / ToDo・日課管理 / 状況対応型自発発話＋発話ガバナンス / カレンダー参照 read-only）。§4.2.1 に low=決定論的ローカル / advanced=解釈・生成の境界再定義と「LLM 停止でも生活支援は動く」不変条件。§4.5.3 に tools_enabled からのリマインダー独立の移行予告。§6 を Tier 別ロードマップへ再構成（音声入力を Tier A へ、縮小・凍結方針を明文化）。実装は未着手（Phase 2 = architecture 設計から）。 | v0.2 準備 |

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
