# 品質チェックリスト (M4c Phase G 起点)

実機検証や手動 QA で使うチェック項目を集約する。`docs/test-plan.md` の自動テスト計画とは独立で、本ドキュメントは**実機で人が確認する**項目に絞る。

---

## M4c Irodori-TTS 実機検証 (Phase G)

実機要件:
- Windows 10/11 x64
- NVIDIA GPU (CUDA 12.x 対応、VRAM 4GB 以上推奨)
- ディスク空き容量 6 GB 以上 (Python + PyTorch + Irodori モデル + コーデック)
- インターネット接続 (HF / PyTorch wheel index / GitHub)

### G1. 前提セットアップ
- [ ] `npm run tauri dev` で ugg が起動する (voicevox 経路に回帰なし)
- [ ] 設定パネル「音声 (Irodori-TTS / 高品質モード)」セクションが表示される
- [ ] 「GPU 検出」欄に実機 GPU 名が表示される (例: "NVIDIA GeForce RTX 4070")
- [ ] GPU 不可環境ではダウンロードボタンが disabled、reason テキストが日本語で表示される

### G2. Python ランタイム + 共通依存 DL (Phase C 範囲)
- [ ] 「ランタイムをダウンロード」を押すと確認ダイアログが出る
- [ ] 確認 → 進捗欄に逐次ログが出る (Embeddable Python / pip / fastapi / torch …)
- [ ] DL 完了で `%APPDATA%\ugg\irodori\python\python.exe` と `Lib\site-packages\torch` が存在する
- [ ] 「Python ランタイム」欄が "導入済み" に変わる
- [ ] notify: ゴーストが `irodori_dl_complete` を発話する
- [ ] DL 失敗時 (例: ネット切断) は `irodori_dl_failed` をゴーストが発話する

### G3. サイドカー起動 (モックモード, Phase D)
- [ ] 設定で `tts_engine = irodori` に切替 + 「実モデルを使う (β)」OFF
- [ ] 参照音声生成 (キャプション入力 → 生成) で main の参照音声が `%APPDATA%\ugg\irodori\refs\main_<ts>.wav` に保存される
- [ ] 「プレビュー」で 1 秒程度の正弦波が再生される
- [ ] `synthesize_voice(tts_engine=irodori)` 経路で短文を発話 → 正弦波が再生される
- [ ] 5 分以上放置 → サイドカープロセス (Task Manager の python.exe) が自動 kill される (Phase E アイドル監視)
- [ ] トレイ「終了」/コンテキストメニュー「終了」で python.exe の残骸が出ない

### G4. ヘルスチェック (Phase G)
- [ ] サイドカー起動中に Task Manager から手動 kill → **90 秒以内**にゴーストが `irodori_unavailable` を発話 (30 秒間隔 × 3 回連続失敗で発火、最悪 60〜90 秒)
- [ ] 次回 `synthesize_voice` 呼び出しで自動再起動される
- [ ] 実モデル経路 (`tts_irodori_use_real_model=true`) でサイドカー起動したが GPU が取れなかった場合 (`/health` が `gpu: null` を返す)、次回 `health_ping` で即 `irodori_unavailable` が発火する

### G5. 実モデル結線 (Phase G + 実機調整)
- [ ] 「実モデルを使う (β)」を ON に切替 (GPU + 資産 (`irodori_tts` パッケージ込み) が揃っているときのみ有効化される)
- [ ] 次回 `synthesize_voice` / `voice_ref_generate` 呼び出しでサイドカーが実モデル経路で起動
- [ ] 初回起動時に Aratako/Irodori-TTS 系モデル (約 2-4 GB) が `%APPDATA%\ugg\irodori\model\` に DL される (進捗は `[hf-download] ...` 行が `irodori-download` イベント経由で設定パネル進捗欄に流れる)
- [ ] 参照音声生成: キャプションから自然な音声 (キャラクター性が反映された短い wav) が生成される
- [ ] 通常合成: 上記参照音声を使った合成で、音声がキャプションに沿った声質で読み上げられる
- [ ] 漢字混じり文章で発話 → preprocess (voicevox OpenJtalk) が効いていることを確認 (聞き取りやすさ)
- [ ] ※ `RealModelBackend.synthesize` / `generate_voice_ref` は upstream `irodori_tts.inference_runtime.InferenceRuntime` API で結線済 (commit 9118205)。`InferenceRuntime` 初期化や `SamplingRequest` のデフォルト引数が upstream の最新コードと整合しなければ実機でチューニングする

### G6. フォールバック
- [ ] GPU 不可環境で `tts_engine = irodori` を選んでも、明示エラーで voicevox 経路に切替えるよう案内される
- [ ] サイドカー起動失敗時に notify(`irodori_unavailable`) が発火し、voicevox に手動切替で復旧できる

---

## 既存マイルストーン (M0〜M4b) の手動チェックは `docs/test-plan.md` §5 を参照。
