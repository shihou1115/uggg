import { invoke } from "@tauri-apps/api/core";

import { reposition } from "../dialogue/balloon";
import { refreshInputPosition } from "../dialogue/input";
import type { SlotName } from "../types";

/// キャラごとの X 位置管理 (spec §4.1.6 / §4.3.4)。
/// 座標系: ステージ (= ウインドウ) 左端基準の CSS px。
/// .char-slot は transform-origin: bottom left なので style.left = 視覚ボックス左端。
/// Y は CSS の bottom:0 固定で、本モジュールは X だけ扱う。

/// 既定配置での main-sub 間隔 (CSS px)。spec §4.1.1。
const DEFAULT_GAP = 40;

const xPos = new Map<SlotName, number>();

function slotRoot(slot: SlotName): HTMLElement | null {
  const el = document.getElementById(`char-${slot}`);
  return el && el.classList.contains("ready") ? el : null;
}

/// スケール込みの視覚幅。mountSlot が width を style 指定するため画像未ロードでも取れる。
function visualWidth(el: HTMLElement): number {
  const w = el.getBoundingClientRect().width;
  return w > 0 ? w : 1;
}

function clampX(x: number, w: number): number {
  const max = Math.max(0, window.innerWidth - w);
  return Math.min(max, Math.max(0, x));
}

function apply(slot: SlotName, x: number): void {
  const el = slotRoot(slot);
  if (!el) return;
  const clamped = clampX(x, visualWidth(el));
  xPos.set(slot, clamped);
  el.style.left = `${clamped}px`;
  reposition(slot);
  refreshInputPosition(slot); // 入力欄がこのキャラにアンカーしていれば追従
}

/// boot 時 (mountSlot 直後) に呼ぶ: 保存値があれば clamp して復元、
/// 無ければ既定配置 (main = ステージ右端、sub = main の左 DEFAULT_GAP)。
export function initCharPositions(saved: { main: number | null; sub: number | null }): void {
  const main = slotRoot("main");
  if (main) {
    apply("main", saved.main ?? window.innerWidth - visualWidth(main));
  }
  const sub = slotRoot("sub");
  if (sub) {
    const anchor = xPos.get("main") ?? window.innerWidth;
    apply("sub", saved.sub ?? anchor - DEFAULT_GAP - visualWidth(sub));
  }
}

/// ドラッグ中: dx (CSS px) だけ横に動かす。ステージ内に clamp。
export function moveCharBy(slot: SlotName, dx: number): void {
  const cur = xPos.get(slot);
  if (cur == null) return;
  apply(slot, cur + dx);
}

/// ステージリサイズ・スケール変更後: 全キャラをステージ内に収め直す。
export function reclampAll(): void {
  for (const slot of ["main", "sub"] as const) {
    const cur = xPos.get(slot);
    if (cur != null) apply(slot, cur);
  }
}

/// ドラッグ終了時 (mouseup) に呼ぶ: 現在位置を即時保存 (spec §4.1.6)。
export function persistCharPositions(): void {
  void invoke("set_char_positions", {
    main: xPos.get("main") ?? null,
    sub: xPos.get("sub") ?? null,
  }).catch((err) => console.error("set_char_positions failed", err));
}
