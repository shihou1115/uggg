import type { BootSlot, PokeRegions, ShellCharacter, SlotName } from "../types";

export type Region = "head" | "chest" | "body";

interface SlotView {
  root: HTMLElement;
  poseImgs: Map<string, HTMLImageElement>;
  currentPose: string;
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

  const initialPose = pickInitialPose(boot.shell);
  const initial = poseImgs.get(initialPose);
  if (initial) {
    initial.classList.add("visible");
  }

  root.classList.add("ready");
  slotViews[slot] = {
    root,
    poseImgs,
    currentPose: initialPose,
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
  next.classList.add("visible");
  view.currentPose = pose;
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
