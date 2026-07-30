use axum::{
    Json,
    extract::{Path, Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use illumia_core::{
    argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString},
    chrono::{SecondsFormat, Utc},
    db::Database,
    rand,
    sha2::{Digest, Sha256},
    uuid::Uuid,
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    error::{ApiError, ApiResult},
};

const PASSWORD_HASH_KEY: &str = "auth.password_hash";

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
        validate_credentials(password, device_name)?;
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
        validate_credentials(password, device_name)?;
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
        let changed = self
            .database
            .with_connection(|connection| {
                Ok(connection.execute(
                    "UPDATE auth_tokens SET last_used = ?2 WHERE token_hash = ?1",
                    (hash.as_slice(), last_used.as_str()),
                )?)
            })
            .map_err(ApiError::from)?;
        if changed == 1 {
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

pub async fn server_info(State(state): State<AppState>) -> ApiResult<Json<ServerInfo>> {
    Ok(Json(ServerInfo {
        version: illumia_core::VERSION,
        setup_completed: state.auth.setup_completed()?,
    }))
}

pub async fn setup(
    State(state): State<AppState>,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Json<TokenResponse>> {
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || {
        auth.setup(&credentials.password, &credentials.device_name)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(TokenResponse { token }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Json<TokenResponse>> {
    let auth = state.auth.clone();
    let token = tokio::task::spawn_blocking(move || {
        auth.login(&credentials.password, &credentials.device_name)
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(Json(TokenResponse { token }))
}

pub async fn devices(State(state): State<AppState>) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(state.auth.devices()?))
}

pub async fn revoke_device(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<()> {
    state.auth.revoke(&id)
}

pub async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token(request.headers())?;
    state.auth.verify_token(token)?;
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

fn validate_credentials(password: &str, device_name: &str) -> ApiResult<()> {
    if password.is_empty() {
        return Err(ApiError::bad_request("password must not be empty"));
    }
    if device_name.trim().is_empty() {
        return Err(ApiError::bad_request("device_name must not be empty"));
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
