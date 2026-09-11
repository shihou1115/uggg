import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadIndexHtml } from "./dom";
import type { DialogueResponse } from "../types";
import type { TypewriterToken } from "../dialogue/typewriter";

/// 操作列テスト 3/4「連続通知」（spec §6.0 項目 9、v0.5.3）。
///
/// 通知は「一度出したら配達済みの記録が残り、二度と出ない」。だから消されると
/// **記録だけが残って永久に届かない**。v0.5.2 まで起きていたのは 2 つ:
///   - 同じ tick で 2 件のカレンダー通知が配達されると、後の 1 件が前を消す
///   - 80% コスト警告を emit した直後にチャット応答が返ると、応答が警告を消す
///
/// 単機能テスト（「1 件の通知が表示される」）では捕まらない。**2 件目が来る**、
/// **応答が割り込む**という順番があって初めて壊れる。

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// `restoreMocks: true` なので `vi.fn().mockResolvedValue(...)` は 1 本目のテストで
// 実装ごと消える（2 本目以降 undefined が返り、`unlisten()` が落ちる）。
// **実装を渡した `vi.fn(impl)` にすること**（reset は impl に戻す）。
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock("../stage/character", () => ({ setPose: vi.fn() }));

/// **最後まで表示された**本文を順に記録する。
const shown: string[] = [];

/// 描画には必ず時間がかかる（実機のタイプライター描画に相当）。ここを即完了に
/// すると「打ち切られた発話」も表示済みとして数えてしまい、消えたことが観測
/// できなくなる。**打ち切られた分は `shown` に入らない**のがこのテストの肝。
const TYPE_MS = 10;

vi.mock("../dialogue/typewriter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../dialogue/typewriter")>();
  return {
    ...actual,
    typeInto: vi.fn(
      async (el: HTMLElement, text: string, _speed: unknown, token: TypewriterToken) => {
        await new Promise((r) => setTimeout(r, TYPE_MS));
        if (token.cancelled) return; // 途中で消された = ユーザーは読めていない
        el.textContent = text;
        shown.push(text);
      },
    ),
  };
});

function base(text: string): DialogueResponse {
  return { kind: "event", mode: "low", pattern: 1, main: { text, pose: null }, sub: null };
}

/// notify() 経由の告知（コスト警告・降格・DL 完了など）。
function systemNotice(text: string): DialogueResponse {
  return { ...base(text), kind: "system_message" };
}

/// deliver_event の Notice（リマインダー・カレンダー）。
function calendarNotice(text: string): DialogueResponse {
  return { ...base(text), priority: "notice" };
}

function reply(text: string): DialogueResponse {
  return { ...base(text), kind: "reply", mode: "advanced" };
}

/// 状況発話・独り言。消えても記録が残らず、また出る＝守る対象ではない。
function ambient(text: string): DialogueResponse {
  return { ...base(text), priority: "ambient" };
}

/// 表示保持（holdDuration、最大 12 秒）まで含めて描画を最後まで進める。
async function runOut(): Promise<void> {
  await vi.advanceTimersByTimeAsync(30_000);
}

/// 描画を「始まったが、まだ終わっていない」ところで止める。
async function midRender(): Promise<void> {
  await vi.advanceTimersByTimeAsync(TYPE_MS / 2);
}

describe("操作列: 通知が後続の発話に消されない", () => {
  beforeEach(() => {
    shown.length = 0;
    loadIndexHtml();
    vi.resetModules();
    vi.useFakeTimers();
  });

  it("同じ tick で 2 件の通知が来たら、両方とも最後まで表示される", async () => {
    const { renderResponse } = await import("../system/ghost-speech");

    void renderResponse(calendarNotice("10 分後に打ち合わせ"));
    void renderResponse(calendarNotice("15 分後に歯医者"));
    await runOut();

    expect(shown).toEqual(["10 分後に打ち合わせ", "15 分後に歯医者"]);
  });

  it("通知の描画中に応答が割り込んだら、応答を先に出してから通知を出し直す", async () => {
    const { renderResponse } = await import("../system/ghost-speech");

    // 1. コスト警告が出はじめる（まだ読み終えていない）
    void renderResponse(systemNotice("今月のぶん、8 割使ったって"));
    await midRender();
    expect(shown, "まだ読み終えていない").toEqual([]);

    // 2. 直後にチャット応答が返る。ユーザーの入力は待たせないので割り込みは許す
    void renderResponse(reply("おかえり!"));
    await runOut();

    // 3. 応答のあとで、消された通知が出し直される
    expect(shown).toEqual(["おかえり!", "今月のぶん、8 割使ったって"]);
  });

  it("通知は割り込まない: 応答の描画中に来た通知は、応答が終わってから出る", async () => {
    const { renderResponse } = await import("../system/ghost-speech");

    void renderResponse(reply("ただいま、って言った?"));
    await midRender();
    void renderResponse(systemNotice("新しいバージョンが出てるよ"));
    await runOut();

    expect(shown).toEqual(["ただいま、って言った?", "新しいバージョンが出てるよ"]);
  });

  it("Ambient（状況発話・独り言）は守らない: 従来どおり後続に消される", async () => {
    const { renderResponse } = await import("../system/ghost-speech");

    void renderResponse(ambient("そろそろ休憩しない?"));
    await midRender();
    void renderResponse(reply("うん、そうする"));
    await runOut();

    expect(
      shown,
      "また出るものまで守ると、ユーザーの操作を待たせるだけになる",
    ).toEqual(["うん、そうする"]);
  });
});
