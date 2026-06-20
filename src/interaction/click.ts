import { startWindowDrag } from "./drag";

const CLICK_WINDOW_MS = 250;
const DRAG_THRESHOLD_PX = 4;

export interface ClickInfo {
  count: number;
  /// 最後の mousedown 座標 (CSS px、ビューポート基準)。部位判定で使う。
  x: number;
  y: number;
}

export type ClickHandler = (info: ClickInfo) => void;

interface ClickState {
  count: number;
  resolveTimer: number | null;
  downX: number;
  downY: number;
  pressed: boolean;
  dragged: boolean;
}

const state: ClickState = {
  count: 0,
  resolveTimer: null,
  downX: 0,
  downY: 0,
  pressed: false,
  dragged: false,
};

let handler: ClickHandler | null = null;

export function attachClickDetector(handle: ClickHandler): void {
  handler = handle;
  // mousedown は stage 上 (透過しない opaque な位置で発火)、move/up は window 全体で拾う
  // (ドラッグでカーソルがウインドウ外へ行く可能性に備えて)。
  document.addEventListener("mousedown", onMouseDown);
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
}

function onMouseDown(ev: MouseEvent): void {
  if (ev.button !== 0) return; // 左クリックのみ
  // 入力欄やボタン (solid) は素通り
  const target = ev.target as HTMLElement | null;
  if (target?.closest(".chat-input-target, input, button, textarea")) {
    return;
  }
  state.pressed = true;
  state.dragged = false;
  state.downX = ev.clientX;
  state.downY = ev.clientY;
}

function onMouseMove(ev: MouseEvent): void {
  if (!state.pressed || state.dragged) return;
  const dx = ev.clientX - state.downX;
  const dy = ev.clientY - state.downY;
  if (dx * dx + dy * dy >= DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX) {
    state.dragged = true;
    void startWindowDrag();
  }
}

function onMouseUp(): void {
  if (!state.pressed) return;
  state.pressed = false;
  if (state.dragged) {
    state.dragged = false;
    state.count = 0;
    if (state.resolveTimer !== null) {
      clearTimeout(state.resolveTimer);
      state.resolveTimer = null;
    }
    return;
  }
  state.count++;
  if (state.resolveTimer !== null) clearTimeout(state.resolveTimer);
  const x = state.downX;
  const y = state.downY;
  state.resolveTimer = window.setTimeout(() => {
    const count = state.count;
    state.count = 0;
    state.resolveTimer = null;
    handler?.({ count, x, y });
  }, CLICK_WINDOW_MS);
}
