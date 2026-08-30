# nar2ugg

「伺か」(SSP) のシェル `.nar` を [ugg](../../README.md) のシェル形式へ変換する CLI。

**現在 scaffold のみ。** モジュール境界・CLI・出力契約は確定しているが、変換の実装は未着手
（`todo!()`）。

## なぜ ugg 本体と別なのか

ugg 本体は 伺か形式を一切パースしない。SERIKO / SHIORI の語彙が本体へ染み出すのを防ぐため、
変換はこの独立バイナリで完結させ、出力を通常のシェル DnD で読み込ませる。**ugg 側は無改修**。

Cargo workspace にも属していない。ugg のリリース手順書と dev スクリプトが `src-tauri\target\`
を直に参照しており、workspace 化すると target が移動して両方壊れるため。

## 使い方（予定）

```pwsh
# 中身を見る（surface ID・alias・サイズの一覧）
nar2ugg list mysh.nar

# 変換する
nar2ugg convert mysh.nar -o out --pose main.happy=5 --pose main.surprised=2
```

伺かには**表情番号の標準が無い**（surface 0 = 本体、10 = 相方だけが保証される）。`sakura.surface.alias`
に名前があればそれを使うが、命名は作者の自由なので当たる保証はない。自動割り当ては推測を含み、
推測した pose は変換ログに「(推測)」と表示される。意図と違えば `list` で中身を見て `--pose` で指定する。

## 制限

- **アニメーションは変換しない。** ugg は静止画のみ（2026-08-28 に凍結）。SERIKO のアニメーション
  定義は読み飛ばす。
- **着せ替え (`animation*.interval,bind`) は原理的に完全再現できない。** どのパーツが既定で
  「着ている」かはゴースト側の SHIORI が決めるので、シェル単体からは分からない。素の絵が
  未完成に見えることがある。
- **3 体目以降のキャラは捨てる。** ugg は main / sub の 2 枠しか持たない。
- クロマキー透過（左上 1px の色を抜く方式）のアンチエイリアス縁に残る色は自動では消えない。

## ライセンス

nar2ugg 自身は MIT（リポジトリルートの [LICENSE](../../LICENSE)）。

**変換結果の権利は元シェルの作者にある。** 再配布可否は作者ごとに readme に書いてあるだけで
機械判定できない。手元利用に留め、再配布は原作者の許諾を得ること。

## 開発

ugg 本体とは独立にビルドする。

```pwsh
cd crates/nar2ugg
cargo test
```
