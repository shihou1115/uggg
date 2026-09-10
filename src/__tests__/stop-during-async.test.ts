import { beforeEach, describe, expect, it, vi } from "vitest";

/// 操作列テスト 4/4「非同期処理途中の停止」（spec §6.0 項目 9、v0.5.3）。
///
/// 合成 → デコード → 再生 のうち、**デコードを待っている最中に停止する**列。
/// v0.5.2 までは合成後にしか世代を見ておらず、デコード中に停止すると
/// `currentSource` がまだ null なので `stopAll` の停止対象も無く、
/// **停止したあとに音と口パクが始まっていた**。
///
/// 単機能テスト（「停止したら currentSource.stop() が呼ばれる」）では捕まらない。
/// 止める対象がまだ存在しない瞬間が問題なので、順番に流さないと再現しない。

const invokeMock = vi.fn();
const attachMouthMock = vi.fn();
const stopMouthMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock("../tts/mouth", () => ({
  attachMouth: (...args: unknown[]) => attachMouthMock(...args),
  stopMouth: (...args: unknown[]) => stopMouthMock(...args),
}));

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (v: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

class FakeSource {
  buffer: unknown = null;
  playbackRate = { value: 1 };
  onended: (() => void) | null = null;
  started = false;
  stopped = false;
  connect(next: unknown): unknown {
    return next;
  }
  start(): void {
    this.started = true;
    // 実機の onended は再生完了で鳴る。テストでは即座に終わったことにする。
    queueMicrotask(() => this.onended?.());
  }
  stop(): void {
    this.stopped = true;
  }
}

/// 生成された BufferSource を全部覚えておく。**1 本も作られないこと**を検査したいので、
/// 「start されたか」ではなく「作られたか」から見る。
const sources: FakeSource[] = [];
/// decodeAudioData の解決を握る。これを resolve しない限り再生段へ進まない。
let pendingDecode: Deferred<unknown> | null = null;

class FakeAudioContext {
  state = "running";
  destination = {};
  resume(): Promise<void> {
    return Promise.resolve();
  }
  decodeAudioData(): Promise<unknown> {
    pendingDecode = deferred<unknown>();
    return pendingDecode.promise;
  }
  createBufferSource(): FakeSource {
    const s = new FakeSource();
    sources.push(s);
    return s;
  }
  createGain(): unknown {
    return { gain: { value: 1 }, connect: (next: unknown) => next };
  }
}

/// マイクロタスクを数回流す（await が何段か挟まるため）。
async function settle(times = 5): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

describe("操作列: 合成 → デコード中に停止", () => {
  beforeEach(() => {
    sources.length = 0;
    pendingDecode = null;
    invokeMock.mockReset();
    attachMouthMock.mockReset();
    stopMouthMock.mockReset();
    attachMouthMock.mockImplementation(() => ({
      connect: () => ({ connect: () => undefined }),
    }));
    (globalThis as unknown as { AudioContext: unknown }).AudioContext = FakeAudioContext;
    vi.resetModules();
  });

  it("デコードを待っている間に停止したら、そのあと再生も口パクも始まらない", async () => {
    const { createSpeaker, setTtsParams } = await import("../tts/speaker");
    setTtsParams({ enabled: true, speed: 1, volume: 1 });
    const speaker = createSpeaker();

    // 1. 合成は成功して WAV が返る
    invokeMock.mockResolvedValue("AAAA");
    const spoken = speaker.speak("main", "こんにちは");
    await settle();
    expect(pendingDecode, "デコード待ちに入っていること").not.toBeNull();
    expect(sources).toHaveLength(0);

    // 2. デコードの最中に停止する（この時点で止める対象はまだ存在しない）
    speaker.interrupt();

    // 3. そのあとデコードが完了する
    pendingDecode!.resolve({});
    await settle();

    expect(sources, "停止後に再生を始めてはいけない").toHaveLength(0);
    expect(attachMouthMock, "停止後に口を動かしてはいけない").not.toHaveBeenCalled();
    // speak() の Promise は必ず解放される（呼び出し側が固まらないこと）
    await expect(spoken).resolves.toBeUndefined();
  });

  it("停止しなければ、デコード完了後に再生と口パクが始まる", async () => {
    const { createSpeaker, setTtsParams } = await import("../tts/speaker");
    setTtsParams({ enabled: true, speed: 1, volume: 1 });
    const speaker = createSpeaker();

    invokeMock.mockResolvedValue("AAAA");
    const spoken = speaker.speak("main", "こんにちは");
    await settle();
    pendingDecode!.resolve({});
    await settle();

    expect(sources, "1 本だけ再生される").toHaveLength(1);
    expect(sources[0].started).toBe(true);
    expect(attachMouthMock).toHaveBeenCalledTimes(1);
    await expect(spoken).resolves.toBeUndefined();
  });

  it("音量 0 では口を動かさない（無音なのに喋って見えない）", async () => {
    const { createSpeaker, setTtsParams } = await import("../tts/speaker");
    setTtsParams({ enabled: true, speed: 1, volume: 0 });
    const speaker = createSpeaker();

    invokeMock.mockResolvedValue("AAAA");
    const spoken = speaker.speak("main", "こんにちは");
    await settle();
    pendingDecode!.resolve({});
    await settle();

    expect(sources).toHaveLength(1);
    expect(attachMouthMock).not.toHaveBeenCalled();
    await expect(spoken).resolves.toBeUndefined();
  });
});
