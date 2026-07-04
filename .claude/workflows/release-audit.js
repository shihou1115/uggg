export const meta = {
  name: 'release-audit',
  description: 'リリース前監査 — ビルド検証・契約/バージョン/bundle 突合・差分レビューを並列実行し、出荷判定の材料を返す',
  whenToUse: 'releasing-ugg skill の冒頭（Step 1〜4 相当）。リリース準備・タグ打ちの前に必ず 1 回実行する。',
  phases: [
    { title: '監査', detail: '機械検査 4 本 (haiku) + 差分レビュー (opus) を並列実行' },
  ],
}

// 5 本は互いに独立なので単一 parallel で実行する（バリアはこの 1 回だけ）。
// モデルは agentType 側の定義（build-checker/reviewer）または model 指定で固定。
phase('監査')
const [build, contract, version, bundle, review] = await parallel([
  () => agent(
    'C:\\claude\\uggg で次を実行し結果を報告: (1) src-tauri/ 内で cargo test (2) src-tauri/ 内で cargo check (3) リポジトリルートで npx tsc --noEmit。エラー・警告は file:line 付きで全件転記。',
    { agentType: 'build-checker', label: 'build-check' }),
  () => agent(
    'C:\\claude\\uggg の契約突合。docs/architecture.md に列挙された Tauri コマンド名・イベント名と、実装（src-tauri/src/main.rs の invoke_handler、src-tauri/src/commands/*.rs、src/ 側の invoke/listen 呼び出し）を突合し、「docs にあって実装にない」「実装にあって docs にない」名前を全件列挙。解釈はせず一致/不一致の事実のみ返す。',
    { model: 'haiku', effort: 'low', label: 'contract-check' }),
  () => agent(
    'C:\\claude\\uggg のバージョン整合検査。package.json / src-tauri/Cargo.toml / src-tauri/tauri.conf.json の version、package-lock.json と src-tauri/Cargo.lock の対応エントリを読み、全値を列挙して一致するか答える。さらに git grep で 1 つ前のバージョン文字列が README・docs に残っていないか確認。事実のみ返す。',
    { model: 'haiku', effort: 'low', label: 'version-check' }),
  () => agent(
    'C:\\claude\\uggg の配布設定検査。src-tauri/tauri.conf.json の bundle について: bundle.active が true か / bundle.resources に ../ghosts/・../shells/・python/sidecar.py が含まれるか / bundle.icon の指す実ファイルのサイズ（icon.png が 1KB 未満ならプレースホルダ疑いと明記）。加えて src-tauri/src/main.rs に panic dialog hook（install_panic_dialog_hook）が存在するか確認。事実のみ返す。',
    { model: 'haiku', effort: 'low', label: 'bundle-check' }),
  () => agent(
    'C:\\claude\\uggg で直近のリリースタグ以降の差分をレビュー。まず git describe --tags --abbrev=0 でタグを特定し、git diff <タグ>..HEAD を対象とする。観点: 正しさ / docs/spec.md・docs/architecture.md との契約整合 / CLAUDE.md の規律違反（spec 外機能・後付け抽象）/ リリースを止めるべき問題。指摘は重大度順・file:line 付き。',
    { agentType: 'reviewer', label: 'diff-review' }),
])

return {
  build_check: build,
  contract_check: contract,
  version_check: version,
  bundle_check: bundle,
  diff_review: review,
  note: '出荷可否の判定はメインセッションが行う（Fable 5 推奨 — CLAUDE.md の切替ルール参照）。インストール版の実機起動確認（releasing-ugg Step 6）はこの監査に含まれない。',
}
