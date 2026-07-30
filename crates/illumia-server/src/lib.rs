//! Axum transport for the framework-independent `illumia-core` services.

mod api;
mod auth;
mod config;
mod error;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request},
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
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

pub use config::Config;
pub use illumia_core::VERSION;

const EVENT_BUFFER: usize = 128;

#[derive(Clone, Debug)]
pub struct AppState {
    database: Database,
    auth: auth::AuthService,
    events: broadcast::Sender<Value>,
}

impl AppState {
    fn new(database: Database, events: broadcast::Sender<Value>) -> Self {
        Self {
            auth: auth::AuthService::new(database.clone()),
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
    let (events, _) = broadcast::channel(EVENT_BUFFER);
    app_with_events(database, web_dist, events)
}

fn app_with_events(
    database: Database,
    web_dist: Option<PathBuf>,
    events: broadcast::Sender<Value>,
) -> Router {
    let state = AppState::new(database, events);

    let protected = Router::new()
        .route("/auth/devices", get(auth::devices))
        .route("/auth/devices/{id}", delete(auth::revoke_device))
        .route("/assets", post(api::upload_asset))
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
        .route("/search", get(api::search))
        .route("/jobs", get(api::jobs))
        .route("/jobs/{id}/cancel", post(api::cancel_job))
        .route(
            "/settings",
            get(api::get_settings).patch(api::patch_settings),
        )
        .route("/ws", get(api::websocket))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let public = Router::new()
        .route("/server/info", get(auth::server_info))
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login));

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
        .layer(DefaultBodyLimit::disable())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(normalize_error_response))
}

/// Opens storage, starts background services, binds the configured listener,
/// and performs graceful shutdown.
pub async fn run(config: Config) -> Result<()> {
    let database = Database::open(&config.data_dir).context("open Illumia database")?;
    let purge = PurgeService::new(database.clone());
    purge
        .resume_purging()
        .context("resume interrupted purges")?;
    let listener = tokio::net::TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("bind {}", config.addr))?;

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
    tracing::info!(address = %config.addr, "Illumia server listening");

    let serve_result = axum::serve(listener, app_with_events(database, config.web_dist, events))
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
