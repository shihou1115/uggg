import type { SlotName } from "../types";

interface BalloonView {
  root: HTMLElement;
  textEl: HTMLElement;
  /// バルーン内メニュー (spec §4.3.5) のコンテナ。balloon-main のみ持つ。
  menuEl: HTMLElement | null;
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
  const menuEl = root.querySelector<HTMLElement>(".balloon-menu");
  const view = { root, textEl, menuEl };
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
/// バルーン内メニューの残骸は新しい発話のたびに掃除する (メニューは発話で置き換わる仕様)。
export function showBalloon(slot: SlotName): HTMLElement {
  const view = ensureView(slot);
  view.textEl.textContent = "";
  if (view.menuEl) view.menuEl.innerHTML = "";
  view.root.classList.add("visible");
  reposition(slot);
  return view.textEl;
}

/// バルーン内メニュー (spec §4.3.5) のコンテナを返す。無い slot は null。
export function balloonMenuContainer(slot: SlotName): HTMLElement | null {
  return ensureView(slot).menuEl;
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

/// キャラ左端と吹き出し右端の間隔 (しっぽ 8px を含む)。入力欄の配置計算も共有する。
export const GAP_X = 24;
/// 吹き出し上端をキャラ上端からどれだけ下げるか (キャラ高さ比)。顔の横に来る。
export const HEAD_RATIO = 0.12;
/// ウインドウ端との最小余白。
export const MARGIN = 8;

/// 吹き出しをキャラの左横に配置する (伺か風)。
/// - 横: キャラ左端から GAP_X 空けて右端を合わせる。キャラが画面左端付近で
///   収まらない場合はキャラの右横へ反転 (.flip、しっぽも反転。spec §4.1.3)
/// - 縦: キャラ上端 + キャラ高さ × HEAD_RATIO (顔の高さ)
/// - 相方の吹き出しと重なる場合は main を上へ・sub を下へ退避
export function reposition(slot: SlotName): void {
  const view = views.get(slot);
  if (!view) return;
  const char = document.getElementById(`char-${slot}`);
  if (!char) return;
  const rect = char.getBoundingClientRect();
  const w = view.root.offsetWidth || 200;
  const h = view.root.offsetHeight || 60;
  const winW = window.innerWidth;
  const winH = window.innerHeight;

  let left = Math.round(rect.left - GAP_X - w);
  let flip = false;
  if (left < MARGIN) {
    const rightSide = Math.round(rect.right + GAP_X);
    if (rightSide + w <= winW - MARGIN) {
      left = rightSide;
      flip = true;
    } else {
      left = MARGIN; // 両側とも収まらない極端ケースは左置きで clamp
    }
  }
  view.root.classList.toggle("flip", flip);

  let top = Math.round(rect.top + rect.height * HEAD_RATIO);

  const other = views.get(slot === "main" ? "sub" : "main");
  if (other && other.root.classList.contains("visible")) {
    const o = other.root.getBoundingClientRect();
    const overlaps =
      left < o.right && left + w > o.left && top < o.bottom && top + h > o.top;
    if (overlaps) {
      top = slot === "main" ? Math.round(o.top - h - MARGIN) : Math.round(o.bottom + MARGIN);
    }
  }

  if (top + h > winH - MARGIN) top = winH - MARGIN - h;
  if (top < MARGIN) top = MARGIN;
  view.root.style.left = `${left}px`;
  view.root.style.top = `${top}px`;
}
