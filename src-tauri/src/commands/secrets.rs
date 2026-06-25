use crate::system::secrets;

/// keyring (Windows Credential Manager) の同期 API は環境次第で稀にハングする
/// (keyring-rs の Microsoft アカウント環境問題)。tokio の blocking pool に逃がして
/// Tauri runtime の他の経路 (set_settings 等) を巻き込まないようにする。
async fn run_keyring<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("keyring task 起動失敗: {e}"))?
        .map_err(|err| format!("{err:#}"))
}

#[tauri::command]
pub async fn set_api_key(provider: String, key: String) -> Result<(), String> {
    if provider.trim().is_empty() {
        return Err("プロバイダ名が空です".to_string());
    }
    if key.trim().is_empty() {
        return Err("API キーが空です".to_string());
    }
    let p = provider.trim().to_string();
    let k = key;
    run_keyring(move || secrets::set_api_key(&p, &k)).await
}

#[tauri::command]
pub async fn has_api_key(provider: String) -> Result<bool, String> {
    let p = provider.trim().to_string();
    run_keyring(move || secrets::has_api_key(&p)).await
}

#[tauri::command]
pub async fn delete_api_key(provider: String) -> Result<(), String> {
    let p = provider.trim().to_string();
    run_keyring(move || secrets::delete_api_key(&p)).await
}
