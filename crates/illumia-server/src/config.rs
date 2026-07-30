use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use illumia_core::sha2::{Digest, Sha256};

const MIN_SETUP_TOKEN_BYTES: usize = 32;
const MAX_SETUP_TOKEN_BYTES: usize = 256;

#[derive(Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub addr: SocketAddr,
    pub web_dist: Option<PathBuf>,
    pub setup_token_hash: Option<[u8; 32]>,
    pub secure_cookies: bool,
    pub trust_proxy_headers: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("data_dir", &self.data_dir)
            .field("addr", &self.addr)
            .field("web_dist", &self.web_dist)
            .field(
                "setup_token_hash",
                &self.setup_token_hash.map(|_| "[REDACTED]"),
            )
            .field("secure_cookies", &self.secure_cookies)
            .field("trust_proxy_headers", &self.trust_proxy_headers)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let data_dir = env::var_os("ILLUMIA_DATA_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("ILLUMIA_DATA_DIR is required"))?;
        let addr = env::var("ILLUMIA_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:2283".to_owned())
            .parse()
            .context("ILLUMIA_ADDR must be a valid socket address")?;
        let web_dist = env::var_os("ILLUMIA_WEB_DIST")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let setup_token_hash = env::var("ILLUMIA_SETUP_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| {
                if !(MIN_SETUP_TOKEN_BYTES..=MAX_SETUP_TOKEN_BYTES).contains(&value.len()) {
                    return Err(anyhow!(
                        "ILLUMIA_SETUP_TOKEN must be between {MIN_SETUP_TOKEN_BYTES} and \
                         {MAX_SETUP_TOKEN_BYTES} bytes"
                    ));
                }
                Ok(Sha256::digest(value.as_bytes()).into())
            })
            .transpose()?;
        let secure_cookies = boolean_env("ILLUMIA_SECURE_COOKIES", true)?;
        let trust_proxy_headers = boolean_env("ILLUMIA_TRUST_PROXY_HEADERS", false)?;
        Ok(Self {
            data_dir,
            addr,
            web_dist,
            setup_token_hash,
            secure_cookies,
            trust_proxy_headers,
        })
    }
}

fn boolean_env(name: &str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(anyhow!("{name} must be a boolean")),
        },
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).with_context(|| format!("read {name}")),
    }
}
