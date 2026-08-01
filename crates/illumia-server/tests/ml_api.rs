use std::{fs, path::PathBuf};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use illumia_core::{assets::AssetService, db::Database, uuid::Uuid};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestApp {
    directory: PathBuf,
    database: Database,
    router: Router,
}

impl TestApp {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("illumia-ml-api-{}", Uuid::now_v7()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let database = Database::open(&directory).expect("database should open");
        Self {
            router: illumia_server::app(database.clone(), None),
            database,
            directory,
        }
    }

    async fn json(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.request(builder.body(Body::from(body.to_string())).expect("request"))
            .await
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.request(builder.body(Body::empty()).expect("request"))
            .await
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
            .expect("body should collect")
            .to_bytes();
        let value = serde_json::from_slice(&bytes).expect("response should be JSON");
        (status, value)
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn authenticated_cluster_merge_split_review_and_minimum_filter_flow() {
    let app = TestApp::new();
    let (status, setup) = app
        .json(
            Method::POST,
            "/api/auth/setup",
            None,
            json!({"password":"character password", "device_name":"ml api"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let token = setup["token"].as_str().expect("token").to_owned();

    let (status, ml_status) = app.get("/api/ml/status", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ml_status, json!({"enabled":true, "sidecar":null}));
    let (status, missing_socket) = app
        .json(
            Method::POST,
            "/api/ml/analyze-all",
            Some(&token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(missing_socket["error"]["code"], "bad_request");

    let (status, error) = app.get("/api/clusters", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["error"]["code"], "unauthorized");

    let assets = (0..10)
        .map(|index| {
            AssetService::new(app.database.clone())
                .ingest(&one_pixel_png(), &format!("character-{index}.png"), None)
                .expect("asset should ingest")
                .asset
        })
        .collect::<Vec<_>>();
    let faces = [
        ("from-a", 0, "from", "auto"),
        ("from-b", 1, "from", "auto"),
        ("from-c", 2, "from", "auto"),
        ("reject-me", 3, "from", "candidate"),
        ("into-a", 4, "into", "auto"),
        ("into-b", 5, "into", "auto"),
        ("into-c", 6, "into", "auto"),
        ("accept-me", 7, "into", "candidate"),
        ("small-a", 8, "small", "auto"),
        ("small-b", 9, "small", "auto"),
    ];
    app.database
        .with_connection(|connection| {
            for (id, name) in [("from", None), ("into", Some("Before")), ("small", None)] {
                connection.execute(
                    "INSERT INTO clusters(id, name, created_by, created_at)
                     VALUES (?1, ?2, 'user', '2026-01-01T00:00:00Z')",
                    (id, name),
                )?;
            }
            for (id, asset, cluster, state) in faces {
                connection.execute(
                    "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                       embedding, model_version, cluster_id, state, similarity)
                     VALUES (?1, ?2, 'face', '[0,0,1,1]', 0.9, '[]', ?3,
                             'test-v1', ?4, ?5, 0.7)",
                    (
                        id,
                        &assets[asset].id,
                        [1.0_f32.to_le_bytes(), 0.0_f32.to_le_bytes()].concat(),
                        cluster,
                        state,
                    ),
                )?;
            }
            Ok(())
        })
        .expect("ML fixture should insert");

    let (status, clusters) = app.get("/api/clusters", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let cluster_ids = clusters
        .as_array()
        .expect("clusters")
        .iter()
        .map(|cluster| cluster["id"].as_str().expect("cluster id"))
        .collect::<Vec<_>>();
    assert_eq!(cluster_ids.len(), 2);
    assert!(!cluster_ids.contains(&"small"));

    let (status, candidates) = app.get("/api/review/candidates", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(candidates.as_array().expect("candidates").len(), 2);
    assert!(candidates[0]["face"]["similarity"].is_number());
    assert!(candidates[0]["asset"]["filename"].is_string());

    let (status, rejected) = app
        .json(
            Method::POST,
            "/api/review/candidates/reject-me",
            Some(&token),
            json!({"action":"reject"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rejected["state"], "rejected");
    assert!(rejected["cluster_id"].is_null());
    assert!(rejection_exists(&app.database, "reject-me", "from"));

    let (status, accepted) = app
        .json(
            Method::POST,
            "/api/review/candidates/accept-me",
            Some(&token),
            json!({"action":"accept"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted["state"], "confirmed");

    let (status, merged) = app
        .json(
            Method::POST,
            "/api/clusters/merge",
            Some(&token),
            json!({"from_id":"from", "into_id":"into"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(merged["id"], "into");
    assert_eq!(merged["asset_count"], 7);

    let (status, renamed) = app
        .json(
            Method::PATCH,
            "/api/clusters/into",
            Some(&token),
            json!({"name":"Hero Team"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "Hero Team");
    let (status, search) = app.get("/api/search?q=Hero", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search["clusters"][0]["id"], "into");

    let (status, split) = app
        .json(
            Method::POST,
            "/api/clusters/into/split",
            Some(&token),
            json!({"face_ids":["accept-me", "into-a"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(split["id"], "into");
    assert_eq!(split["asset_count"], 2);

    let (status, assets_response) = app.get("/api/clusters/into/assets", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(assets_response.as_array().expect("assets").len(), 5);
}

fn rejection_exists(database: &Database, face_id: &str, cluster_id: &str) -> bool {
    database
        .with_connection(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_rejections
                 WHERE face_id = ?1 AND cluster_id = ?2)",
                (face_id, cluster_id),
                |row| row.get::<_, bool>(0),
            )?)
        })
        .expect("rejection should query")
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
