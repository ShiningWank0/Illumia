// Illumia 専用の HTTP ブリッジ (docs/12_security.md, SEC-004)。
//
// 汎用の plugin-http fetch を frontend へ公開すると、WebView が XSS 等で侵害された
// 場合に Rust backend 経由で任意の host へ通信でき、token / password / metadata の
// 持ち出し経路になる。そのため plugin-http は capability から外し、代わりに
// 「登録済み Illumia サーバーへの、許可された API 呼び出しだけ」を通すこの
// command を公開する。
//
// 検査項目:
//   - 宛先は `set_server` で登録済みの base URL と完全一致 (scheme/host/port)
//   - path は `/api/` 配下のみ。`..`、非 ASCII、制御文字を拒否
//   - method / header 名は allowlist
//   - request / response の body に上限
//
// base URL 自体の妥当性 (https のみ、平文 HTTP は private 宛先のみ) は SEC-002 と
// 同じ規則で Rust 側でも検証する。frontend の検証だけに依存しない。

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
    sync::Mutex,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// server 側の上限 (129 MiB) に合わせる。これを超える送信は往復前に拒否する。
const MAX_REQUEST_BODY: usize = 129 * 1024 * 1024;
/// 原本ダウンロードを想定した response 上限。
const MAX_RESPONSE_BODY: usize = 129 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 許可する HTTP method。
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// 許可する request header 名 (小文字)。認証系以外は通さない。
const ALLOWED_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "authorization",
    "x-illumia-setup-token",
    "x-illumia-auth-mode",
    "x-vault-session",
];

#[derive(Default)]
pub struct ServerBinding {
    base: Mutex<Option<Url>>,
}

impl ServerBinding {
    fn get(&self) -> Option<Url> {
        self.base
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set(&self, url: Option<Url>) {
        *self
            .base
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = url;
    }
}

#[derive(Deserialize)]
pub struct BridgeRequest {
    /// `/api/...` で始まる path + query。
    path: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    /// base64 エンコードした request body。
    #[serde(default)]
    body_base64: Option<String>,
}

#[derive(Serialize)]
pub struct BridgeResponse {
    status: u16,
    headers: HashMap<String, String>,
    body_base64: String,
}

/// private / loopback / link-local 宛か。平文 HTTP を許すのはこの範囲だけ。
fn is_private_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || *v4 == Ipv4Addr::UNSPECIFIED
        }
        Host::Ipv6(v6) => {
            v6.is_loopback()
                || *v6 == Ipv6Addr::UNSPECIFIED
                // fc00::/7 (ULA) と fe80::/10 (link-local)
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
        Host::Domain(name) => {
            let lower = name.to_ascii_lowercase();
            lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local")
        }
    }
}

/// base URL を検証して正規化する (SEC-002 と同じ規則)。
fn validate_base_url(raw: &str) -> Result<Url, String> {
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URL に制御文字または空白が含まれています".into());
    }
    let url = Url::parse(raw).map_err(|_| "URL の形式が正しくありません".to_string())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL に認証情報を埋め込むことはできません".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("URL にクエリ・フラグメントは指定できません".into());
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err("URL にパスは指定できません".into());
    }
    let host = url.host().ok_or("URL のホストが空です".to_string())?;
    match url.scheme() {
        "https" => {}
        "http" => {
            if !is_private_host(&host) {
                return Err("平文 HTTP はプライベートアドレスのみ使用できます".into());
            }
        }
        _ => return Err("スキームは https のみ使用できます".into()),
    }
    drop(host);
    Ok(url)
}

/// path が `/api/` 配下の安全な相対参照か検証する。
fn validate_path(path: &str) -> Result<(), String> {
    if !path.starts_with("/api/") {
        return Err("許可されていないパスです".into());
    }
    if !path.is_ascii() {
        return Err("パスに非 ASCII 文字を含められません".into());
    }
    if path.chars().any(|c| c.is_ascii_control() || c == ' ') {
        return Err("パスに制御文字を含められません".into());
    }
    // `..` による上位参照と、authority への化けを防ぐ。
    if path.contains("..") || path.starts_with("//") {
        return Err("パスに上位参照を含められません".into());
    }
    Ok(())
}

/// 登録済みサーバーへ HTTP を発行する。宛先・method・header・body を検査する。
#[tauri::command]
pub async fn illumia_request(
    binding: tauri::State<'_, ServerBinding>,
    client: tauri::State<'_, reqwest::Client>,
    request: BridgeRequest,
) -> Result<BridgeResponse, String> {
    let base = binding.get().ok_or("接続先サーバーが未設定です".to_string())?;
    validate_path(&request.path)?;

    let method = request.method.as_deref().unwrap_or("GET").to_ascii_uppercase();
    if !ALLOWED_METHODS.contains(&method.as_str()) {
        return Err("許可されていない HTTP メソッドです".into());
    }
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| "許可されていない HTTP メソッドです".to_string())?;

    // base を起点に解決し、結果が base と同一 origin であることを再確認する。
    let target = base
        .join(&request.path)
        .map_err(|_| "パスを解決できません".to_string())?;
    if target.scheme() != base.scheme()
        || target.host_str() != base.host_str()
        || target.port_or_known_default() != base.port_or_known_default()
    {
        return Err("登録済みサーバー以外へは接続できません".into());
    }

    let body = match request.body_base64.as_deref() {
        Some(encoded) => {
            let bytes = BASE64
                .decode(encoded)
                .map_err(|_| "リクエストボディが不正です".to_string())?;
            if bytes.len() > MAX_REQUEST_BODY {
                return Err("リクエストボディが大きすぎます".into());
            }
            Some(bytes)
        }
        None => None,
    };

    let mut builder = client.request(method, target).timeout(REQUEST_TIMEOUT);
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if !ALLOWED_HEADERS.contains(&lower.as_str()) {
            return Err(format!("許可されていないヘッダです: {lower}"));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err("ヘッダ値に制御文字を含められません".into());
        }
        builder = builder.header(lower, value);
    }
    if let Some(body) = body {
        builder = builder.body(body);
    }

    let response = builder
        .send()
        .await
        .map_err(|_| "サーバーに接続できませんでした".to_string())?;

    let status = response.status().as_u16();
    let mut headers = HashMap::new();
    for (name, value) in response.headers() {
        // frontend が必要とするのは content-type 程度。Set-Cookie 等は渡さない。
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "content-type" || lower == "content-length" {
            if let Ok(value) = value.to_str() {
                headers.insert(lower, value.to_owned());
            }
        }
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|_| "応答を読み取れませんでした".to_string())?;
    if bytes.len() > MAX_RESPONSE_BODY {
        return Err("応答が大きすぎます".into());
    }

    Ok(BridgeResponse {
        status,
        headers,
        body_base64: BASE64.encode(&bytes),
    })
}

/// 接続先サーバーを登録する。検証を通らない URL は保持しない。
#[tauri::command]
pub fn illumia_set_server(
    binding: tauri::State<'_, ServerBinding>,
    url: Option<String>,
) -> Result<(), String> {
    match url {
        Some(raw) => {
            let validated = validate_base_url(&raw)?;
            binding.set(Some(validated));
        }
        None => binding.set(None),
    }
    Ok(())
}

/// 起動直後や未接続時に IP リテラルを解決しないことを保証するための補助。
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // リダイレクトで別 host へ飛ばされる経路を塞ぐ。
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP クライアントを構築できる")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_requires_https_or_private_http() {
        assert!(validate_base_url("https://illumia.example.com").is_ok());
        assert!(validate_base_url("http://192.168.1.10:2283").is_ok());
        assert!(validate_base_url("http://illumia.example.com").is_err());
    }

    #[test]
    fn base_url_rejects_embedded_credentials_and_paths() {
        assert!(validate_base_url("https://user:pass@example.com").is_err());
        assert!(validate_base_url("https://example.com/sub").is_err());
        assert!(validate_base_url("https://example.com?a=1").is_err());
        assert!(validate_base_url("https://example.com#x").is_err());
        assert!(validate_base_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn path_must_stay_under_api() {
        assert!(validate_path("/api/server/info").is_ok());
        assert!(validate_path("/api/search?q=x").is_ok());
        assert!(validate_path("/etc/passwd").is_err());
        assert!(validate_path("/api/../../secret").is_err());
        assert!(validate_path("//evil.example.com/api/x").is_err());
    }

    #[test]
    fn unset_binding_is_rejected() {
        let binding = ServerBinding::default();
        assert!(binding.get().is_none());
    }

    #[test]
    fn private_hosts_are_recognized() {
        assert!(is_private_host(&Host::Ipv4("10.0.0.1".parse().unwrap())));
        assert!(is_private_host(&Host::Ipv4("192.168.1.1".parse().unwrap())));
        assert!(is_private_host(&Host::Ipv4("127.0.0.1".parse().unwrap())));
        assert!(!is_private_host(&Host::Ipv4("8.8.8.8".parse().unwrap())));
        assert!(is_private_host(&Host::Domain("localhost")));
        assert!(!is_private_host(&Host::Domain("example.com")));
    }
}
