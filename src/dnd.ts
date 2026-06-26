//! ゴースト/シェル DnD インストール (M5-A)。
//!
//! Tauri 2 の WebviewWindow.onDragDropEvent を介して OS パスを取得し、
//! バックの `dnd_install` を呼ぶ。conflict があれば confirm() で上書き確認、
//! 再度 `dnd_install({overwrite: true})` を打つ。
//!
//! 設定パネル「キャラクター」セクションには `<input type="file">` の
//! フォールバックを置き、WebView で path が取れない環境では File 自体は
//! 扱えないので「DnD でファイルを window にドロップしてください」と案内する。

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

import { uggConfirm } from "./confirm";
import type { DndResult } from "./types";

type DropHandler = (result: DndResult) => void;

let onResult: DropHandler | null = null;

/// 起動時に 1 度だけ呼ぶ。OS の DnD イベントを listen する。
export async function mountDnd(): Promise<void> {
  await getCurrentWebviewWindow().onDragDropEvent(async (event) => {
    if (event.payload.type !== "drop") return;
    const paths = event.payload.paths;
    if (!paths || paths.length === 0) return;
    await handleDnd(paths);
  });
}

/// 結果を受け取るリスナを登録 (設定パネル等から)。1 つだけ覚える。
export function registerDndResultListener(handler: DropHandler | null): void {
  onResult = handler;
}

async function handleDnd(paths: string[]): Promise<void> {
  let result: DndResult;
  try {
    result = await invoke<DndResult>("dnd_install", { paths, overwrite: false });
  } catch (err) {
    console.warn("[dnd] install failed", err);
    return;
  }
  // conflict があれば confirm して再投入
  if (result.conflicts.length > 0) {
    const ids = result.conflicts.map((c) => `${c.kind}:${c.id}`).join(", ");
    const ok = await uggConfirm(
      `次の項目は既存と重複しています。上書きしますか?\n${ids}`,
      "上書き確認",
    );
    if (ok) {
      const retryPaths = result.conflicts.map((c) => c.source);
      try {
        const retried = await invoke<DndResult>("dnd_install", {
          paths: retryPaths,
          overwrite: true,
        });
        // installed をマージ
        result = {
          installed: [...result.installed, ...retried.installed],
          conflicts: retried.conflicts,
          errors: [...result.errors, ...retried.errors],
        };
      } catch (err) {
        console.warn("[dnd] retry overwrite failed", err);
      }
    }
  }
  onResult?.(result);
  emitToWindow(result);
}

function emitToWindow(result: DndResult): void {
  // 設定パネルが閉じていても通知を出せるよう、`window` カスタムイベントで配る
  window.dispatchEvent(new CustomEvent("ugg-dnd-result", { detail: result }));
}
