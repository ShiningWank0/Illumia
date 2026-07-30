use std::{
    fs,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use illumia_core::{blake3, db::Database, thumbnails, uuid::Uuid};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("illumia-server-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("test data directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct TestApp {
    _directory: TestDirectory,
    database: Database,
    router: Router,
}

impl TestApp {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let database = Database::open(directory.path()).expect("test database should open");
        Self {
            router: illumia_server::app(database.clone(), None),
            database,
            _directory: directory,
        }
    }

    async fn request(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router should respond");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes();
        let value = serde_json::from_slice(&bytes).expect("response should be JSON");
        (status, value)
    }

    async fn json(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        value: Value,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.request(
            builder
                .body(Body::from(value.to_string()))
                .expect("request should build"),
        )
        .await
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.request(builder.body(Body::empty()).expect("request should build"))
            .await
    }
}

#[tokio::test]
async fn complete_m1_api_flow() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let app = TestApp::new();

    let (status, error) = app.get("/api/settings", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["error"]["code"], "unauthorized");

    let (status, info) = app.get("/api/server/info", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["setup_completed"], false);

    let (status, setup) = app
        .json(
            Method::POST,
            "/api/auth/setup",
            None,
            json!({"password": "correct horse", "device_name": "setup device"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let setup_token = setup["token"].as_str().expect("setup should issue a token");
    assert_eq!(setup_token.len(), 64);

    let (status, _) = app
        .json(
            Method::POST,
            "/api/auth/login",
            None,
            json!({"password": "wrong", "device_name": "bad login"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, login) = app
        .json(
            Method::POST,
            "/api/auth/login",
            None,
            json!({"password": "correct horse", "device_name": "integration test"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let token = login["token"]
        .as_str()
        .expect("login should issue a token")
        .to_owned();
    assert_ne!(token, setup_token);

    let (status, devices) = app.get("/api/auth/devices", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(devices.as_array().expect("device array").len(), 2);

    let png = one_pixel_png();
    let checksum = blake3::hash(&png).to_hex().to_string();
    let (status, first) = upload(
        &app,
        &token,
        &png,
        "テスト画像.png",
        &checksum,
        "2026-07-30T12:34:56+09:00",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first:?}");
    assert_eq!(first["status"], "created");
    let first_id = first["id"].as_str().expect("created asset id").to_owned();

    let (status, duplicate) = upload(
        &app,
        &token,
        &png,
        "コピー.png",
        &checksum,
        "2026-07-30T12:35:56+09:00",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(duplicate["status"], "duplicate");
    assert_eq!(duplicate["duplicate_of"], first_id);
    let duplicate_id = duplicate["id"]
        .as_str()
        .expect("duplicate asset id")
        .to_owned();

    thumbnails::generate_thumbnails(&app.database, &first_id)
        .expect("thumbnail generation should succeed");
    for variant in ["thumbnail", "preview"] {
        let response = app
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/assets/{first_id}/{variant}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("variant request should build"),
            )
            .await
            .expect("variant should respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/webp");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public,max-age=31536000,immutable"
        );
        assert!(
            !response
                .into_body()
                .collect()
                .await
                .expect("variant should stream")
                .to_bytes()
                .is_empty()
        );
    }

    let (status, jobs) = app.get("/api/jobs?state=queued", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jobs.as_array().expect("queued jobs").len(), 2);

    let (status, exists) = app
        .json(
            Method::POST,
            "/api/assets/exists",
            Some(&token),
            json!({"hashes": [checksum]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exists["exists"][&checksum], first_id);

    let (status, buckets) = app
        .get("/api/timeline/buckets?granularity=day", Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(buckets, json!([{"key": "2026-07-30", "count": 1}]));

    let (status, bucket) = app
        .get(
            "/api/timeline/buckets/2026-07-30?granularity=day",
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bucket.as_array().expect("bucket items").len(), 1);
    assert_eq!(bucket[0]["id"], first_id);

    let (status, duplicates) = app.get("/api/duplicates", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(duplicates[0]["dup"]["id"], duplicate_id);
    assert_eq!(duplicates[0]["original"]["id"], first_id);

    let (status, _) = app
        .json(
            Method::DELETE,
            &format!("/api/assets/{first_id}"),
            Some(&token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, trash) = app.get("/api/trash", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(trash[0]["id"], first_id);
    assert!(trash[0]["purge_after"].is_string());

    let (status, hidden_search) = app
        .get(
            &format!("/api/search?q={}", percent_encode("スト画")),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hidden_search["assets"], json!([]));

    let (status, _) = app
        .json(
            Method::POST,
            &format!("/api/assets/{first_id}/restore"),
            Some(&token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, search) = app
        .get(
            &format!("/api/search?q={}", percent_encode("スト画")),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search["assets"].as_array().expect("assets").len(), 1);
    assert_eq!(search["assets"][0]["id"], first_id);
    assert_eq!(search["stacks"], json!([]));
    assert_eq!(search["clusters"], json!([]));

    let (status, short_search) = app
        .get(
            &format!("/api/search?q={}", percent_encode("画像")),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(short_search["assets"].as_array().expect("assets").len(), 1);

    let (status, settings) = app.get("/api/settings", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["trash.retention_days"], 30);
    assert_eq!(settings["dedup.retention_days"], 30);

    let (status, settings) = app
        .json(
            Method::PATCH,
            "/api/settings",
            Some(&token),
            json!({
                "trash.retention_days": 45,
                "jobs.thumbnail_concurrency": 2,
                "ml.quality_gate": "strict"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["trash.retention_days"], 45);
    assert_eq!(settings["jobs.thumbnail_concurrency"], 2);
    assert_eq!(settings["ml.quality_gate"], "strict");

    let (status, error) = app
        .json(
            Method::PATCH,
            "/api/settings",
            Some(&token),
            json!({"unknown.key": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "bad_request");

    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/assets/{first_id}/original"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("original should respond");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(header::CONTENT_DISPOSITION));
    let original = response
        .into_body()
        .collect()
        .await
        .expect("original should stream")
        .to_bytes();
    assert_eq!(original.as_ref(), png.as_slice());

    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/trash/{duplicate_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("purge should respond");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let (status, original) = app
        .get(&format!("/api/assets/{first_id}"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(original["id"], first_id);
}

async fn upload(
    app: &TestApp,
    token: &str,
    bytes: &[u8],
    filename: &str,
    checksum: &str,
    taken_at: &str,
) -> (StatusCode, Value) {
    let boundary = "illumia-test-boundary";
    let body = multipart_body(boundary, filename, bytes);
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/api/assets")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("X-Illumia-Checksum", checksum)
            .header("X-Illumia-Taken-At", taken_at)
            .body(Body::from(body))
            .expect("upload request should build"),
    )
    .await
}

fn multipart_body(boundary: &str, filename: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn one_pixel_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x72, 0x9c, 0x52, 0x67, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}
