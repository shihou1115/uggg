import { getCurrentWindow } from "@tauri-apps/api/window";

/// 自前ドラッグ: クリック判別側で 4px 超えの移動を検出したら本関数を呼ぶ。
/// `data-tauri-drag-region` は使わない (透過部分を誤ドラッグしないため)。
export async function startWindowDrag(): Promise<void> {
  try {
    await getCurrentWindow().startDragging();
  } catch (err) {
    console.error("startDragging failed", err);
  }
}
