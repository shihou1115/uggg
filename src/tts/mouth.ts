//! 口パク (architecture §A-4)。
//! TTS 発話中は WAV の振幅に応じて開口フレームを切り替える。無音時 (TTS 無効) は
//! 描画中だけ機械的に約 120ms 間隔でパクパクさせる。
//!
//! 開口フレームは shell の `<pose>_talk.png` 形式 (M4 以降、talk_poses 機能を追加するなら)。
//! M4a 段階の本実装は「シェルに talk フレームが無いケースをグレースフルに扱う」シンプル版。

import type { SlotName } from "../types";

const FLAP_INTERVAL_MS = 120;
const flapTimers: Partial<Record<SlotName, number>> = {};

/// 描画中の機械的口パク開始。終了時に stopFlap を必ず呼ぶこと。
export function startFlap(slot: SlotName, setMouth: (open: boolean) => void): void {
  stopFlap(slot, setMouth);
  let open = false;
  const id = window.setInterval(() => {
    open = !open;
    setMouth(open);
  }, FLAP_INTERVAL_MS);
  flapTimers[slot] = id;
}

export function stopFlap(slot: SlotName, setMouth?: (open: boolean) => void): void {
  const id = flapTimers[slot];
  if (id !== undefined) {
    clearInterval(id);
    delete flapTimers[slot];
  }
  setMouth?.(false);
}
