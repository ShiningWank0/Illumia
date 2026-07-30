//! HTTP vault session lifetime management.
//!
//! `vault: no-log` — do not log tokens, filenames, asset ids, search terms, or key material.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
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
    next_generation: AtomicU64,
}

#[derive(Debug)]
struct VaultSession {
    token_hash: [u8; 32],
    handle: VaultHandle,
    expires_at: Instant,
    generation: u64,
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
                next_generation: AtomicU64::new(1),
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
        {
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
            Some(VaultAccess {
                handle: current.handle.clone(),
                token_hash: current.token_hash,
            })
        }
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
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let expires_at = (Utc::now()
            + illumia_core::chrono::Duration::from_std(self.inner.ttl)
                .unwrap_or_else(|_| illumia_core::chrono::Duration::minutes(15)))
        .to_rfc3339_opts(SecondsFormat::Micros, true);
        self.session().replace(VaultSession {
            token_hash,
            handle,
            expires_at: expires_at_instant,
            generation,
        });
        self.arm_expiry(generation);
        IssuedSession { token, expires_at }
    }

    fn arm_expiry(&self, generation: u64) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                let remaining = {
                    let session = manager.session();
                    let Some(current) = session
                        .as_ref()
                        .filter(|current| current.generation == generation)
                    else {
                        return;
                    };
                    current.expires_at.saturating_duration_since(Instant::now())
                };
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining).await;
                    continue;
                }
                let mut session = manager.session();
                if session.as_ref().is_some_and(|current| {
                    current.generation == generation && current.expires_at <= Instant::now()
                }) {
                    session.take();
                }
                return;
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
