//! VOICEVOX クレジット表示 (spec §4.5.1 / architecture §A-5)。
//!
//! VOICEVOX 音声モデルの利用規約により、合成音声を利用するときは「VOICEVOX:キャラ名」
//! のクレジット表記が必要。声が有効な間はステージ下端に常時表示する。
//!
//! 静的配置 (WebView2 透過バグ対策) で index.html に `#tts-credit` を置き、
//! .visible クラスのトグルで表示・非表示を切り替える。

import { invoke } from "@tauri-apps/api/core";

import type { GhostSpeakers, SlotName } from "../types";
import type { VoiceOption } from "./types";

/// `synthesize_voice` の戻りの `engine` が VOICEVOX を指す値（設定の `tts_engine` と同じ綴り）。
const VOICEVOX_ENGINE = "voicevox_core";

interface Last {
  enabled: boolean;
  engine: string;
  speakers: GhostSpeakers;
}

interface CreditState {
  el: HTMLElement;
  voices: VoiceOption[] | null;
  last: Last | null;
  /// slot ごとに、**直近の合成を VOICEVOX で鳴らしたか**（`noteSynthesized`）。
  playedByVoicevox: Record<SlotName, boolean>;
  /// 古い描画の結果で新しい状態を上書きしないための世代。
  seq: number;
}

let state: CreditState | null = null;

export function mountCredit(): void {
  const el = document.getElementById("tts-credit");
  if (!el) return;
  state = { el, voices: null, last: null, playedByVoicevox: { main: false, sub: false }, seq: 0 };
}

/// クレジットを出す slot（純粋部分）。**実際に VOICEVOX で鳴らしたか**で決める（v0.6.0 項目 2、spec §6.0）。
/// - 声が無効: 無し
/// - エンジンが VOICEVOX: 両方（設定どおり VOICEVOX で鳴る）
/// - 高品質モード: **直近の合成が VOICEVOX だった slot だけ**（参照音声が無い・Irodori が使えないときのフォールバック。
///   規約上の常時表示が、VOICEVOX が鳴り続ける間に欠けないように）
export function creditSlots(
  enabled: boolean,
  engine: string,
  played: Record<SlotName, boolean>,
): SlotName[] {
  if (!enabled) return [];
  if (engine === VOICEVOX_ENGINE) return ["main", "sub"];
  return (["main", "sub"] as const).filter((slot) => played[slot]);
}

/// TTS が有効になった or 話者が変わったときに呼ぶ。
/// 表示するのは VOICEVOX で鳴らすとき（VOICEVOX の音声ライブラリの規約が「VOICEVOX:話者名」の表示を求める）。
/// Irodori-TTS のモデル（MIT）には音声へのクレジット表示の定めが無い。ただし**モデルカードの倫理条項**（本人の
/// 同意なく他人の声をクローン・なりすましに使わない／人を欺く目的で使わない）があり、ugg は取得と更新の確認と
/// 取説で提示する（v0.5.7 項目 8、spec §6.0）。以前は「規約上の帰属表示義務がない」と言い切っていた。
/// 高品質モードでも、フォールバックで VOICEVOX が鳴った slot は出す（`noteSynthesized`）。
/// `speakers` は**読み込み中のゴーストの話者**（`speakersFor`。v0.6.0 項目 2）。設定の単一の話者ではない。
export async function refreshCredit(
  enabled: boolean,
  engine: string,
  speakers: GhostSpeakers,
): Promise<void> {
  if (!state) return;
  // エンジンを替えたら、前のエンジンで見た「VOICEVOX で鳴らした」は引き継がない
  if (state.last && state.last.engine !== engine) {
    state.playedByVoicevox = { main: false, sub: false };
  }
  state.last = { enabled, engine, speakers };
  await render();
}

/// 合成の結果（`synthesize_voice` の戻りの `engine`）を伝える。VOICEVOX で鳴らした slot はクレジットを出し、
/// 高品質モードで鳴らした slot は外す。変わらなければ何もしない。
export function noteSynthesized(slot: SlotName, engine: string): void {
  if (!state) return;
  const played = engine === VOICEVOX_ENGINE;
  if (state.playedByVoicevox[slot] === played) return;
  state.playedByVoicevox[slot] = played;
  void render();
}

async function render(): Promise<void> {
  if (!state || !state.last) return;
  const current = state;
  const seq = ++current.seq;
  const { enabled, engine, speakers } = state.last;
  const slots = creditSlots(enabled, engine, state.playedByVoicevox);
  if (slots.length === 0) {
    hide(current);
    return;
  }
  try {
    const voices = current.voices ?? (await invoke<VoiceOption[]>("list_voices"));
    if (seq !== current.seq) return;
    current.voices = voices;
    current.el.textContent = formatCreditText(voices, speakers, slots);
    current.el.classList.add("visible");
  } catch (err) {
    if (seq !== current.seq) return;
    // 資産未 DL 等で list_voices が失敗する場合はクレジット表示自体を隠す
    console.warn("[tts-credit] list_voices failed", err);
    hide(current);
  }
}

function hide(s: CreditState): void {
  s.el.classList.remove("visible");
  s.el.textContent = "";
}

/// クレジットの文面（純粋部分）。出す slot の話者だけを並べる（同じ話者は 1 つにまとめる）。
export function formatCreditText(
  voices: VoiceOption[],
  speakers: GhostSpeakers,
  slots: SlotName[],
): string {
  const findName = (id: number): string => {
    const v = voices.find((x) => x.id === id);
    if (!v) return `#${id}`;
    // 「四国めたん (ノーマル)」→ 「四国めたん」だけクレジットに使う
    const m = v.name.match(/^([^(]+)/);
    return m ? m[1].trim() : v.name;
  };
  // 同じ話者（style ID）は 1 つにまとめる（main と sub が同じ話者のとき）
  const ids = [...new Set(slots.map((slot) => speakers[slot]))];
  return `VOICEVOX:${ids.map(findName).join(" / ")}`;
}
