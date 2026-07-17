//! 予定・ToDo パネル (M7/M8、spec §4.6.1/§4.6.2 / daily-support-design §6・§7.1・§7.2)。
//!
//! - リマインダー節 (M7): 一覧・自然文追加・完了・スヌーズ・削除・通知履歴。
//! - ToDo 節 (M8): 今日/今週/いつかの 3 タブ、追加・チェック完了・優先度トグル・
//!   日課トグル (なし→毎日→毎週)・削除。
//! 登録の主経路は従来どおりチャット自然文で、本パネルは確認・編集用。
//! WebView2 透過バグ対策で index.html に静的配置し、`.visible` クラスでトグル。
//! バックエンドの変更は `reminders-changed` / `todos-changed` イベントで通知され、
//! 表示中なら再取得する。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { ReminderEntry, ReminderLogRow, TodoBucket, TodoEntry } from "../types";

type Filter = "active" | "all" | "completed";

interface Inputs {
  panel: HTMLElement;
  list: HTMLElement;
  filter: HTMLSelectElement;
  addInput: HTMLInputElement;
  addBtn: HTMLButtonElement;
  todoTabs: HTMLElement;
  todoList: HTMLElement;
  todoInput: HTMLInputElement;
  todoAddBtn: HTMLButtonElement;
  closeBtn: HTMLButtonElement;
  msg: HTMLElement;
}

let inputs: Inputs | null = null;
/// 通知履歴を展開中のリマインダー id (再描画で維持する)。
let expandedLogId: number | null = null;
/// ToDo 節で表示中のバケット。
let activeBucket: TodoBucket = "today";
/// 全バケットの ToDo キャッシュ (タブの件数表示に使う)。
let todosCache: TodoEntry[] = [];

export async function mountDailyPanel(): Promise<void> {
  inputs = {
    panel: byId("daily-panel"),
    list: byId("daily-reminder-list"),
    filter: byId<HTMLSelectElement>("daily-filter"),
    addInput: byId<HTMLInputElement>("daily-add-input"),
    addBtn: byId<HTMLButtonElement>("daily-add-btn"),
    todoTabs: byId("daily-todo-tabs"),
    todoList: byId("daily-todo-list"),
    todoInput: byId<HTMLInputElement>("daily-todo-input"),
    todoAddBtn: byId<HTMLButtonElement>("daily-todo-add"),
    closeBtn: byId<HTMLButtonElement>("daily-close"),
    msg: byId("daily-message"),
  };
  inputs.closeBtn.addEventListener("click", () => closeDailyPanel());
  inputs.filter.addEventListener("change", () => void refresh());
  inputs.addBtn.addEventListener("click", () => void onAdd());
  inputs.addInput.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      void onAdd();
    }
  });
  // ToDo 節
  inputs.todoTabs.querySelectorAll<HTMLButtonElement>("button[data-bucket]").forEach((btn) => {
    btn.addEventListener("click", () => {
      activeBucket = (btn.dataset.bucket as TodoBucket) ?? "today";
      renderTodos();
    });
  });
  inputs.todoAddBtn.addEventListener("click", () => void onAddTodo());
  inputs.todoInput.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      void onAddTodo();
    }
  });
  inputs.panel.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeDailyPanel();
    }
  });
  // バック起点の変化 (発火・会話登録・日課復活) で表示中なら再取得
  await listen("reminders-changed", () => {
    if (isOpen()) void refresh();
  });
  await listen("todos-changed", () => {
    if (isOpen()) void refreshTodos();
  });
}

export async function openDailyPanel(): Promise<void> {
  if (!inputs) return;
  inputs.panel.classList.add("visible");
  inputs.msg.hidden = true;
  expandedLogId = null;
  await Promise.all([refresh(), refreshTodos()]);
  setTimeout(() => inputs?.addInput.focus(), 0);
}

export function closeDailyPanel(): void {
  inputs?.panel.classList.remove("visible");
}

function isOpen(): boolean {
  return !!inputs?.panel.classList.contains("visible");
}

async function refresh(): Promise<void> {
  if (!inputs) return;
  try {
    const filter = inputs.filter.value as Filter;
    const list = await invoke<ReminderEntry[]>("list_reminders", { filter });
    renderList(list);
  } catch (err) {
    showMessage(`一覧の取得に失敗: ${formatErr(err)}`, true);
  }
}

async function onAdd(): Promise<void> {
  if (!inputs) return;
  const text = inputs.addInput.value.trim();
  if (!text) return;
  inputs.addBtn.disabled = true;
  try {
    const list = await invoke<ReminderEntry[]>("add_reminder_nl", { text });
    inputs.addInput.value = "";
    inputs.filter.value = "active";
    renderList(list);
    showMessage("登録しました", false);
  } catch (err) {
    showMessage(formatErr(err), true);
  } finally {
    inputs.addBtn.disabled = false;
  }
}

async function action(cmd: string, args: Record<string, unknown>): Promise<void> {
  if (!inputs) return;
  try {
    await invoke<ReminderEntry[]>(cmd, args);
    // 現在のフィルタで取り直す (コマンドの戻りは active 固定のため)
    await refresh();
  } catch (err) {
    showMessage(`操作に失敗: ${formatErr(err)}`, true);
  }
}

function renderList(list: ReminderEntry[]): void {
  if (!inputs) return;
  inputs.list.innerHTML = "";
  if (list.length === 0) {
    const empty = document.createElement("p");
    empty.className = "panel-hint";
    empty.textContent =
      "リマインダーはありません。下の欄かチャットで「3分後にお茶」「毎週月曜9時に会議」のように登録できます。";
    inputs.list.appendChild(empty);
    return;
  }
  const fragment = document.createDocumentFragment();
  for (const r of list) {
    fragment.appendChild(renderItem(r));
  }
  inputs.list.appendChild(fragment);
}

function renderItem(r: ReminderEntry): HTMLElement {
  const item = document.createElement("div");
  item.className = "daily-item";
  if (r.pending) item.classList.add("pending");

  const head = document.createElement("div");
  head.className = "daily-item-head";

  const badge = document.createElement("span");
  badge.className = "daily-badge";
  if (r.pending) {
    badge.classList.add("pending");
    badge.textContent = "未完了";
  } else if (r.active) {
    badge.textContent = "予定";
  } else {
    badge.classList.add("done");
    badge.textContent = "終了";
  }
  head.appendChild(badge);

  const when = document.createElement("span");
  when.className = "daily-when";
  when.textContent = describeWhen(r);
  head.appendChild(when);

  const text = document.createElement("span");
  text.className = "daily-text";
  text.textContent = r.text;
  head.appendChild(text);

  // 通知履歴の開閉 (行クリック)
  head.addEventListener("click", () => void toggleLog(item, r.id));

  const actions = document.createElement("div");
  actions.className = "daily-actions";
  if (r.pending) {
    actions.appendChild(actionBtn("完了", () => action("complete_reminder", { id: r.id })));
    actions.appendChild(
      actionBtn("10分後", () => action("snooze_reminder", { id: r.id, mins: 10 })),
    );
    actions.appendChild(actionBtn("無視", () => action("dismiss_reminder", { id: r.id })));
  } else if (r.active) {
    actions.appendChild(actionBtn("完了", () => action("complete_reminder", { id: r.id })));
  }
  actions.appendChild(actionBtn("削除", () => action("delete_reminder", { id: r.id })));

  item.appendChild(head);
  item.appendChild(actions);
  return item;
}

function actionBtn(label: string, onClick: () => Promise<void>): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.textContent = label;
  btn.addEventListener("click", (ev) => {
    ev.stopPropagation();
    void onClick();
  });
  return btn;
}

async function toggleLog(item: HTMLElement, id: number): Promise<void> {
  const existing = item.querySelector(".daily-log");
  if (existing) {
    existing.remove();
    expandedLogId = null;
    return;
  }
  // 他の行の履歴は閉じる
  document.querySelectorAll("#daily-reminder-list .daily-log").forEach((el) => el.remove());
  expandedLogId = id;
  const box = document.createElement("div");
  box.className = "daily-log";
  box.textContent = "履歴を読み込み中…";
  item.appendChild(box);
  try {
    const rows = await invoke<ReminderLogRow[]>("get_reminder_log", { id });
    if (expandedLogId !== id) return; // 読み込み中に閉じられた
    box.innerHTML = "";
    if (rows.length === 0) {
      box.textContent = "通知履歴はまだありません";
      return;
    }
    for (const log of rows) {
      const line = document.createElement("div");
      line.textContent = `${formatTs(log.fired_ts)} 通知 → ${ackLabel(log)}`;
      box.appendChild(line);
    }
  } catch (err) {
    box.textContent = `履歴の取得に失敗: ${formatErr(err)}`;
  }
}

function ackLabel(log: ReminderLogRow): string {
  switch (log.ack) {
    case "completed":
      return `完了 (${log.ack_ts ? formatTs(log.ack_ts) : "-"})`;
    case "dismissed":
      return `無視 (${log.ack_ts ? formatTs(log.ack_ts) : "-"})`;
    default:
      return "未処理";
  }
}

/// 時刻・繰り返しの表示ラベル。
function describeWhen(r: ReminderEntry): string {
  if (r.kind === "daily") {
    return `毎日 ${formatTod(r.time_of_day)} ↻`;
  }
  if (r.kind === "weekly") {
    return `毎週${weekdayNames(r.weekday_mask)} ${formatTod(r.time_of_day)} ↻`;
  }
  const snoozed = r.base_due_ts != null ? " (スヌーズ中)" : "";
  return formatTs(r.due_ts) + snoozed;
}

function weekdayNames(mask: number): string {
  const names = ["月", "火", "水", "木", "金", "土", "日"];
  return names.filter((_, i) => mask & (1 << i)).join("・");
}

function formatTod(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${h}:${String(m).padStart(2, "0")}`;
}

function formatTs(ts: number): string {
  return new Date(ts * 1000).toLocaleString("ja-JP", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// ===== ToDo 節 (M8) =====

async function refreshTodos(): Promise<void> {
  if (!inputs) return;
  try {
    todosCache = await invoke<TodoEntry[]>("list_todos", {});
    renderTodos();
  } catch (err) {
    showMessage(`ToDo の取得に失敗: ${formatErr(err)}`, true);
  }
}

async function onAddTodo(): Promise<void> {
  if (!inputs) return;
  const text = inputs.todoInput.value.trim();
  if (!text) return;
  inputs.todoAddBtn.disabled = true;
  try {
    todosCache = await invoke<TodoEntry[]>("add_todo", {
      text,
      bucket: activeBucket,
      priority: 0,
      recurring: null,
    });
    inputs.todoInput.value = "";
    renderTodos();
  } catch (err) {
    showMessage(formatErr(err), true);
  } finally {
    inputs.todoAddBtn.disabled = false;
  }
}

async function todoAction(cmd: string, args: Record<string, unknown>): Promise<void> {
  try {
    todosCache = await invoke<TodoEntry[]>(cmd, args);
    renderTodos();
  } catch (err) {
    showMessage(`操作に失敗: ${formatErr(err)}`, true);
    // チェックボックス等の見た目が DB と乖離したまま残らないよう取り直す
    void refreshTodos();
  }
}

function renderTodos(): void {
  if (!inputs) return;
  // タブの active + 件数バッジ
  inputs.todoTabs.querySelectorAll<HTMLButtonElement>("button[data-bucket]").forEach((btn) => {
    const bucket = btn.dataset.bucket as TodoBucket;
    btn.classList.toggle("active", bucket === activeBucket);
    const open = todosCache.filter((t) => t.bucket === bucket && t.status === "open").length;
    const label = bucket === "today" ? "今日" : bucket === "week" ? "今週" : "いつか";
    btn.textContent = open > 0 ? `${label} (${open})` : label;
  });

  inputs.todoList.innerHTML = "";
  const items = todosCache.filter((t) => t.bucket === activeBucket);
  if (items.length === 0) {
    const empty = document.createElement("p");
    empty.className = "panel-hint";
    empty.textContent = "ToDo はありません。上の欄から追加できます。";
    inputs.todoList.appendChild(empty);
    return;
  }
  const fragment = document.createDocumentFragment();
  for (const t of items) {
    fragment.appendChild(renderTodoItem(t));
  }
  inputs.todoList.appendChild(fragment);
}

function renderTodoItem(t: TodoEntry): HTMLElement {
  const item = document.createElement("div");
  item.className = "daily-item todo-item";
  if (t.status === "done") item.classList.add("done");

  const head = document.createElement("div");
  head.className = "daily-item-head";

  const check = document.createElement("input");
  check.type = "checkbox";
  check.checked = t.status === "done";
  check.title = t.status === "done" ? "未完了に戻す" : "完了にする";
  check.addEventListener("change", () => {
    void todoAction(check.checked ? "complete_todo" : "reopen_todo", { id: t.id });
  });
  head.appendChild(check);

  const text = document.createElement("span");
  text.className = "daily-text";
  text.textContent = t.text;
  head.appendChild(text);

  if (t.recurring) {
    const rec = document.createElement("span");
    rec.className = "daily-badge";
    rec.textContent = t.recurring === "daily" ? "毎日↻" : "毎週↻";
    head.appendChild(rec);
  }

  const actions = document.createElement("div");
  actions.className = "daily-actions";

  // 優先度トグル (0 ↔ 1)
  const star = document.createElement("button");
  star.type = "button";
  star.textContent = t.priority === 1 ? "★" : "☆";
  star.title = t.priority === 1 ? "優先を外す" : "優先にする";
  star.classList.toggle("starred", t.priority === 1);
  star.addEventListener("click", () => {
    void todoAction("update_todo", { id: t.id, patch: { priority: t.priority === 1 ? 0 : 1 } });
  });
  actions.appendChild(star);

  // 日課トグル (なし → 毎日 → 毎週 → なし)
  const rec = document.createElement("button");
  rec.type = "button";
  rec.textContent = "↻";
  rec.title =
    t.recurring === null
      ? "日課にする (毎日)"
      : t.recurring === "daily"
        ? "毎週の日課に変える"
        : "日課を解除する";
  rec.classList.toggle("starred", t.recurring !== null);
  rec.addEventListener("click", () => {
    const next = t.recurring === null ? "daily" : t.recurring === "daily" ? "weekly" : null;
    void todoAction("update_todo", { id: t.id, patch: { recurring: next } });
  });
  actions.appendChild(rec);

  const del = document.createElement("button");
  del.type = "button";
  del.textContent = "削除";
  del.addEventListener("click", () => void todoAction("delete_todo", { id: t.id }));
  actions.appendChild(del);

  item.appendChild(head);
  item.appendChild(actions);
  return item;
}

function showMessage(msg: string, isError: boolean): void {
  if (!inputs) return;
  inputs.msg.textContent = msg;
  inputs.msg.classList.toggle("error", isError);
  inputs.msg.hidden = false;
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
