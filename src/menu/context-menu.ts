import { invoke } from "@tauri-apps/api/core";

import { balloonMenuContainer, reposition } from "../dialogue/balloon";
import { closeInput, isInputOpen } from "../dialogue/input";
import { openPomodoroPanel } from "../panels/pomodoro";
import { openReaderPanel } from "../panels/reader";
import { openSettingsPanel } from "../panels/settings";
import { hitTest } from "../stage/character";
import { cancelSpeech, renderMenuPrompt } from "../system/ghost-speech";
import type { Settings, SlotName, SpeechTurn } from "../types";

/// 右クリックメニュー (spec §4.3.5)。独立 UI ではなく **メインのバルーン内** に表示する。
/// - メイン右クリック: 前口上 (辞書 menu_prompt.main) → 同じバルーン内にメニュー項目
/// - サブ右クリック: 誘導セリフ (menu_prompt.sub) をサブの吹き出しに出した後、メインへ遷移
/// - キャラ以外の右クリック: 何もしない (既定メニューの抑止のみ)
/// - 閉じる: 項目実行 / Esc / バルーン外 mousedown / 新しい発話による置き換え

interface MenuItem {
  label: string;
  onClick: () => void | Promise<void>;
}

let getSettings: () => Settings | null = () => null;
let onModeToggle: (next: "low" | "advanced") => void = () => {};
let menuOpen = false;
/// 右クリック連打・sub→main 遷移中の追い越しを無効化する世代カウンタ。
let flowSeq = 0;

export function mountContextMenu(opts: {
  current: () => Settings | null;
  onModeToggle: (next: "low" | "advanced") => void;
}): void {
  getSettings = opts.current;
  onModeToggle = opts.onModeToggle;
  document.addEventListener("contextmenu", onContext);
  // バルーン外の mousedown で閉じる (項目クリックとバルーン内は素通り)。
  // キャラ上の mousedown はここでは閉じない: 即閉じると main.ts のクリックゲートが
  // メニュー表示中と認識できず、閉じた直後に入力導線が開いてしまう。
  // キャラ上のクリックはゲート側 (250ms 後) が「閉じるだけ」で処理する。
  document.addEventListener("mousedown", (ev) => {
    if (!menuOpen) return;
    const balloon = document.getElementById("balloon-main");
    if (ev.target instanceof Node && balloon?.contains(ev.target)) return;
    if (hitTest(ev.clientX, ev.clientY)) return;
    closeMenu();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && menuOpen) {
      closeMenu();
    }
  });
}

export function isMenuOpen(): boolean {
  // フラグ単独だと、メニュー表示中に発話 (自発トーク・つつき等) が割り込んで
  // showBalloon が .balloon-menu を空にしても menuOpen が true のまま残り、
  // 次の 1 クリックが main.ts のゲートで closeMenu に食われて空振りする (v0.1.4 監査)。
  // 実際に項目が残っているかで判定し、消えていればフラグも畳む。
  if (!menuOpen) return false;
  const menu = balloonMenuContainer("main");
  if (!menu || menu.childElementCount === 0) {
    menuOpen = false;
    return false;
  }
  return true;
}

/// メニューを閉じる: 項目を空にし、前口上の発話ごとバルーンを畳む。
export function closeMenu(): void {
  if (!menuOpen) return;
  menuOpen = false;
  flowSeq++;
  const menu = balloonMenuContainer("main");
  if (menu) menu.innerHTML = "";
  cancelSpeech();
}

function onContext(ev: MouseEvent): void {
  ev.preventDefault();
  const hit = hitTest(ev.clientX, ev.clientY);
  if (!hit) return;
  void openMenuFlow(hit.slot);
}

async function openMenuFlow(slot: SlotName): Promise<void> {
  if (menuOpen) closeMenu();
  if (isInputOpen()) closeInput();
  const seq = ++flowSeq;
  menuOpen = true;

  // サブ右クリックは誘導セリフを先に取り、メインの前口上→メニューへ遷移する
  const subTurn =
    slot === "sub"
      ? await invoke<SpeechTurn | null>("menu_prompt", { target: "sub" }).catch((err) => {
          console.error("menu_prompt(sub) failed", err);
          return null;
        })
      : null;
  const mainTurn = await invoke<SpeechTurn | null>("menu_prompt", { target: "main" }).catch(
    (err) => {
      console.error("menu_prompt(main) failed", err);
      return null;
    },
  );
  if (seq !== flowSeq || !menuOpen) return; // 閉じられた・別フローに追い越された

  const done = await renderMenuPrompt(subTurn, mainTurn);
  if (!done || seq !== flowSeq || !menuOpen) return;
  populateMenu();
}

function populateMenu(): void {
  const menu = balloonMenuContainer("main");
  if (!menu) return;
  menu.innerHTML = "";
  for (const item of buildItems()) {
    if ("divider" in item) {
      const sep = document.createElement("div");
      sep.className = "balloon-menu-divider";
      menu.appendChild(sep);
      continue;
    }
    const el = document.createElement("div");
    el.className = "balloon-menu-item";
    el.textContent = item.label;
    el.addEventListener("click", () => {
      closeMenu();
      void item.onClick();
    });
    menu.appendChild(el);
  }
  // 項目追加でバルーンが育つので配置し直す
  reposition("main");
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
      label: modeLabel,
      onClick: () => onModeToggle(next),
    },
    {
      label: quietLabel,
      onClick: () => window.dispatchEvent(new CustomEvent("ugg-toggle-quiet")),
    },
    { divider: true },
    {
      label: "ポモドーロタイマー",
      onClick: () => openPomodoroPanel(),
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
