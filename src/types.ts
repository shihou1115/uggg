export type DialogueMode = "low" | "advanced";
export type SlotName = "main" | "sub";
export type TalkSpeed = "slow" | "normal" | "fast" | "instant";

export interface Settings {
  mode: DialogueMode;
  ghost_id: string;
  shell_id: string;
  display_scale: number;
  quiet_mode: boolean;
  talk_speed: TalkSpeed;
  llm_provider: string;
  llm_model: string;
  llm_base_url: string | null;
  monthly_limit_usd: number;
  profile_max_count: number;
  auto_quiet_fullscreen: boolean;
  monologue_interval_min: number;
  pomodoro_work_min: number;
  pomodoro_break_min: number;
  pomodoro_rounds: number;
  tts_enabled: boolean;
  tts_engine: string;
  tts_speaker_main: number;
  tts_speaker_sub: number;
  tts_speed: number;
  tts_volume: number;
  tts_irodori_use_real_model: boolean;
  autostart: boolean;
  update_feed_url: string | null;
  topics_enabled: boolean;
  tools_enabled: boolean;
  // === 日常支援 Tier S (M7, daily-support-design §5) ===
  daily_support_enabled: boolean;
  situation_break_enabled: boolean;
  situation_late_night_enabled: boolean;
  situation_battery_enabled: boolean;
  todo_follow_enabled: boolean;
  min_speak_interval_min: number;
  night_quiet_enabled: boolean;
  /// 0:00 からの分 (0〜1439)。from > to は日跨ぎ、from == to は終日。
  night_quiet_from: number;
  night_quiet_to: number;
  reminder_notify_enabled: boolean;
  // === カレンダー (M10, spec §4.6.4) ===
  calendar_sources: CalendarSource[];
  calendar_notify_min: number;
}

/// M10: カレンダー ICS ソース (Rust の CalendarSource と serde tag 同期)。
export type CalendarSource =
  | { kind: "file"; path: string }
  | { kind: "url"; url: string };

/// M10: 予定 1 件 (バックエンド CalendarEvent と同期)。
export interface CalendarEvent {
  source_id: number;
  uid: string;
  summary: string;
  start_ts: number;
  end_ts: number | null;
  all_day: boolean;
  /// 繰り返し（未対応）で当日分のみ表示している予定。
  unsupported: boolean;
}

/// リマインダーの繰り返し種別 (M7)。
export type ReminderKind = "once" | "daily" | "weekly";

/// M5-B → M7 拡張: リマインダー 1 件 (バックエンド ReminderEntry と同期)。
export interface ReminderEntry {
  id: number;
  /// 次回発火予定 (UTC 秒)。
  due_ts: number;
  text: string;
  created_ts: number;
  kind: ReminderKind;
  /// weekly のみ: bit0=月 .. bit6=日。
  weekday_mask: number;
  /// daily/weekly のみ: ローカル 0:00 からの秒。
  time_of_day: number;
  /// false = 再発火しない (once の発火済み・完了・無視)。
  active: boolean;
  /// スヌーズ前の本来時刻 (UTC 秒)。
  base_due_ts: number | null;
  /// 通知済み・未処理 (完了/無視されていない発火が残っている)。
  pending: boolean;
}

/// M7: リマインダー通知履歴 1 件 (バックエンド ReminderLogRow と同期)。
export interface ReminderLogRow {
  id: number;
  reminder_id: number;
  fired_ts: number;
  ack: "fired" | "completed" | "dismissed";
  ack_ts: number | null;
  delivery: "ghost" | "toast" | "deferred" | "failed";
}

/// M8: ToDo の分類 (spec §4.6.2、3 区分のみ)。
export type TodoBucket = "today" | "week" | "someday";
/// M8: 日課の繰り返し。null = 単発。
export type TodoRecurring = "daily" | "weekly" | null;

/// M8: ToDo 1 件 (バックエンド TodoEntry と同期)。
export interface TodoEntry {
  id: number;
  text: string;
  bucket: TodoBucket;
  /// 0=普通, 1=高。
  priority: number;
  recurring: TodoRecurring;
  status: "open" | "done";
  done_ts: number | null;
  created_ts: number;
  sort_order: number;
}

/// M5-F: ghosts / shells リストエントリ。
export interface AssetEntry {
  id: string;
  name: string;
}

/// M5-C: 興味分野 1 件。
export interface InterestTopic {
  id: number;
  topic: string;
  enabled: boolean;
}

/// M5-A: DnD インストールの内訳。
export type AssetKind = "ghost" | "shell";

export interface DndInstalled {
  id: string;
  name: string;
  kind: AssetKind;
}

export interface DndConflict {
  id: string;
  name: string;
  kind: AssetKind;
  source: string;
}

export interface DndItemError {
  source: string;
  message: string;
}

export interface DndResult {
  installed: DndInstalled[];
  conflicts: DndConflict[];
  errors: DndItemError[];
}

/// M5-E: clear_history の戻り値。
export interface ClearResult {
  chat_cleared: boolean;
  profile_cleared_count: number;
}

export interface SpeechTurn {
  text: string;
  pose?: string | null;
}

export interface DialogueResponse {
  kind: "reply" | "event" | "system_message";
  mode: DialogueMode;
  pattern: number;
  main: SpeechTurn;
  sub: SpeechTurn | null;
  // === M9: バック起点発話のメタ (🔕 フィードバック用)。ユーザー応答には付かない ===
  /// 発話ごとの一意 id。🔕 クリック時に feedback_speech へ送り返す (誤適用防止)。
  speech_id?: string;
  /// SpeechCategory 識別子 ("monologue" | "situation_break" 等)。
  category?: string;
  priority?: "notice" | "ambient";
  /// true のときだけ 🔕 を表示する (Situation* の Ambient のみ)。
  feedback_allowed?: boolean;
}

export interface BaseSize {
  width: number;
  height: number;
}

export interface PokeRegions {
  head_max: number;
  chest_max: number;
}

export interface ShellCharacter {
  base_size: BaseSize;
  default_pose: string;
  poses: Record<string, string>;
  poke_regions: PokeRegions;
}

export interface BootSlot {
  display_name: string;
  shell: ShellCharacter;
}

export interface BootCharacters {
  main: BootSlot;
  sub: BootSlot | null;
}

/// キャラごとの保存済み X 位置 (ステージ内 CSS px、視覚ボックス左端)。spec §4.1.6。
export interface CharPositions {
  main: number | null;
  sub: number | null;
}

export interface BootPayload {
  settings: Settings;
  ghost_id: string;
  ghost_name: string;
  shell_id: string;
  shell_name: string;
  characters: BootCharacters;
  pose_names: string[];
  onboarded: boolean;
  char_positions: CharPositions;
}

/// M5-G: チャットログ 1 件 (バックエンド `Db::list_recent_chat_log` の戻り値)。
export interface ChatLogRow {
  id: number;
  ts: number;
  mode: string;
  role: "user" | "main" | "sub";
  text: string;
  pose: string | null;
}

// === Irodori-TTS (M4c) ===

/// `irodori_check_gpu` の戻り値。Phase A はスタブで常に available=false。
export interface IrodoriGpuInfo {
  available: boolean;
  name: string | null;
  reason: string | null;
}

/// 参照音声 1 件分のフロント向けメタ。`voice_ref_list` / `voice_ref_delete` が返す。
/// バックの DB 行は file_path も持つが、フロントへは露出しない。
export interface VoiceRef {
  slot: SlotName;
  caption: string;
  created_ts: number;
}

// === 台本形式対応 (docs/script-reader-spec.md) ===

/// `ReadingChunk.slot` の値。`SlotName` と同義だが台本仕様書 (§2.9) の型名に合わせて別名を張る。
export type VoiceSlot = "main" | "sub";

/// `reader_load_text` が返すチャンク 1 件 (script-reader-spec.md §2.9)。
/// caption は常にキーを含み、無指定時は null (undefined ではない)。
export interface ReadingChunk {
  text: string;
  slot: VoiceSlot;
  speed_offset: number;
  caption: string | null;
  pause_after_ms: number;
}
