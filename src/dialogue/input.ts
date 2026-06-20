import { invoke } from "@tauri-apps/api/core";

import type { DialogueResponse } from "../types";

let renderRoute: ((resp: DialogueResponse) => Promise<void>) | null = null;
let view: InputView | null = null;

interface InputView {
  root: HTMLElement;
  input: HTMLInputElement;
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
  layer.appendChild(root);
  view = { root, input, busy: false };

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

export function openInput(): void {
  if (!view) return;
  view.root.classList.add("visible");
  view.input.focus();
}

export function closeInput(): void {
  if (!view) return;
  view.root.classList.remove("visible");
  view.input.blur();
}

export function toggleInput(): void {
  if (!view) return;
  if (view.root.classList.contains("visible")) {
    closeInput();
  } else {
    openInput();
  }
}

export function isInputOpen(): boolean {
  return !!view?.root.classList.contains("visible");
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
