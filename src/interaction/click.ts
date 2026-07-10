import { hitTest } from "../stage/character";
import { moveCharBy, persistCharPositions } from "../stage/charpos";
import type { SlotName } from "../types";

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
  /// mousedown がヒットしたキャラ。ドラッグ時の移動対象 (spec §4.3.4)。キャラ外は null。
  slot: SlotName | null;
  /// ドラッグ差分適用用: 直前 mousemove の X。
  lastX: number;
}

const state: ClickState = {
  count: 0,
  resolveTimer: null,
  downX: 0,
  downY: 0,
  pressed: false,
  dragged: false,
  slot: null,
  lastX: 0,
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
  state.lastX = ev.clientX;
  state.slot = hitTest(ev.clientX, ev.clientY)?.slot ?? null;
}

function onMouseMove(ev: MouseEvent): void {
  if (!state.pressed) return;
  // ウインドウ外で mouseup を取りこぼした場合の自己回復:
  // ボタンが離れているのに pressed のままなら押下シーケンスを破棄する。
  if (ev.buttons === 0) {
    finishPress(false);
    return;
  }
  if (!state.dragged) {
    const dx = ev.clientX - state.downX;
    const dy = ev.clientY - state.downY;
    if (dx * dx + dy * dy < DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX) return;
    state.dragged = true;
    // 4px の遊び分は反映せず「掴んだ位置」から差分適用を始める
    state.lastX = ev.clientX;
    return;
  }
  // キャラ移動は X 軸のみ (spec §4.3.4)。キャラ外ドラッグでは何も動かさない。
  if (state.slot) {
    moveCharBy(state.slot, ev.clientX - state.lastX);
    state.lastX = ev.clientX;
  }
}

function onMouseUp(): void {
  if (!state.pressed) return;
  finishPress(true);
}

/// 押下シーケンスの終了。countClick=true なら (非ドラッグ時) クリック数判定へ進める。
function finishPress(countClick: boolean): void {
  const wasDragged = state.dragged;
  const slot = state.slot;
  state.pressed = false;
  state.dragged = false;
  state.slot = null;

  if (wasDragged) {
    if (slot) persistCharPositions();
    state.count = 0;
    if (state.resolveTimer !== null) {
      clearTimeout(state.resolveTimer);
      state.resolveTimer = null;
    }
    return;
  }
  if (!countClick) return;

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
