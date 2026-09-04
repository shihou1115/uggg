import { invoke } from "@tauri-apps/api/core";

import type { SlotName, TalkSpeed } from "../types";
import { attachMouth, stopMouth } from "./mouth";

/// TTS スピーカー: テキストを WAV (base64) として取得し、Web Audio で再生する。
/// **全 slot 直列の単一キュー**: main/sub を問わず enqueue 順に 1 つずつ再生し、
/// 音声が重なることは構造的にない (掛け合いで main 再生中に sub が話し始めない)。
/// 再生中に次アイテムの合成を先行させる (先読み 1) ため、話者交代の無音は最小。
/// interrupt() でキュー破棄 + 再生停止。失敗 (TTS 無効 / 資産未 DL / 合成エラー) は
/// 黙殺 (フロントは声なしで継続)。
///
/// テキスト読み上げツール (panels/reader.ts) は専用の再生ループを持ち、この
/// キューとは独立 (仕様: ユーザー起点のチャット応答と読み上げの同時発声は許容)。

interface QueueItem {
  text: string;
  slot: SlotName;
  resolve: () => void;
}

interface SynthEntry {
  item: QueueItem;
  wav: Promise<string | null>;
}

const queue: QueueItem[] = [];
let processing = false;
/// interrupt のたびに増える世代番号。世代を跨いだ先読み結果は再生しない。
let generation = 0;
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
        queue.push({ slot, text, resolve });
        void pump();
      });
    },
    interrupt() {
      stopAll();
    },
    whenIdle() {
      return new Promise<void>((resolve) => {
        const check = () => {
          if (!processing && queue.length === 0) resolve();
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

/// キュー先頭を取り出し、合成を開始する (再生とは切り離した先読み用)。
function dequeueAndSynth(): SynthEntry | null {
  const item = queue.shift();
  if (!item) return null;
  return { item, wav: synthOne(item) };
}

async function synthOne(item: QueueItem): Promise<string | null> {
  try {
    return await invoke<string>("synthesize_voice", { text: item.text, slot: item.slot });
  } catch (err) {
    console.warn("[tts] synth failed", err);
    return null;
  }
}

/// 単一ポンプ: enqueue 順に「合成済み WAV を再生 → その間に次を合成」を繰り返す。
async function pump(): Promise<void> {
  if (processing) return;
  processing = true;
  try {
    const gen = generation;
    let prefetched: SynthEntry | null = null;
    while (generation === gen) {
      const entry = prefetched ?? dequeueAndSynth();
      prefetched = null;
      if (!entry) break;
      const wav = await entry.wav;
      if (generation !== gen) {
        entry.item.resolve();
        break;
      }
      // 再生中に次アイテムの合成を進めておく (先読み 1)
      prefetched = dequeueAndSynth();
      if (wav !== null) {
        await playBase64Wav(wav, entry.item.slot);
      }
      entry.item.resolve();
    }
    if (prefetched !== null) {
      // 中断などでループを抜けた場合、先読み分も speak() の Promise を解放する
      prefetched.item.resolve();
    }
  } finally {
    processing = false;
    // 中断後 (新世代) に積まれたアイテムが残っていれば再開する
    if (queue.length > 0) void pump();
  }
}

/// 設定パネルからの参照音声プレビュー用。speaker キューを通さず即時再生する。
/// 既存のキャラ発話を止めずに重ねて鳴らすため `currentSource` は触らない。
export async function previewWavBase64(b64: string): Promise<void> {
  try {
    const bytes = base64ToBytes(b64);
    const ctx = ensureAudioCtx();
    const ab = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(ab).set(bytes);
    const buffer = await ctx.decodeAudioData(ab);
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    const gain = ctx.createGain();
    gain.gain.value = ttsVolume;
    source.connect(gain).connect(ctx.destination);
    source.start();
  } catch (err) {
    console.warn("[tts] preview decode/play failed", err);
  }
}

/// 本発話の再生。**口パク (spec §4.1.4) はここだけ**。設定画面のプレビューは
/// キャラが喋っているわけではないので駆動しない。
async function playBase64Wav(b64: string, slot: SlotName): Promise<void> {
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
    // 解析器を挟んで振幅で口を動かす。解析器は素通しなので音は変わらない。
    attachMouth(slot, ctx, source).connect(gain).connect(ctx.destination);
    currentSource = source;
    await new Promise<void>((resolve) => {
      source.onended = () => {
        stopMouth(slot);
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
  generation += 1;
  for (const item of queue) item.resolve();
  queue.length = 0;
  if (currentSource) {
    // 中断でも必ず口を閉じる。stop() は onended を確実には発火しない。
    stopMouth("main");
    stopMouth("sub");
    try {
      currentSource.stop();
    } catch {
      // already stopped
    }
    currentSource = null;
  }
}

/**
 * AudioContext を取り出し、**suspended なら resume を試みる**（spec §4.5.1 の
 * 「ウォッチドッグ」）。
 *
 * WebView2 の自動再生ポリシーにより、ユーザー操作を伴わない発話（起動挨拶・
 * ランダムトーク・リマインダー）では AudioContext が `suspended` のまま作られる
 * ことがある。その状態でも `start()` は例外を投げず Promise も解決するため、
 * **正常に喋ったように見えて音だけ出ない**。`--autoplay-policy` を渡していても
 * 起動タイミングによっては suspended になりうるので、再生のたびに確認する。
 */
function ensureAudioCtx(): AudioContext {
  if (!audioCtx) audioCtx = new AudioContext();
  if (audioCtx.state === "suspended") {
    // resume は Promise だが待たない: 失敗しても再生自体は試みる
    // (待つと発話が数百 ms 遅れ、しかも失敗時に無音なのは変わらない)。
    void audioCtx.resume().catch((err) => {
      console.warn("[tts] AudioContext.resume 失敗 (無音になる可能性):", err);
    });
  }
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
