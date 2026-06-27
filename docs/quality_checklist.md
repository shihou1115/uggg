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
- [x] `npm run tauri dev` で ugg が起動する (voicevox 経路に回帰なし)
- [x] 設定パネル「音声 (Irodori-TTS / 高品質モード)」セクションが表示される
- [x] 「GPU 検出」欄に実機 GPU 名が表示される (例: "NVIDIA GeForce RTX 4070")
- [x] GPU 可環境では DL ボタン enabled / 実モデルチェック disabled (資産未 DL のため) — 2026-06-28 実機検証 ✅

### G2. Python ランタイム + 共通依存 DL (Phase C 範囲)
- [x] 「ランタイムをダウンロード」を押すと確認ダイアログが出る (要 z-index 修正: `#ugg-confirm-panel` を 300 に。2026-06-28 セッションで発見)
- [x] 確認 → 進捗欄に逐次ログが出る (Embeddable Python 3.11.9 / pip / fastapi / torch cu128 / Irodori-TTS runtime / HF モデル本体)
- [x] DL 完了で `%APPDATA%\ugg\irodori\python\python.exe` と `Lib\site-packages\torch / irodori_tts / dacvae / silentcipher / fastapi` が存在する (合計 ~5GB)
- [x] DL 完了で `%APPDATA%\ugg\irodori\model\Aratako__Irodori-TTS-500M-v3 / -v2-VoiceDesign / Aratako__Semantic-DACVAE-Japanese-32dim` が揃う (合計 4.3GB)
- [x] 「Irodori 資産」欄が "導入済み" に変わる + 「実モデルを使う (β)」 toggle が enabled になる (※ DL 完了直後にパネルを開きっぱなしの場合、再オープンで更新される — UX 改善余地)
- [x] notify: ゴーストが `irodori_dl_complete` (「高品質モードの準備もできたよ！」) を発話する
- [ ] (任意) DL 失敗時 (例: ネット切断) は `irodori_dl_failed` をゴーストが発話する

### G3. サイドカー起動 (モックモード, Phase D)
- [x] 設定で `tts_engine = irodori` に切替 + 「実モデルを使う (β)」OFF (2026-06-28 実機検証 ✅)
- [x] 参照音声生成 (キャプション入力 → 生成) で main の参照音声が `%APPDATA%\ugg\irodori\refs\main_<ts>.wav` に保存される (44KB = 1秒無音 wav、`make_mock_voice_ref_wav` 仕様通り)
- [x] 「プレビュー」で 440Hz 正弦波 ~1 秒程度が再生される
- [x] `synthesize_voice(tts_engine=irodori)` 経路で短文を発話 → 文字数に応じた長さの正弦波が再生される (80ms/文字)
- [x] 5 分以上放置 → サイドカープロセス (python.exe) が自動 kill される (実測: 監視開始から 8 分 3 秒、monologue=0 で測定)
- [x] トレイ「終了」/コンテキストメニュー「終了」で python.exe の残骸が出ない (実測: 終了操作から 29 秒で消失。POST /shutdown 経路成功)

### G4. ヘルスチェック (Phase G)
- [x] サイドカー起動中に Task Manager から手動 kill → **90 秒以内**にゴーストが `irodori_unavailable` を発話 (30 秒間隔 × 3 回連続失敗で発火、最悪 60〜90 秒) — 2026-06-28 実機検証 ✅
- [x] 次回 `synthesize_voice` 呼び出しで **disable_until (20 分) に従い voicevox 経路へ自動 fallback** される (0ee4c80 で実装、auto-restart は GPU 永続不在環境での 90 秒 churn 防止のため抑制) ✅
- [ ] (任意・GPU 必須環境) 実モデル経路 (`tts_irodori_use_real_model=true`) でサイドカー起動したが GPU が取れなかった場合 (`/health` が 503) 、次回 `health_ping` で即 `irodori_unavailable` が発火する

### G5. 実モデル結線 (Phase G + 実機調整)
- [x] 「実モデルを使う (β)」を ON に切替 (GPU + 資産 (`irodori_tts` パッケージ込み) が揃っているときのみ有効化される) — 2026-06-28 実機検証 ✅
- [x] 次回 `synthesize_voice` / `voice_ref_generate` 呼び出しでサイドカーが実モデル経路で起動 (`--no-download --mock なし`、~2GB メモリ確保)
- [x] HF モデルは G2 step 6 (`install_irodori_models`) で事前 DL 済 (約 4.3 GB)
- [x] 参照音声生成 (VoiceDesign): キャプションから自然なキャラ音声 wav が生成される
- [x] 通常合成: 参照音声のキャラ声で読み上げ
- [x] 漢字混じり文章で発話 → preprocess (voicevox OpenJtalk) で漢字→かな変換が効き、自然な読み上げ
- [x] ※ G5-2 実機検証時に 2 件のバグ発見・修正済 (commit c3dffe0): (1) `_audio_to_wav_bytes` が torch.Tensor `(channels, samples)` を soundfile に渡せなかった、(2) `dacvae` の transitive 依存 `descript-audiotools` が未 install

### G6. フォールバック
- [x] GPU 不可環境で `tts_engine = irodori` を選んでも、UI gate (G1-4 で確認済) + 0ee4c80 の二段 gate (irodori option 自体を disabled、選択中なら自動で voicevox_core に倒す) により事前抑制
- [x] サイドカー起動失敗時に notify(`irodori_unavailable`) が発火 (G4-1 で確認済)、その後は auto fallback で voicevox 経路 (G4-2 で確認済) + 設定の手動切替でも voicevox に即時復旧可 (2026-06-28 実機 ✅)

---

## 既存マイルストーン (M0〜M4b) の手動チェックは `docs/test-plan.md` §5 を参照。
