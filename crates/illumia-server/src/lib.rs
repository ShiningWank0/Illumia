//! Axum transport for the framework-independent `illumia-core` services.

mod api;
mod auth;
mod config;
mod error;
mod security;
#[cfg(test)]
mod security_tests;
mod vault;

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, MatchedPath, Request},
    http::{StatusCode, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use illumia_core::{
    PurgeService,
    db::Database,
    jobs::JobRunner,
    thumbnails::{self, THUMBNAIL_JOB_KIND, ThumbnailPayload},
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

pub use config::Config;
pub use illumia_core::VERSION;

const EVENT_BUFFER: usize = 128;
const JSON_BODY_LIMIT: usize = 256 * 1024;
const UPLOAD_BODY_LIMIT: usize = 129 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct AppState {
    database: Database,
    auth: auth::AuthService,
    events: broadcast::Sender<Value>,
    security: security::Security,
    vault: vault::VaultSessionManager,
}

impl AppState {
    fn new(
        database: Database,
        events: broadcast::Sender<Value>,
        vault_ttl: Duration,
        setup_token_hash: Option<[u8; 32]>,
        secure_cookies: bool,
        trust_proxy_headers: bool,
    ) -> Self {
        Self {
            auth: auth::AuthService::new(database.clone()),
            security: security::Security::new(
                setup_token_hash,
                secure_cookies,
                trust_proxy_headers,
            ),
            vault: vault::VaultSessionManager::new(database.data_root(), vault_ttl),
            database,
            events,
        }
    }

    fn notify_assets_added(&self, bucket_key: &str) {
        let _ = self.events.send(json!({
            "type": "assets_added",
            "bucket_keys": [bucket_key],
        }));
    }
}

/// Builds the HTTP service without binding a TCP listener.
///
/// This is also the entry point used by integration tests and future in-process
/// hosts that need the transport service itself.
pub fn app(database: Database, web_dist: Option<PathBuf>) -> Router {
    app_with_vault_ttl(database, web_dist, Duration::from_secs(15 * 60))
}

/// Builds the HTTP service with an injectable vault TTL.
///
/// The custom duration is primarily useful for deterministic expiry tests.
pub fn app_with_vault_ttl(
    database: Database,
    web_dist: Option<PathBuf>,
    vault_ttl: Duration,
) -> Router {
    let (events, _) = broadcast::channel(EVENT_BUFFER);
    app_with_events(database, web_dist, events, vault_ttl, None, false, false)
}

fn app_with_events(
    database: Database,
    web_dist: Option<PathBuf>,
    events: broadcast::Sender<Value>,
    vault_ttl: Duration,
    setup_token_hash: Option<[u8; 32]>,
    secure_cookies: bool,
    trust_proxy_headers: bool,
) -> Router {
    let state = AppState::new(
        database,
        events,
        vault_ttl,
        setup_token_hash,
        secure_cookies,
        trust_proxy_headers,
    );

    let vault_session = Router::new()
        .route("/vault/lock", post(api::vault_lock))
        .route("/vault/import", post(api::vault_import))
        .route("/vault/export", post(api::vault_export))
        .route(
            "/vault/assets/{id}",
            get(api::asset_metadata).delete(api::trash_asset),
        )
        .route("/vault/assets/{id}/restore", post(api::restore_asset))
        .route("/vault/assets/{id}/original", get(api::original))
        .route("/vault/assets/{id}/thumbnail", get(api::thumbnail))
        .route("/vault/assets/{id}/preview", get(api::preview))
        .route("/vault/timeline/buckets", get(api::timeline_buckets))
        .route("/vault/timeline/buckets/{key}", get(api::timeline_bucket))
        .route("/vault/trash", get(api::trash))
        .route("/vault/trash/{id}", delete(api::purge_now))
        .route("/vault/duplicates", get(api::duplicates))
        .route(
            "/vault/stacks",
            get(api::list_stacks).post(api::create_stack),
        )
        .route(
            "/vault/stacks/{id}",
            get(api::get_stack)
                .patch(api::patch_stack)
                .delete(api::delete_stack),
        )
        .route(
            "/vault/stacks/{id}/structure",
            axum::routing::put(api::replace_stack_structure),
        )
        .route("/vault/stacks/{id}/pages", post(api::add_stack_pages))
        .route(
            "/vault/stacks/{id}/pages/{asset_id}",
            axum::routing::patch(api::patch_stack_page).delete(api::remove_stack_page),
        )
        .route("/vault/search", get(api::search))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            vault::require_session,
        ));

    let protected = Router::new()
        .route("/auth/logout", post(auth::logout))
        .route("/auth/devices", get(auth::devices))
        .route("/auth/devices/{id}", delete(auth::revoke_device))
        .route(
            "/assets",
            post(api::upload_asset).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route("/assets/exists", post(api::assets_exist))
        .route(
            "/assets/{id}",
            get(api::asset_metadata).delete(api::trash_asset),
        )
        .route("/assets/{id}/original", get(api::original))
        .route("/assets/{id}/thumbnail", get(api::thumbnail))
        .route("/assets/{id}/preview", get(api::preview))
        .route("/assets/{id}/restore", post(api::restore_asset))
        .route("/timeline/buckets", get(api::timeline_buckets))
        .route("/timeline/buckets/{key}", get(api::timeline_bucket))
        .route("/trash", get(api::trash))
        .route("/duplicates", get(api::duplicates))
        .route("/trash/{id}", delete(api::purge_now))
        .route("/stacks", get(api::list_stacks).post(api::create_stack))
        .route(
            "/stacks/{id}",
            get(api::get_stack)
                .patch(api::patch_stack)
                .delete(api::delete_stack),
        )
        .route(
            "/stacks/{id}/structure",
            axum::routing::put(api::replace_stack_structure),
        )
        .route("/stacks/{id}/pages", post(api::add_stack_pages))
        .route(
            "/stacks/{id}/pages/{asset_id}",
            axum::routing::patch(api::patch_stack_page).delete(api::remove_stack_page),
        )
        .route("/search", get(api::search))
        .route("/jobs", get(api::jobs))
        .route("/jobs/{id}/cancel", post(api::cancel_job))
        .route(
            "/settings",
            get(api::get_settings).patch(api::patch_settings),
        )
        .route("/vault/init", post(api::vault_init))
        .route("/vault/unlock", post(api::vault_unlock))
        .route("/vault/status", get(api::vault_status))
        .merge(vault_session)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let public_auth = Router::new()
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            security::limit_auth_attempts,
        ));
    let websocket = Router::new().route("/ws", get(api::websocket)).route_layer(
        middleware::from_fn_with_state(state.clone(), auth::require_websocket_auth),
    );
    let public = Router::new()
        .route("/server/info", get(auth::server_info))
        .merge(public_auth)
        // WebSocket upgrade 前に header/Cookie の双方を middleware で検証する。
        .merge(websocket);

    let api = protected
        .merge(public)
        .fallback(api_not_found)
        .with_state(state);
    let mut router = Router::new().nest("/api", api);

    if let Some(dist) = web_dist {
        let index = dist.join("index.html");
        router = router.fallback_service(ServeDir::new(dist).fallback(ServeFile::new(index)));
    } else {
        router = router.fallback(not_found);
    }

    router
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                let uri = if request.uri().path().starts_with("/api/vault/") {
                    "/api/vault/*".to_owned()
                } else {
                    request.extensions().get::<MatchedPath>().map_or_else(
                        || request.uri().path().to_owned(),
                        |path| path.as_str().to_owned(),
                    )
                };
                tracing::info_span!(
                    "request",
                    method = %request.method(),
                    uri = %uri,
                    version = ?request.version(),
                )
            }),
        )
        .layer(middleware::from_fn(security::add_security_headers))
        .layer(middleware::from_fn(normalize_error_response))
}

/// Opens storage, starts background services, binds the configured listener,
/// and performs graceful shutdown.
pub async fn run(config: Config) -> Result<()> {
    let Config {
        data_dir,
        addr,
        web_dist,
        setup_token_hash,
        secure_cookies,
        trust_proxy_headers,
    } = config;
    let database = Database::open(&data_dir).context("open Illumia database")?;
    let setup_completed = auth::AuthService::new(database.clone())
        .setup_completed()
        .map_err(|_| anyhow::anyhow!("check initial setup state"))?;
    if !addr.ip().is_loopback() && !setup_completed && setup_token_hash.is_none() {
        bail!(
            "ILLUMIA_SETUP_TOKEN is required before an uninitialized server may listen \
             on a non-loopback address"
        );
    }
    if !addr.ip().is_loopback() && !secure_cookies {
        tracing::warn!(
            "secure cookies are disabled on a non-loopback listener; do not expose this over HTTP"
        );
    }
    let purge = PurgeService::new(database.clone());
    purge
        .resume_purging()
        .context("resume interrupted purges")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    let (events, _) = broadcast::channel(EVENT_BUFFER);
    let mut runner = JobRunner::new(database.clone());
    let thumbnail_events = events.clone();
    runner.register_handler(THUMBNAIL_JOB_KIND, move |database, job| {
        thumbnails::handle_thumbnail_job(database, job)?;
        let payload: ThumbnailPayload = serde_json::from_str(&job.payload)?;
        if let Some(asset) =
            illumia_core::assets::AssetService::new(database.clone()).get(&payload.asset_id)?
        {
            let _ = thumbnail_events.send(json!({
                "type": "assets_added",
                "bucket_keys": [asset.taken_at_local_date],
            }));
        }
        Ok(())
    });
    runner.start().context("start job runner")?;

    let purge_task = tokio::spawn(run_purge_loop(purge));
    tracing::info!(address = %addr, "Illumia server listening");

    let router = app_with_events(
        database,
        web_dist,
        events,
        Duration::from_secs(15 * 60),
        setup_token_hash,
        secure_cookies,
        trust_proxy_headers,
    );
    let serve_result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;

    purge_task.abort();
    let _ = purge_task.await;
    let shutdown_result = tokio::task::spawn_blocking(move || runner.shutdown())
        .await
        .context("join job runner shutdown task")
        .and_then(|result| result.context("shut down job runner"));
    serve_result.context("HTTP server failed")?;
    shutdown_result
}

async fn run_purge_loop(service: PurgeService) {
    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
    interval.tick().await;
    loop {
        interval.tick().await;
        let purge = service.clone();
        match tokio::task::spawn_blocking(move || purge.run_due()).await {
            Ok(Ok(count)) if count != 0 => tracing::info!(count, "expired assets purged"),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::error!(error = %error, "scheduled purge failed"),
            Err(error) => tracing::error!(error = %error, "scheduled purge task failed"),
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to install SIGINT handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

async fn normalize_error_response(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
    {
        return response;
    }
    let status = response.status();
    let code = match status {
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        _ if status.is_server_error() => "internal_error",
        _ => "request_error",
    };
    let message = status.canonical_reason().unwrap_or("request failed");
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"code": "not_found", "message": "API route not found"}})),
    )
}

async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"code": "not_found", "message": "route not found"}})),
    )
}
