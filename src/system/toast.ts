//! システムトースト表示 (M7、daily-support-design §3.2)。
//!
//! バックエンドの `system-toast` イベント (通知配達のフォールバック経路・notify() の
//! 辞書未定義フォールバック) を受けて短時間の帯を表示する。これまでイベントは
//! 発火されるだけで受け手が無かったが、M7 の到達保証 (Toast = 到達扱い) の成立には
//! ユーザーに見える受け皿が必要になった。
//! WebView2 透過バグ対策で #system-toast は index.html に静的配置し、.visible でトグル。

import { listen } from "@tauri-apps/api/event";

const SHOW_MS = 6000;

let el: HTMLElement | null = null;
let hideTimer: number | null = null;

export async function mountSystemToast(): Promise<void> {
  el = document.getElementById("system-toast");
  if (!el) return;
  el.addEventListener("click", () => hide());
  await listen<string>("system-toast", (ev) => show(ev.payload));
}

function show(text: string): void {
  if (!el || !text) return;
  el.textContent = text;
  el.classList.add("visible");
  if (hideTimer !== null) window.clearTimeout(hideTimer);
  hideTimer = window.setTimeout(() => hide(), SHOW_MS);
}

function hide(): void {
  if (hideTimer !== null) {
    window.clearTimeout(hideTimer);
    hideTimer = null;
  }
  el?.classList.remove("visible");
}
