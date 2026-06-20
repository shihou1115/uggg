import { invoke } from "@tauri-apps/api/core";

import { hitTest } from "../stage/character";
import type { DialogueResponse } from "../types";

/// 撫で判定 (spec §4.3.3): キャラ上をボタン無しで往復するホバーを検知。
/// 合わせ技:
///   - 方向反転回数 ≥ 2
///   - 累積移動量 ≥ MIN_TRAVEL
///   - 局所性: 開始セルから半径 LOCAL_RADIUS 以内に重心が留まる
///   - 最低継続時間 ≥ MIN_DURATION_MS
///   - cooldown: 直前発火から COOLDOWN_MS 以内は無視
///
/// 「ただのカーソル通過」と区別するため、上記をすべて満たした時のみ nade コマンドを叩く。

const SAMPLE_INTERVAL_MS = 30;
const HISTORY_WINDOW_MS = 1200;
const MIN_TRAVEL = 120; // px (CSS)
const LOCAL_RADIUS = 90; // px
const MIN_DIRECTION_REVERSALS = 2;
const MIN_DURATION_MS = 400;
const COOLDOWN_MS = 1500;
const MIN_LEG_TRAVEL = 8; // 方向反転判定の最小ストローク

interface Sample {
  t: number;
  x: number;
  y: number;
}

let samples: Sample[] = [];
let lastDir: 1 | -1 | 0 = 0;
let reversals = 0;
let legTravel = 0;
let lastFireAt = 0;
let lastSampleAt = 0;
let currentSlot: "main" | "sub" | null = null;
let currentRegion: "head" | "chest" | "body" | null = null;

export function attachNadeDetector(): void {
  window.addEventListener("mousemove", onMove, { passive: true });
}

function onMove(ev: MouseEvent): void {
  // mousedown 中は drag/poke 経路に任せる: ボタンが押されていたら nade ではない
  if (ev.buttons !== 0) {
    reset();
    return;
  }
  const now = performance.now();
  if (now - lastSampleAt < SAMPLE_INTERVAL_MS) return;
  lastSampleAt = now;

  const hit = hitTest(ev.clientX, ev.clientY);
  if (!hit) {
    reset();
    return;
  }
  // slot が変わったらリセット
  if (currentSlot !== hit.slot) {
    reset();
    currentSlot = hit.slot;
    currentRegion = hit.region;
  } else {
    currentRegion = hit.region; // 最終位置の region を採用
  }

  // 方向反転判定 (X 軸基準。撫では往復が水平・垂直どちらもあるが、簡略のため X 軸)
  if (samples.length > 0) {
    const prev = samples[samples.length - 1];
    const dx = ev.clientX - prev.x;
    legTravel += Math.abs(dx);
    if (Math.abs(dx) > MIN_LEG_TRAVEL) {
      const dir: 1 | -1 = dx > 0 ? 1 : -1;
      if (lastDir !== 0 && dir !== lastDir && legTravel >= MIN_LEG_TRAVEL * 2) {
        reversals++;
        legTravel = 0;
      }
      lastDir = dir;
    }
  }

  samples.push({ t: now, x: ev.clientX, y: ev.clientY });
  // 古いサンプルを切り捨て
  while (samples.length > 0 && now - samples[0].t > HISTORY_WINDOW_MS) {
    samples.shift();
  }

  evaluate(now);
}

function evaluate(now: number): void {
  if (now - lastFireAt < COOLDOWN_MS) return;
  if (samples.length < 3) return;

  // 累積移動量
  let travel = 0;
  for (let i = 1; i < samples.length; i++) {
    const a = samples[i - 1];
    const b = samples[i];
    travel += Math.hypot(b.x - a.x, b.y - a.y);
  }
  if (travel < MIN_TRAVEL) return;

  // 局所性: 重心からの最大距離
  let cx = 0;
  let cy = 0;
  for (const s of samples) {
    cx += s.x;
    cy += s.y;
  }
  cx /= samples.length;
  cy /= samples.length;
  for (const s of samples) {
    if (Math.hypot(s.x - cx, s.y - cy) > LOCAL_RADIUS) return;
  }

  // 継続時間
  const duration = samples[samples.length - 1].t - samples[0].t;
  if (duration < MIN_DURATION_MS) return;

  // 反転回数
  if (reversals < MIN_DIRECTION_REVERSALS) return;

  fire();
  lastFireAt = now;
  reset();
}

function fire(): void {
  if (!currentSlot || !currentRegion) return;
  const target = currentSlot;
  const region = currentRegion;
  void invoke<DialogueResponse | null>("nade", { target, region }).catch((err) => {
    console.error("nade failed", err);
  });
}

function reset(): void {
  samples = [];
  lastDir = 0;
  reversals = 0;
  legTravel = 0;
  currentSlot = null;
  currentRegion = null;
}
