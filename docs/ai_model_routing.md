# AI Model Routing — 役割分担の正本

目的:
1. Fable 5 のトークン消費を最適化する（過剰消費も過剰節約もしない）。
2. Fable 5 使用不可時でも Opus 4.8 / Sonnet 5 / Haiku 4.5 だけで高パフォーマンスを維持する。

基本思想: **Fable 起動前に、準備（収集・整形・検査）を下位モデルで済ませ、机が完璧になった状態で Fable に働いてもらう。**
Fable に任せるのは「型がない × 失敗コストが大きい × 全体を見る必要がある」判断だけ。

## 役割分担表

### A. Fable 5（メインセッション。切替提案の対象）

| 作業 | 理由 | 動かし方 |
|---|---|---|
| 仕様改訂（spec.md の機能取捨選択・コアコンセプト変更） | 型がない×手戻り最大×全体整合が必要 | メイン直（切替提案 → /model） |
| アーキテクチャ・契約変更（architecture.md、コマンド/イベント/DB スキーマ） | 契約ミスは front/back 両方に波及 | メイン直（同上） |
| 難所実装（voicevox_core FFI・透過ウインドウ・非同期/Mutex まわり） | この層のミスは実機でしか発覚せず高コスト | メイン直（同上） |
| リリース可否の最終判定 | 全体を見る一回きりの判断。材料は release-audit Workflow が準備 | メイン直（Workflow 結果を受けて判定） |

### B. Opus 4.8（日常の判断・レビュー・執筆。メインの常用モデル）

| 作業 | 理由 | 動かし方 |
|---|---|---|
| 日常のオーケストレーション・タスク分解・相談 | 判断力は必要だが失敗は会話内で修正可能 | メイン直（常用） |
| バグ調査・原因特定（推論部分） | 仮説立案に推論力が要る。情報収集は code-scout に出す | メイン直（単発業務のため未定義。繰り返すなら固定） |
| コードレビュー | 指摘の選別に判断力が要る | `.claude/agents/reviewer.md` |
| ドキュメント執筆（manual.md・release-notes・spec 下書き） | 文章の質は Opus で十分、確定は上位 | `.claude/agents/doc-writer.md` |
| 辞書・セリフ執筆（ghosts の掛け合い） | 創作でキャラ一貫性が要る | `.claude/agents/dict-writer.md` |

### C. Sonnet 5（型が決まった量産・実行）

| 作業 | 理由 | 動かし方 |
|---|---|---|
| 仕様確定済みの実装 | 契約が docs にあれば型作業。判断に踏み込んだら保留させ上位へ | `.claude/agents/implementer.md` |
| テスト追加（test-plan.md 準拠） | 期待値が確定しており型がある | `.claude/agents/test-writer.md` |
| 既存コード調査・影響範囲調査 | 網羅性が要るが判断は不要 | `.claude/agents/code-scout.md` |
| リリース作業の実行（version bump・build・タグ） | releasing-ugg skill に手順固定済み | skill releasing-ugg → implementer 等に委譲 |

### D. Haiku 4.5（機械的な変換・検査）

| 作業 | 理由 | 動かし方 |
|---|---|---|
| 型検査・ビルド検証（cargo check/test・tsc の実行と転記） | 実行→転記のみ。判断ゼロ | `.claude/agents/build-checker.md` |
| docs⇔実装の突合検査（コマンド名・イベント名・バージョン） | grep と突合のみ。解釈は上位 | `.claude/workflows/release-audit.js` の stage / `.claude/agents/mechanic.md` |
| 差分要約・コミットメッセージ下書き・単純一括置換 | 機械的変換 | `.claude/agents/mechanic.md` |

### E. Fable 5 不可時のフォールバック

- メイン = Opus 4.8 が A 欄（設計・判定）も担当する。B〜D の割当は**一切変えない**。
- 品質補償: A 欄の作業のみ、Opus メインの結論を**別の Opus サブエージェント（reviewer）に反証レビュー**させ、Fable の一発判断を「Opus×2 の相互検証」で代替する。契約変更時は mechanic の突合検査を必須化。
- Sonnet / Haiku はもともと Fable 非依存なので変更なし。

## メインセッションのモデル切替（/model はユーザー操作）

アシスタントはメインのモデルを自分で変更できない。切替の**提案**までを担当する:

1. 既定は Opus 4.8。
2. A 欄の作業が発生したら「Fable に切替推奨」と明示提案する。
3. A 欄の作業が終わり量産・検証フェーズに入ったら「Opus に戻して OK」と明示提案する。
4. 1 ターンで済む軽い設計相談は切替提案しない（切替コスト＞効果）。

## 運用ルール

1. サブエージェントに渡すのは「対象ファイル・契約（docs の該当節）・完了条件」のみ。会話履歴全体は渡さない。
2. 報告フォーマットは全エージェント共通: **変更内容 / 判断理由 / 懸念点 / 上位で判断すべきこと**。
3. サブエージェントが設計判断に踏み込みそうな場合は、判断を保留して「上位で判断すべきこと」として返させる（各エージェント定義に記載済み）。
4. 実装完了後は build-checker（または上位モデル自身）で `cargo check` / `tsc --noEmit` と契約整合を必ず検証する。
5. **単発・未確定の業務は定義ファイル化しない。** 1 回やってみて、繰り返すと分かった時点で `.claude/agents/` に固定する（例: debugger は現状未定義のままメイン直で扱う）。
6. 節約の対象は中間作業のみ。最終的な設計整合性・UX・品質の確認は上位モデルで行い、成果物の品質を下げる節約はしない。

## 定義ファイル一覧

| 種別 | 場所 | モデル指定 |
|---|---|---|
| サブエージェント | `.claude/agents/*.md` | frontmatter の `model:`（opus: reviewer, doc-writer, dict-writer / sonnet: implementer, test-writer, code-scout / haiku: build-checker, mechanic） |
| Workflow | `.claude/workflows/release-audit.js` | stage ごとの `agentType` / `model` / `effort` |
| スキル | `.claude/skills/releasing-ugg/` | **モデルを書かない**。上記エージェント・Workflow を呼ぶ |
| 運用ルール | CLAUDE.md「Model Routing」節 | メイン切替の提案ルール含む |
