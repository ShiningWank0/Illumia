//! HTTP vault session lifetime management.
//!
//! `vault: no-log` — do not log tokens, filenames, asset ids, search terms, or key material.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use illumia_core::{
    chrono::{SecondsFormat, Utc},
    hex, rand,
    sha2::{Digest, Sha256},
    vault::VaultHandle,
};

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{AppState, error::ApiError};

pub(crate) const SESSION_HEADER: &str = "X-Vault-Session";

#[derive(Clone, Debug)]
pub(crate) struct VaultSessionManager {
    inner: Arc<SessionManagerInner>,
}

#[derive(Debug)]
struct SessionManagerInner {
    data_root: PathBuf,
    ttl: Duration,
    session: Mutex<Option<VaultSession>>,
}

#[derive(Debug)]
struct VaultSession {
    token_hash: [u8; 32],
    handle: VaultHandle,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct VaultAccess {
    pub(crate) handle: VaultHandle,
    token_hash: [u8; 32],
}

#[derive(Debug)]
pub(crate) struct IssuedSession {
    pub(crate) token: String,
    pub(crate) expires_at: String,
}

impl VaultSessionManager {
    pub(crate) fn new(data_root: impl AsRef<Path>, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(SessionManagerInner {
                data_root: data_root.as_ref().to_path_buf(),
                ttl,
                session: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn initialized(&self) -> bool {
        self.inner
            .data_root
            .join("vault")
            .join("vault.keyfile")
            .is_file()
    }

    /// Opens the vault with a password and replaces any existing session.
    ///
    /// `vault: no-log`
    pub(crate) fn unlock_with_password(
        &self,
        password: &str,
    ) -> illumia_core::db::Result<IssuedSession> {
        let handle = VaultHandle::unlock(&self.inner.data_root, password)?;
        Ok(self.issue(handle))
    }

    /// Opens the vault with a recovery key and replaces any existing session.
    ///
    /// `vault: no-log`
    pub(crate) fn unlock_with_recovery(
        &self,
        recovery_key: &str,
    ) -> illumia_core::db::Result<IssuedSession> {
        let handle = VaultHandle::unlock_with_recovery(&self.inner.data_root, recovery_key)?;
        Ok(self.issue(handle))
    }

    /// Validates and extends the single active session.
    ///
    /// `vault: no-log`
    pub(crate) fn authenticate(&self, token: &str) -> Option<VaultAccess> {
        let candidate = token_hash(token)?;
        let now = Instant::now();
        let access = {
            let mut session = self.session();
            let current = session.as_mut()?;
            if current.expires_at <= now {
                session.take();
                return None;
            }
            if !constant_time_eq(&current.token_hash, &candidate) {
                return None;
            }
            current.expires_at = now + self.inner.ttl;
            VaultAccess {
                handle: current.handle.clone(),
                token_hash: current.token_hash,
            }
        };
        self.arm_expiry();
        Some(access)
    }

    /// Removes the active session only when it is still the authenticated one.
    ///
    /// `vault: no-log`
    pub(crate) fn lock(&self, access: &VaultAccess) {
        let mut session = self.session();
        if session
            .as_ref()
            .is_some_and(|current| constant_time_eq(&current.token_hash, &access.token_hash))
        {
            session.take();
        }
    }

    pub(crate) fn unlocked(&self) -> bool {
        let mut session = self.session();
        if session
            .as_ref()
            .is_some_and(|current| current.expires_at <= Instant::now())
        {
            session.take();
        }
        session.is_some()
    }

    fn issue(&self, handle: VaultHandle) -> IssuedSession {
        let token_bytes = rand::random::<[u8; 32]>();
        let token = hex::encode(token_bytes);
        let token_hash = Sha256::digest(token.as_bytes()).into();
        let expires_at_instant = Instant::now() + self.inner.ttl;
        let expires_at = (Utc::now()
            + illumia_core::chrono::Duration::from_std(self.inner.ttl)
                .unwrap_or_else(|_| illumia_core::chrono::Duration::minutes(15)))
        .to_rfc3339_opts(SecondsFormat::Micros, true);
        self.session().replace(VaultSession {
            token_hash,
            handle,
            expires_at: expires_at_instant,
        });
        self.arm_expiry();
        IssuedSession { token, expires_at }
    }

    fn arm_expiry(&self) {
        let manager = self.clone();
        let ttl = self.inner.ttl;
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let mut session = manager.session();
            if session
                .as_ref()
                .is_some_and(|current| current.expires_at <= Instant::now())
            {
                session.take();
            }
        });
    }

    fn session(&self) -> MutexGuard<'_, Option<VaultSession>> {
        self.inner
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn token_hash(token: &str) -> Option<[u8; 32]> {
    if token.len() != 64 || hex::decode(token).ok()?.len() != 32 {
        return None;
    }
    Some(Sha256::digest(token.as_bytes()).into())
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

/// Hides every missing, stale, or invalid vault session behind the same 404.
///
/// `vault: no-log`
pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(token) = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(ApiError::not_found("not found"));
    };
    let Some(access) = state.vault.authenticate(token) else {
        return Err(ApiError::not_found("not found"));
    };
    request.extensions_mut().insert(access);
    Ok(next.run(request).await)
}
