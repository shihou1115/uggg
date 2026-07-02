# ugg — デスクトップ常駐コンパニオンアプリ（本開発）

「伺か」コンセプトを Tauri v2 (Rust + TypeScript) で再構築したデスクトップマスコット。
本リポジトリは **C:\claude\ugga（プロトタイプ v0.0.3）** を経て、**仕様を明確化したうえで作り直す**本開発フェーズ。

## 開発方針（プロトタイプの教訓を踏まえて）

v0.0.3 で得た主な負債:
- 仕様が曖昧なままコーディングを進めた結果、途中変更が相次ぎ、後付け抽象が肥大した。
- TTS 周りに 3 エンジン抽象が積み上がった（core/http/openai_compat）。
- AppState のフィールド数が肥大し、責務の分割が曖昧になった。
- DB テーブルとコマンドの数が増え、相互依存の見通しが悪化した。

本開発ではこれを避けるため、次の規律を厳守する:

1. **仕様確定前にコードを書かない**。 Phase 1〜3 が終わるまで `src/`・`src-tauri/src/` には**コードを追加しない**。
2. **「将来のために」を入れない**。 spec.md にある機能だけ書く。後付けの抽象化を禁止。
3. **コマンド/イベント/設定フィールドを増やすときは spec の改訂を伴う**。 場当たりの追加を禁止。
4. **v0.0.3 を直接コピーしない**。 [docs/_legacy-v003/](docs/_legacy-v003/) は**参考資料**としてのみ扱い、コードや構造をそのまま流用しない。
5. **ghost / shell 資産は流用するが Phase 1 で見直しの対象**。 辞書 events キーの整理も Phase 1 で実施。

## 進行ロードマップ（Phase 4 着手前）

| Phase | 内容 | 成果物 | 状態 |
|---|---|---|---|
| 0 | 環境セットアップ | このリポジトリの初期構成 | ✅ |
| 1 | 仕様再定義（機能の取捨選択・コアコンセプト言語化） | docs/spec.md | ✅ |
| 2 | アーキテクチャ設計（TTS再設計・状態管理再設計・DB再設計） | docs/architecture.md | ✅ |
| 3 | テスト計画 | docs/test-plan.md | ✅ |
| **4** | **実装着手**（垂直スライス・M0〜M6） | src/, src-tauri/src/ | ✅ M0〜M6 完了、**v0.1.0 リリース済**（2026-07-02、タグ `v0.1.0`） |

## 採用済みの技術選定（Phase 1〜2 で再調査しない）

- プラットフォーム: **Tauri v2 + Vanilla TypeScript + Rust + SQLite**、**Windows 専用**
- TTS 方式: **voicevox_core 埋め込み**（libloading + プリビルド C API、CPU 合成、無サーバ）
- クリック透過: **アルファマスク方式**（フロントで 8px グリッド合成 → Rust 側ポーリング）
- 対話: **二モード**（low=辞書 / advanced=LLM）、辞書スキーマは v2 形式
- 配布: **NSIS インストーラ**（currentUser モード）
- データ: **SQLite + keyring + ファイル資産（ghosts/shells）**

技術選定の理由・経緯は [docs/_legacy-v003/baseline-v0.0.3.md](docs/_legacy-v003/baseline-v0.0.3.md) を参照。

## ビルド・検証コマンド（実装着手後に使用）

- `npm run tauri dev` — 開発起動
- `cargo check`（src-tauri/ 内で実行）— Rust 型検査
- `npx tsc --noEmit` — TypeScript 型検査
- `npm run tauri build` — リリースビルド（NSIS インストーラ生成）。**リリース作業時は `.claude/skills/releasing-ugg` の手順に従うこと**（dev で動いても配布版が壊れる罠の再発防止）

## Model Routing / Token ROI
- Fable 5（最大推論）= オーケストレーター、設計・判断・監査・レビュー・最終品質確認
- Opus = 深い推論サブエージェント
- Sonnet = 機械的な作業サブエージェント
- 調査、整理、既存コード読解、単純実装、テスト追加、差分確認、軽微な修正は Opus/Sonnet をサブエージェントとして切り出す
- 実装難易度が高い箇所、設計判断を誤ると手戻りが大きい箇所、UX・アーキテクチャに影響する箇所は Fable5 が直接扱ってよい。
- サブエージェントには必要最小限のコンテキストだけを渡す
- 詳細: [docs/_legacy-v003/ai_model_routing.md](docs/_legacy-v003/ai_model_routing.md)

## ドキュメント索引

| ファイル | 役割 | 状態 |
|---|---|---|
| docs/spec.md | 要件の正本 | v1 ✅ |
| docs/architecture.md | モジュール構成・契約・設計判断 | v1 ✅ |
| docs/test-plan.md | テスト戦略・手動チェックリスト | v1 ✅ |
| docs/implementation-plan.md | 実装計画（M0〜M6 マイルストーン） | v1 ✅ |
| [docs/_legacy-v003/baseline-v0.0.3.md](docs/_legacy-v003/baseline-v0.0.3.md) | **v0.0.3 機能・契約・残課題の網羅スナップショット** | 参照用 |
| [docs/_legacy-v003/spec.md](docs/_legacy-v003/spec.md) | v0.0.3 要件（インプット） | 参照用 |
| [docs/_legacy-v003/architecture.md](docs/_legacy-v003/architecture.md) | v0.0.3 設計（インプット） | 参照用 |
| [docs/_legacy-v003/quality_checklist.md](docs/_legacy-v003/quality_checklist.md) | v0.0.3 リリース前チェック（インプット） | 参照用 |
