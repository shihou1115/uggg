import { defineConfig } from "vitest/config";

/// フロントの「操作列テスト」用（spec §6.0 項目 9、v0.5.3）。
///
/// v0.5.3 で確定した 14 件は**単機能テストでは 1 件も捕まらなかった**。
/// 残っていたのは更新経路・保存後の再保存・連続通知・非同期の中断という
/// つなぎ目で、そこを固定するにはフロント側でも「操作を順に流す」テストが要る。
/// それまで src/ にテストの仕組みが無かったため、ここで最小限だけ足した。
///
/// happy-dom を使い、DOM は `index.html` を読み込んで組み立てる
/// （id のずれもテストで落ちるようにするため。`src/__tests__/dom.ts` 参照）。
export default defineConfig({
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.ts"],
    restoreMocks: true,
  },
});
