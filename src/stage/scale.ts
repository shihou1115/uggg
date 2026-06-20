const SCALE_VAR = "--ugg-scale";
const MIN_SCALE = 0.5;
const MAX_SCALE = 2.0;

export function applyDisplayScale(scale: number): void {
  const clamped = clampScale(scale);
  document.documentElement.style.setProperty(SCALE_VAR, clamped.toFixed(3));
}

export function clampScale(scale: number): number {
  if (!Number.isFinite(scale)) {
    return 1.0;
  }
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}
