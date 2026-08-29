//! SQLite 接続と versioned migration。

use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rusqlite::Connection;
use thiserror::Error;
use zeroize::Zeroizing;

const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_vault_blobs.sql")),
    (3, include_str!("../migrations/0003_job_admission.sql")),
    (4, include_str!("../migrations/0004_vault_transfers.sql")),
    (5, include_str!("../migrations/0005_job_cancellation.sql")),
];

/// illumia-core の共通エラー。
#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ML client error: {0}")]
    MlClient(#[from] crate::ml_client::Error),
    #[error("image processing error: {0}")]
    ImageProcessing(String),
    #[error("invalid image input: {0}")]
    InvalidImage(String),
    #[error("WebP encoding error: {0}")]
    WebpEncoding(String),
    #[error("database mutex is poisoned")]
    DatabasePoisoned,
    #[error("asset not found")]
    AssetNotFound,
    #[error("face not found")]
    FaceNotFound,
    #[error("cluster not found")]
    ClusterNotFound,
    #[error("invalid ML operation: {0}")]
    InvalidMl(String),
    #[error("manga stack not found")]
    StackNotFound,
    #[error("stack chapter not found")]
    StackChapterNotFound,
    #[error("invalid manga stack: {0}")]
    InvalidStack(String),
    #[error("invalid search query")]
    InvalidSearch,
    #[error("unsupported image extension: {0}")]
    UnsupportedExtension(String),
    #[error("invalid setting value for {0}")]
    InvalidSetting(&'static str),
    #[error("invalid timeline bucket key")]
    InvalidBucketKey,
    #[error("invalid asset-owned path")]
    InvalidAssetPath,
    #[error("invalid job state: {0}")]
    InvalidJobState(String),
    #[error("job progress must be between 0 and 1")]
    InvalidJobProgress,
    #[error("job runner is already started")]
    JobRunnerAlreadyStarted,
    #[error("a job worker thread panicked")]
    JobWorkerPanicked,
    #[error("job queue admission limit reached")]
    JobQueueFull,
    #[error("vault is already initialized")]
    VaultAlreadyInitialized,
    #[error("vault is not initialized")]
    VaultNotInitialized,
    #[error("vault authentication failed")]
    VaultAuthenticationFailed,
    #[error("invalid vault key file")]
    InvalidVaultKeyFile,
    #[error("vault cryptographic operation failed")]
    VaultCrypto,
    #[error("invalid vault blob")]
    InvalidVaultBlob,
    #[error("vault blob not found")]
    VaultBlobNotFound,
    #[error("invalid recovery key")]
    InvalidRecoveryKey,
    #[error("invalid Argon2 parameters")]
    InvalidKdfParameters,
    #[error("random number generation failed")]
    RandomGeneration,
    #[error("vault transfer requires at least one asset")]
    EmptyVaultTransfer,
    #[error("vault transfer source is incomplete")]
    IncompleteVaultTransfer,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
struct DatabaseInner {
    connection: Mutex<Connection>,
    data_root: PathBuf,
}

/// WAL 設定済みのメイン DB とデータディレクトリを束ねる共有ハンドル。
type AccessGuard = dyn Fn() -> Result<()> + Send + Sync + 'static;

#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
    access_guard: Option<Arc<AccessGuard>>,
}

impl fmt::Debug for Database {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Database")
            .field("data_root", &self.inner.data_root)
            .field("access_guarded", &self.access_guard.is_some())
            .finish_non_exhaustive()
    }
}

impl Database {
    /// `<data_root>/illumia.db` を開き、未適用 migration を適用する。
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self> {
        let data_root = data_root.as_ref();
        create_private_dir_all(data_root)?;
        let database_path = data_root.join("illumia.db");
        let mut connection = Connection::open(&database_path)?;
        set_private_file_permissions(&database_path)?;
        configure(&connection)?;
        migrate(&mut connection)?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                connection: Mutex::new(connection),
                data_root: data_root.to_path_buf(),
            }),
            access_guard: None,
        })
    }

    /// SQLCipher 鍵を設定して `<data_root>/vault/vault.db` を開く。
    ///
    /// `vault: no-log` — 呼び出し元は鍵・パス・asset id をログへ出さないこと。
    pub(crate) fn open_vault(data_root: &Path, sqlcipher_key: &[u8; 32]) -> Result<Self> {
        let vault_dir = data_root.join("vault");
        create_private_dir_all(&vault_dir)?;
        create_private_dir_all(&vault_dir.join("blobs"))?;
        let database_path = vault_dir.join("vault.db");
        let mut connection = Connection::open(&database_path)?;
        set_private_file_permissions(&database_path)?;
        let key = Zeroizing::new(hex::encode(sqlcipher_key));
        let key_pragma = Zeroizing::new(format!(
            "PRAGMA key = \"x'{}'\";
             PRAGMA cipher_memory_security = ON;",
            key.as_str()
        ));
        connection.execute_batch(&key_pragma)?;
        // Wrong keys fail on the first schema read. Do this before any migration.
        connection.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        configure(&connection)?;
        migrate(&mut connection)?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                connection: Mutex::new(connection),
                data_root: data_root.to_path_buf(),
            }),
            access_guard: None,
        })
    }

    /// Returns a request-scoped clone that revalidates access immediately before and after
    /// every connection use, including after waiting for the process-wide SQLite mutex.
    #[must_use]
    pub fn with_access_guard(
        &self,
        guard: impl Fn() -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            access_guard: Some(Arc::new(guard)),
        }
    }

    /// データルート。
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.inner.data_root
    }

    /// 読み取り専用の接続アクセス。テストや上位サービスの複合クエリ向け。
    pub fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        self.ensure_access()?;
        let connection = self
            .inner
            .connection
            .lock()
            .map_err(|_| Error::DatabasePoisoned)?;
        self.ensure_access()?;
        let result = operation(&connection);
        self.ensure_access()?;
        result
    }

    pub(crate) fn with_connection_mut<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        self.ensure_access()?;
        let mut connection = self
            .inner
            .connection
            .lock()
            .map_err(|_| Error::DatabasePoisoned)?;
        self.ensure_access()?;
        let result = operation(&mut connection);
        self.ensure_access()?;
        result
    }

    /// WAL の内容を DB 本体へ反映して切り詰める。
    pub fn checkpoint_truncate(&self) -> Result<()> {
        self.with_connection(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
    }

    fn ensure_access(&self) -> Result<()> {
        self.access_guard.as_ref().map_or(Ok(()), |guard| guard())
    }
}

pub(crate) fn create_private_dir_all(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    Ok(())
}

pub(crate) fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn configure(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA secure_delete = ON;
        PRAGMA temp_store = MEMORY;
        PRAGMA busy_timeout = 5000;
        ",
    )?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let current: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    for &(version, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }

        let transaction = connection.transaction()?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }

    Ok(())
}
