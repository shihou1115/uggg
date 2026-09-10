import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/// 操作列テスト用に `index.html` の body を実際に読み込んで DOM を作る（v0.5.3）。
///
/// ダミーの DOM を手で書くと、**id がずれても気づけない**（フロントは
/// `getElementById` で静的配置された要素を掴む設計なので、そこが本番と食い違うと
/// テストだけ通る）。出荷する HTML をそのまま使えば id のずれもテストで落ちる。
///
/// `<script>` は取り除く。happy-dom に `/src/main.ts` を読ませても意味が無く、
/// アプリの起動処理が走るとテストの前提が壊れるため。
export function loadIndexHtml(): void {
  // happy-dom 環境では `import.meta.url` が document の URL (http://localhost/) に
  // なるためファイル解決に使えない。vitest はリポジトリ直下で走るので cwd から引く。
  const path = resolve(process.cwd(), "index.html");
  const html = readFileSync(path, "utf-8");
  const start = html.indexOf(">", html.indexOf("<body")) + 1;
  const end = html.lastIndexOf("</body>");
  const body = html.slice(start, end);
  document.body.innerHTML = body.replace(/<script[\s\S]*?<\/script>/g, "");
}
