# コンテキスト索引 — 詳細情報の所在

| 知りたいこと | 場所 |
|---|---|
| **v0.0.3 プロトタイプ完成版の現状スナップショット**（出発点） | docs/baseline-v0.0.3.md |
| プロダクト要件・技術的課題と対策の正本 | docs/spec.md |
| モジュール構成・Tauri コマンド/イベント契約・DB スキーマ | docs/architecture.md |
| ゴースト定義スキーマの実例 | ghosts/default/ghost.json, ghosts/default/dic/main.yaml |
| シェル定義スキーマの実例 | shells/default/shell.json |
| モデルルーティング方針 | docs/ai_model_routing.md |
| サブエージェントへの依頼文テンプレ | docs/subagent_prompt_templates.md |
| リリース前チェックリスト | docs/quality_checklist.md |
| Rust 依存クレートとバージョン | src-tauri/Cargo.toml |
| ウインドウ生成・透過・セーフモード設定 | src-tauri/src/window_ctl.rs |
| フロントエンドのエントリポイント | src/main.ts |
| TTSエンジン抽象（埋め込み/HTTP/OpenAI互換の振り分け） | src-tauri/src/tts_engine.rs |
| 埋め込み voicevox_core FFI（libloading / 初期化 / 合成 / metas） | src-tauri/src/voicevox_ffi.rs |
| 公式ダウンローダ取得・規約同意自動応答・進捗 | src-tauri/src/voicevox_download.rs |
| 外部 VOICEVOX エンジン (HTTP) 連携 | src-tauri/src/voicevox.rs |
| OpenAI 互換 TTS（VoiceDesign キャプション = instructions） | src-tauri/src/openai_tts.rs |
| フロント TTS（キュー・再生・口パク振幅解析・クレジット表示） | src/tts.ts, src/main.ts (updateTtsCredit) |
