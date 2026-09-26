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
/// 表示するのは VOICEVOX のときだけ（VOICEVOX の音声ライブラリの規約が「VOICEVOX:話者名」の表示を求める）。
/// Irodori-TTS のモデル（MIT）には音声へのクレジット表示の定めが無い。ただし**モデルカードの倫理条項**（本人の
/// 同意なく他人の声をクローン・なりすましに使わない／人を欺く目的で使わない）があり、ugg は取得と更新の確認と
/// 取説で提示する（v0.5.7 項目 8、spec §6.0）。以前は「規約上の帰属表示義務がない」と言い切っていた。
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
