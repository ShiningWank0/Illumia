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
    assert!(info["version"].is_null());

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
            "private,max-age=31536000,immutable"
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

#[tokio::test]
async fn manga_stack_api_preserves_structure_when_a_page_is_trashed() {
    let app = TestApp::new();
    let (status, setup) = app
        .json(
            Method::POST,
            "/api/auth/setup",
            None,
            json!({"password": "stack password", "device_name": "stack test"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let token = setup["token"]
        .as_str()
        .expect("setup should issue a token")
        .to_owned();

    let (status, unauthorized) = app
        .json(
            Method::POST,
            "/api/stacks",
            None,
            json!({"title": "銀河漫画作品", "asset_ids": []}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized["error"]["code"], "unauthorized");

    let png = one_pixel_png();
    let checksum = blake3::hash(&png).to_hex().to_string();
    let mut asset_ids = Vec::new();
    for (index, filename) in ["一頁.png", "二頁.png", "三頁.png"].iter().enumerate() {
        let (status, asset) = upload(
            &app,
            &token,
            &png,
            filename,
            &checksum,
            &format!("2026-07-30T12:3{}:56+09:00", index + 1),
        )
        .await;
        assert!(
            matches!(status, StatusCode::CREATED | StatusCode::OK),
            "{asset:?}"
        );
        asset_ids.push(asset["id"].as_str().expect("asset id").to_owned());
    }

    let (status, created) = app
        .json(
            Method::POST,
            "/api/stacks",
            Some(&token),
            json!({
                "title": "銀河漫画作品",
                "asset_ids": [asset_ids[0], asset_ids[1]]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created:?}");
    let stack_id = created["id"].as_str().expect("stack id").to_owned();
    assert_eq!(created["cover_asset_id"], asset_ids[0]);
    assert_eq!(
        created["chapters"][0]["pages"]
            .as_array()
            .expect("pages should be an array")
            .len(),
        2
    );
    assert_eq!(created["chapters"][0]["pages"][0]["page_no"], 1);

    let (status, stacks) = app.get("/api/stacks", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stacks[0]["id"], stack_id);
    assert_eq!(stacks[0]["chapter_count"], 1);
    assert_eq!(stacks[0]["page_count"], 2);

    let (status, patched) = app
        .json(
            Method::PATCH,
            &format!("/api/stacks/{stack_id}"),
            Some(&token),
            json!({"title": "銀河漫画完全版", "cover_asset_id": asset_ids[1]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["title"], "銀河漫画完全版");
    assert_eq!(patched["cover_asset_id"], asset_ids[1]);

    let (status, replaced) = app
        .json(
            Method::PUT,
            &format!("/api/stacks/{stack_id}/structure"),
            Some(&token),
            json!({
                "chapters": [
                    {"title": "前編", "pages": [asset_ids[1], asset_ids[2]]},
                    {"title": null, "pages": [asset_ids[0]]}
                ]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replaced:?}");
    assert_eq!(replaced["chapters"][0]["chapter_no"], 1);
    assert_eq!(replaced["chapters"][0]["pages"][0]["page_no"], 1);
    assert_eq!(replaced["chapters"][0]["pages"][1]["page_no"], 2);
    assert_eq!(replaced["chapters"][1]["chapter_no"], 2);
    assert_eq!(replaced["chapters"][1]["pages"][0]["page_no"], 1);

    let (status, flagged) = app
        .json(
            Method::PATCH,
            &format!("/api/stacks/{stack_id}/pages/{}", asset_ids[0]),
            Some(&token),
            json!({"show_in_timeline": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        flagged["chapters"][1]["pages"][0]["asset"]["id"],
        asset_ids[0]
    );
    assert_eq!(flagged["chapters"][1]["pages"][0]["show_in_timeline"], true);
    assert!(asset_visible(&app.database, &asset_ids[0]));

    let (status, _) = app
        .json(
            Method::DELETE,
            &format!("/api/assets/{}", asset_ids[1]),
            Some(&token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, with_trashed_page) = app
        .get(&format!("/api/stacks/{stack_id}"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        with_trashed_page["chapters"][0]["pages"][0]["asset"]["id"],
        asset_ids[1]
    );
    assert_eq!(
        with_trashed_page["chapters"][0]["pages"][0]["asset"]["status"],
        "trashed"
    );
    assert_eq!(
        with_trashed_page["chapters"][0]["pages"]
            .as_array()
            .expect("pages should be an array")
            .len(),
        2
    );

    let (status, trashed_flagged) = app
        .json(
            Method::PATCH,
            &format!("/api/stacks/{stack_id}/pages/{}", asset_ids[1]),
            Some(&token),
            json!({"show_in_timeline": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        trashed_flagged["chapters"][0]["pages"][0]["asset"]["status"],
        "trashed"
    );
    assert!(!asset_visible(&app.database, &asset_ids[1]));

    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/stacks/{stack_id}/pages/{}", asset_ids[2]))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("page removal should respond");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(asset_visible(&app.database, &asset_ids[2]));

    let (status, readded) = app
        .json(
            Method::POST,
            &format!("/api/stacks/{stack_id}/pages"),
            Some(&token),
            json!({"asset_ids": [asset_ids[2]]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        readded["chapters"][1]["pages"][1]["asset"]["id"],
        asset_ids[2]
    );
    assert_eq!(
        readded["chapters"][1]["pages"][1]["show_in_timeline"],
        false
    );
    assert!(!asset_visible(&app.database, &asset_ids[2]));

    for query in ["河漫画", "銀河"] {
        let (status, search) = app
            .get(
                &format!("/api/search?q={}", percent_encode(query)),
                Some(&token),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(search["stacks"][0]["id"], stack_id);
        assert_eq!(search["stacks"][0]["title"], "銀河漫画完全版");
    }

    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/stacks/{stack_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("stack delete should respond");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    for asset_id in asset_ids {
        let (status, _) = app
            .get(&format!("/api/assets/{asset_id}"), Some(&token))
            .await;
        assert_eq!(status, StatusCode::OK);
    }
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

fn asset_visible(database: &Database, id: &str) -> bool {
    database
        .with_connection(|connection| {
            connection
                .query_row(
                    "SELECT visible_in_timeline FROM assets WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        })
        .expect("visibility should be readable")
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
