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

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use illumia_core::{
    assets::AssetService,
    db::Database,
    timeline::{Granularity, TimelineService},
};

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
        std::fs::read(&path).with_context(|| format!("画像を読めない: {}", path.display()))
    }
}

// ---- client-only ----

/// リモート server へ HTTP 接続する backend。
pub struct RemoteBackend {
    base_url: String,
    token: String,
    agent: ureq::Agent,
}

impl RemoteBackend {
    /// `base_url` は https のみ (平文 HTTP はループバックに限る)。
    pub fn connect(base_url: &str, token: &str) -> Result<Self> {
        let normalized = validate_base_url(base_url)?;
        Ok(Self {
            base_url: normalized,
            token: token.to_owned(),
            agent: ureq::Agent::new_with_defaults(),
        })
    }

    fn get(&self, path: &str) -> Result<Vec<u8>> {
        let mut response = self
            .agent
            .get(format!("{}{path}", self.base_url))
            .header("Authorization", &format!("Bearer {}", self.token))
            .call()
            .context("サーバーに接続できない")?;
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut response.body_mut().as_reader(), &mut body)
            .context("応答を読めない")?;
        Ok(body)
    }
}

/// 接続先 URL の検証 (docs/12: SEC-002 と同じ規則)。
fn validate_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("URL に制御文字または空白を含められない");
    }
    let rest = if let Some(rest) = trimmed.strip_prefix("https://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        if host != "localhost" && host != "127.0.0.1" && host != "[::1]" {
            bail!("平文 HTTP はループバックのみ使用できる");
        }
        rest
    } else {
        bail!("スキームは https のみ使用できる");
    };
    if rest.contains('@') {
        bail!("URL に認証情報を埋め込めない");
    }
    if rest.contains('?') || rest.contains('#') {
        bail!("URL にクエリ・フラグメントを指定できない");
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
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
        let body = self.get(&format!("/api/timeline/buckets?granularity={granularity}"))?;
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
        let body = self.get(&format!("/api/timeline/bucket/{bucket_key}"))?;
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
        let variant = match variant {
            Variant::Thumbnail => "thumbnail",
            Variant::Preview => "preview",
        };
        self.get(&format!("/api/assets/{asset_id}/{variant}"))
    }
}

/// 起動モード。
pub enum Mode {
    AllInOne { data_root: std::path::PathBuf },
    ClientOnly { base_url: String, token: String },
}

pub fn build(mode: Mode) -> Result<Arc<dyn Backend>> {
    Ok(match mode {
        Mode::AllInOne { data_root } => Arc::new(LocalBackend::open(data_root)?),
        Mode::ClientOnly { base_url, token } => {
            Arc::new(RemoteBackend::connect(&base_url, &token)?)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_base_url_requires_https_or_loopback() {
        assert!(validate_base_url("https://illumia.example.com").is_ok());
        assert!(validate_base_url("http://127.0.0.1:2283").is_ok());
        assert!(validate_base_url("http://illumia.example.com").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
    }

    #[test]
    fn remote_base_url_rejects_credentials_and_query() {
        assert!(validate_base_url("https://user:pass@example.com").is_err());
        assert!(validate_base_url("https://example.com?a=1").is_err());
        assert!(validate_base_url("https://example.com#x").is_err());
    }

    #[test]
    fn local_backend_never_uses_the_network() {
        let dir = std::env::temp_dir().join(format!("illumia-desktop-{}", std::process::id()));
        let backend = LocalBackend::open(&dir).expect("ローカル backend を開ける");
        assert!(!backend.uses_network());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
