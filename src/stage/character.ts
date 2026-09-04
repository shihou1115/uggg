import type { BootSlot, PokeRegions, ShellCharacter, SlotName } from "../types";

export type Region = "head" | "chest" | "body";

interface SlotView {
  root: HTMLElement;
  poseImgs: Map<string, HTMLImageElement>;
  talkImgs: Map<string, HTMLImageElement>;
  currentPose: string;
  /** 口パク中に開口フレームを表示しているか。 */
  mouthOpen: boolean;
  pokeRegions: PokeRegions;
}

const slotViews: Partial<Record<SlotName, SlotView>> = {};

export function mountSlot(slot: SlotName, boot: BootSlot): void {
  const root = document.getElementById(`char-${slot}`);
  if (!root) {
    throw new Error(`character slot DOM not found: char-${slot}`);
  }
  root.innerHTML = "";
  root.style.width = `${boot.shell.base_size.width}px`;
  root.style.height = `${boot.shell.base_size.height}px`;
  root.setAttribute("data-display-name", boot.display_name);

  const poseImgs = new Map<string, HTMLImageElement>();
  for (const [poseName, dataUrl] of Object.entries(boot.shell.poses)) {
    const img = new Image();
    img.classList.add("pose");
    img.alt = "";
    img.draggable = false;
    img.src = dataUrl;
    img.width = boot.shell.base_size.width;
    img.height = boot.shell.base_size.height;
    root.appendChild(img);
    poseImgs.set(poseName, img);
  }

  // 開口フレーム (spec §4.1.4)。閉口フレームと同じ位置に重ねておき、
  // 口パク中は表示を差し替えるだけにする（動的 createElement は WebView2 の
  // 透過レイヤーで描画されないため、ここで静的に作っておく）。
  const talkImgs = new Map<string, HTMLImageElement>();
  for (const [poseName, dataUrl] of Object.entries(boot.shell.talk_poses ?? {})) {
    const img = new Image();
    img.classList.add("pose");
    img.alt = "";
    img.draggable = false;
    img.src = dataUrl;
    img.width = boot.shell.base_size.width;
    img.height = boot.shell.base_size.height;
    root.appendChild(img);
    talkImgs.set(poseName, img);
  }

  const initialPose = pickInitialPose(boot.shell);
  const initial = poseImgs.get(initialPose);
  if (initial) {
    initial.classList.add("visible");
  }

  root.classList.add("ready");
  slotViews[slot] = {
    root,
    poseImgs,
    talkImgs,
    currentPose: initialPose,
    mouthOpen: false,
    pokeRegions: boot.shell.poke_regions,
  };
}

export function unmountSlot(slot: SlotName): void {
  const view = slotViews[slot];
  if (!view) return;
  view.root.classList.remove("ready");
  view.root.innerHTML = "";
  delete slotViews[slot];
}

export function setPose(slot: SlotName, pose: string): void {
  const view = slotViews[slot];
  if (!view) return;
  if (view.currentPose === pose) return;
  const next = view.poseImgs.get(pose);
  if (!next) return;
  view.poseImgs.get(view.currentPose)?.classList.remove("visible");
  view.talkImgs.get(view.currentPose)?.classList.remove("visible");
  view.currentPose = pose;
  // 口パク中に pose が変わっても開口状態を保つ。
  showFrame(view, pose, view.mouthOpen);
}

/// 指定 pose の閉口/開口フレームを出し分ける。開口フレームが無ければ閉口のまま。
function showFrame(view: SlotView, pose: string, open: boolean): void {
  const talk = view.talkImgs.get(pose);
  const idle = view.poseImgs.get(pose);
  if (open && talk) {
    idle?.classList.remove("visible");
    talk.classList.add("visible");
  } else {
    talk?.classList.remove("visible");
    idle?.classList.add("visible");
  }
}

/**
 * 口の開閉 (spec §4.1.4)。TTS の振幅駆動から呼ぶ。
 * 開口フレームを持たないシェルでは何も起きない（= 口パクなし）。
 */
export function setMouthOpen(slot: SlotName, open: boolean): void {
  const view = slotViews[slot];
  if (!view) return;
  if (view.mouthOpen === open) return;
  view.mouthOpen = open;
  showFrame(view, view.currentPose, open);
}

/// ビューポート座標 (CSS px) を受け取り、ヒットした slot と縦部位 (head/chest/body) を返す。
/// どの slot にも当たらなければ null。
export function hitTest(x: number, y: number): { slot: SlotName; region: Region } | null {
  for (const slot of ["main", "sub"] as const) {
    const view = slotViews[slot];
    if (!view || !view.root.classList.contains("ready")) continue;
    const rect = view.root.getBoundingClientRect();
    if (
      x < rect.left ||
      x >= rect.right ||
      y < rect.top ||
      y >= rect.bottom
    ) {
      continue;
    }
    const ny = (y - rect.top) / Math.max(1, rect.height);
    const region = regionFromNy(ny, view.pokeRegions);
    return { slot, region };
  }
  return null;
}

function regionFromNy(ny: number, r: PokeRegions): Region {
  if (ny < r.head_max) return "head";
  if (ny < r.chest_max) return "chest";
  return "body";
}

function pickInitialPose(shell: ShellCharacter): string {
  if (shell.poses[shell.default_pose]) {
    return shell.default_pose;
  }
  const fallback = Object.keys(shell.poses)[0];
  if (!fallback) {
    throw new Error("shell has no poses");
  }
  return fallback;
}
