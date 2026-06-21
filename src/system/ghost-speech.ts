import { listen } from "@tauri-apps/api/event";

import { hideAllBalloons, hideBalloon, reposition, showBalloon } from "../dialogue/balloon";
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
