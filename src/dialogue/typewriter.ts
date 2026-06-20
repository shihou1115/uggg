import type { TalkSpeed } from "../types";

const SPEED_MS: Record<TalkSpeed, number> = {
  slow: 80,
  normal: 35,
  fast: 12,
  instant: 0,
};

export interface TypewriterToken {
  cancelled: boolean;
}

export function newToken(): TypewriterToken {
  return { cancelled: false };
}

/// `el.textContent` を 1 文字ずつ書き換える。
/// `token.cancelled` が true になった時点で即抜けるが、textContent はその場の状態で残る
/// (呼び出し側で hide するか上書きする)。
export async function typeInto(
  el: HTMLElement,
  text: string,
  speed: TalkSpeed,
  token: TypewriterToken,
  onTick?: () => void,
): Promise<void> {
  const interval = SPEED_MS[speed] ?? SPEED_MS.normal;
  if (interval === 0 || text.length === 0) {
    el.textContent = text;
    onTick?.();
    return;
  }
  el.textContent = "";
  for (let i = 1; i <= text.length; i++) {
    if (token.cancelled) return;
    el.textContent = text.slice(0, i);
    onTick?.();
    await sleep(interval);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
