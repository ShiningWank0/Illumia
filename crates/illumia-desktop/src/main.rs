//! Illumia デスクトップクライアントのエントリポイント (M6 → docs/08_clients.md)。
//!
//! 起動モードは環境変数で選ぶ。既定は all-in-one。
//!
//! - `ILLUMIA_DESKTOP_MODE=all-in-one` (既定)
//!   - `ILLUMIA_DATA_DIR` … データディレクトリ (既定: OS 標準のアプリデータ位置)
//!   - **TCP を一切 bind しない**。HTTP listener は存在しない (docs/01 の必須要件)。
//! - `ILLUMIA_DESKTOP_MODE=client-only`
//!   - `ILLUMIA_SERVER_URL` … 接続先 (https 推奨。平文 HTTP は確認付き loopback のみ)
//!   - password は terminal で echo 無し入力し、token/pin は OS secure storage へ保存する

use std::{io::Write, sync::Arc};

use anyhow::{Context, Result, bail};
use illumia_desktop::{
    app::IllumiaApp,
    backend::{self, Mode, RemoteCredential},
    credential_store,
};
use zeroize::Zeroize;

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
            if std::env::var_os("ILLUMIA_DEVICE_TOKEN").is_some() {
                bail!(
                    "ILLUMIA_DEVICE_TOKEN は安全上の理由で廃止されました。環境変数を削除し、interactive login を使用してください"
                );
            }
            let raw_base_url = std::env::var("ILLUMIA_SERVER_URL")
                .context("client-only モードには ILLUMIA_SERVER_URL が必要です")?;
            let base_url = backend::normalize_base_url(&raw_base_url)?;

            if backend::is_insecure_loopback(&base_url)?
                && !confirm(
                    "警告: loopback でも HTTP 通信は暗号化されません。この接続を今回使用しますか? [y/N] ",
                )?
            {
                bail!("平文 HTTP 接続は利用者に拒否されました");
            }

            let probed_instance_id = backend::probe_identity(&base_url)?;
            let credential = match credential_store::load(&base_url)? {
                Some(credential) if credential.instance_id() == probed_instance_id => credential,
                Some(_) => {
                    eprintln!(
                        "保存済み pin と異なる server identity が応答しました: {probed_instance_id}"
                    );
                    if !confirm(
                        "接続先を置き換えますか? 信頼できる server だと確認した場合だけ y を入力してください [y/N] ",
                    )? {
                        bail!("server identity mismatch のため credential を送信しませんでした");
                    }
                    login_and_store(&base_url, &probed_instance_id)?
                }
                None => {
                    eprintln!("初回接続の server identity: {probed_instance_id}");
                    if !confirm(
                        "この identity を pin して login しますか? 信頼できる場合だけ y を入力してください [y/N] ",
                    )? {
                        bail!("初回 server identity が承認されませんでした");
                    }
                    login_and_store(&base_url, &probed_instance_id)?
                }
            };
            Ok(Mode::ClientOnly {
                base_url,
                credential,
            })
        }
        other => bail!("未知の ILLUMIA_DESKTOP_MODE: {other}"),
    }
}

fn login_and_store(base_url: &str, instance_id: &str) -> Result<RemoteCredential> {
    let mut password = rpassword::prompt_password("Illumia password: ")
        .context("password を安全に読み取れませんでした")?;
    if password.is_empty() {
        bail!("password は空にできません");
    }
    let result = backend::login(base_url, &password, instance_id);
    password.zeroize();
    let credential = result?;
    credential_store::save(base_url, &credential)?;
    Ok(credential)
}

fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt}");
    std::io::stderr()
        .flush()
        .context("確認 prompt を表示できない")?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("確認入力を読み取れない")?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
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
