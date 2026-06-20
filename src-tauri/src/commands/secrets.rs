use crate::system::secrets;

#[tauri::command]
pub fn set_api_key(provider: String, key: String) -> Result<(), String> {
    if provider.trim().is_empty() {
        return Err("プロバイダ名が空です".to_string());
    }
    if key.trim().is_empty() {
        return Err("API キーが空です".to_string());
    }
    secrets::set_api_key(provider.trim(), &key).map_err(|err| format!("{err:#}"))
}

#[tauri::command]
pub fn has_api_key(provider: String) -> Result<bool, String> {
    secrets::has_api_key(provider.trim()).map_err(|err| format!("{err:#}"))
}

#[tauri::command]
pub fn delete_api_key(provider: String) -> Result<(), String> {
    secrets::delete_api_key(provider.trim()).map_err(|err| format!("{err:#}"))
}
