//! Vault の鍵管理、SQLCipher DB、暗号化 blob、および DB 間移動。
//!
//! `vault: no-log` — このモジュールではファイル名・asset id・鍵素材をログへ出さない。

use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use chrono::{DateTime, Utc};
use hkdf::Hkdf;
use rand::{TryRng, rngs::SysRng};
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    assets::{Asset, AssetService, IngestResult, Lifecycle, add_days, timestamp},
    db::{Database, Error, Result, create_private_dir_all, set_private_file_permissions},
    images, settings, thumbnails,
};

const KEYFILE_VERSION: u32 = 1;
const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const MAGIC: &[u8; 5] = b"ILMV1";
const NONCE_PREFIX_LEN: usize = 16;
const BLOB_HEADER_LEN: usize = MAGIC.len() + NONCE_PREFIX_LEN + 4;
/// Vault blob の暗号化・復号 API が一度に保持する最大平文 chunk サイズ。
pub const VAULT_BLOB_CHUNK_SIZE: usize = 1024 * 1024;
const CHUNK_SIZE: usize = VAULT_BLOB_CHUNK_SIZE;
const TRANSFER_STAGING_DIR: &str = "transfer-staging";
const MAX_KEYFILE_BYTES: u64 = 64 * 1024;
const MAX_ENCRYPTED_BLOB_BYTES: u64 = images::MAX_ASSET_BYTES as u64 + 1024 * 1024;
pub const MAX_VAULT_TRANSFER_ASSETS: usize = 100;
const MAX_VAULT_TRANSFER_BYTES: usize = 512 * 1024 * 1024;
const MAX_VAULT_PASSWORD_BYTES: usize = 1024;
const MAX_KDF_MEMORY_KIB: u32 = 256 * 1024;
const MAX_KDF_ITERATIONS: u32 = 10;
const MAX_KDF_PARALLELISM: u32 = 16;

/// Argon2id の keyfile 記録パラメータ。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            memory_kib: 64 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }
}

impl KdfParams {
    /// 高速なテスト専用設定。永続データには [`Default`] を使う。
    #[must_use]
    pub const fn for_tests() -> Self {
        Self {
            memory_kib: 64,
            iterations: 1,
            parallelism: 1,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct WrappedRecord {
    nonce: String,
    ciphertext: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct KeyFile {
    version: u32,
    kdf: KdfParams,
    salt: String,
    password: WrappedRecord,
    recovery: WrappedRecord,
}

/// メモリ破棄時に master key をゼロ化する Vault 鍵。
#[derive(Clone)]
pub struct VaultKey {
    inner: Arc<VaultKeyInner>,
}

struct VaultKeyInner {
    master_key: Mutex<Zeroizing<[u8; KEY_LEN]>>,
    revoked: AtomicBool,
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VaultKey([REDACTED])")
    }
}

impl VaultKey {
    fn new(master_key: [u8; KEY_LEN]) -> Self {
        Self {
            inner: Arc::new(VaultKeyInner {
                master_key: Mutex::new(Zeroizing::new(master_key)),
                revoked: AtomicBool::new(false),
            }),
        }
    }

    fn derive(&self, info: &[u8]) -> Result<[u8; KEY_LEN]> {
        self.with_master_key(|master_key| {
            let hkdf = Hkdf::<Sha256>::new(None, master_key);
            let mut output = [0_u8; KEY_LEN];
            hkdf.expand(info, &mut output)
                .map_err(|_| Error::VaultCrypto)?;
            Ok(output)
        })
    }

    fn with_master_key<T>(&self, operation: impl FnOnce(&[u8; KEY_LEN]) -> Result<T>) -> Result<T> {
        self.ensure_active()?;
        let master_key = self
            .inner
            .master_key
            .lock()
            .map_err(|_| Error::VaultCrypto)?;
        self.ensure_active()?;
        operation(&master_key)
    }

    fn ensure_active(&self) -> Result<()> {
        if self.inner.revoked.load(AtomicOrdering::Acquire) {
            Err(Error::VaultAuthenticationFailed)
        } else {
            Ok(())
        }
    }

    fn revoke(&self) {
        self.inner.revoked.store(true, AtomicOrdering::Release);
        if let Ok(mut master_key) = self.inner.master_key.lock() {
            master_key.zeroize();
        }
    }
}

/// SQLCipher DB と、その DB/blobs を復号できる master key の寿命を束ねる。
#[derive(Clone, Debug)]
pub struct VaultHandle {
    pub db: Database,
    pub key: VaultKey,
}

/// 1 chunk ずつ AEAD 検証して返す Vault blob reader。
///
/// `vault: no-log`
pub struct VaultBlobReader {
    file: fs::File,
    blob_id: String,
    file_key: Zeroizing<[u8; KEY_LEN]>,
    header: [u8; BLOB_HEADER_LEN],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    encrypted_remaining: u64,
    plaintext_total: usize,
    index: usize,
    finished: bool,
    key: VaultKey,
}

impl VaultHandle {
    /// Revokes this handle and every clone sharing its in-memory master key.
    pub fn revoke(&self) {
        self.key.revoke();
    }

    /// Fails after the unlock session owning this handle has been revoked.
    pub fn ensure_active(&self) -> Result<()> {
        self.key.ensure_active()
    }

    /// unlock 済み鍵で vault DB を開く。
    ///
    /// `vault: no-log`
    pub fn open(data_root: impl AsRef<Path>, key: VaultKey) -> Result<Self> {
        let sqlcipher_key = Zeroizing::new(key.derive(b"vault-db")?);
        let db = Database::open_vault(data_root.as_ref(), &sqlcipher_key)?;
        let handle = Self { db, key };
        handle.reconcile_direct_ingests()?;
        handle.cleanup_orphan_staging()?;
        Ok(handle)
    }

    /// password unlock と DB open をまとめて行う。
    ///
    /// `vault: no-log`
    pub fn unlock(data_root: impl AsRef<Path>, password: &str) -> Result<Self> {
        let data_root = data_root.as_ref();
        Self::open(data_root, unlock(data_root, password)?)
    }

    /// recovery key unlock と DB open をまとめて行う。
    ///
    /// `vault: no-log`
    pub fn unlock_with_recovery(data_root: impl AsRef<Path>, recovery_key: &str) -> Result<Self> {
        let data_root = data_root.as_ref();
        Self::open(data_root, unlock_with_recovery(data_root, recovery_key)?)
    }

    /// standalone blob を暗号化して保存する。
    ///
    /// `vault: no-log`
    pub fn write_blob(&self, bytes: &[u8]) -> Result<String> {
        self.write_blob_for(bytes, BlobKind::Standalone, None)
    }

    /// main library を経由せず、最初の persistent write を暗号化 blob として取り込む。
    ///
    /// `vault: no-log`
    pub fn ingest(
        &self,
        bytes: &[u8],
        original_name: &str,
        taken_at: Option<DateTime<Utc>>,
    ) -> Result<IngestResult> {
        self.ingest_at(bytes, original_name, taken_at, Utc::now())
    }

    /// 時刻を固定できる direct encrypted ingest。テストと migration 用。
    ///
    /// `vault: no-log`
    pub fn ingest_at(
        &self,
        bytes: &[u8],
        original_name: &str,
        taken_at: Option<DateTime<Utc>>,
        uploaded_at: DateTime<Utc>,
    ) -> Result<IngestResult> {
        self.ensure_active()?;
        let ext = images::normalized_extension(original_name)?;
        let (width, height) = images::validate_and_dimensions(bytes, &ext)?;
        let hash = blake3::hash(bytes);
        let asset_id = Uuid::now_v7().to_string();
        let transfer_id = Uuid::now_v7().to_string();
        let taken_at = taken_at.unwrap_or(uploaded_at);
        let staging_dir = transfer_staging_dir(self.db.data_root(), &transfer_id)?;
        create_private_dir_all(&staging_dir)?;
        let mut reader = Cursor::new(bytes);
        let prepared_blob = prepare_reader_blob(
            self,
            &mut reader,
            bytes.len(),
            BlobKind::Original,
            &asset_id,
            &staging_dir,
            Some((
                i64::try_from(bytes.len()).map_err(|_| Error::InvalidVaultBlob)?,
                hash.as_bytes(),
            )),
        )?;
        self.ensure_active()?;
        let blob_id = prepared_blob.id.clone();
        let result = self.db.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let duplicate_of = transaction
                .query_row(
                    "SELECT id FROM assets
                     WHERE hash = ?1 AND lifecycle = 'active' AND duplicate_of IS NULL
                       AND NOT EXISTS (
                         SELECT 1 FROM vault_transfers transfer, json_each(transfer.asset_ids) item
                         WHERE transfer.role = 'source' AND item.value = assets.id
                       )",
                    [hash.as_bytes().as_slice()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let lifecycle = if duplicate_of.is_some() {
                "duplicate"
            } else {
                "active"
            };
            let purge_after = if duplicate_of.is_some() {
                Some(add_days(
                    uploaded_at,
                    settings::dedup_retention_days(&transaction)?,
                ))
            } else {
                None
            };
            transaction.execute(
                "INSERT INTO assets(
                   id, hash, original_name, ext, size, width, height,
                   taken_at, taken_at_local_date, uploaded_at, thumbhash,
                   in_timeline, visible_in_timeline, lifecycle, duplicate_of,
                   trashed_at, purge_after, library_path
                 ) VALUES (
                   ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL,
                   1, ?11, ?12, ?13, NULL, ?14, ?15
                 )",
                params![
                    asset_id,
                    hash.as_bytes().as_slice(),
                    original_name,
                    ext,
                    i64::try_from(bytes.len()).map_err(|_| Error::InvalidVaultBlob)?,
                    i64::from(width),
                    i64::from(height),
                    timestamp(taken_at),
                    taken_at.format("%Y-%m-%d").to_string(),
                    timestamp(uploaded_at),
                    i64::from(duplicate_of.is_none()),
                    lifecycle,
                    duplicate_of,
                    purge_after,
                    blob_id,
                ],
            )?;
            transaction.execute(
                "INSERT INTO vault_blobs(blob_id, wrapped_key, kind, asset_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    prepared_blob.id,
                    prepared_blob.wrapped_key,
                    prepared_blob.kind.as_str(),
                    prepared_blob.asset_id,
                ],
            )?;
            insert_transfer_record(
                &transaction,
                &transfer_id,
                TransferDirection::DirectIngest,
                "destination",
                std::slice::from_ref(&asset_id),
                TransferState::DestinationReady,
            )?;
            transaction.commit()?;
            Ok(duplicate_of)
        });
        let duplicate_of = match result {
            Ok(value) => value,
            Err(error) => {
                remove_transfer_staging(self.db.data_root(), &transfer_id)?;
                return Err(error);
            }
        };
        self.complete_direct_ingest(&transfer_id, std::slice::from_ref(&asset_id))?;
        self.ensure_active()?;
        let asset = AssetService::new(self.db.clone())
            .get(&asset_id)?
            .ok_or(Error::IncompleteVaultTransfer)?;
        Ok(IngestResult {
            asset,
            duplicate_of,
        })
    }

    /// blob を逐次復号する reader を開く。
    ///
    /// `vault: no-log`
    pub fn blob_reader(&self, blob_id: &str) -> Result<VaultBlobReader> {
        let path = self.blob_path(blob_id)?;
        let (wrapped_key, kind, asset_id) = self.db.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT wrapped_key, kind, asset_id
                     FROM vault_blobs WHERE blob_id = ?1",
                    [blob_id],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(Error::VaultBlobNotFound)
        })?;
        let aad = blob_key_aad(blob_id, &kind, asset_id.as_deref());
        let file_key = Zeroizing::new(
            self.key
                .with_master_key(|master_key| unwrap_bytes(master_key, &wrapped_key, &aad))?,
        );
        let mut file = fs::File::open(path)?;
        let encrypted_len = file.metadata()?.len();
        let minimum_len =
            u64::try_from(BLOB_HEADER_LEN + 5 + TAG_LEN).map_err(|_| Error::InvalidVaultBlob)?;
        if !(minimum_len..=MAX_ENCRYPTED_BLOB_BYTES).contains(&encrypted_len) {
            return Err(Error::InvalidVaultBlob);
        }

        let mut header = [0_u8; BLOB_HEADER_LEN];
        file.read_exact(&mut header)
            .map_err(|_| Error::InvalidVaultBlob)?;
        if &header[..MAGIC.len()] != MAGIC {
            return Err(Error::InvalidVaultBlob);
        }
        let mut nonce_prefix = [0_u8; NONCE_PREFIX_LEN];
        nonce_prefix.copy_from_slice(&header[MAGIC.len()..MAGIC.len() + NONCE_PREFIX_LEN]);
        let chunk_size = u32::from_be_bytes(
            header[MAGIC.len() + NONCE_PREFIX_LEN..]
                .try_into()
                .map_err(|_| Error::InvalidVaultBlob)?,
        );
        if usize::try_from(chunk_size).map_err(|_| Error::InvalidVaultBlob)? != CHUNK_SIZE {
            return Err(Error::InvalidVaultBlob);
        }

        Ok(VaultBlobReader {
            file,
            blob_id: blob_id.to_owned(),
            file_key,
            header,
            nonce_prefix,
            encrypted_remaining: encrypted_len
                .checked_sub(u64::try_from(BLOB_HEADER_LEN).map_err(|_| Error::InvalidVaultBlob)?)
                .ok_or(Error::InvalidVaultBlob)?,
            plaintext_total: 0,
            index: 0,
            finished: false,
            key: self.key.clone(),
        })
    }

    /// 内部処理向けに blob 全体をメモリへ復号する。
    ///
    /// `vault: no-log`
    pub fn read_blob(&self, blob_id: &str) -> Result<Vec<u8>> {
        let mut plaintext = Vec::new();
        for chunk in self.blob_reader(blob_id)? {
            plaintext.extend_from_slice(&chunk?);
        }
        Ok(plaintext)
    }

    /// Vault 内原本から 240px/1440px WebP をメモリ内生成して暗号化保存する。
    ///
    /// `vault: no-log`
    pub fn generate_thumbnails(&self, asset_id: &str) -> Result<()> {
        self.ensure_active()?;
        let extension = AssetService::new(self.db.clone())
            .get(asset_id)?
            .ok_or(Error::AssetNotFound)?
            .ext;
        let original_blob = self
            .blob_id_for(asset_id, BlobKind::Original)?
            .ok_or(Error::VaultBlobNotFound)?;
        let source = Zeroizing::new(self.read_blob(&original_blob)?);
        let variants = thumbnails::generate_variants_in_memory(&source, &extension)?;

        if self.blob_id_for(asset_id, BlobKind::Thumbnail)?.is_none() {
            self.write_blob_for(&variants.thumbnail, BlobKind::Thumbnail, Some(asset_id))?;
        }
        if self.blob_id_for(asset_id, BlobKind::Preview)?.is_none() {
            self.write_blob_for(&variants.preview, BlobKind::Preview, Some(asset_id))?;
        }
        self.ensure_active()?;
        self.db.with_connection(|connection| {
            connection.execute(
                "UPDATE assets SET thumbhash = ?2 WHERE id = ?1",
                params![asset_id, variants.thumbhash],
            )?;
            Ok(())
        })
    }

    fn blob_id_for(&self, asset_id: &str, kind: BlobKind) -> Result<Option<String>> {
        self.db.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT blob_id FROM vault_blobs
                     WHERE asset_id = ?1 AND kind = ?2",
                    params![asset_id, kind.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Into::into)
        })
    }

    fn write_blob_for(
        &self,
        bytes: &[u8],
        kind: BlobKind,
        asset_id: Option<&str>,
    ) -> Result<String> {
        if bytes.len() > images::MAX_ASSET_BYTES {
            return Err(Error::InvalidVaultBlob);
        }
        let blob_id = Uuid::now_v7().to_string();
        let file_key = Zeroizing::new(random_array()?);
        let aad = blob_key_aad(&blob_id, kind.as_str(), asset_id);
        let wrapped_key = self
            .key
            .with_master_key(|master_key| wrap_bytes(master_key, file_key.as_slice(), &aad))?;
        let path = self.blob_path(&blob_id)?;
        let mut output = create_new_private_file(&path)?;
        let mut input = Cursor::new(bytes);
        encrypt_reader_to_writer(
            &blob_id,
            &file_key,
            &mut input,
            &mut output,
            bytes.len(),
            Some(&self.key),
        )?;
        output.sync_all()?;
        let inserted = self.db.with_connection(|connection| {
            connection.execute(
                "INSERT INTO vault_blobs(blob_id, wrapped_key, kind, asset_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![blob_id, wrapped_key, kind.as_str(), asset_id],
            )?;
            Ok(())
        });
        if inserted.is_err() {
            remove_if_exists(&path)?;
        }
        inserted?;
        Ok(blob_id)
    }

    fn complete_direct_ingest(&self, transfer_id: &str, asset_ids: &[String]) -> Result<()> {
        self.ensure_active()?;
        finalize_import_files(self, transfer_id, asset_ids)?;
        self.ensure_active()?;
        validate_import_destination(self, asset_ids)?;
        delete_transfer_record(&self.db, transfer_id)?;
        self.db.checkpoint_truncate()?;
        remove_transfer_staging(self.db.data_root(), transfer_id)
    }

    fn reconcile_direct_ingests(&self) -> Result<()> {
        for journal in destination_transfers(&self.db, TransferDirection::DirectIngest)? {
            self.complete_direct_ingest(&journal.id, &journal.asset_ids)?;
        }
        Ok(())
    }

    fn cleanup_orphan_staging(&self) -> Result<()> {
        let referenced = transfer_ids(&self.db)?;
        let root = self.db.data_root().join("vault").join(TRANSFER_STAGING_DIR);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let transfer_id = entry
                .file_name()
                .to_str()
                .ok_or(Error::InvalidAssetPath)?
                .to_owned();
            Uuid::parse_str(&transfer_id).map_err(|_| Error::InvalidAssetPath)?;
            if !entry.file_type()?.is_dir() {
                return Err(Error::InvalidAssetPath);
            }
            if !referenced.contains(&transfer_id) {
                remove_transfer_staging(self.db.data_root(), &transfer_id)?;
            }
        }
        Ok(())
    }

    fn blob_path(&self, blob_id: &str) -> Result<PathBuf> {
        Uuid::parse_str(blob_id).map_err(|_| Error::InvalidVaultBlob)?;
        Ok(self
            .db
            .data_root()
            .join("vault")
            .join("blobs")
            .join(blob_id))
    }
}

impl VaultBlobReader {
    fn read_chunk(&mut self) -> Result<Vec<u8>> {
        self.key.ensure_active()?;
        if self.encrypted_remaining < 5 {
            return Err(Error::InvalidVaultBlob);
        }
        let mut record_header = [0_u8; 5];
        self.file
            .read_exact(&mut record_header)
            .map_err(|_| Error::InvalidVaultBlob)?;
        self.encrypted_remaining -= 5;

        let final_chunk = match record_header[0] {
            0 => false,
            1 => true,
            _ => return Err(Error::InvalidVaultBlob),
        };
        let ciphertext_len = usize::try_from(u32::from_be_bytes(
            record_header[1..]
                .try_into()
                .map_err(|_| Error::InvalidVaultBlob)?,
        ))
        .map_err(|_| Error::InvalidVaultBlob)?;
        if !(TAG_LEN..=CHUNK_SIZE + TAG_LEN).contains(&ciphertext_len)
            || u64::try_from(ciphertext_len).map_err(|_| Error::InvalidVaultBlob)?
                > self.encrypted_remaining
        {
            return Err(Error::InvalidVaultBlob);
        }

        let mut ciphertext = vec![0_u8; ciphertext_len];
        self.file
            .read_exact(&mut ciphertext)
            .map_err(|_| Error::InvalidVaultBlob)?;
        self.encrypted_remaining -=
            u64::try_from(ciphertext_len).map_err(|_| Error::InvalidVaultBlob)?;
        let nonce = chunk_nonce(&self.nonce_prefix, self.index)?;
        let aad = chunk_aad(&self.blob_id, &self.header, self.index, final_chunk)?;
        let plaintext = aead_decrypt(&self.file_key, &nonce, &ciphertext, &aad)
            .map_err(|_| Error::InvalidVaultBlob)?;
        if (!final_chunk && plaintext.len() != CHUNK_SIZE)
            || (final_chunk && self.encrypted_remaining != 0)
        {
            return Err(Error::InvalidVaultBlob);
        }
        self.plaintext_total = self
            .plaintext_total
            .checked_add(plaintext.len())
            .ok_or(Error::InvalidVaultBlob)?;
        if self.plaintext_total > images::MAX_ASSET_BYTES {
            return Err(Error::InvalidVaultBlob);
        }

        if final_chunk {
            self.finished = true;
        } else {
            self.index = self.index.checked_add(1).ok_or(Error::InvalidVaultBlob)?;
        }
        Ok(plaintext)
    }
}

impl Iterator for VaultBlobReader {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let result = self.read_chunk();
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

struct PreparedAsset {
    asset: Asset,
    original: PreparedSource,
    thumbnail: Option<PreparedSource>,
    preview: Option<PreparedSource>,
}

enum PreparedSource {
    PlainFile(PathBuf),
    VaultBlob(String),
}

#[derive(Clone)]
struct StackRecord {
    id: String,
    title: String,
    cover_asset_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone)]
struct ChapterRecord {
    id: String,
    stack_id: String,
    chapter_no: i64,
    title: Option<String>,
}

#[derive(Clone)]
struct PageRecord {
    stack_id: String,
    chapter_id: String,
    asset_id: String,
    page_no: i64,
    show_in_timeline: bool,
}

#[derive(Clone)]
struct ClusterRecord {
    id: String,
    name: Option<String>,
    cover_face_id: Option<String>,
    created_by: String,
    created_at: String,
}

#[derive(Clone)]
struct FaceRecord {
    id: String,
    asset_id: String,
    kind: String,
    bbox: String,
    det_conf: f64,
    quality_flags: String,
    embedding: Option<Vec<u8>>,
    model_version: String,
    cluster_id: Option<String>,
    state: String,
    similarity: Option<f64>,
}

#[derive(Clone)]
struct RejectionRecord {
    face_id: String,
    cluster_id: String,
}

#[derive(Default)]
struct Relations {
    stacks: Vec<StackRecord>,
    chapters: Vec<ChapterRecord>,
    pages: Vec<PageRecord>,
    clusters: Vec<ClusterRecord>,
    faces: Vec<FaceRecord>,
    rejections: Vec<RejectionRecord>,
}

struct PreparedBlob {
    id: String,
    wrapped_key: Vec<u8>,
    kind: BlobKind,
    asset_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferDirection {
    Import,
    Export,
    DirectIngest,
}

impl TransferDirection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Export => "export",
            Self::DirectIngest => "direct_ingest",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum TransferState {
    Preparing,
    DestinationReady,
    SourceFilesDeleted,
    SourceDatabaseDeleted,
}

impl TransferState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::DestinationReady => "destination_ready",
            Self::SourceFilesDeleted => "source_files_deleted",
            Self::SourceDatabaseDeleted => "source_database_deleted",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "preparing" => Ok(Self::Preparing),
            "destination_ready" => Ok(Self::DestinationReady),
            "source_files_deleted" => Ok(Self::SourceFilesDeleted),
            "source_database_deleted" => Ok(Self::SourceDatabaseDeleted),
            _ => Err(Error::IncompleteVaultTransfer),
        }
    }
}

struct TransferJournal {
    id: String,
    asset_ids: Vec<String>,
    state: TransferState,
}

/// メイン DB の指定 asset を Vault へ完全移動する。
///
/// `vault: no-log`
pub fn import_assets(main_db: &Database, vault: &VaultHandle, asset_ids: &[String]) -> Result<()> {
    vault.ensure_active()?;
    reconcile_transfers(main_db, vault)?;
    if assets_moved(main_db, &vault.db, asset_ids)? {
        return Ok(());
    }
    import_assets_inner(main_db, vault, asset_ids, &mut |_| Ok(()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImportPhase {
    SourcePrepared,
    VaultStaged,
    VaultCommitted,
    PlainFilesDeleted,
    PlainDatabaseDeleted,
}

fn import_assets_inner(
    main_db: &Database,
    vault: &VaultHandle,
    asset_ids: &[String],
    observer: &mut impl FnMut(ImportPhase) -> Result<()>,
) -> Result<()> {
    validate_transfer_ids(asset_ids)?;
    let assets = prepare_main_assets(main_db, asset_ids)?;
    let relations = collect_relations(main_db, asset_ids)?;
    let transfer_id = Uuid::now_v7().to_string();
    insert_source_transfer(main_db, &transfer_id, TransferDirection::Import, asset_ids)?;
    if let Err(error) = observer(ImportPhase::SourcePrepared) {
        delete_transfer_record(main_db, &transfer_id)?;
        return Err(error);
    }
    if let Err(error) = stage_vault_import(vault, &assets, &relations, &transfer_id, observer) {
        remove_transfer_staging(vault.db.data_root(), &transfer_id)?;
        delete_transfer_record(main_db, &transfer_id)?;
        return Err(error);
    }
    vault.ensure_active()?;
    observer(ImportPhase::VaultCommitted)?;
    let journal = TransferJournal {
        id: transfer_id,
        asset_ids: asset_ids.to_vec(),
        state: TransferState::Preparing,
    };
    complete_import_transfer(main_db, vault, &journal, observer)
}

/// 漫画スタックと全ページをまとめて Vault へ移動する。
///
/// `vault: no-log`
pub fn import_stack(main_db: &Database, vault: &VaultHandle, stack_id: &str) -> Result<()> {
    let asset_ids = stack_asset_ids(main_db, stack_id)?;
    if asset_ids.is_empty() {
        return Err(Error::StackNotFound);
    }
    import_assets(main_db, vault, &asset_ids)
}

/// Vault の指定 asset をメイン library へ完全移動する。
///
/// `vault: no-log`
pub fn export_assets(vault: &VaultHandle, main_db: &Database, asset_ids: &[String]) -> Result<()> {
    vault.ensure_active()?;
    validate_transfer_ids(asset_ids)?;
    // A normal export must be able to decrypt/write UUID-owned files before contending on the
    // process-wide main SQLite mutex. Only an interrupted prior export needs reconciliation first.
    if !source_transfers(&vault.db, TransferDirection::Export)?.is_empty() {
        reconcile_transfers(main_db, vault)?;
    }
    let source_present = asset_ids.iter().all(|asset_id| {
        AssetService::new(vault.db.clone())
            .get(asset_id)
            .is_ok_and(|asset| asset.is_some())
    });
    if source_present {
        return export_assets_inner(vault, main_db, asset_ids, &mut |_| Ok(()));
    }
    if assets_moved(&vault.db, main_db, asset_ids)? {
        return Ok(());
    }
    export_assets_inner(vault, main_db, asset_ids, &mut |_| Ok(()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExportPhase {
    SourcePrepared,
    MainStaged,
    MainCommitted,
    VaultFilesDeleted,
    VaultDatabaseDeleted,
}

fn export_assets_inner(
    vault: &VaultHandle,
    main_db: &Database,
    asset_ids: &[String],
    observer: &mut impl FnMut(ExportPhase) -> Result<()>,
) -> Result<()> {
    validate_transfer_ids(asset_ids)?;
    let assets = prepare_vault_assets(vault, asset_ids)?;
    let relations = collect_relations(&vault.db, asset_ids)?;
    let transfer_id = Uuid::now_v7().to_string();
    insert_source_transfer(
        &vault.db,
        &transfer_id,
        TransferDirection::Export,
        asset_ids,
    )?;
    if let Err(error) = observer(ExportPhase::SourcePrepared) {
        delete_transfer_record(&vault.db, &transfer_id)?;
        return Err(error);
    }
    if let Err(error) =
        stage_main_export(vault, main_db, &assets, &relations, &transfer_id, observer)
    {
        delete_transfer_record(&vault.db, &transfer_id)?;
        return Err(error);
    }
    vault.ensure_active()?;
    observer(ExportPhase::MainCommitted)?;
    let journal = TransferJournal {
        id: transfer_id,
        asset_ids: asset_ids.to_vec(),
        state: TransferState::Preparing,
    };
    complete_export_transfer(vault, main_db, &journal, observer)
}

/// Durable journal を照合し、commit 済み transfer は destination へ収束させる。
///
/// Vault unlock 後と import/export 再試行前に呼ぶ。`vault: no-log`
pub fn reconcile_transfers(main_db: &Database, vault: &VaultHandle) -> Result<()> {
    for journal in source_transfers(main_db, TransferDirection::Import)? {
        if destination_transfer_exists(&vault.db, &journal.id, TransferDirection::Import)? {
            complete_import_transfer(main_db, vault, &journal, &mut |_| Ok(()))?;
        } else {
            remove_transfer_staging(vault.db.data_root(), &journal.id)?;
            delete_transfer_record(main_db, &journal.id)?;
        }
    }
    for journal in source_transfers(&vault.db, TransferDirection::Export)? {
        if destination_transfer_exists(main_db, &journal.id, TransferDirection::Export)? {
            complete_export_transfer(vault, main_db, &journal, &mut |_| Ok(()))?;
        } else {
            rollback_export_files(vault, main_db, &journal.asset_ids)?;
            delete_transfer_record(&vault.db, &journal.id)?;
        }
    }
    vault.reconcile_direct_ingests()?;
    Ok(())
}

fn complete_import_transfer(
    main_db: &Database,
    vault: &VaultHandle,
    journal: &TransferJournal,
    observer: &mut impl FnMut(ImportPhase) -> Result<()>,
) -> Result<()> {
    vault.ensure_active()?;
    if !destination_transfer_exists(&vault.db, &journal.id, TransferDirection::Import)? {
        return Err(Error::IncompleteVaultTransfer);
    }
    finalize_import_files(vault, &journal.id, &journal.asset_ids)?;
    vault.ensure_active()?;
    validate_import_destination(vault, &journal.asset_ids)?;
    if journal.state < TransferState::DestinationReady {
        update_transfer_state(main_db, &journal.id, TransferState::DestinationReady)?;
    }

    if journal.state < TransferState::SourceFilesDeleted {
        vault.ensure_active()?;
        lease_transfer_source_deletion(main_db, &journal.asset_ids)?;
        vault.ensure_active()?;
        delete_plain_source_files(main_db, &vault.db, &journal.asset_ids)?;
        update_transfer_state(main_db, &journal.id, TransferState::SourceFilesDeleted)?;
    }
    observer(ImportPhase::PlainFilesDeleted)?;

    if journal.state < TransferState::SourceDatabaseDeleted {
        vault.ensure_active()?;
        delete_assets_from_database_idempotent(main_db, &journal.asset_ids)?;
        update_transfer_state(main_db, &journal.id, TransferState::SourceDatabaseDeleted)?;
    }
    observer(ImportPhase::PlainDatabaseDeleted)?;
    main_db.checkpoint_truncate()?;
    delete_transfer_record(&vault.db, &journal.id)?;
    vault.db.checkpoint_truncate()?;
    delete_transfer_record(main_db, &journal.id)?;
    main_db.checkpoint_truncate()?;
    remove_transfer_staging(vault.db.data_root(), &journal.id)
}

fn complete_export_transfer(
    vault: &VaultHandle,
    main_db: &Database,
    journal: &TransferJournal,
    observer: &mut impl FnMut(ExportPhase) -> Result<()>,
) -> Result<()> {
    vault.ensure_active()?;
    if !destination_transfer_exists(main_db, &journal.id, TransferDirection::Export)? {
        return Err(Error::IncompleteVaultTransfer);
    }
    validate_export_destination(main_db, &journal.asset_ids)?;
    if journal.state < TransferState::DestinationReady {
        update_transfer_state(&vault.db, &journal.id, TransferState::DestinationReady)?;
    }

    if journal.state < TransferState::SourceFilesDeleted {
        vault.ensure_active()?;
        lease_transfer_source_deletion(&vault.db, &journal.asset_ids)?;
        for blob_id in vault_blob_ids(&vault.db, &journal.asset_ids)? {
            vault.ensure_active()?;
            remove_if_exists(&vault.blob_path(&blob_id)?)?;
        }
        update_transfer_state(&vault.db, &journal.id, TransferState::SourceFilesDeleted)?;
    }
    observer(ExportPhase::VaultFilesDeleted)?;

    if journal.state < TransferState::SourceDatabaseDeleted {
        vault.ensure_active()?;
        delete_assets_from_database_idempotent(&vault.db, &journal.asset_ids)?;
        update_transfer_state(&vault.db, &journal.id, TransferState::SourceDatabaseDeleted)?;
    }
    observer(ExportPhase::VaultDatabaseDeleted)?;
    vault.db.checkpoint_truncate()?;
    delete_transfer_record(main_db, &journal.id)?;
    main_db.checkpoint_truncate()?;
    delete_transfer_record(&vault.db, &journal.id)?;
    vault.db.checkpoint_truncate()
}

fn insert_source_transfer(
    database: &Database,
    transfer_id: &str,
    direction: TransferDirection,
    asset_ids: &[String],
) -> Result<()> {
    database.with_connection_mut(|connection| {
        let transaction = connection.transaction()?;
        validate_duplicate_closure(&transaction, asset_ids)?;
        transaction.execute(
            "INSERT INTO vault_transfers(id, direction, role, asset_ids, state, created_at)
             VALUES (?1, ?2, 'source', ?3, ?4, ?5)",
            params![
                transfer_id,
                direction.as_str(),
                serde_json::to_string(asset_ids)?,
                TransferState::Preparing.as_str(),
                timestamp(Utc::now()),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    })
}

fn validate_duplicate_closure(
    connection: &rusqlite::Connection,
    asset_ids: &[String],
) -> Result<()> {
    let selected = serde_json::to_string(asset_ids)?;
    let outside_reference = connection
        .query_row(
            "SELECT 1
             FROM assets dependent
             WHERE dependent.duplicate_of IN (SELECT value FROM json_each(?1))
               AND dependent.id NOT IN (SELECT value FROM json_each(?1))
             LIMIT 1",
            [&selected],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if outside_reference {
        return Err(Error::IncompleteVaultTransfer);
    }
    Ok(())
}

/// Atomically revalidates the duplicate closure and hides every source row before
/// physical deletion starts. Application writes cannot choose a `purging` row as
/// a new duplicate target, closing the check-to-file-delete race.
fn lease_transfer_source_deletion(database: &Database, asset_ids: &[String]) -> Result<()> {
    database.with_connection_mut(|connection| {
        let transaction = connection.transaction()?;
        validate_duplicate_closure(&transaction, asset_ids)?;
        let sql = format!(
            "UPDATE assets SET lifecycle = 'purging', visible_in_timeline = 0
             WHERE id IN ({})",
            sql_placeholders(asset_ids.len())
        );
        transaction.execute(&sql, params_from_iter(asset_ids.iter()))?;
        transaction.commit()?;
        Ok(())
    })
}

fn insert_transfer_record(
    transaction: &Transaction<'_>,
    transfer_id: &str,
    direction: TransferDirection,
    role: &str,
    asset_ids: &[String],
    state: TransferState,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO vault_transfers(id, direction, role, asset_ids, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            transfer_id,
            direction.as_str(),
            role,
            serde_json::to_string(asset_ids)?,
            state.as_str(),
            timestamp(Utc::now()),
        ],
    )?;
    Ok(())
}

fn source_transfers(
    database: &Database,
    direction: TransferDirection,
) -> Result<Vec<TransferJournal>> {
    database.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id, asset_ids, state FROM vault_transfers
             WHERE direction = ?1 AND role = 'source' ORDER BY created_at, id",
        )?;
        statement
            .query_map([direction.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (id, asset_ids, state) = row?;
                Uuid::parse_str(&id).map_err(|_| Error::IncompleteVaultTransfer)?;
                let asset_ids: Vec<String> = serde_json::from_str(&asset_ids)?;
                validate_transfer_ids(&asset_ids)?;
                Ok(TransferJournal {
                    id,
                    asset_ids,
                    state: TransferState::from_str(&state)?,
                })
            })
            .collect()
    })
}

fn destination_transfers(
    database: &Database,
    direction: TransferDirection,
) -> Result<Vec<TransferJournal>> {
    database.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id, asset_ids, state FROM vault_transfers
             WHERE direction = ?1 AND role = 'destination' ORDER BY created_at, id",
        )?;
        statement
            .query_map([direction.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (id, asset_ids, state) = row?;
                Uuid::parse_str(&id).map_err(|_| Error::IncompleteVaultTransfer)?;
                let asset_ids: Vec<String> = serde_json::from_str(&asset_ids)?;
                validate_transfer_ids(&asset_ids)?;
                Ok(TransferJournal {
                    id,
                    asset_ids,
                    state: TransferState::from_str(&state)?,
                })
            })
            .collect()
    })
}

fn transfer_ids(database: &Database) -> Result<HashSet<String>> {
    database.with_connection(|connection| {
        let mut statement = connection.prepare("SELECT id FROM vault_transfers")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|row| {
                let transfer_id = row?;
                Uuid::parse_str(&transfer_id).map_err(|_| Error::IncompleteVaultTransfer)?;
                Ok(transfer_id)
            })
            .collect()
    })
}

fn destination_transfer_exists(
    database: &Database,
    transfer_id: &str,
    direction: TransferDirection,
) -> Result<bool> {
    database.with_connection(|connection| {
        Ok(connection
            .query_row(
                "SELECT 1 FROM vault_transfers
                 WHERE id = ?1 AND direction = ?2 AND role = 'destination'",
                params![transfer_id, direction.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    })
}

fn update_transfer_state(
    database: &Database,
    transfer_id: &str,
    state: TransferState,
) -> Result<()> {
    let changed = database.with_connection(|connection| {
        Ok(connection.execute(
            "UPDATE vault_transfers SET state = ?2 WHERE id = ?1 AND role = 'source'",
            params![transfer_id, state.as_str()],
        )?)
    })?;
    if changed != 1 {
        return Err(Error::IncompleteVaultTransfer);
    }
    Ok(())
}

fn delete_transfer_record(database: &Database, transfer_id: &str) -> Result<()> {
    database.with_connection(|connection| {
        connection.execute("DELETE FROM vault_transfers WHERE id = ?1", [transfer_id])?;
        Ok(())
    })
}

fn assets_moved(source: &Database, destination: &Database, asset_ids: &[String]) -> Result<bool> {
    validate_transfer_ids(asset_ids)?;
    let source_count = asset_count(source, asset_ids)?;
    let destination_count = asset_count(destination, asset_ids)?;
    Ok(source_count == 0 && destination_count == asset_ids.len())
}

fn asset_count(database: &Database, asset_ids: &[String]) -> Result<usize> {
    database.with_connection(|connection| {
        let sql = format!(
            "SELECT count(*) FROM assets WHERE id IN ({})",
            sql_placeholders(asset_ids.len())
        );
        let count = connection.query_row(&sql, params_from_iter(asset_ids.iter()), |row| {
            row.get::<_, i64>(0)
        })?;
        usize::try_from(count).map_err(|_| Error::IncompleteVaultTransfer)
    })
}

/// Vault 内漫画スタックと全ページをまとめてメインへ戻す。
///
/// `vault: no-log`
pub fn export_stack(vault: &VaultHandle, main_db: &Database, stack_id: &str) -> Result<()> {
    let asset_ids = stack_asset_ids(&vault.db, stack_id)?;
    if asset_ids.is_empty() {
        return Err(Error::StackNotFound);
    }
    export_assets(vault, main_db, &asset_ids)
}

fn validate_transfer_ids(asset_ids: &[String]) -> Result<()> {
    if asset_ids.is_empty() || asset_ids.len() > MAX_VAULT_TRANSFER_ASSETS {
        return Err(Error::EmptyVaultTransfer);
    }
    let mut unique = HashSet::with_capacity(asset_ids.len());
    for asset_id in asset_ids {
        Uuid::parse_str(asset_id).map_err(|_| Error::InvalidAssetPath)?;
        if !unique.insert(asset_id) {
            return Err(Error::IncompleteVaultTransfer);
        }
    }
    Ok(())
}

fn prepare_main_assets(database: &Database, asset_ids: &[String]) -> Result<Vec<PreparedAsset>> {
    let service = AssetService::new(database.clone());
    let mut output = Vec::with_capacity(asset_ids.len());
    let mut total_bytes = 0_usize;
    for asset_id in asset_ids {
        let asset = service.get(asset_id)?.ok_or(Error::AssetNotFound)?;
        reserve_transfer_bytes(&mut total_bytes, asset.size)?;
        let original = database
            .data_root()
            .join(checked_relative_path(&asset.library_path)?);
        validate_plain_source_size(&original, Some(asset.size))?;
        let thumbnail = optional_plain_source(
            database
                .data_root()
                .join("thumbs")
                .join(format!("{asset_id}_t.webp")),
        )?;
        let preview = optional_plain_source(
            database
                .data_root()
                .join("thumbs")
                .join(format!("{asset_id}_p.webp")),
        )?;
        output.push(PreparedAsset {
            asset,
            original: PreparedSource::PlainFile(original),
            thumbnail,
            preview,
        });
    }
    Ok(output)
}

fn prepare_vault_assets(vault: &VaultHandle, asset_ids: &[String]) -> Result<Vec<PreparedAsset>> {
    let service = AssetService::new(vault.db.clone());
    let mut output = Vec::with_capacity(asset_ids.len());
    let mut total_bytes = 0_usize;
    for asset_id in asset_ids {
        let asset = service.get(asset_id)?.ok_or(Error::AssetNotFound)?;
        reserve_transfer_bytes(&mut total_bytes, asset.size)?;
        let original_id = vault
            .blob_id_for(asset_id, BlobKind::Original)?
            .ok_or(Error::IncompleteVaultTransfer)?;
        validate_vault_original(vault, &asset, &original_id)?;
        let thumbnail = vault
            .blob_id_for(asset_id, BlobKind::Thumbnail)?
            .map(PreparedSource::VaultBlob);
        let preview = vault
            .blob_id_for(asset_id, BlobKind::Preview)?
            .map(PreparedSource::VaultBlob);
        output.push(PreparedAsset {
            asset,
            original: PreparedSource::VaultBlob(original_id),
            thumbnail,
            preview,
        });
    }
    Ok(output)
}

fn stage_vault_import(
    vault: &VaultHandle,
    assets: &[PreparedAsset],
    relations: &Relations,
    transfer_id: &str,
    observer: &mut impl FnMut(ImportPhase) -> Result<()>,
) -> Result<()> {
    let staging_dir = transfer_staging_dir(vault.db.data_root(), transfer_id)?;
    create_private_dir_all(&staging_dir)?;
    let mut prepared_blobs = HashMap::<String, Vec<PreparedBlob>>::new();
    for prepared in assets {
        let mut blobs = vec![prepare_plain_file_blob(
            vault,
            plain_source(&prepared.original)?,
            BlobKind::Original,
            &prepared.asset.id,
            &staging_dir,
            Some(&prepared.asset),
        )?];
        if let Some(source) = &prepared.thumbnail {
            blobs.push(prepare_plain_file_blob(
                vault,
                plain_source(source)?,
                BlobKind::Thumbnail,
                &prepared.asset.id,
                &staging_dir,
                None,
            )?);
        }
        if let Some(source) = &prepared.preview {
            blobs.push(prepare_plain_file_blob(
                vault,
                plain_source(source)?,
                BlobKind::Preview,
                &prepared.asset.id,
                &staging_dir,
                None,
            )?);
        }
        prepared_blobs.insert(prepared.asset.id.clone(), blobs);
    }

    let result = vault.db.with_connection_mut(|connection| {
        let transaction = connection.transaction()?;
        for prepared in assets {
            let blobs = prepared_blobs
                .get(&prepared.asset.id)
                .ok_or(Error::IncompleteVaultTransfer)?;
            let original = blobs
                .iter()
                .find(|blob| matches!(blob.kind, BlobKind::Original))
                .ok_or(Error::IncompleteVaultTransfer)?;
            insert_asset(&transaction, &prepared.asset, &original.id)?;
            for blob in blobs {
                transaction.execute(
                    "INSERT INTO vault_blobs(blob_id, wrapped_key, kind, asset_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![blob.id, blob.wrapped_key, blob.kind.as_str(), blob.asset_id],
                )?;
            }
        }
        insert_relations(&transaction, relations, assets)?;
        insert_transfer_record(
            &transaction,
            transfer_id,
            TransferDirection::Import,
            "destination",
            &assets
                .iter()
                .map(|prepared| prepared.asset.id.clone())
                .collect::<Vec<_>>(),
            TransferState::DestinationReady,
        )?;
        observer(ImportPhase::VaultStaged)?;
        transaction.commit()?;
        Ok(())
    });
    if result.is_err() {
        remove_transfer_staging(vault.db.data_root(), transfer_id)?;
    }
    result
}

fn prepare_plain_file_blob(
    vault: &VaultHandle,
    source: &Path,
    kind: BlobKind,
    asset_id: &str,
    staging_dir: &Path,
    expected_asset: Option<&Asset>,
) -> Result<PreparedBlob> {
    let plaintext_len = validate_plain_source_size(source, expected_asset.map(|asset| asset.size))?;
    let mut input = fs::File::open(source)?;
    let expected = expected_asset.map(|asset| (asset.size, asset.hash.as_slice()));
    prepare_reader_blob(
        vault,
        &mut input,
        plaintext_len,
        kind,
        asset_id,
        staging_dir,
        expected,
    )
}

fn prepare_reader_blob(
    vault: &VaultHandle,
    input: &mut impl Read,
    plaintext_len: usize,
    kind: BlobKind,
    asset_id: &str,
    staging_dir: &Path,
    expected: Option<(i64, &[u8])>,
) -> Result<PreparedBlob> {
    let id = Uuid::now_v7().to_string();
    let file_key = Zeroizing::new(random_array()?);
    let aad = blob_key_aad(&id, kind.as_str(), Some(asset_id));
    let wrapped_key = vault
        .key
        .with_master_key(|master_key| wrap_bytes(master_key, file_key.as_slice(), &aad))?;
    let output_path = staging_dir.join(&id);
    let mut output = create_new_private_file(&output_path)?;
    let digest = encrypt_reader_to_writer(
        &id,
        &file_key,
        input,
        &mut output,
        plaintext_len,
        Some(&vault.key),
    )?;
    output.sync_all()?;
    if let Some((expected_len, expected_digest)) = expected {
        validate_original_parts(expected_len, expected_digest, plaintext_len, &digest)?;
    }
    Ok(PreparedBlob {
        id,
        wrapped_key,
        kind,
        asset_id: asset_id.to_owned(),
    })
}

fn stage_main_export(
    vault: &VaultHandle,
    database: &Database,
    assets: &[PreparedAsset],
    relations: &Relations,
    transfer_id: &str,
    observer: &mut impl FnMut(ExportPhase) -> Result<()>,
) -> Result<()> {
    let mut written = Vec::new();
    let prepared_paths = (|| {
        let mut relative_paths = Vec::with_capacity(assets.len());
        for prepared in assets {
            let relative_path = export_library_path(&prepared.asset)?;
            let original_path = database.data_root().join(&relative_path);
            write_vault_blob_to_new_file(
                vault,
                vault_source(&prepared.original)?,
                &original_path,
                Some(&prepared.asset),
            )?;
            written.push(original_path);
            if let Some(source) = &prepared.thumbnail {
                let path = database
                    .data_root()
                    .join("thumbs")
                    .join(format!("{}_t.webp", prepared.asset.id));
                write_vault_blob_to_new_file(vault, vault_source(source)?, &path, None)?;
                written.push(path);
            }
            if let Some(source) = &prepared.preview {
                let path = database
                    .data_root()
                    .join("thumbs")
                    .join(format!("{}_p.webp", prepared.asset.id));
                write_vault_blob_to_new_file(vault, vault_source(source)?, &path, None)?;
                written.push(path);
            }
            relative_paths.push(
                relative_path
                    .to_str()
                    .ok_or(Error::InvalidAssetPath)?
                    .to_owned(),
            );
        }
        Ok(relative_paths)
    })();
    let relative_paths = match prepared_paths {
        Ok(paths) => paths,
        Err(error) => {
            for path in written {
                remove_if_exists(&path)?;
            }
            return Err(error);
        }
    };

    // File decryption, writes, hashing and fsync complete before taking the single main DB mutex.
    // Until this short transaction commits the UUID paths are unreachable through API metadata.
    let result = database.with_connection_mut(|connection| {
        let transaction = connection.transaction()?;
        for (prepared, relative_path) in assets.iter().zip(&relative_paths) {
            insert_asset(&transaction, &prepared.asset, relative_path)?;
        }
        insert_relations(&transaction, relations, assets)?;
        insert_transfer_record(
            &transaction,
            transfer_id,
            TransferDirection::Export,
            "destination",
            &assets
                .iter()
                .map(|prepared| prepared.asset.id.clone())
                .collect::<Vec<_>>(),
            TransferState::DestinationReady,
        )?;
        observer(ExportPhase::MainStaged)?;
        transaction.commit()?;
        Ok(())
    });
    if result.is_err() {
        for path in written {
            remove_if_exists(&path)?;
        }
    }
    result
}

fn insert_asset(transaction: &Transaction<'_>, asset: &Asset, library_path: &str) -> Result<()> {
    let mut lifecycle = lifecycle_name(asset.lifecycle);
    let mut duplicate_owned = asset.duplicate_of.clone();
    let mut purge_after = asset.purge_after.as_deref();
    let mut visible = asset.visible_in_timeline;
    let primary = transaction
        .query_row(
            "SELECT id FROM assets
                 WHERE hash = ?1 AND lifecycle = 'active' AND duplicate_of IS NULL
                   AND NOT EXISTS (
                     SELECT 1 FROM vault_transfers transfer, json_each(transfer.asset_ids) item
                     WHERE transfer.role = 'source' AND item.value = assets.id
                   )",
            [&asset.hash],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(primary) = primary {
        duplicate_owned = Some(primary);
        visible = false;
        if matches!(asset.lifecycle, Lifecycle::Active | Lifecycle::Duplicate) {
            lifecycle = "duplicate";
            purge_after = asset.purge_after.as_deref();
        }
    } else if duplicate_owned.is_some() {
        // A source-side duplicate relation must not point outside this DB.
        duplicate_owned = None;
        if lifecycle == "duplicate" {
            lifecycle = "active";
            purge_after = None;
            visible = asset.in_timeline;
        }
    }
    transaction.execute(
        "INSERT INTO assets(
           id, hash, original_name, ext, size, width, height,
           taken_at, taken_at_local_date, uploaded_at, thumbhash,
           in_timeline, visible_in_timeline, lifecycle, duplicate_of,
           trashed_at, purge_after, library_path
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
           ?12, ?13, ?14, ?15, ?16, ?17, ?18
         )",
        params![
            asset.id,
            asset.hash,
            asset.original_name,
            asset.ext,
            asset.size,
            asset.width,
            asset.height,
            asset.taken_at,
            asset.taken_at_local_date,
            asset.uploaded_at,
            asset.thumbhash,
            asset.in_timeline,
            visible,
            lifecycle,
            duplicate_owned,
            asset.trashed_at,
            purge_after,
            library_path,
        ],
    )?;
    Ok(())
}

fn lifecycle_name(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Active => "active",
        Lifecycle::Duplicate => "duplicate",
        Lifecycle::Trashed => "trashed",
        Lifecycle::Purging => "purging",
    }
}

fn insert_relations(
    transaction: &Transaction<'_>,
    relations: &Relations,
    assets: &[PreparedAsset],
) -> Result<()> {
    let asset_ids: HashSet<&str> = assets.iter().map(|item| item.asset.id.as_str()).collect();
    let face_ids: HashSet<&str> = relations
        .faces
        .iter()
        .map(|face| face.id.as_str())
        .collect();
    for cluster in &relations.clusters {
        let cover = cluster
            .cover_face_id
            .as_deref()
            .filter(|id| face_ids.contains(id));
        transaction.execute(
            "INSERT OR IGNORE INTO clusters(id, name, cover_face_id, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                cluster.id,
                cluster.name,
                cover,
                cluster.created_by,
                cluster.created_at
            ],
        )?;
    }
    for stack in &relations.stacks {
        let cover = stack
            .cover_asset_id
            .as_deref()
            .filter(|id| asset_ids.contains(id))
            .or_else(|| {
                relations
                    .pages
                    .iter()
                    .find(|page| page.stack_id == stack.id)
                    .map(|page| page.asset_id.as_str())
            });
        transaction.execute(
            "INSERT OR IGNORE INTO manga_stacks(
               id, title, cover_asset_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                stack.id,
                stack.title,
                cover,
                stack.created_at,
                stack.updated_at
            ],
        )?;
    }
    for chapter in &relations.chapters {
        transaction.execute(
            "INSERT OR IGNORE INTO stack_chapters(id, stack_id, chapter_no, title)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                chapter.id,
                chapter.stack_id,
                chapter.chapter_no,
                chapter.title
            ],
        )?;
    }
    for page in &relations.pages {
        transaction.execute(
            "INSERT OR IGNORE INTO stack_pages(
               stack_id, chapter_id, asset_id, page_no, show_in_timeline
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                page.stack_id,
                page.chapter_id,
                page.asset_id,
                page.page_no,
                page.show_in_timeline
            ],
        )?;
    }
    for face in &relations.faces {
        transaction.execute(
            "INSERT INTO faces(
               id, asset_id, kind, bbox, det_conf, quality_flags, embedding,
               model_version, cluster_id, state, similarity
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                face.id,
                face.asset_id,
                face.kind,
                face.bbox,
                face.det_conf,
                face.quality_flags,
                face.embedding,
                face.model_version,
                face.cluster_id,
                face.state,
                face.similarity
            ],
        )?;
    }
    for rejection in &relations.rejections {
        transaction.execute(
            "INSERT OR IGNORE INTO cluster_rejections(face_id, cluster_id)
             VALUES (?1, ?2)",
            params![rejection.face_id, rejection.cluster_id],
        )?;
    }
    let cluster_ids = relations
        .clusters
        .iter()
        .map(|cluster| cluster.id.clone())
        .collect::<Vec<_>>();
    refresh_cluster_covers(transaction, &cluster_ids)?;
    Ok(())
}

fn collect_relations(database: &Database, asset_ids: &[String]) -> Result<Relations> {
    database.with_connection(|connection| {
        let placeholders = sql_placeholders(asset_ids.len());
        let mut relations = Relations::default();

        let stack_sql = format!(
            "SELECT DISTINCT s.id, s.title, s.cover_asset_id, s.created_at, s.updated_at
             FROM manga_stacks s
             JOIN stack_pages p ON p.stack_id = s.id
             WHERE p.asset_id IN ({placeholders})"
        );
        relations.stacks = connection
            .prepare(&stack_sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| {
                Ok(StackRecord {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    cover_asset_id: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let chapter_sql = format!(
            "SELECT DISTINCT c.id, c.stack_id, c.chapter_no, c.title
             FROM stack_chapters c
             JOIN stack_pages p ON p.chapter_id = c.id
             WHERE p.asset_id IN ({placeholders})"
        );
        relations.chapters = connection
            .prepare(&chapter_sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| {
                Ok(ChapterRecord {
                    id: row.get(0)?,
                    stack_id: row.get(1)?,
                    chapter_no: row.get(2)?,
                    title: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let page_sql = format!(
            "SELECT stack_id, chapter_id, asset_id, page_no, show_in_timeline
             FROM stack_pages WHERE asset_id IN ({placeholders})"
        );
        relations.pages = connection
            .prepare(&page_sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| {
                Ok(PageRecord {
                    stack_id: row.get(0)?,
                    chapter_id: row.get(1)?,
                    asset_id: row.get(2)?,
                    page_no: row.get(3)?,
                    show_in_timeline: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let face_sql = format!(
            "SELECT id, asset_id, kind, bbox, det_conf, quality_flags, embedding,
                    model_version, cluster_id, state, similarity
             FROM faces WHERE asset_id IN ({placeholders})"
        );
        relations.faces = connection
            .prepare(&face_sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| {
                Ok(FaceRecord {
                    id: row.get(0)?,
                    asset_id: row.get(1)?,
                    kind: row.get(2)?,
                    bbox: row.get(3)?,
                    det_conf: row.get(4)?,
                    quality_flags: row.get(5)?,
                    embedding: row.get(6)?,
                    model_version: row.get(7)?,
                    cluster_id: row.get(8)?,
                    state: row.get(9)?,
                    similarity: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let cluster_ids: Vec<String> = relations
            .faces
            .iter()
            .filter_map(|face| face.cluster_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if !cluster_ids.is_empty() {
            let cluster_sql = format!(
                "SELECT id, name, cover_face_id, created_by, created_at
                 FROM clusters WHERE id IN ({})",
                sql_placeholders(cluster_ids.len())
            );
            relations.clusters = connection
                .prepare(&cluster_sql)?
                .query_map(params_from_iter(cluster_ids.iter()), |row| {
                    Ok(ClusterRecord {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        cover_face_id: row.get(2)?,
                        created_by: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }

        let face_ids: Vec<&str> = relations
            .faces
            .iter()
            .map(|face| face.id.as_str())
            .collect();
        if !face_ids.is_empty() && !cluster_ids.is_empty() {
            let rejection_sql = format!(
                "SELECT face_id, cluster_id FROM cluster_rejections
                 WHERE face_id IN ({}) AND cluster_id IN ({})",
                sql_placeholders(face_ids.len()),
                numbered_placeholders(face_ids.len() + 1, cluster_ids.len())
            );
            let values = face_ids
                .iter()
                .copied()
                .chain(cluster_ids.iter().map(String::as_str));
            relations.rejections = connection
                .prepare(&rejection_sql)?
                .query_map(params_from_iter(values), |row| {
                    Ok(RejectionRecord {
                        face_id: row.get(0)?,
                        cluster_id: row.get(1)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }
        Ok(relations)
    })
}

fn delete_assets_from_database_idempotent(database: &Database, asset_ids: &[String]) -> Result<()> {
    database.with_connection_mut(|connection| {
        let transaction = connection.transaction()?;
        let placeholders = sql_placeholders(asset_ids.len());
        reparent_legacy_external_duplicates(&transaction, asset_ids)?;
        let affected_sql =
            format!("SELECT id FROM manga_stacks WHERE cover_asset_id IN ({placeholders})");
        let affected: Vec<String> = transaction
            .prepare(&affected_sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let cluster_sql = format!(
            "SELECT DISTINCT cluster_id FROM faces
             WHERE asset_id IN ({placeholders}) AND cluster_id IS NOT NULL"
        );
        let affected_clusters: Vec<String> = transaction
            .prepare(&cluster_sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let clear_sql = format!(
            "UPDATE manga_stacks SET cover_asset_id = NULL
             WHERE cover_asset_id IN ({placeholders})"
        );
        transaction.execute(&clear_sql, params_from_iter(asset_ids.iter()))?;
        let delete_sql = format!("DELETE FROM assets WHERE id IN ({placeholders})");
        transaction.execute(&delete_sql, params_from_iter(asset_ids.iter()))?;
        let delete_jobs_sql = format!(
            "DELETE FROM jobs
             WHERE json_valid(payload)
               AND json_extract(payload, '$.asset_id') IN ({placeholders})"
        );
        transaction.execute(&delete_jobs_sql, params_from_iter(asset_ids.iter()))?;
        transaction.execute(
            "DELETE FROM stack_chapters
             WHERE NOT EXISTS (
               SELECT 1 FROM stack_pages p WHERE p.chapter_id = stack_chapters.id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM manga_stacks
             WHERE NOT EXISTS (
               SELECT 1 FROM stack_pages p WHERE p.stack_id = manga_stacks.id
             )",
            [],
        )?;
        for stack_id in affected {
            transaction.execute(
                "UPDATE manga_stacks
                 SET cover_asset_id = (
                   SELECT p.asset_id
                   FROM stack_pages p
                   JOIN stack_chapters c ON c.id = p.chapter_id
                   WHERE p.stack_id = manga_stacks.id
                   ORDER BY c.chapter_no, p.page_no LIMIT 1
                 )
                 WHERE id = ?1 AND cover_asset_id IS NULL",
                [stack_id],
            )?;
        }
        for cluster_id in affected_clusters {
            refresh_cluster_covers(&transaction, std::slice::from_ref(&cluster_id))?;
            transaction.execute(
                "DELETE FROM clusters
                 WHERE id = ?1
                   AND NOT EXISTS (
                     SELECT 1 FROM faces WHERE cluster_id = clusters.id
                   )",
                [cluster_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    })
}

/// Last-resort recovery for journals created by versions that allowed a canonical
/// asset to move without all reverse duplicate references. New transfers reject
/// this before writing a journal; this path exists so a source-files-deleted journal
/// can still converge without losing the surviving duplicate files.
fn reparent_legacy_external_duplicates(
    transaction: &Transaction<'_>,
    asset_ids: &[String],
) -> Result<()> {
    let selected = serde_json::to_string(asset_ids)?;
    let mark_sql = format!(
        "UPDATE assets SET lifecycle = 'purging', visible_in_timeline = 0
         WHERE id IN ({})",
        sql_placeholders(asset_ids.len())
    );
    transaction.execute(&mark_sql, params_from_iter(asset_ids.iter()))?;

    for source_id in asset_ids {
        let replacement = transaction
            .query_row(
                "SELECT id FROM assets
                 WHERE duplicate_of = ?1
                   AND id NOT IN (SELECT value FROM json_each(?2))
                 ORDER BY lifecycle = 'active' DESC,
                          trashed_at IS NULL DESC, uploaded_at, id
                 LIMIT 1",
                params![source_id, selected],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(replacement) = replacement else {
            continue;
        };
        transaction.execute(
            "UPDATE assets
             SET duplicate_of = ?2
             WHERE duplicate_of = ?1
               AND id != ?2
               AND id NOT IN (SELECT value FROM json_each(?3))",
            params![source_id, replacement, selected],
        )?;
        transaction.execute(
            "UPDATE assets
             SET duplicate_of = NULL,
                 lifecycle = 'active',
                 trashed_at = NULL,
                 purge_after = NULL,
                 visible_in_timeline = CASE
                   WHEN in_timeline = 1 AND NOT EXISTS (
                     SELECT 1 FROM stack_pages sp
                     WHERE sp.asset_id = assets.id AND sp.show_in_timeline = 0
                   ) THEN 1 ELSE 0 END
             WHERE id = ?1",
            [&replacement],
        )?;
    }
    Ok(())
}

fn refresh_cluster_covers(transaction: &Transaction<'_>, cluster_ids: &[String]) -> Result<()> {
    for cluster_id in cluster_ids {
        transaction.execute(
            "UPDATE clusters
             SET cover_face_id = (
               SELECT id FROM faces
               WHERE cluster_id = ?1
               ORDER BY state = 'confirmed' DESC, id
               LIMIT 1
             )
             WHERE id = ?1
               AND (cover_face_id IS NULL OR NOT EXISTS (
                 SELECT 1 FROM faces cover
                 WHERE cover.id = clusters.cover_face_id
                   AND cover.cluster_id = clusters.id
               ))",
            [cluster_id],
        )?;
    }
    Ok(())
}

fn stack_asset_ids(database: &Database, stack_id: &str) -> Result<Vec<String>> {
    database.with_connection(|connection| {
        let exists = connection
            .query_row(
                "SELECT 1 FROM manga_stacks WHERE id = ?1",
                [stack_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(Error::StackNotFound);
        }
        let mut statement = connection.prepare(
            "SELECT p.asset_id
             FROM stack_pages p
             JOIN stack_chapters c ON c.id = p.chapter_id
             WHERE p.stack_id = ?1
             ORDER BY c.chapter_no, p.page_no",
        )?;
        let ids = statement
            .query_map([stack_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ids)
    })
}

fn vault_blob_ids(database: &Database, asset_ids: &[String]) -> Result<Vec<String>> {
    database.with_connection(|connection| {
        let sql = format!(
            "SELECT blob_id FROM vault_blobs WHERE asset_id IN ({})",
            sql_placeholders(asset_ids.len())
        );
        let ids = connection
            .prepare(&sql)?
            .query_map(params_from_iter(asset_ids.iter()), |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ids)
    })
}

fn sql_placeholders(count: usize) -> String {
    numbered_placeholders(1, count)
}

fn numbered_placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn checked_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidAssetPath);
    }
    Ok(path.to_path_buf())
}

fn export_library_path(asset: &Asset) -> Result<PathBuf> {
    Uuid::parse_str(&asset.id).map_err(|_| Error::InvalidAssetPath)?;
    if asset.taken_at_local_date.len() < 7
        || asset.taken_at_local_date.as_bytes().get(4) != Some(&b'-')
        || !asset.ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(Error::InvalidAssetPath);
    }
    checked_relative_path(
        &PathBuf::from("library")
            .join(&asset.taken_at_local_date[..4])
            .join(&asset.taken_at_local_date[5..7])
            .join(format!("{}.{}", asset.id, asset.ext))
            .to_string_lossy(),
    )
}

fn create_new_private_file(path: &Path) -> Result<fs::File> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

fn optional_plain_source(path: PathBuf) -> Result<Option<PreparedSource>> {
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(PreparedSource::PlainFile(path))),
        Ok(_) => Err(Error::InvalidAssetPath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_plain_source_size(path: &Path, expected: Option<i64>) -> Result<usize> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(Error::InvalidAssetPath);
    }
    let size = usize::try_from(metadata.len()).map_err(|_| Error::IncompleteVaultTransfer)?;
    if size > images::MAX_ASSET_BYTES
        || expected.is_some_and(|value| usize::try_from(value).ok() != Some(size))
    {
        return Err(Error::IncompleteVaultTransfer);
    }
    Ok(size)
}

fn plain_source(source: &PreparedSource) -> Result<&Path> {
    match source {
        PreparedSource::PlainFile(path) => Ok(path),
        PreparedSource::VaultBlob(_) => Err(Error::IncompleteVaultTransfer),
    }
}

fn vault_source(source: &PreparedSource) -> Result<&str> {
    match source {
        PreparedSource::VaultBlob(blob_id) => Ok(blob_id),
        PreparedSource::PlainFile(_) => Err(Error::IncompleteVaultTransfer),
    }
}

fn validate_original_parts(
    expected_len: i64,
    expected_digest: &[u8],
    actual_len: usize,
    actual_digest: &[u8; 32],
) -> Result<()> {
    if i64::try_from(actual_len).map_err(|_| Error::IncompleteVaultTransfer)? != expected_len
        || actual_digest.as_slice() != expected_digest
    {
        return Err(Error::IncompleteVaultTransfer);
    }
    Ok(())
}

fn validate_original_digest(
    asset: &Asset,
    actual_len: usize,
    actual_digest: &[u8; 32],
) -> Result<()> {
    validate_original_parts(asset.size, &asset.hash, actual_len, actual_digest)
}

fn validate_vault_original(vault: &VaultHandle, asset: &Asset, blob_id: &str) -> Result<()> {
    let mut total = 0_usize;
    let mut digest = blake3::Hasher::new();
    for chunk in vault.blob_reader(blob_id)? {
        let chunk = Zeroizing::new(chunk?);
        total = total
            .checked_add(chunk.len())
            .ok_or(Error::IncompleteVaultTransfer)?;
        digest.update(&chunk);
    }
    validate_original_digest(asset, total, digest.finalize().as_bytes())
}

fn write_vault_blob_to_new_file(
    vault: &VaultHandle,
    blob_id: &str,
    path: &Path,
    expected_asset: Option<&Asset>,
) -> Result<()> {
    let result = (|| {
        let mut output = create_new_private_file(path)?;
        let mut total = 0_usize;
        let mut digest = blake3::Hasher::new();
        for chunk in vault.blob_reader(blob_id)? {
            let chunk = Zeroizing::new(chunk?);
            total = total
                .checked_add(chunk.len())
                .ok_or(Error::IncompleteVaultTransfer)?;
            if total > images::MAX_ASSET_BYTES {
                return Err(Error::IncompleteVaultTransfer);
            }
            digest.update(&chunk);
            output.write_all(&chunk)?;
        }
        if let Some(asset) = expected_asset {
            validate_original_digest(asset, total, digest.finalize().as_bytes())?;
        }
        output.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        remove_if_exists(path)?;
    }
    result
}

fn validate_plain_original(path: &Path, asset: &Asset) -> Result<()> {
    let mut input = fs::File::open(path)?;
    let mut buffer = Zeroizing::new(vec![0_u8; CHUNK_SIZE]);
    let mut total = 0_usize;
    let mut digest = blake3::Hasher::new();
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count)
            .ok_or(Error::IncompleteVaultTransfer)?;
        if total > images::MAX_ASSET_BYTES {
            return Err(Error::IncompleteVaultTransfer);
        }
        digest.update(&buffer[..count]);
    }
    validate_original_digest(asset, total, digest.finalize().as_bytes())
}

fn validate_import_destination(vault: &VaultHandle, asset_ids: &[String]) -> Result<()> {
    let service = AssetService::new(vault.db.clone());
    for asset_id in asset_ids {
        let asset = service
            .get(asset_id)?
            .ok_or(Error::IncompleteVaultTransfer)?;
        let blob_id = vault
            .blob_id_for(asset_id, BlobKind::Original)?
            .ok_or(Error::IncompleteVaultTransfer)?;
        validate_vault_original(vault, &asset, &blob_id)?;
    }
    Ok(())
}

fn validate_export_destination(database: &Database, asset_ids: &[String]) -> Result<()> {
    let service = AssetService::new(database.clone());
    for asset_id in asset_ids {
        let asset = service
            .get(asset_id)?
            .ok_or(Error::IncompleteVaultTransfer)?;
        let path = database
            .data_root()
            .join(checked_relative_path(&asset.library_path)?);
        validate_plain_original(&path, &asset)?;
    }
    Ok(())
}

fn transfer_staging_dir(data_root: &Path, transfer_id: &str) -> Result<PathBuf> {
    Uuid::parse_str(transfer_id).map_err(|_| Error::IncompleteVaultTransfer)?;
    Ok(data_root
        .join("vault")
        .join(TRANSFER_STAGING_DIR)
        .join(transfer_id))
}

fn finalize_import_files(
    vault: &VaultHandle,
    transfer_id: &str,
    asset_ids: &[String],
) -> Result<()> {
    let staging_dir = transfer_staging_dir(vault.db.data_root(), transfer_id)?;
    for blob_id in vault_blob_ids(&vault.db, asset_ids)? {
        let staged = staging_dir.join(&blob_id);
        let destination = vault.blob_path(&blob_id)?;
        if destination.is_file() {
            remove_if_exists(&staged)?;
            continue;
        }
        if !staged.is_file() {
            return Err(Error::IncompleteVaultTransfer);
        }
        fs::rename(&staged, &destination)?;
        set_private_file_permissions(&destination)?;
    }
    remove_transfer_staging(vault.db.data_root(), transfer_id)
}

fn remove_transfer_staging(data_root: &Path, transfer_id: &str) -> Result<()> {
    let directory = transfer_staging_dir(data_root, transfer_id)?;
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_dir() {
        return Err(Error::InvalidAssetPath);
    }
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(Error::InvalidAssetPath)?;
        Uuid::parse_str(name).map_err(|_| Error::InvalidAssetPath)?;
        if !entry.file_type()?.is_file() {
            return Err(Error::InvalidAssetPath);
        }
        remove_if_exists(&entry.path())?;
    }
    fs::remove_dir(directory)?;
    Ok(())
}

fn delete_plain_source_files(
    source: &Database,
    destination: &Database,
    asset_ids: &[String],
) -> Result<()> {
    let destination_service = AssetService::new(destination.clone());
    for asset_id in asset_ids {
        let source_library_path = source.with_connection(|connection| {
            Ok(connection
                .query_row(
                    "SELECT library_path FROM assets WHERE id = ?1",
                    [asset_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })?;
        let relative = if let Some(library_path) = source_library_path {
            checked_relative_path(&library_path)?
        } else {
            let asset = destination_service
                .get(asset_id)?
                .ok_or(Error::IncompleteVaultTransfer)?;
            export_library_path(&asset)?
        };
        remove_if_exists(&source.data_root().join(relative))?;
        remove_if_exists(
            &source
                .data_root()
                .join("thumbs")
                .join(format!("{asset_id}_t.webp")),
        )?;
        remove_if_exists(
            &source
                .data_root()
                .join("thumbs")
                .join(format!("{asset_id}_p.webp")),
        )?;
    }
    Ok(())
}

fn rollback_export_files(
    vault: &VaultHandle,
    main_db: &Database,
    asset_ids: &[String],
) -> Result<()> {
    let vault_service = AssetService::new(vault.db.clone());
    let main_service = AssetService::new(main_db.clone());
    for asset_id in asset_ids {
        if main_service.get(asset_id)?.is_some() {
            continue;
        }
        let asset = vault_service
            .get(asset_id)?
            .ok_or(Error::IncompleteVaultTransfer)?;
        let original = main_db.data_root().join(export_library_path(&asset)?);
        if original.is_file() && validate_plain_original(&original, &asset).is_ok() {
            remove_if_exists(&original)?;
        }
        remove_if_exists(
            &main_db
                .data_root()
                .join("thumbs")
                .join(format!("{asset_id}_t.webp")),
        )?;
        remove_if_exists(
            &main_db
                .data_root()
                .join("thumbs")
                .join(format!("{asset_id}_p.webp")),
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum BlobKind {
    Standalone,
    Original,
    Thumbnail,
    Preview,
}

impl BlobKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Original => "original",
            Self::Thumbnail => "thumbnail",
            Self::Preview => "preview",
        }
    }
}

/// Vault を初期化し、1 度だけ表示する recovery key を返す。
///
/// `vault: no-log`
pub fn init(data_root: impl AsRef<Path>, password: &str) -> Result<String> {
    init_with_kdf(data_root, password, KdfParams::default())
}

/// 指定 Argon2id パラメータで初期化する。テストでは弱い設定を渡せる。
///
/// `vault: no-log`
pub fn init_with_kdf(
    data_root: impl AsRef<Path>,
    password: &str,
    kdf: KdfParams,
) -> Result<String> {
    let data_root = data_root.as_ref();
    let vault_dir = data_root.join("vault");
    let keyfile_path = vault_dir.join("vault.keyfile");
    if keyfile_path.exists() {
        return Err(Error::VaultAlreadyInitialized);
    }
    create_private_dir_all(&vault_dir)?;
    create_private_dir_all(&vault_dir.join("blobs"))?;

    let master_key = Zeroizing::new(random_array::<KEY_LEN>()?);
    let recovery_key = Zeroizing::new(random_array::<KEY_LEN>()?);
    let salt: [u8; SALT_LEN] = random_array()?;
    let password_key = Zeroizing::new(derive_password_key(password, &salt, kdf)?);
    let keyfile = KeyFile {
        version: KEYFILE_VERSION,
        kdf,
        salt: hex::encode(salt),
        password: wrap_record(
            &password_key,
            master_key.as_slice(),
            b"illumia-keyfile-password",
        )?,
        recovery: wrap_record(
            &recovery_key,
            master_key.as_slice(),
            b"illumia-keyfile-recovery",
        )?,
    };
    write_keyfile(&keyfile_path, &keyfile, true)?;
    Ok(base32_encode(&recovery_key))
}

/// password で master key を復号する。
///
/// `vault: no-log`
pub fn unlock(data_root: impl AsRef<Path>, password: &str) -> Result<VaultKey> {
    let keyfile = read_keyfile(data_root.as_ref())?;
    let salt = decode_fixed::<SALT_LEN>(&keyfile.salt).map_err(|_| Error::InvalidVaultKeyFile)?;
    let password_key = Zeroizing::new(derive_password_key(password, &salt, keyfile.kdf)?);
    let master_key = unwrap_record(
        &password_key,
        &keyfile.password,
        b"illumia-keyfile-password",
    )
    .map_err(|_| Error::VaultAuthenticationFailed)?;
    Ok(VaultKey::new(master_key))
}

/// recovery key で master key を復号する。
///
/// `vault: no-log`
pub fn unlock_with_recovery(data_root: impl AsRef<Path>, recovery_key: &str) -> Result<VaultKey> {
    let keyfile = read_keyfile(data_root.as_ref())?;
    let recovery_key = Zeroizing::new(base32_decode(recovery_key)?);
    let master_key = unwrap_record(
        &recovery_key,
        &keyfile.recovery,
        b"illumia-keyfile-recovery",
    )
    .map_err(|_| Error::VaultAuthenticationFailed)?;
    Ok(VaultKey::new(master_key))
}

/// password の KEK レコードだけを再作成する。MK と recovery レコードは不変。
///
/// `vault: no-log`
pub fn change_password(
    data_root: impl AsRef<Path>,
    old_password: &str,
    new_password: &str,
) -> Result<()> {
    let data_root = data_root.as_ref();
    let kdf = read_keyfile(data_root)?.kdf;
    change_password_with_kdf(data_root, old_password, new_password, kdf)
}

/// password 変更時に新しい Argon2id パラメータも指定する。
///
/// `vault: no-log`
pub fn change_password_with_kdf(
    data_root: impl AsRef<Path>,
    old_password: &str,
    new_password: &str,
    kdf: KdfParams,
) -> Result<()> {
    let data_root = data_root.as_ref();
    let keyfile_path = data_root.join("vault").join("vault.keyfile");
    let mut keyfile = read_keyfile(data_root)?;
    let master_key = unlock(data_root, old_password)?;
    let salt: [u8; SALT_LEN] = random_array()?;
    let password_key = Zeroizing::new(derive_password_key(new_password, &salt, kdf)?);
    keyfile.kdf = kdf;
    keyfile.salt = hex::encode(salt);
    keyfile.password = master_key.with_master_key(|master_key| {
        wrap_record(&password_key, master_key, b"illumia-keyfile-password")
    })?;
    write_keyfile(&keyfile_path, &keyfile, false)
}

fn derive_password_key(
    password: &str,
    salt: &[u8],
    parameters: KdfParams,
) -> Result<[u8; KEY_LEN]> {
    if password.len() > MAX_VAULT_PASSWORD_BYTES {
        return Err(Error::VaultAuthenticationFailed);
    }
    validate_kdf(parameters)?;
    let parameters = Params::new(
        parameters.memory_kib,
        parameters.iterations,
        parameters.parallelism,
        Some(KEY_LEN),
    )
    .map_err(|_| Error::InvalidKdfParameters)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters);
    let mut output = [0_u8; KEY_LEN];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut output)
        .map_err(|_| Error::VaultCrypto)?;
    Ok(output)
}

fn read_keyfile(data_root: &Path) -> Result<KeyFile> {
    let path = data_root.join("vault").join("vault.keyfile");
    if path.exists() {
        set_private_file_permissions(&path)?;
    }
    match fs::metadata(&path) {
        Ok(metadata) if metadata.len() > MAX_KEYFILE_BYTES => {
            return Err(Error::InvalidVaultKeyFile);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::VaultNotInitialized);
        }
        Err(error) => return Err(error.into()),
    }
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::VaultNotInitialized
        } else {
            error.into()
        }
    })?;
    let keyfile: KeyFile =
        serde_json::from_slice(&bytes).map_err(|_| Error::InvalidVaultKeyFile)?;
    if keyfile.version != KEYFILE_VERSION {
        return Err(Error::InvalidVaultKeyFile);
    }
    validate_kdf(keyfile.kdf).map_err(|_| Error::InvalidVaultKeyFile)?;
    Ok(keyfile)
}

fn write_keyfile(path: &Path, keyfile: &KeyFile, create_new: bool) -> Result<()> {
    let bytes = Zeroizing::new(serde_json::to_vec(keyfile)?);
    if create_new {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Error::VaultAlreadyInitialized
                } else {
                    error.into()
                }
            })?;
        set_private_file_permissions(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        return Ok(());
    }

    let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
    fs::write(&temporary, &bytes)?;
    set_private_file_permissions(&temporary)?;
    if let Err(error) = fs::rename(&temporary, path) {
        #[cfg(windows)]
        if path.exists() {
            let replacement = (|| -> Result<()> {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                remove_if_exists(&temporary)
            })();
            return replacement;
        }
        let _ = remove_if_exists(&temporary);
        return Err(error.into());
    }
    set_private_file_permissions(path)?;
    Ok(())
}

fn wrap_record(key: &[u8; KEY_LEN], value: &[u8], aad: &[u8]) -> Result<WrappedRecord> {
    let nonce: [u8; NONCE_LEN] = random_array()?;
    let ciphertext = aead_encrypt(key, &nonce, value, aad)?;
    Ok(WrappedRecord {
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
    })
}

fn unwrap_record(key: &[u8; KEY_LEN], record: &WrappedRecord, aad: &[u8]) -> Result<[u8; KEY_LEN]> {
    let nonce = decode_fixed::<NONCE_LEN>(&record.nonce).map_err(|_| Error::InvalidVaultKeyFile)?;
    let ciphertext = hex::decode(&record.ciphertext).map_err(|_| Error::InvalidVaultKeyFile)?;
    let plaintext = Zeroizing::new(aead_decrypt(key, &nonce, &ciphertext, aad)?);
    plaintext
        .as_slice()
        .try_into()
        .map_err(|_| Error::InvalidVaultKeyFile)
}

fn wrap_bytes(key: &[u8; KEY_LEN], value: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let nonce: [u8; NONCE_LEN] = random_array()?;
    let ciphertext = aead_encrypt(key, &nonce, value, aad)?;
    let mut output = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

fn unwrap_bytes(key: &[u8; KEY_LEN], wrapped: &[u8], aad: &[u8]) -> Result<[u8; KEY_LEN]> {
    if wrapped.len() != NONCE_LEN + KEY_LEN + TAG_LEN {
        return Err(Error::InvalidVaultBlob);
    }
    let (nonce, ciphertext) = wrapped.split_at(NONCE_LEN);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| Error::InvalidVaultBlob)?;
    let plaintext = Zeroizing::new(aead_decrypt(key, nonce, ciphertext, aad)?);
    plaintext
        .as_slice()
        .try_into()
        .map_err(|_| Error::InvalidVaultBlob)
}

fn aead_encrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::VaultCrypto)?;
    let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| Error::VaultCrypto)?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::VaultCrypto)
}

fn aead_decrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::VaultCrypto)?;
    let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| Error::VaultCrypto)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::VaultAuthenticationFailed)
}

#[cfg(test)]
fn encrypt_blob(blob_id: &str, key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    if plaintext.len() > images::MAX_ASSET_BYTES {
        return Err(Error::InvalidVaultBlob);
    }
    let nonce_prefix: [u8; NONCE_PREFIX_LEN] = random_array()?;
    let mut header = Vec::with_capacity(BLOB_HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&nonce_prefix);
    header.extend_from_slice(
        &u32::try_from(CHUNK_SIZE)
            .map_err(|_| Error::InvalidVaultBlob)?
            .to_be_bytes(),
    );

    let chunk_count = plaintext.len().div_ceil(CHUNK_SIZE).max(1);
    let mut output =
        Vec::with_capacity(header.len() + plaintext.len() + chunk_count * (TAG_LEN + 5));
    output.extend_from_slice(&header);
    for index in 0..chunk_count {
        let start = index * CHUNK_SIZE;
        let end = (start + CHUNK_SIZE).min(plaintext.len());
        let chunk = &plaintext[start..end];
        let final_chunk = index + 1 == chunk_count;
        let nonce = chunk_nonce(&nonce_prefix, index)?;
        let aad = chunk_aad(blob_id, &header, index, final_chunk)?;
        let ciphertext = aead_encrypt(key, &nonce, chunk, &aad)?;
        output.push(u8::from(final_chunk));
        output.extend_from_slice(
            &u32::try_from(ciphertext.len())
                .map_err(|_| Error::InvalidVaultBlob)?
                .to_be_bytes(),
        );
        output.extend_from_slice(&ciphertext);
    }
    Ok(output)
}

fn encrypt_reader_to_writer(
    blob_id: &str,
    key: &[u8; KEY_LEN],
    reader: &mut impl Read,
    writer: &mut impl Write,
    plaintext_len: usize,
    revocation: Option<&VaultKey>,
) -> Result<[u8; 32]> {
    if let Some(key) = revocation {
        key.ensure_active()?;
    }
    if plaintext_len > images::MAX_ASSET_BYTES {
        return Err(Error::InvalidVaultBlob);
    }
    let nonce_prefix: [u8; NONCE_PREFIX_LEN] = random_array()?;
    let mut header = [0_u8; BLOB_HEADER_LEN];
    header[..MAGIC.len()].copy_from_slice(MAGIC);
    header[MAGIC.len()..MAGIC.len() + NONCE_PREFIX_LEN].copy_from_slice(&nonce_prefix);
    header[MAGIC.len() + NONCE_PREFIX_LEN..].copy_from_slice(
        &u32::try_from(CHUNK_SIZE)
            .map_err(|_| Error::InvalidVaultBlob)?
            .to_be_bytes(),
    );
    writer.write_all(&header)?;

    let chunk_count = plaintext_len.div_ceil(CHUNK_SIZE).max(1);
    let mut remaining = plaintext_len;
    let mut plaintext = Zeroizing::new(vec![0_u8; CHUNK_SIZE]);
    let mut digest = blake3::Hasher::new();
    for index in 0..chunk_count {
        if let Some(key) = revocation {
            key.ensure_active()?;
        }
        let chunk_len = remaining.min(CHUNK_SIZE);
        if chunk_len != 0 {
            reader.read_exact(&mut plaintext[..chunk_len])?;
            digest.update(&plaintext[..chunk_len]);
        }
        remaining -= chunk_len;
        let final_chunk = index + 1 == chunk_count;
        let nonce = chunk_nonce(&nonce_prefix, index)?;
        let aad = chunk_aad(blob_id, &header, index, final_chunk)?;
        let ciphertext = aead_encrypt(key, &nonce, &plaintext[..chunk_len], &aad)?;
        writer.write_all(&[u8::from(final_chunk)])?;
        writer.write_all(
            &u32::try_from(ciphertext.len())
                .map_err(|_| Error::InvalidVaultBlob)?
                .to_be_bytes(),
        )?;
        writer.write_all(&ciphertext)?;
    }
    if let Some(key) = revocation {
        key.ensure_active()?;
    }
    let mut extra = [0_u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(Error::InvalidVaultBlob);
    }
    Ok(*digest.finalize().as_bytes())
}

#[cfg(test)]
fn decrypt_blob(blob_id: &str, key: &[u8; KEY_LEN], encrypted: &[u8]) -> Result<Vec<u8>> {
    if encrypted.len() < BLOB_HEADER_LEN
        || u64::try_from(encrypted.len()).map_err(|_| Error::InvalidVaultBlob)?
            > MAX_ENCRYPTED_BLOB_BYTES
        || &encrypted[..MAGIC.len()] != MAGIC
    {
        return Err(Error::InvalidVaultBlob);
    }
    let header = &encrypted[..BLOB_HEADER_LEN];
    let nonce_prefix: &[u8; NONCE_PREFIX_LEN] = encrypted
        [MAGIC.len()..MAGIC.len() + NONCE_PREFIX_LEN]
        .try_into()
        .map_err(|_| Error::InvalidVaultBlob)?;
    let chunk_size = u32::from_be_bytes(
        encrypted[MAGIC.len() + NONCE_PREFIX_LEN..BLOB_HEADER_LEN]
            .try_into()
            .map_err(|_| Error::InvalidVaultBlob)?,
    );
    if usize::try_from(chunk_size).map_err(|_| Error::InvalidVaultBlob)? != CHUNK_SIZE {
        return Err(Error::InvalidVaultBlob);
    }

    let mut cursor = BLOB_HEADER_LEN;
    let mut index = 0_usize;
    let mut output = Vec::new();
    loop {
        if encrypted.len().saturating_sub(cursor) < 5 {
            return Err(Error::InvalidVaultBlob);
        }
        let final_chunk = match encrypted[cursor] {
            0 => false,
            1 => true,
            _ => return Err(Error::InvalidVaultBlob),
        };
        let length = u32::from_be_bytes(
            encrypted[cursor + 1..cursor + 5]
                .try_into()
                .map_err(|_| Error::InvalidVaultBlob)?,
        );
        let length = usize::try_from(length).map_err(|_| Error::InvalidVaultBlob)?;
        cursor += 5;
        if !(TAG_LEN..=CHUNK_SIZE + TAG_LEN).contains(&length)
            || encrypted.len().saturating_sub(cursor) < length
        {
            return Err(Error::InvalidVaultBlob);
        }
        let nonce = chunk_nonce(nonce_prefix, index)?;
        let aad = chunk_aad(blob_id, header, index, final_chunk)?;
        let plaintext = aead_decrypt(key, &nonce, &encrypted[cursor..cursor + length], &aad)
            .map_err(|_| Error::InvalidVaultBlob)?;
        if !final_chunk && plaintext.len() != CHUNK_SIZE {
            return Err(Error::InvalidVaultBlob);
        }
        output.extend_from_slice(&plaintext);
        cursor += length;
        if final_chunk {
            if cursor != encrypted.len() {
                return Err(Error::InvalidVaultBlob);
            }
            return Ok(output);
        }
        index = index.checked_add(1).ok_or(Error::InvalidVaultBlob)?;
    }
}

fn validate_kdf(parameters: KdfParams) -> Result<()> {
    if !(64..=MAX_KDF_MEMORY_KIB).contains(&parameters.memory_kib)
        || !(1..=MAX_KDF_ITERATIONS).contains(&parameters.iterations)
        || !(1..=MAX_KDF_PARALLELISM).contains(&parameters.parallelism)
    {
        return Err(Error::InvalidKdfParameters);
    }
    Ok(())
}

fn reserve_transfer_bytes(total: &mut usize, size: i64) -> Result<()> {
    let size = usize::try_from(size).map_err(|_| Error::IncompleteVaultTransfer)?;
    if size > images::MAX_ASSET_BYTES {
        return Err(Error::IncompleteVaultTransfer);
    }
    *total = total
        .checked_add(size)
        .ok_or(Error::IncompleteVaultTransfer)?;
    if *total > MAX_VAULT_TRANSFER_BYTES {
        return Err(Error::IncompleteVaultTransfer);
    }
    Ok(())
}

fn chunk_nonce(prefix: &[u8; NONCE_PREFIX_LEN], index: usize) -> Result<[u8; NONCE_LEN]> {
    let mut nonce = [0_u8; NONCE_LEN];
    nonce[..NONCE_PREFIX_LEN].copy_from_slice(prefix);
    nonce[NONCE_PREFIX_LEN..].copy_from_slice(
        &u64::try_from(index)
            .map_err(|_| Error::InvalidVaultBlob)?
            .to_be_bytes(),
    );
    Ok(nonce)
}

fn chunk_aad(blob_id: &str, header: &[u8], index: usize, final_chunk: bool) -> Result<Vec<u8>> {
    let mut aad = Vec::with_capacity(blob_id.len() + header.len() + 9);
    aad.extend_from_slice(blob_id.as_bytes());
    aad.extend_from_slice(header);
    aad.extend_from_slice(
        &u64::try_from(index)
            .map_err(|_| Error::InvalidVaultBlob)?
            .to_be_bytes(),
    );
    aad.push(u8::from(final_chunk));
    Ok(aad)
}

fn blob_key_aad(blob_id: &str, kind: &str, asset_id: Option<&str>) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(16 + blob_id.len() + kind.len() + asset_id.map_or(0, str::len));
    aad.extend_from_slice(b"illumia-blob-key");
    aad.extend_from_slice(blob_id.as_bytes());
    aad.extend_from_slice(kind.as_bytes());
    if let Some(asset_id) = asset_id {
        aad.extend_from_slice(asset_id.as_bytes());
    }
    aad
}

fn random_array<const N: usize>() -> Result<[u8; N]> {
    let mut output = [0_u8; N];
    SysRng
        .try_fill_bytes(&mut output)
        .map_err(|_| Error::RandomGeneration)?;
    Ok(output)
}

fn decode_fixed<const N: usize>(value: &str) -> std::result::Result<[u8; N], ()> {
    let decoded = hex::decode(value).map_err(|_| ())?;
    decoded.try_into().map_err(|_| ())
}

fn base32_encode(bytes: &[u8; KEY_LEN]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut output = String::with_capacity(52);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        accumulator = (accumulator << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = usize::try_from((accumulator >> bits) & 0x1f).unwrap_or_default();
            output.push(char::from(ALPHABET[index]));
        }
    }
    if bits > 0 {
        let index = usize::try_from((accumulator << (5 - bits)) & 0x1f).unwrap_or_default();
        output.push(char::from(ALPHABET[index]));
    }
    output
}

fn base32_decode(value: &str) -> Result<[u8; KEY_LEN]> {
    let compact: Vec<u8> = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'-')
        .map(|byte| byte.to_ascii_uppercase())
        .collect();
    if compact.len() != 52 {
        return Err(Error::InvalidRecoveryKey);
    }
    let mut output = Vec::with_capacity(KEY_LEN);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in compact {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return Err(Error::InvalidRecoveryKey),
        };
        accumulator = (accumulator << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push(
                u8::try_from((accumulator >> bits) & 0xff)
                    .map_err(|_| Error::InvalidRecoveryKey)?,
            );
        }
    }
    if output.len() != KEY_LEN || accumulator & ((1_u32 << bits) - 1) != 0 {
        output.zeroize();
        return Err(Error::InvalidRecoveryKey);
    }
    output.try_into().map_err(|_| Error::InvalidRecoveryKey)
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, process::Command};

    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

    use super::*;

    fn png() -> Vec<u8> {
        let pixels = RgbaImage::from_pixel(2, 2, Rgba([12, 34, 56, 255]));
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(pixels)
            .write_to(&mut output, ImageFormat::Png)
            .expect("PNG should encode");
        output.into_inner()
    }

    #[test]
    fn blob_aad_binds_ciphertext_to_its_identifier() -> Result<()> {
        let key = [7_u8; KEY_LEN];
        let encrypted = encrypt_blob("first-blob", &key, b"bound plaintext")?;
        assert_eq!(
            decrypt_blob("first-blob", &key, &encrypted)?,
            b"bound plaintext"
        );
        assert!(matches!(
            decrypt_blob("second-blob", &key, &encrypted),
            Err(Error::InvalidVaultBlob)
        ));
        Ok(())
    }

    #[test]
    fn import_process_crash_preserves_a_complete_copy_at_every_boundary() -> Result<()> {
        for failed_phase in [
            "source-prepared",
            "vault-staged",
            "vault-committed",
            "plain-files-deleted",
            "plain-database-deleted",
        ] {
            let directory = tempfile::tempdir()?;
            let bytes = png();
            let asset_id = {
                let main = Database::open(directory.path())?;
                init_with_kdf(directory.path(), "password", KdfParams::for_tests())?;
                let _vault =
                    VaultHandle::open(directory.path(), unlock(directory.path(), "password")?)?;
                AssetService::new(main)
                    .ingest(&bytes, "process-crash.png", None)?
                    .asset
                    .id
            };

            let status = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "vault::tests::import_crash_worker",
                    "--test-threads=1",
                ])
                .env("ILLUMIA_VAULT_CRASH_WORKER", "1")
                .env("ILLUMIA_VAULT_CRASH_ROOT", directory.path())
                .env("ILLUMIA_VAULT_CRASH_ASSET", &asset_id)
                .env("ILLUMIA_VAULT_CRASH_PHASE", failed_phase)
                .status()?;
            assert!(!status.success());

            let main = Database::open(directory.path())?;
            let vault = VaultHandle::open(directory.path(), unlock(directory.path(), "password")?)?;
            reconcile_transfers(&main, &vault)?;
            let main_complete =
                if let Some(main_asset) = AssetService::new(main.clone()).get(&asset_id)? {
                    fs::read(main.data_root().join(main_asset.library_path))
                        .is_ok_and(|content| content == bytes)
                } else {
                    false
                };
            let vault_complete =
                if let Some(vault_asset) = AssetService::new(vault.db.clone()).get(&asset_id)? {
                    vault
                        .read_blob(&vault_asset.library_path)
                        .is_ok_and(|content| content == bytes)
                } else {
                    false
                };
            let destination_committed = matches!(
                failed_phase,
                "vault-committed" | "plain-files-deleted" | "plain-database-deleted"
            );
            assert_eq!(vault_complete, destination_committed);
            assert_eq!(main_complete, !destination_committed);
            assert_transfer_artifacts_clean(&main, &vault)?;
        }
        Ok(())
    }

    #[test]
    fn import_crash_worker() {
        if std::env::var_os("ILLUMIA_VAULT_CRASH_WORKER").is_none() {
            return;
        }
        let root = PathBuf::from(
            std::env::var_os("ILLUMIA_VAULT_CRASH_ROOT").expect("worker root must be set"),
        );
        let asset_id =
            std::env::var("ILLUMIA_VAULT_CRASH_ASSET").expect("worker asset must be set");
        let failed_phase =
            std::env::var("ILLUMIA_VAULT_CRASH_PHASE").expect("worker phase must be set");
        let main = Database::open(&root).expect("worker main DB should open");
        let vault = VaultHandle::open(&root, unlock(&root, "password").expect("worker unlock"))
            .expect("worker vault DB should open");
        let result = import_assets_inner(&main, &vault, &[asset_id], &mut |phase| -> Result<()> {
            let phase_name = match phase {
                ImportPhase::SourcePrepared => "source-prepared",
                ImportPhase::VaultStaged => "vault-staged",
                ImportPhase::VaultCommitted => "vault-committed",
                ImportPhase::PlainFilesDeleted => "plain-files-deleted",
                ImportPhase::PlainDatabaseDeleted => "plain-database-deleted",
            };
            if phase_name == failed_phase {
                std::process::abort();
            }
            Ok(())
        });
        panic!("worker did not abort: {result:?}");
    }

    #[test]
    fn import_failure_injection_preserves_a_complete_copy_at_every_boundary() -> Result<()> {
        for failed_phase in [
            ImportPhase::SourcePrepared,
            ImportPhase::VaultStaged,
            ImportPhase::VaultCommitted,
            ImportPhase::PlainFilesDeleted,
            ImportPhase::PlainDatabaseDeleted,
        ] {
            let directory = tempfile::tempdir()?;
            let main = Database::open(directory.path())?;
            init_with_kdf(directory.path(), "password", KdfParams::for_tests())?;
            let vault = VaultHandle::open(directory.path(), unlock(directory.path(), "password")?)?;
            let bytes = png();
            let asset = AssetService::new(main.clone())
                .ingest(&bytes, "boundary.png", None)?
                .asset;
            let source_path = main.data_root().join(&asset.library_path);
            let mut injected = false;
            let result = import_assets_inner(
                &main,
                &vault,
                std::slice::from_ref(&asset.id),
                &mut |phase| {
                    if phase == failed_phase {
                        injected = true;
                        Err(Error::IncompleteVaultTransfer)
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(injected);
            assert!(result.is_err());

            reconcile_transfers(&main, &vault)?;

            let main_complete = AssetService::new(main.clone()).get(&asset.id)?.is_some()
                && fs::read(&source_path).is_ok_and(|content| content == bytes);
            let vault_complete =
                if let Some(vault_asset) = AssetService::new(vault.db.clone()).get(&asset.id)? {
                    vault
                        .read_blob(&vault_asset.library_path)
                        .is_ok_and(|content| content == bytes)
                } else {
                    false
                };
            let destination_committed = matches!(
                failed_phase,
                ImportPhase::VaultCommitted
                    | ImportPhase::PlainFilesDeleted
                    | ImportPhase::PlainDatabaseDeleted
            );
            assert_eq!(vault_complete, destination_committed);
            assert_eq!(main_complete, !destination_committed);
            assert_transfer_artifacts_clean(&main, &vault)?;
        }
        Ok(())
    }

    #[test]
    fn export_failure_injection_preserves_a_complete_copy_at_every_boundary() -> Result<()> {
        for failed_phase in [
            ExportPhase::SourcePrepared,
            ExportPhase::MainStaged,
            ExportPhase::MainCommitted,
            ExportPhase::VaultFilesDeleted,
            ExportPhase::VaultDatabaseDeleted,
        ] {
            let directory = tempfile::tempdir()?;
            let main = Database::open(directory.path())?;
            init_with_kdf(directory.path(), "password", KdfParams::for_tests())?;
            let vault = VaultHandle::open(directory.path(), unlock(directory.path(), "password")?)?;
            let bytes = png();
            let asset = AssetService::new(main.clone())
                .ingest(&bytes, "boundary.png", None)?
                .asset;
            import_assets(&main, &vault, std::slice::from_ref(&asset.id))?;

            let mut injected = false;
            let result = export_assets_inner(
                &vault,
                &main,
                std::slice::from_ref(&asset.id),
                &mut |phase| {
                    if phase == failed_phase {
                        injected = true;
                        Err(Error::IncompleteVaultTransfer)
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(injected);
            assert!(result.is_err());

            reconcile_transfers(&main, &vault)?;

            let main_complete =
                if let Some(main_asset) = AssetService::new(main.clone()).get(&asset.id)? {
                    fs::read(main.data_root().join(main_asset.library_path))
                        .is_ok_and(|content| content == bytes)
                } else {
                    false
                };
            let vault_complete =
                if let Some(vault_asset) = AssetService::new(vault.db.clone()).get(&asset.id)? {
                    vault
                        .read_blob(&vault_asset.library_path)
                        .is_ok_and(|content| content == bytes)
                } else {
                    false
                };
            let destination_committed = matches!(
                failed_phase,
                ExportPhase::MainCommitted
                    | ExportPhase::VaultFilesDeleted
                    | ExportPhase::VaultDatabaseDeleted
            );
            assert_eq!(main_complete, destination_committed);
            assert_eq!(vault_complete, !destination_committed);
            assert_transfer_artifacts_clean(&main, &vault)?;
        }
        Ok(())
    }

    fn assert_transfer_artifacts_clean(main: &Database, vault: &VaultHandle) -> Result<()> {
        for database in [main, &vault.db] {
            database.with_connection(|connection| {
                let count =
                    connection.query_row("SELECT count(*) FROM vault_transfers", [], |row| {
                        row.get::<_, i64>(0)
                    })?;
                assert_eq!(count, 0);
                Ok(())
            })?;
        }
        let staging = vault
            .db
            .data_root()
            .join("vault")
            .join(TRANSFER_STAGING_DIR);
        assert!(!staging.exists() || fs::read_dir(staging)?.next().is_none());
        Ok(())
    }
}
