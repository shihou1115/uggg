import { invoke } from "@tauri-apps/api/core";

import type { SlotName, TalkSpeed } from "../types";

/// TTS スピーカー: テキストを WAV (base64) として取得し、Web Audio で再生する。
/// slot ごとにキューを持ち、同一 slot 内は順序保証、interrupt() で全 slot 一括停止。
/// 失敗 (TTS 無効 / 資産未 DL / 合成エラー) は黙殺 (フロントは声なしで継続)。

interface QueueItem {
  text: string;
  slot: SlotName;
  resolve: () => void;
}

const queue: Map<SlotName, QueueItem[]> = new Map([
  ["main", []],
  ["sub", []],
]);
let processing: Partial<Record<SlotName, boolean>> = {};
let audioCtx: AudioContext | null = null;
let currentSource: AudioBufferSourceNode | null = null;
let ttsEnabled = false;
let ttsSpeed = 1.0;
let ttsVolume = 1.0;

export function setTtsParams(params: { enabled: boolean; speed: number; volume: number }): void {
  ttsEnabled = params.enabled;
  ttsSpeed = clamp(params.speed, 0.5, 2.0);
  ttsVolume = clamp(params.volume, 0, 2);
}

export interface TtsSpeaker {
  speak(slot: SlotName, text: string): Promise<void>;
  interrupt(): void;
  whenIdle(): Promise<void>;
  isAudible(): boolean;
}

export function createSpeaker(): TtsSpeaker {
  return {
    speak(slot, text) {
      if (!ttsEnabled || !text.trim()) {
        return Promise.resolve();
      }
      return new Promise<void>((resolve) => {
        const item: QueueItem = { slot, text, resolve };
        queue.get(slot)?.push(item);
        void pump(slot);
      });
    },
    interrupt() {
      stopAll();
    },
    whenIdle() {
      return new Promise<void>((resolve) => {
        const check = () => {
          const idle = !processing.main && !processing.sub && queueEmpty();
          if (idle) resolve();
          else setTimeout(check, 50);
        };
        check();
      });
    },
    isAudible() {
      return ttsEnabled;
    },
  };
}

function queueEmpty(): boolean {
  return (queue.get("main")?.length ?? 0) === 0 && (queue.get("sub")?.length ?? 0) === 0;
}

async function pump(slot: SlotName): Promise<void> {
  if (processing[slot]) return;
  processing[slot] = true;
  try {
    while (true) {
      const next = queue.get(slot)?.shift();
      if (!next) break;
      await playOne(next);
    }
  } finally {
    processing[slot] = false;
  }
}

async function playOne(item: QueueItem): Promise<void> {
  try {
    const b64 = await invoke<string>("synthesize_voice", { text: item.text, slot: item.slot });
    await playBase64Wav(b64);
  } catch (err) {
    console.warn("[tts] synth/play failed", err);
  } finally {
    item.resolve();
  }
}

async function playBase64Wav(b64: string): Promise<void> {
  try {
    const bytes = base64ToBytes(b64);
    const ctx = ensureAudioCtx();
    // SharedArrayBuffer 衝突を避けるためコピーして ArrayBuffer に揃える
    const ab = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(ab).set(bytes);
    const buffer = await ctx.decodeAudioData(ab);
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.playbackRate.value = ttsSpeed;
    const gain = ctx.createGain();
    gain.gain.value = ttsVolume;
    source.connect(gain).connect(ctx.destination);
    currentSource = source;
    await new Promise<void>((resolve) => {
      source.onended = () => {
        if (currentSource === source) {
          currentSource = null;
        }
        resolve();
      };
      source.start();
    });
  } catch (err) {
    console.warn("[tts] decode/play failed", err);
  }
}

function stopAll(): void {
  for (const slot of ["main", "sub"] as const) {
    const q = queue.get(slot);
    if (q) {
      for (const item of q) item.resolve();
      q.length = 0;
    }
  }
  if (currentSource) {
    try {
      currentSource.stop();
    } catch {
      // already stopped
    }
    currentSource = null;
  }
}

function ensureAudioCtx(): AudioContext {
  if (audioCtx) return audioCtx;
  audioCtx = new AudioContext();
  return audioCtx;
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function clamp(n: number, min: number, max: number): number {
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, n));
}

// TalkSpeed (タイプライター用) と TTS speed は別 (TTS は連続値、TalkSpeed は離散)。
export function ttsSpeedFromTalkSpeed(_t: TalkSpeed): number {
  // 現状は連動させず ttsSpeed をそのまま使う。将来 talk_speed 連動が欲しくなったらここで写像。
  return ttsSpeed;
}
