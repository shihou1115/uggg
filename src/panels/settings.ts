import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { DialogueMode, Settings, TalkSpeed } from "../types";

interface Inputs {
  panel: HTMLElement;
  mode: HTMLSelectElement;
  provider: HTMLInputElement;
  model: HTMLInputElement;
  baseUrl: HTMLInputElement;
  costLimit: HTMLInputElement;
  keyState: HTMLElement;
  keyInput: HTMLInputElement;
  keyDelete: HTMLButtonElement;
  displayScale: HTMLInputElement;
  talkSpeed: HTMLSelectElement;
  quietMode: HTMLInputElement;
  autoQuietFullscreen: HTMLInputElement;
  monologueInterval: HTMLInputElement;
  pomoWork: HTMLInputElement;
  pomoBreak: HTMLInputElement;
  pomoRounds: HTMLInputElement;
  saveBtn: HTMLButtonElement;
  cancelBtn: HTMLButtonElement;
  closeBtn: HTMLButtonElement;
  msg: HTMLElement;
}

let inputs: Inputs | null = null;
let current: Settings | null = null;
let onSaved: ((s: Settings) => void) | null = null;

export async function mountSettingsPanel(): Promise<void> {
  inputs = collectInputs();
  attachHandlers(inputs);
  // 外部 (トレイ・notify) からの設定変更を反映するため
  await listen<Settings>("settings-changed", (ev) => {
    current = ev.payload;
    if (isOpen()) applySettingsToForm(ev.payload);
  });
  // トレイの「設定を開く」からの通知
  await listen("open-settings", () => {
    void openSettingsPanel();
  });
}

export function registerSavedListener(cb: (s: Settings) => void): void {
  onSaved = cb;
}

export async function openSettingsPanel(): Promise<void> {
  if (!inputs) return;
  if (!current) {
    current = await invoke<Settings>("get_settings");
  }
  applySettingsToForm(current);
  await refreshKeyState(current.llm_provider);
  inputs.panel.classList.add("visible");
  inputs.msg.hidden = true;
  inputs.keyInput.value = "";
  setTimeout(() => inputs?.mode.focus(), 0);
}

export function closeSettingsPanel(): void {
  inputs?.panel.classList.remove("visible");
}

export function isOpen(): boolean {
  return !!inputs?.panel.classList.contains("visible");
}

function collectInputs(): Inputs {
  return {
    panel: byId("settings-panel"),
    mode: byId<HTMLSelectElement>("settings-mode"),
    provider: byId<HTMLInputElement>("settings-llm-provider"),
    model: byId<HTMLInputElement>("settings-llm-model"),
    baseUrl: byId<HTMLInputElement>("settings-llm-base-url"),
    costLimit: byId<HTMLInputElement>("settings-cost-limit"),
    keyState: byId("settings-key-state"),
    keyInput: byId<HTMLInputElement>("settings-llm-key"),
    keyDelete: byId<HTMLButtonElement>("settings-key-delete"),
    displayScale: byId<HTMLInputElement>("settings-display-scale"),
    talkSpeed: byId<HTMLSelectElement>("settings-talk-speed"),
    quietMode: byId<HTMLInputElement>("settings-quiet-mode"),
    autoQuietFullscreen: byId<HTMLInputElement>("settings-auto-quiet-fullscreen"),
    monologueInterval: byId<HTMLInputElement>("settings-monologue-interval"),
    pomoWork: byId<HTMLInputElement>("settings-pomodoro-work"),
    pomoBreak: byId<HTMLInputElement>("settings-pomodoro-break"),
    pomoRounds: byId<HTMLInputElement>("settings-pomodoro-rounds"),
    saveBtn: byId<HTMLButtonElement>("settings-save"),
    cancelBtn: byId<HTMLButtonElement>("settings-cancel"),
    closeBtn: byId<HTMLButtonElement>("settings-close"),
    msg: byId("settings-message"),
  };
}

function attachHandlers(i: Inputs): void {
  i.closeBtn.addEventListener("click", () => closeSettingsPanel());
  i.cancelBtn.addEventListener("click", () => closeSettingsPanel());
  i.saveBtn.addEventListener("click", () => void onSave());
  i.keyDelete.addEventListener("click", () => void onDeleteKey());
  i.provider.addEventListener("change", () => {
    void refreshKeyState(i.provider.value);
  });
  i.panel.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeSettingsPanel();
    }
  });
}

function applySettingsToForm(s: Settings): void {
  if (!inputs) return;
  inputs.mode.value = s.mode;
  inputs.provider.value = s.llm_provider;
  inputs.model.value = s.llm_model;
  inputs.baseUrl.value = s.llm_base_url ?? "";
  inputs.costLimit.value = String(s.monthly_limit_usd);
  inputs.displayScale.value = String(s.display_scale);
  inputs.talkSpeed.value = s.talk_speed;
  inputs.quietMode.checked = s.quiet_mode;
  inputs.autoQuietFullscreen.checked = s.auto_quiet_fullscreen;
  inputs.monologueInterval.value = String(s.monologue_interval_min);
  inputs.pomoWork.value = String(s.pomodoro_work_min);
  inputs.pomoBreak.value = String(s.pomodoro_break_min);
  inputs.pomoRounds.value = String(s.pomodoro_rounds);
}

async function refreshKeyState(provider: string): Promise<void> {
  if (!inputs) return;
  if (!provider.trim()) {
    inputs.keyState.textContent = "—";
    inputs.keyState.classList.remove("has-key");
    return;
  }
  try {
    const has = await invoke<boolean>("has_api_key", { provider: provider.trim() });
    inputs.keyState.textContent = has ? "保存済み" : "未保存";
    inputs.keyState.classList.toggle("has-key", has);
  } catch (err) {
    inputs.keyState.textContent = "確認失敗";
    inputs.keyState.classList.remove("has-key");
    console.error("has_api_key failed", err);
  }
}

async function onSave(): Promise<void> {
  if (!inputs || !current) return;
  hideMessage();

  const next: Settings = {
    ...current,
    mode: inputs.mode.value as DialogueMode,
    llm_provider: inputs.provider.value.trim() || current.llm_provider,
    llm_model: inputs.model.value.trim() || current.llm_model,
    llm_base_url: inputs.baseUrl.value.trim() || null,
    monthly_limit_usd: Number(inputs.costLimit.value) || 0,
    display_scale: Number(inputs.displayScale.value) || 1.0,
    talk_speed: inputs.talkSpeed.value as TalkSpeed,
    quiet_mode: inputs.quietMode.checked,
    auto_quiet_fullscreen: inputs.autoQuietFullscreen.checked,
    monologue_interval_min: Number(inputs.monologueInterval.value) || 0,
    pomodoro_work_min: Number(inputs.pomoWork.value) || current.pomodoro_work_min,
    pomodoro_break_min: Number(inputs.pomoBreak.value) || current.pomodoro_break_min,
    pomodoro_rounds: Number(inputs.pomoRounds.value) || current.pomodoro_rounds,
  };

  // API キー入力があれば先に保存 (settings 保存より前)
  const keyVal = inputs.keyInput.value;
  if (keyVal.trim()) {
    try {
      await invoke("set_api_key", { provider: next.llm_provider, key: keyVal });
      inputs.keyInput.value = "";
    } catch (err) {
      showMessage(`API キー保存失敗: ${formatErr(err)}`, true);
      return;
    }
  }

  try {
    const saved = await invoke<Settings>("set_settings", { settings: next });
    current = saved;
    applySettingsToForm(saved);
    await refreshKeyState(saved.llm_provider);
    onSaved?.(saved);
    showMessage("保存しました", false);
    setTimeout(() => closeSettingsPanel(), 600);
  } catch (err) {
    showMessage(`設定保存失敗: ${formatErr(err)}`, true);
  }
}

async function onDeleteKey(): Promise<void> {
  if (!inputs) return;
  const provider = inputs.provider.value.trim();
  if (!provider) return;
  try {
    await invoke("delete_api_key", { provider });
    await refreshKeyState(provider);
    showMessage("API キーを削除しました", false);
  } catch (err) {
    showMessage(`削除失敗: ${formatErr(err)}`, true);
  }
}

function showMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.msg.textContent = msg;
  inputs.msg.classList.toggle("error", isError);
  inputs.msg.hidden = false;
}

function hideMessage(): void {
  if (!inputs) return;
  inputs.msg.hidden = true;
}

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`${id} が見つかりません`);
  return el as T;
}

function formatErr(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
