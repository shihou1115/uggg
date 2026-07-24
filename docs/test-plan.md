# ugg テスト計画（test-plan.md v1）

**フェーズ**: 本開発 Phase 3 確定版
**作成日**: 2026-06-18
**根拠**: [spec.md](spec.md) v1 / [architecture.md](architecture.md) v1
**位置付け**: **テスト戦略の正本**。Phase 4 実装着手の前提条件として、コードを書く前に本書のテスト枠組みが整っていること。

---

## 0. 本書の使い方

- 本書はテストの **戦略・分類・配置** を定める。**個別テストケースの全列挙はしない**（実装段階で各モジュールの責任で追記）。
- 「**手動テストチェックリスト**」（§7）はリリース前の必須通過項目。
- 既存の v0.0.3 quality_checklist は本書 §7 の祖型として参照（[docs/_legacy-v003/quality_checklist.md](_legacy-v003/quality_checklist.md)）。

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
- 定例会話（§4.7.1、朝・夜の定例会話）は **M12 で実装予定**（本節に追記する）

### 5.7 横断方針

- [ ] **ゴースト発話原則**: コスト警告/降格/更新/DL完了/失敗が**ゴーストの口から**告知される（トーストのみではない）
- [ ] **無サーバ要件**: アプリを起動するだけで TTS が鳴る（VOICEVOX エディタ等の別アプリを起動しない）
- [ ] **プライバシー**: 発話テキストが LLM プロバイダ以外に送信されない、時事ネタの検索は明示同意済みのキーワードのみ

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
