//! テキスト読み上げパネル (docs/text-reader-spec.md)。
//!
//! パネル表示中に .txt が DnD されると `dnd.ts` から `startReading(path)` が呼ばれる。
//! `reader_load_text` でチャンク列を取得し、既存 `synthesize_voice` をチャンクごとに
//! 呼んで逐次再生する (先読み 1: チャンク i の再生中に i+1 を合成)。
//!
//! 再生はゴースト発話の speaker キューとは独立の専用 AudioContext で行う。読み上げ中は
//! `set_reading_active(true)` で自発発話を抑制し、停止/完走/クローズで false に戻す。

import { invoke } from "@tauri-apps/api/core";

import type { ReadingChunk, Settings, VoiceRef, VoiceSlot } from "../types";

interface Inputs {
  panel: HTMLElement;
  close: HTMLButtonElement;
  file: HTMLElement;
  progress: HTMLElement;
  current: HTMLElement;
  captionNote: HTMLElement;
  stop: HTMLButtonElement;
  message: HTMLElement;
}

/// 読み上げ 1 回分のキャンセルトークン。停止/クローズ/新規開始で cancelled を立てる。
interface ReadToken {
  cancelled: boolean;
}

let inputs: Inputs | null = null;
let activeToken: ReadToken | null = null;
let audioCtx: AudioContext | null = null;
let currentSource: AudioBufferSourceNode | null = null;

export function mountReaderPanel(): void {
  inputs = {
    panel: byId("reader-panel"),
    close: byId<HTMLButtonElement>("reader-close"),
    file: byId("reader-file"),
    progress: byId("reader-progress"),
    current: byId("reader-current"),
    captionNote: byId("reader-caption-note"),
    stop: byId<HTMLButtonElement>("reader-stop"),
    message: byId("reader-message"),
  };
  inputs.close.addEventListener("click", () => {
    void stopReading();
    inputs?.panel.classList.remove("visible");
  });
  inputs.stop.addEventListener("click", () => {
    void stopReading();
    showMessage("停止しました", false);
  });
}

export function openReaderPanel(): void {
  if (!inputs) return;
  inputs.panel.classList.add("visible");
}

export function isReaderOpen(): boolean {
  return inputs?.panel.classList.contains("visible") ?? false;
}

/// 読み上げを開始する (実行中なら止めてから)。dnd.ts から .txt/.md の DnD で呼ばれる。
export async function startReading(path: string): Promise<void> {
  if (!inputs) return;
  await stopReading();

  const token: ReadToken = { cancelled: false };
  activeToken = token;

  const fileName = path.split(/[\\/]/).pop() ?? path;
  inputs.file.textContent = fileName;
  inputs.progress.textContent = "読み込み中…";
  hideMessage();
  hideCaptionNote();

  let chunks: ReadingChunk[];
  try {
    chunks = await invoke<ReadingChunk[]>("reader_load_text", { path });
  } catch (err) {
    inputs.progress.textContent = "—";
    showMessage(`読み込み失敗: ${formatErr(err)}`, true);
    return;
  }
  if (token.cancelled) return;

  // 速度・音量は開始時点の設定を使う (読み上げ中の設定変更は次回から反映)
  let speed = 1.0;
  let volume = 1.0;
  let captionEnabled = false;
  try {
    const s = await invoke<Settings>("get_settings");
    if (!s.tts_enabled) {
      inputs.progress.textContent = "—";
      showMessage("設定で「声で話す (TTS)」を有効にしてください", true);
      return;
    }
    speed = s.tts_speed;
    volume = s.tts_volume;

    // 再生開始前の slot 検証 (script-reader-spec.md §2.7)。Irodori 実モデル時のみ、
    // 台本が使う slot に参照音声が揃っているか確認する。voicevox 時は検証不要。
    if (s.tts_engine === "irodori") {
      const assetsReady = await invoke<boolean>("irodori_assets_ready").catch(() => false);
      captionEnabled = assetsReady;
      if (assetsReady) {
        const usedSlots = new Set<VoiceSlot>(chunks.map((c) => c.slot));
        const refs = await invoke<VoiceRef[]>("voice_ref_list").catch(() => [] as VoiceRef[]);
        const availableSlots = new Set(refs.map((r) => r.slot));
        for (const slot of usedSlots) {
          if (!availableSlots.has(slot)) {
            inputs.progress.textContent = "—";
            showMessage(
              `slot=${slot} を使用していますが、${slot} の参照音声が未生成です` +
                "(設定 → 音声 → 参照音声で生成してください)",
              true,
            );
            return;
          }
        }
      }
    }
  } catch {
    // 設定が読めなくても既定値で続行
  }
  if (token.cancelled) return;

  // caption 注記 (script-reader-spec.md §2.5): caption 行を含み、かつ caption が
  // 無効な環境 (voicevox またはフォールバック中) のときだけ、再生中ずっと表示する。
  const hasCaption = chunks.some((c) => c.caption !== null);
  if (hasCaption && !captionEnabled) {
    showCaptionNote();
  }

  await invoke("set_reading_active", { active: true }).catch(() => {});
  inputs.stop.disabled = false;

  const synth = (chunk: ReadingChunk): Promise<string | null> => {
    const args: Record<string, unknown> = { text: chunk.text, slot: chunk.slot };
    if (chunk.caption !== null) {
      args.caption = chunk.caption;
    }
    return invoke<string>("synthesize_voice", args).catch((err) => {
      console.warn("[reader] chunk synth failed", err);
      return null;
    });
  };

  let skipped = 0;
  try {
    // 先読み 1: チャンク i を再生している間に i+1 を合成しておく
    let next: Promise<string | null> = synth(chunks[0]);
    for (let i = 0; i < chunks.length; i++) {
      if (token.cancelled) return;
      updateProgress(i + 1, chunks.length, chunks[i].text);
      const wav = await next;
      next = i + 1 < chunks.length ? synth(chunks[i + 1]) : Promise.resolve(null);
      if (token.cancelled) return;
      if (wav === null) {
        skipped += 1;
        continue;
      }
      const rate = clamp(speed + chunks[i].speed_offset, 0.5, 2.0);
      await playWav(wav, token, rate, volume);
      // チャンク間ポーズ: 文章の切れ目で一息つく。最終チャンクの後には入れない。
      if (i + 1 < chunks.length) {
        await sleepCancellable(chunks[i].pause_after_ms, token);
      }
    }
    if (!token.cancelled) {
      inputs.progress.textContent = `完了 (${chunks.length} チャンク)`;
      inputs.current.hidden = true;
      showMessage(
        skipped > 0 ? `読み上げ完了 (${skipped} 件スキップ)` : "読み上げ完了",
        skipped > 0,
      );
    }
  } finally {
    if (activeToken === token) {
      activeToken = null;
      inputs.stop.disabled = true;
      hideCaptionNote();
      await invoke("set_reading_active", { active: false }).catch(() => {});
    }
  }
}

/// 読み上げを停止する (再生中の音を止め、未処理チャンクを破棄)。
export async function stopReading(): Promise<void> {
  if (activeToken) {
    activeToken.cancelled = true;
    activeToken = null;
  }
  if (currentSource) {
    try {
      currentSource.stop();
    } catch {
      // 既に停止済みなら無視
    }
    currentSource = null;
  }
  if (inputs) {
    inputs.stop.disabled = true;
    inputs.progress.textContent = "—";
    inputs.current.hidden = true;
    hideCaptionNote();
  }
  await invoke("set_reading_active", { active: false }).catch(() => {});
}

function updateProgress(n: number, m: number, chunk: string): void {
  if (!inputs) return;
  inputs.progress.textContent = `${n} / ${m}`;
  const preview = chunk.length > 60 ? `${chunk.slice(0, 60)}…` : chunk;
  inputs.current.textContent = preview;
  inputs.current.hidden = false;
}

/// base64 WAV を専用 AudioContext で再生し、終了まで待つ。
function playWav(
  b64: string,
  token: ReadToken,
  speed: number,
  volume: number,
): Promise<void> {
  return new Promise((resolve) => {
    void (async () => {
      try {
        const bytes = base64ToBytes(b64);
        const ctx = ensureAudioCtx();
        const ab = new ArrayBuffer(bytes.byteLength);
        new Uint8Array(ab).set(bytes);
        const buffer = await ctx.decodeAudioData(ab);
        if (token.cancelled) {
          resolve();
          return;
        }
        const source = ctx.createBufferSource();
        source.buffer = buffer;
        source.playbackRate.value = clamp(speed, 0.5, 2.0);
        const gain = ctx.createGain();
        gain.gain.value = clamp(volume, 0, 2);
        source.connect(gain);
        gain.connect(ctx.destination);
        currentSource = source;
        source.onended = () => {
          if (currentSource === source) currentSource = null;
          resolve();
        };
        source.start();
      } catch (err) {
        console.warn("[reader] play failed", err);
        resolve();
      }
    })();
  });
}

/// キャンセル可能な sleep。50ms 刻みで token を確認し、停止時は即座に抜ける。
function sleepCancellable(ms: number, token: ReadToken): Promise<void> {
  return new Promise((resolve) => {
    const start = performance.now();
    const tick = () => {
      if (token.cancelled || performance.now() - start >= ms) {
        resolve();
        return;
      }
      setTimeout(tick, 50);
    };
    tick();
  });
}

function ensureAudioCtx(): AudioContext {
  if (!audioCtx) {
    audioCtx = new AudioContext();
  }
  return audioCtx;
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return bytes;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function showMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.message.textContent = msg;
  inputs.message.classList.toggle("error", isError);
  inputs.message.hidden = false;
}

function hideMessage(): void {
  if (!inputs) return;
  inputs.message.hidden = true;
}

/// caption 無視の常設注記を表示する (script-reader-spec.md §2.5)。
function showCaptionNote(): void {
  if (!inputs) return;
  inputs.captionNote.hidden = false;
}

function hideCaptionNote(): void {
  if (!inputs) return;
  inputs.captionNote.hidden = true;
}

function formatErr(err: unknown): string {
  return typeof err === "string" ? err : err instanceof Error ? err.message : String(err);
}

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`要素が見つかりません: ${id}`);
  return el as T;
}
