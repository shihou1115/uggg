import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  hideAllBalloons,
  hideBalloon,
  reposition,
  showBalloon,
} from "../dialogue/balloon";
import { newToken, typeInto, type TypewriterToken } from "../dialogue/typewriter";
import { setPose } from "../stage/character";
import type { BalloonSlot, DialogueResponse, SlotName, SpeechTurn, TalkSpeed } from "../types";

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
/// M9 🔕: 表示中のフィードバック可能発話 (speech_id + category)。
/// 発話が置き換わるたびに更新し、古い発話への誤適用を speech_id で防ぐ (バック側でも照合)。
let currentSpeechMeta: { id: string; category: string } | null = null;
let muteBtn: HTMLElement | null = null;

export function setSpeaker(s: SpeakerLike): void {
  ttsSpeaker = s;
}

export function setTalkSpeed(speed: TalkSpeed): void {
  talkSpeed = speed;
}

export async function startListening(): Promise<void> {
  muteBtn = document.getElementById("balloon-mute");
  muteBtn?.addEventListener("click", (ev) => {
    ev.stopPropagation();
    void onMuteClick();
  });
  await listen<DialogueResponse>("dialogue", async (event) => {
    await renderResponse(event.payload);
  });
}

/// 🔕 クリック:「いまのは邪魔」をバックへ送り、発話を畳む。
async function onMuteClick(): Promise<void> {
  const meta = currentSpeechMeta;
  if (!meta) return;
  try {
    await invoke("feedback_speech", { speechId: meta.id, category: meta.category });
  } catch (err) {
    console.error("feedback_speech failed", err);
  }
  cancelSpeech();
}

/// 表示中発話の 🔕 メタを更新し、ボタンの表示を切り替える。
function setSpeechMeta(resp: DialogueResponse | null): void {
  const allowed = !!(resp && resp.feedback_allowed && resp.speech_id && resp.category);
  currentSpeechMeta = allowed
    ? { id: resp!.speech_id as string, category: resp!.category as string }
    : null;
  muteBtn?.classList.toggle("visible", allowed);
}

interface Turn {
  charSlot: SlotName;
  balloonSlot: BalloonSlot;
  turn: SpeechTurn;
}

/// pattern (spec §4.2.4) からターン列を組み立てる:
///   1: main → sub
///   2: sub → main
///   3: main → sub → main (3ターン目は #balloon-extra、話者は main)
///   4: sub → main → sub (3ターン目は #balloon-extra、話者は sub)
/// sub/extra が欠けている (サブ無しゴースト・安全縮退済み) 場合はそのターンを飛ばす。
function buildTurns(resp: DialogueResponse): Turn[] {
  const turns: Turn[] = [];
  const subTurn = resp.sub ? { charSlot: "sub" as const, balloonSlot: "sub" as const, turn: resp.sub } : null;
  const mainTurn = { charSlot: "main" as const, balloonSlot: "main" as const, turn: resp.main };
  if (resp.pattern === 2 || resp.pattern === 4) {
    if (subTurn) turns.push(subTurn);
    turns.push(mainTurn);
  } else {
    turns.push(mainTurn);
    if (subTurn) turns.push(subTurn);
  }
  if (resp.pattern === 3 && resp.extra) {
    turns.push({ charSlot: "main", balloonSlot: "extra", turn: resp.extra });
  } else if (resp.pattern === 4 && resp.extra) {
    turns.push({ charSlot: "sub", balloonSlot: "extra", turn: resp.extra });
  }
  return turns;
}

/// DialogueResponse を 1 件レンダリングする。
/// 連続呼び出しは前ターンを cancel して即座に新ターンを開始する。
export async function renderResponse(resp: DialogueResponse): Promise<void> {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  const token = newToken();
  currentToken = token;

  promptSlot = null; // 促し表示は新しい応答で置き換えられる
  setSpeechMeta(resp); // M9 🔕: フィードバック可能発話なら 🔕 を出す
  hideAllBalloons();

  for (const t of buildTurns(resp)) {
    if (token.cancelled) return;
    await speakSlot(token, t.charSlot, t.balloonSlot, t.turn);
  }
  if (token.cancelled) return;
  // 保険: speakSlot が各ターンで再生完了を待つので通常は即座に解決するが、
  // 将来 fire-and-forget が再び混入しても spec §4.1.3 (発話完了後に消去) を守れるようにする。
  await ttsSpeaker?.whenIdle();
  if (token.cancelled) return;
  await sleep(holdDuration(resp));
  if (token.cancelled) return;
  // 全ターンの描画+発話完了後に一括消去 (spec §4.1.3)。extra を含め、表示していない
  // 枠を隠しても無害 (hideBalloon は冪等)。
  hideAllBalloons();
}

/// 入力促し (spec §4.3.1): クリックされたキャラ単独の短い発話。
/// 通常の応答と違い自動では消さず、入力欄が閉じるとき clearPrompt() で消す。
export async function renderPrompt(slot: SlotName, turn: SpeechTurn): Promise<void> {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  const token = newToken();
  currentToken = token;

  setSpeechMeta(null);
  hideAllBalloons();
  promptSlot = slot;
  await speakSlot(token, slot, slot, turn);
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
  setSpeechMeta(null);
  hideAllBalloons();
  if (subTurn) {
    await speakSlot(token, "sub", "sub", subTurn);
    if (token.cancelled) return false;
  }
  if (mainTurn) {
    await speakSlot(token, "main", "main", mainTurn);
  } else {
    showBalloon("main", "main");
  }
  return !token.cancelled;
}

/// 進行中の発話・促し表示を打ち切って全バルーンを隠す (メニュークローズ等から呼ぶ)。
export function cancelSpeech(): void {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  promptSlot = null;
  setSpeechMeta(null);
  hideAllBalloons();
}

/// `charSlot` = 発話するキャラ (pose・TTS 話者)、`balloonSlot` = 表示先の吹き出し枠。
/// 通常ターンは両者が一致するが、掛け合いパターン3/4 の3ターン目は
/// charSlot=main/sub・balloonSlot=extra になる (spec §4.1.3、architecture §10.4)。
async function speakSlot(
  token: TypewriterToken,
  charSlot: SlotName,
  balloonSlot: BalloonSlot,
  turn: SpeechTurn,
): Promise<void> {
  if (turn.pose) setPose(charSlot, turn.pose);
  const textEl = showBalloon(balloonSlot, charSlot);
  // TTS フック: 描画と再生を**同時に開始し、両方の完了を待つ** (spec §4.1.3)。
  //
  // 以前は `void` で Promise を捨てて描画だけを待っていたため、main の音声が鳴っている
  // 最中に sub の文字表示が始まり、長文・低速 TTS では音声の途中で吹き出しが消えていた
  // (Codex レビュー指摘 5、2026-08-23)。speaker 側の Promise は実際の再生終了で解決する。
  //
  // 待っても声なし運用のテンポは落ちない: TTS 無効時と空文字は `speak` が即 resolve し、
  // 合成失敗・中断 (interrupt による世代交代) でも必ず resolve される設計になっている。
  const spoken = ttsSpeaker?.speak(charSlot, turn.text);
  await typeInto(textEl, turn.text, talkSpeed, token, () => reposition(balloonSlot));
  await spoken;
}

function holdDuration(resp: DialogueResponse): number {
  const total =
    resp.main.text.length + (resp.sub?.text.length ?? 0) + (resp.extra?.text.length ?? 0);
  // ベース 2.0 秒 + 文字数 × 80ms、上限 12 秒。M1 検証用にやや長め。
  return Math.min(12000, 2000 + total * 80);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
