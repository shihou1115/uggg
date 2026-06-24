//! チャットログパネル (M5-G)。
//!
//! 設定パネルの「データ管理」セクションから開く。新しい順 200 件を `get_chat_log` で取得して
//! 役割 (user / main / sub) ごとに色分けで表示。スクロール可能。WebView2 透過バグ対策で
//! index.html に静的配置し、`.visible` クラスでトグル。

import { invoke } from "@tauri-apps/api/core";

import type { ChatLogRow } from "../types";

interface Inputs {
  panel: HTMLElement;
  list: HTMLElement;
  closeBtn: HTMLButtonElement;
  msg: HTMLElement;
}

let inputs: Inputs | null = null;

export function mountChatLog(): void {
  inputs = {
    panel: byId("chatlog-panel"),
    list: byId("chatlog-list"),
    closeBtn: byId<HTMLButtonElement>("chatlog-close"),
    msg: byId("chatlog-message"),
  };
  inputs.closeBtn.addEventListener("click", () => closeChatLog());
  inputs.panel.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeChatLog();
    }
  });
}

export async function openChatLog(): Promise<void> {
  if (!inputs) return;
  inputs.panel.classList.add("visible");
  inputs.msg.hidden = true;
  inputs.list.innerHTML = "";
  try {
    const rows = await invoke<ChatLogRow[]>("get_chat_log", { limit: 200 });
    renderRows(rows);
  } catch (err) {
    inputs.msg.textContent = `読み込み失敗: ${formatErr(err)}`;
    inputs.msg.hidden = false;
  }
}

export function closeChatLog(): void {
  inputs?.panel.classList.remove("visible");
}

function renderRows(rows: ChatLogRow[]): void {
  if (!inputs) return;
  if (rows.length === 0) {
    inputs.msg.textContent = "ログがまだありません";
    inputs.msg.hidden = false;
    return;
  }
  // 新しい順で取得しているので、UI では古い→新しい順にして読みやすく
  const fragment = document.createDocumentFragment();
  for (const row of rows.slice().reverse()) {
    const item = document.createElement("div");
    item.className = `chatlog-item chatlog-role-${row.role}`;
    const meta = document.createElement("div");
    meta.className = "chatlog-meta";
    meta.textContent = `${roleLabel(row.role)} · ${formatTs(row.ts)}`;
    const text = document.createElement("div");
    text.className = "chatlog-text";
    text.textContent = row.text;
    item.appendChild(meta);
    item.appendChild(text);
    fragment.appendChild(item);
  }
  inputs.list.appendChild(fragment);
  // 最新を見やすいよう一番下にスクロール
  inputs.list.scrollTop = inputs.list.scrollHeight;
}

function roleLabel(role: ChatLogRow["role"]): string {
  switch (role) {
    case "user":
      return "あなた";
    case "main":
      return "メイン";
    case "sub":
      return "サブ";
  }
}

function formatTs(ts: number): string {
  return new Date(ts * 1000).toLocaleString("ja-JP", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`要素が見つかりません: ${id}`);
  return el as T;
}

function formatErr(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}
