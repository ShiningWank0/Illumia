use std::{
    io::{Cursor, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use illumia_core::{
    assets::AssetService,
    db::Database,
    ml::{MlService, enqueue_analyze},
    ml_client::{Error as ClientError, MlClient},
};
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde_json::{Value, json};
use tempfile::TempDir;

struct MockSidecar {
    socket: PathBuf,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockSidecar {
    fn new(
        directory: &Path,
        requests: usize,
        handler: impl Fn(usize, &str, &Value) -> Value + Send + 'static,
    ) -> std::io::Result<Self> {
        let socket = directory.join("ml.sock");
        let listener = UnixListener::bind(&socket)?;
        let thread = thread::spawn(move || {
            for index in 0..requests {
                let (mut stream, _) = listener.accept().expect("mock should accept");
                let (path, content_type, body) = read_request(&mut stream);
                let request = if content_type.as_deref() == Some("application/json") {
                    serde_json::from_slice(&body).expect("cluster request should be JSON")
                } else {
                    Value::Null
                };
                let response = handler(index, &path, &request).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .expect("mock response should write");
            }
        });
        Ok(Self {
            socket,
            thread: Some(thread),
        })
    }

    fn client(&self) -> MlClient {
        MlClient::new(&self.socket)
    }
}

impl Drop for MockSidecar {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            thread.join().expect("mock thread should finish");
        }
    }
}

fn read_request(stream: &mut UnixStream) -> (String, Option<String>, Vec<u8>) {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).expect("request should read");
        assert_ne!(count, 0, "request headers should be complete");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let header = std::str::from_utf8(&bytes[..header_end]).expect("headers should be UTF-8");
    let path = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("request path")
        .to_owned();
    let content_length = header
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("content length"))
            })
        })
        .expect("content length should exist");
    let content_type = header.lines().find_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            name.eq_ignore_ascii_case("content-type")
                .then(|| value.trim().to_owned())
        })
    });
    let mut body = bytes.split_off(header_end);
    let already = body.len();
    body.resize(content_length, 0);
    if already < content_length {
        stream
            .read_exact(&mut body[already..])
            .expect("request body should read");
    }
    (path, content_type, body)
}

fn analysis() -> Value {
    json!({
        "model_version": "test-v1",
        "instances": [{
            "kind": "face",
            "bbox": [0.1, 0.2, 0.3, 0.4],
            "det_conf": 0.95,
            "quality": {"passed": true, "flags": []},
            "embedding": {"dtype": "f32", "dim": 2, "b64": "AACAPwAAAAA="},
            "tags": []
        }]
    })
}

fn database() -> (TempDir, Database) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database = Database::open(directory.path()).expect("database should open");
    (directory, database)
}

fn mock_or_skip(result: std::io::Result<MockSidecar>) -> Option<MockSidecar> {
    match result {
        Ok(mock) => Some(mock),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => None,
        Err(error) => panic!("mock socket should bind: {error}"),
    }
}

fn png() -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::new(2, 2))
        .write_to(&mut output, ImageFormat::Png)
        .expect("PNG should encode");
    output.into_inner()
}

#[test]
fn analyze_jobs_persist_faces_and_assign_all_three_threshold_states() {
    let (directory, database) = database();
    let seed = AssetService::new(database.clone())
        .ingest(&png(), "seed.png", None)
        .expect("seed should ingest")
        .asset;
    database
        .with_connection(|connection| {
            connection.execute(
                "INSERT INTO clusters(id, name, created_by, created_at)
                 VALUES ('known', NULL, 'user', '2026-01-01T00:00:00Z')",
                [],
            )?;
            connection.execute(
                "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                    embedding, model_version, cluster_id, state)
                 VALUES ('medoid', ?1, 'face', '[0,0,1,1]', 1, '[]', ?2,
                         'test-v1', 'known', 'confirmed')",
                rusqlite::params![
                    seed.id,
                    [1.0_f32.to_le_bytes(), 0.0_f32.to_le_bytes()].concat()
                ],
            )?;
            Ok(())
        })
        .expect("seed cluster should insert");
    let assets = (0..3)
        .map(|index| {
            AssetService::new(database.clone())
                .ingest(&png_with_marker(index), &format!("asset-{index}.png"), None)
                .expect("asset should ingest")
                .asset
        })
        .collect::<Vec<_>>();
    let Some(mock) = mock_or_skip(MockSidecar::new(
        directory.path(),
        10,
        |index, path, request| {
            if path == "/ml/v1/health" {
                json!({
                    "status":"ok", "backend":"onnx",
                    "model_bundle":{"name":"test", "version":"test-v1", "sha256":"00"},
                    "providers":["CPUExecutionProvider"]
                })
            } else if path.starts_with("/ml/v1/analyze") {
                analysis()
            } else {
                assert_eq!(path, "/ml/v1/cluster");
                assert_eq!(request["mode"], "assign");
                assert!(request["embeddings"].is_array());
                let id = request["ids"][0].as_str().expect("face id");
                let assignment_index = index / 3;
                let (cluster, state, similarity) = match assignment_index {
                    0 => (json!("known"), "auto", 0.9),
                    1 => (json!("known"), "candidate", 0.7),
                    _ => (Value::Null, "unassigned", 0.2),
                };
                json!({
                    "assignments": [{
                        "id": id, "cluster": cluster, "state": state, "similarity": similarity
                    }],
                    "new_clusters": []
                })
            }
        },
    )) else {
        return;
    };
    let service = MlService::new(database.clone(), mock.client());
    for asset in &assets {
        let queued = enqueue_analyze(&database, &asset.id).expect("job should enqueue");
        service
            .handle_analyze_job(&queued)
            .expect("analyze job should run");
    }
    let duplicate =
        enqueue_analyze(&database, &assets[0].id).expect("duplicate job should enqueue");
    service
        .handle_analyze_job(&duplicate)
        .expect("same model analysis should skip after health");
    let states = database
        .with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT state FROM faces WHERE id != 'medoid' ORDER BY rowid")?;
            Ok(statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .expect("states should query");
    assert_eq!(states, ["auto", "candidate", "unassigned"]);
}

#[test]
fn full_recluster_cannot_move_confirmed_or_rejected_and_refuses_rejection_pairs() {
    let (directory, database) = database();
    let assets = (0..3)
        .map(|index| {
            AssetService::new(database.clone())
                .ingest(&png_with_marker(index), &format!("face-{index}.png"), None)
                .expect("asset should ingest")
                .asset
        })
        .collect::<Vec<_>>();
    database
        .with_connection(|connection| {
            for id in ["old", "blocked"] {
                connection.execute(
                    "INSERT INTO clusters(id, name, created_by, created_at)
                     VALUES (?1, NULL, 'user', '2026-01-01T00:00:00Z')",
                    [id],
                )?;
            }
            for (index, (id, state, cluster)) in [
                ("confirmed", "confirmed", Some("old")),
                ("rejected", "rejected", None),
                ("guarded", "auto", Some("old")),
            ]
            .into_iter()
            .enumerate()
            {
                connection.execute(
                    "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                       embedding, model_version, cluster_id, state)
                     VALUES (?1, ?2, 'face', '[0,0,1,1]', 1, '[]', ?3,
                             'test-v1', ?4, ?5)",
                    rusqlite::params![
                        id,
                        assets[index].id,
                        [1.0_f32.to_le_bytes(), 0.0_f32.to_le_bytes()].concat(),
                        cluster,
                        state
                    ],
                )?;
            }
            connection.execute(
                "INSERT INTO cluster_rejections(face_id, cluster_id)
                 VALUES ('guarded', 'blocked')",
                [],
            )?;
            Ok(())
        })
        .expect("fixture should insert");
    let Some(mock) = mock_or_skip(MockSidecar::new(
        directory.path(),
        1,
        |_index, path, request| {
            assert_eq!(path, "/ml/v1/cluster");
            assert_eq!(request["mode"], "full");
            assert!(
                request["confirmed"]
                    .as_array()
                    .expect("confirmed array")
                    .iter()
                    .any(|id| id == "confirmed")
            );
            json!({
                "assignments": [
                    {"id":"confirmed", "cluster":"blocked", "state":"auto", "similarity":1.0},
                    {"id":"rejected", "cluster":"blocked", "state":"auto", "similarity":1.0},
                    {"id":"guarded", "cluster":"blocked", "state":"auto", "similarity":1.0}
                ],
                "new_clusters": []
            })
        },
    )) else {
        return;
    };
    MlService::new(database.clone(), mock.client())
        .recluster()
        .expect("recluster should succeed");
    let rows = database
        .with_connection(|connection| {
            let mut statement =
                connection.prepare("SELECT id, state, cluster_id FROM faces ORDER BY id")?;
            Ok(statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .expect("faces should query");
    assert_eq!(
        rows[0],
        ("confirmed".into(), "confirmed".into(), Some("old".into()))
    );
    assert_eq!(rows[1], ("guarded".into(), "unassigned".into(), None));
    assert_eq!(rows[2], ("rejected".into(), "rejected".into(), None));
}

#[test]
fn client_reports_unavailable_and_timeout_separately() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = MlClient::with_timeout(
        directory.path().join("missing.sock"),
        Duration::from_millis(50),
    );
    assert!(matches!(missing.health(), Err(ClientError::Unavailable)));

    let socket = directory.path().join("slow.sock");
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("slow socket should bind: {error}"),
    };
    let thread = thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("slow mock should accept");
        thread::sleep(Duration::from_millis(150));
    });
    let slow = MlClient::with_timeout(socket, Duration::from_millis(25));
    assert!(matches!(slow.health(), Err(ClientError::Timeout)));
    thread.join().expect("slow mock should finish");
}

fn png_with_marker(marker: u8) -> Vec<u8> {
    let mut image = RgbaImage::new(2, 2);
    image.get_pixel_mut(0, 0).0 = [marker, 0, 0, 255];
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .expect("PNG should encode");
    output.into_inner()
}
