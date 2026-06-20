import { invoke } from "@tauri-apps/api/core";

interface Els {
  panel: HTMLElement;
  nickname: HTMLInputElement;
  talkStyle: HTMLInputElement;
  topics: HTMLInputElement;
  startBtn: HTMLButtonElement;
  skipBtn: HTMLButtonElement;
}

let els: Els | null = null;

/// 初回起動 (onboarded=false) のときのみ呼ぶ。パネルを表示してハンドラを張る。
export function showOnboarding(): void {
  els = collect();
  els.startBtn.addEventListener("click", () => void onStart());
  els.skipBtn.addEventListener("click", () => void onSkip());
  els.panel.classList.add("visible");
  setTimeout(() => els?.nickname.focus(), 0);
}

async function onStart(): Promise<void> {
  if (!els) return;
  const nickname = els.nickname.value.trim() || null;
  const talkStyle = els.talkStyle.value.trim() || null;
  const topicsEnabled = els.topics.checked;
  try {
    await invoke("complete_onboarding", {
      nickname,
      interests: [],
      talkStyle,
      topicsEnabled,
    });
  } catch (err) {
    console.error("complete_onboarding failed", err);
  }
  close();
}

async function onSkip(): Promise<void> {
  try {
    await invoke("skip_onboarding");
  } catch (err) {
    console.error("skip_onboarding failed", err);
  }
  close();
}

function close(): void {
  els?.panel.classList.remove("visible");
}

function collect(): Els {
  return {
    panel: byId("onboarding-panel"),
    nickname: byId<HTMLInputElement>("onboarding-nickname"),
    talkStyle: byId<HTMLInputElement>("onboarding-talk-style"),
    topics: byId<HTMLInputElement>("onboarding-topics"),
    startBtn: byId<HTMLButtonElement>("onboarding-start"),
    skipBtn: byId<HTMLButtonElement>("onboarding-skip"),
  };
}

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`${id} が見つかりません`);
  return el as T;
}
