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
