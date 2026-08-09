//! Axum transport for the framework-independent `illumia-core` services.

mod api;
mod auth;
mod config;
mod error;
mod security;
#[cfg(test)]
mod security_tests;
mod vault;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io,
    net::IpAddr,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, MatchedPath, Request},
    http::{StatusCode, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto::Builder as ConnectionBuilder,
    service::TowerToHyperService,
};
use illumia_core::{
    PurgeService,
    db::Database,
    jobs::JobRunner,
    ml::{ML_ANALYZE_JOB_KIND, ML_RECLUSTER_JOB_KIND, MlService},
    ml_client::{Health as MlHealth, MlClient},
    settings::Settings,
    thumbnails::{self, THUMBNAIL_JOB_KIND, ThumbnailPayload},
};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, broadcast};
use tower_http::{
    services::{ServeDir, ServeFile},
    timeout::{RequestBodyTimeoutLayer, ResponseBodyTimeoutLayer},
    trace::TraceLayer,
};

pub use config::Config;
pub use illumia_core::VERSION;

const EVENT_BUFFER: usize = 128;
const JSON_BODY_LIMIT: usize = 256 * 1024;
const UPLOAD_BODY_LIMIT: usize = 129 * 1024 * 1024;
const ML_HEALTH_CACHE_TTL: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_ML_HEALTH_PROBES: usize = 1;
const MAX_CONCURRENT_BLOCKING_DB_REQUESTS: usize = 8;
#[cfg(not(test))]
pub(crate) const MAX_HTTP_CONNECTIONS: usize = 256;
#[cfg(test)]
pub(crate) const MAX_HTTP_CONNECTIONS: usize = 8;
#[cfg(not(test))]
pub(crate) const MAX_HTTP_CONNECTIONS_PER_IP: usize = 32;
#[cfg(test)]
pub(crate) const MAX_HTTP_CONNECTIONS_PER_IP: usize = 2;
#[cfg(not(test))]
const HTTP_HEADER_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
const HTTP_HEADER_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const HTTP_BODY_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const HTTP_BODY_PROGRESS_TIMEOUT: Duration = Duration::from_millis(300);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const HTTP_CONNECTION_MAX_LIFETIME: Duration = Duration::from_secs(10 * 60);
const HTTP_GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP2_STREAMS_PER_CONNECTION: u32 = 32;

struct HttpConnectionPermit {
    _global: OwnedSemaphorePermit,
    peer: IpAddr,
    by_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl HttpConnectionPermit {
    fn try_new(
        global: OwnedSemaphorePermit,
        peer: IpAddr,
        by_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    ) -> Option<Self> {
        let mut counts = by_ip.lock().unwrap_or_else(|error| error.into_inner());
        let active = counts.entry(peer).or_default();
        if *active >= MAX_HTTP_CONNECTIONS_PER_IP {
            return None;
        }
        *active += 1;
        drop(counts);
        Some(Self {
            _global: global,
            peer,
            by_ip,
        })
    }
}

impl Drop for HttpConnectionPermit {
    fn drop(&mut self) {
        let mut counts = self.by_ip.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(active) = counts.get_mut(&self.peer) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                counts.remove(&self.peer);
            }
        }
    }
}

#[derive(Clone, Debug)]
struct CachedMlHealth {
    socket_path: PathBuf,
    checked_at: tokio::time::Instant,
    health: Option<MlHealth>,
}

#[derive(Clone, Debug)]
struct MlConcurrencyGate {
    inner: Arc<(Mutex<usize>, Condvar)>,
    limit: usize,
}

struct MlConcurrencyPermit {
    gate: MlConcurrencyGate,
}

impl MlConcurrencyGate {
    fn new(limit: usize) -> Self {
        Self {
            inner: Arc::new((Mutex::new(0), Condvar::new())),
            limit: limit.max(1),
        }
    }

    fn acquire(&self) -> MlConcurrencyPermit {
        let (mutex, condition) = &*self.inner;
        let mut active = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *active >= self.limit {
            active = condition
                .wait(active)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *active += 1;
        MlConcurrencyPermit { gate: self.clone() }
    }
}

impl Drop for MlConcurrencyPermit {
    fn drop(&mut self) {
        let (mutex, condition) = &*self.gate.inner;
        let mut active = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        condition.notify_one();
    }
}

#[derive(Clone, Debug)]
pub struct AppState {
    database: Database,
    auth: auth::AuthService,
    events: broadcast::Sender<Value>,
    security: security::Security,
    vault: vault::VaultSessionManager,
    job_poller_started: Arc<AtomicBool>,
    ml_probe_slots: Arc<Semaphore>,
    ml_health_cache: Arc<AsyncMutex<Option<CachedMlHealth>>>,
    ml_gate: MlConcurrencyGate,
    vault_ml_workers: Arc<Mutex<HashSet<u64>>>,
    blocking_db_slots: Arc<Semaphore>,
}

impl AppState {
    fn new(
        database: Database,
        events: broadcast::Sender<Value>,
        vault_ttl: Duration,
        setup_token_hash: Option<[u8; 32]>,
        secure_cookies: bool,
        trusted_proxy_cidrs: Vec<config::TrustedProxy>,
        ml_gate: Option<MlConcurrencyGate>,
    ) -> Self {
        let configured_ml_concurrency = Settings::new(database.clone())
            .ml_concurrency()
            .ok()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(1);
        Self {
            auth: auth::AuthService::new(database.clone()),
            security: security::Security::new(
                setup_token_hash,
                secure_cookies,
                trusted_proxy_cidrs,
            ),
            vault: vault::VaultSessionManager::new(database.clone(), vault_ttl),
            database,
            events,
            job_poller_started: Arc::new(AtomicBool::new(false)),
            ml_probe_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_ML_HEALTH_PROBES)),
            ml_health_cache: Arc::new(AsyncMutex::new(None)),
            ml_gate: ml_gate.unwrap_or_else(|| MlConcurrencyGate::new(configured_ml_concurrency)),
            vault_ml_workers: Arc::new(Mutex::new(HashSet::new())),
            blocking_db_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_BLOCKING_DB_REQUESTS)),
        }
    }

    /// このサーバーインスタンスの一意識別子 (初回参照時に生成・永続化)。
    /// ネイティブクライアントの接続先 pin に使う (→ docs/12_security.md, SEC-002)。
    fn instance_id(&self) -> Result<String, error::ApiError> {
        Ok(Settings::new(self.database.clone()).instance_id()?)
    }

    fn notify_assets_added(&self, bucket_key: &str) {
        let _ = self.events.send(json!({
            "type": "assets_added",
            "bucket_keys": [bucket_key],
        }));
    }

    fn ensure_job_event_poller(&self) {
        if self
            .job_poller_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let database = self.database.clone();
        let events = self.events.clone();
        let started = Arc::clone(&self.job_poller_started);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            let mut previous = HashSet::new();
            loop {
                interval.tick().await;
                if events.receiver_count() == 0 {
                    started.store(false, Ordering::Release);
                    if events.receiver_count() == 0
                        || started
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_err()
                    {
                        return;
                    }
                }
                let poll_database = database.clone();
                let jobs = match tokio::task::spawn_blocking(move || {
                    illumia_core::jobs::JobQueue::new(poll_database).list()
                })
                .await
                {
                    Ok(Ok(jobs)) => jobs,
                    Ok(Err(error)) => {
                        tracing::warn!(error = %error, "websocket job polling failed");
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "websocket job poll task failed");
                        continue;
                    }
                };
                let snapshot: HashSet<_> = jobs.iter().map(api::job_snapshot).collect();
                for job in &jobs {
                    let current = api::job_snapshot(job);
                    if !previous.contains(&current) {
                        let _ = events.send(json!({
                            "type": "job",
                            "id": job.id,
                            "state": api::job_state_name(job.state),
                            "progress": job.progress,
                        }));
                    }
                }
                previous = snapshot;
            }
        });
    }

    async fn ml_health(&self, socket_path: PathBuf) -> Option<MlHealth> {
        {
            let cache = self.ml_health_cache.lock().await;
            if let Some(cached) = cache.as_ref()
                && cached.socket_path == socket_path
                && cached.checked_at.elapsed() < ML_HEALTH_CACHE_TTL
            {
                return cached.health.clone();
            }
        }
        let permit = self.ml_probe_slots.clone().acquire_owned().await.ok()?;
        {
            let cache = self.ml_health_cache.lock().await;
            if let Some(cached) = cache.as_ref()
                && cached.socket_path == socket_path
                && cached.checked_at.elapsed() < ML_HEALTH_CACHE_TTL
            {
                return cached.health.clone();
            }
        }
        let probe_path = socket_path.clone();
        let health = tokio::task::spawn_blocking(move || MlClient::new(probe_path).health().ok())
            .await
            .ok()
            .flatten();
        drop(permit);
        *self.ml_health_cache.lock().await = Some(CachedMlHealth {
            socket_path,
            checked_at: tokio::time::Instant::now(),
            health: health.clone(),
        });
        health
    }

    fn try_blocking_db_slot(&self) -> Result<OwnedSemaphorePermit, error::ApiError> {
        self.blocking_db_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| error::ApiError::too_many_requests("database work is temporarily busy"))
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
    app_with_events(
        database,
        web_dist,
        events,
        vault_ttl,
        AppBuildOptions::default(),
    )
}

#[derive(Default)]
struct AppBuildOptions {
    setup_token_hash: Option<[u8; 32]>,
    secure_cookies: bool,
    trusted_proxy_cidrs: Vec<config::TrustedProxy>,
    ml_gate: Option<MlConcurrencyGate>,
}

fn app_with_events(
    database: Database,
    web_dist: Option<PathBuf>,
    events: broadcast::Sender<Value>,
    vault_ttl: Duration,
    options: AppBuildOptions,
) -> Router {
    let AppBuildOptions {
        setup_token_hash,
        secure_cookies,
        trusted_proxy_cidrs,
        ml_gate,
    } = options;
    let state = AppState::new(
        database,
        events,
        vault_ttl,
        setup_token_hash,
        secure_cookies,
        trusted_proxy_cidrs,
        ml_gate,
    );

    let vault_session = Router::new()
        .route("/vault/lock", post(api::vault_lock))
        .route("/vault/import", post(api::vault_import))
        .route("/vault/export", post(api::vault_export))
        .route(
            "/vault/assets",
            post(api::vault_upload_asset).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
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
        .route("/vault/clusters", get(api::list_clusters))
        .route("/vault/clusters/merge", post(api::merge_clusters))
        .route(
            "/vault/clusters/{id}",
            axum::routing::patch(api::rename_cluster),
        )
        .route("/vault/clusters/{id}/assets", get(api::cluster_assets))
        .route("/vault/clusters/{id}/split", post(api::split_cluster))
        .route("/vault/review/candidates", get(api::review_candidates))
        .route(
            "/vault/review/candidates/{face_id}",
            post(api::review_candidate),
        )
        .route("/vault/ml/analyze-all", post(api::vault_analyze_all))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            vault::require_session,
        ));

    let vault_auth = Router::new()
        .route("/vault/init", post(api::vault_init))
        .route("/vault/unlock", post(api::vault_unlock))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            security::limit_auth_attempts,
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
        .route("/ml/status", get(api::ml_status))
        .route("/ml/analyze-all", post(api::analyze_all))
        .route("/ml/recluster", post(api::recluster))
        .route("/clusters", get(api::list_clusters))
        .route("/clusters/merge", post(api::merge_clusters))
        .route("/clusters/{id}", axum::routing::patch(api::rename_cluster))
        .route("/clusters/{id}/assets", get(api::cluster_assets))
        .route("/clusters/{id}/split", post(api::split_cluster))
        .route("/review/candidates", get(api::review_candidates))
        .route("/review/candidates/{face_id}", post(api::review_candidate))
        .route("/jobs", get(api::jobs))
        .route("/jobs/{id}/cancel", post(api::cancel_job))
        .route(
            "/settings",
            get(api::get_settings).patch(api::patch_settings),
        )
        .route("/vault/status", get(api::vault_status))
        .merge(vault_auth)
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
        .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_PROGRESS_TIMEOUT))
        .layer(ResponseBodyTimeoutLayer::new(HTTP_BODY_PROGRESS_TIMEOUT))
        .layer(middleware::from_fn(request_deadline))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                let uri = if request.uri().path().starts_with("/api/vault/") {
                    "/api/vault/*".to_owned()
                } else {
                    request.extensions().get::<MatchedPath>().map_or_else(
                        || {
                            if request.uri().path().starts_with("/api") {
                                "/api/<unmatched>".to_owned()
                            } else {
                                "/<static>".to_owned()
                            }
                        },
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

async fn request_deadline(request: Request, next: Next) -> Response {
    match tokio::time::timeout(HTTP_REQUEST_TIMEOUT, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error::ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "request deadline exceeded",
        )
        .into_response(),
    }
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS));
    let by_ip = Arc::new(Mutex::new(HashMap::new()));
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    tokio::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            _ = &mut shutdown => {
                let _ = shutdown_tx.send(true);
                break;
            }
            permit = slots.clone().acquire_owned() => permit.expect("HTTP admission semaphore closed"),
        };
        let accepted = tokio::select! {
            _ = &mut shutdown => {
                drop(permit);
                let _ = shutdown_tx.send(true);
                break;
            }
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                drop(permit);
                tracing::warn!(error = %error, "HTTP accept failed");
                continue;
            }
        };
        let Some(permit) = HttpConnectionPermit::try_new(permit, peer.ip(), Arc::clone(&by_ip))
        else {
            drop(stream);
            continue;
        };
        let _ = stream.set_nodelay(true);
        let service = router.clone().layer(Extension(ConnectInfo(peer)));
        let mut shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            let _permit = permit;
            let mut builder = ConnectionBuilder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(HTTP_HEADER_TIMEOUT)
                .max_buf_size(MAX_HTTP_HEADER_BYTES)
                .keep_alive(true);
            builder
                .http2()
                .timer(TokioTimer::new())
                .max_header_list_size(MAX_HTTP_HEADER_BYTES as u32)
                .max_concurrent_streams(MAX_HTTP2_STREAMS_PER_CONNECTION)
                .keep_alive_interval(Some(HTTP_BODY_PROGRESS_TIMEOUT))
                .keep_alive_timeout(HTTP_HEADER_TIMEOUT);
            let io = TokioIo::new(stream);
            let service = TowerToHyperService::new(service);
            let connection = builder.serve_connection_with_upgrades(io, service);
            tokio::pin!(connection);
            let lifetime = tokio::time::sleep(HTTP_CONNECTION_MAX_LIFETIME);
            tokio::pin!(lifetime);
            tokio::select! {
                result = &mut connection => {
                    if let Err(error) = result {
                        tracing::debug!(error = %error, "HTTP connection closed with error");
                    }
                }
                _ = shutdown_rx.changed() => {
                    connection.as_mut().graceful_shutdown();
                    let _ = tokio::time::timeout(
                        HTTP_GRACEFUL_DRAIN_TIMEOUT,
                        connection.as_mut(),
                    ).await;
                }
                _ = &mut lifetime => {
                    connection.as_mut().graceful_shutdown();
                    if tokio::time::timeout(
                        HTTP_GRACEFUL_DRAIN_TIMEOUT,
                        connection.as_mut(),
                    ).await.is_err() {
                        tracing::debug!("HTTP connection exceeded its hard lifetime");
                    }
                }
            }
        });
    }
    let _ = tokio::time::timeout(
        Duration::from_secs(30),
        slots.acquire_many_owned(MAX_HTTP_CONNECTIONS as u32),
    )
    .await;
    Ok(())
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
        trusted_proxy_cidrs,
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
    let ml_settings = Settings::new(database.clone());
    let ml_gate = if ml_settings.ml_enabled().context("read ml.enabled")?
        && let Some(socket_path) = ml_settings
            .ml_socket_path()
            .context("read ml.socket_path")?
    {
        let gate = MlConcurrencyGate::new(
            usize::try_from(
                ml_settings
                    .ml_concurrency()
                    .context("read jobs.ml_concurrency")?,
            )
            .unwrap_or(1),
        );
        let analyze_client = MlClient::new(socket_path.clone());
        let analyze_gate = gate.clone();
        runner.register_handler(ML_ANALYZE_JOB_KIND, move |database, job| {
            let _permit = analyze_gate.acquire();
            MlService::new(database.clone(), analyze_client.clone()).handle_analyze_job(job)
        });
        let recluster_client = MlClient::new(socket_path);
        let recluster_gate = gate.clone();
        runner.register_handler(ML_RECLUSTER_JOB_KIND, move |database, job| {
            let _permit = recluster_gate.acquire();
            MlService::new(database.clone(), recluster_client.clone()).handle_recluster_job(job)
        });
        Some(gate)
    } else {
        None
    };
    runner.start().context("start job runner")?;

    let purge_task = tokio::spawn(run_purge_loop(purge));
    tracing::info!(address = %addr, "Illumia server listening");

    let router = app_with_events(
        database,
        web_dist,
        events,
        Duration::from_secs(15 * 60),
        AppBuildOptions {
            setup_token_hash,
            secure_cookies,
            trusted_proxy_cidrs,
            ml_gate,
        },
    );
    let serve_result = serve_http(listener, router, shutdown_signal()).await;

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
