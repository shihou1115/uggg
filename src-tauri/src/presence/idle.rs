//! 放置反応 (spec §4.4.3)。
//! 60 秒間隔チェック、最終操作から 30 分で 1 回 idle 発火。静音中・busy 中は持ち越し。
//! ユーザー操作 (last_interaction 更新) でリセット。
//!
//! 実体はバックグラウンドタスク (tasks::spawn_idle_watcher) として M3-C で配線する。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::state::AppState;

/// 最終操作からの経過秒がしきい値を超えたか。
pub const IDLE_THRESHOLD_SECS: i64 = 30 * 60;

pub fn idle_due(state: &Arc<AppState>, now: i64) -> bool {
    let last = state.dialogue.last_interaction.load(Ordering::SeqCst);
    if last == 0 {
        return false;
    }
    now - last >= IDLE_THRESHOLD_SECS
}

/// ユーザー操作でリセット: idle_fired を下ろす。
pub fn reset(state: &Arc<AppState>) {
    state.presence.idle_fired.store(false, Ordering::SeqCst);
}
