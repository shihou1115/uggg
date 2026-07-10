import { invoke } from "@tauri-apps/api/core";

import { GAP_X, HEAD_RATIO, MARGIN } from "./balloon";
import { clearPrompt } from "../system/ghost-speech";
import type { DialogueResponse, SlotName } from "../types";

let renderRoute: ((resp: DialogueResponse) => Promise<void>) | null = null;
let view: InputView | null = null;
/// 入力欄のアンカー先キャラ (spec §4.3.1: クリックされたキャラのバルーン上側に出す)。
let targetSlot: SlotName = "main";
/// 📋 貼り付けボタンの表示可否 (= Settings.tools_enabled)。OFF では非表示。
let toolsEnabled = false;

interface InputView {
  root: HTMLElement;
  input: HTMLInputElement;
  pasteBtn: HTMLButtonElement;
  busy: boolean;
}

export function mountInput(
  renderResponse: (resp: DialogueResponse) => Promise<void>,
): void {
  if (view) return;
  renderRoute = renderResponse;
  const layer = document.getElementById("ui-layer");
  if (!layer) throw new Error("ui-layer DOM not found");
  const root = document.createElement("div");
  root.id = "chat-input-wrap";
  root.classList.add("solid");
  const input = document.createElement("input");
  input.type = "text";
  input.placeholder = "メッセージを入力 (Enter で送信 / Esc で閉じる)";
  input.autocomplete = "off";
  input.spellcheck = false;
  root.appendChild(input);
  // M5-B: クリップボード貼り付けボタン (📋)。tools_enabled=true のときだけ呼び出しが通る。
  const pasteBtn = document.createElement("button");
  pasteBtn.type = "button";
  pasteBtn.id = "chat-paste-clipboard";
  pasteBtn.title = "クリップボードを末尾に貼り付け (tools_enabled 必須)";
  pasteBtn.textContent = "📋";
  pasteBtn.addEventListener("click", () => void onPasteClipboard());
  pasteBtn.hidden = !toolsEnabled;
  root.appendChild(pasteBtn);
  layer.appendChild(root);
  view = { root, input, pasteBtn, busy: false };

  input.addEventListener("keydown", (ev) => {
    if (ev.isComposing) return;
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeInput();
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      void submit();
    }
  });
}

/// slot のバルーンの上側に入力欄を開く (spec §4.3.1)。
export function openInputFor(slot: SlotName): void {
  if (!view) return;
  targetSlot = slot;
  view.root.classList.add("visible");
  positionAboveBalloon();
  view.input.focus();
}

/// Settings.tools_enabled の反映: OFF のとき 📋 貼り付けボタンを非表示にする。
/// boot 時と設定保存時に main.ts から呼ばれる。
export function setToolsEnabled(enabled: boolean): void {
  toolsEnabled = enabled;
  if (view) {
    view.pasteBtn.hidden = !enabled;
  }
}

export function closeInput(): void {
  if (!view) return;
  view.root.classList.remove("visible");
  view.input.blur();
  clearPrompt(); // 促し発話の吹き出しも一緒に閉じる
}

export function isInputOpen(): boolean {
  return !!view?.root.classList.contains("visible");
}

/// 入力欄をアンカー先キャラのバルーンの上側に置く。
/// バルーンと同じ幾何 (GAP_X / HEAD_RATIO、左に収まらなければ右横へ反転) を使い、
/// バルーン上端 (= キャラの顔の高さ) のさらに上に底辺を合わせる。
function positionAboveBalloon(): void {
  if (!view) return;
  const char = document.getElementById(`char-${targetSlot}`);
  if (!char || !char.classList.contains("ready")) return;
  const rect = char.getBoundingClientRect();
  const w = view.root.offsetWidth || 320;
  const h = view.root.offsetHeight || 46;

  const balloonTop = rect.top + rect.height * HEAD_RATIO;
  let top = Math.round(balloonTop - MARGIN - h);
  if (top < MARGIN) top = MARGIN;

  let left = Math.round(rect.left - GAP_X - w);
  if (left < MARGIN) {
    const rightSide = Math.round(rect.right + GAP_X);
    left = rightSide + w <= window.innerWidth - MARGIN ? rightSide : MARGIN;
  }
  view.root.style.left = `${left}px`;
  view.root.style.top = `${top}px`;
}

/// キャラのドラッグ移動・スケール変更に追従する (charpos.ts から呼ばれる)。
/// 動いたのがアンカー先でなければ何もしない。
export function refreshInputPosition(movedSlot: SlotName): void {
  if (!isInputOpen() || movedSlot !== targetSlot) return;
  positionAboveBalloon();
}

async function onPasteClipboard(): Promise<void> {
  if (!view) return;
  try {
    const txt = await invoke<string>("read_clipboard_text");
    if (!txt) return;
    // 既存テキストの末尾に貼り付け (前後にスペースを入れて区切り)
    const sep = view.input.value.length > 0 && !view.input.value.endsWith(" ") ? " " : "";
    view.input.value = view.input.value + sep + txt;
    view.input.focus();
  } catch (err) {
    console.warn("read_clipboard_text failed", err);
  }
}

async function submit(): Promise<void> {
  if (!view || view.busy || !renderRoute) return;
  const text = view.input.value.trim();
  if (!text) return;
  view.busy = true;
  view.input.disabled = true;
  try {
    const resp = await invoke<DialogueResponse>("send_user_message", { text });
    view.input.value = "";
    closeInput();
    await renderRoute(resp);
  } catch (err) {
    console.error("send_user_message failed", err);
  } finally {
    if (view) {
      view.input.disabled = false;
      view.busy = false;
    }
  }
}
