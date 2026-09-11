import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadIndexHtml } from "./dom";
import type { Settings } from "../types";

/// 操作列テスト 2/4「保存してからの再保存」（spec §6.0 項目 9、v0.5.3）。
///
/// ゴーストを切り替えて保存すると、バックは `default_shell` へシェルを追従させて
/// **保存前と違う値**を返す（spec §4.5.6）。v0.5.2 まではその結果をフォームへ
/// 書き戻す処理がゴースト・シェルの select だけを対象外にしていたため、
/// select は古い id を表示したままで、**パネルを開いたまま再保存すると
/// 追従が黙って取り消されていた**。
///
/// ゴースト切替は「再起動が必要」の案内のためにパネルを閉じない経路なので、
/// 「保存 → もう一度保存」は普通に起きる。
///
/// 単機能テスト（「保存すると set_settings が呼ばれる」「追従は成立する」）では
/// 捕まらない。**2 回目の保存**まで流さないと壊れない。
///
/// v0.5.1 でこの指摘を「イベントを再適用するから成立しない」として棄却したのも
/// 同じ理由で、`settings-changed` の受信までは確かめたが、その先の
/// `applySettingsToForm` が対象の select に触れるかを見ていなかった。

const invoked: { cmd: string; args: unknown }[] = [];
let currentSettings: Settings;
/// `set_settings` の応答を作る。既定は「渡されたものをそのまま保存」。
let saveResponder: (next: Settings) => Settings;

// `restoreMocks: true` なので `vi.fn().mockResolvedValue(...)` は 1 本目のテストで
// 実装ごと消える（2 本目以降 undefined が返り、`unlisten()` が落ちる）。
// **実装を渡した `vi.fn(impl)` にすること**（reset は impl に戻す）。
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock("../tts/speaker", () => ({ previewWavBase64: vi.fn() }));
vi.mock("../confirm", () => ({ uggConfirm: vi.fn(async () => true) }));
vi.mock("./chatlog", () => ({ openChatLog: vi.fn() }));
vi.mock("../weather/credit", () => ({ isWeatherReady: () => false }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: unknown) => {
    invoked.push({ cmd, args });
    switch (cmd) {
      case "get_settings":
        return currentSettings;
      case "set_settings": {
        const next = (args as { settings: Settings }).settings;
        currentSettings = saveResponder(next);
        return currentSettings;
      }
      case "list_ghosts":
        return [
          { id: "mimi", name: "ミミとクロ" },
          { id: "poko", name: "ポコ" },
        ];
      case "list_shells":
        return [
          { id: "default", name: "既定シェル" },
          { id: "poko_shell", name: "ポコのシェル" },
        ];
      case "has_api_key":
      case "has_github_token":
      case "voicevox_assets_ready":
      case "irodori_assets_ready":
        return false;
      case "get_cost_status":
        return { current_usd: 0, limit_usd: 0, unlimited: true, ratio: 0 };
      case "get_db_health":
        return { ok: true, detail: "ok", backup_path: null, salvaged_path: null };
      case "get_interests":
      case "get_profile":
      case "voice_ref_list":
        return [];
      case "get_irodori_status":
        return {
          present: false,
          has_record: false,
          up_to_date: false,
          outdated: [],
          resolved: {},
        };
      case "irodori_check_gpu":
        return { available: false, name: null, reason: "テスト環境" };
      case "list_monitors":
        return { monitors: [], current: null };
      case "set_interests":
      case "set_autostart":
        return null;
      default:
        return null;
    }
  }),
}));

function baseSettings(): Settings {
  return {
    mode: "low",
    ghost_id: "mimi",
    shell_id: "default",
    display_scale: 1,
    quiet_mode: false,
    talk_speed: "normal",
    llm_provider: "openai",
    llm_model: "gpt-4o-mini",
    llm_base_url: null,
    monthly_limit_usd: 0,
    profile_max_count: 50,
    auto_quiet_fullscreen: false,
    monologue_interval_min: 30,
    pomodoro_work_min: 25,
    pomodoro_break_min: 5,
    pomodoro_rounds: 4,
    tts_enabled: false,
    tts_engine: "voicevox_core",
    tts_speaker_main: 1,
    tts_speaker_sub: 2,
    tts_speed: 1,
    tts_volume: 1,
    tts_irodori_use_real_model: false,
    autostart: false,
    update_feed_url: null,
    topics_enabled: false,
    tools_enabled: false,
    daily_support_enabled: true,
    reminder_notify_enabled: true,
    night_quiet_enabled: false,
    night_quiet_from: 0,
    night_quiet_to: 420,
    situation_break_enabled: false,
    situation_late_night_enabled: false,
    situation_battery_enabled: false,
    todo_follow_enabled: false,
    min_speak_interval_min: 10,
    calendar_sources: [],
    calendar_notify_min: 15,
    regular_morning_enabled: false,
    regular_morning_time: 480,
    regular_morning_days: 0b0111_1111,
    regular_evening_enabled: false,
    regular_evening_time: 1200,
    regular_evening_days: 0b0111_1111,
    weather_enabled: false,
    weather_latitude: null,
    weather_longitude: null,
    weather_place_name: "",
    situation_rain_enabled: false,
  };
}

function savedPayloads(): Settings[] {
  return invoked
    .filter((c) => c.cmd === "set_settings")
    .map((c) => (c.args as { settings: Settings }).settings);
}

describe("操作列: ゴーストを切り替えて保存 → もう一度保存", () => {
  beforeEach(() => {
    invoked.length = 0;
    currentSettings = baseSettings();
    saveResponder = (next) => next;
    loadIndexHtml();
    vi.resetModules();
  });

  it("default_shell の追従が、2 回目の保存で取り消されない", async () => {
    const panel = await import("../panels/settings");
    await panel.mountSettingsPanel();
    await panel.openSettingsPanel();

    const ghostSelect = document.getElementById("settings-ghost-id") as HTMLSelectElement;
    const shellSelect = document.getElementById("settings-shell-id") as HTMLSelectElement;
    expect(shellSelect.value).toBe("default");

    // バックは ghost=poko の default_shell へシェルを追従させる (spec §4.5.6)
    saveResponder = (next) =>
      next.ghost_id === "poko" ? { ...next, shell_id: "poko_shell" } : next;

    // 1 回目: ゴーストだけ変えて保存
    ghostSelect.value = "poko";
    document.getElementById("settings-save")!.dispatchEvent(new Event("click"));
    await vi.waitFor(() => expect(savedPayloads()).toHaveLength(1));

    expect(savedPayloads()[0].shell_id, "保存時点ではまだ旧シェル").toBe("default");
    expect(
      shellSelect.value,
      "追従した結果がフォームへ戻っていること (ここが本体)",
    ).toBe("poko_shell");

    // 2 回目: 何も触らずもう一度保存する
    //（ゴースト切替は再起動案内のためパネルを閉じないので、普通に起きる操作）
    document.getElementById("settings-save")!.dispatchEvent(new Event("click"));
    await vi.waitFor(() => expect(savedPayloads()).toHaveLength(2));

    expect(
      savedPayloads()[1].shell_id,
      "再保存でフォームが古い id を送り返すと、追従が黙って取り消される",
    ).toBe("poko_shell");
    expect(savedPayloads()[1].ghost_id).toBe("poko");
  });

  it("ユーザーが同じ操作でシェルも明示的に選んだら、そちらが優先される", async () => {
    const panel = await import("../panels/settings");
    await panel.mountSettingsPanel();
    await panel.openSettingsPanel();

    const ghostSelect = document.getElementById("settings-ghost-id") as HTMLSelectElement;
    const shellSelect = document.getElementById("settings-shell-id") as HTMLSelectElement;
    ghostSelect.value = "poko";
    shellSelect.value = "default";
    document.getElementById("settings-save")!.dispatchEvent(new Event("click"));
    await vi.waitFor(() => expect(savedPayloads()).toHaveLength(1));

    expect(savedPayloads()[0].shell_id).toBe("default");
    expect(savedPayloads()[0].ghost_id).toBe("poko");
  });
});
