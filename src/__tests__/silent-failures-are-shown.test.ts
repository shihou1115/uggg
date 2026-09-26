import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadIndexHtml } from "./dom";
import type { CalendarRefreshResult, ExportResult } from "../types";

/// 「静かに失敗していたものが画面に出る」（spec §6.0 項目 9、v0.5.7）。
///
/// エクスポートは、データが壊れたときに「まず控えて」と案内している手段で、壊れた部分を
/// 飛ばして読めた分だけ書き出す（v0.5.3）。ところが読めなかったテーブルはファイルの
/// `failed_tables` にしか載らず、画面は「保存しました」だけだった。**会話ログだけ落ちた
/// 控えを「全部控えた」と信じて元のデータを捨てる**経路が残る。
///
/// カレンダーの「いま取得」も同じ形で、URL が失効した取得元があっても件数だけを返し、
/// 「0 件の予定を取り込みました」と成功の見た目で出ていた（背景の取得を直した掃討で発見）。

let exportResult: ExportResult;
let refreshResult: CalendarRefreshResult;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock("../tts/speaker", () => ({ previewWavBase64: vi.fn() }));
vi.mock("../confirm", () => ({ uggConfirm: vi.fn(async () => true) }));
vi.mock("./chatlog", () => ({ openChatLog: vi.fn() }));
vi.mock("../weather/credit", () => ({ isWeatherReady: () => false }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    switch (cmd) {
      case "export_data":
        return exportResult;
      case "refresh_calendar":
        return refreshResult;
      default:
        return null;
    }
  }),
}));

const settle = () => new Promise((r) => setTimeout(r, 0));

async function click(buttonId: string) {
  const panel = await import("../panels/settings");
  await panel.mountSettingsPanel();
  document.getElementById(buttonId)!.dispatchEvent(new Event("click"));
  await settle();
}

describe("操作列: データを書き出す", () => {
  const message = () => document.getElementById("settings-data-message")!;

  beforeEach(() => {
    loadIndexHtml();
    vi.resetModules();
  });

  it("読めなかったものがあれば、名前と「このファイルには入っていない」を出す", async () => {
    exportResult = { path: "C:\\Downloads\\ugg-export-1.json", failed_tables: ["chat_log", "todos"] };
    await click("settings-data-export");

    const text = message().textContent ?? "";
    expect(text).toContain("C:\\Downloads\\ugg-export-1.json");
    expect(text, "取説と同じ呼び方で出す").toContain("会話履歴・ToDo");
    expect(text).toContain("このファイルには入っていません");
    expect(message().classList.contains("error"), "成功と同じ見た目にしない").toBe(true);
  });

  it("全部読めたら、保存先だけを出す", async () => {
    exportResult = { path: "C:\\Downloads\\ugg-export-2.json", failed_tables: [] };
    await click("settings-data-export");

    expect(message().textContent).toBe("保存しました: C:\\Downloads\\ugg-export-2.json");
    expect(message().classList.contains("error")).toBe(false);
  });
});

describe("操作列: カレンダーをいま取得する", () => {
  const message = () => document.getElementById("settings-calendar-message")!;

  beforeEach(() => {
    loadIndexHtml();
    vi.resetModules();
  });

  it("取れなかった取得元があれば、呼び名を出して成功の見た目にしない", async () => {
    refreshResult = { total: 4, failed: ["calendar.google.com"] };
    await click("settings-calendar-refresh");

    const text = message().textContent ?? "";
    expect(text).toContain("4 件の予定を取り込みました");
    expect(text).toContain("取れなかったもの: calendar.google.com");
    expect(message().classList.contains("error")).toBe(true);
  });

  it("全部取れたら、件数だけを出す", async () => {
    refreshResult = { total: 4, failed: [] };
    await click("settings-calendar-refresh");

    expect(message().textContent).toBe("4 件の予定を取り込みました");
    expect(message().classList.contains("error")).toBe(false);
  });
});
