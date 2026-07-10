import { invoke } from "@tauri-apps/api/core";

import { preallocateBalloons } from "./dialogue/balloon";
import {
  closeInput,
  isInputOpen,
  mountInput,
  openInputFor,
  setToolsEnabled,
} from "./dialogue/input";
import { attachClickDetector } from "./interaction/click";
import { attachNadeDetector } from "./interaction/nade";
import { firePoke } from "./interaction/poke";
import { mountContextMenu } from "./menu/context-menu";
import { showOnboarding } from "./panels/onboarding";
import { mountPomodoroBadge } from "./panels/pomodoro";
import { mountConfirm } from "./confirm";
import { mountDnd } from "./dnd";
import { mountChatLog } from "./panels/chatlog";
import { mountReaderPanel } from "./panels/reader";
import { mountSettingsPanel, registerSavedListener } from "./panels/settings";
import { mountCredit, refreshCredit } from "./tts/credit";
import { createSpeaker, setTtsParams } from "./tts/speaker";
import { installAlphaMaskHooks, scheduleMaskUpdate } from "./stage/alphamask";
import { hitTest, mountSlot, unmountSlot } from "./stage/character";
import { initCharPositions, reclampAll } from "./stage/charpos";
import { applyDisplayScale } from "./stage/scale";
import { renderPrompt, renderResponse, setTalkSpeed, startListening } from "./system/ghost-speech";
import type { BootPayload, Settings, SpeechTurn } from "./types";

let currentSettings: Settings | null = null;

async function boot(): Promise<void> {
  let payload: BootPayload;
  try {
    payload = await invoke<BootPayload>("get_boot_payload");
  } catch (err) {
    showFatal(`起動データの取得に失敗しました: ${formatError(err)}`);
    return;
  }

  currentSettings = payload.settings;
  applyDisplayScale(payload.settings.display_scale);
  setTalkSpeed(payload.settings.talk_speed);
  setTtsParams({
    enabled: payload.settings.tts_enabled,
    speed: payload.settings.tts_speed,
    volume: payload.settings.tts_volume,
  });

  mountSlot("main", payload.characters.main);
  if (payload.characters.sub) {
    mountSlot("sub", payload.characters.sub);
  } else {
    unmountSlot("sub");
  }
  // 保存済みキャラ X 位置の復元 (無ければ既定配置)。ペイント前に反映される。
  initCharPositions(payload.char_positions);
  // ステージリサイズ (モニタ構成変更の再ドック) でキャラをステージ内に収め直す
  window.addEventListener("resize", reclampAll);

  preallocateBalloons();
  mountInput(renderResponse);
  setToolsEnabled(payload.settings.tools_enabled);
  attachClickDetector(({ count, x, y }) => {
    // spec §4.3.1: 1 回 = 入力導線 (促し発話 + 入力欄)、2-3 回 = つつき、4 回以上 = 連打
    if (count === 1) {
      onSingleClick(x, y);
    } else if (count >= 2) {
      void firePoke(count, x, y);
    }
  });
  attachNadeDetector();

  await mountSettingsPanel();
  await mountPomodoroBadge();
  mountCredit();
  mountChatLog();
  mountReaderPanel();
  mountConfirm();
  await mountDnd();
  // TTS スピーカーを ghost-speech に渡す
  const speaker = createSpeaker();
  const { setSpeaker } = await import("./system/ghost-speech");
  setSpeaker(speaker);
  await refreshCredit(
    payload.settings.tts_enabled,
    payload.settings.tts_engine,
    payload.settings.tts_speaker_main,
    payload.settings.tts_speaker_sub,
  );
  registerSavedListener((s) => {
    currentSettings = s;
    applyDisplayScale(s.display_scale);
    reclampAll(); // スケール変更で視覚幅が変わるためステージ内に収め直す
    setToolsEnabled(s.tools_enabled);
    setTalkSpeed(s.talk_speed);
    setTtsParams({ enabled: s.tts_enabled, speed: s.tts_speed, volume: s.tts_volume });
    void refreshCredit(s.tts_enabled, s.tts_engine, s.tts_speaker_main, s.tts_speaker_sub);
  });
  mountContextMenu({
    current: () => currentSettings,
    onModeToggle: async (next) => {
      if (!currentSettings) return;
      const updated = await invoke<Settings>("set_settings", {
        settings: { ...currentSettings, mode: next },
      });
      currentSettings = updated;
    },
  });
  window.addEventListener("ugg-hide-window", () => {
    void invoke("hide_window").catch((err) => console.error(err));
  });
  window.addEventListener("ugg-toggle-quiet", () => {
    if (!currentSettings) return;
    const next = { ...currentSettings, quiet_mode: !currentSettings.quiet_mode };
    void invoke<Settings>("set_settings", { settings: next })
      .then((s) => {
        currentSettings = s;
      })
      .catch((err) => console.error(err));
  });

  installAlphaMaskHooks();
  // pose / バルーン表示 / 入力欄表示が変わるたびにマスク更新が要る。
  // 個別 hook より MutationObserver の方が網羅的なので採用する。
  observeUiMutations();

  await startListening();
  // 起動挨拶。エラーは握りつぶす (UX に致命的でないため)。
  try {
    await invoke("frontend_ready");
  } catch (err) {
    console.error("frontend_ready failed", err);
  }

  // 初回のみオンボーディングを表示 (起動挨拶とは独立、両方出てよい)。
  if (!payload.onboarded) {
    showOnboarding();
  }
}

/// クリック 1 回の入力導線 (spec §4.3.1)。
/// 開いていれば閉じる。閉じていれば、クリックされたキャラの促し発話を出し、
/// そのキャラのバルーン上側に入力欄を開く。キャラ外 (吹き出し等) は main 扱い。
function onSingleClick(x: number, y: number): void {
  if (isInputOpen()) {
    closeInput();
    return;
  }
  const slot = hitTest(x, y)?.slot ?? "main";
  openInputFor(slot);
  // 促し発話は辞書 input_prompt から。未定義の辞書では null (入力欄だけ開く)。
  void invoke<SpeechTurn | null>("input_prompt", { target: slot })
    .then((turn) => (turn ? renderPrompt(slot, turn) : undefined))
    .catch((err) => console.error("input_prompt failed", err));
}

function observeUiMutations(): void {
  const layer = document.getElementById("stage");
  if (!layer) return;
  const observer = new MutationObserver(() => scheduleMaskUpdate());
  observer.observe(layer, {
    attributes: true,
    attributeFilter: ["class", "style"],
    subtree: true,
    childList: true,
  });
}

function formatError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

function showFatal(message: string): void {
  const layer = document.getElementById("ui-layer");
  if (!layer) return;
  const box = document.createElement("div");
  box.style.cssText = [
    "position:absolute",
    "left:16px",
    "top:16px",
    "right:16px",
    "padding:12px 14px",
    "background:rgba(255, 235, 235, 0.95)",
    "color:#4a1010",
    "border:1px solid #c98080",
    "border-radius:6px",
    "font-size:12px",
    "line-height:1.5",
    "white-space:pre-wrap",
    "pointer-events:auto",
  ].join(";");
  box.textContent = message;
  layer.appendChild(box);
}

void boot();
