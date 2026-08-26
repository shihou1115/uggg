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

/// `get_api_key` の**非同期経路向け**ラッパ。
///
/// keyring (Windows Credential Manager) の同期 API は環境次第で稀にハングする
/// (keyring-rs の Microsoft アカウント環境問題)。`commands/secrets.rs` は早くから
/// これを認識して `spawn_blocking` に逃がしていたが、**LLM 経路 3 本が同期版を
/// `async fn` から直接呼んでいた** (Codex レビュー指摘 6、2026-08-23)。
/// そこでハングすると tokio のワーカースレッドごと止まり、しかも停止するのは
/// LLM 呼び出しの**前**なので `tokio::time::timeout` でも救済されない。
pub async fn get_api_key_async(provider: &str) -> Result<Option<String>> {
    let provider = provider.to_string();
    tauri::async_runtime::spawn_blocking(move || get_api_key(&provider))
        .await
        .map_err(|e| anyhow::anyhow!("keyring task 起動失敗: {e}"))?
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **keyring の feature 未指定に対する回帰テスト**（2026-08-17 に実機で発覚）。
    ///
    /// keyring 3.x は `windows-native` 等のストア feature を明示しないと、OS の資格情報
    /// ストアを組み込まず**メモリ上のモック**にフォールバックする。その状態でも
    /// `set_password` は成功を返すため、UI は「保存しました」と出るのに、次に作られた
    /// `Entry` からは読めず（設定を開き直すと「未保存」）、LLM 呼び出しには鍵が乗らない
    /// （401 "You didn't provide an API key"）。**advanced モードが丸ごと機能しない**のに
    /// テストは 1 本も落ちなかった。
    ///
    /// 本テストは `get_api_key` が**別の `Entry` を作り直して**読むことを利用し、
    /// 「プロセス内のメモリではなく実ストアに載ったか」を検査する。
    #[test]
    fn api_key_round_trips_through_a_new_entry() {
        const PROVIDER: &str = "ugg-selftest-roundtrip";
        const VALUE: &str = "dummy-value-not-a-real-key";
        let _ = delete_api_key(PROVIDER); // 前回の残骸を掃除

        set_api_key(PROVIDER, VALUE).expect("set_api_key");
        let got = get_api_key(PROVIDER).expect("get_api_key");
        let has = has_api_key(PROVIDER).expect("has_api_key");
        let cleanup = delete_api_key(PROVIDER); // assert より先に必ず片付ける

        assert_eq!(
            got.as_deref(),
            Some(VALUE),
            "別 Entry から読み戻せない = 実ストアに載っていない (モックへのフォールバック)"
        );
        assert!(has, "has_api_key が false = 設定 UI が「未保存」と表示する状態");
        cleanup.expect("delete_api_key");
        assert_eq!(get_api_key(PROVIDER).expect("get after delete"), None);
    }
}
