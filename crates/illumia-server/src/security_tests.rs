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
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

use crate::{EVENT_BUFFER, JSON_BODY_LIMIT, app_with_events};

struct TestApp {
    path: PathBuf,
    router: Router,
}

impl TestApp {
    fn new(setup_token: Option<&str>) -> Self {
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
            setup_token_hash,
            false,
            false,
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
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
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
