use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::ghost::manifest::{build_shell_character, ShellCharacter};
use crate::state::{AppState, Settings};

#[derive(Debug, Serialize)]
pub struct BootCharacters {
    pub main: BootSlot,
    pub sub: Option<BootSlot>,
}

#[derive(Debug, Serialize)]
pub struct BootSlot {
    pub display_name: String,
    pub shell: ShellCharacter,
}

#[derive(Debug, Serialize)]
pub struct BootPayload {
    pub settings: Settings,
    pub ghost_id: String,
    pub ghost_name: String,
    pub shell_id: String,
    pub shell_name: String,
    pub characters: BootCharacters,
    pub pose_names: Vec<String>,
    /// false なら初回オンボーディングを表示する。
    pub onboarded: bool,
}

#[tauri::command]
pub fn get_boot_payload(state: State<'_, Arc<AppState>>) -> Result<BootPayload, String> {
    build_payload(&state).map_err(|err| format!("{err:#}"))
}

fn build_payload(state: &AppState) -> anyhow::Result<BootPayload> {
    let settings = state.settings.lock().expect("settings poisoned").clone();
    let guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = match guard.as_ref() {
        Ok(b) => b,
        Err(msg) => anyhow::bail!("{msg}"),
    };

    let main_shell = build_shell_character(&bundle.shell.characters.main, &bundle.shell_dir)?;
    let sub_shell = match (&bundle.shell.characters.sub, &bundle.ghost.characters.sub) {
        (Some(def), _) => Some(build_shell_character(def, &bundle.shell_dir)?),
        (None, _) => None,
    };

    let main_slot = BootSlot {
        display_name: bundle.ghost.characters.main.name.clone(),
        shell: main_shell,
    };

    let sub_slot = match (sub_shell, &bundle.ghost.characters.sub) {
        (Some(shell), Some(ghost_sub)) => Some(BootSlot {
            display_name: ghost_sub.name.clone(),
            shell,
        }),
        (Some(shell), None) => Some(BootSlot {
            display_name: "sub".to_string(),
            shell,
        }),
        (None, _) => None,
    };

    let mut pose_names: Vec<String> = main_slot
        .shell
        .poses
        .keys()
        .cloned()
        .collect();
    pose_names.sort();
    pose_names.dedup();

    let onboarded = crate::commands::onboarding::is_onboarded(&state.db);

    Ok(BootPayload {
        settings,
        ghost_id: bundle.ghost.id.clone(),
        ghost_name: bundle.ghost.name.clone(),
        shell_id: bundle.shell.id.clone(),
        shell_name: bundle.shell.name.clone(),
        characters: BootCharacters {
            main: main_slot,
            sub: sub_slot,
        },
        pose_names,
        onboarded,
    })
}
