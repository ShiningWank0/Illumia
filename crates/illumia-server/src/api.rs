use std::{
    collections::BTreeMap,
    path::{Component, Path as FsPath, PathBuf},
    time::Duration,
};

use axum::{
    Json,
    body::Body,
    extract::{
        Extension, Multipart, Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
    },
    response::{IntoResponse, Response},
};
use illumia_core::{
    PurgeService,
    assets::{Asset, AssetService, DuplicatePair, Lifecycle},
    blake3,
    chrono::{DateTime, Utc},
    db::Database,
    jobs::{Job, JobQueue, JobState},
    settings::{QualityGate, Settings},
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
    vault::VaultAccess,
};

const IMMUTABLE_CACHE: &str = "public,max-age=31536000,immutable";
const NO_STORE: &str = "no-store";

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
    clusters: Vec<Value>,
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
        }
    }
}

#[derive(Serialize)]
pub struct CancelResponse {
    cancelled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct JobSnapshot {
    id: String,
    state: &'static str,
    progress_bits: u64,
}

#[derive(Clone)]
struct SelectedDatabase {
    database: Database,
    vault: Option<VaultHandle>,
}

impl SelectedDatabase {
    fn from_request(state: &AppState, access: Option<Extension<VaultAccess>>) -> Self {
        match access {
            Some(Extension(access)) => Self {
                database: access.handle.db.clone(),
                vault: Some(access.handle),
            },
            None => Self {
                database: state.database.clone(),
                vault: None,
            },
        }
    }

    fn result<T>(&self, result: illumia_core::db::Result<T>) -> ApiResult<T> {
        result.map_err(|error| {
            if self.vault.is_none() {
                return ApiError::from(error);
            }
            vault_core_error(error)
        })
    }
}

/// Initializes vault key material without exposing whether it exists to unauthenticated callers.
///
/// `vault: no-log`
pub async fn vault_init(
    State(state): State<AppState>,
    Json(request): Json<VaultInitRequest>,
) -> ApiResult<(StatusCode, Json<VaultInitResponse>)> {
    if request.password.is_empty() {
        return Err(ApiError::bad_request("password must not be empty"));
    }
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
    let manager = state.vault.clone();
    let issued = match (request.password, request.recovery_key) {
        (Some(password), None) if !password.is_empty() => {
            tokio::task::spawn_blocking(move || manager.unlock_with_password(&password))
                .await
                .map_err(|_| ApiError::internal_silent())?
        }
        (None, Some(recovery_key)) if !recovery_key.is_empty() => {
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
    let main = state.database;
    let vault = access.handle;
    tokio::task::spawn_blocking(move || -> illumia_core::db::Result<()> {
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
            vault.generate_thumbnails(&asset_id)?;
        }
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
    let main = state.database;
    let vault = access.handle;
    tokio::task::spawn_blocking(move || match (request.asset_ids, request.stack_id) {
        (Some(asset_ids), None) => core_vault::export_assets(&vault, &main, &asset_ids),
        (None, Some(stack_id)) => core_vault::export_stack(&vault, &main, &stack_id),
        _ => Err(illumia_core::db::Error::EmptyVaultTransfer),
    })
    .await
    .map_err(|_| ApiError::internal_silent())?
    .map_err(vault_core_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn upload_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Response> {
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
        let bytes = field
            .bytes()
            .await
            .map_err(|error| ApiError::bad_request(format!("invalid file field: {error}")))?;
        upload = Some((filename, bytes.to_vec()));
    }
    let (filename, bytes) =
        upload.ok_or_else(|| ApiError::bad_request("multipart file field is required"))?;
    if blake3::hash(&bytes).as_bytes() != expected_bytes.as_slice() {
        return Err(ApiError::bad_request("checksum mismatch"));
    }

    let service = AssetService::new(state.database.clone());
    let result = tokio::task::spawn_blocking(move || service.ingest(&bytes, &filename, taken_at))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::from)?;
    thumbnails::enqueue_thumbnail(&state.database, &result.asset.id)?;
    state.notify_assets_added(&result.asset.taken_at_local_date);

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
    let mut decoded = Vec::with_capacity(request.hashes.len());
    for hash in request.hashes {
        let normalized = hash.to_ascii_lowercase();
        decoded.push((normalized.clone(), decode_hash(&normalized)?));
    }
    let exists = state
        .database
        .with_connection(|connection| {
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
        .map_err(ApiError::from)?;
    Ok(Json(ExistsResponse { exists }))
}

pub async fn asset_metadata(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<AssetResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let asset = selected
        .result(AssetService::new(selected.database.clone()).get(&id))?
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
        .result(AssetService::new(selected.database.clone()).get(&id))?
        .ok_or_else(|| ApiError::not_found("asset not found"))?;
    let mut response = if let Some(vault) = &selected.vault {
        let bytes = vault
            .read_blob(&asset.library_path)
            .map_err(vault_core_error)?;
        let mut response = Body::from(bytes).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static(content_type_for_extension(&asset.ext)),
        );
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
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
    let asset = selected.result(AssetService::new(selected.database.clone()).trash(&id))?;
    Ok(Json(asset.into()))
}

pub async fn restore_asset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<AssetResponse>> {
    let asset = AssetService::new(state.database.clone()).restore(&id)?;
    state.notify_assets_added(&asset.taken_at_local_date);
    Ok(Json(asset.into()))
}

pub async fn timeline_buckets(
    State(state): State<AppState>,
    Query(query): Query<BucketQuery>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<BucketResponse>>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let buckets = selected
        .result(
            TimelineService::new(selected.database.clone())
                .bucket_records(query.granularity.into()),
        )?
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
        .result(TimelineService::new(selected.database.clone()).bucket_items(&key))?
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
        .result(AssetService::new(selected.database.clone()).list_trash())?
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
        .result(AssetService::new(selected.database.clone()).list_duplicates())?
        .into_iter()
        .map(DuplicateResponse::from)
        .collect();
    Ok(Json(duplicates))
}

pub async fn purge_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    PurgeService::new(state.database).purge_now(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_stack(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<CreateStackRequest>,
) -> ApiResult<(StatusCode, Json<StackResponse>)> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected.result(
        StackService::new(selected.database.clone()).create(&request.title, &request.asset_ids),
    )?;
    Ok((StatusCode::CREATED, Json(stack.into())))
}

pub async fn list_stacks(
    State(state): State<AppState>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<Vec<StackSummaryResponse>>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stacks = selected
        .result(StackService::new(selected.database.clone()).list())?
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
        .result(StackService::new(selected.database.clone()).get(&id))?
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
    let stack = selected.result(
        StackService::new(selected.database.clone()).update_metadata(
            &id,
            request.title.as_deref(),
            request.cover_asset_id.as_deref(),
        ),
    )?;
    Ok(Json(stack.into()))
}

pub async fn delete_stack(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<StatusCode> {
    let selected = SelectedDatabase::from_request(&state, access);
    selected.result(StackService::new(selected.database.clone()).delete_stack(&id))?;
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
        .result(StackService::new(selected.database.clone()).replace_structure(&id, &chapters))?;
    Ok(Json(stack.into()))
}

pub async fn add_stack_pages(
    State(state): State<AppState>,
    Path(id): Path<String>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<AddStackPagesRequest>,
) -> ApiResult<Json<StackResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected.result(StackService::new(selected.database.clone()).add_pages(
        &id,
        &request.asset_ids,
        request.chapter_id.as_deref(),
    ))?;
    Ok(Json(stack.into()))
}

pub async fn remove_stack_page(
    State(state): State<AppState>,
    Path((id, asset_id)): Path<(String, String)>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<StatusCode> {
    let selected = SelectedDatabase::from_request(&state, access);
    selected.result(StackService::new(selected.database.clone()).remove_page(&id, &asset_id))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn patch_stack_page(
    State(state): State<AppState>,
    Path((id, asset_id)): Path<(String, String)>,
    access: Option<Extension<VaultAccess>>,
    Json(request): Json<PatchStackPageRequest>,
) -> ApiResult<Json<StackResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let stack = selected.result(StackService::new(selected.database.clone()).set_page_flag(
        &id,
        &asset_id,
        request.show_in_timeline,
    ))?;
    Ok(Json(stack.into()))
}

pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
    access: Option<Extension<VaultAccess>>,
) -> ApiResult<Json<SearchResponse>> {
    let selected = SelectedDatabase::from_request(&state, access);
    let query = query.q.trim().to_owned();
    if query.is_empty() {
        return Ok(Json(SearchResponse {
            assets: Vec::new(),
            stacks: Vec::new(),
            clusters: Vec::new(),
        }));
    }
    let ids = selected.result(search_asset_ids(&selected.database, &query))?;
    let stacks = selected
        .result(StackService::new(selected.database.clone()).search(&query))?
        .into_iter()
        .map(StackSummaryResponse::from)
        .collect();
    let service = AssetService::new(selected.database.clone());
    let mut assets = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(asset) = selected.result(service.get(&id))? {
            assets.push(asset.into());
        }
    }
    Ok(Json(SearchResponse {
        assets,
        stacks,
        clusters: Vec::new(),
    }))
}

pub async fn jobs(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Json<Vec<JobResponse>>> {
    let state_filter = query.state.as_deref().map(parse_job_state).transpose()?;
    let jobs = JobQueue::new(state.database)
        .list()?
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
    let cancelled = JobQueue::new(state.database).cancel(&id)?;
    if !cancelled {
        return Err(ApiError::not_found("cancellable job not found"));
    }
    Ok(Json(CancelResponse { cancelled }))
}

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(settings_json(&Settings::new(state.database))?))
}

pub async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<Value>,
) -> ApiResult<Json<Value>> {
    let patch = patch
        .as_object()
        .ok_or_else(|| ApiError::bad_request("settings patch must be an object"))?;
    let changes = validate_settings_patch(patch)?;
    let settings = Settings::new(state.database);
    apply_settings_patch(&settings, changes)?;
    Ok(Json(settings_json(&settings)?))
}

#[derive(Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

/// ブラウザの WebSocket API は任意ヘッダを付与できないため、
/// `?token=` クエリでも認証を受け付ける (Bearer ヘッダも引き続き可)。
pub async fn websocket(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let token = match query.token.as_deref() {
        Some(token) if !token.is_empty() => token.to_owned(),
        _ => crate::auth::bearer_token(&headers)?.to_owned(),
    };
    state.auth.verify_token(&token)?;
    Ok(upgrade.on_upgrade(move |socket| websocket_loop(state, socket)))
}

async fn websocket_loop(state: AppState, mut socket: WebSocket) {
    let mut events = state.events.subscribe();
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut previous = Vec::new();
    loop {
        tokio::select! {
            received = events.recv() => {
                match received {
                    Ok(value) => {
                        if send_json(&mut socket, &value).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = interval.tick() => {
                let database = state.database.clone();
                let jobs = match tokio::task::spawn_blocking(move || {
                    JobQueue::new(database).list()
                }).await {
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
                let snapshot: Vec<_> = jobs.iter().map(job_snapshot).collect();
                for job in &jobs {
                    let current = job_snapshot(job);
                    if !previous.contains(&current) {
                        let value = json!({
                            "type": "job",
                            "id": job.id,
                            "state": job_state_name(job.state),
                            "progress": job.progress,
                        });
                        if send_json(&mut socket, &value).await.is_err() {
                            return;
                        }
                    }
                }
                previous = snapshot;
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), ()> {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
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
        .result(AssetService::new(selected.database.clone()).get(&id))?
        .is_none()
    {
        return Err(ApiError::not_found("asset not found"));
    }
    let mut response = if let Some(vault) = &selected.vault {
        let blob_id = selected
            .result(vault_blob_id(&selected.database, &id, vault_kind))?
            .ok_or_else(|| ApiError::not_found("image variant not found"))?;
        let bytes = vault.read_blob(&blob_id).map_err(vault_core_error)?;
        Body::from(bytes).into_response()
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

async fn serve_file(path: PathBuf, request: Request) -> ApiResult<Response> {
    let mut service = ServeFile::new(path);
    let response = service
        .try_call(request)
        .await
        .map_err(ApiError::internal)?;
    Ok(response.map(Body::new))
}

fn search_asset_ids(database: &Database, query: &str) -> illumia_core::db::Result<Vec<String>> {
    database.with_connection(|connection| {
        let sql = if query.chars().count() < 3 {
            "SELECT a.id
                 FROM search_fts f
                 JOIN assets a ON a.id = f.entity_id
                 WHERE f.entity_type = 'asset'
                   AND f.text LIKE '%' || ?1 || '%'
                   AND a.lifecycle = 'active'
                   AND a.visible_in_timeline = 1
                 ORDER BY a.taken_at DESC"
        } else {
            "SELECT a.id
                 FROM search_fts f
                 JOIN assets a ON a.id = f.entity_id
                 WHERE search_fts MATCH ?1
                   AND f.entity_type = 'asset'
                   AND a.lifecycle = 'active'
                   AND a.visible_in_timeline = 1
                 ORDER BY a.taken_at DESC"
        };
        let parameter = if query.chars().count() < 3 {
            query.to_owned()
        } else {
            format!("\"{}\"", query.replace('"', "\"\""))
        };
        let mut statement = connection.prepare(sql)?;
        let ids = statement
            .query_map([parameter], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    })
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
        illumia_core::db::Error::EmptyVaultTransfer => {
            ApiError::bad_request("provide exactly one of asset_ids or stack_id")
        }
        _ => ApiError::internal_silent(),
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
        "avif" => "image/avif",
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
}

fn validate_settings_patch(patch: &Map<String, Value>) -> ApiResult<Vec<SettingChange>> {
    patch
        .iter()
        .map(|(key, value)| match key.as_str() {
            "trash.retention_days" => Ok(SettingChange::TrashRetention(value_u32(key, value)?)),
            "dedup.retention_days" => Ok(SettingChange::DedupRetention(value_u32(key, value)?)),
            "jobs.thumbnail_concurrency" => {
                Ok(SettingChange::ThumbnailConcurrency(value_u32(key, value)?))
            }
            "jobs.ml_concurrency" => Ok(SettingChange::MlConcurrency(value_u32(key, value)?)),
            "ml.tau_high_override" => Ok(SettingChange::TauHigh(value_f64(key, value)?)),
            "ml.tau_low_override" => Ok(SettingChange::TauLow(value_f64(key, value)?)),
            "ml.min_cluster_size" => Ok(SettingChange::MinClusterSize(value_u32(key, value)?)),
            "ml.quality_gate" => match value.as_str() {
                Some("review_only") => Ok(SettingChange::QualityGate(QualityGate::ReviewOnly)),
                Some("strict") => Ok(SettingChange::QualityGate(QualityGate::Strict)),
                _ => Err(ApiError::bad_request(
                    "ml.quality_gate must be review_only or strict",
                )),
            },
            _ => Err(ApiError::bad_request(format!(
                "unsupported setting key: {key}"
            ))),
        })
        .collect()
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
        "ml.quality_gate": settings.quality_gate()?.map(quality_gate_name),
    }))
}

fn value_u32(key: &str, value: &Value) -> ApiResult<u32> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| ApiError::bad_request(format!("{key} must be a non-negative integer")))
}

fn value_f64(key: &str, value: &Value) -> ApiResult<f64> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| ApiError::bad_request(format!("{key} must be a finite number")))
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

fn job_state_name(state: JobState) -> &'static str {
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

fn job_snapshot(job: &Job) -> JobSnapshot {
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
