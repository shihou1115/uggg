# ugg テスト計画（test-plan.md v1）

**フェーズ**: 本開発 Phase 3 確定版
**作成日**: 2026-06-18
**根拠**: [spec.md](spec.md) v1 / [architecture.md](architecture.md) v1
**位置付け**: **テスト戦略の正本**。Phase 4 実装着手の前提条件として、コードを書く前に本書のテスト枠組みが整っていること。

---

## 0. 本書の使い方

- 本書はテストの **戦略・分類・配置** を定める。**個別テストケースの全列挙はしない**（実装段階で各モジュールの責任で追記）。
- 「**手動テストチェックリスト**」（§7）はリリース前の必須通過項目。
- 機能導入時の実機検証記録（Irodori / 読み上げ / 台本）は §5.8 に集約（旧 quality_checklist.md、2026-07-24 統合）。

---

## 1. テスト戦略の原則

### 1.1 何のためにテストするか
1. **回帰防止**: 変更で既存機能を壊さないこと（Phase 1 で「シンプル化」した割に肥大化しないこと）
2. **設計判断の保護**: spec.md / architecture.md の判断を実装が逸脱したら検知すること
3. **手戻り削減**: 実装中の早期検出で「Phase 1 仕様再検討」相当の手戻りを防ぐこと

### 1.2 何をテストしないか
1. **WebView2 / Tauri / OS API 自体**: 上流の責任、信頼する
2. **voicevox_core / Irodori モデルの合成品質**: 上流の責任、入出力の存在のみ検証
3. **LLM プロバイダの応答品質**: 同上
4. **キャラ画像の見た目品質**: 手動目視のみ

### 1.3 三段構成
| レベル | 対象 | 自動化 | 実行頻度 |
|---|---|---|---|
| ユニット | 純関数・パーサー・データ変換 | ◎ | push 時 (CI) |
| 統合 | Tauri コマンド・DB マイグレーション・サイドカー HTTP | ◎ | push 時 (CI) |
| 手動 | UI・実発話・実 GPU・配布 | △ | リリース前 |

---

## 2. テスト技術スタック

### 2.1 Rust 側
- **ユニット**: `cargo test`（標準 `#[test]`）
- **非同期テスト**: `#[tokio::test]`
- **HTTP モック**: [wiremock](https://crates.io/crates/wiremock) または `mockito`（サイドカー Mock 用）
- **SQLite テスト**: `:memory:` で都度新規 DB
- **アサーション**: `assert_eq!` 中心、複雑な構造比較に `pretty_assertions`
- **CI 注釈**: GPU/サイドカー実機依存は `#[ignore]` 付与、ラベル `gpu-required` を運用ルール上設ける

### 2.2 TypeScript 側
- **ユニット**: [Vitest](https://vitest.dev/)（Vite ネイティブ、軽量）
- **DOM 操作**: jsdom（Vitest の標準）
- **Tauri IPC モック**: `invoke` 関数をモック注入

### 2.3 CI
- **GitHub Actions** (Windows runner)
- ジョブ: `lint`（fmt/clippy）, `test-rust`（cargo test）, `test-ts`（tsc + vitest）

---

## 3. ユニットテスト戦略

### 3.1 Rust 側の重点対象

| モジュール | テスト対象 | テストの種類 |
|---|---|---|
| `ghost/dict.rs` | 辞書 v3 のパース・バリデート・when 条件評価 | ユニット |
| `ghost/manifest.rs` | ghost.json / shell.json のスキーマ検証、サブ任意の扱い | ユニット |
| `ghost/asset_dnd.rs` | zip slip 検証・サイズ上限・拡張子フィルタ | ユニット |
| `dialogue/banter.rs` | 掛け合いパターン1-4 + question_curiosity の確率 | ユニット |
| `dialogue/low.rs` | rules マッチ・priority 評価・fallback 順序 | ユニット |
| `system/cost.rs` | 月次集計・80%/100% 判定境界・降格復帰 | ユニット |
| `system/notify.rs` | NoticeKind → 辞書キー解決・severity による分岐 | ユニット |
| `tts/preprocess.rs` | カタカナ→ひらがな変換、句切れ挿入 | ユニット |
| `presence/idle.rs` | 30分判定・操作リセット | ユニット |
| `presence/quiet.rs` | 静音条件の組合せ（quiet_mode/フルスクリーン/ポモドーロ） | ユニット |
| `db.rs` | マイグレーション up 順序、各テーブル CRUD | 統合（インメモリ DB） |

### 3.2 TypeScript 側の重点対象

| モジュール | テスト対象 |
|---|---|
| `stage/alphamask.ts` | 8px グリッド生成・座標変換 |
| `interaction/nade.ts` | ホバー往復判定（方向反転・累積移動量・局所性閾値） |
| `interaction/click.ts` | 1回/2-3回/連打の判別、250ms 設定 |
| `dialogue/typewriter.ts` | 速度可変、interrupt 動作 |
| `dialogue/balloon.ts` | 3つ目の吹き出し配置（パターン3/4） |
| `tts/credit.ts` | クレジット文字列の組み立て・常時表示維持 |
| `system/ghost-speech.ts` | dialogue イベント受信 → balloon.show の流れ |
| `menu/context-menu.ts` | バルーン内メニュー: 右クリック導線・項目生成・各アクションの dispatch |

### 3.3 テストデータ
- 辞書: `tests/fixtures/dict/` に最小限の v3 サンプル
- DB: `:memory:` で都度生成、フィクスチャ関数で初期投入
- 設定: 各テストでデフォルト Settings から差分指定

---

## 4. 統合テスト戦略

### 4.1 Tauri コマンドレベル E2E（Rust）

各コマンド（architecture.md §4）に対し、最低限以下を検証:

| コマンド群 | 主要テスト |
|---|---|
| boot | `get_boot_payload` が必須フィールドを満たす、画像 data URL が valid |
| settings | `set_settings` で clamp 適用、`apply_settings` が後処理を呼ぶ |
| dialogue | `send_user_message` が low/advanced で適切な経路を通る（LLM 部分はモック） |
| interaction | `poke` / `nade` の events 探索順、サブ無しゴーストでの分岐 |
| profile | `add_profile` で origin=manual、容量上限到達時の挙動 |
| tts | `synthesize_voice` が slot 振り分け（合成本体は voicevox_core Stub） |
| pomodoro | `start/stop`・状態機械の遷移・節目 emit |
| assets | `dnd_install` が zip/フォルダを正しく検出、衝突時の戻り値 |
| data | `export_data` の JSON 構造、`clear_history` の影響範囲 |

### 4.2 サイドカー HTTP クライアントの Mock テスト（Z3）

```rust
#[tokio::test]
async fn irodori_synthesize_returns_wav() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_bytes(include_bytes!("../fixtures/sample.wav")))
        .mount(&mock).await;

    let engine = IrodoriEngine::new_with_url(mock.uri());
    let wav = engine.synthesize(Slot::Main, "あいうえお", 1.0, 1.0).await.unwrap();
    assert!(wav.starts_with(b"RIFF"));
}
```

- 起動失敗時、ヘルスチェック未応答時、5xx 応答時の各分岐をカバー
- 漢字→ひらがな前処理が入る分岐（needs_kana_preprocess=true）も含めて検証

### 4.3 辞書 v3 バリデータの統合テスト

- ✓ 正しい v3 辞書がパース成功
- ✓ schema_version != 3 で起動時警告
- ✓ when 条件（all_of/any_of/not/not_in_recent/probability）が個別に評価される
- ✓ sub: null が main 単独として処理
- ✓ 必須 system_messages キーの不足を warn する（致命ではない）
- ✓ 未知の events キーは warn して読み飛ばし

### 4.4 DB マイグレーション

- ✓ 空 DB に対し v1 への初期マイグレーションが通る
- ✓ 既存 v1 DB を起動して破壊しない
- ✓ マイグレーション中にエラーが起きてもアプリは起動失敗で安全停止（壊さない）

### 4.5 GPU/サイドカー実機テスト（Z3、手動）

- 開発者の GPU 環境で `cargo test --features gpu-required -- --ignored` で実行
- 対象: 実 Python サイドカー起動、実 GPU 検出、Irodori モデルロード、合成精度の確認（出力 wav が再生可能か）

---

## 5. 手動テストチェックリスト（リリース前必須）

spec.md の §4 機能仕様の構造に従う。各項目に **○/×/該当なし** を記入してリリース判定に使う。

### 5.1 A. キャラクター・ウインドウ

- [ ] **A-1 サブ任意化**: shell.json に sub 定義なしのゴーストで、画面に main のみ表示・掛け合い辞書は無視・撫で/つつき sub 操作不可
- [ ] **A-2 pose プリロード**: 全 pose を起動時に取得、切替時にチラつきなし
- [ ] **A-3 タイプライター速度可変**: 設定で「速い/普通/遅い/一気に」が反映される、変更後の発話で効く
- [ ] **A-3 3 つ目の吹き出し**: パターン3/4 で 3 つ目が独立表示、main/sub と重ならない
- [ ] **A-4 口パク**: TTS 有効時のみ、振幅に応じて動く、TTS 無効時は動かない
- [ ] **A-5 表示スケール**: 0.5〜2.0 でキャラサイズが変わる、**吹き出しのフォント・borderは変わらない（視認性維持）**
- [ ] **A-6 ステージとキャラ位置**: ステージは作業領域下端の全幅帯（ウインドウ自体は動かせない）。キャラを X ドラッグ→終了→再起動で位置復元。保存なし・モニタ消滅時は既定配置（main 右端・sub その左）
- [ ] **A-9 伺か風レイアウト**: 既定配置でメインが右端・サブがその左隣・下端揃え（足元がタスクバー上端）、吹き出しは各キャラの左横（顔の高さ）に出て、しっぽがキャラ側を向く
- [ ] **A-10 キャラ個別移動**: main / sub をそれぞれドラッグで X 方向にのみ移動できる（Y は固定）。ステージ端で clamp。吹き出し表示中はキャラに追従し、画面左端付近では吹き出しが右横に反転する。透明部・吹き出しのドラッグでは何も動かない
- [ ] **A-11 入力導線**: キャラを 1 クリックで、そのキャラが入力促し（辞書 input_prompt、単独発話・TTS あり）を返し、入力欄がそのキャラのバルーン上側に出る。促し吹き出しは入力欄を閉じるまで残る。sub クリックでは sub が反応し sub 側に入力欄。再クリック / Esc で両方閉じる。キャラをドラッグすると入力欄も追従
- [ ] **A-7 セーフモードがない**: 設定パネルにセーフモード項目なし、トレイメニューにも項目なし
- [ ] **A-8 トレイ**: 左クリックで表示/非表示、右クリックメニューに「セーフモード」項目なし

### 5.2 B. 対話

- [ ] **B-1 二モード**: API キー無しで low モード動作、設定で advanced に切替可、API エラー連続で自動降格
- [ ] **B-2 OpenAI 互換のみ**: 設定にプロバイダ選択（OpenAI / Grok / LMStudio / Ollama / カスタム URL）、**Anthropic 選択肢がない**
- [ ] **B-3 辞書 v3 動作**: default ゴーストの辞書が新スキーマで動く、v2 辞書を読み込もうとすると警告で停止 or 既定フォールバック
- [ ] **B-4 パターン1-4**: advanced モードで複数回会話してパターンが切り替わる、辞書系は常にパターン1
- [ ] **B-4 問いかけパターン**: 極低確率で「ユーザーへの問いかけ」発生、ユーザー応答で自然に続く
- [ ] **B-5 長期記憶 + 自動抽出**: 設定で閲覧・追加・削除、advanced 会話後に origin=auto の行が増える
- [ ] **B-5 容量管理**: モード別の挙動（advanced=要約サイクル / low=件数上限削除）が走る
- [ ] **B-7 コスト警告**: 設定で月額上限を低く（例 $0.10）し、80% でゴースト発話、100% で降格＋ゴースト発話
- [ ] **B-8 API キー**: 入力→保存→「保存済み」表示、削除→「未保存」表示、再起動後も維持

### 5.3 C. ユーザー操作

- [ ] **C-1 クリック判別**: 1回=入力欄、2-3回=つつき、4回連打=連打反応
- [ ] **C-2 つつき部位（縦のみ）**: 頭/胸/それ以外の3区分で別の反応、左右の差はない
- [ ] **C-3 撫で**: ボタンなしで上下に往復→反応、ただ通過するだけでは反応しない
- [ ] **C-4 ドラッグ**: 透過部分ではドラッグできない、キャラ本体でドラッグ→そのキャラだけ X 移動（A-10 と同内容）
- [ ] **C-5 バルーンメニュー**: メイン右クリック→前口上がメインのバルーンに出て、同じバルーン内にメニュー項目（モード/静音/ポモドーロ/読み上げ/設定/隠す/終了）。サブ右クリック→サブが誘導セリフ→メインの前口上+メニューへ遷移（サブの吹き出しは残る）。項目実行/Esc/外クリックで閉じ、表示中の左クリックは閉じる専用。キャラ以外の右クリックは無反応

### 5.4 D. 存在感系イベント

- [ ] **D-1 起動挨拶**: 初回 first_boot、2回目以降は時間帯別 boot
- [ ] **D-2 終了挨拶**: トレイ「終了」で quit 発話→終了
- [ ] **D-3 放置反応**: 30分無操作で1回 idle 発火、操作でリセット
- [ ] **D-4 ランダムトーク既定有効**: 既定で 10 分間隔で独り言、設定で 0 にすると無効化
- [ ] **D-5 ポモドーロ**: 集中→休憩→ラウンド遷移、focus_end / pomodoro_done で**ねぎらい台詞**が出る
- [ ] **D-5b ポモドーロGUI**: 右クリック「ポモドーロタイマー」でパネルが開く。開始→バッジ表示、停止でカウント一時停止（残り時間が止まる）→再開で同じ残りから続く、中断で idle に戻る。進行中は時間設定が編集不可、idle 時に変えて開始すると反映され設定パネルにも保存される。閉じてもタイマーは継続、バッジクリックで再度パネルが開く
- [ ] **D-6 時事ネタ**: 既定オフ、オンボーディング同意でオン、advanced モードで独り言に混ざる、暗い見出しは出ない
- [ ] **D-7 更新通知**: 新バージョン検出でゴーストが告知、1回だけ
- [ ] **D-8 静音モード**: ON で自発発話停止、応答は通常通り、起動・終了挨拶は鳴る
- [ ] **D-9 フルスクリーン自動静音**: **既定 OFF**、ON にしたらフルスクリーンアプリ起動で静音

### 5.5 E. 音声・補助

- [ ] **E-1 voicevox_core**: 規約同意→DL→声のテストで発話、再起動後も維持、クレジット「VOICEVOX:話者名」が画面下端に常時表示
- [ ] **E-1 Irodori-TTS（GPU 環境）**: GPU 検出成功→DL 可能、参照音声生成→声のテスト、漢字混じりテキストでも適切に発話
- [ ] **E-1 Irodori-TTS（GPU 無し環境）**: 設定 UI で「GPU 環境でのみ利用可能」表示、DL ボタン無効
- [ ] **E-1 Irodori-TTS GPU 故障時**: サイドカー起動失敗→ voicevox_core にフォールバック+ゴーストが告知
- [ ] **E-2 STT がない**: 設定パネルに音声入力タブなし、入力欄にマイクボタンなし
- [ ] **E-3 ツール**: tools_enabled で「今何時?」「N分後に教えて」「📋ボタン+翻訳」が動く。**tools_enabled=OFF では入力欄に 📋 ボタンが表示されない**（設定変更で即時反映）
- [ ] **E-4 自動起動**: 設定 ON→OS ログイン時起動、OFF→自動起動しない
- [ ] **E-5 エクスポート**: JSON で出力、`include_profile` で記憶を含む/含まない切替
- [ ] **E-5 履歴クリア**: 会話ログ消去、`include_profile` で記憶も削除
- [ ] **E-6 ゴースト/シェル切替**: 設定で別ゴーストに切替→再読込で反映、ホットリロードはない（再起動が必要）
- [ ] **E-6 DnD 展開**: zip ファイルをウインドウにドロップ→展開→再起動案内、フォルダドロップも同様
- [ ] **E-6 DnD セキュリティ**: 不正な zip（パス脱出含む）でエラー、サイズ上限超過でエラー
- [ ] **E-7 ログパネル**: 設定から開ける、最近の会話が新しい順に表示

### 5.6 F. 天気・定例会話（§4.7）

**天気（§4.7.2、M11 実装済み）**:
- [ ] **F-1 地域設定（設定行為=同意）**: 天気節で地名検索（例「横浜市」）→候補が都道府県付きで複数表示→選択で「設定中の地域」に反映され天気が有効化。既定は未設定・無効。送信同意の注記が常設
- [ ] **F-1b 日本語入力の言い換え案内**: 「東京」「渋谷区」等で 0 件のとき「市区町村名・都道府県名で言い換えて…」の案内が出る。検索欄下に言い換えヒントが常設
- [ ] **F-2 出典表示（CC-BY 4.0）**: 地域設定後、画面下端に「天気: Open-Meteo.com」クレジットが常時表示、title に正式表記。設定パネルにも正式表記＋ライセンスリンク。解除で消える
- [ ] **F-3 いま取得**: 「いま取得」で今日/明日の天気（ラベル・気温・降水確率）がプレビュー表示される
- [ ] **F-4 解除＝同意撤回**: 「解除」で地域・地名・天気キャッシュが消え、天気が無効化、クレジットも消える
- [ ] **F-5 座標丸め（プライバシー）**: 設定した座標が小数 1 桁（市区町村粒度）に丸まる。外部送信は気象 API への座標・地名クエリのみ
- [ ] **F-6 降雨の一言**: 「降雨の一言」ON かつ降雨予報の朝（5-11 時）に 1 回、`weather_rain`（当日に時刻付き予定があれば `weather_rain_outing` の強め版）が控えめに出る。🔕 3 回で当該トグル OFF。既定 OFF
- [ ] **F-7 天気無効時の縮退**: 地域未設定だと降雨トグルは無効＋「先に地域を設定してください」。天気が無効/取得失敗/古いキャッシュ（6h 超）のときは天気項目を黙って省く
- [ ] **F-8 オフライン**: ネット遮断で取得失敗しても既存キャッシュを使う。アプリは落ちない
**定例会話（§4.7.1、M12 実装済み）**:
- [ ] **F-9 朝の定例会話**: 朝を有効化し時刻を直近に設定 → 設定時刻以降に PC を使っている最初のタイミングで、キャラが今日の予定・ToDo 件数・未完了リマインダー・天気（有効時）をまとめて 1 回話す。全材料が空でも短いあいさつは出る（空リストは読み上げない）
- [ ] **F-10 夜の定例会話**: 夜を有効化 → 設定時刻以降の在席時に、今日の完了数・残り・明日の予定・明日の天気をまとめて 1 回話す
- [ ] **F-11 発火規則（失効・在席）**: 設定時刻きっかりではなく「設定時刻以降の初回在席」で配達。設定時刻から 6 時間で当日分は失効（終日 PC を触らず夜に開いても朝の定例が夜に鳴らない）。1 枠 1 日 1 回、日付が変われば前日分は消化
- [ ] **F-12 曜日・独立設定**: 朝/夜それぞれ有効・時刻・曜日を独立設定。曜日トグルで外した曜日は鳴らない
- [ ] **F-13 吸収**: 朝の定例会話が有効な間、朝の ToDo 件数告知・降雨の一言は単独発火しない（定例会話に吸収）。朝を無効に戻すと従来どおり単独発火
- [ ] **F-14 夜間静音重なり警告**: 枠の時刻を夜間静音帯内に置くと設定 UI に警告が出る（保存は止めない）。実際その枠は夜間静音優先で配達されない
- [ ] **F-15 🔕 と advanced**: 定例会話中の 🔕 を 3 回で当該枠が OFF になる（Situation* の間隔バックオフはしない）。advanced モードでは同じ材料が会話調の言い回しになり、LLM 失敗・降格中は定型文にフォールバック

### 5.7 横断方針

- [ ] **ゴースト発話原則**: コスト警告/降格/更新/DL完了/失敗が**ゴーストの口から**告知される（トーストのみではない）
- [ ] **無サーバ要件**: アプリを起動するだけで TTS が鳴る（VOICEVOX エディタ等の別アプリを起動しない）
- [ ] **プライバシー**: 発話テキストが LLM プロバイダ以外に送信されない、時事ネタの検索は明示同意済みのキーワードのみ

---

### 5.8 機能別の実機検証記録（旧 docs/quality_checklist.md）

実機で人が確認した項目の記録（2026-07-24 の docs 整理で `quality_checklist.md` から本節へ統合）。
§5.1〜5.7 のリリース前チェックとは別に、機能導入時の実機検証結果をそのまま残し、以後のリリースでは
リグレッション観点の参照元として使う。

#### M4c Irodori-TTS 実機検証（Phase G）

実機要件: Windows 10/11 x64 / NVIDIA GPU (CUDA 12.x 対応、VRAM 4GB 以上推奨) / ディスク空き 6 GB 以上 / インターネット接続 (HF / PyTorch wheel index / GitHub)

**G1. 前提セットアップ**
- [x] `npm run tauri dev` で ugg が起動する (voicevox 経路に回帰なし)
- [x] 設定パネル「音声 (Irodori-TTS / 高品質モード)」セクションが表示される
- [x] 「GPU 検出」欄に実機 GPU 名が表示される (例: "NVIDIA GeForce RTX 4070")
- [x] GPU 可環境では DL ボタン enabled / 実モデルチェック disabled (資産未 DL のため) — 2026-06-28 実機検証 ✅

**G2. Python ランタイム + 共通依存 DL (Phase C 範囲)**
- [x] 「ランタイムをダウンロード」を押すと確認ダイアログが出る (要 z-index 修正: `#ugg-confirm-panel` を 300 に。2026-06-28 セッションで発見)
- [x] 確認 → 進捗欄に逐次ログが出る (Embeddable Python 3.11.9 / pip / fastapi / torch cu128 / Irodori-TTS runtime / HF モデル本体)
- [x] DL 完了で `%APPDATA%\ugg\irodori\python\python.exe` と `Lib\site-packages\torch / irodori_tts / dacvae / silentcipher / fastapi` が存在する (合計 ~5GB)
- [x] DL 完了で `%APPDATA%\ugg\irodori\model\Aratako__Irodori-TTS-500M-v3 / -v2-VoiceDesign / Aratako__Semantic-DACVAE-Japanese-32dim` が揃う (合計 4.3GB)
- [x] 「Irodori 資産」欄が "導入済み" に変わる + 「実モデルを使う (β)」 toggle が enabled になる (※ DL 完了直後にパネルを開きっぱなしの場合、再オープンで更新される — UX 改善余地)
- [x] notify: ゴーストが `irodori_dl_complete` (「高品質モードの準備もできたよ！」) を発話する
- [ ] (任意) DL 失敗時 (例: ネット切断) は `irodori_dl_failed` をゴーストが発話する

**G3. サイドカー起動 (モックモード, Phase D)**
- [x] 設定で `tts_engine = irodori` に切替 + 「実モデルを使う (β)」OFF (2026-06-28 実機検証 ✅)
- [x] 参照音声生成 (キャプション入力 → 生成) で main の参照音声が `%APPDATA%\ugg\irodori\refs\main_<ts>.wav` に保存される (44KB = 1秒無音 wav、`make_mock_voice_ref_wav` 仕様通り)
- [x] 「プレビュー」で 440Hz 正弦波 ~1 秒程度が再生される
- [x] `synthesize_voice(tts_engine=irodori)` 経路で短文を発話 → 文字数に応じた長さの正弦波が再生される (80ms/文字)
- [x] 5 分以上放置 → サイドカープロセス (python.exe) が自動 kill される (実測: 監視開始から 8 分 3 秒、monologue=0 で測定)
- [x] トレイ「終了」/コンテキストメニュー「終了」で python.exe の残骸が出ない (実測: 終了操作から 29 秒で消失。POST /shutdown 経路成功)

**G4. ヘルスチェック (Phase G)**
- [x] サイドカー起動中に Task Manager から手動 kill → **90 秒以内**にゴーストが `irodori_unavailable` を発話 (30 秒間隔 × 3 回連続失敗で発火、最悪 60〜90 秒) — 2026-06-28 実機検証 ✅
- [x] 次回 `synthesize_voice` 呼び出しで **disable_until (20 分) に従い voicevox 経路へ自動 fallback** される (0ee4c80 で実装、auto-restart は GPU 永続不在環境での 90 秒 churn 防止のため抑制) ✅
- [ ] (任意・GPU 必須環境) 実モデル経路 (`tts_irodori_use_real_model=true`) でサイドカー起動したが GPU が取れなかった場合 (`/health` が 503) 、次回 `health_ping` で即 `irodori_unavailable` が発火する

**G5. 実モデル結線 (Phase G + 実機調整)**
- [x] 「実モデルを使う (β)」を ON に切替 (GPU + 資産 (`irodori_tts` パッケージ込み) が揃っているときのみ有効化される) — 2026-06-28 実機検証 ✅
- [x] 次回 `synthesize_voice` / `voice_ref_generate` 呼び出しでサイドカーが実モデル経路で起動 (`--no-download --mock なし`、~2GB メモリ確保)
- [x] HF モデルは G2 step 6 (`install_irodori_models`) で事前 DL 済 (約 4.3 GB)
- [x] 参照音声生成 (VoiceDesign): キャプションから自然なキャラ音声 wav が生成される
- [x] 通常合成: 参照音声のキャラ声で読み上げ
- [x] 漢字混じり文章で発話 → preprocess (voicevox OpenJtalk) で漢字→かな変換が効き、自然な読み上げ
- [x] ※ G5-2 実機検証時に 2 件のバグ発見・修正済 (commit c3dffe0): (1) `_audio_to_wav_bytes` が torch.Tensor `(channels, samples)` を soundfile に渡せなかった、(2) `dacvae` の transitive 依存 `descript-audiotools` が未 install

**G5+. 絵文字アノテーション (Irodori-TTS V3 感情制御) — テキスト読み上げ R4 で消化 (2026-07-04)**
- [x] 実モデル経路で「😊今日はいい天気だね」を発話 → 楽しげなトーンが乗る (絵文字自体は読み上げられない) ✅
- [x] 「うぅ…😭ひどいよ…😭」→ 悲しげ/嗚咽調になる ✅
- [x] 同一絵文字の連続 (「🤧🤧ごめん、風邪引いちゃって」) で効果が強調される ✅
- [x] 非対応絵文字 (🍕 等) を含めても発話が壊れない (従来通り無視される) ✅
- [x] voicevox 経路では絵文字入りテキストでも従来挙動のまま (回帰なし) ✅

**G6. フォールバック**
- [x] GPU 不可環境で `tts_engine = irodori` を選んでも、UI gate (G1-4 で確認済) + 0ee4c80 の二段 gate (irodori option 自体を disabled、選択中なら自動で voicevox_core に倒す) により事前抑制
- [x] サイドカー起動失敗時に notify(`irodori_unavailable`) が発火 (G4-1 で確認済)、その後は auto fallback で voicevox 経路 (G4-2 で確認済) + 設定の手動切替でも voicevox に即時復旧可 (2026-06-28 実機 ✅)

#### テキスト読み上げツール（docs/text-reader-spec.md §5.2）

- [x] R1: コンテキストメニュー →「テキスト読み上げ」でパネルが開く (2026-07-04 実機 ✅)
- [x] R2: パネル表示中に UTF-8 の .txt を DnD → 先頭から読み上げ、進捗 n/m が進む ✅
- [x] R3: 長文 .txt → チャンク間で途切れず最後まで読む。生成失敗しない ✅
- [x] R4: **Irodori 実モデル + 絵文字入り .txt** (😊/😭/🤧🤧/👂/🍕) → エモートが音声に乗る。🍕 は無視。**G5+ もこれで消化 ✅**
- [x] R5: 読み上げ中に停止ボタン → 即停止、進捗リセット ✅
- [x] R6: 読み上げ中に放置 → 独り言が出ない (抑制) ✅
- [x] R7: Shift_JIS の .txt → 文字化けせず読める ✅
- [x] R8: パネル非表示時に .txt を DnD → 何も起きない。zip の DnD 導入は従来通り ✅
- [x] R9: パネル (設定/読み上げ等) 表示中にキャラ上でカーソルを往復 → 撫で発話が出ない (2026-07-04 実機 ✅)
- [x] R10: チャンク境界で約 0.5 秒のポーズが入り、改行跨ぎが不自然でない (2026-07-04 実機 ✅)

#### テキスト読み上げツール 台本形式（docs/script-reader-spec.md §5.2）

**前提**: S4・S9・S11 は Irodori 実モデル導入済み環境で行う。未導入環境では代替として
sidecar ログで `SamplingRequest.caption` に値が透過されることを確認する (S4')。

**実機結果**: S1〜S12 全項目 PASS (2026-07-04 実機、dev ビルド)。

- [x] S1: 台本 .md を DnD (voicevox) → host 行=main の声、guest 行=sub の声で交互に読む
- [x] S2: speed 指定行 (±0.15) → 該当行だけ速度が変わる。実効レートは [0.5, 2.0] に収まる
- [x] S3: pause_after 0.6 の行 → 行の後の間が明確に長い
- [x] S4: **Irodori 実モデル + caption 行** (「驚いて大声で」等) → 演技が音声に乗る。sidecar ログの SamplingRequest.caption に値が入る
- [x] S5: voicevox で caption 入り台本 → エラーにならず読む + パネルに注記が**再生終了まで**表示。caption なし台本、および Irodori 実モデル可判定の環境では注記が出ない
- [x] S6: 不正台本 (未定義話者 / ref_wav / JSON 破損 / speed 範囲外 / 通常の Markdown 文書) → 再生開始せず、種別ごとの文言 (§2.3/§2.8) で原因が分かる
- [x] S7: プレーン .txt (回帰) → v0.1.1 と同一挙動 (順序・速度・間・停止)
- [x] S8: 台本読み上げ中の停止/クローズ/自発発話抑制 (回帰) → .txt 読みと同一挙動。**pause 待機中に停止しても残りの pause・次チャンクが実行されない**
- [x] S9: Irodori 実モデルで sub の参照音声を削除してから sub 使用台本を DnD → 再生開始前にエラー (§2.7 の文言)。1 チャンクも再生されない
- [x] S10: 長い 1 行 (300 字) を含む台本 → 行の途中に不自然な間が入らない (中間断片 pause=0)
- [x] S11: Irodori 実モデルで caption なし台本 → v0.1.1 相当の合成品質 (caption=None 透過の回帰)
- [x] S12: 台本読み上げ中にゴーストへチャット (回帰) → 応答発話がブロックされない (既存仕様)

---

## 6. CI 構成（X1: GitHub Actions 最小 CI）

### 6.1 ワークフロー定義（雛形）

```yaml
# .github/workflows/ci.yml
name: CI
on:
  push:
    branches: [main, develop]
  pull_request:

jobs:
  test-rust:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --manifest-path src-tauri/Cargo.toml --check
      - run: cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
      - run: cargo test --manifest-path src-tauri/Cargo.toml --no-default-features
        # -- --ignored は対象外（GPU/サイドカー実機は手動）

  test-ts:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 20
      - run: npm ci
      - run: npx tsc --noEmit
      - run: npx vitest run
```

### 6.2 リリースワークフロー（手動トリガ or タグ）

```yaml
# .github/workflows/release.yml（タグ push 時）
on:
  push:
    tags: ['v*']
jobs:
  build:
    runs-on: windows-latest
    steps:
      - ...
      - run: npm ci
      - run: npm run tauri build
      - uses: actions/upload-artifact@v4
        with:
          name: installer
          path: src-tauri/target/release/bundle/nsis/*.exe
```

### 6.3 CI で**やらない**こと
- GPU テスト（Z3）
- 実 Python サイドカー起動
- voicevox_core 実資産 DL
- LLM プロバイダ実接続

これらは手動テスト（§5）で担保。

---

## 7. リリース前手順

### 7.1 チェックリストの完走
- [ ] §5 手動テストチェックリスト全項目を実機で実施
- [ ] 結果を docs/release-notes/<version>.md に記録（合否＋備考）

### 7.2 ビルド・配布物
- [ ] バージョン番号を `tauri.conf.json` / `Cargo.toml` / `package.json` で一致
- [ ] `npm run tauri build` 成功
- [ ] インストーラの FileVersion / ProductVersion がタグと一致
- [ ] SHA-256 ハッシュを記録
- [ ] クリーンな Windows VM でインストール→起動→主要動作確認

### 7.3 アップデートフィード
- [ ] `update_feed_url` の JSON を更新（既定 OFF だが配信する場合）

---

## 8. リグレッション防止

### 8.1 「Phase 1 で削除したものが復活していないか」
- ★ Anthropic 専用ルートが Cargo.toml の依存にいないこと
- ★ STT 関連コード（stt.rs / stt.ts）がリポジトリに存在しないこと
- ★ セーフモード関連のフィールド・コマンドが存在しないこと
- ★ openai_compat / voicevox_http エンジンのコードが残っていないこと
- ★ context_summaries テーブルが DB スキーマに存在しないこと

これらは静的検査スクリプト（grep ベース）で CI に組み込む。

### 8.2 「Phase 1 で追加したものが消えていないか」
- ★ Irodori エンジン・参照音声・前処理のコードが存在
- ★ system_messages セクションが辞書スキーマに定義されている
- ★ 右クリックメニュー（バルーン内）がキャラ右クリックで開く
- ★ schema_version=3 が辞書バリデータで要求される
- ★ system/notify.rs が存在し、各発火点から呼ばれている

### 8.3 ベースライン比較
- リリースごとに `cargo bloat` 等でバイナリサイズを記録
- インストーラサイズが急増したら調査（不要な依存の混入チェック）

---

## 9. テストデータ管理

### 9.1 フィクスチャ
```
tests/
├── fixtures/
│   ├── dict/
│   │   ├── valid_v3.yaml
│   │   ├── empty.yaml
│   │   ├── malformed.yaml
│   │   └── with_subs_null.yaml
│   ├── shell/
│   │   ├── with_sub.json
│   │   └── main_only.json
│   ├── llm/
│   │   ├── mock_chat_response.json
│   │   └── mock_summary_response.json
│   └── audio/
│       └── sample.wav
```

### 9.2 機微情報の取扱い
- 実 API キー・PAT はテストに含めない
- モックレスポンスのみで完結
- 個人情報（実 user_profile データ）もテストでは合成データのみ

---

## 10. 既知の制約

| 制約 | 影響 | 緩和策 |
|---|---|---|
| WebView2 内 UI は Vitest + jsdom では完全再現できない | UI 回帰は手動依存 | §5 手動チェックリストでカバー |
| Tauri の `invoke` は Mock 注入が必要 | TS テストで実コマンド呼べない | テスト用 IPC ハーネスを用意（テストヘルパー） |
| GPU 環境テストが CI で回らない | サイドカー実機回帰検出が遅れる | リリース前手動テスト + 公開後の早期 hotfix 体制 |
| Windows 専用なので CI runner も Windows | 実行時間が長め（Linux runner より遅い） | キャッシュ（cargo registry / target）を活用 |

---

## 11. 改訂履歴

| 日付 | 版 | 内容 |
|---|---|---|
| 2026-06-18 | v1 | Phase 3 対話確定（X1/Y2/Z3）に基づき初版 |
| 2026-07-24 | v1.1 | §5 に F 節（天気・定例会話 §4.7）を新設し M11 天気の手動チェック F-1〜F-8 を追加（横断方針を 5.7 へ繰り下げ）。定例会話 §4.7.1 は M12 で追記予定。**既知の負債**: Tier S（§4.6 リマインダー/ToDo/状況発話/カレンダー）の手動項目は §5 に未追加のまま（M7〜M10 実装時に取り残し）。 |
| 2026-07-24 | v1.2 | M12（定例会話）実装済みを受けて §5 F 節に定例会話の手動チェック F-9〜F-15 を追加。v0.3（§4.7）の手動チェックが揃った。 |
| 2026-07-24 | v1.3 | docs 整理: `docs/quality_checklist.md`（機能別の実機検証記録 = Irodori G1〜G6 / 読み上げ R1〜R10 / 台本 S1〜S12）を §5.8 として本書へ統合し、同ファイルを廃止。§0 の v0.0.3 quality_checklist 参照（_legacy-v003 削除に伴い）も更新。 |
