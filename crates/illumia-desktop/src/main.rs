//! Illumia デスクトップクライアントのエントリポイント (M6 → docs/08_clients.md)。
//!
//! 起動モードは環境変数で選ぶ。既定は all-in-one。
//!
//! - `ILLUMIA_DESKTOP_MODE=all-in-one` (既定)
//!   - `ILLUMIA_DATA_DIR` … データディレクトリ (既定: OS 標準のアプリデータ位置)
//!   - **TCP を一切 bind しない**。HTTP listener は存在しない (docs/01 の必須要件)。
//! - `ILLUMIA_DESKTOP_MODE=client-only`
//!   - `ILLUMIA_SERVER_URL` … 接続先 (https のみ。平文 HTTP はループバックのみ)
//!   - `ILLUMIA_DEVICE_TOKEN` … device token

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use illumia_desktop::{
    app::IllumiaApp,
    backend::{self, Mode},
};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let backend = backend::build(resolve_mode()?)?;
    tracing::info!(mode = backend.mode_label(), "Illumia desktop starting");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Illumia"),
        ..Default::default()
    };

    let backend = Arc::clone(&backend);
    eframe::run_native(
        "Illumia",
        options,
        Box::new(move |_context| Ok(Box::new(IllumiaApp::new(backend)))),
    )
    .map_err(|error| anyhow::anyhow!("UI を起動できません: {error}"))
}

fn resolve_mode() -> Result<Mode> {
    let mode = std::env::var("ILLUMIA_DESKTOP_MODE").unwrap_or_else(|_| "all-in-one".to_owned());
    match mode.as_str() {
        "all-in-one" => Ok(Mode::AllInOne {
            data_root: data_root()?,
        }),
        "client-only" => {
            let base_url = std::env::var("ILLUMIA_SERVER_URL")
                .context("client-only モードには ILLUMIA_SERVER_URL が必要です")?;
            let token = std::env::var("ILLUMIA_DEVICE_TOKEN")
                .context("client-only モードには ILLUMIA_DEVICE_TOKEN が必要です")?;
            Ok(Mode::ClientOnly { base_url, token })
        }
        other => bail!("未知の ILLUMIA_DESKTOP_MODE: {other}"),
    }
}

/// データディレクトリ。明示指定が無ければ OS 標準のアプリデータ位置を使う。
fn data_root() -> Result<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("ILLUMIA_DATA_DIR") {
        return Ok(std::path::PathBuf::from(explicit));
    }
    let dirs = directories::ProjectDirs::from("com", "shiningwank0", "Illumia")
        .context("アプリデータディレクトリを解決できません")?;
    Ok(dirs.data_dir().to_path_buf())
}
