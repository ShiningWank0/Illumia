use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
        header::{
            CACHE_CONTROL, CONTENT_SECURITY_POLICY, COOKIE, HOST, ORIGIN, SET_COOKIE,
            X_CONTENT_TYPE_OPTIONS,
        },
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use illumia_core::sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{AppState, error::ApiError};

const SESSION_COOKIE: &str = "illumia_session";
const SETUP_TOKEN_HEADER: &str = "x-illumia-setup-token";
const FORWARDED_FOR_HEADER: &str = "x-forwarded-for";
const AUTH_FAILURE_LIMIT: usize = 5;
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
const MAX_AUTH_SOURCES: usize = 4096;
const MAX_ARGON2_CONCURRENCY: usize = 2;
const MAX_INGEST_CONCURRENCY: usize = 2;
const MAX_WEBSOCKETS: usize = 32;
const MIN_SETUP_TOKEN_BYTES: usize = 32;
const MAX_SETUP_TOKEN_BYTES: usize = 256;
const OVERFLOW_SOURCE: &str = "<overflow>";
const UNKNOWN_SOURCE: &str = "<unknown>";

const CSP: &str = "default-src 'self'; base-uri 'none'; object-src 'none'; \
    frame-ancestors 'none'; form-action 'self'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
    media-src 'self' blob:; connect-src 'self'; worker-src 'self' blob:; \
    font-src 'self'; manifest-src 'self'";
const PERMISSIONS_POLICY: &str =
    "camera=(), microphone=(), geolocation=(), payment=(), usb=(), browsing-topics=()";
const API_CACHE_CONTROL: &str = "private, no-store";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthTransport {
    Bearer,
    Cookie,
}

#[derive(Clone, Debug)]
pub struct Authenticated {
    pub token: String,
    pub transport: AuthTransport,
}

#[derive(Clone, Debug)]
pub struct Security {
    inner: Arc<SecurityInner>,
}

#[derive(Debug)]
struct SecurityInner {
    setup_token_hash: Option<[u8; 32]>,
    secure_cookies: bool,
    trust_proxy_headers: bool,
    auth_failures: Mutex<HashMap<String, VecDeque<Instant>>>,
    argon2_slots: Arc<Semaphore>,
    ingest_slots: Arc<Semaphore>,
    websocket_slots: Arc<Semaphore>,
}

impl Security {
    pub fn new(
        setup_token_hash: Option<[u8; 32]>,
        secure_cookies: bool,
        trust_proxy_headers: bool,
    ) -> Self {
        Self {
            inner: Arc::new(SecurityInner {
                setup_token_hash,
                secure_cookies,
                trust_proxy_headers,
                auth_failures: Mutex::new(HashMap::new()),
                argon2_slots: Arc::new(Semaphore::new(MAX_ARGON2_CONCURRENCY)),
                ingest_slots: Arc::new(Semaphore::new(MAX_INGEST_CONCURRENCY)),
                websocket_slots: Arc::new(Semaphore::new(MAX_WEBSOCKETS)),
            }),
        }
    }

    pub fn setup_token_required(&self) -> bool {
        self.inner.setup_token_hash.is_some()
    }

    pub fn verify_setup_token(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let Some(expected) = self.inner.setup_token_hash else {
            return Ok(());
        };
        let provided = headers
            .get(SETUP_TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|value| (MIN_SETUP_TOKEN_BYTES..=MAX_SETUP_TOKEN_BYTES).contains(&value.len()))
            .ok_or_else(|| ApiError::forbidden("initial setup token required"))?;
        let provided_hash: [u8; 32] = Sha256::digest(provided.as_bytes()).into();
        if bool::from(expected.ct_eq(&provided_hash)) {
            Ok(())
        } else {
            Err(ApiError::forbidden("invalid initial setup token"))
        }
    }

    pub fn session_cookie(&self, token: &str) -> HeaderValue {
        let secure = if self.inner.secure_cookies {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}={token}; Path=/api; HttpOnly; SameSite=Strict; \
             Max-Age=2592000{secure}"
        ))
        .expect("hex token produces a valid Set-Cookie header")
    }

    pub fn expired_session_cookie(&self) -> HeaderValue {
        let secure = if self.inner.secure_cookies {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}=; Path=/api; HttpOnly; SameSite=Strict; \
             Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT{secure}"
        ))
        .expect("static Set-Cookie header is valid")
    }

    pub fn cookie_token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        for header in headers.get_all(COOKIE) {
            let Ok(value) = header.to_str() else {
                continue;
            };
            for pair in value.split(';') {
                let Some((name, value)) = pair.trim().split_once('=') else {
                    continue;
                };
                if name == SESSION_COOKIE && !value.is_empty() {
                    return Some(value);
                }
            }
        }
        None
    }

    pub fn validate_cookie_origin(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let host = headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| ApiError::forbidden("same-origin request required"))?;
        let origin = headers
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| ApiError::forbidden("same-origin request required"))?;
        let uri = origin
            .parse::<Uri>()
            .map_err(|_| ApiError::forbidden("same-origin request required"))?;
        let authority = uri
            .authority()
            .map(|value| value.as_str())
            .ok_or_else(|| ApiError::forbidden("same-origin request required"))?;
        if authority.eq_ignore_ascii_case(host) {
            Ok(())
        } else {
            Err(ApiError::forbidden("same-origin request required"))
        }
    }

    pub fn try_argon2_slot(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        self.inner
            .argon2_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::too_many_requests("authentication is temporarily busy"))
    }

    pub fn try_websocket_slot(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        self.inner
            .websocket_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::too_many_requests("too many websocket connections"))
    }

    pub fn try_ingest_slot(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        self.inner
            .ingest_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::too_many_requests("too many concurrent image uploads"))
    }

    fn source_key(&self, request: &Request) -> String {
        if self.inner.trust_proxy_headers
            && let Some(address) = request
                .headers()
                .get(FORWARDED_FOR_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
                .map(str::trim)
                .and_then(|value| value.parse::<IpAddr>().ok())
        {
            return address.to_string();
        }
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map_or_else(
                || UNKNOWN_SOURCE.to_owned(),
                |ConnectInfo(address)| address.ip().to_string(),
            )
    }

    fn retry_after(&self, source: &str, now: Instant) -> Option<Duration> {
        let mut failures = self.auth_failures();
        prune_failures(&mut failures, now);
        let key = bounded_source(&failures, source);
        let attempts = failures.get(key)?;
        if attempts.len() < AUTH_FAILURE_LIMIT {
            return None;
        }
        attempts
            .front()
            .and_then(|oldest| AUTH_FAILURE_WINDOW.checked_sub(now.duration_since(*oldest)))
    }

    fn record_failure(&self, source: &str, now: Instant) {
        let mut failures = self.auth_failures();
        prune_failures(&mut failures, now);
        let key = bounded_source(&failures, source).to_owned();
        failures.entry(key).or_default().push_back(now);
    }

    fn clear_failures(&self, source: &str) {
        self.auth_failures().remove(source);
    }

    fn auth_failures(&self) -> MutexGuard<'_, HashMap<String, VecDeque<Instant>>> {
        self.inner
            .auth_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub async fn limit_auth_attempts(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let source = state.security.source_key(&request);
    if let Some(retry_after) = state.security.retry_after(&source, Instant::now()) {
        let mut response =
            ApiError::too_many_requests("too many authentication failures").into_response();
        if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
            response.headers_mut().insert("retry-after", value);
        }
        return response;
    }

    let response = next.run(request).await;
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            state.security.record_failure(&source, Instant::now());
        }
        status if status.is_success() => state.security.clear_failures(&source),
        _ => {}
    }
    response
}

pub async fn add_security_headers(request: Request, next: Next) -> Response {
    let api_response = request.uri().path() == "/api" || request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry(CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(CSP));
    headers
        .entry(X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    headers
        .entry("x-frame-options")
        .or_insert(HeaderValue::from_static("DENY"));
    headers
        .entry("referrer-policy")
        .or_insert(HeaderValue::from_static("no-referrer"));
    headers
        .entry("permissions-policy")
        .or_insert(HeaderValue::from_static(PERMISSIONS_POLICY));
    headers
        .entry("strict-transport-security")
        .or_insert(HeaderValue::from_static("max-age=31536000"));
    headers
        .entry("cross-origin-opener-policy")
        .or_insert(HeaderValue::from_static("same-origin"));
    headers
        .entry("cross-origin-resource-policy")
        .or_insert(HeaderValue::from_static("same-origin"));
    if api_response {
        headers
            .entry(CACHE_CONTROL)
            .or_insert(HeaderValue::from_static(API_CACHE_CONTROL));
    }
    response
}

pub fn is_safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

pub fn append_session_cookie(response: &mut Response, cookie: HeaderValue) {
    response.headers_mut().append(SET_COOKIE, cookie);
}

fn prune_failures(failures: &mut HashMap<String, VecDeque<Instant>>, now: Instant) {
    failures.retain(|_, attempts| {
        while attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= AUTH_FAILURE_WINDOW)
        {
            attempts.pop_front();
        }
        !attempts.is_empty()
    });
}

fn bounded_source<'a>(
    failures: &'a HashMap<String, VecDeque<Instant>>,
    source: &'a str,
) -> &'a str {
    if failures.contains_key(source) || failures.len() < MAX_AUTH_SOURCES {
        source
    } else {
        OVERFLOW_SOURCE
    }
}
