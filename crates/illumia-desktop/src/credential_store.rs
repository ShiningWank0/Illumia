//! Desktop client-only の device token / server identity pin を OS secure storage に保存する。
//!
//! macOS は Keychain、Windows は Credential Manager を利用する。平文 file や環境変数への
//! fallback は設けない。対応 secure store が無い platform では client-only を fail closed にする。

#[cfg(any(target_os = "macos", target_os = "windows"))]
use anyhow::Context;
use anyhow::Result;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use zeroize::Zeroize;

use crate::backend::RemoteCredential;

const SERVICE: &str = "com.shiningwank0.illumia.desktop";

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn entry(base_url: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, base_url).context("OS secure storage entry を開けない")
}

/// 保存済み credential を読み取る。未登録は `None`。
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn load(base_url: &str) -> Result<Option<RemoteCredential>> {
    let entry = entry(base_url)?;
    let mut encoded = match entry.get_password() {
        Ok(value) => value,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(error).context("OS secure storage から credential を読めない"),
    };
    let decoded = serde_json::from_str(&encoded).context("保存済み credential の形式が不正");
    encoded.zeroize();
    decoded.map(Some)
}

/// token と pin を 1 つの OS secure storage entry として原子的に置き換える。
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn save(base_url: &str, credential: &RemoteCredential) -> Result<()> {
    let mut encoded =
        serde_json::to_string(credential).context("credential を保存形式へ変換できない")?;
    let result = entry(base_url)?.set_password(&encoded);
    encoded.zeroize();
    result.context("credential を OS secure storage へ保存できない")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn load(_base_url: &str) -> Result<Option<RemoteCredential>> {
    let _ = SERVICE;
    anyhow::bail!("client-only secure storage は macOS / Windows でのみ利用できます")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn save(_base_url: &str, _credential: &RemoteCredential) -> Result<()> {
    anyhow::bail!("client-only secure storage は macOS / Windows でのみ利用できます")
}
