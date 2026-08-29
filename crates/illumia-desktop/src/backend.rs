//! デスクトップクライアントのデータ供給層 (docs/01_architecture.md)。
//!
//! 2 つの実装を同じ trait の裏に置く:
//!
//! - [`LocalBackend`] (all-in-one): `illumia-core` のサービス層を **in-process で直接呼ぶ**。
//!   HTTP クライアントも listener も使わない。これにより「TCP を一切 bind しない」という
//!   必須要件を、実行時の設定ではなく**構造として**保証する
//!   (localhost で listen して認証で弾く方式は禁止 → docs/01)。
//! - [`RemoteBackend`] (client-only): リモート server へ HTTP 接続する。
//!
//! UI 層は `Arc<dyn Backend>` にのみ依存し、どちらで動いているかを意識しない。

use std::{io::Read, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use illumia_core::{
    assets::AssetService,
    db::Database,
    timeline::{Granularity, TimelineService},
};
use url::{Host, Url};
use zeroize::Zeroize;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROBE_BODY: usize = 64 * 1024;
const MAX_JSON_BODY: usize = 4 * 1024 * 1024;
const MAX_THUMBNAIL_BODY: usize = 2 * 1024 * 1024;
const MAX_PREVIEW_BODY: usize = 16 * 1024 * 1024;

/// タイムラインの 1 バケット (日 / 月 / 年)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub key: String,
    pub count: u64,
}

/// バケット内の 1 アセット。`ratio` は justified レイアウトへ渡す。
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub ratio: f64,
    pub taken_at: String,
}

/// 画像の解像度種別。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Variant {
    Thumbnail,
    Preview,
}

impl Variant {
    fn suffix(self) -> &'static str {
        match self {
            Self::Thumbnail => "t",
            Self::Preview => "p",
        }
    }

    fn response_limit(self) -> usize {
        match self {
            Self::Thumbnail => MAX_THUMBNAIL_BODY,
            Self::Preview => MAX_PREVIEW_BODY,
        }
    }
}

/// UI が必要とする読み取り操作。
pub trait Backend: Send + Sync {
    /// この backend が TCP 接続を行うか。all-in-one では必ず false。
    fn uses_network(&self) -> bool;

    /// 表示用のモード名。
    fn mode_label(&self) -> &'static str;

    fn buckets(&self, granularity: Granularity) -> Result<Vec<Bucket>>;

    fn items(&self, bucket_key: &str) -> Result<Vec<Item>>;

    /// 派生画像のバイト列 (WebP)。
    fn image(&self, asset_id: &str, variant: Variant) -> Result<Vec<u8>>;
}

// ---- all-in-one ----

/// サービス層を直接呼ぶ backend。listener も HTTP クライアントも持たない。
pub struct LocalBackend {
    database: Database,
    data_root: std::path::PathBuf,
}

impl LocalBackend {
    pub fn open(data_root: impl Into<std::path::PathBuf>) -> Result<Self> {
        let data_root = data_root.into();
        std::fs::create_dir_all(&data_root).with_context(|| {
            format!("データディレクトリを作成できない: {}", data_root.display())
        })?;
        let database = Database::open(&data_root).context("データベースを開けない")?;
        Ok(Self {
            database,
            data_root,
        })
    }

    fn thumb_path(&self, asset_id: &str, variant: Variant) -> std::path::PathBuf {
        self.data_root
            .join("thumbs")
            .join(format!("{asset_id}_{}.webp", variant.suffix()))
    }
}

impl Backend for LocalBackend {
    fn uses_network(&self) -> bool {
        // all-in-one は構造的に TCP を使わない (docs/01 の必須要件)。
        false
    }

    fn mode_label(&self) -> &'static str {
        "all-in-one (ローカル / ネットワーク未使用)"
    }

    fn buckets(&self, granularity: Granularity) -> Result<Vec<Bucket>> {
        let service = TimelineService::new(self.database.clone());
        Ok(service
            .buckets(granularity)
            .context("バケットを取得できない")?
            .into_iter()
            .map(|(key, count)| Bucket { key, count })
            .collect())
    }

    fn items(&self, bucket_key: &str) -> Result<Vec<Item>> {
        let service = TimelineService::new(self.database.clone());
        Ok(service
            .bucket_items(bucket_key)
            .context("バケット内のアセットを取得できない")?
            .into_iter()
            .map(|item| Item {
                id: item.id,
                ratio: item.ratio,
                taken_at: item.taken_at,
            })
            .collect())
    }

    fn image(&self, asset_id: &str, variant: Variant) -> Result<Vec<u8>> {
        // asset id は DB 由来。パスは UUID + 固定拡張子から組み立てる (docs/12)。
        let service = AssetService::new(self.database.clone());
        if service
            .get(asset_id)
            .context("アセットを参照できない")?
            .is_none()
        {
            bail!("アセットが存在しない");
        }
        let path = self.thumb_path(asset_id, variant);
        let limit = variant.response_limit();
        let file = std::fs::File::open(&path)
            .with_context(|| format!("画像を読めない: {}", path.display()))?;
        if file.metadata().context("画像サイズを確認できない")?.len() > limit as u64 {
            bail!("派生画像が上限を超えている");
        }
        read_capped(file, limit)
    }
}

// ---- client-only ----

/// リモート server へ HTTP 接続する backend。
pub struct RemoteBackend {
    base_url: Url,
    token: String,
    agent: ureq::Agent,
}

/// OS secure storage に保存する remote credential と server identity pin。
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCredential {
    token: String,
    instance_id: String,
}

impl RemoteCredential {
    fn validate(&self) -> Result<()> {
        if self.token.is_empty()
            || self.token.len() > 1024
            || self.token.chars().any(char::is_control)
        {
            bail!("保存済み device token の形式が不正");
        }
        validate_instance_id(&self.instance_id)
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn into_token(mut self) -> String {
        std::mem::take(&mut self.token)
    }
}

impl Drop for RemoteCredential {
    fn drop(&mut self) {
        self.token.zeroize();
        self.instance_id.zeroize();
    }
}

impl Drop for RemoteBackend {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl RemoteBackend {
    /// credential を送る前に unauthenticated probe と identity pin 照合を行う。
    pub fn connect(base_url: &str, credential: RemoteCredential) -> Result<Self> {
        let normalized = validate_base_url(base_url)?;
        credential.validate()?;
        let info = probe_url(&normalized)?;
        if info.instance_id != credential.instance_id {
            bail!("保存済みのサーバーと異なるサーバーが応答したため接続を中止した");
        }
        let token = credential.into_token();
        Ok(Self {
            base_url: normalized,
            token,
            agent: build_agent(REQUEST_TIMEOUT),
        })
    }

    fn get(&self, path: &str, limit: usize) -> Result<Vec<u8>> {
        let target = self.base_url.join(path).context("API URL を構築できない")?;
        if target.origin() != self.base_url.origin() || !target.path().starts_with("/api/") {
            bail!("API 境界外への request を拒否した");
        }
        let mut authorization = String::with_capacity(7 + self.token.len());
        authorization.push_str("Bearer ");
        authorization.push_str(&self.token);
        let response = self
            .agent
            .get(target.as_str())
            .header("Authorization", &authorization)
            .call();
        authorization.zeroize();
        read_response(response.context("サーバーに接続できない")?, limit)
    }
}

/// 接続先 URL の検証 (docs/12: SEC-002 と同じ規則)。
fn validate_base_url(raw: &str) -> Result<Url> {
    if raw.len() > 2048 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("URL に制御文字または空白を含められない");
    }
    let url = Url::parse(raw).context("URL の形式が正しくない")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("URL に認証情報を埋め込めない");
    }
    if url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        bail!("URL に path・クエリ・フラグメントを指定できない");
    }
    let host = url.host().context("URL のホストが空")?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&host) => {}
        "http" => bail!("平文 HTTP はループバックのみ使用できる"),
        _ => bail!("スキームは https のみ使用できる"),
    }
    Ok(url)
}

fn is_loopback_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
        Host::Domain(name) => {
            let lower = name.to_ascii_lowercase();
            lower == "localhost" || lower.ends_with(".localhost")
        }
    }
}

pub fn normalize_base_url(raw: &str) -> Result<String> {
    Ok(validate_base_url(raw)?
        .as_str()
        .trim_end_matches('/')
        .to_owned())
}

pub fn is_insecure_loopback(raw: &str) -> Result<bool> {
    Ok(validate_base_url(raw)?.scheme() == "http")
}

fn build_agent(global_timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(global_timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT.min(global_timeout)))
        .max_redirects(0)
        .build()
        .into()
}

fn read_response(mut response: ureq::http::Response<ureq::Body>, limit: usize) -> Result<Vec<u8>> {
    if response
        .headers()
        .get(ureq::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit as u64)
    {
        bail!("サーバー応答が上限を超えている");
    }

    let reader = response
        .body_mut()
        .with_config()
        .limit(limit.saturating_add(1) as u64)
        .reader();
    read_capped(reader, limit)
}

fn read_capped(mut reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut chunk).context("応答を読み取れない")?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > limit {
            bail!("サーバー応答が上限を超えている");
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn validate_instance_id(instance_id: &str) -> Result<()> {
    if instance_id.len() != 32 || !instance_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("サーバー識別子の形式が不正");
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct ServerInfoDto {
    setup_completed: bool,
    authenticated: bool,
    setup_token_required: bool,
    instance_id: String,
}

fn probe_url(base_url: &Url) -> Result<ServerInfoDto> {
    let target = base_url
        .join("/api/server/info")
        .context("probe URL を構築できない")?;
    let agent = build_agent(PROBE_TIMEOUT);
    let response = agent
        .get(target.as_str())
        .call()
        .context("サーバー identity probe に失敗した")?;
    let bytes = read_response(response, MAX_PROBE_BODY)?;
    let info: ServerInfoDto =
        serde_json::from_slice(&bytes).context("Illumia server info の形式が不正")?;
    validate_instance_id(&info.instance_id)?;
    let _ = (
        info.setup_completed,
        info.authenticated,
        info.setup_token_required,
    );
    Ok(info)
}

/// token/password を付けない server identity probe。
pub fn probe_identity(base_url: &str) -> Result<String> {
    let normalized = validate_base_url(base_url)?;
    Ok(probe_url(&normalized)?.instance_id)
}

#[derive(serde::Serialize)]
struct LoginRequest<'a> {
    password: &'a str,
    device_name: &'a str,
}

#[derive(serde::Deserialize)]
struct LoginResponse {
    token: String,
}

/// identity を再確認した後にだけ password login し、device token を受け取る。
pub fn login(
    base_url: &str,
    password: &str,
    expected_instance_id: &str,
) -> Result<RemoteCredential> {
    validate_instance_id(expected_instance_id)?;
    let normalized = validate_base_url(base_url)?;
    let info = probe_url(&normalized)?;
    if info.instance_id != expected_instance_id {
        bail!("確認後にサーバー identity が変化したため login を中止した");
    }

    let target = normalized
        .join("/api/auth/login")
        .context("login URL を構築できない")?;
    let mut body = serde_json::to_vec(&LoginRequest {
        password,
        device_name: "Illumia Desktop",
    })
    .context("login request を構築できない")?;
    let result = build_agent(REQUEST_TIMEOUT)
        .post(target.as_str())
        .header("Content-Type", "application/json")
        .send(body.as_slice());
    body.zeroize();
    let response = result.context("login に失敗した")?;
    let mut response_body = read_response(response, MAX_PROBE_BODY)?;
    let parsed = serde_json::from_slice(&response_body);
    response_body.zeroize();
    let parsed: LoginResponse = parsed.context("login response の形式が不正")?;
    let credential = RemoteCredential {
        token: parsed.token,
        instance_id: info.instance_id,
    };
    credential.validate()?;
    Ok(credential)
}

#[derive(serde::Deserialize)]
struct BucketDto {
    key: String,
    count: u64,
}

#[derive(serde::Deserialize)]
struct ItemDto {
    id: String,
    ratio: f64,
    taken_at: String,
}

impl Backend for RemoteBackend {
    fn uses_network(&self) -> bool {
        true
    }

    fn mode_label(&self) -> &'static str {
        "client-only (リモート接続)"
    }

    fn buckets(&self, granularity: Granularity) -> Result<Vec<Bucket>> {
        let granularity = match granularity {
            Granularity::Day => "day",
            Granularity::Month => "month",
            Granularity::Year => "year",
        };
        let body = self.get(
            &format!("/api/timeline/buckets?granularity={granularity}"),
            MAX_JSON_BODY,
        )?;
        let dto: Vec<BucketDto> = serde_json::from_slice(&body).context("バケット JSON が不正")?;
        Ok(dto
            .into_iter()
            .map(|bucket| Bucket {
                key: bucket.key,
                count: bucket.count,
            })
            .collect())
    }

    fn items(&self, bucket_key: &str) -> Result<Vec<Item>> {
        // bucket key は server 由来だが、パス組み立て前に形を検証する。
        if !bucket_key.chars().all(|c| c.is_ascii_digit() || c == '-') {
            bail!("バケットキーの形式が不正");
        }
        let body = self.get(&format!("/api/timeline/bucket/{bucket_key}"), MAX_JSON_BODY)?;
        let dto: Vec<ItemDto> = serde_json::from_slice(&body).context("アセット JSON が不正")?;
        Ok(dto
            .into_iter()
            .map(|item| Item {
                id: item.id,
                ratio: item.ratio,
                taken_at: item.taken_at,
            })
            .collect())
    }

    fn image(&self, asset_id: &str, variant: Variant) -> Result<Vec<u8>> {
        if !asset_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            bail!("アセット ID の形式が不正");
        }
        let limit = variant.response_limit();
        let variant = match variant {
            Variant::Thumbnail => "thumbnail",
            Variant::Preview => "preview",
        };
        self.get(&format!("/api/assets/{asset_id}/{variant}"), limit)
    }
}

/// 起動モード。
pub enum Mode {
    AllInOne {
        data_root: std::path::PathBuf,
    },
    ClientOnly {
        base_url: String,
        credential: RemoteCredential,
    },
}

pub fn build(mode: Mode) -> Result<Arc<dyn Backend>> {
    Ok(match mode {
        Mode::AllInOne { data_root } => Arc::new(LocalBackend::open(data_root)?),
        Mode::ClientOnly {
            base_url,
            credential,
        } => Arc::new(RemoteBackend::connect(&base_url, credential)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_one_info(instance_id: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener を作れる");
        let address = listener.local_addr().expect("local address を取得できる");
        let body = format!(
            r#"{{"setup_completed":true,"authenticated":false,"setup_token_required":false,"instance_id":"{instance_id}"}}"#
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("probe を受信できる");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout を設定できる");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).expect("request を読める");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("response を返せる");
            String::from_utf8(request).expect("request は UTF-8")
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn remote_base_url_requires_https_or_loopback() {
        assert!(validate_base_url("https://illumia.example.com").is_ok());
        assert!(validate_base_url("http://127.0.0.1:2283").is_ok());
        assert!(validate_base_url("http://[::1]:2283").is_ok());
        assert!(validate_base_url("http://illumia.localhost:2283").is_ok());
        assert!(validate_base_url("https://photos.local").is_ok());
        assert!(validate_base_url("http://photos.local").is_err());
        assert!(validate_base_url("http://illumia.example.com").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
    }

    #[test]
    fn remote_base_url_rejects_credentials_and_query() {
        assert!(validate_base_url("https://user:pass@example.com").is_err());
        assert!(validate_base_url("https://example.com?a=1").is_err());
        assert!(validate_base_url("https://example.com#x").is_err());
        assert!(validate_base_url("https://example.com/sub").is_err());
        assert!(validate_base_url("https://exam\nple.com").is_err());
    }

    #[test]
    fn unknown_length_body_is_stopped_at_limit() {
        let body = vec![0_u8; 1025];
        assert!(read_capped(std::io::Cursor::new(body), 1024).is_err());
    }

    #[test]
    fn image_response_limits_are_variant_specific() {
        assert_eq!(Variant::Thumbnail.response_limit(), 2 * 1024 * 1024);
        assert_eq!(Variant::Preview.response_limit(), 16 * 1024 * 1024);
    }

    #[test]
    fn network_agents_have_global_and_connect_deadlines() {
        let agent = build_agent(REQUEST_TIMEOUT);
        let timeouts = agent.config().timeouts();
        assert_eq!(timeouts.global, Some(REQUEST_TIMEOUT));
        assert_eq!(timeouts.connect, Some(CONNECT_TIMEOUT));
        assert_eq!(agent.config().max_redirects(), 0);
    }

    #[test]
    fn identity_mismatch_probe_never_sends_bearer_token() {
        let (base_url, server) = serve_one_info("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let credential = RemoteCredential {
            token: "secret-device-token".into(),
            instance_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        };

        assert!(RemoteBackend::connect(&base_url, credential).is_err());
        let request = server.join().expect("server thread が完了する");
        assert!(request.starts_with("GET /api/server/info HTTP/1.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(!request.contains("secret-device-token"));
    }

    #[test]
    fn instance_identity_has_the_server_generated_hex_shape() {
        assert!(validate_instance_id("0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_instance_id("0123456789ABCDEF0123456789ABCDEF").is_ok());
        assert!(validate_instance_id("attacker-instance").is_err());
        assert!(validate_instance_id("0123456789abcdef0123456789abcde\n").is_err());
    }

    #[test]
    fn local_backend_never_uses_the_network() {
        let dir = std::env::temp_dir().join(format!("illumia-desktop-{}", std::process::id()));
        let backend = LocalBackend::open(&dir).expect("ローカル backend を開ける");
        assert!(!backend.uses_network());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
