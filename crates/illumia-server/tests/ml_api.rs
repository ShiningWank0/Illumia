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

    async fn setup(&self) -> String {
        let (status, setup) = self
            .json(
                Method::POST,
                "/api/auth/setup",
                None,
                json!({"password":"character password", "device_name":"ml api"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        setup["token"].as_str().expect("token").to_owned()
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
    let token = app.setup().await;

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

#[tokio::test]
async fn cluster_responses_include_cover_and_only_member_faces() {
    let app = TestApp::new();
    let token = app.setup().await;
    let assets = (0..3)
        .map(|index| {
            AssetService::new(app.database.clone())
                .ingest(&one_pixel_png(), &format!("cover-{index}.png"), None)
                .expect("asset should ingest")
                .asset
        })
        .collect::<Vec<_>>();

    app.database
        .with_connection(|connection| {
            for id in ["target", "other"] {
                connection.execute(
                    "INSERT INTO clusters(id, name, cover_face_id, created_by, created_at)
                     VALUES (?1, NULL, NULL, 'user', '2026-01-01T00:00:00Z')",
                    [id],
                )?;
            }
            for (id, asset_index, cluster_id, state, similarity) in [
                ("target-a", 0, "target", "auto", Some(0.91)),
                ("target-a-2", 0, "target", "confirmed", None),
                ("foreign-a", 0, "other", "auto", Some(0.88)),
                ("target-b", 1, "target", "auto", Some(0.82)),
                ("target-c", 2, "target", "auto", Some(0.73)),
            ] {
                connection.execute(
                    "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                       embedding, model_version, cluster_id, state, similarity)
                     VALUES (?1, ?2, 'face', '[0.1,0.2,0.3,0.4]', 0.9, '[]', ?3,
                             'test-v1', ?4, ?5, ?6)",
                    (
                        id,
                        &assets[asset_index].id,
                        [1.0_f32.to_le_bytes(), 0.0_f32.to_le_bytes()].concat(),
                        cluster_id,
                        state,
                        similarity,
                    ),
                )?;
            }
            connection.execute(
                "UPDATE clusters SET cover_face_id = 'target-a' WHERE id = 'target'",
                [],
            )?;
            Ok(())
        })
        .expect("cluster fixture should insert");

    let (status, clusters) = app.get("/api/clusters", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let target = clusters
        .as_array()
        .expect("clusters")
        .iter()
        .find(|cluster| cluster["id"] == "target")
        .expect("target cluster");
    assert_eq!(
        target["cover"],
        json!({
            "face_id": "target-a",
            "asset_id": assets[0].id,
            "bbox": [0.1, 0.2, 0.3, 0.4]
        })
    );
    assert!(target.get("cover_face_id").is_none());

    let (status, rows) = app
        .get("/api/clusters/target/assets", Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.as_array().expect("cluster assets").len(), 3);
    let first = rows
        .as_array()
        .expect("cluster assets")
        .iter()
        .find(|row| row["id"] == assets[0].id)
        .expect("first asset");
    assert!(first["filename"].is_string());
    assert_eq!(
        first["faces"],
        json!([
            {
                "face_id": "target-a",
                "bbox": [0.1, 0.2, 0.3, 0.4],
                "state": "auto",
                "similarity": 0.91
            },
            {
                "face_id": "target-a-2",
                "bbox": [0.1, 0.2, 0.3, 0.4],
                "state": "confirmed",
                "similarity": null
            }
        ])
    );
}

#[tokio::test]
async fn cluster_cover_is_null_when_face_is_missing_or_asset_is_trashed() {
    let app = TestApp::new();
    let token = app.setup().await;
    let assets = (0..3)
        .map(|index| {
            AssetService::new(app.database.clone())
                .ingest(&one_pixel_png(), &format!("missing-cover-{index}.png"), None)
                .expect("asset should ingest")
                .asset
        })
        .collect::<Vec<_>>();

    app.database
        .with_connection(|connection| {
            connection.execute(
                "INSERT INTO clusters(id, name, cover_face_id, created_by, created_at)
                 VALUES ('target', NULL, 'missing-face', 'user', '2026-01-01T00:00:00Z')",
                [],
            )?;
            for (index, asset) in assets.iter().enumerate() {
                connection.execute(
                    "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                       embedding, model_version, cluster_id, state, similarity)
                     VALUES (?1, ?2, 'face', '[0,0,1,1]', 0.9, '[]', ?3,
                             'test-v1', 'target', 'auto', 0.7)",
                    (
                        format!("target-{index}"),
                        &asset.id,
                        [1.0_f32.to_le_bytes(), 0.0_f32.to_le_bytes()].concat(),
                    ),
                )?;
            }
            Ok(())
        })
        .expect("cluster fixture should insert");

    let (status, clusters) = app.get("/api/clusters", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(clusters[0]["cover"].is_null());

    app.database
        .with_connection(|connection| {
            connection.execute(
                "UPDATE clusters SET cover_face_id = 'target-0' WHERE id = 'target'",
                [],
            )?;
            Ok(())
        })
        .expect("cover should update");
    AssetService::new(app.database.clone())
        .trash(&assets[0].id)
        .expect("cover asset should trash");

    let (status, clusters) = app.get("/api/clusters", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(clusters[0]["cover"].is_null());
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
