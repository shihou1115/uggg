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
| **4** | **実装着手**（垂直スライス・M0〜M6） | src/, src-tauri/src/ | ✅ M0〜M6 完了、**v0.1.1 リリース済**（2026-07-04、タグ `v0.1.1`） |

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

4 モデル体制。役割分担表の正本: [docs/ai_model_routing.md](docs/ai_model_routing.md)

- **Fable 5** = 「型がない × 失敗コストが大きい × 全体を見る」判断だけ（仕様改訂・アーキ/契約変更・難所実装・リリース最終判定）
- **Opus 4.8** = メインセッションの常用モデル。日常のオーケストレーション・レビュー・執筆・バグ原因推論
- **Sonnet 5** = 型が決まった量産・実行（確定仕様の実装・テスト追加・コード調査・リリース作業の実行）
- **Haiku 4.5** = 機械的な変換・検査（cargo check/tsc の実行と転記・突合検査・差分要約）

### 自動振り分け（定義ファイルで固定済み）
- サブエージェント: `.claude/agents/`（opus: reviewer / doc-writer / dict-writer、sonnet: implementer / test-writer / code-scout、haiku: build-checker / mechanic）
- Workflow: `.claude/workflows/release-audit.js`（リリース前監査。stage ごとに model/effort 指定済み）
- スキルにはモデルを書かない。スキルは上記エージェント・Workflow を呼ぶ

### メインセッションのモデル切替（/model はユーザーが操作。アシスタントは提案まで）
- 既定は Opus 4.8。Fable 5 の担当作業（上記 Fable 欄）が発生したら「Fable に切替推奨」と明示提案する
- Fable 欄の作業が終わり量産・検証フェーズに入ったら「Opus に戻して OK」と明示提案する
- 1 ターンで済む軽い設計相談は切替提案しない
- **Fable 5 不可時**: Opus がメインとして Fable 欄も担当し、その結論を別 Opus サブエージェント（reviewer）に反証レビューさせる。Sonnet/Haiku の割当は変えない

### 運用原則
- Fable 起動前に、準備（収集・整形・検査）を下位モデルで済ませ、判断材料が揃った状態で Fable に渡す
- サブエージェントには「対象ファイル・契約・完了条件」だけ渡す。報告は **変更内容 / 判断理由 / 懸念点 / 上位で判断すべきこと** に圧縮させる
- 単発・未確定の業務は定義ファイル化せず、繰り返すと分かった時点で `.claude/agents/` に固定する
- 節約するのは中間作業のみ。最終的な設計整合性・UX・品質の確認は上位モデルで行う

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
