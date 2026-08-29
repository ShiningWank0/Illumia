use std::{
    collections::BTreeMap,
    io,
    path::{Component, Path as FsPath, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{
        Extension, Multipart, Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_NONE_MATCH,
        },
    },
    response::{IntoResponse, Response},
};
use futures_util::stream;
use illumia_core::{
    PurgeService,
    assets::{Asset, AssetService, DuplicatePair, Lifecycle},
    blake3,
    chrono::{DateTime, Utc},
    db::Database,
    jobs::{Job, JobQueue, JobState},
    ml::{
        ClusterAssetFace, ClusterSummary, FaceRecord, ML_RECLUSTER_JOB_KIND,
        ML_VAULT_ANALYZE_JOB_KIND, MlAnalyzePayload, MlService, enqueue_analyze_all_for_model,
        enqueue_recluster, enqueue_vault_analyze_all,
    },
    ml_client::MlClient,
    search::SearchService,
    settings::{
        MAX_CLUSTER_SIZE_VALUE, MAX_JOB_CONCURRENCY, MAX_ML_CONCURRENCY, MAX_RETENTION_DAYS,
        MIN_CLUSTER_SIZE_VALUE, MIN_JOB_CONCURRENCY, QualityGate, Settings,
    },
    stacks::{ChapterInput, MangaStack, StackChapter, StackPage, StackService, StackSummary},
    thumbnails,
    timeline::{BucketItem, Granularity, TimelineService},
    uuid::Uuid,
    vault::{self as core_vault, VaultHandle},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tower_http::services::ServeFile;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
    security::Authenticated,
    vault::VaultAccess,
};

const IMMUTABLE_CACHE: &str = "private,max-age=31536000,immutable";
const NO_STORE: &str = "no-store";
const MAX_EXISTS_HASHES: usize = 4096;
const MIN_NEW_VAULT_PASSWORD_CHARS: usize = 12;
const MAX_VAULT_PASSWORD_BYTES: usize = 1024;
const RECOVERY_KEY_CHARS: usize = 52;
const VAULT_STREAM_CHANNEL_CAPACITY: usize = 2;
#[cfg(not(test))]
const VAULT_STREAM_BACKPRESSURE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const VAULT_STREAM_BACKPRESSURE_TIMEOUT: Duration = Duration::from_millis(100);
const VAULT_STREAM_BACKPRESSURE_RETRY: Duration = Duration::from_millis(10);

#[derive(Serialize)]
pub struct AssetResponse {
    id: String,
    filename: String,
    width: u32,
    height: u32,
    ratio: f64,
    thumbhash: Option<String>,
    taken_at: String,
    created_at: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    duplicate_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trashed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    purge_after: Option<String>,
}

impl From<Asset> for AssetResponse {
    fn from(asset: Asset) -> Self {
        Self {
            id: asset.id,
            filename: asset.original_name,
            width: asset.width,
            height: asset.height,
            ratio: asset.aspect_ratio,
            thumbhash: asset.thumbhash.as_deref().map(base64),
            taken_at: asset.taken_at,
            created_at: asset.uploaded_at,
            status: lifecycle_name(asset.lifecycle),
            duplicate_of: asset.duplicate_of,
            trashed_at: asset.trashed_at,
            purge_after: asset.purge_after,
        }
    }
}

#[derive(Serialize)]
pub struct ClusterAssetResponse {
    #[serde(flatten)]
    asset: AssetResponse,
    faces: Vec<ClusterAssetFace>,
}

#[derive(Serialize)]
struct UploadResponse {
    id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    duplicate_of: Option<String>,
}

#[derive(Deserialize)]
pub struct ExistsRequest {
    hashes: Vec<String>,
}

#[derive(Serialize)]
pub struct ExistsResponse {
    exists: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub struct BucketQuery {
    granularity: GranularityParam,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GranularityParam {
    Day,
    Month,
    Year,
}

impl From<GranularityParam> for Granularity {
    fn from(value: GranularityParam) -> Self {
        match value {
            GranularityParam::Day => Self::Day,
            GranularityParam::Month => Self::Month,
            GranularityParam::Year => Self::Year,
        }
    }
}

#[derive(Serialize)]
pub struct BucketResponse {
    key: String,
    count: u64,
}

#[derive(Serialize)]
struct BucketItemResponse {
    id: String,
    ratio: f64,
    thumbhash: Option<String>,
    taken_at: String,
}

impl From<BucketItem> for BucketItemResponse {
    fn from(item: BucketItem) -> Self {
        Self {
            id: item.id,
            ratio: item.ratio,
            thumbhash: item.thumbhash.as_deref().map(base64),
            taken_at: item.taken_at,
        }
    }
}

#[derive(Serialize)]
pub struct DuplicateResponse {
    dup: AssetResponse,
    original: AssetResponse,
    purge_after: Option<String>,
}

impl From<DuplicatePair> for DuplicateResponse {
    fn from(pair: DuplicatePair) -> Self {
        let purge_after = pair.duplicate.purge_after.clone();
        Self {
            dup: pair.duplicate.into(),
            original: pair.original.into(),
            purge_after,
        }
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
pub struct SearchResponse {
    assets: Vec<AssetResponse>,
    stacks: Vec<StackSummaryResponse>,
    clusters: Vec<ClusterSummary>,
}

#[derive(Deserialize)]
pub struct RenameClusterRequest {
    name: String,
}

#[derive(Deserialize)]
pub struct MergeClustersRequest {
    from_id: String,
    into_id: String,
}

#[derive(Deserialize)]
pub struct SplitClusterRequest {
    face_ids: Vec<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewAction {
    Accept,
    Reject,
}

#[derive(Deserialize)]
pub struct ReviewCandidateRequest {
    action: ReviewAction,
}

#[derive(Serialize)]
pub struct ReviewCandidateResponse {
    face: FaceRecord,
    asset: AssetResponse,
}

#[derive(Deserialize)]
pub struct VaultInitRequest {
    password: String,
}

#[derive(Serialize)]
pub struct VaultInitResponse {
    recovery_key: String,
}

#[derive(Deserialize)]
pub struct VaultUnlockRequest {
    password: Option<String>,
    recovery_key: Option<String>,
}

#[derive(Serialize)]
pub struct VaultUnlockResponse {
    vault_session: String,
    expires_at: String,
}

#[derive(Serialize)]
pub struct VaultStatusResponse {
    initialized: bool,
    unlocked: bool,
}

#[derive(Deserialize)]
pub struct VaultTransferRequest {
    asset_ids: Option<Vec<String>>,
    stack_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateStackRequest {
    title: String,
    asset_ids: Vec<String>,
}

#[derive(Deserialize)]
pub struct PatchStackRequest {
    title: Option<String>,
    cover_asset_id: Option<String>,
}

#[derive(Deserialize)]
pub struct ReplaceStackStructureRequest {
    chapters: Vec<ChapterRequest>,
}

#[derive(Deserialize)]
pub struct ChapterRequest {
    title: Option<String>,
    pages: Vec<String>,
}

#[derive(Deserialize)]
pub struct AddStackPagesRequest {
    asset_ids: Vec<String>,
    chapter_id: Option<String>,
}

#[derive(Deserialize)]
pub struct PatchStackPageRequest {
    show_in_timeline: bool,
}

#[derive(Serialize)]
pub struct StackSummaryResponse {
    id: String,
    title: String,
    cover_asset_id: Option<String>,
    chapter_count: u32,
    page_count: u32,
    created_at: String,
    updated_at: String,
}

impl From<StackSummary> for StackSummaryResponse {
    fn from(stack: StackSummary) -> Self {
        Self {
            id: stack.id,
            title: stack.title,
            cover_asset_id: stack.cover_asset_id,
            chapter_count: stack.chapter_count,
            page_count: stack.page_count,
            created_at: stack.created_at,
            updated_at: stack.updated_at,
        }
    }
}

#[derive(Serialize)]
pub struct StackResponse {
    id: String,
    title: String,
    cover_asset_id: Option<String>,
    created_at: String,
    updated_at: String,
    chapters: Vec<StackChapterResponse>,
}

impl From<MangaStack> for StackResponse {
    fn from(stack: MangaStack) -> Self {
        Self {
            id: stack.id,
            title: stack.title,
            cover_asset_id: stack.cover_asset_id,
            created_at: stack.created_at,
            updated_at: stack.updated_at,
            chapters: stack
                .chapters
                .into_iter()
                .map(StackChapterResponse::from)
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct StackChapterResponse {
    id: String,
    chapter_no: u32,
    title: Option<String>,
    pages: Vec<StackPageResponse>,
}

impl From<StackChapter> for StackChapterResponse {
    fn from(chapter: StackChapter) -> Self {
        Self {
            id: chapter.id,
            chapter_no: chapter.chapter_no,
            title: chapter.title,
            pages: chapter
                .pages
                .into_iter()
                .map(StackPageResponse::from)
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct StackPageResponse {
    page_no: u32,
    show_in_timeline: bool,
    asset: AssetResponse,
}

impl From<StackPage> for StackPageResponse {
    fn from(page: StackPage) -> Self {
        Self {
            page_no: page.page_no,
            show_in_timeline: page.show_in_timeline,
            asset: page.asset.into(),
        }
    }
}

#[derive(Deserialize)]
pub struct JobsQuery {
    state: Option<String>,
}

#[derive(Serialize)]
pub struct JobResponse {
    id: String,
    kind: String,
    payload: Value,
    state: &'static str,
    priority: i64,
    progress: f64,
    error: Option<String>,
    created_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    cancel_requested: bool,
}

impl From<Job> for JobResponse {
    fn from(job: Job) -> Self {
        Self {
            id: job.id,
            kind: job.kind,
            payload: serde_json::from_str(&job.payload).unwrap_or(Value::Null),
            state: job_state_name(job.state),
            priority: job.priority,
            progress: job.progress,
            error: job.error,
            created_at: job.created_at,
            started_at: job.started_at,
            finished_at: job.finished_at,
            cancel_requested: job.cancel_requested,
        }
    }
}

#[derive(Serialize)]
pub struct CancelResponse {
    cancelled: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct JobSnapshot {
    id: String,
    state: &'static str,
    progress_bits: u64,
}

#[derive(Clone)]
struct SelectedDatabase {
    database: Database,
    vault: Option<VaultHandle>,
    vault_generation: Option<u64>,
    vault_sessions: Option<crate::vault::VaultSessionManager>,
}

impl SelectedDatabase {
    fn from_request(state: &AppState, access: Option<Extension<VaultAccess>>) -> Self {
        match access {
            Some(Extension(access)) => {
                let guard_handle = access.handle.clone();
                let guard_sessions = state.vault.clone();
                let generation = access.generation;
                Self {
                    database: access.handle.db.with_access_guard(move || {
                        ensure_active_vault_session(&guard_handle, &guard_sessions, generation)
                    }),
                    vault: Some(access.handle),
                    vault_generation: Some(generation),
                    vault_sessions: Some(state.vault.clone()),
                }
            }
            None => Self {
                database: state.database.clone(),
                vault: None,
                vault_generation: None,
                vault_sessions: None,
            },
        }
    }

    async fn run_blocking<T>(
        &self,
        state: &AppState,
        operation: impl FnOnce(Database) -> illumia_core::db::Result<T> + Send + 'static,
    ) -> ApiResult<T>
    where
        T: Send + 'static,
    {
        let permit = state.try_blocking_db_slot()?;
        let database = self.database.clone();
        let vault = self.vault.clone();
        let vault_generation = self.vault_generation;
        let vault_sessions = self.vault_sessions.clone();
        let is_vault = vault.is_some();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            ensure_vault_generation(&vault, &vault_sessions, vault_generation)?;
            let result = operation(database);
            ensure_vault_generation(&vault, &vault_sessions, vault_generation)?;
            result
        })
        .await
        .map_err(ApiError::internal)?;
        selected_result(is_vault, result)
    }
}

fn ensure_vault_generation(
    vault: &Option<VaultHandle>,
    sessions: &Option<crate::vault::VaultSessionManager>,
    generation: Option<u64>,
) -> illumia_core::db::Result<()> {
    let Some(handle) = vault else {
        return Ok(());
    };
    let Some((sessions, generation)) = sessions.as_ref().zip(generation) else {
        return Err(illumia_core::db::Error::VaultAuthenticationFailed);
    };
    ensure_active_vault_session(handle, sessions, generation)
}

fn ensure_active_vault_session(
    handle: &VaultHandle,
    sessions: &crate::vault::VaultSessionManager,
    generation: u64,
) -> illumia_core::db::Result<()> {
    handle.ensure_active()?;
    if sessions.generation_active(generation) {
        Ok(())
    } else {
        Err(illumia_core::db::Error::VaultAuthenticationFailed)
    }
}

fn selected_result<T>(vault: bool, result: illumia_core::db::Result<T>) -> ApiResult<T> {
    if vault {
        result.map_err(vault_core_error)
    } else {
        result.map_err(ApiError::from)
    }
}

async fn run_main_blocking<T>(
    state: &AppState,
    operation: impl FnOnce(Database) -> illumia_core::db::Result<T> + Send + 'static,
) -> ApiResult<T>
where
    T: Send + 'static,
{
    let permit = state.try_blocking_db_slot()?;
    let database = state.database.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(database)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::from)
}

async fn run_main_api_blocking<T>(
    state: &AppState,
    operation: impl FnOnce(Database) -> ApiResult<T> + Send + 'static,
) -> ApiResult<T>
where
    T: Send + 'static,
{
    let permit = state.try_blocking_db_slot()?;
    let database = state.database.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(database)
    })
    .await
    .map_err(ApiError::internal)?
}

/// Initializes vault key material without exposing whether it exists to unauthenticated callers.
///
/// `vault: no-log`
pub async fn vault_init(
    State(state): State<AppState>,
    Json(request): Json<VaultInitRequest>,
) -> ApiResult<(StatusCode, Json<VaultInitResponse>)> {
    if request.password.chars().count() < MIN_NEW_VAULT_PASSWORD_CHARS
        || request.password.len() > MAX_VAULT_PASSWORD_BYTES
    {
        return Err(ApiError::bad_request(
            "vault password must contain 12 to 1024 characters",
        ));
    }
    let _permit = state.security.try_argon2_slot()?;
    let data_root = state.database.data_root().to_path_buf();
    let recovery_key =
        tokio::task::spawn_blocking(move || core_vault::init(data_root, &request.password))
            .await
            .map_err(|_| ApiError::internal_silent())?
            .map_err(vault_management_error)?;
    Ok((
        StatusCode::CREATED,
        Json(VaultInitResponse { recovery_key }),
    ))
}

/// Replaces the active vault session after password or recovery-key authentication.
///
/// `vault: no-log`
pub async fn vault_unlock(
    State(state): State<AppState>,
    Json(request): Json<VaultUnlockRequest>,
) -> ApiResult<Json<VaultUnlockResponse>> {
    let _permit = state.security.try_argon2_slot()?;
    let manager = state.vault.clone();
    let issued = match (request.password, request.recovery_key) {
        (Some(password), None)
            if !password.is_empty() && password.len() <= MAX_VAULT_PASSWORD_BYTES =>
        {
            tokio::task::spawn_blocking(move || manager.unlock_with_password(&password))
                .await
                .map_err(|_| ApiError::internal_silent())?
        }
        (None, Some(recovery_key))
            if recovery_key.len() == RECOVERY_KEY_CHARS && recovery_key.is_ascii() =>
        {
            tokio::task::spawn_blocking(move || manager.unlock_with_recovery(&recovery_key))
                .await
                .map_err(|_| ApiError::internal_silent())?
        }
        _ => {
            return Err(ApiError::bad_request(
                "provide exactly one of password or recovery_key",
            ));
        }
    }
    .map_err(vault_unlock_error)?;
    Ok(Json(VaultUnlockResponse {
        vault_session: issued.token,
        expires_at: issued.expires_at,
    }))
}

/// Destroys the currently authenticated in-memory vault session.
///
/// `vault: no-log`
pub async fn vault_lock(
    State(state): State<AppState>,
    Extension(access): Extension<VaultAccess>,
) -> StatusCode {
    state.vault.lock(&access);
    StatusCode::NO_CONTENT
}

pub async fn vault_status(State(state): State<AppState>) -> ApiResult<Json<VaultStatusResponse>> {
    let initialized = state.vault.initialized();
    Ok(Json(VaultStatusResponse {
        initialized,
        unlocked: initialized && state.vault.unlocked(),
    }))
}

/// Moves assets or a whole stack into the unlocked vault and fills missing image variants.
///
/// `vault: no-log`
pub async fn vault_import(
    State(state): State<AppState>,
    Extension(access): Extension<VaultAccess>,
    Json(request): Json<VaultTransferRequest>,
) -> ApiResult<StatusCode> {
    validate_vault_transfer_request(&request)?;
    let _permit = state.security.try_ingest_slot()?;
    let main = state.database;
    let sessions = state.vault;
    let generation = access.generation;
    let vault = access.handle;
    tokio::task::spawn_blocking(move || -> illumia_core::db::Result<()> {
        ensure_active_vault_session(&vault, &sessions, generation)?;
        let imported_ids = match (request.asset_ids, request.stack_id) {
            (Some(asset_ids), None) => {
                core_vault::import_assets(&main, &vault, &asset_ids)?;
                asset_ids
            }
            (None, Some(stack_id)) => {
                core_vault::import_stack(&main, &vault, &stack_id)?;
                stack_asset_ids_for_vault(&vault.db, &stack_id)?
            }
            _ => return Err(illumia_core::db::Error::EmptyVaultTransfer),
        };
        for asset_id in imported_ids {
            ensure_active_vault_session(&vault, &sessions, generation)?;
            vault.generate_thumbnails(&asset_id)?;
        }
        ensure_active_vault_session(&vault, &sessions, generation)?;
        Ok(())
    })
    .await
    .map_err(|_| ApiError::internal_silent())?
    .map_err(vault_core_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Moves assets or a whole stack out of the unlocked vault.
///
/// `vault: no-log`
pub async fn vault_export(
    State(state): State<AppState>,
    Extension(access): Extension<VaultAccess>,
    Json(request): Json<VaultTransferRequest>,
) -> ApiResult<StatusCode> {
    validate_vault_transfer_request(&request)?;
    let _permit = state.security.try_ingest_slot()?;
    let main = state.database;
    let sessions = state.vault;
    let generation = access.generation;
    let vault = access.handle;
    tokio::task::spawn_blocking(move || {
        ensure_active_vault_session(&vault, &sessions, generation)?;
        match (request.asset_ids, request.stack_id) {
            (Some(asset_ids), None) => core_vault::export_assets(&vault, &main, &asset_ids)?,
            (None, Some(stack_id)) => core_vault::export_stack(&vault, &main, &stack_id)?,
            _ => return Err(illumia_core::db::Error::EmptyVaultTransfer),
        }
        ensure_active_vault_session(&vault, &sessions, generation)
    })
    .await
    .map_err(|_| ApiError::internal_silent())?
    .map_err(vault_core_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn upload_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> ApiResult<Response> {
    let _permit = state.security.try_ingest_slot()?;
    let (filename, bytes, taken_at) = receive_upload(headers, multipart).await?;

    let database = state.database.clone();
    let result = tokio::task::spawn_blocking(move || {
        let result = AssetService::new(database.clone()).ingest(&bytes, &filename, taken_at)?;
        thumbnails::enqueue_thumbnail(&database, &result.asset.id)?;
        Ok::<_, illumia_core::db::Error>(result)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::from)?;
    state.notify_assets_added(&result.asset.taken_at_local_date);

    upload_response(result)
}

/// Main libraryへ平文を書かず、unlock済みVaultへ直接暗号化して取り込む。
///
/// `vault: no-log`
pub async fn vault_upload_asset(
    State(state): State<AppState>,
    Extension(access): Extension<VaultAccess>,
    headers: HeaderMap,
    multipart: Multipart,
) -> ApiResult<Response> {
    let _permit = state.security.try_ingest_slot()?;
    let (filename, bytes, taken_at) = receive_upload(headers, multipart).await?;
    let sessions = state.vault;
    let generation = access.generation;
    let handle = access.handle;
    let result = tokio::task::spawn_blocking(move || {
        ensure_active_vault_session(&handle, &sessions, generation)?;
        let result = handle.ingest(&bytes, &filename, taken_at)?;
        drop(bytes);
        ensure_active_vault_session(&handle, &sessions, generation)?;
        handle.generate_thumbnails(&result.asset.id)?;
        ensure_active_vault_session(&handle, &sessions, generation)?;
        Ok::<_, illumia_core::db::Error>(result)
    })
    .await
    .map_err(|_| ApiError::internal_silent())?
    .map_err(vault_core_error)?;
    upload_response(result)
}

async fn receive_upload(
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<(String, Bytes, Option<DateTime<Utc>>)> {
    let expected_hash = headers
        .get("X-Illumia-Checksum")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("X-Illumia-Checksum is required"))?
        .to_ascii_lowercase();
    let expected_bytes = decode_hash(&expected_hash)?;
    let taken_at = headers
        .get("X-Illumia-Taken-At")
        .map(|value| {
            let raw = value
                .to_str()
                .map_err(|_| ApiError::bad_request("X-Illumia-Taken-At is invalid"))?;
            DateTime::parse_from_rfc3339(raw)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| ApiError::bad_request("X-Illumia-Taken-At must be RFC3339"))
        })
        .transpose()?;

    let mut upload = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("invalid multipart body: {error}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        if upload.is_some() {
            return Err(ApiError::bad_request(
                "multipart body must contain one file field",
            ));
        }
        let filename = field
            .file_name()
            .map(ToOwned::to_owned)
            .ok_or_else(|| ApiError::bad_request("file field requires a filename"))?;
        illumia_core::images::normalized_extension(&filename).map_err(ApiError::from)?;
        let bytes = field
            .bytes()
            .await
            .map_err(|error| ApiError::bad_request(format!("invalid file field: {error}")))?;
        if bytes.len() > illumia_core::images::MAX_ASSET_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "image exceeds the 128 MiB limit",
            ));
        }
        upload = Some((filename, bytes));
    }
    let (filename, bytes) =
        upload.ok_or_else(|| ApiError::bad_request("multipart file field is required"))?;
    if blake3::hash(&bytes).as_bytes() != expected_bytes.as_slice() {
        return Err(ApiError::bad_request("checksum mismatch"));
    }
    Ok((filename, bytes, taken_at))
}

fn upload_response(result: illumia_core::assets::IngestResult) -> ApiResult<Response> {
    let duplicate_of = result.duplicate_of;
    let status = if duplicate_of.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(UploadResponse {
            id: result.asset.id,
            status: if duplicate_of.is_some() {
                "duplicate"
            } else {
                "created"
            },
            duplicate_of,
        }),
    )
        .into_response())
}

pub async fn assets_exist(
    State(state): State<AppState>,
    Json(request): Json<ExistsRequest>,
) -> ApiResult<Json<ExistsResponse>> {
    if request.hashes.len() > MAX_EXISTS_HASHES {
        return Err(ApiError::bad_request("too many hashes"));
    }
    let mut decoded = Vec::with_capacity(request.hashes.len());
    for hash in request.hashes {
        let normalized = hash.to_ascii_lowercase();
        decoded.push((normalized.clone(), decode_hash(&normalized)?));
    }
    let exists = run_main_blocking(&state, move |database| {
        database.with_connection(|connection| {
            let mut found = BTreeMap::new();
            let mut statement = connection.prepare(
                "SELECT id FROM assets
                 WHERE hash = ?1
                   AND lifecycle = 'active'
                   AND duplicate_of IS NULL",
            )?;
            for (encoded, hash) in &decoded {
                let mut rows = statement.query([hash.as_slice()])?;
                if let Some(row) = rows.next()? {
                    found.insert(encoded.clone(), row.get(0)?);
                }
            }
            Ok(found)
        })
    })
    .await?;
    Ok(Json(ExistsResponse { exists }))
}

pub async fn asset_metadata(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<AssetResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let asset = selected
        .run_blocking(&state, move |database| AssetService::new(database).get(&id))
        .await?
        .ok_or_else(|| ApiError::not_found("asset not found"))?;
    Ok(Json(asset.into()))
}

pub async fn original(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    request: Request,
) -> ApiResult<Response> {
    let selected = SelectedDatabase::from_request(&state, access);
    let asset = selected
        .run_blocking(&state, move |database| AssetService::new(database).get(&id))
        .await?
        .ok_or_else(|| ApiError::not_found("asset not found"))?;
    let mut response = if let Some(vault) = &selected.vault {
        let mut response = vault_blob_response(&state, vault, &asset.library_path).await?;
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static(content_type_for_extension(&asset.ext)),
        );
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
        response.headers_mut().insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&asset.size.to_string()).map_err(ApiError::internal)?,
        );
        response
    } else {
        let path = asset_path(&selected.database, &asset.library_path)?;
        let response = serve_file(path, request).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ApiError::not_found("original file not found"));
        }
        response
    };
    let disposition = format!(
        "attachment; filename=\"asset.{}\"; filename*=UTF-8''{}",
        asset.ext,
        percent_encode(&asset.original_name)
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(ApiError::internal)?,
    );
    Ok(response)
}

pub async fn thumbnail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    request: Request,
) -> ApiResult<Response> {
    image_variant(state, id, access, request, "thumbnail", "t").await
}

pub async fn preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    request: Request,
) -> ApiResult<Response> {
    image_variant(state, id, access, request, "preview", "p").await
}

pub async fn trash_asset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<AssetResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let asset = selected
        .run_blocking(&state, move |database| {
            AssetService::new(database).trash(&id)
        })
        .await?;
    Ok(Json(asset.into()))
}

pub async fn restore_asset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<AssetResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let main_database = selected.vault.is_none();
    let asset = selected
        .run_blocking(&state, move |database| {
            AssetService::new(database).restore(&id)
        })
        .await?;
    // vault 側の復元はバケットキー (日付) を WS に流さない (docs/06 ログ抑制)
    if main_database {
        state.notify_assets_added(&asset.taken_at_local_date);
    }
    Ok(Json(asset.into()))
}

pub async fn timeline_buckets(
    State(state): State<AppState>,
    Query(query): Query<BucketQuery>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<BucketResponse>>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let buckets = selected
        .run_blocking(&state, move |database| {
            TimelineService::new(database).bucket_records(query.granularity.into())
        })
        .await?
        .into_iter()
        .map(|bucket| BucketResponse {
            key: bucket.key,
            count: bucket.count,
        })
        .collect();
    Ok(Json(buckets))
}

pub async fn timeline_bucket(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<BucketQuery>,
    headers: HeaderMap,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Response> {
    validate_bucket_granularity(&key, query.granularity)?;
    let selected = SelectedDatabase::from_request(&state, access);
    let items: Vec<_> = selected
        .run_blocking(&state, move |database| {
            TimelineService::new(database).bucket_items(&key)
        })
        .await?
        .into_iter()
        .map(BucketItemResponse::from)
        .collect();
    let encoded = serde_json::to_vec(&items).map_err(ApiError::internal)?;
    let etag = format!("\"{}\"", blake3::hash(&encoded).to_hex());
    if headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let mut response = (
        StatusCode::OK,
        [(CONTENT_TYPE, "application/json")],
        encoded,
    )
        .into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&etag).map_err(ApiError::internal)?,
    );
    Ok(response)
}

pub async fn trash(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<AssetResponse>>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let assets = selected
        .run_blocking(&state, move |database| {
            AssetService::new(database).list_trash()
        })
        .await?
        .into_iter()
        .map(AssetResponse::from)
        .collect();
    Ok(Json(assets))
}

pub async fn duplicates(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<DuplicateResponse>>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let duplicates = selected
        .run_blocking(&state, move |database| {
            AssetService::new(database).list_duplicates()
        })
        .await?
        .into_iter()
        .map(DuplicateResponse::from)
        .collect();
    Ok(Json(duplicates))
}

pub async fn purge_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<StatusCode> {
    let selected = SelectedDatabase::from_request(&state, access);
    selected
        .run_blocking(&state, move |database| {
            PurgeService::new(database).purge_now(&id)
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_stack(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<CreateStackRequest>,
) -> ApiResult<(StatusCode, Json<StackResponse>)> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected
        .run_blocking(&state, move |database| {
            StackService::new(database).create(&request.title, &request.asset_ids)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(stack.into())))
}

pub async fn list_stacks(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<StackSummaryResponse>>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stacks = selected
        .run_blocking(&state, |database| StackService::new(database).list())
        .await?
        .into_iter()
        .map(StackSummaryResponse::from)
        .collect();
    Ok(Json(stacks))
}

pub async fn get_stack(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<StackResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected
        .run_blocking(&state, move |database| StackService::new(database).get(&id))
        .await?
        .ok_or_else(|| ApiError::not_found("manga stack not found"))?;
    Ok(Json(stack.into()))
}

pub async fn patch_stack(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<PatchStackRequest>,
) -> ApiResult<Json<StackResponse>> {
    if request.title.is_none() && request.cover_asset_id.is_none() {
        return Err(ApiError::bad_request(
            "title or cover_asset_id must be provided",
        ));
    }
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected
        .run_blocking(&state, move |database| {
            StackService::new(database).update_metadata(
                &id,
                request.title.as_deref(),
                request.cover_asset_id.as_deref(),
            )
        })
        .await?;
    Ok(Json(stack.into()))
}

pub async fn delete_stack(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<StatusCode> {
    let selected = SelectedDatabase::from_request(&state, access);
    selected
        .run_blocking(&state, move |database| {
            StackService::new(database).delete_stack(&id)
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn replace_stack_structure(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<ReplaceStackStructureRequest>,
) -> ApiResult<Json<StackResponse>> {
    let chapters = request
        .chapters
        .into_iter()
        .map(|chapter| ChapterInput {
            title: chapter.title,
            pages: chapter.pages,
        })
        .collect::<Vec<_>>();
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected
        .run_blocking(&state, move |database| {
            StackService::new(database).replace_structure(&id, &chapters)
        })
        .await?;
    Ok(Json(stack.into()))
}

pub async fn add_stack_pages(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<AddStackPagesRequest>,
) -> ApiResult<Json<StackResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected
        .run_blocking(&state, move |database| {
            StackService::new(database).add_pages(
                &id,
                &request.asset_ids,
                request.chapter_id.as_deref(),
            )
        })
        .await?;
    Ok(Json(stack.into()))
}

pub async fn remove_stack_page(
    State(state): State<AppState>,
    Path((id, asset_id)): Path<(String, String)>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<StatusCode> {
    let selected = SelectedDatabase::from_request(&state, access);
    selected
        .run_blocking(&state, move |database| {
            StackService::new(database).remove_page(&id, &asset_id)
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn patch_stack_page(
    State(state): State<AppState>,
    Path((id, asset_id)): Path<(String, String)>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<PatchStackPageRequest>,
) -> ApiResult<Json<StackResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected
        .run_blocking(&state, move |database| {
            StackService::new(database).set_page_flag(&id, &asset_id, request.show_in_timeline)
        })
        .await?;
    Ok(Json(stack.into()))
}

pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<SearchResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let query = query.q.trim().to_owned();
    if query.len() > illumia_core::stacks::MAX_SEARCH_BYTES
        || query.chars().count() > illumia_core::stacks::MAX_SEARCH_CHARS
    {
        return Err(ApiError::bad_request("search query is too long"));
    }
    if query.is_empty() {
        return Ok(Json(SearchResponse {
            assets: Vec::new(),
            stacks: Vec::new(),
            clusters: Vec::new(),
        }));
    }
    let result = selected
        .run_blocking(&state, move |database| {
            SearchService::new(database).search(&query)
        })
        .await?;
    let stacks = result
        .stacks
        .into_iter()
        .map(StackSummaryResponse::from)
        .collect();
    let assets = result.assets.into_iter().map(AssetResponse::from).collect();
    Ok(Json(SearchResponse {
        assets,
        stacks,
        clusters: result.clusters,
    }))
}

pub async fn ml_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let (enabled, socket_path) = ml_configuration(&state).await?;
    let sidecar = if enabled {
        if let Some(path) = socket_path {
            state.ml_health(path).await
        } else {
            None
        }
    } else {
        None
    };
    Ok(Json(json!({"enabled": enabled, "sidecar": sidecar})))
}

pub async fn analyze_all(State(state): State<AppState>) -> ApiResult<(StatusCode, Json<Value>)> {
    let socket_path = require_ml_configured(&state).await?;
    let model_version = state
        .ml_health(socket_path)
        .await
        .and_then(|health| health.model_bundle.map(|bundle| bundle.version));
    let jobs = run_main_blocking(&state, move |database| {
        enqueue_analyze_all_for_model(&database, model_version.as_deref())
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"enqueued": jobs.len()}))))
}

pub async fn recluster(State(state): State<AppState>) -> ApiResult<(StatusCode, Json<Value>)> {
    require_ml_configured(&state).await?;
    let job = run_main_blocking(&state, move |database| enqueue_recluster(&database)).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"job_id": job.id}))))
}

pub async fn list_clusters(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<ClusterSummary>>> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    Ok(Json(
        selected
            .run_blocking(&state, move |database| {
                MlService::new(database, MlClient::new(socket_path)).list_clusters()
            })
            .await?,
    ))
}

pub async fn cluster_assets(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<ClusterAssetResponse>>> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    let assets = selected
        .run_blocking(&state, move |database| {
            MlService::new(database, MlClient::new(socket_path)).cluster_assets(&id)
        })
        .await?
        .into_iter()
        .map(|item| ClusterAssetResponse {
            asset: item.asset.into(),
            faces: item.faces,
        })
        .collect();
    Ok(Json(assets))
}

pub async fn rename_cluster(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<RenameClusterRequest>,
) -> ApiResult<Json<ClusterSummary>> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    Ok(Json(
        selected
            .run_blocking(&state, move |database| {
                MlService::new(database, MlClient::new(socket_path))
                    .rename_cluster(&id, &request.name)
            })
            .await?,
    ))
}

pub async fn merge_clusters(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<MergeClustersRequest>,
) -> ApiResult<Json<ClusterSummary>> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    Ok(Json(
        selected
            .run_blocking(&state, move |database| {
                MlService::new(database, MlClient::new(socket_path))
                    .merge_clusters(&request.from_id, &request.into_id)
            })
            .await?,
    ))
}

pub async fn split_cluster(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<SplitClusterRequest>,
) -> ApiResult<(StatusCode, Json<ClusterSummary>)> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    let cluster = selected
        .run_blocking(&state, move |database| {
            MlService::new(database, MlClient::new(socket_path))
                .split_cluster(&id, &request.face_ids)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(cluster)))
}

pub async fn review_candidates(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<ReviewCandidateResponse>>> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    let candidates = selected
        .run_blocking(&state, move |database| {
            MlService::new(database, MlClient::new(socket_path)).review_candidates()
        })
        .await?
        .into_iter()
        .map(|candidate| ReviewCandidateResponse {
            face: candidate.face,
            asset: candidate.asset.into(),
        })
        .collect();
    Ok(Json(candidates))
}

pub async fn review_candidate(
    State(state): State<AppState>,
    Path(face_id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<ReviewCandidateRequest>,
) -> ApiResult<Json<FaceRecord>> {
    let socket_path = PathBuf::from("/dev/null");
    let selected = SelectedDatabase::from_request(&state, access);
    let accept = matches!(request.action, ReviewAction::Accept);
    Ok(Json(
        selected
            .run_blocking(&state, move |database| {
                MlService::new(database, MlClient::new(socket_path))
                    .review_candidate(&face_id, accept)
            })
            .await?,
    ))
}

/// Runs vault inference only while the request holds a valid unlock session.
///
/// `vault: no-log`
pub async fn vault_analyze_all(
    State(state): State<AppState>,
    Extension(access): Extension<VaultAccess>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let socket_path = require_ml_configured(&state).await?;
    if !state.vault.generation_active(access.generation) {
        return Err(ApiError::not_found("vault session not found"));
    }
    let permit = state.try_blocking_db_slot()?;
    let handle = access.handle.clone();
    let jobs = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        handle.ensure_active()?;
        let jobs = enqueue_vault_analyze_all(&handle.db)?;
        if !jobs.is_empty() {
            enqueue_recluster(&handle.db)?;
        }
        handle.ensure_active()?;
        Ok::<_, illumia_core::db::Error>(jobs)
    })
    .await
    .map_err(|_| ApiError::internal_silent())?
    .map_err(vault_core_error)?;
    if !state.vault.generation_active(access.generation) {
        return Err(ApiError::not_found("vault session not found"));
    }
    ensure_vault_ml_worker(&state, access, socket_path);
    Ok((StatusCode::ACCEPTED, Json(json!({"enqueued": jobs.len()}))))
}

fn ensure_vault_ml_worker(state: &AppState, access: VaultAccess, socket_path: PathBuf) {
    let generation = access.generation;
    {
        let mut workers = state
            .vault_ml_workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !workers.insert(generation) {
            return;
        }
    }
    let state = state.clone();
    tokio::spawn(async move {
        while state.vault.generation_active(generation) {
            let handle = access.handle.clone();
            let sessions = state.vault.clone();
            let gate = state.ml_gate.clone();
            let socket_path = socket_path.clone();
            let processed = tokio::task::spawn_blocking(move || {
                process_one_vault_ml_job(handle, sessions, generation, gate, socket_path)
            })
            .await;
            match processed {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => tokio::time::sleep(Duration::from_millis(100)).await,
                Ok(Err(_)) | Err(_) => break,
            }
        }
        state
            .vault_ml_workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&generation);
    });
}

/// Processes at most one Vault ML job. No identifier or filename may be logged.
/// `vault: no-log`
fn process_one_vault_ml_job(
    handle: VaultHandle,
    sessions: crate::vault::VaultSessionManager,
    generation: u64,
    gate: crate::MlConcurrencyGate,
    socket_path: PathBuf,
) -> illumia_core::db::Result<bool> {
    let queue = JobQueue::new(handle.db.clone());
    let Some(job) = queue.claim_kinds(&[ML_VAULT_ANALYZE_JOB_KIND, ML_RECLUSTER_JOB_KIND])? else {
        return Ok(false);
    };
    let active = || handle.ensure_active().is_ok() && sessions.generation_active(generation);
    if !active() {
        let _ = queue.cancel(&job.id)?;
        let _ = queue.complete(&job.id)?;
        return Ok(true);
    }

    let _permit = gate.acquire();
    let service = MlService::new(handle.db.clone(), MlClient::new(socket_path));
    let result = (|| match job.kind.as_str() {
        ML_VAULT_ANALYZE_JOB_KIND => {
            let payload: MlAnalyzePayload = serde_json::from_str(&job.payload)?;
            let blob_id = vault_blob_id(&handle.db, &payload.asset_id, "original")?
                .ok_or(illumia_core::db::Error::VaultBlobNotFound)?;
            let bytes = handle.read_blob(&blob_id)?;
            service.analyze_bytes_cancellable(&payload.asset_id, &bytes, || {
                Ok(queue.cancellation_requested(&job.id)? || !active())
            })
        }
        ML_RECLUSTER_JOB_KIND => service
            .recluster_cancellable(|| Ok(queue.cancellation_requested(&job.id)? || !active())),
        _ => unreachable!("claim_kinds restricts Vault ML kinds"),
    })();

    if !active() {
        let _ = queue.cancel(&job.id)?;
        let _ = queue.complete(&job.id)?;
    } else if let Err(error) = result {
        let _ = queue.fail(&job.id, &error.to_string())?;
    } else {
        let _ = queue.complete(&job.id)?;
    }
    Ok(true)
}

pub async fn jobs(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Json<Vec<JobResponse>>> {
    let state_filter = query.state.as_deref().map(parse_job_state).transpose()?;
    let jobs = run_main_blocking(&state, move |database| JobQueue::new(database).list())
        .await?
        .into_iter()
        .filter(|job| state_filter.is_none_or(|filter| job.state == filter))
        .map(JobResponse::from)
        .collect();
    Ok(Json(jobs))
}

pub async fn cancel_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<CancelResponse>> {
    let cancelled =
        run_main_blocking(&state, move |database| JobQueue::new(database).cancel(&id)).await?;
    if !cancelled {
        return Err(ApiError::not_found("cancellable job not found"));
    }
    Ok(Json(CancelResponse { cancelled }))
}

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(
        run_main_api_blocking(&state, move |database| {
            settings_json(&Settings::new(database))
        })
        .await?,
    ))
}

pub async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    let patch = patch
        .as_object()
        .ok_or_else(|| ApiError::bad_request("settings patch must be an object"))?;
    let changes = validate_settings_patch(patch)?;
    let value = run_main_api_blocking(&state, move |database| {
        let settings = Settings::new(database);
        validate_setting_relationships(&settings, &changes)?;
        apply_settings_patch(&settings, changes)?;
        settings_json(&settings)
    })
    .await?;
    Ok(Json(value))
}

pub(crate) const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 64 * 1024;
#[cfg(not(test))]
const WEBSOCKET_PING_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(test)]
const WEBSOCKET_PING_INTERVAL: Duration = Duration::from_millis(200);
const WEBSOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(75);
const WEBSOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Browser clients authenticate with the HttpOnly session cookie. Native
/// clients may use the Authorization header. Credentials are never accepted
/// from the URL, where proxies and request logs could retain them.
pub async fn websocket(
    State(state): State<AppState>,
    Extension(authentication): Extension<Authenticated>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let permit = state.security.try_websocket_slot(&authentication.token)?;
    let token = authentication.token;
    Ok(upgrade
        .max_frame_size(MAX_WEBSOCKET_MESSAGE_BYTES)
        .max_message_size(MAX_WEBSOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            websocket_loop(state, token, socket).await;
        }))
}

async fn websocket_loop(state: AppState, token: String, mut socket: WebSocket) {
    let mut events = state.events.subscribe();
    state.ensure_job_event_poller();
    let mut ping = tokio::time::interval(WEBSOCKET_PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seen = tokio::time::Instant::now();
    loop {
        if state.auth.verify_token_cached(&token).is_err() {
            let _ = send_message(&mut socket, Message::Close(None)).await;
            break;
        }
        tokio::select! {
            received = events.recv() => {
                match received {
                    Ok(value) => {
                        if state.auth.verify_token_cached(&token).is_err() {
                            let _ = send_message(&mut socket, Message::Close(None)).await;
                            break;
                        }
                        if send_json(&mut socket, &value).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() >= WEBSOCKET_IDLE_TIMEOUT {
                    let _ = send_message(&mut socket, Message::Close(None)).await;
                    break;
                }
                if send_message(&mut socket, Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => last_seen = tokio::time::Instant::now(),
                }
            }
        }
    }
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), ()> {
    send_message(socket, Message::Text(value.to_string().into())).await
}

async fn send_message(socket: &mut WebSocket, message: Message) -> Result<(), ()> {
    tokio::time::timeout(WEBSOCKET_SEND_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

async fn image_variant(
    state: AppState,
    id: String,
    access: Option<Extension<VaultAccess>>,
    request: Request,
    vault_kind: &'static str,
    suffix: &str,
) -> ApiResult<Response> {
    Uuid::parse_str(&id).map_err(|_| ApiError::not_found("asset not found"))?;
    let selected = SelectedDatabase::from_request(&state, access);
    if selected
        .run_blocking(&state, {
            let id = id.clone();
            move |database| AssetService::new(database).get(&id)
        })
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("asset not found"));
    }
    let mut response = if let Some(vault) = &selected.vault {
        let blob_id = selected
            .run_blocking(&state, {
                let id = id.clone();
                move |database| vault_blob_id(&database, &id, vault_kind)
            })
            .await?
            .ok_or_else(|| ApiError::not_found("image variant not found"))?;
        vault_blob_response(&state, vault, &blob_id).await?
    } else {
        let path = selected
            .database
            .data_root()
            .join("thumbs")
            .join(format!("{id}_{suffix}.webp"));
        let response = serve_file(path, request).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ApiError::not_found("image variant not found"));
        }
        response
    };
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if selected.vault.is_some() {
            NO_STORE
        } else {
            IMMUTABLE_CACHE
        }),
    );
    Ok(response)
}

/// Vault blobを固定長channelで逐次配信する。producerはblocking file/AEAD処理を
/// async runtimeから隔離し、receiver切断時は次のsendで停止する。
///
/// `vault: no-log`
pub(crate) async fn vault_blob_response(
    state: &AppState,
    vault: &VaultHandle,
    blob_id: &str,
) -> ApiResult<Response> {
    let permit = state.security.try_vault_stream_slot()?;
    let handle = vault.clone();
    let blob_id = blob_id.to_owned();
    let reader = tokio::task::spawn_blocking(move || handle.blob_reader(&blob_id))
        .await
        .map_err(|_| ApiError::internal_silent())?
        .map_err(vault_core_error)?;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<Vec<u8>, io::Error>>(VAULT_STREAM_CHANNEL_CAPACITY);
    let aborted = Arc::new(AtomicBool::new(false));
    let producer_aborted = Arc::clone(&aborted);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        for chunk in reader {
            let item = chunk.map_err(|_| io::Error::other("vault stream failed"));
            let terminal_error = item.is_err();
            match send_vault_chunk_with_deadline(&sender, item) {
                VaultChunkSend::Sent if terminal_error => return,
                VaultChunkSend::Sent => {}
                VaultChunkSend::Closed => return,
                VaultChunkSend::TimedOut => {
                    producer_aborted.store(true, Ordering::Release);
                    return;
                }
            }
        }
    });
    let body_stream = vault_body_stream(receiver, aborted);
    Ok(Body::from_stream(body_stream).into_response())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VaultChunkSend {
    Sent,
    Closed,
    TimedOut,
}

fn send_vault_chunk_with_deadline(
    sender: &tokio::sync::mpsc::Sender<Result<Vec<u8>, io::Error>>,
    mut item: Result<Vec<u8>, io::Error>,
) -> VaultChunkSend {
    let deadline = std::time::Instant::now() + VAULT_STREAM_BACKPRESSURE_TIMEOUT;
    loop {
        match sender.try_send(item) {
            Ok(()) => return VaultChunkSend::Sent,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                return VaultChunkSend::Closed;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                if std::time::Instant::now() >= deadline {
                    return VaultChunkSend::TimedOut;
                }
                item = returned;
                std::thread::sleep(VAULT_STREAM_BACKPRESSURE_RETRY);
            }
        }
    }
}

struct VaultBodyStreamState {
    receiver: tokio::sync::mpsc::Receiver<Result<Vec<u8>, io::Error>>,
    aborted: Arc<AtomicBool>,
    abort_reported: bool,
}

fn vault_body_stream(
    receiver: tokio::sync::mpsc::Receiver<Result<Vec<u8>, io::Error>>,
    aborted: Arc<AtomicBool>,
) -> impl futures_util::Stream<Item = Result<Vec<u8>, io::Error>> {
    stream::unfold(
        VaultBodyStreamState {
            receiver,
            aborted,
            abort_reported: false,
        },
        |mut state| async move {
            if let Some(item) = state.receiver.recv().await {
                return Some((item, state));
            }
            if !state.abort_reported && state.aborted.load(Ordering::Acquire) {
                state.abort_reported = true;
                return Some((Err(io::Error::other("vault stream aborted")), state));
            }
            None
        },
    )
}

async fn serve_file(path: PathBuf, request: Request) -> ApiResult<Response> {
    let mut service = ServeFile::new(path);
    let response = service
        .try_call(request)
        .await
        .map_err(ApiError::internal)?;
    Ok(response.map(Body::new))
}

/// `vault: no-log`
fn vault_blob_id(
    database: &Database,
    asset_id: &str,
    kind: &str,
) -> illumia_core::db::Result<Option<String>> {
    database.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT blob_id FROM vault_blobs
             WHERE asset_id = ?1 AND kind = ?2",
        )?;
        let mut rows = statement.query((asset_id, kind))?;
        Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
    })
}

/// `vault: no-log`
fn stack_asset_ids_for_vault(
    database: &Database,
    stack_id: &str,
) -> illumia_core::db::Result<Vec<String>> {
    database.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT p.asset_id
             FROM stack_pages p
             JOIN stack_chapters c ON c.id = p.chapter_id
             WHERE p.stack_id = ?1
             ORDER BY c.chapter_no, p.page_no",
        )?;
        let ids = statement
            .query_map([stack_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    })
}

/// Maps vault errors without logging their attached SQL or filesystem context.
///
/// `vault: no-log`
fn vault_core_error(error: illumia_core::db::Error) -> ApiError {
    match error {
        illumia_core::db::Error::AssetNotFound => ApiError::not_found("asset not found"),
        illumia_core::db::Error::StackNotFound => ApiError::not_found("manga stack not found"),
        illumia_core::db::Error::StackChapterNotFound => {
            ApiError::not_found("stack chapter not found")
        }
        illumia_core::db::Error::VaultBlobNotFound => ApiError::not_found("image data not found"),
        illumia_core::db::Error::InvalidBucketKey => {
            ApiError::bad_request("invalid timeline bucket key")
        }
        illumia_core::db::Error::InvalidStack(_) => ApiError::bad_request("invalid manga stack"),
        illumia_core::db::Error::InvalidSearch => ApiError::bad_request("invalid search query"),
        illumia_core::db::Error::EmptyVaultTransfer => {
            ApiError::bad_request("provide exactly one of asset_ids or stack_id")
        }
        illumia_core::db::Error::IncompleteVaultTransfer
        | illumia_core::db::Error::InvalidAssetPath => {
            ApiError::bad_request("vault transfer input exceeds limits or is invalid")
        }
        _ => ApiError::internal_silent(),
    }
}

fn validate_vault_transfer_request(request: &VaultTransferRequest) -> ApiResult<()> {
    match (&request.asset_ids, &request.stack_id) {
        (Some(asset_ids), None)
            if !asset_ids.is_empty()
                && asset_ids.len() <= core_vault::MAX_VAULT_TRANSFER_ASSETS =>
        {
            for asset_id in asset_ids {
                Uuid::parse_str(asset_id)
                    .map_err(|_| ApiError::bad_request("invalid vault transfer input"))?;
            }
            Ok(())
        }
        (None, Some(stack_id)) => {
            Uuid::parse_str(stack_id)
                .map_err(|_| ApiError::bad_request("invalid vault transfer input"))?;
            Ok(())
        }
        _ => Err(ApiError::bad_request(
            "provide exactly one bounded asset_ids list or stack_id",
        )),
    }
}

/// `vault: no-log`
fn vault_management_error(error: illumia_core::db::Error) -> ApiError {
    match error {
        illumia_core::db::Error::VaultAlreadyInitialized => {
            ApiError::conflict("vault_already_initialized", "vault is already initialized")
        }
        _ => ApiError::internal_silent(),
    }
}

/// `vault: no-log`
fn vault_unlock_error(error: illumia_core::db::Error) -> ApiError {
    match error {
        illumia_core::db::Error::VaultNotInitialized => ApiError::not_found("not found"),
        illumia_core::db::Error::VaultAuthenticationFailed
        | illumia_core::db::Error::InvalidRecoveryKey => ApiError::unauthorized(),
        _ => ApiError::internal_silent(),
    }
}

fn content_type_for_extension(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

enum SettingChange {
    TrashRetention(u32),
    DedupRetention(u32),
    ThumbnailConcurrency(u32),
    MlConcurrency(u32),
    TauHigh(f64),
    TauLow(f64),
    MinClusterSize(u32),
    QualityGate(QualityGate),
    MlEnabled(bool),
    MlSocketPath(String),
}

fn validate_settings_patch(patch: &Map<String, Value>) -> ApiResult<Vec<SettingChange>> {
    patch
        .iter()
        .map(|(key, value)| match key.as_str() {
            "trash.retention_days" => Ok(SettingChange::TrashRetention(value_u32_range(
                key,
                value,
                0,
                MAX_RETENTION_DAYS,
            )?)),
            "dedup.retention_days" => Ok(SettingChange::DedupRetention(value_u32_range(
                key,
                value,
                0,
                MAX_RETENTION_DAYS,
            )?)),
            "jobs.thumbnail_concurrency" => Ok(SettingChange::ThumbnailConcurrency(
                value_u32_range(key, value, MIN_JOB_CONCURRENCY, MAX_JOB_CONCURRENCY)?,
            )),
            "jobs.ml_concurrency" => Ok(SettingChange::MlConcurrency(value_u32_range(
                key,
                value,
                MIN_JOB_CONCURRENCY,
                MAX_ML_CONCURRENCY,
            )?)),
            "ml.tau_high_override" => Ok(SettingChange::TauHigh(value_f64_ratio(key, value)?)),
            "ml.tau_low_override" => Ok(SettingChange::TauLow(value_f64_ratio(key, value)?)),
            "ml.min_cluster_size" => Ok(SettingChange::MinClusterSize(value_u32_range(
                key,
                value,
                MIN_CLUSTER_SIZE_VALUE,
                MAX_CLUSTER_SIZE_VALUE,
            )?)),
            "ml.quality_gate" => match value.as_str() {
                Some("review_only") => Ok(SettingChange::QualityGate(QualityGate::ReviewOnly)),
                Some("strict") => Ok(SettingChange::QualityGate(QualityGate::Strict)),
                _ => Err(ApiError::bad_request(
                    "ml.quality_gate must be review_only or strict",
                )),
            },
            "ml.enabled" => value
                .as_bool()
                .map(SettingChange::MlEnabled)
                .ok_or_else(|| ApiError::bad_request("ml.enabled must be a boolean")),
            "ml.socket_path" => value
                .as_str()
                .filter(|path| !path.is_empty() && path.len() <= 4096 && !path.contains('\0'))
                .map(|path| SettingChange::MlSocketPath(path.to_owned()))
                .ok_or_else(|| ApiError::bad_request("ml.socket_path must be a non-empty path")),
            _ => Err(ApiError::bad_request(format!(
                "unsupported setting key: {key}"
            ))),
        })
        .collect()
}

fn validate_setting_relationships(settings: &Settings, changes: &[SettingChange]) -> ApiResult<()> {
    let mut high = settings.tau_high_override()?;
    let mut low = settings.tau_low_override()?;
    for change in changes {
        match change {
            SettingChange::TauHigh(value) => high = Some(*value),
            SettingChange::TauLow(value) => low = Some(*value),
            _ => {}
        }
    }
    if matches!((low, high), (Some(low), Some(high)) if low > high) {
        return Err(ApiError::bad_request(
            "ml.tau_low_override must not exceed ml.tau_high_override",
        ));
    }
    Ok(())
}

fn apply_settings_patch(settings: &Settings, changes: Vec<SettingChange>) -> ApiResult<()> {
    for change in changes {
        match change {
            SettingChange::TrashRetention(value) => {
                settings.set_trash_retention_days(value)?;
            }
            SettingChange::DedupRetention(value) => {
                settings.set_dedup_retention_days(value)?;
            }
            SettingChange::ThumbnailConcurrency(value) => {
                settings.set_thumbnail_concurrency(value)?;
            }
            SettingChange::MlConcurrency(value) => settings.set_ml_concurrency(value)?,
            SettingChange::TauHigh(value) => settings.set_tau_high_override(value)?,
            SettingChange::TauLow(value) => settings.set_tau_low_override(value)?,
            SettingChange::MinClusterSize(value) => settings.set_min_cluster_size(value)?,
            SettingChange::QualityGate(value) => settings.set_quality_gate(value)?,
            SettingChange::MlEnabled(value) => settings.set_ml_enabled(value)?,
            SettingChange::MlSocketPath(value) => settings.set_ml_socket_path(&value)?,
        }
    }
    Ok(())
}

fn settings_json(settings: &Settings) -> ApiResult<Value> {
    Ok(json!({
        "trash.retention_days": settings.trash_retention_days()?,
        "dedup.retention_days": settings.dedup_retention_days()?,
        "jobs.thumbnail_concurrency": settings.thumbnail_concurrency()?,
        "jobs.ml_concurrency": settings.ml_concurrency()?,
        "ml.tau_high_override": settings.tau_high_override()?,
        "ml.tau_low_override": settings.tau_low_override()?,
        "ml.min_cluster_size": settings.min_cluster_size()?,
        "ml.quality_gate": quality_gate_name(settings.quality_gate()?),
        "ml.enabled": settings.ml_enabled()?,
        "ml.socket_path": settings.ml_socket_path()?,
    }))
}

async fn require_ml_configured(state: &AppState) -> ApiResult<PathBuf> {
    let (enabled, socket_path) = ml_configuration(state).await?;
    if !enabled {
        return Err(ApiError::bad_request("ML is disabled"));
    }
    socket_path.ok_or_else(|| ApiError::bad_request("ml.socket_path is not configured"))
}

async fn ml_configuration(state: &AppState) -> ApiResult<(bool, Option<PathBuf>)> {
    let permit = state.try_blocking_db_slot()?;
    let database = state.database.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let settings = Settings::new(database);
        Ok::<_, illumia_core::db::Error>((settings.ml_enabled()?, settings.ml_socket_path()?))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::from)
}

fn value_u32(key: &str, value: &Value) -> ApiResult<u32> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| ApiError::bad_request(format!("{key} must be a non-negative integer")))
}

fn value_u32_range(key: &str, value: &Value, minimum: u32, maximum: u32) -> ApiResult<u32> {
    let value = value_u32(key, value)?;
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(ApiError::bad_request(format!(
            "{key} must be between {minimum} and {maximum}"
        )))
    }
}

fn value_f64_ratio(key: &str, value: &Value) -> ApiResult<f64> {
    let value = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| ApiError::bad_request(format!("{key} must be a finite number")))?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(ApiError::bad_request(format!(
            "{key} must be between 0 and 1"
        )))
    }
}

fn decode_hash(encoded: &str) -> ApiResult<Vec<u8>> {
    let decoded =
        illumia_core::hex::decode(encoded).map_err(|_| ApiError::bad_request("invalid hash"))?;
    if decoded.len() != 32 {
        return Err(ApiError::bad_request("hash must encode exactly 32 bytes"));
    }
    Ok(decoded)
}

fn asset_path(database: &Database, relative: &str) -> ApiResult<PathBuf> {
    let path = FsPath::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ApiError::internal("invalid asset path"));
    }
    Ok(database.data_root().join(path))
}

fn validate_bucket_granularity(key: &str, granularity: GranularityParam) -> ApiResult<()> {
    let expected = match granularity {
        GranularityParam::Day => 10,
        GranularityParam::Month => 7,
        GranularityParam::Year => 4,
    };
    if key.len() == expected {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "bucket key does not match granularity",
        ))
    }
}

fn parse_job_state(value: &str) -> ApiResult<JobState> {
    match value {
        "queued" => Ok(JobState::Queued),
        "running" => Ok(JobState::Running),
        "done" => Ok(JobState::Done),
        "failed" => Ok(JobState::Failed),
        "cancelled" => Ok(JobState::Cancelled),
        _ => Err(ApiError::bad_request("invalid job state")),
    }
}

fn lifecycle_name(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Active => "created",
        Lifecycle::Duplicate => "duplicate",
        Lifecycle::Trashed => "trashed",
        Lifecycle::Purging => "purging",
    }
}

pub(crate) fn job_state_name(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Done => "done",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn quality_gate_name(value: QualityGate) -> &'static str {
    match value {
        QualityGate::ReviewOnly => "review_only",
        QualityGate::Strict => "strict",
    }
}

pub(crate) fn job_snapshot(job: &Job) -> JobSnapshot {
    JobSnapshot {
        id: job.id.clone(),
        state: job_state_name(job.state),
        progress_bits: job.progress.to_bits(),
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(char::from(TABLE[usize::from(a >> 2)]));
        output.push(char::from(TABLE[usize::from(((a & 0x03) << 4) | (b >> 4))]));
        if chunk.len() > 1 {
            output.push(char::from(TABLE[usize::from(((b & 0x0f) << 2) | (c >> 6))]));
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(char::from(TABLE[usize::from(c & 0x3f)]));
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use tokio::sync::Semaphore;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unpolled_vault_stream_releases_its_permit_after_backpressure_deadline() {
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().acquire_owned().await.expect("stream permit");
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Ok(Vec::new()))
            .expect("channel should be filled");
        let aborted = Arc::new(AtomicBool::new(false));
        let producer_aborted = Arc::clone(&aborted);

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            assert_eq!(
                send_vault_chunk_with_deadline(&sender, Ok(vec![1])),
                VaultChunkSend::TimedOut
            );
            producer_aborted.store(true, Ordering::Release);
        });

        let _reacquired = tokio::time::timeout(Duration::from_secs(1), slots.acquire())
            .await
            .expect("an unpolled receiver must not pin a stream slot")
            .expect("semaphore should remain open");

        let mut body = Box::pin(vault_body_stream(receiver, aborted));
        assert!(matches!(body.next().await, Some(Ok(chunk)) if chunk.is_empty()));
        assert!(matches!(body.next().await, Some(Err(_))));
        assert!(body.next().await.is_none());
    }
}
