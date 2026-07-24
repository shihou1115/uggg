---
name: tidy-docs
description: docs/ の定期整理 — リリースのタグ打ち後に、役目を終えた文書の削除・統合で docs の肥大化を防ぐ。「docs 整理」「ドキュメント削減」「docs が肥大化」「不要なドキュメントを消す」という話が出たとき、および releasing-ugg の Step 9 から必ずこの skill を使うこと。判定基準（4 分類）・被参照検査の手順・消してはいけない不可侵リストが入っている。
---

# docs 整理手順（リリース後の定例）

## この skill が存在する理由

v0.3.0 リリース直後の初回整理（2026-07-24）で docs/*.md は **28 → 18 ファイル（約 2,000 行削減）**になった。
溜まっていたのは: 完遂済みの実装計画 2 本・Phase 1〜3 で消費済みの v0.0.3 生 docs 6 本・
乖離した重複索引 1 本・正本の外に生えた実機検証記録 1 本。いずれも**作成時は必要で、
役目を終えた瞬間から無用**になる文書だが、刈り取りの定例がなかったため最長 3 リリース分滞留した。
本 skill はその刈り取りをリリース後の定例にし、あわせて入口（新規作成時）の基準を定める。

## 実行タイミング

- **リリースのタグ打ち・push 完了後**（releasing-ugg Step 8 の後）に 1 回実行する。
  - タグ後に行う理由: 削除しても「原本はタグ vX.Y.Z 以前の履歴にある」と復元先を一言で言える。
    リリース作業自体を hygiene で遅らせない（**リリースの完了条件には含めない**）。
  - 次バージョンのスコープ選定前に済ませる（新しい設計書を書き始める前に土台を軽くする）。
- リリースを跨がない臨時実行は、ユーザーの明示指示があった場合のみ。

## 判定手順

### Step 1: 棚卸し

```bash
find docs -name "*.md" | while read f; do printf "%6d 行  %s\n" $(wc -l < "$f") "$f"; done | sort -k3
```

### Step 2: 被参照検査（削除可否の決め手。ここを飛ばして消さない）

候補ごとにリポジトリ全体を検査する:

```bash
grep -rln "<ファイル名の stem>" --include="*.md" --include="*.rs" --include="*.ts" \
  --include="*.json" --include="*.js" --include="*.yaml" --include="*.ps1" \
  CLAUDE.md README.md docs src src-tauri/src src-tauri/tauri.conf.json .claude scripts
```

判定ルール:
- **コード（.rs / .ts）のコメントから参照される文書は削除・改名しない**（v0.3 時点で設計書 4 本に計 40+ 箇所の参照がある。doc 整理のためにコードを触るのは本末転倒）
- **`tauri.conf.json` の `bundle.resources` にある文書（`docs/manual.md`）は絶対に動かさない**（配布物が壊れる）
- **`docs/release-notes/` からの参照は dangling 扱いしない**（当時の状態の記録。歴史は書き換えない）
- 被参照が「索引類（CLAUDE.md 索引・spec §7 等）のみ」なら、参照行ごと整理できるので削除可能

### Step 3: 4 分類で処置を決める

| 分類 | 見分け方 | 例（初回整理） | 処置 |
|---|---|---|---|
| **完遂した計画** | 実装計画・移行手順で、対象がすべて出荷済み | implementation-plan.md（M0〜M6） | 削除（git 履歴が記録） |
| **消費済みインプット** | 検討材料として持ち込まれ、成果物（spec 等）に消化済み | _legacy-v003 の生 docs | 削除（蒸留版・原本の所在を残す側に注記） |
| **重複索引・乖離した写し** | 正本と同じ役割で更新が止まっている | docs/README.md（索引の正本は CLAUDE.md） | 削除 |
| **正本の外に生えた記録** | 既存正本の一節であるべき内容が独立ファイル化 | quality_checklist.md → test-plan §5.8 | 正本へ統合（出自を見出しに注記） |

### 不可侵リスト（消さない・動かさない。増えたらここに追記）

- 正本 4: `spec.md` / `architecture.md` / `test-plan.md` / `ai_model_routing.md`
- コード被参照の機能設計書: `daily-support-design.md` / `regular-talk-design.md` / `text-reader-spec.md` / `script-reader-spec.md`
- `manual.md`（**インストーラ同梱**）と `samples/`（manual から参照）
- `release-notes/`（歴史記録。削除も編集もしない）
- `_legacy-v003/baseline-v0.0.3.md`（CLAUDE.md「採用済み技術選定」の根拠参照先）

### Step 4: 参照の付け替え

削除・統合したら参照元をすべて直す（release-notes は除く）:
- CLAUDE.md のドキュメント索引
- spec.md §7 参照（**spec は改訂履歴の行も足す** — 参照整理でも履歴文化を守る）
- 各設計書・spec 間の相互参照、`.claude/skills/` 内の参照
- 統合先には「旧 <ファイル名>、YYYY-MM-DD 統合」の出自注記を付ける

### Step 5: 検証とコミット

dangling 検査を再実行してゼロを確認（改訂履歴の記述文と release-notes は除外して見る）。
1 コミットにまとめ、**コミットはユーザーに確認してから**行う。

## モデル運用

機械的な検査（Step 1/2/5）と参照付け替えは下位モデル・エージェント（mechanic / implementer）で可。
**正本級の削除や CLAUDE.md 規律に触れる判定に迷ったら Fable 切替を提案する**（docs/ai_model_routing.md）。
初回（2026-07-24）で 4 分類の型ができたため、型に収まる判定は Opus で完結してよい。

## 入口対策（新規 .md 作成時の規律 — CLAUDE.md 開発方針 6 の実体）

docs/ に新しい .md を作る**前に**:
1. 既存の正本（spec / architecture / test-plan / 機能別設計書）への**節追加で足りないか**を先に検討する
2. 完遂したら不要になる文書（実装計画・移行手順・一時的な検討メモ）は、**冒頭に「〜完了後に本ファイルは削除する」と削除タイミングを明記**して作る（次回の tidy-docs が機械的に刈れる）
3. **索引を新設しない**（索引の正本は CLAUDE.md のドキュメント索引 1 箇所。docs/README.md の乖離が前例）
