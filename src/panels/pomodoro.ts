import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { Settings } from "../types";

export interface PomodoroStatus {
  phase: "idle" | "focus" | "break";
  remaining_sec: number;
  round: number;
  rounds: number;
  /// 一時停止中か (spec §4.4.5)。
  paused: boolean;
}

// ===== バッジ (進行状況の常時表示) =====
let badge: HTMLElement | null = null;
let phaseEl: HTMLElement | null = null;
let timeEl: HTMLElement | null = null;
let roundEl: HTMLElement | null = null;

// ===== 操作パネル (spec §4.4.5) =====
interface PanelRefs {
  root: HTMLElement;
  status: HTMLElement;
  work: HTMLInputElement;
  brk: HTMLInputElement;
  rounds: HTMLInputElement;
  startBtn: HTMLButtonElement;
  pauseBtn: HTMLButtonElement;
  abortBtn: HTMLButtonElement;
}
let panel: PanelRefs | null = null;
let getSettings: () => Settings | null = () => null;
let persistSettings: (p: PomodoroSettingsPatch) => Promise<void> = async () => {};

interface PomodoroSettingsPatch {
  pomodoro_work_min: number;
  pomodoro_break_min: number;
  pomodoro_rounds: number;
}

/// 直近の状態。パネルを開いた瞬間のボタン状態決定と、pause/resume のトグル判定に使う。
let lastStatus: PomodoroStatus = {
  phase: "idle",
  remaining_sec: 0,
  round: 0,
  rounds: 0,
  paused: false,
};

export async function mountPomodoroBadge(): Promise<void> {
  badge = document.getElementById("pomodoro-badge");
  phaseEl = document.getElementById("pomodoro-badge-phase");
  timeEl = document.getElementById("pomodoro-badge-time");
  roundEl = document.getElementById("pomodoro-badge-round");
  if (badge) {
    // バッジクリックで操作パネルを開く (旧: 即停止 → パネル導線に統一)。
    badge.addEventListener("click", () => openPomodoroPanel());
  }

  await listen<PomodoroStatus>("pomodoro", (ev) => apply(ev.payload));
  try {
    apply(await invoke<PomodoroStatus>("get_pomodoro_status"));
  } catch (err) {
    console.error("get_pomodoro_status failed", err);
  }
}

export function mountPomodoroPanel(opts: {
  current: () => Settings | null;
  persist: (p: PomodoroSettingsPatch) => Promise<void>;
}): void {
  getSettings = opts.current;
  persistSettings = opts.persist;
  const root = document.getElementById("pomodoro-panel");
  if (!root) return;
  const status = document.getElementById("pomodoro-panel-status");
  const work = document.getElementById("pomodoro-work") as HTMLInputElement | null;
  const brk = document.getElementById("pomodoro-break") as HTMLInputElement | null;
  const rounds = document.getElementById("pomodoro-rounds") as HTMLInputElement | null;
  const startBtn = document.getElementById("pomodoro-start") as HTMLButtonElement | null;
  const pauseBtn = document.getElementById("pomodoro-pause") as HTMLButtonElement | null;
  const abortBtn = document.getElementById("pomodoro-abort") as HTMLButtonElement | null;
  const closeBtn = document.getElementById("pomodoro-close") as HTMLButtonElement | null;
  if (!status || !work || !brk || !rounds || !startBtn || !pauseBtn || !abortBtn || !closeBtn) {
    return;
  }
  panel = { root, status, work, brk, rounds, startBtn, pauseBtn, abortBtn };

  startBtn.addEventListener("click", () => void onStart());
  pauseBtn.addEventListener("click", () => void onPauseToggle());
  abortBtn.addEventListener("click", () => {
    void invoke("stop_pomodoro").catch((e) => console.error("stop_pomodoro failed", e));
  });
  closeBtn.addEventListener("click", () => closePomodoroPanel());
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && root.classList.contains("visible")) {
      closePomodoroPanel();
    }
  });
}

export function openPomodoroPanel(): void {
  if (!panel) return;
  // idle のときだけ現在の既定設定を入力欄へ流し込む (進行中は編集させないため触らない)。
  if (lastStatus.phase === "idle") {
    const s = getSettings();
    if (s) {
      panel.work.value = String(s.pomodoro_work_min);
      panel.brk.value = String(s.pomodoro_break_min);
      panel.rounds.value = String(s.pomodoro_rounds);
    }
  }
  panel.root.classList.add("visible");
  applyPanel(lastStatus);
}

export function closePomodoroPanel(): void {
  panel?.root.classList.remove("visible");
}

/// 「開始」: 入力値を既定設定に保存してからタイマー開始 (start は settings を読む)。
async function onStart(): Promise<void> {
  if (!panel) return;
  const patch: PomodoroSettingsPatch = {
    pomodoro_work_min: clampInt(panel.work.value, 1, 180, 25),
    pomodoro_break_min: clampInt(panel.brk.value, 1, 60, 5),
    pomodoro_rounds: clampInt(panel.rounds.value, 1, 20, 4),
  };
  try {
    await persistSettings(patch);
    await invoke("start_pomodoro");
  } catch (err) {
    console.error("start_pomodoro failed", err);
  }
}

/// 「停止」ボタン: 一時停止中なら再開、そうでなければ一時停止。
async function onPauseToggle(): Promise<void> {
  const cmd = lastStatus.paused ? "resume_pomodoro" : "pause_pomodoro";
  try {
    await invoke(cmd);
  } catch (err) {
    console.error(`${cmd} failed`, err);
  }
}

function clampInt(v: string, min: number, max: number, fallback: number): number {
  const n = Math.round(Number(v));
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

function apply(status: PomodoroStatus): void {
  lastStatus = status;
  applyBadge(status);
  applyPanel(status);
}

function applyBadge(status: PomodoroStatus): void {
  if (!badge || !phaseEl || !timeEl || !roundEl) return;
  if (status.phase === "idle") {
    badge.classList.remove("visible", "focus", "break");
    return;
  }
  badge.classList.add("visible");
  badge.classList.toggle("focus", status.phase === "focus");
  badge.classList.toggle("break", status.phase === "break");
  phaseEl.textContent = status.paused ? `${status.phase} ⏸` : status.phase;
  timeEl.textContent = formatMMSS(status.remaining_sec);
  roundEl.textContent = `${status.round}/${status.rounds}`;
}

function applyPanel(status: PomodoroStatus): void {
  if (!panel) return;
  const running = status.phase !== "idle";
  if (!running) {
    panel.status.textContent = "停止中";
  } else {
    const ph = status.phase === "focus" ? "集中" : "休憩";
    const pausedTxt = status.paused ? "（一時停止中）" : "";
    panel.status.textContent =
      `${ph} ${formatMMSS(status.remaining_sec)}　ラウンド ${status.round}/${status.rounds}${pausedTxt}`;
  }
  panel.startBtn.disabled = running;
  panel.pauseBtn.disabled = !running;
  panel.abortBtn.disabled = !running;
  panel.pauseBtn.textContent = status.paused ? "再開" : "停止";
  // 時間設定は idle 時のみ編集可 (進行中は次の開始時反映のため触らせない)。
  panel.work.disabled = running;
  panel.brk.disabled = running;
  panel.rounds.disabled = running;
}

function formatMMSS(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  const m = Math.floor(s / 60);
  const ss = s % 60;
  return `${m.toString().padStart(2, "0")}:${ss.toString().padStart(2, "0")}`;
}
