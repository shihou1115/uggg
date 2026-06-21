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
  ttsEnabled: HTMLInputElement;
  ttsSpeakerMain: HTMLSelectElement;
  ttsSpeakerSub: HTMLSelectElement;
  ttsSpeed: HTMLInputElement;
  ttsVolume: HTMLInputElement;
  ttsAssetsState: HTMLElement;
  ttsDownloadBtn: HTMLButtonElement;
  ghTokenState: HTMLElement;
  ghTokenInput: HTMLInputElement;
  ghTokenDeleteBtn: HTMLButtonElement;
  ttsProgress: HTMLElement;
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
  await refreshTtsState();
  inputs.panel.classList.add("visible");
  inputs.msg.hidden = true;
  inputs.ttsProgress.hidden = true;
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
    ttsEnabled: byId<HTMLInputElement>("settings-tts-enabled"),
    ttsSpeakerMain: byId<HTMLSelectElement>("settings-tts-speaker-main"),
    ttsSpeakerSub: byId<HTMLSelectElement>("settings-tts-speaker-sub"),
    ttsSpeed: byId<HTMLInputElement>("settings-tts-speed"),
    ttsVolume: byId<HTMLInputElement>("settings-tts-volume"),
    ttsAssetsState: byId("settings-tts-assets-state"),
    ttsDownloadBtn: byId<HTMLButtonElement>("settings-tts-download"),
    ghTokenState: byId("settings-gh-token-state"),
    ghTokenInput: byId<HTMLInputElement>("settings-gh-token"),
    ghTokenDeleteBtn: byId<HTMLButtonElement>("settings-gh-token-delete"),
    ttsProgress: byId("settings-tts-progress"),
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
  i.ttsDownloadBtn.addEventListener("click", () => void onTtsDownload());
  i.ghTokenDeleteBtn.addEventListener("click", () => void onDeleteGhToken());
  i.panel.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeSettingsPanel();
    }
  });
}

async function refreshTtsState(): Promise<void> {
  if (!inputs) return;
  try {
    const ready = await invoke<boolean>("voicevox_assets_ready");
    inputs.ttsAssetsState.textContent = ready ? "ダウンロード済み" : "未ダウンロード";
    inputs.ttsAssetsState.classList.toggle("has-key", ready);
    if (ready) {
      // 話者一覧を取得して select に流し込む
      const voices = await invoke<Array<{ id: number; name: string }>>("list_voices");
      const currentMain = inputs.ttsSpeakerMain.value || (current?.tts_speaker_main ?? 2).toString();
      const currentSub = inputs.ttsSpeakerSub.value || (current?.tts_speaker_sub ?? 3).toString();
      fillSpeakerSelect(inputs.ttsSpeakerMain, voices, currentMain);
      fillSpeakerSelect(inputs.ttsSpeakerSub, voices, currentSub);
    }
  } catch (err) {
    console.warn("voicevox_assets_ready/list_voices failed", err);
  }
  try {
    const hasToken = await invoke<boolean>("has_github_token");
    inputs.ghTokenState.textContent = hasToken ? "保存済み" : "未保存";
    inputs.ghTokenState.classList.toggle("has-key", hasToken);
  } catch (err) {
    console.warn("has_github_token failed", err);
  }
}

function fillSpeakerSelect(
  select: HTMLSelectElement,
  voices: Array<{ id: number; name: string }>,
  selectedId: string,
): void {
  select.innerHTML = "";
  for (const v of voices) {
    const opt = document.createElement("option");
    opt.value = String(v.id);
    opt.textContent = `${v.name} (#${v.id})`;
    select.appendChild(opt);
  }
  if (!voices.some((v) => String(v.id) === selectedId)) {
    const opt = document.createElement("option");
    opt.value = selectedId;
    opt.textContent = `#${selectedId} (現在の設定)`;
    select.insertBefore(opt, select.firstChild);
  }
  select.value = selectedId;
}

async function onTtsDownload(): Promise<void> {
  if (!inputs) return;
  // 規約同意ダイアログ。シンプルに confirm で済ませる (パネル既出のリンクで規約は提示済み)。
  const ok = window.confirm(
    "VOICEVOX 音声モデルおよび ONNX Runtime のライセンス・規約に同意してダウンロードしますか?\n（数百 MB の通信が発生します）",
  );
  if (!ok) return;
  showProgress("ダウンローダ取得中…", false);
  inputs.ttsDownloadBtn.disabled = true;

  // 進捗 listen を貼る (毎回貼り直して done で外す)
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<string>("voicevox-download", (ev) => {
    if (ev.payload === "__done__") {
      return;
    }
    showProgress(ev.payload, false);
  });

  try {
    await invoke("download_voicevox_assets", { agreed: true, ghToken: null });
    showProgress("ダウンロード完了。話者リストを更新します…", false);
    await refreshTtsState();
    showProgress("完了しました。", false);
  } catch (err) {
    showProgress(`ダウンロード失敗: ${formatErr(err)}`, true);
  } finally {
    unlisten();
    inputs.ttsDownloadBtn.disabled = false;
  }
}

async function onDeleteGhToken(): Promise<void> {
  if (!inputs) return;
  try {
    await invoke("delete_github_token");
    await refreshTtsState();
    showMessage("GitHub PAT を削除しました", false);
  } catch (err) {
    showMessage(`PAT 削除失敗: ${formatErr(err)}`, true);
  }
}

function showProgress(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.ttsProgress.textContent = msg;
  inputs.ttsProgress.classList.toggle("error", isError);
  inputs.ttsProgress.hidden = false;
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
  inputs.ttsEnabled.checked = s.tts_enabled;
  inputs.ttsSpeed.value = String(s.tts_speed);
  inputs.ttsVolume.value = String(s.tts_volume);
  // 話者 select は資産 DL 済みのときだけ list_voices で埋められる。値は文字列で保持。
  ensureSpeakerSelection(inputs.ttsSpeakerMain, s.tts_speaker_main);
  ensureSpeakerSelection(inputs.ttsSpeakerSub, s.tts_speaker_sub);
}

/// 話者 select に id が無ければ「#<id> (未取得)」項目を作って current を保つ。
function ensureSpeakerSelection(select: HTMLSelectElement, id: number): void {
  const value = String(id);
  if (Array.from(select.options).some((o) => o.value === value)) {
    select.value = value;
    return;
  }
  const opt = document.createElement("option");
  opt.value = value;
  opt.textContent = `#${id} (未取得)`;
  select.appendChild(opt);
  select.value = value;
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
    tts_enabled: inputs.ttsEnabled.checked,
    tts_speaker_main: Number(inputs.ttsSpeakerMain.value) || current.tts_speaker_main,
    tts_speaker_sub: Number(inputs.ttsSpeakerSub.value) || current.tts_speaker_sub,
    tts_speed: Number(inputs.ttsSpeed.value) || current.tts_speed,
    tts_volume: Number(inputs.ttsVolume.value) || current.tts_volume,
  };

  // GitHub PAT 入力があれば先に keyring へ
  const ghToken = inputs.ghTokenInput.value;
  if (ghToken.trim()) {
    try {
      await invoke("set_github_token", { token: ghToken });
      inputs.ghTokenInput.value = "";
    } catch (err) {
      showMessage(`PAT 保存失敗: ${formatErr(err)}`, true);
      return;
    }
  }

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
