//! 声をゴーストごとに覚える（v0.6.0 項目 2、spec §6.0）。フロント側の「どのゴーストの声か」の解決。
//!
//! 引くのは**読み込み中のゴースト**（boot payload の ghost id）で、設定の `ghost_id` ではない。設定の
//! `ghost_id` は切り替えて再起動するまでの間に先に変わるので、それで引くと旧ゴーストが新ゴーストの声で
//! 喋る・編集される。合成側（Rust の `Settings::speakers_for`）と同じ解決で、行が無ければアプリの既定。

import type { GhostSpeakers, Settings } from "../types";

/// 読み込み中のゴーストと、声を決めていないゴーストの既定の話者（どちらも boot payload の値）。
export interface LoadedGhost {
  id: string;
  name: string;
  defaultSpeakers: GhostSpeakers;
}

/// 読み込み中のゴーストの話者。行が無ければアプリの既定。
/// 自前の行だけを見る（`"constructor"` のようなゴースト ID が Object のプロトタイプに当たらないように）。
export function speakersFor(
  settings: Pick<Settings, "tts_speakers_by_ghost">,
  loaded: LoadedGhost,
): GhostSpeakers {
  const map = settings.tts_speakers_by_ghost;
  return Object.prototype.hasOwnProperty.call(map, loaded.id) ? map[loaded.id] : loaded.defaultSpeakers;
}
