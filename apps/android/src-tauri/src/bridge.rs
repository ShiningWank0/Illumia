// Illumia 専用の HTTP ブリッジ (docs/12_security.md, SEC-004)。
//
// 汎用の plugin-http fetch を frontend へ公開すると、WebView が XSS 等で侵害された
// 場合に Rust backend 経由で任意の host へ通信でき、token / password / metadata の
// 持ち出し経路になる。そのため plugin-http は capability から外し、代わりに
// 「登録済み Illumia サーバーへの、許可された API 呼び出しだけ」を通すこの
// command を公開する。
//
// 検査項目:
//   - 宛先は identity probe 後に 1 回だけ bind した base URL と完全一致
//   - path は `/api/` 配下のみ。`..`、非 ASCII、制御文字を拒否
//   - method / header 名は allowlist
//   - request / response の body に上限
//
// base URL 自体の妥当性 (https のみ、平文 HTTP は private 宛先のみ) は SEC-002 と
// 同じ規則で Rust 側でも検証する。frontend の検証だけに依存しない。

use std::{collections::HashMap, io::Write, sync::Mutex, time::Duration};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_fs::{FsExt, OpenOptions};
use url::{Host, Url};
use zeroize::{Zeroize, Zeroizing};

/// Base64 IPC の増幅を含めても端末を圧迫しない上限。multipart overhead を含む。
const MAX_REQUEST_BODY: usize = 17 * 1024 * 1024;
/// 原本はIPCへ載せず、native保存先へ直接streamする。
const MAX_ORIGINAL_DOWNLOAD: usize = 128 * 1024 * 1024;
const MAX_THUMBNAIL_RESPONSE_BODY: usize = 2 * 1024 * 1024;
const MAX_PREVIEW_RESPONSE_BODY: usize = 16 * 1024 * 1024;
const MAX_API_RESPONSE_BODY: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 許可する HTTP method。
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// 許可する request header 名 (小文字)。認証系以外は通さない。
const ALLOWED_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "x-illumia-setup-token",
    "x-vault-session",
];

#[derive(Clone)]
struct BoundServer {
    base: Url,
    instance_id: String,
}

#[derive(Default)]
pub struct ServerBinding {
    server: Mutex<Option<BoundServer>>,
}

impl ServerBinding {
    fn get(&self) -> Option<BoundServer> {
        self.server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// WebView からの再 bind を拒否する。接続先変更にはプロセス再起動が必要。
    fn bind_once(&self, candidate: BoundServer) -> Result<(), String> {
        let mut current = self
            .server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bound) = current.as_ref() {
            if bound.base == candidate.base && bound.instance_id == candidate.instance_id {
                return Ok(());
            }
            return Err(
                "接続先は既に固定されています。変更するにはアプリを再起動してください".into(),
            );
        }
        *current = Some(candidate);
        Ok(())
    }
}

/// device token は WebView へ返さず Rust process memory だけに置く。
/// Android Keystore 永続化を実装するまでは再起動時に再ログインする縮退動作となる。
#[derive(Default)]
pub struct NativeAuthState {
    token: Mutex<Option<Zeroizing<String>>>,
}

impl NativeAuthState {
    fn store_response_token(&self, bytes: &mut [u8]) -> Result<(), String> {
        let mut response: AuthTokenResponse = match serde_json::from_slice(bytes) {
            Ok(response) => response,
            Err(_) => {
                bytes.zeroize();
                return Err("認証応答の形式が不正です".into());
            }
        };
        if response.token.len() != 64
            || !response.token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            response.token.zeroize();
            bytes.zeroize();
            return Err("認証応答の形式が不正です".into());
        }
        let token = Zeroizing::new(std::mem::take(&mut response.token));
        let mut current = self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = Some(token);
        bytes.zeroize();
        Ok(())
    }

    fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let token = self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match token {
            Some(token) => builder.bearer_auth(token.as_str()),
            None => builder,
        }
    }

    fn clear(&self) {
        let mut current = self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = None;
    }
}

#[derive(Deserialize)]
struct AuthTokenResponse {
    token: String,
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

/// credential を付けない native probe の結果。
#[derive(Deserialize, Serialize)]
pub struct ProbeResponse {
    setup_completed: bool,
    authenticated: bool,
    setup_token_required: bool,
    instance_id: String,
}

/// private / loopback / link-local 宛か。平文 HTTP を許すのはこの範囲だけ。
fn is_private_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        Host::Ipv6(v6) => {
            v6.is_loopback()
                // fc00::/7 (ULA) と fe80::/10 (link-local)
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
        Host::Domain(name) => {
            let lower = name.to_ascii_lowercase();
            lower == "localhost" || lower.ends_with(".localhost")
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
    Ok(url)
}

/// path が `/api/` 配下の安全な相対参照か検証する。
fn validate_path(path: &str) -> Result<(), String> {
    let path_only = path.split_once('?').map_or(path, |(before, _)| before);
    if !path_only.starts_with("/api/") {
        return Err("許可されていないパスです".into());
    }
    if !path.is_ascii() {
        return Err("パスに非 ASCII 文字を含められません".into());
    }
    if path.chars().any(|c| c.is_ascii_control() || c == ' ') {
        return Err("パスに制御文字を含められません".into());
    }
    // path 部分の encoded dot/slash/backslash と二重 decode の起点になる encoded percent を
    // 拒否する。query の percent encoding は API 検索等で必要なので対象外。
    let bytes = path_only.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'%' {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                let decoded = high * 16 + low;
                if matches!(decoded, b'.' | b'/' | b'\\' | b'%') {
                    return Err("パスに encoded dot または separator を含められません".into());
                }
                index += 3;
                continue;
            }
        }
        index += 1;
    }
    // `..` による上位参照と backslash による parser 差異を防ぐ。
    if path_only.contains("..") || path_only.contains('\\') || path_only.starts_with("//") {
        return Err("パスに上位参照を含められません".into());
    }
    Ok(())
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// join 後にも origin と正規化済み `/api/` 境界を検査する。
fn resolve_target(base: &Url, path: &str) -> Result<Url, String> {
    validate_path(path)?;
    let target = base
        .join(path)
        .map_err(|_| "パスを解決できません".to_string())?;
    if target.scheme() != base.scheme()
        || target.host_str() != base.host_str()
        || target.port_or_known_default() != base.port_or_known_default()
    {
        return Err("登録済みサーバー以外へは接続できません".into());
    }
    if !target.path().starts_with("/api/") {
        return Err("正規化後のパスが API 境界外です".into());
    }
    Ok(target)
}

fn response_limit(path: &str) -> usize {
    if path.ends_with("/thumbnail") {
        MAX_THUMBNAIL_RESPONSE_BODY
    } else if path.ends_with("/preview") {
        MAX_PREVIEW_RESPONSE_BODY
    } else {
        MAX_API_RESPONSE_BODY
    }
}

fn is_original_api_path(path: &str) -> bool {
    let segments = path
        .strip_prefix('/')
        .map(|value| value.split('/').collect::<Vec<_>>())
        .unwrap_or_default();
    let asset_id = match segments.as_slice() {
        ["api", "assets", asset_id, "original"]
        | ["api", "vault", "assets", asset_id, "original"] => *asset_id,
        _ => return false,
    };
    is_uuid(asset_id)
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn validate_download_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.chars().count() > 255
        || value.len() > 512
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        || matches!(value, "." | "..")
    {
        return Err("保存ファイル名が不正です".into());
    }
    Ok(())
}

fn validated_download_headers(
    headers: &HashMap<String, String>,
) -> Result<reqwest::header::HeaderMap, String> {
    const DOWNLOAD_HEADERS: &[&str] = &["accept", "x-vault-session"];
    let mut validated = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if !DOWNLOAD_HEADERS.contains(&lower.as_str()) || value.chars().any(char::is_control) {
            return Err("許可されていないダウンロードヘッダです".into());
        }
        let name = reqwest::header::HeaderName::from_bytes(lower.as_bytes())
            .map_err(|_| "ダウンロードヘッダが不正です".to_string())?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| "ダウンロードヘッダが不正です".to_string())?;
        validated.insert(name, value);
    }
    Ok(validated)
}

struct CappedWriter<W> {
    inner: W,
    written: usize,
    limit: usize,
    overflowed: bool,
}

impl<W: Write> Write for CappedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self.written.checked_add(bytes.len());
        if !matches!(next, Some(length) if length <= self.limit) {
            self.overflowed = true;
            return Err(std::io::Error::other("download size limit exceeded"));
        }
        let written = self.inner.write(bytes)?;
        self.written += written;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

async fn read_capped_response(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("応答が大きすぎます".into());
    }
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let mut bytes = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "応答を読み取れませんでした".to_string())?
    {
        append_capped(&mut bytes, &chunk, limit)?;
    }
    Ok(bytes)
}

fn append_capped(bytes: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<(), String> {
    let next_len = bytes
        .len()
        .checked_add(chunk.len())
        .ok_or("応答が大きすぎます".to_string())?;
    if next_len > limit {
        return Err("応答が大きすぎます".into());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

fn decode_request_body(encoded: &str) -> Result<Vec<u8>, String> {
    let max_encoded_len = MAX_REQUEST_BODY.div_ceil(3) * 4;
    if encoded.len() > max_encoded_len {
        return Err("リクエストボディが大きすぎます".into());
    }
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| "リクエストボディが不正です".to_string())?;
    if bytes.len() > MAX_REQUEST_BODY {
        return Err("リクエストボディが大きすぎます".into());
    }
    Ok(bytes)
}

async fn probe_server(client: &reqwest::Client, base: &Url) -> Result<ProbeResponse, String> {
    const MAX_PROBE_BODY: usize = 64 * 1024;
    let target = resolve_target(base, "/api/server/info")?;
    let response = client
        .get(target)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "サーバーに接続できませんでした".to_string())?;
    if !response.status().is_success() {
        return Err("Illumia サーバー情報を取得できませんでした".into());
    }
    let bytes = read_capped_response(response, MAX_PROBE_BODY).await?;
    let info: ProbeResponse =
        serde_json::from_slice(&bytes).map_err(|_| "サーバー情報の形式が不正です".to_string())?;
    if !is_valid_instance_id(&info.instance_id) {
        return Err("サーバー識別子の形式が不正です".into());
    }
    Ok(info)
}

fn is_valid_instance_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// credential を一切付けずに候補サーバーの identity/schema を確認する。
#[tauri::command]
pub async fn illumia_probe_server(
    client: tauri::State<'_, reqwest::Client>,
    url: String,
) -> Result<ProbeResponse, String> {
    let base = validate_base_url(&url)?;
    probe_server(&client, &base).await
}

/// probe 済み identity を Rust 側でも再確認し、接続先をプロセス中固定する。
#[tauri::command]
pub async fn illumia_bind_server(
    app: tauri::AppHandle,
    binding: tauri::State<'_, ServerBinding>,
    client: tauri::State<'_, reqwest::Client>,
    url: String,
    instance_id: String,
) -> Result<(), String> {
    let base = validate_base_url(&url)?;
    let info = probe_server(&client, &base).await?;
    if info.instance_id != instance_id {
        return Err("登録済みのサーバーと異なるサーバーが応答しました".into());
    }
    if binding.get().is_none() {
        let displayed_origin = base.origin().ascii_serialization();
        let displayed_instance_id = instance_id.clone();
        let approved = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .message(format!(
                    "この Illumia サーバーへ接続しますか？\n\n接続先: {displayed_origin}\nサーバーID: {displayed_instance_id}\n\nこの確認は WebView から変更できません。"
                ))
                .title("Illumia サーバーの確認")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::YesNo)
                .blocking_show()
        })
        .await
        .map_err(|_| "接続先の確認を完了できませんでした".to_string())?;
        if !approved {
            return Err("接続先が承認されませんでした".into());
        }
    }
    binding.bind_once(BoundServer { base, instance_id })
}

/// 登録済みサーバーへ HTTP を発行する。宛先・method・header・body を検査する。
#[tauri::command]
pub async fn illumia_request(
    binding: tauri::State<'_, ServerBinding>,
    auth: tauri::State<'_, NativeAuthState>,
    client: tauri::State<'_, reqwest::Client>,
    request: BridgeRequest,
) -> Result<BridgeResponse, String> {
    let bound = binding
        .get()
        .ok_or("接続先サーバーが未設定です".to_string())?;

    let method = request
        .method
        .as_deref()
        .unwrap_or("GET")
        .to_ascii_uppercase();
    if !ALLOWED_METHODS.contains(&method.as_str()) {
        return Err("許可されていない HTTP メソッドです".into());
    }
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| "許可されていない HTTP メソッドです".to_string())?;

    // base を起点に解決し、結果が base と同一 origin であることを再確認する。
    let target = resolve_target(&bound.base, &request.path)?;
    if is_original_api_path(target.path()) {
        return Err("原本はネイティブ保存機能からダウンロードしてください".into());
    }
    let captures_auth_token = method == reqwest::Method::POST
        && matches!(target.path(), "/api/auth/setup" | "/api/auth/login");
    let clears_auth_token = method == reqwest::Method::POST && target.path() == "/api/auth/logout";
    let response_limit = response_limit(target.path());

    let body = match request.body_base64.as_deref() {
        Some(encoded) => Some(decode_request_body(encoded)?),
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
    // WebView から Authorization を受け取らず、Rust が保持する token をここでだけ付与する。
    builder = auth.apply(builder);

    let response = builder
        .send()
        .await
        .map_err(|_| "サーバーに接続できませんでした".to_string())?;

    let response_succeeded = response.status().is_success();
    let status = response.status().as_u16();
    let mut headers = HashMap::new();
    for (name, value) in response.headers() {
        // frontend が必要とするのは content-type 程度。Set-Cookie 等は渡さない。
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "content-type" {
            if let Ok(value) = value.to_str() {
                headers.insert(lower, value.to_owned());
            }
        }
    }

    let mut bytes = read_capped_response(response, response_limit).await?;
    if response_succeeded && captures_auth_token {
        auth.store_response_token(&mut bytes)?;
        // token を WebView IPC に返さない。frontend が必要なのは成功可否だけ。
        bytes.extend_from_slice(b"{}");
    } else if response_succeeded && clears_auth_token {
        auth.clear();
    }

    Ok(BridgeResponse {
        status,
        headers,
        body_base64: BASE64.encode(&bytes),
    })
}

/// 原本をBase64 IPCへ展開せず、利用者がnative dialogで選んだ保存先へ逐次書き込む。
/// pathはUUIDを含むmain/Vaultのoriginal endpointだけを受理する。
///
/// `vault: no-log`
#[tauri::command]
pub async fn illumia_download_original(
    app: tauri::AppHandle,
    binding: tauri::State<'_, ServerBinding>,
    auth: tauri::State<'_, NativeAuthState>,
    path: String,
    headers: HashMap<String, String>,
    filename: String,
) -> Result<bool, String> {
    validate_download_name(&filename)?;
    let bound = binding
        .get()
        .ok_or("接続先サーバーが未設定です".to_string())?;
    let target = resolve_target(&bound.base, &path)?;
    if !is_original_api_path(target.path()) {
        return Err("原本以外は保存機能から取得できません".into());
    }
    let headers = validated_download_headers(&headers)?;
    let token = auth
        .token
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();

    tauri::async_runtime::spawn_blocking(move || {
        let destination = match app
            .dialog()
            .file()
            .set_file_name(filename)
            .blocking_save_file()
        {
            Some(destination) => destination,
            None => return Ok(false),
        };

        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "ダウンロードを開始できませんでした".to_string())?;
        let mut request = client.get(target).headers(headers);
        if let Some(token) = token.as_ref() {
            request = request.bearer_auth(token.as_str());
        }
        let mut response = request
            .send()
            .map_err(|_| "サーバーに接続できませんでした".to_string())?;
        if !response.status().is_success() {
            return Err("原本をダウンロードできませんでした".into());
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ORIGINAL_DOWNLOAD as u64)
        {
            return Err("原本が大きすぎます".into());
        }

        let mut options = OpenOptions::new();
        options.read(false).write(true).create(true).truncate(true);
        let file = app
            .fs()
            .open(destination, options)
            .map_err(|_| "保存先を開けませんでした".to_string())?;
        let mut output = CappedWriter {
            inner: file,
            written: 0,
            limit: MAX_ORIGINAL_DOWNLOAD,
            overflowed: false,
        };
        if response.copy_to(&mut output).is_err() {
            let overflowed = output.overflowed;
            let _ = output.inner.set_len(0);
            return Err(if overflowed {
                "原本が大きすぎます".into()
            } else {
                "原本の保存中に通信が中断されました".into()
            });
        }
        if output.flush().is_err() {
            let _ = output.inner.set_len(0);
            return Err("原本を保存できませんでした".into());
        }
        Ok(true)
    })
    .await
    .map_err(|_| "原本の保存処理を完了できませんでした".to_string())?
}

/// logout の通信失敗時にも WebView から Rust 内 token を消去できる command。
#[tauri::command]
pub fn illumia_clear_auth(auth: tauri::State<'_, NativeAuthState>) {
    auth.clear();
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
        assert!(validate_base_url("http://illumia.localhost:2283").is_ok());
        assert!(validate_base_url("https://photos.local").is_ok());
        assert!(validate_base_url("http://photos.local").is_err());
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
        assert!(validate_path("/api/%2e%2e/admin").is_err());
        assert!(validate_path("/api/%2E%2E/admin").is_err());
        assert!(validate_path("/api/assets%2foriginal").is_err());
        assert!(validate_path("/api/assets%5Csecret").is_err());
        assert!(validate_path("/api/%252e%252e/admin").is_err());
        assert!(validate_path("//evil.example.com/api/x").is_err());
    }

    #[test]
    fn normalized_target_must_remain_under_api() {
        let base = Url::parse("https://illumia.example.com").unwrap();
        assert!(resolve_target(&base, "/api/server/info").is_ok());
        assert!(resolve_target(&base, "/api/%2e%2e/admin").is_err());
    }

    #[test]
    fn response_chunks_never_grow_buffer_past_limit() {
        let mut bytes = Vec::new();
        assert!(append_capped(&mut bytes, &[0_u8; 1024], 1024).is_ok());
        assert!(append_capped(&mut bytes, &[1], 1024).is_err());
        assert_eq!(bytes.len(), 1024);
    }

    #[test]
    fn oversized_base64_request_is_rejected_before_decode() {
        let max_encoded_len = MAX_REQUEST_BODY.div_ceil(3) * 4;
        // Invalid Base64 would report a format error if decode were reached. The size error proves
        // the amplification boundary is checked first.
        let encoded = "!".repeat(max_encoded_len + 1);
        let error = decode_request_body(&encoded).unwrap_err();
        assert_eq!(error, "リクエストボディが大きすぎます");
    }

    #[test]
    fn streamed_download_stops_before_writing_past_limit() {
        let mut output = CappedWriter {
            inner: Vec::new(),
            written: 0,
            limit: 4,
            overflowed: false,
        };
        output.write_all(b"safe").unwrap();
        assert!(output.write_all(b"x").is_err());
        assert!(output.overflowed);
        assert_eq!(output.inner, b"safe");
    }

    #[test]
    fn response_limits_are_endpoint_specific() {
        assert_eq!(response_limit("/api/server/info"), MAX_API_RESPONSE_BODY);
        assert_eq!(
            response_limit("/api/assets/id/thumbnail"),
            MAX_THUMBNAIL_RESPONSE_BODY
        );
        assert_eq!(
            response_limit("/api/assets/id/preview"),
            MAX_PREVIEW_RESPONSE_BODY
        );
        assert_eq!(
            response_limit("/api/assets/id/original"),
            MAX_API_RESPONSE_BODY,
            "original bytes must never use the Base64 response bridge"
        );
    }

    #[test]
    fn authorization_header_cannot_be_supplied_by_webview() {
        assert!(!ALLOWED_HEADERS.contains(&"authorization"));
        let headers = HashMap::from([("authorization".to_string(), "Bearer stolen".to_string())]);
        assert!(validated_download_headers(&headers).is_err());
    }

    #[test]
    fn auth_token_is_captured_and_source_buffer_is_zeroized() {
        let auth = NativeAuthState::default();
        let mut body =
            br#"{"token":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#
                .to_vec();
        auth.store_response_token(&mut body).unwrap();
        assert!(body.iter().all(|byte| *byte == 0));
        assert!(auth
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some());
        auth.clear();
        assert!(auth
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());
    }

    #[test]
    fn native_download_accepts_only_original_uuid_endpoints() {
        let id = "018f47b8-e760-7c35-b37d-047c61f4f5be";
        assert!(is_original_api_path(&format!("/api/assets/{id}/original")));
        assert!(is_original_api_path(&format!(
            "/api/vault/assets/{id}/original"
        )));
        assert!(!is_original_api_path("/api/server/info"));
        assert!(!is_original_api_path("/api/assets/not-a-uuid/original"));
        assert!(!is_original_api_path(&format!("/api/assets/{id}/preview")));
    }

    #[test]
    fn download_name_cannot_select_another_path() {
        assert!(validate_download_name("image.jpg").is_ok());
        assert!(validate_download_name("../image.jpg").is_err());
        assert!(validate_download_name("folder/image.jpg").is_err());
        assert!(validate_download_name("folder\\image.jpg").is_err());
    }

    #[test]
    fn unset_binding_is_rejected() {
        let binding = ServerBinding::default();
        assert!(binding.get().is_none());
    }

    #[test]
    fn binding_cannot_change_origin_or_identity() {
        let binding = ServerBinding::default();
        let first = BoundServer {
            base: Url::parse("https://illumia.example.com").unwrap(),
            instance_id: "instance-a".into(),
        };
        assert!(binding.bind_once(first.clone()).is_ok());
        assert!(binding.bind_once(first).is_ok());
        assert!(binding
            .bind_once(BoundServer {
                base: Url::parse("https://attacker.example").unwrap(),
                instance_id: "instance-a".into(),
            })
            .is_err());
        assert!(binding
            .bind_once(BoundServer {
                base: Url::parse("https://illumia.example.com").unwrap(),
                instance_id: "instance-b".into(),
            })
            .is_err());
    }

    #[test]
    fn private_hosts_are_recognized() {
        assert!(is_private_host(&Host::Ipv4("10.0.0.1".parse().unwrap())));
        assert!(is_private_host(&Host::Ipv4("192.168.1.1".parse().unwrap())));
        assert!(is_private_host(&Host::Ipv4("127.0.0.1".parse().unwrap())));
        assert!(!is_private_host(&Host::Ipv4("0.0.0.0".parse().unwrap())));
        assert!(!is_private_host(&Host::Ipv6("::".parse().unwrap())));
        assert!(!is_private_host(&Host::Ipv4("8.8.8.8".parse().unwrap())));
        assert!(is_private_host(&Host::Domain("localhost")));
        assert!(is_private_host(&Host::Domain("illumia.localhost")));
        assert!(!is_private_host(&Host::Domain("photos.local")));
        assert!(!is_private_host(&Host::Domain("example.com")));
    }

    #[test]
    fn instance_id_is_strictly_bounded_for_native_confirmation() {
        assert!(is_valid_instance_id("0123456789abcdef0123456789abcdef"));
        assert!(is_valid_instance_id("0123456789ABCDEF0123456789ABCDEF"));
        assert!(!is_valid_instance_id("instance-a"));
        assert!(!is_valid_instance_id("0123456789abcdef0123456789abcde\n"));
    }
}
