import { invoke } from "@tauri-apps/api/core";

import { openReaderPanel } from "../panels/reader";
import { openSettingsPanel } from "../panels/settings";
import type { Settings } from "../types";

interface MenuItem {
  label: string;
  onClick: () => void | Promise<void>;
}

let menuEl: HTMLElement | null = null;
let getSettings: () => Settings | null = () => null;
let onModeToggle: (next: "low" | "advanced") => void = () => {};

export function mountContextMenu(opts: {
  current: () => Settings | null;
  onModeToggle: (next: "low" | "advanced") => void;
}): void {
  getSettings = opts.current;
  onModeToggle = opts.onModeToggle;
  menuEl = document.getElementById("context-menu");
  if (!menuEl) return;
  document.addEventListener("contextmenu", onContext);
  document.addEventListener("mousedown", (ev) => {
    if (!menuEl?.classList.contains("visible")) return;
    if (ev.target instanceof Node && menuEl.contains(ev.target)) return;
    hideMenu();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && menuEl?.classList.contains("visible")) {
      hideMenu();
    }
  });
}

function onContext(ev: MouseEvent): void {
  ev.preventDefault();
  showMenuAt(ev.clientX, ev.clientY);
}

function showMenuAt(x: number, y: number): void {
  if (!menuEl) return;
  const items = buildItems();
  menuEl.innerHTML = "";
  for (const item of items) {
    if ("divider" in item) {
      const sep = document.createElement("div");
      sep.className = "menu-divider";
      menuEl.appendChild(sep);
      continue;
    }
    const el = document.createElement("div");
    el.className = "menu-item";
    el.textContent = item.label;
    el.addEventListener("click", () => {
      hideMenu();
      void item.onClick();
    });
    menuEl.appendChild(el);
  }
  // ウインドウ端からはみ出さないよう位置補正
  menuEl.classList.add("visible");
  const w = menuEl.offsetWidth;
  const h = menuEl.offsetHeight;
  const left = Math.min(x, window.innerWidth - w - 8);
  const top = Math.min(y, window.innerHeight - h - 8);
  menuEl.style.left = `${Math.max(8, left)}px`;
  menuEl.style.top = `${Math.max(8, top)}px`;
}

export function hideMenu(): void {
  menuEl?.classList.remove("visible");
}

function buildItems(): Array<MenuItem | { divider: true }> {
  const settings = getSettings();
  const modeLabel = settings?.mode === "advanced"
    ? "モードを low に切替"
    : "モードを advanced に切替";
  const next = settings?.mode === "advanced" ? "low" : "advanced";
  const quietLabel = settings?.quiet_mode ? "静音モード解除" : "静音モード ON";
  return [
    {
      label: "このキャラと話す",
      onClick: () => {
        window.dispatchEvent(new CustomEvent("ugg-open-input"));
      },
    },
    {
      label: modeLabel,
      onClick: () => onModeToggle(next),
    },
    {
      label: quietLabel,
      onClick: () => window.dispatchEvent(new CustomEvent("ugg-toggle-quiet")),
    },
    { divider: true },
    {
      label: "ポモドーロ開始",
      onClick: () => invoke("start_pomodoro").catch((e) => console.error(e)),
    },
    {
      label: "ポモドーロ停止",
      onClick: () => invoke("stop_pomodoro").catch((e) => console.error(e)),
    },
    {
      label: "テキスト読み上げ",
      onClick: () => openReaderPanel(),
    },
    { divider: true },
    {
      label: "設定を開く",
      onClick: () => openSettingsPanel(),
    },
    {
      label: "ウインドウを隠す",
      onClick: () => window.dispatchEvent(new CustomEvent("ugg-hide-window")),
    },
    { divider: true },
    {
      label: "終了",
      onClick: async () => {
        try {
          await invoke("quit_app");
        } catch (err) {
          console.error("quit failed", err);
        }
      },
    },
  ];
}
