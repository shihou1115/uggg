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
/// VOICEVOX の資産が入っているか（話者の select が埋まるか）。声の操作列テストだけ true にする。
let voicevoxReady = false;
/// 読み込み中のゴースト（boot payload 相当。v0.6.0 項目 2）。設定の `ghost_id` とは別物 —
/// 切り替えて再起動するまでは食い違う。
let loadedGhost = { id: "mimi", name: "ミミとクロ", defaultSpeakers: { main: 2, sub: 3 } };
const VOICES = [0, 1, 2, 3, 6, 7, 8, 9].map((id) => ({ id, name: `声${id}` }));

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
      case "voicevox_assets_ready":
        return voicevoxReady;
      case "list_voices":
        return VOICES;
      case "has_api_key":
      case "has_github_token":
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
    // 旧版の単一の話者（移行の読み出し元。新版は書き換えない）
    tts_speaker_main: 1,
    tts_speaker_sub: 2,
    tts_speakers_by_ghost: { mimi: { main: 1, sub: 2 }, poko: { main: 6, sub: 7 } },
    tts_speed: 1,
    tts_volume: 1,
    tts_irodori_use_real_model: false,
    // 既定（16）ではない値。フォームへの読み込みと保存の両方を通ったことが分かる（v0.5.7 項目 4）
    tts_irodori_steps: 40,
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
    voicevoxReady = false;
    loadedGhost = { id: "mimi", name: "ミミとクロ", defaultSpeakers: { main: 2, sub: 3 } };
    loadIndexHtml();
    vi.resetModules();
  });

  it("合成のステップ数は、開いたときに保存済みの値を出し、選び直した値で保存される", async () => {
    // v0.5.7 項目 4。読み込み（保存済みの 40 を出す）と保存（選び直した 8 を送る）の両方を見る。
    // 片方だけだと「画面の値を無視して元の値を送る」壊れ方を捕まえられない（元の値も 40 なので）。
    const panel = await import("../panels/settings");
    await panel.mountSettingsPanel(loadedGhost);
    await panel.openSettingsPanel();

    const steps = document.getElementById("settings-irodori-steps") as HTMLSelectElement;
    expect(steps.value, "保存済みの値を出す").toBe("40");

    steps.value = "8";
    document.getElementById("settings-save")!.dispatchEvent(new Event("click"));
    await vi.waitFor(() => expect(savedPayloads()).toHaveLength(1));
    expect(savedPayloads()[0].tts_irodori_steps, "選び直した値で保存される").toBe(8);
  });

  it("default_shell の追従が、2 回目の保存で取り消されない", async () => {
    const panel = await import("../panels/settings");
    await panel.mountSettingsPanel(loadedGhost);
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
      savedPayloads()[0].tts_irodori_steps,
      "合成のステップ数がフォームを通って保存される（既定の 16 に戻らない）",
    ).toBe(40);
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
    await panel.mountSettingsPanel(loadedGhost);
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

/// 操作列テスト（v0.6.0 項目 2）: 声はゴーストごと。保存のたびにフォーム全体から設定を組み立てるので、
/// **読み込み中のゴースト以外の行や map 全体が巻き戻らない**ことを、保存を 2 回流して確かめる。
///
/// 単機能テスト（「話者を選ぶと保存される」）では捕まらない: 壊れ方は「選んだゴーストの行は正しく書けるが、
/// ほかのゴーストの行を落とす / 旧版の単一の話者を書き換える / 声を決めていないゴーストに行を勝手に作る」。
describe("操作列: 声をゴーストごとに保存する", () => {
  const speakerMain = () => document.getElementById("settings-tts-speaker-main") as HTMLSelectElement;
  const speakerSub = () => document.getElementById("settings-tts-speaker-sub") as HTMLSelectElement;
  const voiceNote = () => document.getElementById("settings-voice-ghost-note") as HTMLElement;
  const save = async (count: number) => {
    document.getElementById("settings-save")!.dispatchEvent(new Event("click"));
    await vi.waitFor(() => expect(savedPayloads()).toHaveLength(count));
  };

  beforeEach(() => {
    invoked.length = 0;
    currentSettings = baseSettings();
    saveResponder = (next) => next;
    voicevoxReady = true;
    loadedGhost = { id: "mimi", name: "ミミとクロ", defaultSpeakers: { main: 2, sub: 3 } };
    loadIndexHtml();
    vi.resetModules();
  });

  async function open() {
    const panel = await import("../panels/settings");
    await panel.mountSettingsPanel(loadedGhost);
    await panel.openSettingsPanel();
    return panel;
  }

  it("一覧に無い保存済みの話者は見つからない話者と表示し、その ID のまま保存する", async () => {
    currentSettings.tts_speakers_by_ghost.mimi = { main: 999, sub: 998 };
    await open();
    expect(speakerMain().selectedOptions[0].textContent).toBe("#999 (見つからない話者)");
    expect(speakerSub().selectedOptions[0].textContent).toBe("#998 (見つからない話者)");
    await save(1);
    expect(savedPayloads()[0].tts_speakers_by_ghost.mimi).toEqual({ main: 999, sub: 998 });
  });

  it("一覧を取る前の話者は未取得と表示し、その ID のまま保存する", async () => {
    voicevoxReady = false;
    currentSettings.tts_speakers_by_ghost.mimi = { main: 999, sub: 998 };
    await open();
    expect(invoked.some((call) => call.cmd === "list_voices")).toBe(false);
    expect(speakerMain().selectedOptions[0].textContent).toBe("#999 (未取得)");
    expect(speakerSub().selectedOptions[0].textContent).toBe("#998 (未取得)");
    await save(1);
    expect(savedPayloads()[0].tts_speakers_by_ghost.mimi).toEqual({ main: 999, sub: 998 });
  });

  it("話者 ID 0 をメインとサブに選んで保存すると、元の話者に戻らず 0 を保存する", async () => {
    await open();
    speakerMain().value = "0";
    speakerSub().value = "0";
    await save(1);
    expect(savedPayloads()[0].tts_speakers_by_ghost.mimi).toEqual({ main: 0, sub: 0 });
    await save(2);
    expect(savedPayloads()[1].tts_speakers_by_ghost.mimi).toEqual({ main: 0, sub: 0 });
  });

  it("音量 0 を保存すると、元の音量に戻らず 0 を保存する（話者 ID 0 と同じ形）", async () => {
    await open();
    (document.getElementById("settings-tts-volume") as HTMLInputElement).value = "0";
    await save(1);
    expect(savedPayloads()[0].tts_volume).toBe(0);
  });

  it.each(["", "not-a-number"])("話者の選択値が空または数でない場合は保存済みの話者を保つ: %s", async (value) => {
    await open();
    for (const select of [speakerMain(), speakerSub()]) {
      const option = document.createElement("option");
      option.value = value;
      select.appendChild(option);
      select.value = value;
    }
    await save(1);
    expect(savedPayloads()[0].tts_speakers_by_ghost.mimi).toEqual({ main: 1, sub: 2 });
  });

  it("読み込み中のゴーストの行を出し、そこだけを書く。ほかのゴーストの行と旧版の話者は動かさない", async () => {
    await open();
    expect(speakerMain().value, "読み込み中のゴーストの話者を出す").toBe("1");
    expect(speakerSub().value).toBe("2");

    speakerMain().value = "9";
    await save(1);
    const first = savedPayloads()[0];
    expect(first.tts_speakers_by_ghost.mimi, "選んだ値で、読み込み中のゴーストの行だけが変わる").toEqual({
      main: 9,
      sub: 2,
    });
    expect(first.tts_speakers_by_ghost.poko, "ほかのゴーストの行は巻き戻らない").toEqual({
      main: 6,
      sub: 7,
    });
    expect(
      [first.tts_speaker_main, first.tts_speaker_sub],
      "旧版の単一の話者は書き換えない（旧版へ戻したとき移行前の話者で喋る）",
    ).toEqual([1, 2]);

    // 2 回目: 何も触らずもう一度保存しても、フォームが古い値を送り返して巻き戻さない
    await save(2);
    const second = savedPayloads()[1];
    expect(second.tts_speakers_by_ghost.mimi).toEqual({ main: 9, sub: 2 });
    expect(second.tts_speakers_by_ghost.poko).toEqual({ main: 6, sub: 7 });
  });

  it("ゴーストの選択を変えて保存すると、声の変更は読み込み中のゴースト（旧ゴースト）に入り、注記が出る", async () => {
    await open();
    expect(voiceNote().classList.contains("force-hide"), "選択が読み込み中と同じなら注記は出さない").toBe(true);

    const ghostSelect = document.getElementById("settings-ghost-id") as HTMLSelectElement;
    ghostSelect.value = "poko";
    ghostSelect.dispatchEvent(new Event("change"));
    expect(voiceNote().classList.contains("force-hide")).toBe(false);
    expect(voiceNote().textContent).toBe("再起動まで ミミとクロ の声を編集します");

    speakerSub().value = "8";
    await save(1);
    const saved = savedPayloads()[0];
    expect(saved.ghost_id).toBe("poko");
    expect(saved.tts_speakers_by_ghost.mimi, "変更は読み込み中の旧ゴーストに入る").toEqual({ main: 1, sub: 8 });
    expect(saved.tts_speakers_by_ghost.poko, "切り替え先の行は触らない").toEqual({ main: 6, sub: 7 });

    // 再保存しても、旧ゴーストの行が切り替え先の値で上書きされない
    await save(2);
    expect(savedPayloads()[1].tts_speakers_by_ghost.mimi).toEqual({ main: 1, sub: 8 });
    expect(savedPayloads()[1].tts_speakers_by_ghost.poko).toEqual({ main: 6, sub: 7 });
  });

  it("声を決めていないゴーストは、アプリの既定の話者を出し、関係ない設定の保存で行を作らない", async () => {
    loadedGhost = { id: "newcomer", name: "新入り", defaultSpeakers: { main: 2, sub: 3 } };
    currentSettings = { ...baseSettings(), ghost_id: "newcomer" };
    await open();
    expect(speakerMain().value, "行が無いゴーストは boot payload の既定（直書きではない）").toBe("2");
    expect(speakerSub().value).toBe("3");

    const volume = document.getElementById("settings-tts-volume") as HTMLInputElement;
    volume.value = "1.5";
    await save(1);
    const saved = savedPayloads()[0];
    expect(saved.tts_volume).toBe(1.5);
    expect(Object.keys(saved.tts_speakers_by_ghost).sort(), "声を決めていないのに行を作らない").toEqual([
      "mimi",
      "poko",
    ]);

    // 話者を選べば、そのゴーストの行ができる（ほかの行はそのまま）
    speakerMain().value = "8";
    await save(2);
    const second = savedPayloads()[1];
    expect(second.tts_speakers_by_ghost.newcomer).toEqual({ main: 8, sub: 3 });
    expect(second.tts_speakers_by_ghost.mimi).toEqual({ main: 1, sub: 2 });
    expect(second.tts_speakers_by_ghost.poko).toEqual({ main: 6, sub: 7 });
  });
});
