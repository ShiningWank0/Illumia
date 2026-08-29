use std::{
    io::{Cursor, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use illumia_core::{
    assets::AssetService,
    db::{Database, Error as DatabaseError},
    jobs::JobQueue,
    ml::{MlService, enqueue_analyze, enqueue_recluster},
    ml_client::{ClusterMode, ClusterParams, ClusterRequest, Error as ClientError, MlClient},
};
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde_json::{Value, json};
use tempfile::TempDir;

struct MockSidecar {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
    expected: usize,
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
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicUsize::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_served = Arc::clone(&served);
        let thread = thread::spawn(move || {
            while worker_served.load(Ordering::Acquire) < requests {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        let index = worker_served.load(Ordering::Acquire);
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
                        worker_served.fetch_add(1, Ordering::Release);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("mock should accept: {error}"),
                }
            }
        });
        Ok(Self {
            socket,
            stop,
            served,
            expected: requests,
            thread: Some(thread),
        })
    }

    fn client(&self) -> MlClient {
        MlClient::new(&self.socket)
    }
}

impl Drop for MockSidecar {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("mock thread should finish");
        }
        if !thread::panicking() {
            assert_eq!(
                self.served.load(Ordering::Acquire),
                self.expected,
                "mock received an unexpected number of requests"
            );
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
fn parallel_ml_admission_is_idempotent_per_operation() {
    let (_directory, database) = database();
    let asset = AssetService::new(database.clone())
        .ingest(&png(), "dedup.png", None)
        .expect("asset should ingest")
        .asset;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(17));
    let mut workers = Vec::new();
    for _ in 0..16 {
        let worker_database = database.clone();
        let worker_asset = asset.id.clone();
        let worker_barrier = std::sync::Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            worker_barrier.wait();
            let analyze = enqueue_analyze(&worker_database, &worker_asset)
                .expect("analyze admission should succeed");
            let recluster =
                enqueue_recluster(&worker_database).expect("recluster admission should succeed");
            (analyze.id, recluster.id)
        }));
    }
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("admission worker should finish"))
        .collect::<Vec<_>>();
    assert!(results.iter().all(|result| result.0 == results[0].0));
    assert!(results.iter().all(|result| result.1 == results[0].1));
    let active = JobQueue::new(database)
        .list()
        .expect("jobs should list")
        .into_iter()
        .filter(|job| {
            matches!(
                job.state,
                illumia_core::jobs::JobState::Queued | illumia_core::jobs::JobState::Running
            )
        })
        .count();
    assert_eq!(active, 2);
}

#[test]
fn clustering_request_rejects_rows_over_the_cpu_budget_before_connecting() {
    let rows = illumia_core::ml::MAX_CLUSTER_ROWS + 1;
    let request = ClusterRequest {
        mode: ClusterMode::Full,
        params: ClusterParams {
            tau_high: None,
            tau_low: None,
            min_cluster_size: None,
        },
        embeddings: vec![vec![1.0]; rows],
        shape: [rows, 1],
        ids: (0..rows).map(|index| index.to_string()).collect(),
        medoids: None,
        rejections: Vec::new(),
        confirmed: Vec::new(),
    };
    assert!(matches!(
        MlClient::new("/definitely/missing.sock").cluster(&request),
        Err(ClientError::InvalidEmbedding)
    ));
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
                    "model_bundle":{"name":"test", "version":"test-v1", "sha256":"0000000000000000000000000000000000000000000000000000000000000000"},
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

#[test]
fn client_absolute_deadline_rejects_slow_drip_response() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("drip.sock");
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("drip socket should bind: {error}"),
    };
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("drip mock should accept");
        let _ = read_request(&mut stream);
        let body = br#"{"status":"ok","backend":"mock","model_bundle":null,"providers":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("health body")
        );
        for byte in response.bytes() {
            if stream.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let client = MlClient::with_timeout(socket, Duration::from_millis(30));

    assert!(matches!(client.health(), Err(ClientError::Timeout)));
    thread.join().expect("drip mock should finish");
}

#[test]
fn client_rejects_ambiguous_response_framing() {
    for (index, ambiguous_header) in [
        "Content-Length: 2\r\nContent-Length: 2",
        "Content-Length: 2\r\nTransfer-Encoding: identity",
    ]
    .into_iter()
    .enumerate()
    {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join(format!("framing-{index}.sock"));
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("framing socket should bind: {error}"),
        };
        let header = ambiguous_header.to_owned();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("framing mock should accept");
            let _ = read_request(&mut stream);
            write!(stream, "HTTP/1.1 200 OK\r\n{header}\r\n\r\n{{}}")
                .expect("ambiguous response should write");
        });
        assert!(matches!(
            MlClient::new(socket).health(),
            Err(ClientError::Protocol(_))
        ));
        thread.join().expect("framing mock should finish");
    }
}

#[test]
fn client_rejects_header_delimiter_beyond_the_header_budget() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("large-header.sock");
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("large-header socket should bind: {error}"),
    };
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("large-header mock should accept");
        let _ = read_request(&mut stream);
        let response = format!(
            "HTTP/1.1 200 OK\r\nX-Fill: {}\r\nContent-Length: 2\r\n\r\n{{}}",
            "a".repeat(64 * 1024)
        );
        let _ = stream.write_all(response.as_bytes());
    });

    assert!(matches!(
        MlClient::new(socket).health(),
        Err(ClientError::Protocol("headers too large"))
    ));
    thread.join().expect("large-header mock should finish");
}

#[test]
fn client_rejects_unbounded_or_untrusted_sidecar_response_fields() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let Some(mock) = mock_or_skip(MockSidecar::new(
        directory.path(),
        3,
        |index, _path, request| match index {
            0 => json!({
                "status":"ok", "backend":"onnx",
                "model_bundle":{"name":"test", "version":"v1", "sha256":"short"},
                "providers":[]
            }),
            1 => json!({
                "model_version":"v1",
                "instances":[{
                    "kind":"face", "bbox":[0,0,1,1], "det_conf":1,
                    "quality":{"passed":true,"flags":[]},
                    "embedding":{"dtype":"f32","dim":4097,"b64":""}
                }]
            }),
            _ => {
                let requested_id = request["ids"][0].as_str().expect("request id");
                json!({
                    "assignments":[
                        {"id":requested_id,"cluster":null,"state":"invented","similarity":0}
                    ],
                    "new_clusters":[]
                })
            }
        },
    )) else {
        return;
    };
    let client = mock.client();
    assert!(matches!(client.health(), Err(ClientError::Protocol(_))));
    assert!(matches!(
        client.analyze(b"not-used-by-mock"),
        Err(ClientError::InvalidEmbedding)
    ));
    let request = ClusterRequest {
        mode: ClusterMode::Full,
        params: ClusterParams::default(),
        embeddings: vec![vec![1.0]],
        shape: [1, 1],
        ids: vec!["face-1".into()],
        medoids: None,
        rejections: Vec::new(),
        confirmed: Vec::new(),
    };
    assert!(matches!(
        client.cluster(&request),
        Err(ClientError::Protocol(_))
    ));
}

#[test]
fn clustering_reads_only_limit_plus_one_before_rejecting_persistent_overflow() {
    let (_directory, database) = database();
    let asset = AssetService::new(database.clone())
        .ingest(&png(), "overflow.png", None)
        .expect("asset should ingest")
        .asset;
    database
        .with_connection(|connection| {
            for _ in 0..=illumia_core::ml::MAX_CLUSTER_ROWS {
                connection.execute(
                    "INSERT INTO faces(id, asset_id, kind, bbox, det_conf, quality_flags,
                       embedding, model_version, cluster_id, state)
                     VALUES (?1, ?2, 'face', '[0,0,1,1]', 1, '[]', ?3,
                             'overflow-v1', NULL, 'unassigned')",
                    rusqlite::params![
                        uuid::Uuid::now_v7().to_string(),
                        asset.id,
                        1.0_f32.to_le_bytes()
                    ],
                )?;
            }
            Ok(())
        })
        .expect("overflow fixture should insert");

    assert!(matches!(
        MlService::new(database, MlClient::new("/definitely/missing.sock")).recluster(),
        Err(DatabaseError::InvalidMl(_))
    ));
}

#[test]
fn split_cluster_rejects_oversized_or_non_uuid_input_before_database_work() {
    let (_directory, database) = database();
    let service = MlService::new(database, MlClient::new("/definitely/missing.sock"));
    let cluster_id = uuid::Uuid::now_v7().to_string();
    let too_many = (0..=illumia_core::ml::MAX_CLUSTER_ROWS)
        .map(|_| uuid::Uuid::now_v7().to_string())
        .collect::<Vec<_>>();

    assert!(matches!(
        service.split_cluster(&cluster_id, &too_many),
        Err(DatabaseError::InvalidMl(_))
    ));
    assert!(matches!(
        service.split_cluster("not-a-uuid", &[uuid::Uuid::now_v7().to_string()]),
        Err(DatabaseError::InvalidMl(_))
    ));
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
