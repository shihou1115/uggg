import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadIndexHtml } from "./dom";
import type { IrodoriStatus, Settings } from "../types";

/// 操作列テスト 5「古いランタイムが、古いと分かる形で見える」（spec §6.0 項目 4、v0.5.4）。
///
/// **この表示の要点は「更新あり」を出すことではなく、古いことを理由に
/// 「未導入」へ落とさないこと。** フロントは `canUseReal = gpuOk && assetsOk` で
/// 判定し、false のとき `tts_irodori_use_real_model` を黙って false へ倒して
/// **`set_settings` で永続化する**。ここで `assetsOk` に「最新か」を混ぜると、
/// 更新が届いていないだけのユーザーの設定が勝手に消える（spec §6.0 項目 2 の訂正）。
///
/// 単機能テスト（「未導入なら未導入と出る」）では捕まらない。**古いが動いている**
/// という中間状態を通さないと壊れない。
///
/// v0.5.4 の実装は当初まさにこの形で spec に書かれており、実装前に
/// `settings.ts:546-553` を辿って訂正した。表示の文言より、この副作用のほうが重い。

const invoked: { cmd: string; args: unknown }[] = [];
let currentSettings: Settings;
/// `get_irodori_status` が返す状態。各テストで差し替える。
let irodoriStatus: IrodoriStatus;
/// GPU が使えるか（`canUseReal` のもう一方の条件）。
let gpuAvailable: boolean;
/// `update_irodori_runtime` が呼ばれたあとに状態をどう変えるか。
let onUpdate: () => string[];
/// 確認ダイアログでユーザーが何と答えるか。
let confirmAnswer: boolean;

// `restoreMocks: true` なので `vi.fn().mockResolvedValue(...)` は 1 本目のテストで
// 実装ごと消える（2 本目以降 undefined が返り、`unlisten()` が落ちる）。
// **実装を渡した `vi.fn(impl)` にすること**（reset は impl に戻す）。
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock("../tts/speaker", () => ({ previewWavBase64: vi.fn() }));
// `restoreMocks: true` なので `vi.fn().mockResolvedValue(...)` は 1 本目のテストで
// 実装ごと消える。**実装を渡した `vi.fn(impl)` にすること**（reset で impl に戻る）。
vi.mock("../confirm", () => ({ uggConfirm: vi.fn(async () => confirmAnswer) }));
vi.mock("./chatlog", () => ({ openChatLog: vi.fn() }));
vi.mock("../weather/credit", () => ({ isWeatherReady: () => false }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: unknown) => {
    invoked.push({ cmd, args });
    switch (cmd) {
      case "get_settings":
        return currentSettings;
      case "set_settings": {
        currentSettings = (args as { settings: Settings }).settings;
        return currentSettings;
      }
      case "get_irodori_status":
        return irodoriStatus;
      case "irodori_check_gpu":
        return gpuAvailable
          ? { available: true, name: "テスト GPU", reason: null }
          : { available: false, name: null, reason: "テスト環境" };
      case "update_irodori_runtime":
        return onUpdate();
      case "list_ghosts":
        return [{ id: "mimi", name: "ミミとクロ" }];
      case "list_shells":
        return [{ id: "default", name: "既定シェル" }];
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
      case "list_monitors":
        return { monitors: [], current: null };
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
    tts_enabled: true,
    tts_engine: "irodori",
    tts_speaker_main: 1,
    tts_speaker_sub: 2,
    tts_speed: 1,
    tts_volume: 1,
    // **更新が届いていないだけのユーザー**は、実モデルを使う設定のまま
    tts_irodori_use_real_model: true,
    tts_irodori_steps: 16,
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

function status(over: Partial<IrodoriStatus>): IrodoriStatus {
  return {
    present: true,
    has_record: false,
    up_to_date: false,
    outdated: [],
    resolved: {},
    ...over,
  };
}

const stateText = () => document.getElementById("settings-irodori-assets-state")!.textContent;
const updateBtn = () =>
  document.getElementById("settings-irodori-update") as HTMLButtonElement;
const useReal = () =>
  document.getElementById("settings-irodori-use-real-model") as HTMLInputElement;
const savedRealModelValues = () =>
  invoked
    .filter((i) => i.cmd === "set_settings")
    .map((i) => (i.args as { settings: Settings }).settings.tts_irodori_use_real_model);

async function open() {
  const panel = await import("../panels/settings");
  await panel.mountSettingsPanel();
  await panel.openSettingsPanel();
  return panel;
}

describe("操作列: 更新が届いていないランタイムを開く", () => {
  beforeEach(() => {
    invoked.length = 0;
    currentSettings = baseSettings();
    gpuAvailable = true;
    irodoriStatus = status({ present: true, up_to_date: false });
    onUpdate = () => ["dacvae"];
    confirmAnswer = true;
    loadIndexHtml();
    vi.resetModules();
  });

  it("古いだけのランタイムを「未導入」に落とさず、実モデルの設定も消さない", async () => {
    await open();

    expect(stateText(), "古いことが分かる形で出ること").toBe("導入済み (更新あり)");
    expect(updateBtn().hidden, "更新の導線が出ること").toBe(false);

    // ここが本体。古い = 使えない ではない。
    expect(useReal().disabled, "古くても実モデルは使えるので触れること").toBe(false);
    expect(useReal().checked, "ユーザーの選択が残ること").toBe(true);
    expect(
      savedRealModelValues(),
      "古いことを理由に設定を false で上書き保存してはいけない",
    ).not.toContain(false);
    expect(currentSettings.tts_irodori_use_real_model).toBe(true);
  });

  it("最新なら更新の導線を出さない", async () => {
    irodoriStatus = status({ present: true, has_record: true, up_to_date: true });
    await open();

    expect(stateText()).toBe("導入済み");
    expect(updateBtn().hidden, "最新なのに更新を促さない").toBe(true);
  });

  it("未導入は未導入と出し、更新ではなく導入へ誘導する", async () => {
    irodoriStatus = status({ present: false, up_to_date: false });
    await open();

    expect(stateText()).toBe("未導入");
    expect(updateBtn().hidden, "入っていないものは更新できない").toBe(true);
  });

  it("更新すると表示が最新へ変わり、導線が消える", async () => {
    irodoriStatus = status({
      present: true,
      up_to_date: false,
      outdated: ["dacvae", "irodori_tts"],
    });
    await open();
    expect(updateBtn().hidden).toBe(false);

    // 押したら、バックが入れ直したうえで状態が最新になる
    onUpdate = () => {
      irodoriStatus = status({ present: true, has_record: true, up_to_date: true });
      return ["dacvae", "irodori_tts"];
    };
    updateBtn().dispatchEvent(new Event("click"));

    await vi.waitFor(() => expect(stateText()).toBe("導入済み"));
    expect(
      updateBtn().hidden,
      "更新後に状態を取り直さないと、押せる更新ボタンが残り続ける",
    ).toBe(true);
    expect(invoked.some((i) => i.cmd === "update_irodori_runtime")).toBe(true);
  });

  it("導入済みで「ランタイムをダウンロード」を押すと、全段の入れ直しではなく更新になる", async () => {
    // v0.5.7 項目 1。全段の入れ直しには版の控えも全戻しも合成の確認も無く、依存の major 移行
    // （transformers 4→5）が失敗しても戻らない。導入済みなら更新（1 つのトランザクション）へ回す。
    const settle = () => new Promise((r) => setTimeout(r, 0));
    const called = (cmd: string) => invoked.some((i) => i.cmd === cmd);
    const downloadBtn = () =>
      document.getElementById("settings-irodori-download") as HTMLButtonElement;

    irodoriStatus = status({ present: true, has_record: true, up_to_date: false, outdated: ["transformers"] });
    await open();
    downloadBtn().dispatchEvent(new Event("click"));
    await settle();
    expect(called("update_irodori_runtime"), "導入済みなら更新の経路へ").toBe(true);
    expect(called("download_irodori_assets"), "全段の入れ直しはしない").toBe(false);

    // 同じ待ち方で、未導入なら全段の導入が始まる = 上の false は「早すぎた」からではない
    invoked.length = 0;
    irodoriStatus = status({ present: false, up_to_date: false });
    vi.resetModules();
    loadIndexHtml();
    await open();
    const confirm = vi.mocked((await import("../confirm")).uggConfirm);
    confirm.mockClear();
    downloadBtn().dispatchEvent(new Event("click"));
    await settle();
    expect(called("download_irodori_assets"), "未導入なら導入する").toBe(true);
    expect(called("update_irodori_runtime")).toBe(false);
    // v0.5.7 項目 8: 初回導入の確認で、取得量（正本と同じ数字）と倫理条項を出す
    const message = String(confirm.mock.calls[0][0]);
    expect(message).toContain("約 7 GB");
    expect(message).toContain("約 9 GB");
    expect(message).toContain("倫理条項");
  });

  it("AI モデルが変わる更新だけ、声が変わることと古いモデルを消すことを伝え、倫理条項はいつも出す", async () => {
    // v0.5.7 項目 8。パッケージだけの更新で「声が変わる」「モデルを消す」と言うと事実と違う。
    const confirm = vi.mocked((await import("../confirm")).uggConfirm);
    const settle = () => new Promise((r) => setTimeout(r, 0));
    const messageOf = async (outdated: string[]) => {
      irodoriStatus = status({ present: true, has_record: true, up_to_date: false, outdated });
      confirmAnswer = false;
      invoked.length = 0;
      confirm.mockClear();
      vi.resetModules();
      loadIndexHtml();
      await open();
      updateBtn().dispatchEvent(new Event("click"));
      await settle();
      expect(confirm).toHaveBeenCalledTimes(1);
      return String(confirm.mock.calls[0][0]);
    };
    const withModel = await messageOf(["model_synth", "transformers"]);
    expect(withModel).toContain("声の質感が変わります");
    expect(withModel).toContain("古いモデルは消します");
    expect(withModel).toContain("約 3.6 GB");
    expect(withModel).toContain("倫理条項");
    const packagesOnly = await messageOf(["transformers"]);
    expect(packagesOnly).not.toContain("声の質感");
    expect(packagesOnly).not.toContain("古いモデル");
    expect(packagesOnly).toContain("倫理条項");
  });

  it("確認で断ったら、入れ直しを始めない", async () => {
    // **「起きないこと」は待っても観測できない。** `vi.waitFor` に否定を渡すと
    // 1 回目の判定で通ってしまい、同意を無視する変異を素通りする
    // （実際にそれで MUT16「同意を取らずに入れ直す」を検出できなかった）。
    // そこで**同じ待ち方で肯定側が観測できること**を同じテストで示し、
    // 観測点が早すぎないことをテスト自身に証明させる。
    const settle = () => new Promise((r) => setTimeout(r, 0));
    const startedUpdate = () => invoked.some((i) => i.cmd === "update_irodori_runtime");

    confirmAnswer = false;
    await open();
    updateBtn().dispatchEvent(new Event("click"));
    await settle();
    expect(startedUpdate(), "同意なしに環境を書き換えてはいけない").toBe(false);
    expect(stateText(), "断ったので状態も変わらない").toBe("導入済み (更新あり)");

    // 同じ待ち方で、同意すれば確かに始まる = 上の false は「早すぎた」からではない
    confirmAnswer = true;
    updateBtn().dispatchEvent(new Event("click"));
    await settle();
    expect(startedUpdate(), "この待ち方で肯定側は観測できること").toBe(true);
  });
});
