//! Stateless ML sidecar client over a Unix domain socket.
//!
//! docs/01 は Windows では named pipe を使うと定めているが未実装で、
//! Windows ビルドでは ML を利用不可 (`Error::Unavailable`) として扱う。
//! TCP は開かない (docs/01 の必須要件) ため、代替に TCP を使うことはしない。

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

// 以下は Unix domain socket 経由の実装専用。Windows では named pipe 実装が
// 入るまで参照されないので、cfg で分けて dead_code 警告を出さない。
#[cfg(unix)]
use socket2::{Domain, SockAddr, Socket, Type};
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::fd::OwnedFd,
    os::unix::net::UnixStream,
    time::Instant,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CLUSTER_ROWS: usize = 512;
const MAX_EMBEDDING_DIMENSION: usize = 4_096;
const MAX_CLUSTER_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 256;
const MAX_PROVIDERS: usize = 16;
const MAX_QUALITY_FLAGS: usize = 32;
const MAX_ANALYSIS_INSTANCES: usize = 256;
const MAX_ANALYZE_INPUT_BYTES: usize = 128 * 1024 * 1024;
#[cfg(unix)]
const MAX_HEADER_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const MAX_HEALTH_RESPONSE_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const MAX_ANALYZE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
#[cfg(unix)]
const MAX_CLUSTER_RESPONSE_BYTES: usize = 1024 * 1024;
// Unix socket timeout APIs have microsecond precision. Treat the final smaller fraction as an
// expired deadline instead of extending the absolute deadline through rounding.
#[cfg(unix)]
const MIN_SOCKET_TIMEOUT: Duration = Duration::from_micros(1);

#[derive(Debug, Error)]
pub enum Error {
    #[error("ML sidecar is not running")]
    Unavailable,
    #[error("ML sidecar request timed out")]
    Timeout,
    #[error("ML sidecar I/O failed during {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid ML sidecar HTTP response: {0}")]
    Protocol(&'static str),
    #[error("ML sidecar returned HTTP {0}")]
    HttpStatus(u16),
    #[error("invalid ML sidecar JSON response")]
    Json(#[source] serde_json::Error),
    #[error("invalid ML embedding payload")]
    InvalidEmbedding,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct MlClient {
    socket_path: PathBuf,
    /// Windows は named pipe 実装が未了で socket 経路を通らないため参照されない。
    #[cfg_attr(not(unix), allow(dead_code))]
    timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelBundle {
    pub name: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Health {
    pub status: String,
    pub backend: String,
    pub model_bundle: Option<ModelBundle>,
    pub providers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub model_version: String,
    pub instances: Vec<Instance>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub kind: String,
    pub bbox: [f64; 4],
    pub det_conf: f64,
    pub quality_passed: bool,
    pub quality_flags: Vec<String>,
    pub embedding: Vec<u8>,
    pub embedding_dim: usize,
}

#[derive(Debug, Deserialize)]
struct AnalysisWire {
    model_version: String,
    instances: Vec<InstanceWire>,
}

#[derive(Debug, Deserialize)]
struct InstanceWire {
    kind: String,
    bbox: [f64; 4],
    det_conf: f64,
    quality: QualityWire,
    embedding: EmbeddingWire,
}

#[derive(Debug, Deserialize)]
struct QualityWire {
    passed: bool,
    flags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingWire {
    dtype: String,
    dim: usize,
    b64: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClusterMode {
    Full,
    Assign,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClusterParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tau_high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tau_low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_cluster_size: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterRequest {
    pub mode: ClusterMode,
    pub params: ClusterParams,
    pub embeddings: Vec<Vec<f32>>,
    pub shape: [usize; 2],
    pub ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medoids: Option<BTreeMap<String, Vec<f32>>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejections: Vec<[String; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClusterResponse {
    pub assignments: Vec<Assignment>,
    pub new_clusters: Vec<NewCluster>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Assignment {
    pub id: String,
    pub cluster: Option<String>,
    pub state: String,
    pub similarity: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NewCluster {
    pub tmp_id: String,
    pub member_ids: Vec<String>,
    pub medoid_ids: Vec<String>,
}

impl MlClient {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeout(socket_path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn health(&self) -> Result<Health> {
        let health_client =
            Self::with_timeout(self.socket_path.clone(), self.timeout.min(HEALTH_TIMEOUT));
        let body = health_client.request("GET", "/ml/v1/health", None, &[])?;
        let health: Health = serde_json::from_slice(&body).map_err(Error::Json)?;
        validate_health(&health)?;
        Ok(health)
    }

    pub fn analyze(&self, image_bytes: &[u8]) -> Result<Analysis> {
        if image_bytes.is_empty() || image_bytes.len() > MAX_ANALYZE_INPUT_BYTES {
            return Err(Error::InvalidEmbedding);
        }
        let body = self.request(
            "POST",
            "/ml/v1/analyze?tagger=false",
            Some("application/octet-stream"),
            image_bytes,
        )?;
        let wire: AnalysisWire = serde_json::from_slice(&body).map_err(Error::Json)?;
        if !valid_text(&wire.model_version, MAX_IDENTIFIER_BYTES)
            || wire.instances.len() > MAX_ANALYSIS_INSTANCES
        {
            return Err(Error::InvalidEmbedding);
        }
        let instances = wire
            .instances
            .into_iter()
            .map(Instance::try_from)
            .collect::<Result<Vec<_>>>()?;
        Ok(Analysis {
            model_version: wire.model_version,
            instances,
        })
    }

    pub fn cluster(&self, request: &ClusterRequest) -> Result<ClusterResponse> {
        if request.shape[1] == 0
            || request.shape != [request.embeddings.len(), request.shape[1]]
            || request.ids.len() != request.embeddings.len()
            || request
                .ids
                .iter()
                .any(|id| !valid_text(id, MAX_IDENTIFIER_BYTES))
            || request.ids.iter().collect::<HashSet<_>>().len() != request.ids.len()
            || request.embeddings.iter().any(|embedding| {
                embedding.len() != request.shape[1]
                    || embedding.iter().any(|value| !value.is_finite())
            })
            || request.medoids.as_ref().is_some_and(|medoids| {
                medoids.len() > MAX_CLUSTER_ROWS
                    || medoids.iter().any(|(id, embedding)| {
                        !valid_text(id, MAX_IDENTIFIER_BYTES)
                            || embedding.len() != request.shape[1]
                            || embedding.iter().any(|value| !value.is_finite())
                    })
            })
            || request.rejections.len() > MAX_CLUSTER_ROWS
            || request.rejections.iter().any(|pair| {
                pair.iter()
                    .any(|value| !valid_text(value, MAX_IDENTIFIER_BYTES))
            })
            || request.confirmed.len() > MAX_CLUSTER_ROWS
            || request
                .confirmed
                .iter()
                .any(|id| !valid_text(id, MAX_IDENTIFIER_BYTES))
        {
            return Err(Error::InvalidEmbedding);
        }
        let bytes = request
            .embeddings
            .len()
            .checked_mul(request.shape[1])
            .and_then(|values| values.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(Error::InvalidEmbedding)?;
        if request.embeddings.len() > MAX_CLUSTER_ROWS
            || request.shape[1] > MAX_EMBEDDING_DIMENSION
            || bytes > MAX_CLUSTER_INPUT_BYTES
        {
            return Err(Error::InvalidEmbedding);
        }
        let encoded = serde_json::to_vec(request).map_err(Error::Json)?;
        if encoded.len() > MAX_CLUSTER_INPUT_BYTES {
            return Err(Error::InvalidEmbedding);
        }
        let body = self.request("POST", "/ml/v1/cluster", Some("application/json"), &encoded)?;
        let response: ClusterResponse = serde_json::from_slice(&body).map_err(Error::Json)?;
        validate_cluster_response(&response, request)?;
        Ok(response)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        #[cfg(not(unix))]
        {
            // Windows の named pipe 実装は未了 (docs/01)。TCP へは決してフォールバック
            // しない。呼び出し側は ML 無効時と同じ経路で扱う。
            let _ = (method, path, content_type, body);
            return Err(Error::Unavailable);
        }

        #[cfg(unix)]
        {
            self.request_over_unix_socket(method, path, content_type, body)
        }
    }

    #[cfg(unix)]
    fn request_over_unix_socket(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or(Error::Timeout)?;
        let mut stream = connect_unix_with_deadline(&self.socket_path, deadline)?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nAccept-Encoding: identity\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(content_type) = content_type {
            head.push_str("Content-Type: ");
            head.push_str(content_type);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        write_all_deadline(&mut stream, head.as_bytes(), deadline)?;
        write_all_deadline(&mut stream, body, deadline)?;
        set_write_deadline(&stream, deadline)?;
        stream
            .flush()
            .map_err(|error| map_io("request flush", error))?;
        let response_limit = if path == "/ml/v1/health" {
            MAX_HEALTH_RESPONSE_BYTES
        } else if path.starts_with("/ml/v1/analyze") {
            MAX_ANALYZE_RESPONSE_BYTES
        } else {
            MAX_CLUSTER_RESPONSE_BYTES
        };
        read_response(&mut stream, deadline, response_limit)
    }
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn validate_health(health: &Health) -> Result<()> {
    if health.status != "ok"
        || !matches!(health.backend.as_str(), "mock" | "onnx")
        || (health.backend == "mock"
            && (health.model_bundle.is_some() || !health.providers.is_empty()))
        || (health.backend == "onnx"
            && (health.model_bundle.is_none()
                || !health
                    .providers
                    .iter()
                    .any(|provider| provider == "CPUExecutionProvider")))
        || health.providers.len() > MAX_PROVIDERS
        || health
            .providers
            .iter()
            .any(|provider| !valid_text(provider, MAX_TEXT_BYTES))
        || health.model_bundle.as_ref().is_some_and(|bundle| {
            !valid_text(&bundle.name, MAX_TEXT_BYTES)
                || !valid_text(&bundle.version, MAX_IDENTIFIER_BYTES)
                || bundle.sha256.len() != 64
                || !bundle.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(Error::Protocol("invalid health response"));
    }
    Ok(())
}

fn validate_cluster_response(response: &ClusterResponse, request: &ClusterRequest) -> Result<()> {
    if response.assignments.len() > request.embeddings.len()
        || response.new_clusters.len() > request.embeddings.len()
    {
        return Err(Error::Protocol("cluster response exceeds row budget"));
    }
    let request_ids = request
        .ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut assigned_ids = HashSet::new();
    for assignment in &response.assignments {
        if !request_ids.contains(assignment.id.as_str())
            || !assigned_ids.insert(assignment.id.as_str())
            || assignment
                .cluster
                .as_ref()
                .is_some_and(|cluster| !valid_text(cluster, MAX_IDENTIFIER_BYTES))
            || !matches!(
                assignment.state.as_str(),
                "auto" | "candidate" | "unassigned"
            )
            || !assignment.similarity.is_finite()
            || !(-1.0..=1.0).contains(&assignment.similarity)
            || (assignment.state == "unassigned") != assignment.cluster.is_none()
        {
            return Err(Error::Protocol("invalid cluster assignment"));
        }
    }

    let mut temporary_ids = HashSet::new();
    let mut all_members = HashSet::new();
    let mut total_medoids = 0_usize;
    for cluster in &response.new_clusters {
        total_medoids = total_medoids
            .checked_add(cluster.medoid_ids.len())
            .ok_or(Error::Protocol("cluster response exceeds row budget"))?;
        if !valid_text(&cluster.tmp_id, MAX_IDENTIFIER_BYTES)
            || !temporary_ids.insert(cluster.tmp_id.as_str())
            || cluster.member_ids.len() > MAX_CLUSTER_ROWS
            || cluster.medoid_ids.len() > MAX_CLUSTER_ROWS
            || cluster
                .member_ids
                .iter()
                .any(|id| !request_ids.contains(id.as_str()) || !all_members.insert(id.as_str()))
            || cluster
                .medoid_ids
                .iter()
                .any(|id| !request_ids.contains(id.as_str()) || !cluster.member_ids.contains(id))
        {
            return Err(Error::Protocol("invalid new cluster response"));
        }
    }
    if all_members.len() > request.embeddings.len() || total_medoids > request.embeddings.len() {
        return Err(Error::Protocol("cluster response exceeds row budget"));
    }
    if request.mode == ClusterMode::Assign {
        let allowed_clusters = request
            .medoids
            .iter()
            .flat_map(|medoids| medoids.keys().map(String::as_str))
            .collect::<HashSet<_>>();
        if !response.new_clusters.is_empty()
            || response.assignments.iter().any(|assignment| {
                assignment
                    .cluster
                    .as_deref()
                    .is_some_and(|cluster| !allowed_clusters.contains(cluster))
            })
        {
            return Err(Error::Protocol("invalid assign response cluster"));
        }
    }
    Ok(())
}

impl TryFrom<InstanceWire> for Instance {
    type Error = Error;

    fn try_from(wire: InstanceWire) -> Result<Self> {
        let expected_bytes = wire
            .embedding
            .dim
            .checked_mul(4)
            .ok_or(Error::InvalidEmbedding)?;
        let expected_b64 = expected_bytes
            .div_ceil(3)
            .checked_mul(4)
            .ok_or(Error::InvalidEmbedding)?;
        if wire.embedding.dtype != "f32"
            || wire.embedding.dim == 0
            || wire.embedding.dim > MAX_EMBEDDING_DIMENSION
            || wire.embedding.b64.len() != expected_b64
            || !matches!(wire.kind.as_str(), "person" | "head" | "face")
            || wire.bbox.iter().any(|value| !value.is_finite())
            || wire.bbox.iter().any(|value| !(0.0..=1.0).contains(value))
            || wire.bbox[2] <= 0.0
            || wire.bbox[3] <= 0.0
            || wire.bbox[0] + wire.bbox[2] > 1.000_001
            || wire.bbox[1] + wire.bbox[3] > 1.000_001
            || !wire.det_conf.is_finite()
            || !(0.0..=1.0).contains(&wire.det_conf)
            || wire.quality.flags.len() > MAX_QUALITY_FLAGS
            || wire
                .quality
                .flags
                .iter()
                .any(|flag| !valid_text(flag, MAX_TEXT_BYTES))
        {
            return Err(Error::InvalidEmbedding);
        }
        let embedding = decode_base64(&wire.embedding.b64)?;
        if embedding.len() != expected_bytes
            || embedding.chunks_exact(4).any(|chunk| {
                !f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]).is_finite()
            })
        {
            return Err(Error::InvalidEmbedding);
        }
        Ok(Self {
            kind: wire.kind,
            bbox: wire.bbox,
            det_conf: wire.det_conf,
            quality_passed: wire.quality.passed,
            quality_flags: wire.quality.flags,
            embedding,
            embedding_dim: wire.embedding.dim,
        })
    }
}

#[cfg(unix)]
fn map_io(operation: &'static str, error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Error::Unavailable,
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Error::Timeout,
        _ => Error::Io {
            operation,
            source: error,
        },
    }
}

/// Connect without allowing a full or wedged sidecar accept queue to occupy a worker forever.
#[cfg(unix)]
fn connect_unix_with_deadline(path: &Path, deadline: Instant) -> Result<UnixStream> {
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
        .map_err(|error| map_io("socket creation", error))?;
    let address = SockAddr::unix(path).map_err(|error| map_io("socket address", error))?;
    socket
        .connect_timeout(&address, remaining(deadline)?)
        .map_err(|error| map_io("connect", error))?;
    Ok(UnixStream::from(OwnedFd::from(socket)))
}

#[cfg(unix)]
fn remaining(deadline: Instant) -> Result<Duration> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(Error::Timeout)?;
    if remaining < MIN_SOCKET_TIMEOUT {
        return Err(Error::Timeout);
    }
    Ok(remaining)
}

#[cfg(unix)]
#[cfg(target_os = "macos")]
fn read_after_darwin_timeout_einval(
    stream: &mut UnixStream,
    buffer: &mut [u8],
    operation: &'static str,
    timeout_error: std::io::Error,
) -> Result<usize> {
    // Darwin rejects SO_RCVTIMEO with EINVAL after the peer has fully closed, even when response
    // bytes remain buffered. Never ignore EINVAL and perform a blocking read: prove that data or
    // EOF is immediately available with one nonblocking read instead.
    stream
        .set_nonblocking(true)
        .map_err(|error| map_io("read fallback configuration", error))?;
    let read_result = stream.read(buffer);
    let restore_result = stream.set_nonblocking(false);
    if let Err(error) = restore_result {
        return Err(map_io("read blocking mode restoration", error));
    }
    match read_result {
        Ok(count) => Ok(count),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Err(map_io("read timeout configuration", timeout_error))
        }
        Err(error) => Err(map_io(operation, error)),
    }
}

#[cfg(unix)]
fn read_with_deadline(
    stream: &mut UnixStream,
    buffer: &mut [u8],
    deadline: Instant,
    operation: &'static str,
) -> Result<usize> {
    match stream.set_read_timeout(Some(remaining(deadline)?)) {
        Ok(()) => {}
        #[cfg(target_os = "macos")]
        Err(timeout_error) if timeout_error.kind() == std::io::ErrorKind::InvalidInput => {
            return read_after_darwin_timeout_einval(stream, buffer, operation, timeout_error);
        }
        Err(error) => return Err(map_io("read timeout configuration", error)),
    }
    stream
        .read(buffer)
        .map_err(|error| map_io(operation, error))
}

#[cfg(unix)]
fn set_write_deadline(stream: &UnixStream, deadline: Instant) -> Result<()> {
    stream
        .set_write_timeout(Some(remaining(deadline)?))
        .map_err(|error| map_io("write timeout configuration", error))
}

#[cfg(unix)]
fn write_all_deadline(stream: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        set_write_deadline(stream, deadline)?;
        let written = stream
            .write(bytes)
            .map_err(|error| map_io("request write", error))?;
        if written == 0 {
            return Err(Error::Protocol("incomplete request write"));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

/// HTTP 応答を absolute deadline と endpoint 別サイズ上限の内側で読む。
#[cfg(unix)]
fn read_response(
    stream: &mut UnixStream,
    deadline: Instant,
    max_response_bytes: usize,
) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let header_end = loop {
        let mut buffer = [0_u8; 8192];
        let count = read_with_deadline(stream, &mut buffer, deadline, "response header read")?;
        if count == 0 {
            return Err(Error::Protocol("incomplete headers"));
        }
        response.extend_from_slice(&buffer[..count]);
        if let Some(position) = response.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = position + 4;
            if header_end > MAX_HEADER_BYTES {
                return Err(Error::Protocol("headers too large"));
            }
            break header_end;
        }
        if response.len() >= MAX_HEADER_BYTES {
            return Err(Error::Protocol("headers too large"));
        }
    };
    let header = std::str::from_utf8(&response[..header_end])
        .map_err(|_| Error::Protocol("non-ASCII headers"))?;
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(Error::Protocol("invalid status line"))?;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Error::Protocol("transfer encoding is unsupported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(Error::Protocol("duplicate content length"));
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::Protocol("invalid content length"))?,
            );
        }
    }
    let content_length = content_length.ok_or(Error::Protocol("missing content length"))?;
    if content_length > max_response_bytes {
        return Err(Error::Protocol("response body too large"));
    }
    let mut body = response.split_off(header_end);
    if body.len() > content_length {
        body.truncate(content_length);
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut buffer = [0_u8; 8192];
        let read_len = remaining.min(buffer.len());
        let count = read_with_deadline(
            stream,
            &mut buffer[..read_len],
            deadline,
            "response body read",
        )?;
        if count == 0 {
            return Err(Error::Protocol("incomplete response body"));
        }
        body.extend_from_slice(&buffer[..count]);
    }
    if !(200..300).contains(&status) {
        return Err(Error::HttpStatus(status));
    }
    Ok(body)
}

fn decode_base64(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(4) {
        return Err(Error::InvalidEmbedding);
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    for (chunk_index, chunk) in value.as_bytes().chunks_exact(4).enumerate() {
        let last = chunk_index + 1 == value.len() / 4;
        let a = base64_value(chunk[0]).ok_or(Error::InvalidEmbedding)?;
        let b = base64_value(chunk[1]).ok_or(Error::InvalidEmbedding)?;
        let c_pad = chunk[2] == b'=';
        let d_pad = chunk[3] == b'=';
        if c_pad && !d_pad || (!last && (c_pad || d_pad)) {
            return Err(Error::InvalidEmbedding);
        }
        let c = if c_pad {
            0
        } else {
            base64_value(chunk[2]).ok_or(Error::InvalidEmbedding)?
        };
        let d = if d_pad {
            0
        } else {
            base64_value(chunk[3]).ok_or(Error::InvalidEmbedding)?
        };
        output.push((a << 2) | (b >> 4));
        if !c_pad {
            output.push((b << 4) | (c >> 2));
        }
        if !d_pad {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn buffered_response_is_read_after_peer_fully_closes() {
        let (mut client, mut peer) = UnixStream::pair().expect("unix stream pair");
        peer.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        )
        .expect("response should write");
        drop(peer);

        let body = read_response(
            &mut client,
            Instant::now() + Duration::from_secs(30),
            MAX_HEALTH_RESPONSE_BYTES,
        )
        .expect("buffered response should remain readable after peer close");
        assert_eq!(body, b"{}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_timeout_fallback_restores_blocking_after_buffered_read() {
        let (mut client, mut peer) = UnixStream::pair().expect("unix stream pair");
        peer.write_all(b"a").expect("first byte should write");
        let (send, receive) = std::sync::mpsc::channel();
        let sender = std::thread::spawn(move || {
            receive.recv().expect("send signal");
            std::thread::sleep(Duration::from_millis(20));
            peer.write_all(b"b").expect("delayed byte should write");
        });

        let mut first = [0_u8; 1];
        let count = read_after_darwin_timeout_einval(
            &mut client,
            &mut first,
            "synthetic read",
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "synthetic timeout failure",
            ),
        )
        .expect("buffered byte should be read");
        assert_eq!(count, 1);
        assert_eq!(first, *b"a");

        send.send(()).expect("send signal");
        let mut next = [0_u8; 1];
        let next_result = client.read_exact(&mut next);
        sender.join().expect("sender should finish");
        next_result.expect("the stream should be restored to blocking mode");
        assert_eq!(next, *b"b");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_timeout_fallback_restores_blocking_after_would_block() {
        let (mut client, mut peer) = UnixStream::pair().expect("unix stream pair");
        let (send, receive) = std::sync::mpsc::channel();
        let sender = std::thread::spawn(move || {
            receive.recv().expect("send signal");
            std::thread::sleep(Duration::from_millis(20));
            peer.write_all(b"x").expect("delayed byte should write");
        });

        let mut buffer = [0_u8; 1];
        let result = read_after_darwin_timeout_einval(
            &mut client,
            &mut buffer,
            "synthetic read",
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "synthetic timeout failure",
            ),
        );
        let Err(Error::Io { operation, source }) = result else {
            panic!("no-data fallback should preserve the timeout configuration error");
        };
        assert_eq!(operation, "read timeout configuration");
        assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(source.to_string(), "synthetic timeout failure");

        send.send(()).expect("send signal");
        let mut next = [0_u8; 1];
        let next_result = client.read_exact(&mut next);
        sender.join().expect("sender should finish");
        next_result.expect("the stream should be restored to blocking mode");
        assert_eq!(next, *b"x");
    }

    // Some macOS application sandboxes reject AF_UNIX bind with EPERM. Linux CI exercises
    // the real accept-queue saturation path used by the supported server environment.
    #[cfg(target_os = "linux")]
    #[test]
    fn connect_deadline_bounds_a_full_unix_accept_queue() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("full.sock");
        let address = SockAddr::unix(&path).expect("unix socket address");
        let listener = Socket::new(Domain::UNIX, Type::STREAM, None).expect("listener socket");
        listener.bind(&address).expect("listener should bind");
        listener.listen(1).expect("listener should listen");

        let mut fillers = Vec::new();
        let mut saturated = false;
        for _ in 0..64 {
            let filler = Socket::new(Domain::UNIX, Type::STREAM, None).expect("filler socket");
            match filler.connect_timeout(&address, Duration::from_millis(5)) {
                Ok(()) => fillers.push(filler),
                // Linux can signal a full Unix accept queue as ETIMEDOUT/EAGAIN
                // or as POLLHUP without a corresponding SO_ERROR. Once at least
                // one connection is queued, either outcome proves saturation.
                Err(_) if !fillers.is_empty() => {
                    saturated = true;
                    break;
                }
                Err(error) => panic!("listener rejected the first filler connection: {error}"),
            }
        }
        assert!(saturated, "test did not saturate the unix accept queue");

        let started = Instant::now();
        let result = connect_unix_with_deadline(&path, started + Duration::from_millis(25));
        // Linux may report either ETIMEDOUT/EAGAIN or POLLHUP without SO_ERROR
        // when a Unix accept queue is full. Both are bounded connection failures;
        // the security property under test is that the caller cannot block forever.
        assert!(
            result.is_err(),
            "a saturated accept queue must reject the request"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
