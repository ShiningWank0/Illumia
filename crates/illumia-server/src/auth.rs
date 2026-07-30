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
    argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString},
    chrono::{SecondsFormat, TimeDelta, Utc},
    db::Database,
    rand,
    sha2::{Digest, Sha256},
    uuid::Uuid,
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    error::{ApiError, ApiResult},
    security::{AuthTransport, Authenticated},
};

const PASSWORD_HASH_KEY: &str = "auth.password_hash";
const MIN_NEW_PASSWORD_CHARS: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_DEVICE_NAME_CHARS: usize = 128;
const MAX_DEVICE_NAME_BYTES: usize = 512;
const LAST_USED_WRITE_INTERVAL_MINUTES: i64 = 5;

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
    version: &'static str,
    setup_completed: bool,
    authenticated: bool,
    setup_token_required: bool,
}

#[derive(Serialize)]
pub struct Device {
    id: String,
    device_name: String,
    created_at: String,
    last_used: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AuthService {
    database: Database,
}

impl AuthService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn setup_completed(&self) -> ApiResult<bool> {
        self.database
            .with_connection(|connection| {
                let mut statement =
                    connection.prepare("SELECT 1 FROM settings WHERE key = ?1 LIMIT 1")?;
                let mut rows = statement.query([PASSWORD_HASH_KEY])?;
                Ok(rows.next()?.is_some())
            })
            .map_err(Into::into)
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
        let token_hash = token_hash(&token);
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
        let parsed = PasswordHash::new(&password_hash).map_err(ApiError::internal)?;
        if Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_err()
        {
            return Err(ApiError::unauthorized());
        }
        self.issue_token(device_name)
    }

    pub fn verify_token(&self, token: &str) -> ApiResult<()> {
        if !self.setup_completed()? || token.len() != 64 {
            return Err(ApiError::unauthorized());
        }
        let hash = token_hash(token);
        let last_used = now();
        let write_cutoff = (Utc::now() - TimeDelta::minutes(LAST_USED_WRITE_INTERVAL_MINUTES))
            .to_rfc3339_opts(SecondsFormat::Micros, true);
        let valid = self
            .database
            .with_connection(|connection| {
                let valid = connection.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM auth_tokens WHERE token_hash = ?1
                     )",
                    [hash.as_slice()],
                    |row| row.get::<_, bool>(0),
                )?;
                if valid {
                    connection.execute(
                        "UPDATE auth_tokens
                         SET last_used = ?2
                         WHERE token_hash = ?1
                           AND (last_used IS NULL OR last_used < ?3)",
                        (hash.as_slice(), last_used.as_str(), write_cutoff.as_str()),
                    )?;
                }
                Ok(valid)
            })
            .map_err(ApiError::from)?;
        if valid {
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
                     FROM auth_tokens ORDER BY created_at DESC, id DESC",
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
        let changed = self
            .database
            .with_connection(|connection| {
                Ok(connection.execute("DELETE FROM auth_tokens WHERE id = ?1", [id])?)
            })
            .map_err(ApiError::from)?;
        if changed == 0 {
            Err(ApiError::not_found("device not found"))
        } else {
            Ok(())
        }
    }

    pub fn revoke_token(&self, token: &str) -> ApiResult<()> {
        let hash = token_hash(token);
        self.database
            .with_connection(|connection| {
                connection.execute(
                    "DELETE FROM auth_tokens WHERE token_hash = ?1",
                    [hash.as_slice()],
                )?;
                Ok(())
            })
            .map_err(ApiError::from)
    }

    fn issue_token(&self, device_name: &str) -> ApiResult<String> {
        let token = new_token();
        let hash = token_hash(&token);
        let id = Uuid::now_v7().to_string();
        let created_at = now();
        self.database
            .with_connection(|connection| {
                connection.execute(
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
                Ok(())
            })
            .map_err(ApiError::from)?;
        Ok(token)
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
        version: illumia_core::VERSION,
        setup_completed: state.auth.setup_completed()?,
        authenticated,
        setup_token_required: state.security.setup_token_required(),
    }))
}

pub async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Response> {
    state.security.verify_setup_token(&headers)?;
    validate_setup_credentials(&credentials.password, &credentials.device_name)?;
    let _permit = state.security.try_argon2_slot()?;
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || {
        auth.setup(&credentials.password, &credentials.device_name)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(token_response(&state, token))
}

pub async fn login(
    State(state): State<AppState>,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Response> {
    validate_login_credentials(&credentials.password, &credentials.device_name)?;
    let _permit = state.security.try_argon2_slot()?;
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || {
        auth.login(&credentials.password, &credentials.device_name)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(token_response(&state, token))
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

fn token_response(state: &AppState, token: String) -> Response {
    let cookie = state.security.session_cookie(&token);
    let mut response = Json(TokenResponse { token }).into_response();
    crate::security::append_session_cookie(&mut response, cookie);
    response
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

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)
}
