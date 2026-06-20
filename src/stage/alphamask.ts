import { invoke } from "@tauri-apps/api/core";

const CELL = 8;
const ALPHA_THRESHOLD = 16;
const DEBOUNCE_MS = 50;

let canvas: HTMLCanvasElement | null = null;
let scheduled = false;
let lastSignature = "";

function offscreen(): HTMLCanvasElement {
  if (canvas) return canvas;
  canvas = document.createElement("canvas");
  canvas.style.display = "none";
  document.body.appendChild(canvas);
  return canvas;
}

/// 50ms デバウンスで Rust に最新のマスクを送る。
/// pose 変更 / バルーン表示変更 / リサイズ等から呼ばれる想定。
export function scheduleMaskUpdate(): void {
  if (scheduled) return;
  scheduled = true;
  setTimeout(() => {
    scheduled = false;
    void sendMask();
  }, DEBOUNCE_MS);
}

async function sendMask(): Promise<void> {
  const winW = Math.max(1, Math.floor(window.innerWidth));
  const winH = Math.max(1, Math.floor(window.innerHeight));
  const cols = Math.max(1, Math.ceil(winW / CELL));
  const rows = Math.max(1, Math.ceil(winH / CELL));
  const data = new Uint8Array(cols * rows);

  // 1) キャラ画像を offscreen canvas に描画して alpha を覗く。
  //    キャラレイヤーは CSS transform: scale なので getBoundingClientRect でスケール後矩形が取れる。
  const cv = offscreen();
  cv.width = winW;
  cv.height = winH;
  const ctx = cv.getContext("2d", { willReadFrequently: true });
  if (!ctx) return;
  ctx.clearRect(0, 0, winW, winH);

  for (const slot of ["main", "sub"] as const) {
    const root = document.getElementById(`char-${slot}`);
    if (!root || !root.classList.contains("ready")) continue;
    const rect = root.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) continue;
    const visibleImg = pickVisibleImg(root);
    if (!visibleImg || !visibleImg.complete || visibleImg.naturalWidth === 0) continue;
    ctx.drawImage(visibleImg, rect.left, rect.top, rect.width, rect.height);
  }

  const imgData = ctx.getImageData(0, 0, winW, winH).data;
  for (let r = 0; r < rows; r++) {
    const yStart = r * CELL;
    const yEnd = Math.min(yStart + CELL, winH);
    for (let c = 0; c < cols; c++) {
      const xStart = c * CELL;
      const xEnd = Math.min(xStart + CELL, winW);
      let opaque = false;
      outer: for (let y = yStart; y < yEnd; y++) {
        const rowOffset = y * winW * 4;
        for (let x = xStart; x < xEnd; x++) {
          if (imgData[rowOffset + x * 4 + 3] >= ALPHA_THRESHOLD) {
            opaque = true;
            break outer;
          }
        }
      }
      if (opaque) data[r * cols + c] = 1;
    }
  }

  // 2) .solid 要素 (吹き出し / 入力欄等) の bounding rect で OR
  document.querySelectorAll<HTMLElement>(".solid").forEach((el) => {
    if (!isElementVisible(el)) return;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const c0 = Math.max(0, Math.floor(rect.left / CELL));
    const c1 = Math.min(cols - 1, Math.floor((rect.right - 1) / CELL));
    const r0 = Math.max(0, Math.floor(rect.top / CELL));
    const r1 = Math.min(rows - 1, Math.floor((rect.bottom - 1) / CELL));
    for (let rr = r0; rr <= r1; rr++) {
      for (let cc = c0; cc <= c1; cc++) {
        data[rr * cols + cc] = 1;
      }
    }
  });

  // 3) 変化が無ければ IPC しない
  const signature = `${cols}x${rows}:${quickHash(data)}`;
  if (signature === lastSignature) return;
  lastSignature = signature;

  const b64 = bytesToBase64(data);
  try {
    await invoke("update_alpha_mask", {
      cols,
      rows,
      cellSizeCss: CELL,
      data: b64,
    });
  } catch (err) {
    console.error("update_alpha_mask failed", err);
  }
}

function pickVisibleImg(root: HTMLElement): HTMLImageElement | null {
  const imgs = root.querySelectorAll<HTMLImageElement>("img");
  for (const img of imgs) {
    if (img.classList.contains("visible")) return img;
  }
  return imgs[0] ?? null;
}

function isElementVisible(el: HTMLElement): boolean {
  if (!el.isConnected) return false;
  const style = getComputedStyle(el);
  if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") {
    return false;
  }
  // offsetParent が null の場合は祖先が display:none。
  return el.offsetParent !== null || style.position === "fixed";
}

function quickHash(arr: Uint8Array): number {
  let h = 5381;
  for (let i = 0; i < arr.length; i++) {
    h = ((h << 5) + h + arr[i]) | 0;
  }
  return h;
}

function bytesToBase64(bytes: Uint8Array): string {
  // btoa(spread) は大きな配列でスタックを溢れさせる可能性があるためチャンクで連結
  let bin = "";
  const CHUNK = 8192;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, Math.min(i + CHUNK, bytes.length)));
  }
  return btoa(bin);
}

/// 起動時に呼ぶ。リサイズ/画像読込/初期化で送って Rust 側の不透明状態を更新する。
export function installAlphaMaskHooks(): void {
  window.addEventListener("resize", scheduleMaskUpdate);
  // img.load はバブルしないので capture フェーズで拾う
  document.addEventListener("load", scheduleMaskUpdate, true);
  // 初回送信 (DOM の変化が起きないケースに備えて 1 度トリガ)
  scheduleMaskUpdate();
}
