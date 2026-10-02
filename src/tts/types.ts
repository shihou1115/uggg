export interface VoiceOption {
  id: number;
  name: string;
}

/// `synthesize_voice` の戻り（v0.6.0 項目 2。Rust の `SynthesizedVoice`）。
/// `wav` は base64 の WAV。`engine` は**実際に鳴らしたエンジン**: `"voicevox_core"`（VOICEVOX。高品質モードの
/// フォールバックを含む）か `"irodori"`。VOICEVOX のクレジットの表示に使う（`credit.ts`）。
export interface SynthesizedVoice {
  wav: string;
  engine: string;
}
