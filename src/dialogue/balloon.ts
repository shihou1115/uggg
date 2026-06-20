import type { SlotName } from "../types";

interface BalloonView {
  root: HTMLElement;
  textEl: HTMLElement;
}

const views = new Map<SlotName, BalloonView>();

/// index.html に静的配置された `#balloon-main` / `#balloon-sub` を取得する。
/// 動的 createElement は WebView2 透過レイヤーで描画されないバグがあるため使わない。
function ensureView(slot: SlotName): BalloonView {
  const cached = views.get(slot);
  if (cached) return cached;
  const root = document.getElementById(`balloon-${slot}`);
  if (!root) {
    throw new Error(`balloon-${slot} DOM が見つかりません（index.html 参照）`);
  }
  const textEl = root.querySelector<HTMLElement>(".balloon-text");
  if (!textEl) {
    throw new Error(`balloon-${slot} に .balloon-text 要素がありません`);
  }
  const view = { root, textEl };
  views.set(slot, view);
  return view;
}

/// 起動時に呼び出して両 slot の View を取得しキャッシュする。
/// 取得失敗時に boot エラーとして表に出すための事前確認。
export function preallocateBalloons(): void {
  ensureView("main");
  ensureView("sub");
}

/// 吹き出しを表示状態にして、テキスト書き込み用の `.balloon-text` 要素を返す。
/// 位置決めは reposition() を別途呼ぶ。
export function showBalloon(slot: SlotName): HTMLElement {
  const view = ensureView(slot);
  view.textEl.textContent = "";
  view.root.classList.add("visible");
  reposition(slot);
  return view.textEl;
}

export function hideBalloon(slot: SlotName): void {
  const view = views.get(slot);
  if (!view) return;
  view.root.classList.remove("visible");
}

export function hideAllBalloons(): void {
  for (const slot of views.keys()) {
    hideBalloon(slot);
  }
}

/// 吹き出しをキャラの真上に配置する。
export function reposition(slot: SlotName): void {
  const view = views.get(slot);
  if (!view) return;
  const char = document.getElementById(`char-${slot}`);
  if (!char) return;
  const rect = char.getBoundingClientRect();
  const margin = 12;
  const w = view.root.offsetWidth || 200;
  const h = view.root.offsetHeight || 60;
  const centerX = rect.left + rect.width / 2;
  let left = Math.round(centerX - w / 2);
  let top = Math.round(rect.top - h - margin);
  const winW = window.innerWidth;
  if (left < margin) left = margin;
  if (left + w > winW - margin) left = Math.max(margin, winW - margin - w);
  if (top < margin) top = margin;
  view.root.style.left = `${left}px`;
  view.root.style.top = `${top}px`;
}
