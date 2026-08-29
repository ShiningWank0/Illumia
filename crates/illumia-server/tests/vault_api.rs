use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use illumia_core::{
    assets::AssetService,
    db::Database,
    jobs::JobQueue,
    ml::ML_VAULT_ANALYZE_JOB_KIND,
    settings::Settings,
    uuid::Uuid,
    vault::{KdfParams, VaultHandle, init_with_kdf},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("illumia-vault-api-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("test data directory should be created");
        Self { path }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl TestResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).expect("response should be JSON")
    }
}

struct TestApp {
    _directory: TestDirectory,
    database: Database,
    router: Router,
}

impl TestApp {
    fn new(ttl: Duration) -> Self {
        let directory = TestDirectory::new();
        let database = Database::open(&directory.path).expect("test database should open");
        let router = illumia_server::app_with_vault_ttl(database.clone(), None, ttl);
        Self {
            _directory: directory,
            database,
            router,
        }
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        auth_token: Option<&str>,
        vault_session: Option<&str>,
        body: Option<Value>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = auth_token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(session) = vault_session {
            builder = builder.header("X-Vault-Session", session);
        }
        let body = if let Some(value) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        } else {
            Body::empty()
        };
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).expect("request should build"))
            .await
            .expect("router should respond");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes()
            .to_vec();
        TestResponse {
            status,
            headers,
            bytes,
        }
    }

    async fn setup(&self) -> String {
        let response = self
            .request(
                Method::POST,
                "/api/auth/setup",
                None,
                None,
                Some(json!({
                    "password": "server password",
                    "device_name": "vault integration"
                })),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK);
        response.json()["token"]
            .as_str()
            .expect("setup token")
            .to_owned()
    }

    fn root(&self) -> &Path {
        self.database.data_root()
    }
}

#[tokio::test]
async fn vault_api_full_transfer_and_visibility_flow() {
    let app = TestApp::new(Duration::from_secs(15 * 60));
    let auth = app.setup().await;
    let vault_password = format!("test-vault-password-{}", Uuid::now_v7());

    let status = app
        .request(Method::GET, "/api/vault/status", Some(&auth), None, None)
        .await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(
        status.json(),
        json!({"initialized": false, "unlocked": false})
    );

    let hidden_before_init = app
        .request(
            Method::GET,
            "/api/vault/timeline/buckets?granularity=day",
            Some(&auth),
            Some(&"0".repeat(64)),
            None,
        )
        .await;
    assert_eq!(hidden_before_init.status, StatusCode::NOT_FOUND);

    let initialized = app
        .request(
            Method::POST,
            "/api/vault/init",
            Some(&auth),
            None,
            Some(json!({"password": vault_password.as_str()})),
        )
        .await;
    assert_eq!(initialized.status, StatusCode::CREATED);
    let recovery_key = initialized.json()["recovery_key"]
        .as_str()
        .expect("recovery key")
        .to_owned();

    let initialized_again = app
        .request(
            Method::POST,
            "/api/vault/init",
            Some(&auth),
            None,
            Some(json!({"password": "another password"})),
        )
        .await;
    assert_eq!(initialized_again.status, StatusCode::CONFLICT);

    let wrong_password = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"password": "wrong"})),
        )
        .await;
    assert_eq!(wrong_password.status, StatusCode::UNAUTHORIZED);

    let unlocked = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"password": vault_password.as_str()})),
        )
        .await;
    assert_eq!(unlocked.status, StatusCode::OK);
    let unlocked_json = unlocked.json();
    let session = unlocked_json["vault_session"]
        .as_str()
        .expect("vault session")
        .to_owned();
    assert_eq!(session.len(), 64);
    assert!(unlocked_json["expires_at"].is_string());

    let without_session = app
        .request(
            Method::GET,
            "/api/vault/timeline/buckets?granularity=day",
            Some(&auth),
            None,
            None,
        )
        .await;
    assert_eq!(without_session.status, StatusCode::NOT_FOUND);

    let original_bytes = one_pixel_png();
    let asset = AssetService::new(app.database.clone())
        .ingest(&original_bytes, "秘密画像.png", None)
        .expect("main asset should ingest")
        .asset;
    let original_path = app.root().join(&asset.library_path);
    assert!(original_path.is_file());

    let imported = app
        .request(
            Method::POST,
            "/api/vault/import",
            Some(&auth),
            Some(&session),
            Some(json!({"asset_ids": [asset.id]})),
        )
        .await;
    assert_eq!(imported.status, StatusCode::NO_CONTENT);
    assert!(
        AssetService::new(app.database.clone())
            .get(&asset.id)
            .expect("main database should read")
            .is_none()
    );
    assert!(!original_path.exists());
    assert_plaintext_trace_absent(&app.database, &asset.id, "秘密画像");

    let missing_ml_socket = app.root().join("missing-ml.sock");
    Settings::new(app.database.clone())
        .set_ml_socket_path(missing_ml_socket.to_str().expect("UTF-8 test path"))
        .expect("test ML socket setting should persist");
    let queued_analysis = app
        .request(
            Method::POST,
            "/api/vault/ml/analyze-all",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(queued_analysis.status, StatusCode::ACCEPTED);
    assert_eq!(queued_analysis.json()["enqueued"], 1);
    let queue_view = VaultHandle::unlock(app.root(), &vault_password)
        .expect("vault queue should open for test inspection");
    assert!(
        JobQueue::new(queue_view.db)
            .list()
            .expect("vault jobs should list")
            .iter()
            .any(|job| job.kind == ML_VAULT_ANALYZE_JOB_KIND),
        "Vault analysis must persist in the Vault DB job queue"
    );

    let buckets = app
        .request(
            Method::GET,
            "/api/vault/timeline/buckets?granularity=day",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(buckets.status, StatusCode::OK);
    let buckets_json = buckets.json();
    assert_eq!(buckets_json.as_array().expect("bucket array").len(), 1);
    let bucket_key = buckets_json[0]["key"].as_str().expect("bucket key");

    let bucket = app
        .request(
            Method::GET,
            &format!("/api/vault/timeline/buckets/{bucket_key}?granularity=day"),
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(bucket.status, StatusCode::OK);
    assert_eq!(bucket.json()[0]["id"], asset.id);

    let search = app
        .request(
            Method::GET,
            "/api/vault/search?q=%E7%A7%98%E5%AF%86",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(search.status, StatusCode::OK);
    assert_eq!(search.json()["assets"][0]["id"], asset.id);

    for variant in ["thumbnail", "preview"] {
        let response = app
            .request(
                Method::GET,
                &format!("/api/vault/assets/{}/{variant}", asset.id),
                Some(&auth),
                Some(&session),
                None,
            )
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.headers[header::CONTENT_TYPE], "image/webp");
        assert_eq!(response.headers[header::CACHE_CONTROL], "no-store");
        assert!(!response.bytes.is_empty());
    }

    let original = app
        .request(
            Method::GET,
            &format!("/api/vault/assets/{}/original", asset.id),
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(original.status, StatusCode::OK);
    assert_eq!(original.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(original.bytes, original_bytes);

    let locked = app
        .request(
            Method::POST,
            "/api/vault/lock",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(locked.status, StatusCode::NO_CONTENT);
    let hidden_after_lock = app
        .request(
            Method::GET,
            "/api/vault/search?q=secret",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(hidden_after_lock.status, StatusCode::NOT_FOUND);

    let recovered = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"recovery_key": recovery_key})),
        )
        .await;
    assert_eq!(recovered.status, StatusCode::OK);
    let recovered_session = recovered.json()["vault_session"]
        .as_str()
        .expect("recovery session")
        .to_owned();
    assert_ne!(recovered_session, session);

    let exported = app
        .request(
            Method::POST,
            "/api/vault/export",
            Some(&auth),
            Some(&recovered_session),
            Some(json!({"asset_ids": [asset.id]})),
        )
        .await;
    assert_eq!(exported.status, StatusCode::NO_CONTENT);
    let restored = AssetService::new(app.database.clone())
        .get(&asset.id)
        .expect("main database should read")
        .expect("exported asset should return to main");
    assert_eq!(
        fs::read(app.root().join(restored.library_path)).expect("exported original should read"),
        original_bytes
    );
    let vault_search = app
        .request(
            Method::GET,
            "/api/vault/search?q=%E7%A7%98%E5%AF%86",
            Some(&auth),
            Some(&recovered_session),
            None,
        )
        .await;
    assert_eq!(vault_search.status, StatusCode::OK);
    assert_eq!(vault_search.json()["assets"], json!([]));
}

#[tokio::test]
async fn vault_cluster_endpoints_mirror_cover_and_faces_dtos() {
    let app = TestApp::new(Duration::from_secs(15 * 60));
    let auth = app.setup().await;
    let initialized = app
        .request(
            Method::POST,
            "/api/vault/init",
            Some(&auth),
            None,
            Some(json!({"password": "vault cluster password"})),
        )
        .await;
    assert_eq!(initialized.status, StatusCode::CREATED);
    let unlocked = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"password": "vault cluster password"})),
        )
        .await;
    assert_eq!(unlocked.status, StatusCode::OK);
    let session = unlocked.json()["vault_session"]
        .as_str()
        .expect("vault session")
        .to_owned();

    let assets = (0..3)
        .map(|index| {
            AssetService::new(app.database.clone())
                .ingest(
                    &one_pixel_png(),
                    &format!("vault-cluster-{index}.png"),
                    None,
                )
                .expect("main asset should ingest")
                .asset
        })
        .collect::<Vec<_>>();
    let imported = app
        .request(
            Method::POST,
            "/api/vault/import",
            Some(&auth),
            Some(&session),
            Some(json!({
                "asset_ids": assets.iter().map(|asset| &asset.id).collect::<Vec<_>>()
            })),
        )
        .await;
    assert_eq!(imported.status, StatusCode::NO_CONTENT);

    let vault = VaultHandle::unlock(app.root(), "vault cluster password")
        .expect("vault should open for fixture setup");
    vault
        .db
        .with_connection(|connection| {
            for id in ["target", "other"] {
                connection.execute(
                    "INSERT INTO clusters(id, name, cover_face_id, created_by, created_at)
                     VALUES (?1, NULL, NULL, 'user', '2026-01-01T00:00:00Z')",
                    [id],
                )?;
            }
            for (id, asset_index, cluster_id) in [
                ("target-a", 0, "target"),
                ("foreign-a", 0, "other"),
                ("target-b", 1, "target"),
                ("target-c", 2, "target"),
            ] {
                connection.execute(
                    "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                       embedding, model_version, cluster_id, state, similarity)
                     VALUES (?1, ?2, 'face', '[0.2,0.1,0.4,0.5]', 0.9, '[]', ?3,
                             'test-v1', ?4, 'auto', 0.75)",
                    (
                        id,
                        &assets[asset_index].id,
                        [1.0_f32.to_le_bytes(), 0.0_f32.to_le_bytes()].concat(),
                        cluster_id,
                    ),
                )?;
            }
            connection.execute(
                "UPDATE clusters SET cover_face_id = 'target-a' WHERE id = 'target'",
                [],
            )?;
            Ok(())
        })
        .expect("vault cluster fixture should insert");

    let clusters = app
        .request(
            Method::GET,
            "/api/vault/clusters",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(clusters.status, StatusCode::OK);
    assert_eq!(
        clusters.json()[0]["cover"],
        json!({
            "face_id": "target-a",
            "asset_id": assets[0].id,
            "bbox": [0.2, 0.1, 0.4, 0.5]
        })
    );

    let rows = app
        .request(
            Method::GET,
            "/api/vault/clusters/target/assets",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(rows.status, StatusCode::OK);
    let body = rows.json();
    assert_eq!(body.as_array().expect("cluster assets").len(), 3);
    let first = body
        .as_array()
        .expect("cluster assets")
        .iter()
        .find(|row| row["id"] == assets[0].id)
        .expect("first asset");
    assert_eq!(first["faces"].as_array().expect("member faces").len(), 1);
    assert_eq!(first["faces"][0]["face_id"], "target-a");
}

#[tokio::test]
async fn vault_session_expires_at_injected_ttl() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.path).expect("test database should open");
    init_with_kdf(&directory.path, "vault password", KdfParams::for_tests())
        .expect("test vault should initialize");
    let app = TestApp {
        router: illumia_server::app_with_vault_ttl(
            database.clone(),
            None,
            Duration::from_millis(200),
        ),
        database,
        _directory: directory,
    };
    let auth = app.setup().await;
    let unlocked = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"password": "vault password"})),
        )
        .await;
    assert_eq!(unlocked.status, StatusCode::OK);
    let session = unlocked.json()["vault_session"]
        .as_str()
        .expect("vault session")
        .to_owned();

    let replacement = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"password": "vault password"})),
        )
        .await;
    assert_eq!(replacement.status, StatusCode::OK);
    let replacement_session = replacement.json()["vault_session"]
        .as_str()
        .expect("replacement vault session")
        .to_owned();
    assert_ne!(replacement_session, session);
    let replaced = app
        .request(
            Method::GET,
            "/api/vault/trash",
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(replaced.status, StatusCode::NOT_FOUND);

    tokio::time::sleep(Duration::from_millis(120)).await;
    let active = app
        .request(
            Method::GET,
            "/api/vault/trash",
            Some(&auth),
            Some(&replacement_session),
            None,
        )
        .await;
    assert_eq!(active.status, StatusCode::OK);

    // This request is later than the original deadline but within the extended TTL.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let extended = app
        .request(
            Method::GET,
            "/api/vault/trash",
            Some(&auth),
            Some(&replacement_session),
            None,
        )
        .await;
    assert_eq!(extended.status, StatusCode::OK);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let expired = app
        .request(
            Method::GET,
            "/api/vault/trash",
            Some(&auth),
            Some(&replacement_session),
            None,
        )
        .await;
    assert_eq!(expired.status, StatusCode::NOT_FOUND);
    let status = app
        .request(Method::GET, "/api/vault/status", Some(&auth), None, None)
        .await;
    assert_eq!(
        status.json(),
        json!({"initialized": true, "unlocked": false})
    );
}

#[tokio::test]
async fn vault_manual_purge_deletes_only_the_selected_assets_row_and_blobs() {
    let directory = TestDirectory::new();
    let database = Database::open(&directory.path).expect("test database should open");
    init_with_kdf(
        &directory.path,
        "vault purge password",
        KdfParams::for_tests(),
    )
    .expect("test vault should initialize");
    let app = TestApp {
        router: illumia_server::app_with_vault_ttl(
            database.clone(),
            None,
            Duration::from_secs(15 * 60),
        ),
        database,
        _directory: directory,
    };
    let auth = app.setup().await;
    let unlocked = app
        .request(
            Method::POST,
            "/api/vault/unlock",
            Some(&auth),
            None,
            Some(json!({"password": "vault purge password"})),
        )
        .await;
    assert_eq!(unlocked.status, StatusCode::OK);
    let session = unlocked.json()["vault_session"]
        .as_str()
        .expect("vault session")
        .to_owned();

    let bytes = one_pixel_png();
    let survivor = AssetService::new(app.database.clone())
        .ingest(&bytes, "survivor.png", None)
        .expect("survivor asset should ingest")
        .asset;
    let target = AssetService::new(app.database.clone())
        .ingest(&bytes, "target.png", None)
        .expect("target asset should ingest")
        .asset;
    let imported = app
        .request(
            Method::POST,
            "/api/vault/import",
            Some(&auth),
            Some(&session),
            Some(json!({"asset_ids": [survivor.id, target.id]})),
        )
        .await;
    assert_eq!(imported.status, StatusCode::NO_CONTENT);

    let vault = VaultHandle::unlock(app.root(), "vault purge password")
        .expect("vault should open for assertions");
    let target_blob_ids = asset_blob_ids(&vault.db, &target.id);
    let survivor_blob_ids = asset_blob_ids(&vault.db, &survivor.id);
    assert_eq!(target_blob_ids.len(), 3);
    assert_eq!(survivor_blob_ids.len(), 3);
    for blob_id in target_blob_ids.iter().chain(&survivor_blob_ids) {
        assert!(vault_blob_path(app.root(), blob_id).is_file());
    }

    let trashed = app
        .request(
            Method::DELETE,
            &format!("/api/vault/assets/{}", target.id),
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(trashed.status, StatusCode::OK);
    let purged = app
        .request(
            Method::DELETE,
            &format!("/api/vault/trash/{}", target.id),
            Some(&auth),
            Some(&session),
            None,
        )
        .await;
    assert_eq!(purged.status, StatusCode::NO_CONTENT);

    assert_eq!(asset_row_count(&vault.db, &target.id), 0);
    assert!(asset_blob_ids(&vault.db, &target.id).is_empty());
    for blob_id in &target_blob_ids {
        assert!(!vault_blob_path(app.root(), blob_id).exists());
    }
    assert_eq!(asset_row_count(&vault.db, &survivor.id), 1);
    assert_eq!(asset_blob_ids(&vault.db, &survivor.id), survivor_blob_ids);
    for blob_id in &survivor_blob_ids {
        assert!(vault_blob_path(app.root(), blob_id).is_file());
    }
}

fn asset_blob_ids(database: &Database, asset_id: &str) -> Vec<String> {
    database
        .with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT blob_id FROM vault_blobs WHERE asset_id = ?1 ORDER BY kind")?;
            let ids = statement
                .query_map([asset_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ids)
        })
        .expect("vault blob rows should be readable")
}

fn asset_row_count(database: &Database, asset_id: &str) -> i64 {
    database
        .with_connection(|connection| {
            connection
                .query_row(
                    "SELECT count(*) FROM assets WHERE id = ?1",
                    [asset_id],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        })
        .expect("vault asset row should be countable")
}

fn vault_blob_path(data_root: &Path, blob_id: &str) -> PathBuf {
    data_root.join("vault").join("blobs").join(blob_id)
}

fn assert_plaintext_trace_absent(database: &Database, asset_id: &str, search_term: &str) {
    database
        .with_connection(|connection| {
            for (table, column) in [
                ("assets", "id"),
                ("faces", "asset_id"),
                ("stack_pages", "asset_id"),
                ("search_fts", "entity_id"),
            ] {
                let sql = format!("SELECT count(*) FROM {table} WHERE {column} = ?1");
                assert_eq!(
                    connection.query_row(&sql, [asset_id], |row| row.get::<_, i64>(0))?,
                    0
                );
            }
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH ?1",
                    [search_term],
                    |row| row.get::<_, i64>(0),
                )?,
                0
            );
            Ok(())
        })
        .expect("plaintext database should be inspectable");
    let wal = database.data_root().join("illumia.db-wal");
    assert!(!wal.exists() || fs::metadata(wal).expect("WAL metadata").len() == 0);
    let database_bytes =
        fs::read(database.data_root().join("illumia.db")).expect("main database bytes");
    assert!(!contains_bytes(&database_bytes, asset_id.as_bytes()));
    assert!(!contains_bytes(&database_bytes, search_term.as_bytes()));
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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
