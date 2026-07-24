//! Open-Meteo 天気クレジット表示 (spec §4.7.2 / regular-talk-design §2.3・§9.4)。
//!
//! Open-Meteo のデータは CC BY 4.0。予報を利用するときは帰属表記が必要。
//! 天気が有効な間はステージ下端に常時表示する (src/tts/credit.ts と同型)。
//! 文言は index.html に静的配置済みの固定テキストなので、ここでは
//! .visible クラスの付け外しのみを行う (TTS クレジットのような動的テキスト組み立ては不要)。

import type { Settings } from "../types";

interface CreditState {
  el: HTMLElement;
}

let state: CreditState | null = null;

export function mountWeatherCredit(): void {
  const el = document.getElementById("weather-credit");
  if (!el) return;
  state = { el };
}

/// 天気が有効かどうか (Rust 側 `Settings::weather_ready()` と同一条件)。
export function isWeatherReady(settings: Settings | null): boolean {
  return (
    !!settings &&
    settings.weather_enabled &&
    settings.weather_latitude !== null &&
    settings.weather_longitude !== null
  );
}

/// 天気設定が変わりうるとき (boot 直後・settings-changed) に呼ぶ。
export function refreshWeatherCredit(settings: Settings): void {
  if (!state) return;
  state.el.classList.toggle("visible", isWeatherReady(settings));
}
