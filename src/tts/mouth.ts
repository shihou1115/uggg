//! 口パク (spec §4.1.4)。
//!
//! **TTS 有効時のみ、再生振幅 (AnalyserNode の RMS) でリアルタイム駆動する。**
//! v0.0.3 のタイマー近似 (120ms 間隔の機械的パクパク) は spec §4.1.4 が明文で
//! 廃止しており、本実装はその置き換え。
//!
//! 開口フレームは pose 画像の隣の `<pose>_talk.png` を Rust 側が自動検出して
//! boot payload の `talk_poses` に載せる。無いシェルでは `setMouthOpen` が
//! 何もしないため、自動的に「口パクなし」になる。
//!
//! 背景: v0.4.1 まで本モジュールの `startFlap` / `stopFlap` は**呼び出し元が
//! ゼロ**で、既定シェルに `_talk.png` が 8 枚あるのに一度も使われていなかった。

import { setMouthOpen } from "../stage/character";
import type { SlotName } from "../types";

/** この値を超える RMS を「口が開いている」とみなす。 */
const OPEN_THRESHOLD = 0.02;
/** 解析の粒度。小さすぎると口がバタつく。 */
const FFT_SIZE = 512;

type Session = { raf: number; analyser: AnalyserNode; buf: Float32Array<ArrayBuffer> };

const sessions: Partial<Record<SlotName, Session>> = {};

/**
 * 再生ノードに解析器を挿し込み、振幅で口を駆動する。
 *
 * 戻り値は「解析器を経由した接続先」。呼び出し側はこれを destination へ繋ぐ。
 * 解析器は信号を素通しするので音は変わらない。
 */
export function attachMouth(
  slot: SlotName,
  ctx: AudioContext,
  source: AudioNode,
): AudioNode {
  stopMouth(slot);
  const analyser = ctx.createAnalyser();
  analyser.fftSize = FFT_SIZE;
  source.connect(analyser);
  // SharedArrayBuffer 由来にならないよう ArrayBuffer を明示する
  // (getFloatTimeDomainData の型が Float32Array<ArrayBuffer> を要求する)。
  const buf = new Float32Array(new ArrayBuffer(analyser.fftSize * 4));

  const tick = (): void => {
    const s = sessions[slot];
    if (!s) return;
    s.analyser.getFloatTimeDomainData(s.buf);
    let sum = 0;
    for (let i = 0; i < s.buf.length; i++) sum += s.buf[i] * s.buf[i];
    const rms = Math.sqrt(sum / s.buf.length);
    setMouthOpen(slot, rms > OPEN_THRESHOLD);
    s.raf = window.requestAnimationFrame(tick);
  };
  sessions[slot] = { raf: window.requestAnimationFrame(tick), analyser, buf };
  return analyser;
}

/** 口パクを止め、口を閉じる。再生終了・中断のたびに必ず呼ぶ。 */
export function stopMouth(slot: SlotName): void {
  const s = sessions[slot];
  if (s) {
    window.cancelAnimationFrame(s.raf);
    s.analyser.disconnect();
    delete sessions[slot];
  }
  setMouthOpen(slot, false);
}
