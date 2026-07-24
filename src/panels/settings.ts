import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { openChatLog } from "./chatlog";
import { uggConfirm } from "../confirm";
import { previewWavBase64 } from "../tts/speaker";
import { isWeatherReady } from "../weather/credit";
import type {
  AssetEntry,
  CalendarSource,
  ClearResult,
  DailyWeather,
  DialogueMode,
  DndResult,
  InterestTopic,
  IrodoriGpuInfo,
  LocationHit,
  Settings,
  SlotName,
  TalkSpeed,
  VoiceRef,
  WeatherSnapshot,
} from "../types";

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
  ttsEngine: HTMLSelectElement;
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
  // M4c Phase F: Irodori-TTS セクション
  irodoriGpuState: HTMLElement;
  irodoriAssetsState: HTMLElement;
  irodoriDownloadBtn: HTMLButtonElement;
  irodoriUseRealModel: HTMLInputElement;
  irodoriProgress: HTMLElement;
  voiceRefMainState: HTMLElement;
  voiceRefMainCaption: HTMLInputElement;
  voiceRefMainGenerate: HTMLButtonElement;
  voiceRefMainPreview: HTMLButtonElement;
  voiceRefMainDelete: HTMLButtonElement;
  voiceRefSubState: HTMLElement;
  voiceRefSubCaption: HTMLInputElement;
  voiceRefSubGenerate: HTMLButtonElement;
  voiceRefSubPreview: HTMLButtonElement;
  voiceRefSubDelete: HTMLButtonElement;
  voiceRefProgress: HTMLElement;
  // M5: キャラクター / OS / 更新通知 / データ管理 / 興味分野
  ghostId: HTMLSelectElement;
  shellId: HTMLSelectElement;
  autostart: HTMLInputElement;
  updateFeedUrl: HTMLInputElement;
  updateCheckBtn: HTMLButtonElement;
  updateMessage: HTMLElement;
  openChatlogBtn: HTMLButtonElement;
  dataIncludeProfile: HTMLInputElement;
  dataExportBtn: HTMLButtonElement;
  dataClearBtn: HTMLButtonElement;
  dataMessage: HTMLElement;
  topicsEnabled: HTMLInputElement;
  topicsInterests: HTMLInputElement;
  topicsFetchBtn: HTMLButtonElement;
  topicsMessage: HTMLElement;
  toolsEnabled: HTMLInputElement;
  // M7: 日常支援 (リマインダー / 夜間静音)
  dailySupport: HTMLInputElement;
  reminderNotify: HTMLInputElement;
  nightQuiet: HTMLInputElement;
  nightQuietFrom: HTMLInputElement;
  nightQuietTo: HTMLInputElement;
  // M9: 状況発話 (ガバナンス)
  situationBreak: HTMLInputElement;
  situationLateNight: HTMLInputElement;
  situationBattery: HTMLInputElement;
  todoFollow: HTMLInputElement;
  minSpeakInterval: HTMLInputElement;
  // M10: カレンダー
  calList: HTMLElement;
  calUrl: HTMLInputElement;
  calAddUrl: HTMLButtonElement;
  calPath: HTMLInputElement;
  calAddPath: HTMLButtonElement;
  calNotifyMin: HTMLInputElement;
  calRefresh: HTMLButtonElement;
  calMessage: HTMLElement;
  // M11: 天気
  weatherPlaceState: HTMLElement;
  weatherSearchInput: HTMLInputElement;
  weatherSearchBtn: HTMLButtonElement;
  weatherCandidates: HTMLElement;
  weatherRefreshBtn: HTMLButtonElement;
  weatherClearBtn: HTMLButtonElement;
  weatherPreview: HTMLElement;
  situationRain: HTMLInputElement;
  weatherRainHint: HTMLElement;
  weatherMessage: HTMLElement;
  saveBtn: HTMLButtonElement;
  cancelBtn: HTMLButtonElement;
  closeBtn: HTMLButtonElement;
  msg: HTMLElement;
  // 2026-06-28: ページ切替 (M6 前の設定パネル整理)
  pageSelector: HTMLSelectElement;
  panelBody: HTMLElement;
}

let inputs: Inputs | null = null;
let current: Settings | null = null;
let onSaved: ((s: Settings) => void) | null = null;
// Irodori 資産が導入済みか。未導入のうちは、エンジンが voicevox でも Irodori セクション
// (ランタイム DL 導線) を表示し続ける必要がある (クリーンインストールでの詰み防止)。
// refreshIrodoriState が更新し、updateVoiceEngineVisibility が参照する。
let irodoriAssetsReady = false;

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
  // M5-A: DnD インストール完了の通知 (window 経由) を受けて、パネルを開いて結果表示
  window.addEventListener("ugg-dnd-result", (ev) => {
    const detail = (ev as CustomEvent<DndResult>).detail;
    void onDndResult(detail);
  });
  // M4c Phase G: サイドカー起動時の HF モデル DL 進捗 (`[hf-download] ...` 行) は
  // `download_irodori_assets` の中ではなく、`synthesize_voice` / `voice_ref_generate`
  // から実モデル初回起動時に流れてくる。`onIrodoriDownload` の一時 listener では
  // 拾えないため、ページ寿命の長期 listener を別途貼って Irodori 進捗欄に流す。
  await listen<string>("irodori-download", (ev) => {
    if (ev.payload === "__done__") return;
    if (!ev.payload.startsWith("[hf-download]")) return;
    showIrodoriProgress(ev.payload, false);
  });
}

async function onDndResult(result: DndResult): Promise<void> {
  const parts: string[] = [];
  if (result.installed.length > 0) parts.push(`導入 ${result.installed.length} 件`);
  if (result.conflicts.length > 0) parts.push(`未上書き ${result.conflicts.length} 件`);
  if (result.errors.length > 0) parts.push(`エラー ${result.errors.length} 件`);
  const isError = result.errors.length > 0 && result.installed.length === 0;
  const summary =
    parts.length > 0
      ? `DnD 結果: ${parts.join(" / ")}${
          result.installed.length > 0
            ? "。アプリを再起動すると反映されます"
            : ""
        }`
      : "DnD: 何も処理されませんでした";
  if (!isOpen()) {
    await openSettingsPanel();
  }
  showMessage(summary, isError);
  // 一覧 select を更新
  if (current) await refreshAssetLists(current);
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
  await refreshIrodoriState();
  await refreshAssetLists(current);
  await refreshInterests();
  inputs.panel.classList.add("visible");
  inputs.msg.hidden = true;
  inputs.ttsProgress.hidden = true;
  inputs.irodoriProgress.hidden = true;
  inputs.voiceRefProgress.hidden = true;
  inputs.dataMessage.hidden = true;
  inputs.updateMessage.hidden = true;
  inputs.topicsMessage.hidden = true;
  inputs.weatherMessage.hidden = true;
  inputs.weatherPreview.hidden = true;
  inputs.weatherCandidates.innerHTML = "";
  inputs.weatherSearchInput.value = "";
  inputs.keyInput.value = "";
  setTimeout(() => inputs?.mode.focus(), 0);
}

export function closeSettingsPanel(): void {
  inputs?.panel.classList.remove("visible");
}

export function isOpen(): boolean {
  return !!inputs?.panel.classList.contains("visible");
}

/// 設定パネルのページ切替 (2026-06-28 追加、M6 前の整理)。
/// panel-body の `data-active-page` を書き換えるだけで CSS が section 表示を制御する。
function setActivePage(page: string): void {
  if (!inputs) return;
  inputs.panelBody.dataset.activePage = page;
  // select 側もプログラム的な呼び出しで同期させる (openSettingsPanel 起点等)
  if (inputs.pageSelector.value !== page) {
    inputs.pageSelector.value = page;
  }
  // スクロール位置をリセット (別ページを開いた時に以前のスクロールが残らないよう)
  inputs.panelBody.scrollTop = 0;
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
    ttsEngine: byId<HTMLSelectElement>("settings-tts-engine"),
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
    irodoriGpuState: byId("settings-irodori-gpu-state"),
    irodoriAssetsState: byId("settings-irodori-assets-state"),
    irodoriDownloadBtn: byId<HTMLButtonElement>("settings-irodori-download"),
    irodoriUseRealModel: byId<HTMLInputElement>("settings-irodori-use-real-model"),
    irodoriProgress: byId("settings-irodori-progress"),
    voiceRefMainState: byId("settings-voiceref-main-state"),
    voiceRefMainCaption: byId<HTMLInputElement>("settings-voiceref-main-caption"),
    voiceRefMainGenerate: byId<HTMLButtonElement>("settings-voiceref-main-generate"),
    voiceRefMainPreview: byId<HTMLButtonElement>("settings-voiceref-main-preview"),
    voiceRefMainDelete: byId<HTMLButtonElement>("settings-voiceref-main-delete"),
    voiceRefSubState: byId("settings-voiceref-sub-state"),
    voiceRefSubCaption: byId<HTMLInputElement>("settings-voiceref-sub-caption"),
    voiceRefSubGenerate: byId<HTMLButtonElement>("settings-voiceref-sub-generate"),
    voiceRefSubPreview: byId<HTMLButtonElement>("settings-voiceref-sub-preview"),
    voiceRefSubDelete: byId<HTMLButtonElement>("settings-voiceref-sub-delete"),
    voiceRefProgress: byId("settings-voiceref-progress"),
    ghostId: byId<HTMLSelectElement>("settings-ghost-id"),
    shellId: byId<HTMLSelectElement>("settings-shell-id"),
    autostart: byId<HTMLInputElement>("settings-autostart"),
    updateFeedUrl: byId<HTMLInputElement>("settings-update-feed-url"),
    updateCheckBtn: byId<HTMLButtonElement>("settings-update-check"),
    updateMessage: byId("settings-update-message"),
    openChatlogBtn: byId<HTMLButtonElement>("settings-open-chatlog"),
    dataIncludeProfile: byId<HTMLInputElement>("settings-data-include-profile"),
    dataExportBtn: byId<HTMLButtonElement>("settings-data-export"),
    dataClearBtn: byId<HTMLButtonElement>("settings-data-clear"),
    dataMessage: byId("settings-data-message"),
    topicsEnabled: byId<HTMLInputElement>("settings-topics-enabled"),
    topicsInterests: byId<HTMLInputElement>("settings-topics-interests"),
    topicsFetchBtn: byId<HTMLButtonElement>("settings-topics-fetch"),
    topicsMessage: byId("settings-topics-message"),
    toolsEnabled: byId<HTMLInputElement>("settings-tools-enabled"),
    dailySupport: byId<HTMLInputElement>("settings-daily-support"),
    reminderNotify: byId<HTMLInputElement>("settings-reminder-notify"),
    nightQuiet: byId<HTMLInputElement>("settings-night-quiet"),
    nightQuietFrom: byId<HTMLInputElement>("settings-night-quiet-from"),
    nightQuietTo: byId<HTMLInputElement>("settings-night-quiet-to"),
    situationBreak: byId<HTMLInputElement>("settings-situation-break"),
    situationLateNight: byId<HTMLInputElement>("settings-situation-late-night"),
    situationBattery: byId<HTMLInputElement>("settings-situation-battery"),
    todoFollow: byId<HTMLInputElement>("settings-todo-follow"),
    minSpeakInterval: byId<HTMLInputElement>("settings-min-speak-interval"),
    calList: byId("settings-calendar-list"),
    calUrl: byId<HTMLInputElement>("settings-calendar-url"),
    calAddUrl: byId<HTMLButtonElement>("settings-calendar-add-url"),
    calPath: byId<HTMLInputElement>("settings-calendar-path"),
    calAddPath: byId<HTMLButtonElement>("settings-calendar-add-path"),
    calNotifyMin: byId<HTMLInputElement>("settings-calendar-notify-min"),
    calRefresh: byId<HTMLButtonElement>("settings-calendar-refresh"),
    calMessage: byId("settings-calendar-message"),
    weatherPlaceState: byId("weather-place-state"),
    weatherSearchInput: byId<HTMLInputElement>("weather-search-input"),
    weatherSearchBtn: byId<HTMLButtonElement>("weather-search-btn"),
    weatherCandidates: byId("weather-candidates"),
    weatherRefreshBtn: byId<HTMLButtonElement>("weather-refresh-btn"),
    weatherClearBtn: byId<HTMLButtonElement>("weather-clear-btn"),
    weatherPreview: byId("weather-preview"),
    situationRain: byId<HTMLInputElement>("settings-situation-rain"),
    weatherRainHint: byId("weather-rain-hint"),
    weatherMessage: byId("weather-message"),
    saveBtn: byId<HTMLButtonElement>("settings-save"),
    cancelBtn: byId<HTMLButtonElement>("settings-cancel"),
    closeBtn: byId<HTMLButtonElement>("settings-close"),
    msg: byId("settings-message"),
    pageSelector: byId<HTMLSelectElement>("settings-page-selector"),
    panelBody: (() => {
      const el = byId("settings-panel").querySelector<HTMLElement>(".panel-body");
      if (!el) throw new Error("設定パネルの .panel-body が見つかりません");
      return el;
    })(),
  };
}

function attachHandlers(i: Inputs): void {
  i.closeBtn.addEventListener("click", () => closeSettingsPanel());
  i.cancelBtn.addEventListener("click", () => closeSettingsPanel());
  i.saveBtn.addEventListener("click", () => void onSave());
  i.pageSelector.addEventListener("change", () => setActivePage(i.pageSelector.value));
  i.keyDelete.addEventListener("click", () => void onDeleteKey());
  i.provider.addEventListener("change", () => {
    void refreshKeyState(i.provider.value);
  });
  i.ttsEngine.addEventListener("change", () => updateVoiceEngineVisibility(i.ttsEngine.value));
  i.ttsDownloadBtn.addEventListener("click", () => void onTtsDownload());
  i.ghTokenDeleteBtn.addEventListener("click", () => void onDeleteGhToken());
  i.irodoriDownloadBtn.addEventListener("click", () => void onIrodoriDownload());
  i.voiceRefMainGenerate.addEventListener("click", () => void onVoiceRefGenerate("main"));
  i.voiceRefMainPreview.addEventListener("click", () => void onVoiceRefPreview("main"));
  i.voiceRefMainDelete.addEventListener("click", () => void onVoiceRefDelete("main"));
  i.voiceRefSubGenerate.addEventListener("click", () => void onVoiceRefGenerate("sub"));
  i.voiceRefSubPreview.addEventListener("click", () => void onVoiceRefPreview("sub"));
  i.voiceRefSubDelete.addEventListener("click", () => void onVoiceRefDelete("sub"));
  i.openChatlogBtn.addEventListener("click", () => void openChatLog());
  i.dataExportBtn.addEventListener("click", () => void onDataExport());
  i.dataClearBtn.addEventListener("click", () => void onDataClear());
  i.updateCheckBtn.addEventListener("click", () => void onUpdateCheck());
  i.topicsFetchBtn.addEventListener("click", () => void onTopicsFetch());
  i.calAddUrl.addEventListener("click", () => void onAddCalendarSource("url"));
  i.calAddPath.addEventListener("click", () => void onAddCalendarSource("file"));
  i.calRefresh.addEventListener("click", () => void onCalendarRefresh());
  i.weatherSearchBtn.addEventListener("click", () => void onWeatherSearch());
  i.weatherRefreshBtn.addEventListener("click", () => void onWeatherRefresh());
  i.weatherClearBtn.addEventListener("click", () => void onWeatherClear());
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
  // 規約同意ダイアログ (パネル既出のリンクで規約は提示済み)。
  const ok = await uggConfirm(
    "VOICEVOX 音声モデルおよび ONNX Runtime のライセンス・規約に同意してダウンロードしますか?\n（数百 MB の通信が発生します）",
    "ダウンロード確認",
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

// === M4c Phase F: Irodori-TTS UI =========================================

async function refreshIrodoriState(): Promise<void> {
  if (!inputs) return;
  let gpuOk = false;
  let assetsOk = false;
  // GPU 状態
  try {
    const gpu = await invoke<IrodoriGpuInfo>("irodori_check_gpu");
    if (gpu.available) {
      inputs.irodoriGpuState.textContent = gpu.name ?? "利用可能";
      inputs.irodoriGpuState.classList.add("has-key");
      gpuOk = true;
    } else {
      inputs.irodoriGpuState.textContent = gpu.reason ?? "利用不可";
      inputs.irodoriGpuState.classList.remove("has-key");
    }
  } catch (err) {
    inputs.irodoriGpuState.textContent = "確認失敗";
    console.warn("[irodori] gpu check failed", err);
  }
  // 資産状態
  try {
    const ready = await invoke<boolean>("irodori_assets_ready");
    inputs.irodoriAssetsState.textContent = ready ? "導入済み" : "未導入";
    inputs.irodoriAssetsState.classList.toggle("has-key", ready);
    assetsOk = ready;
  } catch (err) {
    inputs.irodoriAssetsState.textContent = "確認失敗";
    console.warn("[irodori] assets check failed", err);
  }
  // 資産の有無を記録する (未導入なら voicevox 選択中でも Irodori セクションを見せて
  // DL 導線を残す)。セクション表示の再適用は、下の自動フリップでエンジン値が確定した
  // あとにまとめて行う。
  irodoriAssetsReady = assetsOk;
  // ボタン disabled 制御 (GPU が無ければ DL も意味なし、実モデルチェックも不可)
  inputs.irodoriDownloadBtn.disabled = !gpuOk;
  const canUseReal = gpuOk && assetsOk;
  inputs.irodoriUseRealModel.disabled = !canUseReal;
  // 実モデルが使えない状態なら checked も false に倒し、settings.json に永続化された
  // 古い true 値を引きずらないよう同期する (GPU を抜いた / 資産を削除した直後の churn 防止)。
  if (!canUseReal && inputs.irodoriUseRealModel.checked) {
    inputs.irodoriUseRealModel.checked = false;
    if (current) {
      current.tts_irodori_use_real_model = false;
      try {
        await invoke("set_settings", { settings: current });
      } catch (err) {
        console.warn("[irodori] failed to clear stale use_real_model", err);
      }
    }
  }
  // tts_engine select の Irodori option も gpu/資産未準備のときは disabled にし、
  // 既に選択中なら voicevox_core に倒して保存する (毎発話で 90 秒 churn を踏まないため)。
  const irodoriOption = inputs.ttsEngine.querySelector<HTMLOptionElement>(
    'option[value="irodori"]',
  );
  if (irodoriOption) {
    irodoriOption.disabled = !canUseReal;
  }
  if (!canUseReal && inputs.ttsEngine.value === "irodori") {
    inputs.ttsEngine.value = "voicevox_core";
    if (current) {
      current.tts_engine = "voicevox_core";
      try {
        await invoke("set_settings", { settings: current });
      } catch (err) {
        console.warn("[irodori] failed to flip tts_engine to voicevox_core", err);
      }
    }
  }
  // 確定したエンジン値と資産状態でセクション表示を再適用する (資産未導入なら
  // voicevox でも Irodori セクションを表示し、ランタイム DL 導線を残す)。
  updateVoiceEngineVisibility(inputs.ttsEngine.value);
  // 参照音声一覧
  try {
    const refs = await invoke<VoiceRef[]>("voice_ref_list");
    applyVoiceRefState(refs);
  } catch (err) {
    inputs.voiceRefMainState.textContent = "確認失敗";
    inputs.voiceRefSubState.textContent = "確認失敗";
    console.warn("[irodori] voice_ref_list failed", err);
  }
}

function applyVoiceRefState(refs: VoiceRef[]): void {
  if (!inputs) return;
  const findFor = (slot: SlotName): VoiceRef | undefined =>
    refs.find((r) => r.slot === slot);
  const fmt = (r: VoiceRef | undefined): string => {
    if (!r) return "未生成";
    const date = new Date(r.created_ts * 1000).toLocaleString("ja-JP", {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
    const captionHead = r.caption.length > 30 ? r.caption.slice(0, 30) + "…" : r.caption;
    return `${date}「${captionHead}」`;
  };
  const m = findFor("main");
  const s = findFor("sub");
  inputs.voiceRefMainState.textContent = fmt(m);
  inputs.voiceRefMainState.classList.toggle("has-key", !!m);
  inputs.voiceRefSubState.textContent = fmt(s);
  inputs.voiceRefSubState.classList.toggle("has-key", !!s);
  // 既存キャプションを入力欄に復元 (再生成の起点に)
  if (m && !inputs.voiceRefMainCaption.value) {
    inputs.voiceRefMainCaption.value = m.caption;
  }
  if (s && !inputs.voiceRefSubCaption.value) {
    inputs.voiceRefSubCaption.value = s.caption;
  }
}

async function onIrodoriDownload(): Promise<void> {
  if (!inputs) return;
  const ok = await uggConfirm(
    "Irodori-TTS (高品質モード) の Python ランタイム + PyTorch (CUDA 12.8) + " +
      "実モデル実行時ランタイム (irodori-tts / dacvae / silentcipher など) をダウンロードします。\n" +
      "約 2〜3 GB の通信が発生し、10〜20 分かかります。続行しますか?",
    "ダウンロード確認",
  );
  if (!ok) return;
  showIrodoriProgress("ダウンロードを開始しています…", false);
  inputs.irodoriDownloadBtn.disabled = true;

  const unlisten = await listen<string>("irodori-download", (ev) => {
    if (ev.payload === "__done__") return;
    showIrodoriProgress(ev.payload, false);
  });

  try {
    await invoke("download_irodori_assets", { agreed: true });
    await refreshIrodoriState();
    // DL 直後はまだセットアップ継続中 (参照音声が未生成)。refreshIrodoriState は
    // voicevox 選択のままだと Irodori セクションを畳むが、ここで見せ続けて次の一手
    // (参照音声の生成 → エンジンを Irodori に切替) を案内する。engine を irodori に
    // 自動選択はしない (参照音声未生成のまま irodori にすると VoiceRefMissing で無音になる)。
    document
      .getElementById("settings-irodori-section")
      ?.classList.remove("force-hide");
    showIrodoriProgress(
      "ダウンロード完了。参照音声を生成し、エンジンで「Irodori-TTS」を選ぶと有効になります。",
      false,
    );
  } catch (err) {
    showIrodoriProgress(`ダウンロード失敗: ${formatErr(err)}`, true);
  } finally {
    unlisten();
    // 完了後のボタン状態は refreshIrodoriState が GPU/資産に応じて再設定済み
  }
}

async function onVoiceRefGenerate(slot: SlotName): Promise<void> {
  if (!inputs) return;
  const captionEl =
    slot === "main" ? inputs.voiceRefMainCaption : inputs.voiceRefSubCaption;
  const btn =
    slot === "main" ? inputs.voiceRefMainGenerate : inputs.voiceRefSubGenerate;
  const caption = captionEl.value.trim();
  if (!caption) {
    showVoiceRefProgress(`キャプションを入力してください (${slot})`, true);
    return;
  }
  btn.disabled = true;
  showVoiceRefProgress(
    `${slot} の参照音声を生成しています…（数十秒かかります）`,
    false,
  );
  try {
    const refs = await invoke<VoiceRef[]>("voice_ref_generate", { slot, caption });
    applyVoiceRefState(refs);
    showVoiceRefProgress(`${slot} の参照音声を生成しました`, false);
  } catch (err) {
    showVoiceRefProgress(`${slot} の生成失敗: ${formatErr(err)}`, true);
  } finally {
    btn.disabled = false;
  }
}

async function onVoiceRefPreview(slot: SlotName): Promise<void> {
  if (!inputs) return;
  const btn =
    slot === "main" ? inputs.voiceRefMainPreview : inputs.voiceRefSubPreview;
  btn.disabled = true;
  showVoiceRefProgress(`${slot} のプレビューを合成しています…`, false);
  try {
    const text = slot === "main" ? "こんにちは、私のメインの声です" : "こんにちは、サブの声です";
    const wavB64 = await invoke<string>("voice_ref_preview", { slot, text });
    await previewWavBase64(wavB64);
    showVoiceRefProgress(`${slot} のプレビューを再生しました`, false);
  } catch (err) {
    showVoiceRefProgress(`${slot} のプレビュー失敗: ${formatErr(err)}`, true);
  } finally {
    btn.disabled = false;
  }
}

async function onVoiceRefDelete(slot: SlotName): Promise<void> {
  if (!inputs) return;
  if (!(await uggConfirm(`${slot} の参照音声を削除しますか?`, "削除確認"))) return;
  try {
    const refs = await invoke<VoiceRef[]>("voice_ref_delete", { slot });
    applyVoiceRefState(refs);
    // キャプション欄もクリア
    if (slot === "main") inputs.voiceRefMainCaption.value = "";
    else inputs.voiceRefSubCaption.value = "";
    showVoiceRefProgress(`${slot} の参照音声を削除しました`, false);
  } catch (err) {
    showVoiceRefProgress(`${slot} の削除失敗: ${formatErr(err)}`, true);
  }
}

function showIrodoriProgress(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.irodoriProgress.textContent = msg;
  inputs.irodoriProgress.classList.toggle("error", isError);
  inputs.irodoriProgress.hidden = false;
}

function showVoiceRefProgress(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.voiceRefProgress.textContent = msg;
  inputs.voiceRefProgress.classList.toggle("error", isError);
  inputs.voiceRefProgress.hidden = false;
}

// === M5-F: ゴースト/シェル切替 UI =========================================

async function refreshAssetLists(s: Settings): Promise<void> {
  if (!inputs) return;
  try {
    const [ghosts, shells] = await Promise.all([
      invoke<AssetEntry[]>("list_ghosts"),
      invoke<AssetEntry[]>("list_shells"),
    ]);
    fillAssetSelect(inputs.ghostId, ghosts, s.ghost_id);
    fillAssetSelect(inputs.shellId, shells, s.shell_id);
  } catch (err) {
    console.warn("[assets] list_ghosts/list_shells failed", err);
  }
}

function fillAssetSelect(
  select: HTMLSelectElement,
  entries: AssetEntry[],
  current: string,
): void {
  select.innerHTML = "";
  for (const e of entries) {
    const opt = document.createElement("option");
    opt.value = e.id;
    opt.textContent = `${e.name} (${e.id})`;
    select.appendChild(opt);
  }
  // 現在値が一覧に無ければ "現在の設定" を頭に挿入
  if (!entries.some((e) => e.id === current)) {
    const opt = document.createElement("option");
    opt.value = current;
    opt.textContent = `${current} (現在の設定)`;
    select.insertBefore(opt, select.firstChild);
  }
  select.value = current;
}

// === M5-E: データエクスポート / 履歴クリア =================================

async function onDataExport(): Promise<void> {
  if (!inputs) return;
  inputs.dataExportBtn.disabled = true;
  showDataMessage("エクスポート中…", false);
  try {
    const path = await invoke<string>("export_data", {
      includeProfile: inputs.dataIncludeProfile.checked,
    });
    showDataMessage(`保存しました: ${path}`, false);
  } catch (err) {
    showDataMessage(`エクスポート失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.dataExportBtn.disabled = false;
  }
}

async function onDataClear(): Promise<void> {
  if (!inputs) return;
  const includeProfile = inputs.dataIncludeProfile.checked;
  const confirmMsg = includeProfile
    ? "会話ログと記憶 (user_profile) を全て削除します。続行しますか?"
    : "会話ログを全て削除します (記憶は残します)。続行しますか?";
  if (!(await uggConfirm(confirmMsg, "履歴クリア"))) return;
  inputs.dataClearBtn.disabled = true;
  try {
    const res = await invoke<ClearResult>("clear_history", {
      includeProfile,
    });
    const profMsg = includeProfile
      ? `、記憶 ${res.profile_cleared_count} 件削除`
      : "";
    showDataMessage(`会話ログを削除しました${profMsg}`, false);
  } catch (err) {
    showDataMessage(`削除失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.dataClearBtn.disabled = false;
  }
}

function showDataMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.dataMessage.textContent = msg;
  inputs.dataMessage.classList.toggle("error", isError);
  inputs.dataMessage.hidden = false;
}

// === M5-D: 更新通知 =======================================================

async function onUpdateCheck(): Promise<void> {
  if (!inputs) return;
  // 入力欄の URL を Settings に反映させてから check_update_now を呼ぶ
  const next: Settings = {
    ...(current as Settings),
    update_feed_url: inputs.updateFeedUrl.value.trim() || null,
  };
  if (!next.update_feed_url) {
    showUpdateMessage("フィード URL を入力してください", true);
    return;
  }
  inputs.updateCheckBtn.disabled = true;
  showUpdateMessage("確認中…", false);
  try {
    await invoke("set_settings", { settings: next });
    current = next;
    await invoke("check_update_now");
    showUpdateMessage("確認しました (新版があればゴーストが告知します)", false);
  } catch (err) {
    showUpdateMessage(`確認失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.updateCheckBtn.disabled = false;
  }
}

function showUpdateMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.updateMessage.textContent = msg;
  inputs.updateMessage.classList.toggle("error", isError);
  inputs.updateMessage.hidden = false;
}

// === M5-C: 時事ネタ (興味分野) UI =========================================

async function refreshInterests(): Promise<void> {
  if (!inputs) return;
  try {
    const topics = await invoke<InterestTopic[]>("get_interests");
    inputs.topicsInterests.value = topics.map((t) => t.topic).join(", ");
  } catch (err) {
    console.warn("[topics] get_interests failed", err);
  }
}

function parseInterestList(value: string): string[] {
  return value
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
    .slice(0, 20);
}

async function onTopicsFetch(): Promise<void> {
  if (!inputs) return;
  // 入力された interests を先に保存してから即時取得 (UX 上の自然な流れ)
  const list = parseInterestList(inputs.topicsInterests.value);
  if (list.length === 0) {
    showTopicsMessage("キーワードを入力してください", true);
    return;
  }
  inputs.topicsFetchBtn.disabled = true;
  showTopicsMessage("取得中…", false);
  try {
    await invoke("set_interests", { topics: list });
    await invoke("fetch_topics_now");
    showTopicsMessage(
      `${list.length} 件のキーワードで RSS を取得しました`,
      false,
    );
  } catch (err) {
    showTopicsMessage(`取得失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.topicsFetchBtn.disabled = false;
  }
}

function showTopicsMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.topicsMessage.textContent = msg;
  inputs.topicsMessage.classList.toggle("error", isError);
  inputs.topicsMessage.hidden = false;
}

// === M10: カレンダーソース UI =============================================

function renderCalendarSources(sources: CalendarSource[]): void {
  if (!inputs) return;
  inputs.calList.innerHTML = "";
  if (sources.length === 0) {
    const empty = document.createElement("p");
    empty.className = "panel-hint";
    empty.textContent = "登録済みのカレンダーはありません。";
    inputs.calList.appendChild(empty);
    return;
  }
  sources.forEach((src, index) => {
    const item = document.createElement("div");
    item.className = "row";
    const label = document.createElement("span");
    const isFile = src.kind === "file";
    label.textContent = `${isFile ? "📄" : "🌐"} ${isFile ? src.path : src.url}`;
    label.style.overflow = "hidden";
    label.style.textOverflow = "ellipsis";
    const del = document.createElement("button");
    del.type = "button";
    del.textContent = "削除";
    del.addEventListener("click", () => void onRemoveCalendarSource(index));
    item.appendChild(label);
    item.appendChild(del);
    inputs!.calList.appendChild(item);
  });
}

async function onAddCalendarSource(kind: "url" | "file"): Promise<void> {
  if (!inputs) return;
  const input = kind === "url" ? inputs.calUrl : inputs.calPath;
  const value = input.value.trim();
  if (!value) {
    showCalendarMessage(kind === "url" ? "URL を入力してください" : "パスを入力してください", true);
    return;
  }
  const source: CalendarSource =
    kind === "url" ? { kind: "url", url: value } : { kind: "file", path: value };
  try {
    const list = await invoke<CalendarSource[]>("add_calendar_source", { source });
    if (current) current.calendar_sources = list;
    renderCalendarSources(list);
    input.value = "";
    showCalendarMessage("追加しました。『いま取得』で読み込めます", false);
  } catch (err) {
    showCalendarMessage(`追加失敗: ${formatErr(err)}`, true);
  }
}

async function onRemoveCalendarSource(index: number): Promise<void> {
  if (!inputs) return;
  try {
    const list = await invoke<CalendarSource[]>("remove_calendar_source", { index });
    if (current) current.calendar_sources = list;
    renderCalendarSources(list);
    showCalendarMessage("削除しました", false);
  } catch (err) {
    showCalendarMessage(`削除失敗: ${formatErr(err)}`, true);
  }
}

async function onCalendarRefresh(): Promise<void> {
  if (!inputs) return;
  inputs.calRefresh.disabled = true;
  showCalendarMessage("取得中…", false);
  try {
    const n = await invoke<number>("refresh_calendar");
    showCalendarMessage(`${n} 件の予定を取り込みました`, false);
  } catch (err) {
    showCalendarMessage(`取得失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.calRefresh.disabled = false;
  }
}

function showCalendarMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.calMessage.textContent = msg;
  inputs.calMessage.classList.toggle("error", isError);
  inputs.calMessage.hidden = false;
}

// === M11: 天気 UI (spec §4.7.2 / regular-talk-design §9.2) ==================
// カレンダー節の「入力 → 追加 (即保存) → いま取得で検証」パターンを踏襲する。
// 候補選択・解除はどちらも即時 set_settings (設定行為 = 同意/撤回)。

/// 設定済み表示・ボタンの有効無効を現在の Settings から反映する。
function applyWeatherState(s: Settings): void {
  if (!inputs) return;
  const ready = isWeatherReady(s);
  inputs.weatherPlaceState.textContent = ready
    ? `${s.weather_place_name || "(地名未取得)"}（${s.weather_latitude}, ${s.weather_longitude}）`
    : "未設定";
  inputs.weatherPlaceState.classList.toggle("has-key", ready);
  inputs.weatherRefreshBtn.disabled = !ready;
  inputs.weatherClearBtn.disabled = !ready;
  inputs.situationRain.disabled = !ready;
  inputs.weatherRainHint.hidden = ready;
}

function renderWeatherCandidates(hits: LocationHit[]): void {
  if (!inputs) return;
  inputs.weatherCandidates.innerHTML = "";
  for (const hit of hits) {
    const item = document.createElement("div");
    item.className = "row";
    const label = document.createElement("span");
    label.textContent = [hit.name, hit.admin1, hit.country].filter((v) => !!v).join(" / ");
    label.style.overflow = "hidden";
    label.style.textOverflow = "ellipsis";
    const selectBtn = document.createElement("button");
    selectBtn.type = "button";
    selectBtn.textContent = "選択";
    selectBtn.addEventListener("click", () => void onWeatherSelect(hit));
    item.appendChild(label);
    item.appendChild(selectBtn);
    inputs!.weatherCandidates.appendChild(item);
  }
}

async function onWeatherSearch(): Promise<void> {
  if (!inputs) return;
  const query = inputs.weatherSearchInput.value.trim();
  if (query.length < 2) {
    showWeatherMessage("2 文字以上で検索してください", true);
    return;
  }
  inputs.weatherSearchBtn.disabled = true;
  showWeatherMessage("検索中…", false);
  try {
    const hits = await invoke<LocationHit[]>("search_location", { query });
    renderWeatherCandidates(hits);
    showWeatherMessage(
      hits.length > 0
        ? `${hits.length} 件見つかりました。選んでください`
        : "見つかりませんでした。市区町村名・都道府県名で言い換えてお試しください (例: 東京→東京都、横浜→横浜市)",
      false,
    );
  } catch (err) {
    showWeatherMessage(`検索失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.weatherSearchBtn.disabled = false;
  }
}

/// 候補選択 = 同意 (地域設定 + weather_enabled を同時に立てて即保存)。
async function onWeatherSelect(hit: LocationHit): Promise<void> {
  if (!inputs || !current) return;
  const placeName = [hit.name, hit.admin1].filter((v) => !!v).join(" ");
  const next: Settings = {
    ...current,
    weather_enabled: true,
    weather_latitude: hit.latitude,
    weather_longitude: hit.longitude,
    weather_place_name: placeName,
  };
  try {
    const saved = await invoke<Settings>("set_settings", { settings: next });
    current = saved;
    applyWeatherState(saved);
    inputs.weatherCandidates.innerHTML = "";
    inputs.weatherSearchInput.value = "";
    showWeatherMessage(`「${placeName}」を設定しました`, false);
  } catch (err) {
    showWeatherMessage(`設定失敗: ${formatErr(err)}`, true);
  }
}

/// 解除 = 同意の撤回。座標・地名を空に戻す (weather_cache のクリアはバックエンドの
/// set_settings が weather_ready: true→false を検知して自動で行う)。
async function onWeatherClear(): Promise<void> {
  if (!inputs || !current) return;
  if (!(await uggConfirm("設定中の地域を解除しますか？天気機能が無効になります。", "解除確認"))) {
    return;
  }
  const next: Settings = {
    ...current,
    weather_enabled: false,
    weather_latitude: null,
    weather_longitude: null,
    weather_place_name: "",
  };
  try {
    const saved = await invoke<Settings>("set_settings", { settings: next });
    current = saved;
    applyWeatherState(saved);
    inputs.weatherPreview.hidden = true;
    showWeatherMessage("地域設定を解除しました", false);
  } catch (err) {
    showWeatherMessage(`解除失敗: ${formatErr(err)}`, true);
  }
}

/// 検証用の技術的な読み出し (WMO code の日本語ラベル変換はバックエンドのみが持つ
/// 単一情報源なので、ここでは複製せず生値を表示する)。
function formatDailyPreview(d: DailyWeather): string {
  return `${d.date}: code${d.weather_code} 最高${d.temp_max}℃ / 最低${d.temp_min}℃ / 降水${d.precip_prob_max}%`;
}

async function onWeatherRefresh(): Promise<void> {
  if (!inputs) return;
  inputs.weatherRefreshBtn.disabled = true;
  showWeatherMessage("取得中…", false);
  try {
    const snap = await invoke<WeatherSnapshot | null>("get_weather");
    if (!snap || snap.daily.length === 0) {
      showWeatherPreview("天気を取得できませんでした（しばらくしてから再度お試しください）", true);
    } else {
      showWeatherPreview(snap.daily.map(formatDailyPreview).join(" / "), false);
    }
    showWeatherMessage("取得しました", false);
  } catch (err) {
    showWeatherPreview(`取得失敗: ${formatErr(err)}`, true);
  } finally {
    inputs.weatherRefreshBtn.disabled = !isWeatherReady(current);
  }
}

function showWeatherMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.weatherMessage.textContent = msg;
  inputs.weatherMessage.classList.toggle("error", isError);
  inputs.weatherMessage.hidden = false;
}

function showWeatherPreview(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.weatherPreview.textContent = msg;
  inputs.weatherPreview.classList.toggle("error", isError);
  inputs.weatherPreview.hidden = false;
}

// M7: リマインダーの一覧・操作は専用パネル (panels/daily.ts) に移設した。
// 設定パネル側は「日常支援」ページのトグル類のみを持つ。

/// 「HH:MM」⇔ 0:00 からの分。time input の値変換 (M7 夜間静音)。
function minutesToHHMM(min: number): string {
  const clamped = Math.max(0, Math.min(1439, Math.floor(min)));
  const h = Math.floor(clamped / 60);
  const m = clamped % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

function hhmmToMinutes(value: string, fallback: number): number {
  const m = /^(\d{1,2}):(\d{2})$/.exec(value.trim());
  if (!m) return fallback;
  const h = Number(m[1]);
  const mm = Number(m[2]);
  if (h > 23 || mm > 59) return fallback;
  return h * 60 + mm;
}

/// 非負整数に丸める (0 も有効値)。空欄・非数のみ fallback へ。
/// `Number(value) || fallback` だと 0 が fallback に化けるため専用ヘルパにする。
function clampNonNegInt(value: string, fallback: number): number {
  const n = Math.round(Number(value));
  return Number.isFinite(n) ? Math.max(0, n) : fallback;
}

function showProgress(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.ttsProgress.textContent = msg;
  inputs.ttsProgress.classList.toggle("error", isError);
  inputs.ttsProgress.hidden = false;
}

/// エンジン選択に応じて音声タブのセクション表示を切り替える。
/// DOM は index.html に静的配置済み (WebView2 透過バグ回避)。ここでは hidden の
/// 付け外しのみで、非表示中の入力値も保存対象からは外さない (見た目だけ変える)。
function updateVoiceEngineVisibility(engine: string): void {
  const irodori = engine === "irodori";
  // .panel-section / .row は CSS で display を明示しているため hidden 属性では
  // 隠せない。force-hide (display:none !important) の class トグルで確実に切り替える。
  // Irodori セクションは「irodori 選択中」または「資産未導入 (=セットアップが必要)」の
  // どちらかで表示する。voicevox 選択中でも資産が無ければ DL 導線を残す
  // (これを隠すとクリーンインストールで Irodori を有効化する導線が消えて詰む)。
  const irodoriSection = document.getElementById("settings-irodori-section");
  const hideIrodoriSection = !irodori && irodoriAssetsReady;
  if (irodoriSection) irodoriSection.classList.toggle("force-hide", hideIrodoriSection);
  // VOICEVOX の話者 (style ID) 選択は voicevox 時のみ
  for (const row of document.querySelectorAll<HTMLElement>(".vv-speaker-row")) {
    row.classList.toggle("force-hide", irodori);
  }
  // Irodori でも VOICEVOX 資産は必要な旨の注記は irodori 時のみ
  const vvNote = document.getElementById("settings-vv-assets-note");
  if (vvNote) vvNote.classList.toggle("force-hide", !irodori);
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
  // M4c: tts_engine が "voicevox_core" | "irodori" に揃っているか UI で復元
  inputs.ttsEngine.value =
    s.tts_engine === "irodori" ? "irodori" : "voicevox_core";
  updateVoiceEngineVisibility(inputs.ttsEngine.value);
  inputs.irodoriUseRealModel.checked = s.tts_irodori_use_real_model;
  inputs.autostart.checked = s.autostart;
  inputs.updateFeedUrl.value = s.update_feed_url ?? "";
  inputs.topicsEnabled.checked = s.topics_enabled;
  inputs.toolsEnabled.checked = s.tools_enabled;
  inputs.dailySupport.checked = s.daily_support_enabled;
  inputs.reminderNotify.checked = s.reminder_notify_enabled;
  inputs.nightQuiet.checked = s.night_quiet_enabled;
  inputs.nightQuietFrom.value = minutesToHHMM(s.night_quiet_from);
  inputs.nightQuietTo.value = minutesToHHMM(s.night_quiet_to);
  inputs.situationBreak.checked = s.situation_break_enabled;
  inputs.situationLateNight.checked = s.situation_late_night_enabled;
  inputs.situationBattery.checked = s.situation_battery_enabled;
  inputs.todoFollow.checked = s.todo_follow_enabled;
  inputs.minSpeakInterval.value = String(s.min_speak_interval_min);
  inputs.calNotifyMin.value = String(s.calendar_notify_min);
  renderCalendarSources(s.calendar_sources);
  inputs.situationRain.checked = s.situation_rain_enabled;
  applyWeatherState(s);
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
    tts_engine: inputs.ttsEngine.value || current.tts_engine,
    tts_speaker_main: Number(inputs.ttsSpeakerMain.value) || current.tts_speaker_main,
    tts_speaker_sub: Number(inputs.ttsSpeakerSub.value) || current.tts_speaker_sub,
    tts_speed: Number(inputs.ttsSpeed.value) || current.tts_speed,
    tts_volume: Number(inputs.ttsVolume.value) || current.tts_volume,
    tts_irodori_use_real_model: inputs.irodoriUseRealModel.checked,
    autostart: inputs.autostart.checked,
    update_feed_url: inputs.updateFeedUrl.value.trim() || null,
    topics_enabled: inputs.topicsEnabled.checked,
    tools_enabled: inputs.toolsEnabled.checked,
    daily_support_enabled: inputs.dailySupport.checked,
    reminder_notify_enabled: inputs.reminderNotify.checked,
    night_quiet_enabled: inputs.nightQuiet.checked,
    night_quiet_from: hhmmToMinutes(inputs.nightQuietFrom.value, current.night_quiet_from),
    night_quiet_to: hhmmToMinutes(inputs.nightQuietTo.value, current.night_quiet_to),
    situation_break_enabled: inputs.situationBreak.checked,
    situation_late_night_enabled: inputs.situationLateNight.checked,
    situation_battery_enabled: inputs.situationBattery.checked,
    todo_follow_enabled: inputs.todoFollow.checked,
    min_speak_interval_min: clampNonNegInt(
      inputs.minSpeakInterval.value,
      current.min_speak_interval_min,
    ),
    // calendar_sources は add/remove コマンドが即時永続化するので current を保つ
    calendar_notify_min: clampNonNegInt(
      inputs.calNotifyMin.value,
      current.calendar_notify_min,
    ),
    // weather_enabled/latitude/longitude/place_name は候補選択・解除コマンドが
    // 即時永続化するので current を保つ (M11、calendar_sources と同型)。
    situation_rain_enabled: inputs.situationRain.checked,
    ghost_id: inputs.ghostId.value || current.ghost_id,
    shell_id: inputs.shellId.value || current.shell_id,
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

  // M5-H: autostart の変更を OS 側に反映
  if (current.autostart !== next.autostart) {
    try {
      await invoke("set_autostart", { enabled: next.autostart });
    } catch (err) {
      showMessage(`自動起動の切替に失敗: ${formatErr(err)}`, true);
      return;
    }
  }
  // M5-C: 興味分野リスト (Settings の外、DB に直接保存)
  const topicsList = parseInterestList(inputs.topicsInterests.value);
  try {
    await invoke("set_interests", { topics: topicsList });
  } catch (err) {
    showMessage(`興味分野の保存に失敗: ${formatErr(err)}`, true);
    return;
  }
  // M5-F: ghost / shell 切替は再起動が必要
  const needsRestart =
    current.ghost_id !== next.ghost_id || current.shell_id !== next.shell_id;

  try {
    const saved = await invoke<Settings>("set_settings", { settings: next });
    current = saved;
    applySettingsToForm(saved);
    await refreshKeyState(saved.llm_provider);
    onSaved?.(saved);
    if (needsRestart) {
      showMessage(
        "ゴースト/シェルの変更を反映するにはアプリの再起動が必要です",
        false,
      );
      // 再起動案内は閉じずに残す
      return;
    }
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
