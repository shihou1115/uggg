import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface PomodoroStatus {
  phase: "idle" | "focus" | "break";
  remaining_sec: number;
  round: number;
  rounds: number;
}

let badge: HTMLElement | null = null;
let phaseEl: HTMLElement | null = null;
let timeEl: HTMLElement | null = null;
let roundEl: HTMLElement | null = null;

export async function mountPomodoroBadge(): Promise<void> {
  badge = document.getElementById("pomodoro-badge");
  phaseEl = document.getElementById("pomodoro-badge-phase");
  timeEl = document.getElementById("pomodoro-badge-time");
  roundEl = document.getElementById("pomodoro-badge-round");
  if (!badge || !phaseEl || !timeEl || !roundEl) return;

  badge.addEventListener("click", () => {
    void invoke("stop_pomodoro").catch((err) => console.error(err));
  });

  await listen<PomodoroStatus>("pomodoro", (ev) => apply(ev.payload));
  // 初期状態取得 (起動後に既に進行中のケースは無いが、HMR/再ロード時の同期用)
  try {
    apply(await invoke<PomodoroStatus>("get_pomodoro_status"));
  } catch (err) {
    console.error("get_pomodoro_status failed", err);
  }
}

function apply(status: PomodoroStatus): void {
  if (!badge || !phaseEl || !timeEl || !roundEl) return;
  if (status.phase === "idle") {
    badge.classList.remove("visible", "focus", "break");
    return;
  }
  badge.classList.add("visible");
  badge.classList.toggle("focus", status.phase === "focus");
  badge.classList.toggle("break", status.phase === "break");
  phaseEl.textContent = status.phase;
  timeEl.textContent = formatMMSS(status.remaining_sec);
  roundEl.textContent = `${status.round}/${status.rounds}`;
}

function formatMMSS(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  const m = Math.floor(s / 60);
  const ss = s % 60;
  return `${m.toString().padStart(2, "0")}:${ss.toString().padStart(2, "0")}`;
}
