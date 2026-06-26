//! 確認ダイアログ (M6 動作確認時に判明したフリーズ問題の回避)。
//!
//! `window.confirm` は WebView2 + Tauri 2 + `alwaysOnTop`+透過ウインドウの組み合わせで
//! native modal の input message loop が動かなくなり、OK/キャンセル が一切押せない状態で
//! UI が完全フリーズする問題があった。`#ugg-confirm-panel` を index.html に静的配置した
//! 独自モーダルに置き換えて、通常の click event だけで OK/キャンセルを受け取る形にする。
//!
//! 使用側は `await uggConfirm("メッセージ")` で `true`/`false` を取得する。

interface Inputs {
  panel: HTMLElement;
  title: HTMLElement;
  msg: HTMLElement;
  ok: HTMLButtonElement;
  cancel: HTMLButtonElement;
}

let inputs: Inputs | null = null;
let pending: ((value: boolean) => void) | null = null;

export function mountConfirm(): void {
  if (inputs) return;
  inputs = {
    panel: byId("ugg-confirm-panel"),
    title: byId("ugg-confirm-title"),
    msg: byId("ugg-confirm-msg"),
    ok: byId<HTMLButtonElement>("ugg-confirm-ok"),
    cancel: byId<HTMLButtonElement>("ugg-confirm-cancel"),
  };
  inputs.ok.addEventListener("click", () => closeWith(true));
  inputs.cancel.addEventListener("click", () => closeWith(false));
  inputs.panel.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeWith(false);
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      closeWith(true);
    }
  });
}

/// 確認ダイアログを表示。OK で true、キャンセル/Esc で false を返す。
/// すでに別の確認が表示中なら、それを「キャンセル扱い」で閉じて新規表示する。
export function uggConfirm(message: string, title = "確認"): Promise<boolean> {
  if (!inputs) {
    // mount し忘れの保険: fallback として window.confirm を使う
    return Promise.resolve(window.confirm(message));
  }
  // 進行中の Promise があればキャンセルで閉じる
  if (pending) {
    const prev = pending;
    pending = null;
    prev(false);
  }
  inputs.title.textContent = title;
  inputs.msg.textContent = message;
  inputs.panel.classList.add("visible");
  // 初期フォーカスは OK にして Enter 確定を意図しやすく
  setTimeout(() => inputs?.ok.focus(), 0);
  return new Promise<boolean>((resolve) => {
    pending = resolve;
  });
}

function closeWith(value: boolean): void {
  if (!inputs) return;
  inputs.panel.classList.remove("visible");
  if (pending) {
    const r = pending;
    pending = null;
    r(value);
  }
}

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`要素が見つかりません: ${id}`);
  return el as T;
}
