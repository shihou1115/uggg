//! keyring を介した API キーの保存・取得・削除。
//!
//! service は `"ugg"` 固定、user は provider 名 (`settings.llm_provider`)。
//! 1 プロバイダ 1 キー (spec §4.2.8)。

use anyhow::{Context, Result};

const SERVICE: &str = "ugg";

fn entry(provider: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, provider).with_context(|| {
        format!("keyring エントリの取得に失敗しました (provider={provider})")
    })
}

pub fn set_api_key(provider: &str, key: &str) -> Result<()> {
    entry(provider)?
        .set_password(key)
        .with_context(|| format!("API キー保存に失敗しました (provider={provider})"))
}

pub fn get_api_key(provider: &str) -> Result<Option<String>> {
    match entry(provider)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(anyhow::anyhow!(
            "API キー取得に失敗しました (provider={provider}): {err}"
        )),
    }
}

pub fn has_api_key(provider: &str) -> Result<bool> {
    Ok(get_api_key(provider)?.is_some())
}

pub fn delete_api_key(provider: &str) -> Result<()> {
    match entry(provider)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(anyhow::anyhow!(
            "API キー削除に失敗しました (provider={provider}): {err}"
        )),
    }
}
