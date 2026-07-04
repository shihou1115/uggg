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
}

/// M5-B: リマインダー 1 件。
export interface ReminderEntry {
  id: number;
  due_ts: number;
  text: string;
  created_ts: number;
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

export interface BootPayload {
  settings: Settings;
  ghost_id: string;
  ghost_name: string;
  shell_id: string;
  shell_name: string;
  characters: BootCharacters;
  pose_names: string[];
  onboarded: boolean;
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
