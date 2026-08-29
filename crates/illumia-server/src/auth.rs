use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Json,
    extract::{Extension, Path, Request, State},
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, SET_COOKIE},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use illumia_core::{
    argon2::{
        Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString,
    },
    chrono::{SecondsFormat, TimeDelta, Utc},
    db::Database,
    rand,
    sha2::{Digest, Sha256},
    uuid::Uuid,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::{
    AppState,
    error::{ApiError, ApiResult},
    security::{AuthTransport, Authenticated},
};

const PASSWORD_HASH_KEY: &str = "auth.password_hash";
const DEVICE_LIMIT_KEY: &str = "auth.device_limit";
const MIN_NEW_PASSWORD_CHARS: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_DEVICE_NAME_CHARS: usize = 128;
const MAX_DEVICE_NAME_BYTES: usize = 512;
const MAX_DEVICE_TOKENS: usize = 256;
const MAX_PASSWORD_HASH_BYTES: usize = 512;
const MAX_PASSWORD_HASH_MEMORY_KIB: u32 = 256 * 1024;
const MAX_PASSWORD_HASH_ITERATIONS: u32 = 10;
const MAX_PASSWORD_HASH_PARALLELISM: u32 = 16;
const MIN_PASSWORD_HASH_OUTPUT_BYTES: usize = 32;
const MAX_PASSWORD_HASH_OUTPUT_BYTES: usize = 64;
const LAST_USED_WRITE_INTERVAL_MINUTES: i64 = 5;
const AUTH_MODE_HEADER: &str = "x-illumia-auth-mode";
const COOKIE_AUTH_MODE: &str = "cookie";

#[derive(Deserialize)]
pub struct Credentials {
    password: String,
    device_name: String,
}

#[derive(Serialize)]
pub struct TokenResponse {
    token: String,
}

#[derive(Serialize)]
pub struct ServerInfo {
    version: Option<&'static str>,
    setup_completed: bool,
    authenticated: bool,
    setup_token_required: bool,
    /// このサーバーインスタンスの一意識別子。ネイティブクライアントが初回接続で
    /// pin し、以後の接続先が同一サーバーであることの確認に使う (→ SEC-002)。
    instance_id: String,
}

#[derive(Serialize)]
pub struct Device {
    id: String,
    device_name: String,
    created_at: String,
    last_used: Option<String>,
}

#[derive(Clone)]
pub struct AuthService {
    database: Database,
    state: Arc<AuthState>,
}

struct AuthState {
    setup_completed: AtomicBool,
    token_hashes: Mutex<Option<Vec<[u8; 32]>>>,
}

impl std::fmt::Debug for AuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthService")
            .field("database", &self.database)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

impl AuthService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            state: Arc::new(AuthState {
                setup_completed: AtomicBool::new(false),
                token_hashes: Mutex::new(None),
            }),
        }
    }

    pub fn setup_completed(&self) -> ApiResult<bool> {
        if self.state.setup_completed.load(Ordering::Acquire) {
            return Ok(true);
        }
        let completed = self
            .database
            .with_connection(|connection| {
                let mut statement =
                    connection.prepare("SELECT 1 FROM settings WHERE key = ?1 LIMIT 1")?;
                let mut rows = statement.query([PASSWORD_HASH_KEY])?;
                Ok(rows.next()?.is_some())
            })
            .map_err(ApiError::from)?;
        if completed {
            self.state.setup_completed.store(true, Ordering::Release);
        }
        Ok(completed)
    }

    pub fn setup(&self, password: &str, device_name: &str) -> ApiResult<String> {
        validate_setup_credentials(password, device_name)?;
        if self.setup_completed()? {
            return Err(ApiError::conflict(
                "setup_already_completed",
                "initial setup has already completed",
            ));
        }

        let salt_bytes = rand::random::<[u8; 16]>();
        let salt = SaltString::encode_b64(&salt_bytes)
            .map_err(|error| ApiError::internal(format!("password salt: {error}")))?;
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|error| ApiError::internal(format!("password hash: {error}")))?
            .to_string();
        let token = new_token();
        let token_hash = token_hash(&token).expect("new tokens are valid hexadecimal");
        let token_id = Uuid::now_v7().to_string();
        let created_at = now();

        self.database
            .with_connection(|connection| {
                let transaction = connection.unchecked_transaction()?;
                let existing: i64 = transaction.query_row(
                    "SELECT count(*) FROM settings WHERE key = ?1",
                    [PASSWORD_HASH_KEY],
                    |row| row.get(0),
                )?;
                if existing != 0 {
                    return Err(illumia_core::db::Error::InvalidSetting(PASSWORD_HASH_KEY));
                }
                transaction.execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2)",
                    [PASSWORD_HASH_KEY, password_hash.as_str()],
                )?;
                transaction.execute(
                    "INSERT INTO auth_tokens(
                        id, device_name, token_hash, created_at, last_used
                     ) VALUES (?1, ?2, ?3, ?4, NULL)",
                    (
                        token_id.as_str(),
                        device_name,
                        token_hash.as_slice(),
                        created_at.as_str(),
                    ),
                )?;
                transaction.commit()?;
                Ok(())
            })
            .map_err(|error| match error {
                illumia_core::db::Error::InvalidSetting(_) => ApiError::conflict(
                    "setup_already_completed",
                    "initial setup has already completed",
                ),
                other => ApiError::from(other),
            })?;
        self.state.setup_completed.store(true, Ordering::Release);
        self.remember_token_hash(token_hash);
        Ok(token)
    }

    pub fn login(&self, password: &str, device_name: &str) -> ApiResult<String> {
        validate_login_credentials(password, device_name)?;
        let password_hash = self
            .database
            .with_connection(|connection| {
                let mut statement =
                    connection.prepare("SELECT value FROM settings WHERE key = ?1")?;
                let mut rows = statement.query([PASSWORD_HASH_KEY])?;
                Ok(rows
                    .next()?
                    .map(|row| row.get::<_, String>(0))
                    .transpose()?)
            })
            .map_err(ApiError::from)?
            .ok_or_else(ApiError::unauthorized)?;
        let parsed = validated_password_hash(&password_hash)?;
        if Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_err()
        {
            return Err(ApiError::unauthorized());
        }
        self.issue_token(device_name)
    }

    pub fn verify_token(&self, token: &str) -> ApiResult<()> {
        let hash = token_hash(token).ok_or_else(ApiError::unauthorized)?;
        if !self.token_is_known(&hash)? {
            return Err(ApiError::unauthorized());
        }
        let last_used = now();
        let write_cutoff = (Utc::now() - TimeDelta::minutes(LAST_USED_WRITE_INTERVAL_MINUTES))
            .to_rfc3339_opts(SecondsFormat::Micros, true);
        self.database
            .with_connection(|connection| {
                connection.execute(
                    "UPDATE auth_tokens
                     SET last_used = ?2
                     WHERE token_hash = ?1
                       AND (last_used IS NULL OR last_used < ?3)",
                    (hash.as_slice(), last_used.as_str(), write_cutoff.as_str()),
                )?;
                Ok(())
            })
            .map_err(ApiError::from)?;
        Ok(())
    }

    /// Revalidates an already-authenticated long-lived connection against the
    /// process-local revocation cache without polling SQLite per connection.
    pub fn verify_token_cached(&self, token: &str) -> ApiResult<()> {
        let hash = token_hash(token).ok_or_else(ApiError::unauthorized)?;
        if self.token_is_known(&hash)? {
            Ok(())
        } else {
            Err(ApiError::unauthorized())
        }
    }

    pub fn devices(&self) -> ApiResult<Vec<Device>> {
        self.database
            .with_connection(|connection| {
                let mut statement = connection.prepare(
                    "SELECT id, device_name, created_at, last_used
                     FROM auth_tokens ORDER BY created_at DESC, id DESC
                     LIMIT 256",
                )?;
                let devices = statement
                    .query_map([], |row| {
                        Ok(Device {
                            id: row.get(0)?,
                            device_name: row.get(1)?,
                            created_at: row.get(2)?,
                            last_used: row.get(3)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(devices)
            })
            .map_err(Into::into)
    }

    pub fn revoke(&self, id: &str) -> ApiResult<()> {
        let removed_hash = self
            .database
            .with_connection(|connection| {
                let hash = {
                    let mut statement =
                        connection.prepare("SELECT token_hash FROM auth_tokens WHERE id = ?1")?;
                    let mut rows = statement.query([id])?;
                    rows.next()?
                        .map(|row| row.get::<_, Vec<u8>>(0))
                        .transpose()?
                };
                if hash.is_some() {
                    connection.execute("DELETE FROM auth_tokens WHERE id = ?1", [id])?;
                }
                Ok(hash)
            })
            .map_err(ApiError::from)?;
        let removed_hash = removed_hash.ok_or_else(|| ApiError::not_found("device not found"))?;
        let removed_hash: [u8; 32] = removed_hash
            .try_into()
            .map_err(|_| ApiError::internal_silent())?;
        self.forget_token_hash(&removed_hash);
        Ok(())
    }

    pub fn revoke_token(&self, token: &str) -> ApiResult<()> {
        let hash = token_hash(token).ok_or_else(ApiError::unauthorized)?;
        self.database
            .with_connection(|connection| {
                connection.execute(
                    "DELETE FROM auth_tokens WHERE token_hash = ?1",
                    [hash.as_slice()],
                )?;
                Ok(())
            })
            .map_err(ApiError::from)?;
        self.forget_token_hash(&hash);
        Ok(())
    }

    fn issue_token(&self, device_name: &str) -> ApiResult<String> {
        let token = new_token();
        let hash = token_hash(&token).expect("new tokens are valid hexadecimal");
        let id = Uuid::now_v7().to_string();
        let created_at = now();
        self.database
            .with_connection(|connection| {
                let transaction = connection.unchecked_transaction()?;
                let count: i64 =
                    transaction
                        .query_row("SELECT count(*) FROM auth_tokens", [], |row| row.get(0))?;
                if count
                    >= i64::try_from(MAX_DEVICE_TOKENS)
                        .expect("device token limit fits into SQLite integer")
                {
                    return Err(illumia_core::db::Error::InvalidSetting(DEVICE_LIMIT_KEY));
                }
                transaction.execute(
                    "INSERT INTO auth_tokens(
                        id, device_name, token_hash, created_at, last_used
                     ) VALUES (?1, ?2, ?3, ?4, NULL)",
                    (
                        id.as_str(),
                        device_name,
                        hash.as_slice(),
                        created_at.as_str(),
                    ),
                )?;
                transaction.commit()?;
                Ok(())
            })
            .map_err(|error| match error {
                illumia_core::db::Error::InvalidSetting(DEVICE_LIMIT_KEY) => ApiError::conflict(
                    "device_limit_reached",
                    "revoke an existing device before logging in again",
                ),
                other => ApiError::from(other),
            })?;
        self.remember_token_hash(hash);
        Ok(token)
    }

    fn token_is_known(&self, candidate: &[u8; 32]) -> ApiResult<bool> {
        let mut cache = self.token_hashes();
        if cache.is_none() {
            *cache = Some(self.load_token_hashes()?);
        }
        let known = cache
            .as_ref()
            .expect("token cache was initialized")
            .iter()
            .fold(0_u8, |matched, expected| {
                matched | expected.ct_eq(candidate).unwrap_u8()
            });
        Ok(known == 1)
    }

    fn load_token_hashes(&self) -> ApiResult<Vec<[u8; 32]>> {
        self.database
            .with_connection(|connection| {
                let mut statement = connection.prepare(
                    "SELECT token_hash FROM auth_tokens
                     ORDER BY created_at, id
                     LIMIT 257",
                )?;
                let values = statement
                    .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                if values.len() > MAX_DEVICE_TOKENS {
                    return Err(illumia_core::db::Error::InvalidSetting(DEVICE_LIMIT_KEY));
                }
                values
                    .into_iter()
                    .map(|value| {
                        value
                            .try_into()
                            .map_err(|_| illumia_core::db::Error::InvalidSetting(DEVICE_LIMIT_KEY))
                    })
                    .collect()
            })
            .map_err(|_| ApiError::internal_silent())
    }

    fn remember_token_hash(&self, hash: [u8; 32]) {
        if let Some(cache) = self.token_hashes().as_mut()
            && !cache.iter().any(|known| known == &hash)
        {
            cache.push(hash);
        }
    }

    fn forget_token_hash(&self, hash: &[u8; 32]) {
        if let Some(cache) = self.token_hashes().as_mut() {
            cache.retain(|known| known != hash);
        }
    }

    fn token_hashes(&self) -> MutexGuard<'_, Option<Vec<[u8; 32]>>> {
        self.state
            .token_hashes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub async fn server_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ServerInfo>> {
    let authenticated = authentication_from_headers(&state, &headers)
        .and_then(|authentication| state.auth.verify_token(&authentication.token))
        .is_ok();
    Ok(Json(ServerInfo {
        version: authenticated.then_some(illumia_core::VERSION),
        setup_completed: state.auth.setup_completed()?,
        authenticated,
        setup_token_required: state.security.setup_token_required(),
        instance_id: state.instance_id()?,
    }))
}

pub async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Response> {
    state.security.verify_setup_token(&headers)?;
    let cookie_only = wants_cookie_only(&headers);
    validate_setup_credentials(&credentials.password, &credentials.device_name)?;
    let _permit = state.security.try_argon2_slot()?;
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || {
        auth.setup(&credentials.password, &credentials.device_name)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(token_response(&state, token, cookie_only))
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Response> {
    let cookie_only = wants_cookie_only(&headers);
    validate_login_credentials(&credentials.password, &credentials.device_name)?;
    let _permit = state.security.try_argon2_slot()?;
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || {
        auth.login(&credentials.password, &credentials.device_name)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(token_response(&state, token, cookie_only))
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(authentication): Extension<Authenticated>,
) -> ApiResult<Response> {
    state.auth.revoke_token(&authentication.token)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, state.security.expired_session_cookie());
    Ok(response)
}

pub async fn devices(State(state): State<AppState>) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(state.auth.devices()?))
}

pub async fn revoke_device(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<()> {
    state.auth.revoke(&id)
}

pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let authentication = authentication_from_headers(&state, request.headers())?;
    if authentication.transport == AuthTransport::Cookie
        && !crate::security::is_safe_method(request.method())
    {
        state.security.validate_cookie_origin(request.headers())?;
    }
    state.auth.verify_token(&authentication.token)?;
    request.extensions_mut().insert(authentication);
    Ok(next.run(request).await)
}

pub async fn require_websocket_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let authentication = authentication_from_headers(&state, request.headers())?;
    if authentication.transport == AuthTransport::Cookie {
        state.security.validate_cookie_origin(request.headers())?;
    }
    state.auth.verify_token(&authentication.token)?;
    request.extensions_mut().insert(authentication);
    Ok(next.run(request).await)
}

pub(crate) fn bearer_token(headers: &axum::http::HeaderMap) -> ApiResult<&str> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::unauthorized)?;
    value
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
        .ok_or_else(ApiError::unauthorized)
}

pub(crate) fn authentication_from_headers(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<Authenticated> {
    if headers.contains_key(AUTHORIZATION) {
        return Ok(Authenticated {
            token: bearer_token(headers)?.to_owned(),
            transport: AuthTransport::Bearer,
        });
    }
    let token = state
        .security
        .cookie_token(headers)
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Authenticated {
        token: token.to_owned(),
        transport: AuthTransport::Cookie,
    })
}

fn token_response(state: &AppState, token: String, cookie_only: bool) -> Response {
    let cookie = state.security.session_cookie(&token);
    let mut response = if cookie_only {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Json(TokenResponse { token }).into_response()
    };
    crate::security::append_session_cookie(&mut response, cookie);
    response
}

fn wants_cookie_only(headers: &HeaderMap) -> bool {
    headers
        .get(AUTH_MODE_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case(COOKIE_AUTH_MODE))
}

fn validate_setup_credentials(password: &str, device_name: &str) -> ApiResult<()> {
    validate_login_credentials(password, device_name)?;
    if password.chars().count() < MIN_NEW_PASSWORD_CHARS {
        return Err(ApiError::bad_request(format!(
            "new password must contain at least {MIN_NEW_PASSWORD_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_login_credentials(password: &str, device_name: &str) -> ApiResult<()> {
    if password.is_empty() {
        return Err(ApiError::bad_request("password must not be empty"));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::bad_request("password is too long"));
    }
    if device_name.trim().is_empty() {
        return Err(ApiError::bad_request("device_name must not be empty"));
    }
    if device_name.len() > MAX_DEVICE_NAME_BYTES
        || device_name.chars().count() > MAX_DEVICE_NAME_CHARS
    {
        return Err(ApiError::bad_request("device_name is too long"));
    }
    if device_name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "device_name must not contain control characters",
        ));
    }
    Ok(())
}

fn new_token() -> String {
    illumia_core::hex::encode(rand::random::<[u8; 32]>())
}

fn token_hash(token: &str) -> Option<[u8; 32]> {
    if token.len() != 64 || illumia_core::hex::decode(token).ok()?.len() != 32 {
        return None;
    }
    Some(Sha256::digest(token.as_bytes()).into())
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn validated_password_hash(encoded: &str) -> ApiResult<PasswordHash<'_>> {
    if encoded.len() > MAX_PASSWORD_HASH_BYTES {
        return Err(ApiError::internal_silent());
    }
    let parsed = PasswordHash::new(encoded).map_err(|_| ApiError::internal_silent())?;
    if parsed.algorithm.as_str() != "argon2id" || parsed.version != Some(0x13) {
        return Err(ApiError::internal_silent());
    }
    let parameters = Params::try_from(&parsed).map_err(|_| ApiError::internal_silent())?;
    let output_length = parameters
        .output_len()
        .unwrap_or(Params::DEFAULT_OUTPUT_LEN);
    if parameters.m_cost() < Params::DEFAULT_M_COST
        || parameters.m_cost() > MAX_PASSWORD_HASH_MEMORY_KIB
        || parameters.t_cost() < Params::DEFAULT_T_COST
        || parameters.t_cost() > MAX_PASSWORD_HASH_ITERATIONS
        || parameters.p_cost() > MAX_PASSWORD_HASH_PARALLELISM
        || !(MIN_PASSWORD_HASH_OUTPUT_BYTES..=MAX_PASSWORD_HASH_OUTPUT_BYTES)
            .contains(&output_length)
        || parsed.salt.is_none()
        || parsed.hash.is_none()
    {
        return Err(ApiError::internal_silent());
    }
    Ok(parsed)
}
