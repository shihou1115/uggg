import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadIndexHtml } from "./dom";

/// VOICEVOX のクレジットは、**設定のエンジンではなく、実際に VOICEVOX で鳴らしたか**で出す
/// （v0.6.0 項目 2・3、spec §6.0）。以前は高品質モードだとクレジットを消したので、参照音声の無い slot を
/// VOICEVOX で喋らせ続けると、規約上の常時表示が欠けた。
///
/// 合成の結果（`synthesize_voice` の戻りの `engine`）を `noteSynthesized` で受け、高品質モードのときも
/// VOICEVOX で鳴らした slot だけ話者名を出す。

const voices = [
  { id: 2, name: "四国めたん (ノーマル)" },
  { id: 3, name: "ずんだもん (ノーマル)" },
  { id: 8, name: "春日部つむぎ (ノーマル)" },
];
const speakers = { main: 2, sub: 3 };

let listVoices: ReturnType<typeof vi.fn>;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((cmd: string) => listVoices(cmd)),
}));

const settle = () => new Promise((r) => setTimeout(r, 0));
const credit = () => document.getElementById("tts-credit")!;
const shown = () => credit().classList.contains("visible");

async function mount() {
  const mod = await import("../tts/credit");
  mod.mountCredit();
  return mod;
}

describe("操作列: クレジットは実際に鳴らしたエンジンで出る", () => {
  beforeEach(() => {
    loadIndexHtml();
    vi.resetModules();
    listVoices = vi.fn(async (cmd: string) => {
      if (cmd === "list_voices") return voices;
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("VOICEVOX が設定のエンジンなら、両方の話者を出す", async () => {
    const c = await mount();
    await c.refreshCredit(true, "voicevox_core", speakers);
    expect(shown()).toBe(true);
    expect(credit().textContent).toBe("VOICEVOX:四国めたん / ずんだもん");
  });

  it("高品質モードで Irodori が鳴っている間は出さない（Irodori にクレジットの定めは無い）", async () => {
    const c = await mount();
    await c.refreshCredit(true, "irodori", speakers);
    expect(shown()).toBe(false);
    c.noteSynthesized("main", "irodori");
    await settle();
    expect(shown()).toBe(false);
    expect(listVoices, "出さないのに話者の一覧を取りに行かない").not.toHaveBeenCalled();
  });

  it("高品質モードでも、VOICEVOX で鳴らした slot（参照音声が無い・フォールバック）は話者名を出す", async () => {
    const c = await mount();
    await c.refreshCredit(true, "irodori", speakers);
    c.noteSynthesized("main", "voicevox_core");
    await settle();
    expect(shown()).toBe(true);
    expect(credit().textContent, "鳴らした slot の話者だけ").toBe("VOICEVOX:四国めたん");

    c.noteSynthesized("sub", "voicevox_core");
    await settle();
    expect(credit().textContent).toBe("VOICEVOX:四国めたん / ずんだもん");

    // main が高品質の声で鳴るようになったら（参照音声を作った）、main の分は外れる
    c.noteSynthesized("main", "irodori");
    await settle();
    expect(credit().textContent).toBe("VOICEVOX:ずんだもん");
    c.noteSynthesized("sub", "irodori");
    await settle();
    expect(shown(), "VOICEVOX が鳴らなくなったら消える").toBe(false);
    expect(credit().textContent).toBe("");
  });

  it("話者は読み込み中のゴーストのもの（保存で別の話者に替えたら追従する）", async () => {
    const c = await mount();
    await c.refreshCredit(true, "irodori", speakers);
    c.noteSynthesized("sub", "voicevox_core");
    await settle();
    expect(credit().textContent).toBe("VOICEVOX:ずんだもん");
    await c.refreshCredit(true, "irodori", { main: 2, sub: 8 });
    expect(credit().textContent, "鳴らしたことは保存で消えない").toBe("VOICEVOX:春日部つむぎ");
  });

  it("エンジンを替えたら、前のエンジンで見た「VOICEVOX で鳴らした」は引き継がない", async () => {
    const c = await mount();
    await c.refreshCredit(true, "voicevox_core", speakers);
    c.noteSynthesized("main", "voicevox_core");
    await c.refreshCredit(true, "irodori", speakers);
    expect(shown(), "高品質モードへ替えた直後に、古い VOICEVOX のクレジットが残る").toBe(false);
  });

  it("声が無効なら、VOICEVOX で鳴らしていても消える", async () => {
    const c = await mount();
    await c.refreshCredit(true, "irodori", speakers);
    c.noteSynthesized("main", "voicevox_core");
    await settle();
    expect(shown()).toBe(true);
    await c.refreshCredit(false, "irodori", speakers);
    expect(shown()).toBe(false);
  });

  it("話者の一覧が取れなければ（資産未 DL）隠す", async () => {
    listVoices = vi.fn(async () => {
      throw new Error("voicevox 資産が未ダウンロードです");
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const c = await mount();
    await c.refreshCredit(true, "voicevox_core", speakers);
    expect(shown()).toBe(false);
    warn.mockRestore();
  });

  it("同じ話者は 1 つにまとめる", async () => {
    const c = await mount();
    await c.refreshCredit(true, "voicevox_core", { main: 2, sub: 2 });
    expect(credit().textContent).toBe("VOICEVOX:四国めたん");
  });

  it("出す slot は声の有効・エンジン・鳴らした slot で決まる（純粋部分）", async () => {
    const { creditSlots } = await import("../tts/credit");
    const none = { main: false, sub: false };
    expect(creditSlots(false, "voicevox_core", none)).toEqual([]);
    expect(creditSlots(true, "voicevox_core", none)).toEqual(["main", "sub"]);
    expect(creditSlots(true, "irodori", none)).toEqual([]);
    expect(creditSlots(true, "irodori", { main: false, sub: true })).toEqual(["sub"]);
    expect(creditSlots(false, "irodori", { main: true, sub: true })).toEqual([]);
  });
});

describe("配線: 発話は合成の結果のエンジンをクレジットへ渡す", () => {
  beforeEach(() => {
    loadIndexHtml();
    vi.resetModules();
  });

  it("speaker は synthesize_voice の戻りの engine を渡し、wav だけを再生へ回す", async () => {
    const note = vi.fn();
    vi.doMock("../tts/credit", () => ({ noteSynthesized: note }));
    vi.doMock("@tauri-apps/api/core", () => ({
      invoke: vi.fn(async (cmd: string) => {
        expect(cmd).toBe("synthesize_voice");
        return { wav: "AAAA", engine: "voicevox_core" };
      }),
    }));
    vi.spyOn(console, "warn").mockImplementation(() => undefined); // happy-dom に AudioContext が無い
    const speaker = await import("../tts/speaker");
    speaker.setTtsParams({ enabled: true, speed: 1, volume: 1 });
    await speaker.createSpeaker().speak("sub", "こんにちは");
    expect(note).toHaveBeenCalledWith("sub", "voicevox_core");
  });

  it("speaker は合成が失敗しても例外にせず、クレジットへは何も渡さない（声なしで続ける）", async () => {
    const note = vi.fn();
    vi.doMock("../tts/credit", () => ({ noteSynthesized: note }));
    vi.doMock("@tauri-apps/api/core", () => ({
      invoke: vi.fn(async () => {
        throw new Error("synth failed");
      }),
    }));
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const speaker = await import("../tts/speaker");
    speaker.setTtsParams({ enabled: true, speed: 1, volume: 1 });
    await speaker.createSpeaker().speak("main", "こんにちは");
    expect(note).not.toHaveBeenCalled();
  });
});
