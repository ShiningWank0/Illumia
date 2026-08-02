//! Stateless ML sidecar client over a Unix domain socket.
//!
//! docs/01 は Windows では named pipe を使うと定めているが未実装で、
//! Windows ビルドでは ML を利用不可 (`Error::Unavailable`) として扱う。
//! TCP は開かない (docs/01 の必須要件) ため、代替に TCP を使うことはしない。

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ML sidecar is not running")]
    Unavailable,
    #[error("ML sidecar request timed out")]
    Timeout,
    #[error("ML sidecar I/O failed")]
    Io(#[source] std::io::Error),
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
        let body = self.request("GET", "/ml/v1/health", None, &[])?;
        serde_json::from_slice(&body).map_err(Error::Json)
    }

    pub fn analyze(&self, image_bytes: &[u8]) -> Result<Analysis> {
        let body = self.request(
            "POST",
            "/ml/v1/analyze?tagger=false",
            Some("application/octet-stream"),
            image_bytes,
        )?;
        let wire: AnalysisWire = serde_json::from_slice(&body).map_err(Error::Json)?;
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
                .embeddings
                .iter()
                .any(|embedding| embedding.len() != request.shape[1])
        {
            return Err(Error::InvalidEmbedding);
        }
        let encoded = serde_json::to_vec(request).map_err(Error::Json)?;
        let body = self.request("POST", "/ml/v1/cluster", Some("application/json"), &encoded)?;
        serde_json::from_slice(&body).map_err(Error::Json)
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
        let mut stream = UnixStream::connect(&self.socket_path).map_err(map_io)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(map_io)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(map_io)?;
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
        stream.write_all(head.as_bytes()).map_err(map_io)?;
        stream.write_all(body).map_err(map_io)?;
        stream.flush().map_err(map_io)?;
        read_response(&mut stream)
    }
}

impl TryFrom<InstanceWire> for Instance {
    type Error = Error;

    fn try_from(wire: InstanceWire) -> Result<Self> {
        if wire.embedding.dtype != "f32" || wire.embedding.dim == 0 {
            return Err(Error::InvalidEmbedding);
        }
        let embedding = decode_base64(&wire.embedding.b64)?;
        if embedding.len()
            != wire
                .embedding
                .dim
                .checked_mul(4)
                .ok_or(Error::InvalidEmbedding)?
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

fn map_io(error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Error::Unavailable,
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Error::Timeout,
        _ => Error::Io(error),
    }
}

/// HTTP 応答を読む。transport 非依存にして、将来の named pipe 実装でも使える
/// ようにする。
fn read_response<S: Read>(stream: &mut S) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let header_end = loop {
        if response.len() >= MAX_HEADER_BYTES {
            return Err(Error::Protocol("headers too large"));
        }
        let mut buffer = [0_u8; 8192];
        let count = stream.read(&mut buffer).map_err(map_io)?;
        if count == 0 {
            return Err(Error::Protocol("incomplete headers"));
        }
        response.extend_from_slice(&buffer[..count]);
        if let Some(position) = response.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
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
        if name.eq_ignore_ascii_case("transfer-encoding")
            && !value.trim().eq_ignore_ascii_case("identity")
        {
            return Err(Error::Protocol("chunked response is unsupported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::Protocol("invalid content length"))?,
            );
        }
    }
    let content_length = content_length.ok_or(Error::Protocol("missing content length"))?;
    if content_length > MAX_RESPONSE_BYTES {
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
        let count = stream.read(&mut buffer[..read_len]).map_err(map_io)?;
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
