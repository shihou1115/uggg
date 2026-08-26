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
6. **docs を肥大化させない**。 新規 .md は正本への節追加を先に検討し、完遂で不要になる文書は冒頭に削除予定を明記して作る。索引の正本は本ファイルのドキュメント索引 1 箇所（docs 側に索引を新設しない）。**リリースのタグ打ち後に `.claude/skills/tidy-docs` で役目を終えた文書を整理する**（初回 2026-07-24: 28→18 ファイル。判定基準・不可侵リストは skill 側が正本）。

## 進行ロードマップ（Phase 4 着手前）

| Phase | 内容 | 成果物 | 状態 |
|---|---|---|---|
| 0 | 環境セットアップ | このリポジトリの初期構成 | ✅ |
| 1 | 仕様再定義（機能の取捨選択・コアコンセプト言語化） | docs/spec.md | ✅ |
| 2 | アーキテクチャ設計（TTS再設計・状態管理再設計・DB再設計） | docs/architecture.md | ✅ |
| 3 | テスト計画 | docs/test-plan.md | ✅ |
| **4** | **実装着手**（垂直スライス・M0〜M10） | src/, src-tauri/src/ | ✅ M0〜M10 完了。**v0.2.0 リリース済**（2026-07-18、タグ `v0.2.0`。日常支援 Tier S 全 4 機能: リマインダー / ToDo・日課 / 状況発話+ガバナンス / カレンダー参照。記録は docs/release-notes/v0.2.0.md） |
| **v0.4** | 基盤・完成度（spec §6.0、2026-08-10 スコープ確定） | spec v1.3 + docs/foundation-design.md（M13〜M15 実装済み）+ docs/release-notes/v0.4.0.md | ✅ **v0.4.0 タグ済み**（2026-08-17、master・**lightweight**。リリース監査 GO: cargo test 308 / tsc green・契約 78/78・**インストール版起動確認**・**M13/M14 とも実機検証 PASS**・SHA-256 記録済み）。① 表示モニタ選択（M13）② advanced 独り言の LLM 生成+キャッシュ（M14）③ 時事ネタ織り込み・賞味期限 1 週間の二段失効（M14）④ 負債返済（M15）。②③ は spec 未達の解消。音声入力は不採用（完成度を極める必要があり時期尚早）。**実機確認で発見・修正した実バグ 2 件**: keyring 3 の store feature 未指定で **API キーが一度も永続化されていなかった**（advanced が丸ごと死んでいた）／補充タイムアウト 20 秒が短すぎ**ローカル LLM では必ず失敗**していた（→120 秒） |
| **v0.4.1** | 外部レビュー指摘への対応（パッチ） | docs/release-notes/v0.4.1.md | ✅ **v0.4.1 タグ済み**（2026-08-26、master・**lightweight**）。第三者レビュー 9 指摘を実コードで検証（妥当 6 / 部分的 3）し、**ユーザーが踏みうる 5 件**を修正: ① **DnD アセット `id` のパス脱出**（`ghost.json` の `id` が無検証で、`..` や絶対パスによりアセット領域外へ書き込み・削除が到達しうる。確認なしの経路もあった）② keyring 同期 API が LLM 経路 3 本をブロック ③ SQLite エラーを「データなし」に変換（4 箇所）④ カレンダーソース更新がエラーを握り潰して成功を返す ⑤ 音声の再生完了を待たず吹き出しを消す。**見送り**: 月額上限の月次状態機械化（spec §4.2.7 に「次月まで復帰しない」の記述は無く仕様判断が要る）/DL 資産のハッシュ検証 / サイドカー認証 / LICENSE・CSP・CI。cargo test 311 / 契約変更なし（schema v9 維持） |
| **v0.3** | 定例会話 + 天気（spec §4.7、2026-07-18 スコープ確定） | spec v1.2.2 + docs/regular-talk-design.md v1.1（M11〜M12 実装済み）+ docs/release-notes/v0.3.0.md | ✅ **v0.3.0 タグ済み**（2026-07-24、`feat/v0.3-regular-talk` の `b9bf981`・タグ `v0.3.0`・**lightweight**。M11 天気基盤 + M12 定例会話。リリース監査 GO: cargo test 248 / tsc green・ライブ API・実機 UI 目視・**インストール版起動確認**まで PASS・SHA-256 記録済み）。**master へマージ済み**（2026-08-17、v0.4 と同時）。`origin` へは未 push |

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

### dev 実機検証の起動待ちルール（2026-07-10 再発防止）

- dev の起動/再起動待ちは **`scripts/dev-ready.ps1` の同期実行**で行う:
  - 起動待ち: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev-ready.ps1`
  - 再起動待ち（`touch` でリビルドさせた後）: `... -AfterTouch <touch したファイル>`
  - `Port 5273 is already in use` で dev が落ちたら: `... -CleanOrphans` を先に実行（孤児 vite/ugg.exe の掃除）
- **dev ログの grep で起動判定をしない**（ANSI エスケープでパターンが壊れる・追記蓄積で回数閾値が無意味、の 2 通りで誤判定した実績）。ログは診断専用
- **バックグラウンドの watch ループを起動待ちに使わない**（セッション終了で消滅し何も駆動しない実績）。待機は必ず 1 回の同期呼び出しで完結させ、exit code（0=READY / 1=タイムアウト）で分岐する
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
- サブエージェントには委譲パッケージの標準形（目的・対象・**変更可/禁止範囲**・契約・機械検証可能な完了条件）だけ渡す。報告は **変更内容 / 判断理由 / 懸念点 / 上位で判断すべきこと** に圧縮させる
- 単発・未確定の業務は定義ファイル化せず、繰り返すと分かった時点で `.claude/agents/` に固定する
- 節約するのは中間作業のみ。最終的な設計整合性・UX・品質の確認は上位モデルで行う
- **例外系**（サブエージェント側の上限・バックグラウンド中断からの復旧・仕様外論点の裁定分類・委譲とメイン直実行の閾値）は [docs/ai_model_routing.md](docs/ai_model_routing.md) の「例外系・障害時の運用」節に従う

## ドキュメント索引

| ファイル | 役割 | 状態 |
|---|---|---|
| docs/spec.md | 要件の正本 | v1.3 ✅（**§6.0 に v0.4 スコープ**＝表示モニタ選択・advanced 独り言 LLM・時事ネタ織り込み・負債返済。音声入力は不採用と理由を記録） |
| docs/architecture.md | モジュール構成・契約・設計判断 | v2.0 ✅（M7〜M14。**表示モニタ選択**と **advanced 独り言 + 時事ネタ**（DB v9 `monologue_cache`）の契約反映済み） |
| docs/foundation-design.md | **基盤・完成度（v0.4）の Phase 2 設計書**（§4.1.6 / §4.4.4 / §4.4.6 実装契約・M13〜M15） | 設計 v1 ✅（**M13・M14・M15 すべて実装済み**。§7 に未決の決着、§7.1 に実装が設計から意図的に外れた点を記録） |
| docs/test-plan.md | テスト戦略・手動チェックリスト | v1.8 ✅（§5 に A〜G 全節。天気/定例会話 F、日常支援 G、**モニタ選択 A-12〜14（M13）**、**advanced 独り言 D-4b〜e / 時事ネタ D-6b〜c（M14）**、§5.9 実機検証記録） |
| docs/daily-support-design.md | **日常支援 Tier S の Phase 2 設計書**（§4.6 実装契約・DB・M7〜M10） | 設計 v2 ✅（**M7〜M10 実装済み**、Tier S 完了） |
| docs/regular-talk-design.md | **定例会話と天気（v0.3）の Phase 2 設計書**（§4.7 実装契約・天気 API 選定・M11〜M12） | 設計 v1 ✅（**M11・M12 実装済み** 2026-07-24） |
| [docs/_legacy-v003/baseline-v0.0.3.md](docs/_legacy-v003/baseline-v0.0.3.md) | **v0.0.3 機能・契約・残課題の網羅スナップショット**（v0.0.3 の生 docs は 2026-07-24 の docs 整理で削除。原本はプロトタイプ `C:\claude\ugga` と git 履歴に現存） | 参照用 |
