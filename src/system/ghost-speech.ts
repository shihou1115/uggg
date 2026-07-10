import { listen } from "@tauri-apps/api/event";

import {
  hideAllBalloons,
  hideBalloon,
  reposition,
  showBalloon,
} from "../dialogue/balloon";
import { newToken, typeInto, type TypewriterToken } from "../dialogue/typewriter";
import { setPose } from "../stage/character";
import type { DialogueResponse, SlotName, SpeechTurn, TalkSpeed } from "../types";

interface SpeakerLike {
  speak(slot: SlotName, text: string): Promise<void>;
  interrupt(): void;
  whenIdle(): Promise<void>;
  isAudible(): boolean;
}

let currentToken: TypewriterToken | null = null;
let talkSpeed: TalkSpeed = "normal";
let ttsSpeaker: SpeakerLike | null = null;
/// 入力促し (spec §4.3.1) を表示中の slot。入力欄が閉じるまで吹き出しを保持する。
let promptSlot: SlotName | null = null;

export function setSpeaker(s: SpeakerLike): void {
  ttsSpeaker = s;
}

export function setTalkSpeed(speed: TalkSpeed): void {
  talkSpeed = speed;
}

export async function startListening(): Promise<void> {
  await listen<DialogueResponse>("dialogue", async (event) => {
    await renderResponse(event.payload);
  });
}

/// DialogueResponse を 1 件レンダリングする。
/// pattern により main/sub の順序を切り替える:
///   1: main → sub
///   2: sub → main
/// 連続呼び出しは前ターンを cancel して即座に新ターンを開始する。
export async function renderResponse(resp: DialogueResponse): Promise<void> {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  const token = newToken();
  currentToken = token;

  promptSlot = null; // 促し表示は新しい応答で置き換えられる
  hideAllBalloons();

  const subFirst = resp.pattern === 2 && resp.sub != null;
  const turns: Array<{ slot: SlotName; turn: SpeechTurn }> = subFirst
    ? [
        { slot: "sub", turn: resp.sub as SpeechTurn },
        { slot: "main", turn: resp.main },
      ]
    : [
        { slot: "main", turn: resp.main },
        ...(resp.sub ? [{ slot: "sub" as SlotName, turn: resp.sub }] : []),
      ];

  for (const t of turns) {
    if (token.cancelled) return;
    await speakSlot(token, t.slot, t.turn);
  }
  if (token.cancelled) return;
  await sleep(holdDuration(resp));
  if (token.cancelled) return;
  hideBalloon("main");
  if (resp.sub) hideBalloon("sub");
}

/// 入力促し (spec §4.3.1): クリックされたキャラ単独の短い発話。
/// 通常の応答と違い自動では消さず、入力欄が閉じるとき clearPrompt() で消す。
export async function renderPrompt(slot: SlotName, turn: SpeechTurn): Promise<void> {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  const token = newToken();
  currentToken = token;

  hideAllBalloons();
  promptSlot = slot;
  await speakSlot(token, slot, turn);
}

/// 促し発話の吹き出しを消す (入力欄クローズ時に input.ts から呼ばれる)。
export function clearPrompt(): void {
  if (promptSlot === null) return;
  hideBalloon(promptSlot);
  promptSlot = null;
}

/// メニュー導線 (spec §4.3.5): sub の誘導セリフ (任意) → main の前口上、の順に発話する。
/// sub の吹き出しは表示したまま main に遷移する (掛け合いと同じ見え方)。
/// 前口上が無い辞書でも main バルーンだけは開く (メニューの器)。自動では消さない。
/// 戻り値: 途中で cancel されず最後まで到達したら true。
export async function renderMenuPrompt(
  subTurn: SpeechTurn | null,
  mainTurn: SpeechTurn | null,
): Promise<boolean> {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  const token = newToken();
  currentToken = token;

  promptSlot = null;
  hideAllBalloons();
  if (subTurn) {
    await speakSlot(token, "sub", subTurn);
    if (token.cancelled) return false;
  }
  if (mainTurn) {
    await speakSlot(token, "main", mainTurn);
  } else {
    showBalloon("main");
  }
  return !token.cancelled;
}

/// 進行中の発話・促し表示を打ち切って全バルーンを隠す (メニュークローズ等から呼ぶ)。
export function cancelSpeech(): void {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  promptSlot = null;
  hideAllBalloons();
}

async function speakSlot(
  token: TypewriterToken,
  slot: SlotName,
  turn: SpeechTurn,
): Promise<void> {
  if (turn.pose) setPose(slot, turn.pose);
  const textEl = showBalloon(slot);
  // TTS フック: 描画開始と同時に再生開始 (順序保証は speaker 側のキュー)。
  // 失敗は内部で握りつぶされる (声なし継続)。
  void ttsSpeaker?.speak(slot, turn.text);
  await typeInto(textEl, turn.text, talkSpeed, token, () => reposition(slot));
}

function holdDuration(resp: DialogueResponse): number {
  const total = resp.main.text.length + (resp.sub?.text.length ?? 0);
  // ベース 2.0 秒 + 文字数 × 80ms、上限 12 秒。M1 検証用にやや長め。
  return Math.min(12000, 2000 + total * 80);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
