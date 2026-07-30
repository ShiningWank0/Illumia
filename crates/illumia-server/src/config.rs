use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, anyhow};

#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    pub addr: SocketAddr,
    pub web_dist: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let data_dir = env::var_os("ILLUMIA_DATA_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("ILLUMIA_DATA_DIR is required"))?;
        let addr = env::var("ILLUMIA_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:2283".to_owned())
            .parse()
            .context("ILLUMIA_ADDR must be a valid socket address")?;
        let web_dist = env::var_os("ILLUMIA_WEB_DIST")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            data_dir,
            addr,
            web_dist,
        })
    }
}
