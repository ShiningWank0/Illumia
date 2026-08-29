use std::{fs, path::PathBuf, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use illumia_core::{
    db::Database,
    sha2::{Digest, Sha256},
    uuid::Uuid,
    vault::{self as core_vault, VAULT_BLOB_CHUNK_SIZE, VaultHandle},
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

use crate::{
    EVENT_BUFFER, JSON_BODY_LIMIT, api::MAX_WEBSOCKET_MESSAGE_BYTES, app_with_events, serve_http,
};

struct TestApp {
    path: PathBuf,
    router: Router,
}

impl TestApp {
    fn new(setup_token: Option<&str>) -> Self {
        Self::with_trusted_proxies(setup_token, Vec::new())
    }

    fn with_trusted_proxies(
        setup_token: Option<&str>,
        trusted_proxies: Vec<crate::config::TrustedProxy>,
    ) -> Self {
        let path = std::env::temp_dir().join(format!("illumia-security-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("test directory should be created");
        let database = Database::open(&path).expect("test database should open");
        let setup_token_hash =
            setup_token.map(|value| <[u8; 32]>::from(Sha256::digest(value.as_bytes())));
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let router = app_with_events(
            database,
            None,
            events,
            Duration::from_secs(15 * 60),
            crate::AppBuildOptions {
                setup_token_hash,
                secure_cookies: false,
                trusted_proxy_cidrs: trusted_proxies,
                ml_gate: None,
            },
        );
        Self { path, router }
    }

    async fn request(&self, request: Request<Body>) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("router should respond")
    }

    async fn json(
        &self,
        method: Method,
        uri: &str,
        body: Value,
        headers: &[(&str, &str)],
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        self.request(
            builder
                .body(Body::from(body.to_string()))
                .expect("request should build"),
        )
        .await
    }

    async fn json_from(
        &self,
        method: Method,
        uri: &str,
        body: Value,
        peer: &str,
        forwarded: &str,
    ) -> axum::response::Response {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", forwarded)
            .body(Body::from(body.to_string()))
            .expect("request should build");
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            peer.parse::<std::net::SocketAddr>()
                .expect("peer address should parse"),
        ));
        self.request(request).await
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn cloned_ml_gate_serializes_main_and_vault_work() {
    use std::{sync::mpsc, thread};

    let main_gate = crate::MlConcurrencyGate::new(1);
    let main_permit = main_gate.acquire();
    let vault_gate = main_gate.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let vault_worker = thread::spawn(move || {
        let _vault_permit = vault_gate.acquire();
        entered_tx.send(()).expect("signal Vault ML admission");
    });
    assert!(
        entered_rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "Vault inference must wait behind the main ML permit"
    );
    drop(main_permit);
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Vault inference should enter after the shared permit is released");
    vault_worker.join().expect("Vault gate worker");
}

#[tokio::test]
async fn vault_downloads_use_a_bounded_global_stream_gate() {
    let path = std::env::temp_dir().join(format!("illumia-vault-stream-{}", Uuid::now_v7()));
    fs::create_dir_all(&path).expect("test directory should be created");
    let database = Database::open(&path).expect("main database should open");
    core_vault::init(&path, "correct horse battery staple").expect("vault should initialize");
    let vault =
        VaultHandle::unlock(&path, "correct horse battery staple").expect("vault should unlock");
    let plaintext = vec![0x5a; VAULT_BLOB_CHUNK_SIZE * 3 + 17];
    let blob_id = vault.write_blob(&plaintext).expect("blob should encrypt");
    let (events, _) = broadcast::channel(EVENT_BUFFER);
    let state = crate::AppState::new(
        database,
        events,
        Duration::from_secs(15 * 60),
        None,
        false,
        Vec::new(),
        None,
    );

    let mut responses = Vec::new();
    for _ in 0..4 {
        responses.push(
            crate::api::vault_blob_response(&state, &vault, &blob_id)
                .await
                .expect("the configured stream slots should be admitted"),
        );
    }
    assert!(
        crate::api::vault_blob_response(&state, &vault, &blob_id)
            .await
            .is_err(),
        "a fifth concurrent plaintext stream must be rejected"
    );
    drop(responses);

    let response = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(response) = crate::api::vault_blob_response(&state, &vault, &blob_id).await {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnect should release a stream slot");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("stream should complete")
        .to_bytes();
    assert_eq!(bytes.as_ref(), plaintext.as_slice());
    drop(vault);
    drop(state);
    let _ = fs::remove_dir_all(path);
}

#[tokio::test]
async fn trusted_proxy_clients_do_not_share_auth_lockout_buckets() {
    let app = TestApp::with_trusted_proxies(
        None,
        vec!["10.0.0.0/8".parse().expect("trusted CIDR should parse")],
    );
    let setup = app
        .json_from(
            Method::POST,
            "/api/auth/setup",
            json!({"password": "correct horse battery staple", "device_name": "owner"}),
            "10.0.0.2:443",
            "198.51.100.2",
        )
        .await;
    assert_eq!(setup.status(), StatusCode::OK);

    for _ in 0..5 {
        let failed = app
            .json_from(
                Method::POST,
                "/api/auth/login",
                json!({"password": "wrong password", "device_name": "attacker"}),
                "10.0.0.2:443",
                "198.51.100.10",
            )
            .await;
        assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
    }
    let owner = app
        .json_from(
            Method::POST,
            "/api/auth/login",
            json!({"password": "correct horse battery staple", "device_name": "owner retry"}),
            "10.0.0.2:443",
            "198.51.100.11",
        )
        .await;
    assert_eq!(
        owner.status(),
        StatusCode::OK,
        "five failures from one forwarded client must not lock out another client"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn stalled_ml_health_probe_is_isolated_and_coalesced() {
    use std::{os::unix::net::UnixListener, sync::mpsc, thread};

    let path = PathBuf::from("/tmp").join(format!("ilmp-{}", Uuid::now_v7()));
    fs::create_dir_all(&path).expect("probe test directory should be created");
    let database = Database::open(&path).expect("probe test database should open");
    let socket_path = path.join("stall.sock");
    let listener = UnixListener::bind(&socket_path).expect("probe socket should bind");
    let (count_tx, count_rx) = mpsc::channel();
    let sidecar = thread::spawn(move || {
        let (_stream, _) = listener
            .accept()
            .expect("first health probe should connect");
        thread::sleep(Duration::from_millis(3_500));
        listener
            .set_nonblocking(true)
            .expect("listener should become nonblocking");
        let mut accepted = 1;
        while listener.accept().is_ok() {
            accepted += 1;
        }
        count_tx.send(accepted).expect("probe count should send");
    });
    let (events, _) = broadcast::channel(EVENT_BUFFER);
    let state = crate::AppState::new(
        database,
        events,
        Duration::from_secs(15 * 60),
        None,
        false,
        Vec::new(),
        None,
    );

    let probes = (0..8)
        .map(|_| {
            let probe_state = state.clone();
            let probe_path = socket_path.clone();
            tokio::spawn(async move { probe_state.ml_health(probe_path).await })
        })
        .collect::<Vec<_>>();
    let scheduled_at = std::time::Instant::now();
    tokio::task::yield_now().await;
    assert!(
        scheduled_at.elapsed() < Duration::from_millis(250),
        "a blocking UDS read must never run on the current-thread Tokio worker"
    );
    tokio::time::timeout(Duration::from_millis(250), async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        state.instance_id()
    })
    .await
    .expect("unrelated work should run while the sidecar is stalled")
    .expect("unrelated database work should succeed");
    for probe in probes {
        assert!(
            tokio::time::timeout(Duration::from_secs(4), probe)
                .await
                .expect("probe should obey its short timeout")
                .expect("probe task should not panic")
                .is_none()
        );
    }
    sidecar.join().expect("mock sidecar should finish");
    assert_eq!(
        count_rx.recv().expect("probe count should arrive"),
        1,
        "concurrent health checks must coalesce into one blocking UDS probe"
    );
    let _ = fs::remove_dir_all(path);
}

#[tokio::test]
async fn browser_cookie_auth_is_same_origin_and_revocable() {
    let app = TestApp::new(None);
    let unauthenticated_info = app
        .request(
            Request::builder()
                .uri("/api/server/info")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    let unauthenticated_info_json: Value = serde_json::from_slice(
        &unauthenticated_info
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes(),
    )
    .expect("server info JSON");
    assert!(unauthenticated_info_json["version"].is_null());

    let response = app
        .json(
            Method::POST,
            "/api/auth/setup",
            json!({"password": "security password", "device_name": "browser"}),
            &[],
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("session cookie")
        .to_owned();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/api"));
    let cookie = set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: Value = serde_json::from_slice(&bytes).expect("token response JSON");
    let token = json["token"].as_str().expect("native token").to_owned();

    let cookie_only = app
        .json(
            Method::POST,
            "/api/auth/login",
            json!({"password": "security password", "device_name": "web response"}),
            &[("x-illumia-auth-mode", "cookie")],
        )
        .await;
    assert_eq!(cookie_only.status(), StatusCode::NO_CONTENT);
    assert!(cookie_only.headers().get(header::SET_COOKIE).is_some());
    assert!(
        cookie_only
            .into_body()
            .collect()
            .await
            .expect("cookie-only body")
            .to_bytes()
            .is_empty()
    );

    let info = app
        .request(
            Request::builder()
                .uri("/api/server/info")
                .header(header::COOKIE, cookie.as_str())
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(info.status(), StatusCode::OK);
    assert!(
        info.headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_some()
    );
    assert_eq!(
        info.headers()
            .get("x-content-type-options")
            .and_then(|value| value.to_str().ok()),
        Some("nosniff")
    );
    let info_json: Value =
        serde_json::from_slice(&info.into_body().collect().await.expect("body").to_bytes())
            .expect("server info JSON");
    assert_eq!(info_json["authenticated"], true);
    assert_eq!(info_json["version"], illumia_core::VERSION);

    let missing_origin = app
        .json(
            Method::PATCH,
            "/api/settings",
            json!({"trash.retention_days": 31}),
            &[(header::COOKIE.as_str(), cookie.as_str())],
        )
        .await;
    assert_eq!(missing_origin.status(), StatusCode::FORBIDDEN);

    let wrong_origin = app
        .json(
            Method::PATCH,
            "/api/settings",
            json!({"trash.retention_days": 31}),
            &[
                (header::COOKIE.as_str(), cookie.as_str()),
                (header::HOST.as_str(), "illumia.example"),
                (header::ORIGIN.as_str(), "https://attacker.example"),
            ],
        )
        .await;
    assert_eq!(wrong_origin.status(), StatusCode::FORBIDDEN);

    let same_origin = app
        .json(
            Method::PATCH,
            "/api/settings",
            json!({"trash.retention_days": 31}),
            &[
                (header::COOKIE.as_str(), cookie.as_str()),
                (header::HOST.as_str(), "illumia.example"),
                (header::ORIGIN.as_str(), "https://illumia.example"),
            ],
        )
        .await;
    assert_eq!(same_origin.status(), StatusCode::OK);
    assert_eq!(
        same_origin
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );

    let query_token = app
        .request(
            Request::builder()
                .uri(format!("/api/ws?token={token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(query_token.status(), StatusCode::UNAUTHORIZED);

    let logout = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/logout")
                .header(header::COOKIE, cookie)
                .header(header::HOST, "illumia.example")
                .header(header::ORIGIN, "https://illumia.example")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert!(
        logout
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("Max-Age=0"))
    );

    let revoked = app
        .request(
            Request::builder()
                .uri("/api/settings")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tampered_password_hash_cost_is_rejected_before_argon2() {
    let app = TestApp::new(None);
    let setup = app
        .json(
            Method::POST,
            "/api/auth/setup",
            json!({"password": "security password", "device_name": "browser"}),
            &[],
        )
        .await;
    assert_eq!(setup.status(), StatusCode::OK);

    let database = Database::open(&app.path).expect("test database should reopen");
    database
        .with_connection(|connection| {
            let hash: String = connection.query_row(
                "SELECT value FROM settings WHERE key = 'auth.password_hash'",
                [],
                |row| row.get(0),
            )?;
            let tampered = hash.replace("m=19456", "m=4000000");
            connection.execute(
                "UPDATE settings SET value = ?1 WHERE key = 'auth.password_hash'",
                [tampered],
            )?;
            Ok(())
        })
        .expect("password hash should be tampered for the test");

    let login = app
        .json(
            Method::POST,
            "/api/auth/login",
            json!({"password": "security password", "device_name": "browser"}),
            &[],
        )
        .await;
    assert_eq!(login.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn setup_token_body_limit_rate_limit_and_cors_are_enforced() {
    let setup_token = "0123456789abcdef0123456789abcdef";
    let app = TestApp::new(Some(setup_token));

    let missing = app
        .json(
            Method::POST,
            "/api/auth/setup",
            json!({"password": "security password", "device_name": "browser"}),
            &[],
        )
        .await;
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let valid = app
        .json(
            Method::POST,
            "/api/auth/setup",
            json!({"password": "security password", "device_name": "browser"}),
            &[("x-illumia-setup-token", setup_token)],
        )
        .await;
    assert_eq!(valid.status(), StatusCode::OK);
    let valid_json: Value = serde_json::from_slice(
        &valid
            .into_body()
            .collect()
            .await
            .expect("setup body")
            .to_bytes(),
    )
    .expect("setup JSON");
    let token = valid_json["token"]
        .as_str()
        .expect("setup token response")
        .to_owned();

    for headers in [
        Vec::new(),
        vec![("x-illumia-setup-token", "definitely-the-wrong-token-value")],
        vec![("x-illumia-setup-token", setup_token)],
    ] {
        let repeated = app
            .json(
                Method::POST,
                "/api/auth/setup",
                json!({"password": "another secure password", "device_name": "attacker"}),
                &headers,
            )
            .await;
        assert_eq!(repeated.status(), StatusCode::CONFLICT);
    }

    let oversized = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(vec![b'a'; JSON_BODY_LIMIT + 1]))
                .expect("request"),
        )
        .await;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let oversized_setting = app
        .json(
            Method::PATCH,
            "/api/settings",
            json!({"jobs.thumbnail_concurrency": 65}),
            &[(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"))],
        )
        .await;
    assert_eq!(oversized_setting.status(), StatusCode::BAD_REQUEST);

    let too_many_hashes = app
        .json(
            Method::POST,
            "/api/assets/exists",
            json!({"hashes": vec![""; 4097]}),
            &[(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"))],
        )
        .await;
    assert_eq!(too_many_hashes.status(), StatusCode::BAD_REQUEST);

    let long_search = app
        .request(
            Request::builder()
                .uri(format!("/api/search?q={}", "a".repeat(257)))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(long_search.status(), StatusCode::BAD_REQUEST);

    let short_vault_password = app
        .json(
            Method::POST,
            "/api/vault/init",
            json!({"password": "short"}),
            &[(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"))],
        )
        .await;
    assert_eq!(short_vault_password.status(), StatusCode::BAD_REQUEST);

    // The upload route has a deliberate 129 MiB override; a 300 KiB invalid
    // image must reach image validation instead of inheriting the 256 KiB JSON
    // limit.
    let boundary = "illumia-security-boundary";
    let file = vec![0_u8; JSON_BODY_LIMIT + 32 * 1024];
    let checksum = illumia_core::blake3::hash(&file).to_hex().to_string();
    let mut multipart = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
         filename=\"invalid.png\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    multipart.extend_from_slice(&file);
    multipart.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let invalid_image = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/api/assets")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("x-illumia-checksum", checksum)
                .body(Body::from(multipart))
                .expect("request"),
        )
        .await;
    assert_eq!(invalid_image.status(), StatusCode::BAD_REQUEST);

    let preflight = app
        .request(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/settings")
                .header(header::ORIGIN, "https://attacker.example")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );

    let rate_limited = TestApp::new(Some(setup_token));
    for _ in 0..5 {
        let response = rate_limited
            .json(
                Method::POST,
                "/api/auth/setup",
                json!({"password": "security password", "device_name": "browser"}),
                &[],
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = rate_limited
        .json(
            Method::POST,
            "/api/auth/setup",
            json!({"password": "security password", "device_name": "browser"}),
            &[],
        )
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().get("retry-after").is_some());
}

/// 実 TCP listener を立て、`router` を serve する。テスト終了時に abort する。
struct ServedApp {
    path: PathBuf,
    addr: std::net::SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl ServedApp {
    /// setup を済ませ、Bearer token 付きで待ち受ける server を起動する。
    async fn start() -> (Self, String) {
        let path = std::env::temp_dir().join(format!("illumia-ws-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("test directory should be created");
        let database = Database::open(&path).expect("test database should open");
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let router = app_with_events(
            database,
            None,
            events,
            Duration::from_secs(15 * 60),
            crate::AppBuildOptions::default(),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral port should bind");
        let addr = listener.local_addr().expect("listener address");
        let serve_router = router.clone();
        let handle = tokio::spawn(async move {
            let _ = serve_http(listener, serve_router, std::future::pending()).await;
        });

        // device token を得る (WS は認証必須)。
        let response = reqwest_post(
            addr,
            "/api/auth/setup",
            r#"{"password":"security password","device_name":"ws-test"}"#,
        )
        .await;
        let token = response["token"]
            .as_str()
            .expect("setup should return a device token")
            .to_owned();

        (Self { path, addr, handle }, token)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}/api/ws", self.addr)
    }
}

#[tokio::test]
async fn one_peer_cannot_occupy_the_global_http_connection_pool() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const {
        assert!(crate::MAX_HTTP_CONNECTIONS_PER_IP < crate::MAX_HTTP_CONNECTIONS);
    }
    let (app, _token) = ServedApp::start().await;
    let mut held = Vec::new();
    for _ in 0..crate::MAX_HTTP_CONNECTIONS_PER_IP {
        let mut stream = tokio::net::TcpStream::connect(app.addr)
            .await
            .expect("connection within per-peer cap should open");
        stream
            .write_all(b"GET /api/server/info HTTP/1.1\r\n")
            .await
            .expect("partial header should write");
        held.push(stream);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut excess = tokio::net::TcpStream::connect(app.addr)
        .await
        .expect("kernel may accept the excess connection before admission rejects it");
    excess
        .write_all(b"GET /api/server/info HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("request should write");
    let mut byte = [0_u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(1), excess.read(&mut byte))
        .await
        .expect("excess peer connection should be rejected promptly");
    match closed {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("excess peer connection should close, got {other:?}"),
    }

    drop(held.pop());
    for _ in 0..20 {
        let mut retry = tokio::net::TcpStream::connect(app.addr)
            .await
            .expect("retry should connect");
        retry
            .write_all(b"GET /api/server/info HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("retry request should write");
        let mut response = [0_u8; 4096];
        if let Ok(Ok(count)) =
            tokio::time::timeout(Duration::from_millis(200), retry.read(&mut response)).await
            && response[..count].starts_with(b"HTTP/1.1 200")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("dropping a peer connection must release per-peer admission");
}

#[tokio::test]
async fn slow_headers_and_stalled_bodies_are_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (app, _token) = ServedApp::start().await;
    let mut slow_header = tokio::net::TcpStream::connect(app.addr)
        .await
        .expect("slow header socket should connect");
    slow_header
        .write_all(b"GET /api/server/info HTTP/1.1\r\n")
        .await
        .expect("partial header should write");
    tokio::time::sleep(crate::HTTP_HEADER_TIMEOUT + Duration::from_millis(100)).await;
    let mut byte = [0_u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_secs(1), slow_header.read(&mut byte))
            .await
            .is_ok(),
        "slow header must be cut off in finite time"
    );

    let mut slow_body = tokio::net::TcpStream::connect(app.addr)
        .await
        .expect("slow body socket should connect");
    slow_body
        .write_all(
            b"POST /api/auth/login HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{",
        )
        .await
        .expect("partial body should write");
    tokio::time::sleep(crate::HTTP_BODY_PROGRESS_TIMEOUT + Duration::from_millis(100)).await;
    let result = tokio::time::timeout(Duration::from_secs(1), slow_body.read(&mut byte)).await;
    assert!(
        result.is_ok(),
        "stalled request body must terminate in finite time"
    );
}

impl Drop for ServedApp {
    fn drop(&mut self) {
        self.handle.abort();
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// テスト用の最小 JSON POST (依存を増やさないため手書き HTTP)。
async fn reqwest_post(addr: std::net::SocketAddr, path: &str, body: &str) -> Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to test server");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let text = String::from_utf8_lossy(&raw);
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .expect("response should have a body");
    let decoded;
    let body = if headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
    {
        let (size, rest) = body.split_once("\r\n").expect("chunk size should exist");
        let size = usize::from_str_radix(size, 16).expect("chunk size should be hex");
        decoded = rest[..size].to_owned();
        decoded.as_str()
    } else {
        body
    };
    serde_json::from_str(body).expect("response body should be JSON")
}

/// 認証済み WS 接続を 1 本張る。成功したら stream を返す。
async fn open_ws(
    url: &str,
    token: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut request = url.into_client_request().expect("ws url should parse");
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("bearer header"),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(stream, _)| stream)
}

async fn authenticated_post_status(
    addr: std::net::SocketAddr,
    path: &str,
    token: &str,
) -> StatusCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to test server");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write authenticated request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read authenticated response");
    let status = String::from_utf8_lossy(&raw)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("HTTP status code");
    StatusCode::from_u16(status).expect("valid HTTP status")
}

#[tokio::test]
async fn logout_closes_an_already_authenticated_websocket() {
    use futures_util::StreamExt;

    let (app, token) = ServedApp::start().await;
    let mut socket = open_ws(&app.ws_url(), &token)
        .await
        .expect("authenticated websocket should connect");
    assert_eq!(
        authenticated_post_status(app.addr, "/api/auth/logout", &token).await,
        StatusCode::NO_CONTENT
    );

    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "logout must promptly close an active websocket"
    );
}

/// docs/12 が公開前必須とする WS connection flood の adversarial test (SEC-010)。
///
/// 検証内容:
///   - device tokenごとの上限まで接続でき、上限超過は 429 で拒否される
///   - 1 本閉じれば新規接続できる (permit が確実に解放される)
///   - 上限超過フレームを送っても permit がリークしない
#[tokio::test]
async fn websocket_connection_flood_is_bounded_and_permits_are_released() {
    use futures_util::SinkExt;

    // docs/12 が定める上限値そのものを固定する。実装側で黙って緩められた場合に
    // このテストが追随してしまわないよう、値自体を検証する。
    assert_eq!(crate::security::MAX_WEBSOCKETS, 32);
    assert_eq!(crate::security::MAX_WEBSOCKETS_PER_TOKEN, 4);

    let (app, token) = ServedApp::start().await;
    let url = app.ws_url();

    // 1. 上限まで接続を保持する。
    let mut connections = Vec::new();
    for index in 0..crate::security::MAX_WEBSOCKETS_PER_TOKEN {
        let socket = open_ws(&url, &token)
            .await
            .unwrap_or_else(|error| panic!("connection {index} should succeed: {error}"));
        connections.push(socket);
    }
    assert_eq!(connections.len(), crate::security::MAX_WEBSOCKETS_PER_TOKEN);

    // 2. 上限 + 1 本目は 429 で拒否される。
    let rejected = open_ws(&url, &token).await;
    match rejected {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        Err(other) => panic!("expected an HTTP 429 rejection, got {other}"),
        Ok(_) => panic!("connections beyond the limit must be rejected"),
    }

    // 3. 1 本閉じれば再び接続できる (permit の解放)。
    let mut closed = connections.pop().expect("a connection to close");
    closed.close(None).await.expect("close should be sent");
    drop(closed);

    let reconnected = wait_for_slot(&url, &token).await;
    assert!(
        reconnected.is_some(),
        "closing a connection must release its permit"
    );
    connections.push(reconnected.expect("reconnected socket"));

    // 4. 上限超過フレームを送った接続も、切断後に permit を返す。
    let mut offender = connections.pop().expect("a connection to abuse");
    let oversized = vec![b'x'; MAX_WEBSOCKET_MESSAGE_BYTES * 2];
    // 送信自体は失敗しうる (server 側が閉じる)。permit が戻ることだけを見る。
    let _ = offender
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            oversized.into(),
        ))
        .await;
    drop(offender);

    let after_abuse = wait_for_slot(&url, &token).await;
    assert!(
        after_abuse.is_some(),
        "an over-limit frame must not leak the connection permit"
    );
}

/// permit の解放は server 側の非同期処理なので、短時間ポーリングして待つ。
async fn wait_for_slot(
    url: &str,
    token: &str,
) -> Option<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    for _ in 0..50 {
        if let Ok(socket) = open_ws(url, token).await {
            return Some(socket);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}
