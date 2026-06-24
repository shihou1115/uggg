//! VOICEVOX クレジット表示 (spec §4.5.1 / architecture §A-5)。
//!
//! VOICEVOX 音声モデルの利用規約により、合成音声を利用するときは「VOICEVOX:キャラ名」
//! のクレジット表記が必要。声が有効な間はステージ下端に常時表示する。
//!
//! 静的配置 (WebView2 透過バグ対策) で index.html に `#tts-credit` を置き、
//! .visible クラスのトグルで表示・非表示を切り替える。

import { invoke } from "@tauri-apps/api/core";

import type { VoiceOption } from "./types";

interface CreditState {
  el: HTMLElement;
  voices: VoiceOption[] | null;
}

let state: CreditState | null = null;

export function mountCredit(): void {
  const el = document.getElementById("tts-credit");
  if (!el) return;
  state = { el, voices: null };
}

/// TTS が有効になった or 話者が変わったときに呼ぶ。
/// engine が "voicevox_core" 以外 (Irodori 等) のときは規約上の帰属表示義務がないので非表示。
export async function refreshCredit(
  enabled: boolean,
  engine: string,
  speakerMain: number,
  speakerSub: number,
): Promise<void> {
  if (!state) return;
  if (!enabled || engine !== "voicevox_core") {
    state.el.classList.remove("visible");
    state.el.textContent = "";
    return;
  }
  try {
    const voices = await invoke<VoiceOption[]>("list_voices");
    state.voices = voices;
    const text = formatCreditText(voices, speakerMain, speakerSub);
    state.el.textContent = text;
    state.el.classList.add("visible");
  } catch (err) {
    // 資産未 DL 等で list_voices が失敗する場合はクレジット表示自体を隠す
    console.warn("[tts-credit] list_voices failed", err);
    state.el.classList.remove("visible");
    state.el.textContent = "";
  }
}

function formatCreditText(voices: VoiceOption[], main: number, sub: number): string {
  const findName = (id: number): string => {
    const v = voices.find((x) => x.id === id);
    if (!v) return `#${id}`;
    // 「四国めたん (ノーマル)」→ 「四国めたん」だけクレジットに使う
    const m = v.name.match(/^([^(]+)/);
    return m ? m[1].trim() : v.name;
  };
  const mainName = findName(main);
  if (main === sub) {
    return `VOICEVOX:${mainName}`;
  }
  const subName = findName(sub);
  return `VOICEVOX:${mainName} / ${subName}`;
}
