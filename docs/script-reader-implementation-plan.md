# script-reader 実装・テスト実行計画

**日付**: 2026-07-04
**正本**: [script-reader-spec.md](script-reader-spec.md)（改訂 2。判断はすべてこの仕様書に固定済み）
**前提**: **実装・テストの途中で Fable 5 が利用上限に達することが確実**。
本計画はそれを前提に、「Fable の判断を全部前倒しし、実行フェーズは Fable 不在で完走できる」
ように構成する（モデル運用の一般規則は [ai_model_routing.md](ai_model_routing.md)）。

---

## 0. 設計原則（Fable 上限対策）

1. **判断の前倒し**: 設計判断は仕様書（改訂 2）と本計画で出し切った。実装フェーズの
   サブエージェントは**仕様外の判断をせず**、「上位で判断すべきこと」として返す。
2. **裁定の代替体制**: Fable 不在中の裁定は CLAUDE.md のフォールバック規則に従う —
   Opus メインが裁定し、**契約級の裁定は reviewer（opus）の反証レビューを必須**とする。
   裁定は [script-reader-decisions.md](script-reader-decisions.md)（初回裁定時に作成）へ
   追記し、**Fable 復帰後の事後監査対象**として残す。
3. **Step の自己完結**: 各 Step は検証ゲート（cargo test / tsc / reviewer）付きで、
   Step 単位でコミットする。どこで中断しても、次のセッション・次のモデルが
   コミット済み Step から再開できる。
4. **Fable の残り残量は温存する**: 本計画の確定と P0 の確認以降、実装中は Fable に
   切り替えない。復帰後の事後監査（P5-2）に残量を使う。

### 裁定の分類基準（実装中に仕様外の論点が出たとき）

| 分類 | 例 | 裁定者 | 記録 |
|---|---|---|---|
| 仕様の読み方で解ける | エラー文言の細部、テストの書き方 | implementer が仕様参照で続行 | 不要 |
| 仕様に無いが契約に触れない | UI 文言、内部関数名、ログ | Opus メイン | decisions.md |
| **契約に触れる** | コマンド型・エラー種別・直列化形状・イベント | Opus メイン + **reviewer 反証必須** | decisions.md + Fable 復帰後監査 |
| 裁定不能（仕様の根幹に関わる） | 台本形式自体の変更等 | **該当 Step を保留**し他 Step を進め、Fable 復帰を待つ | decisions.md |

---

## 1. フェーズ構成と Step 表

依存関係: P0 → P1（P1-1 → P1-2 → P1-3）。**P2-1 は P1 と並列可能**
（`ReadingChunk`/`VoiceSlot` の型・注記条件・slot 検証は仕様 §2.5/§2.7/§2.9 で確定済みのため、
types.ts とフロントロジックは仕様から直接書ける。結合確認だけ P1-3 完了後）。

| Step | 内容 | 担当 | 検証ゲート | 依存 |
|---|---|---|---|---|
| **P0-1** | spec.md §4.5.8 / architecture.md（§4.7 契約表: `reader_load_text` 戻り値変更・`synthesize_voice` caption 追加、§1.2 に tts/script.rs 追記）/ text-reader-spec.md §7 に本仕様への参照を反映 | doc-writer | mechanic で契約表⇔仕様の突合 | 仕様承認 |
| **P0-2** | P0-1 の反映結果と本計画の最終確認（**Fable 在席中の最後の仕事**） | Fable | — | P0-1 |
| **P1-1** | `tts/script.rs` 新規: フェンス抽出＋パース＋検証＋`ScriptError`。ユニットテスト §5.1 の 1〜19 を同時に書く（TDD 推奨） | implementer | cargo test → reviewer | P0 |
| **P1-2** | `tts/reader.rs` 改修: `ReadingChunk`/`VoiceSlot`・.txt 既定メタ・長行分割のメタ複製＋中間 pause=0・ms 丸め・既定 500ms 定数の移設。テスト 20〜22＋既存テスト改修 | implementer | cargo test（既存 121 件回帰込み）→ reviewer | P1-1 |
| **P1-3** | 契約変更の配線: `commands/reader.rs`・`commands/tts.rs`（caption 引数＋空文字正規化）・`irodori.rs`（SpeechRequest + skip_serializing_if、テスト 23）・`sidecar.py`（caption 透過） | implementer | cargo check/test → **reviewer（契約変更のため必須・反証観点）** | P1-2 |
| **P2-1** | フロント: `types.ts`・`reader.ts`（ReadingChunk 対応・再生開始前 slot 検証・caption 注記ラベル・pause 参照先変更）・`dnd.ts`（.md 追加）・index.html（注記ラベル静的配置 — WebView2 罠回避の既存規約に従う） | implementer | npx tsc --noEmit → reviewer | P0（結合確認は P1-3 後） |
| **P3-1** | 全体回帰: cargo test 全件＋tsc＋`synthesize_voice` caption 省略互換の確認（§5.3） | build-checker | 全緑 | P1-3, P2-1 |
| **P3-2** | 実装差分の総合レビュー（仕様 §2 との適合・既知の罠・裁定記録との整合） | reviewer | 指摘の解消 | P3-1 |
| **P3-3** | 実機テスト用サンプル台本の作成: 正常系 1 種は `docs/samples/script-sample.md` としてリポジトリ同梱（manual からも参照）、不正系・長行系はテスト時に生成 | mechanic | 正常系が S1 で動く | P3-1 |
| **P4** | 実機手動テスト S1〜S12（仕様 §5.2。S4/S9/S11 は Irodori 実モデル環境、未導入なら S4' 代替） | **ユーザー** + Opus（結果の記録・解釈） | チェックリスト全消化 | P3 |
| **P5-1** | 品質確認: Opus メインが総合判定し、reviewer に**反証レビュー**をかける（Fable の最終品質確認の代替。CLAUDE.md フォールバック規則） | Opus + reviewer | 反証で残った指摘ゼロ | P4 |
| **P5-2** | **Fable 復帰後の事後監査**: decisions.md の裁定全件＋契約差分（architecture.md §4.7 変更点）＋P5-1 の判定根拠を監査 | Fable | 監査指摘の解消 | Fable 復帰 |
| **P6** | manual.md（台本形式の書き方）・quality_checklist.md（S 節）更新 → リリースは releasing-ugg skill で別途（v0.1.2、本計画の範囲外） | doc-writer | reviewer | P5-1 |

## 2. テスト実行の割当

| テスト | 内容 | 実行者 | タイミング |
|---|---|---|---|
| ユニット 23 件（仕様 §5.1） | script.rs 1〜19 / reader.rs 20〜22 / irodori.rs 23 | implementer が実装 Step 内で作成、build-checker が実行 | 各 Step のゲート |
| 回帰（仕様 §5.3） | cargo test 全件・tsc・caption 省略互換・zip DnD・.txt 挙動 | build-checker | P3-1 |
| 実機 S1〜S12（仕様 §5.2） | 話者切替・速度・間・caption・fail-fast・回帰 | ユーザー（手順書は仕様 §5.2 表のまま使える） | P4 |
| sidecar 透過確認 | S4 時に sidecar ログで SamplingRequest.caption を確認（実モデル未導入なら S4' 代替） | ユーザー + Opus | P4 |

## 3. Fable 上限到達時の運用プロトコル

1. 上限到達 → ユーザーが `/model claude-opus-4-8` に切替（アシスタントは到達を検知したら
   即座に切替を依頼する）。
2. 以降の裁定は §0 の分類基準に従う。**実装は止めない** — 裁定不能の論点だけ保留リストに
   積み、依存しない Step を先に進める。
3. 復帰後: P5-2 の事後監査を実施。監査で裁定の覆しが出た場合は、その修正を新たな
   Step として P1 と同じゲート（implementer → build-checker → reviewer）で流す。

## 4. 完了判定

- [ ] P0: spec.md / architecture.md / text-reader-spec.md §7 に仕様反映済み
- [ ] P1〜P2: 全 Step のゲート（cargo test / tsc / reviewer）が緑、Step 単位でコミット済み
- [ ] P3: 回帰全緑 + 総合レビュー指摘ゼロ + サンプル台本同梱
- [ ] P4: S1〜S12 全消化（S4 系は実モデル環境 or S4' 代替を明記して記録）
- [ ] P5-1: Opus + reviewer 反証の品質確認クリア
- [ ] P5-2: Fable 復帰後の事後監査クリア（裁定が 0 件なら decisions.md 不要、監査は差分のみ）
- [ ] P6: manual / quality_checklist 更新（リリースは releasing-ugg で別途）
