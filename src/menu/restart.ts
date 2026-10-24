//! 「再起動」の確認（v0.6.0 項目 7、spec §4.4.2b）。
//! 右クリックメニューの「再起動」と、トレイの「再起動」（バックが `request-restart` を送る）の両方がここを通る。
//! 確認のダイアログでひと呼吸置き、OK のときだけ `restart_app` を呼ぶ。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { uggConfirm } from "../confirm";

export async function confirmAndRestart(): Promise<void> {
  const ok = await uggConfirm("ugg を再起動しますか?", "再起動の確認");
  if (!ok) return;
  try {
    await invoke("restart_app");
  } catch (err) {
    console.error("restart failed", err);
  }
}

/// トレイの「再起動」を受ける。
export async function listenRestartRequests(): Promise<void> {
  await listen("request-restart", () => void confirmAndRestart());
}
