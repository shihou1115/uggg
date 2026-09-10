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

// === 通知のキュー (spec §4.1.3、v0.5.3) ==================================
//
// 「新発話で interrupt」は §4.1.3 の明文だが、**通知はその例外**にする。
// 通知は「一度出したら記録が残り、二度と出ない」ので、消されると告知済みの記録だけが
// 残って永久に届かない。実際に起きていたのは次の 2 つ:
//   - 同じ tick で 2 件のカレンダー通知が配達されると、後の 1 件が前を消す
//   - 80% コスト警告を emit した直後にチャット応答が返ると、応答が警告を消す
//
// 規則は 2 つだけ:
//   1. 通知は**割り込まない**。描画中なら待ってから 1 件ずつ出す。
//   2. 通知は**割り込まれても捨てない**。描画中に他の発話が来たらキューの先頭へ戻す
//      （ユーザーの入力への応答を待たせないため、割り込み自体は許す）。

/// 待機中の通知。先頭から 1 件ずつ描画する。
const noticeQueue: DialogueResponse[] = [];
/// いま描画中の通知（割り込まれたら積み直す対象）。通常発話の描画中は null。
let renderingNotice: DialogueResponse | null = null;
/// ステージ（吹き出し + 音声）を誰かが使っているか。通知はこれが空くまで待つ。
let stageBusy = false;
/// pumpNotices の再入防止。
let pumping = false;

/// この発話は通知か。
/// - `kind === "system_message"`: `notify()` 経由（コスト警告・降格告知・DL 完了など）
/// - `priority === "notice"`: `deliver_event` の Notice（リマインダー・カレンダー）
///
/// Ambient（状況発話・独り言・定例会話）は含めない。**消えても記録が残らず、
/// また出る**ので守る必要が無く、含めるとユーザーの操作を待たせるだけになる。
function isNotice(resp: DialogueResponse): boolean {
  return resp.kind === "system_message" || resp.priority === "notice";
}

/// 進行中の描画を打ち切ってステージを空ける。
/// **打ち切る相手が通知ならキューの先頭へ積み直す**（消すと二度と出ない）。
function takeStage(): void {
  if (currentToken) currentToken.cancelled = true;
  ttsSpeaker?.interrupt();
  if (renderingNotice) {
    noticeQueue.unshift(renderingNotice);
    renderingNotice = null;
  }
  stageBusy = false;
}

/// ステージが空いている間、通知を 1 件ずつ描画する。
async function pumpNotices(): Promise<void> {
  if (pumping) return;
  pumping = true;
  try {
    while (noticeQueue.length > 0 && !stageBusy) {
      const next = noticeQueue.shift();
      if (!next) break;
      await renderNow(next);
    }
  } finally {
    pumping = false;
  }
}

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
/// **通知だけは割り込まず、ステージが空いてから 1 件ずつ出す** (spec §4.1.3、v0.5.3)。
export async function renderResponse(resp: DialogueResponse): Promise<void> {
  if (isNotice(resp)) {
    noticeQueue.push(resp);
    void pumpNotices();
    return;
  }
  await renderNow(resp);
  // 割り込みで積み直した分・待たせていた分を、ステージが空いたここで出す。
  void pumpNotices();
}

/// 実際の描画。呼び出した時点でステージを奪う。
async function renderNow(resp: DialogueResponse): Promise<void> {
  takeStage();
  const token = newToken();
  currentToken = token;
  stageBusy = true;
  const notice = isNotice(resp) ? resp : null;
  renderingNotice = notice;

  try {
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
  } finally {
    // **ステージの所有者だけが後片付けをする。** 割り込まれた側は await から戻った
    // 時点で既に所有者が交代しており、ここで解放すると次の描画中に「空き」と
    // 誤認されて通知が割り込む。
    if (currentToken === token) {
      stageBusy = false;
      renderingNotice = null;
    }
  }
}

/// 入力促し (spec §4.3.1): クリックされたキャラ単独の短い発話。
/// 通常の応答と違い自動では消さず、入力欄が閉じるとき clearPrompt() で消す。
export async function renderPrompt(slot: SlotName, turn: SpeechTurn): Promise<void> {
  takeStage();
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
  // 促しが消えてステージが空いたので、待たせていた通知があれば出す。
  void pumpNotices();
}

/// メニュー導線 (spec §4.3.5): sub の誘導セリフ (任意) → main の前口上、の順に発話する。
/// sub の吹き出しは表示したまま main に遷移する (掛け合いと同じ見え方)。
/// 前口上が無い辞書でも main バルーンだけは開く (メニューの器)。自動では消さない。
/// 戻り値: 途中で cancel されず最後まで到達したら true。
export async function renderMenuPrompt(
  subTurn: SpeechTurn | null,
  mainTurn: SpeechTurn | null,
): Promise<boolean> {
  takeStage();
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
  takeStage();
  // 所有者を残さない。残すと、打ち切られた描画が await から戻ったときに
  // 自分をまだ所有者と見なして後片付けをしてしまう。
  currentToken = null;
  promptSlot = null;
  setSpeechMeta(null);
  hideAllBalloons();
  void pumpNotices();
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
