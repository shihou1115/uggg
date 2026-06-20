import { invoke } from "@tauri-apps/api/core";

import { renderResponse } from "../system/ghost-speech";
import { hitTest } from "../stage/character";
import type { DialogueResponse } from "../types";

/// クリック count を受けて poke コマンドを叩く。
/// 2-3 回 = 通常つつき、4 回以上 = rapid。
/// 最後にクリックされた座標で部位判定する (click.ts が記録した downX/Y を渡す)。
export async function firePoke(count: number, x: number, y: number): Promise<void> {
  const hit = hitTest(x, y);
  if (!hit) return;
  const rapid = count >= 4;
  try {
    const resp = await invoke<DialogueResponse | null>("poke", {
      target: hit.slot,
      region: hit.region,
      rapid,
    });
    // バックエンドは pick_event 成功時に persist_and_speak まで済ませて
    // dialogue イベントを emit するため、フロントの ghost-speech listener が拾う。
    // 戻り値の resp は冗長 (履歴用) なのでここでは何もしない。
    void resp;
    void renderResponse;
  } catch (err) {
    console.error("poke failed", err);
  }
}
