//! SQLite 接続と versioned migration。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rusqlite::Connection;
use thiserror::Error;

const MIGRATIONS: &[(u32, &str)] = &[(1, include_str!("../migrations/0001_init.sql"))];

/// illumia-core の共通エラー。
#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("database mutex is poisoned")]
    DatabasePoisoned,
    #[error("asset not found")]
    AssetNotFound,
    #[error("unsupported image extension: {0}")]
    UnsupportedExtension(String),
    #[error("invalid setting value for {0}")]
    InvalidSetting(&'static str),
    #[error("invalid timeline bucket key")]
    InvalidBucketKey,
    #[error("invalid asset-owned path")]
    InvalidAssetPath,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
struct DatabaseInner {
    connection: Mutex<Connection>,
    data_root: PathBuf,
}

/// WAL 設定済みのメイン DB とデータディレクトリを束ねる共有ハンドル。
#[derive(Clone, Debug)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

impl Database {
    /// `<data_root>/illumia.db` を開き、未適用 migration を適用する。
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self> {
        let data_root = data_root.as_ref();
        fs::create_dir_all(data_root)?;
        let mut connection = Connection::open(data_root.join("illumia.db"))?;
        configure(&connection)?;
        migrate(&mut connection)?;

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                connection: Mutex::new(connection),
                data_root: data_root.to_path_buf(),
            }),
        })
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
        let connection = self
            .inner
            .connection
            .lock()
            .map_err(|_| Error::DatabasePoisoned)?;
        operation(&connection)
    }

    pub(crate) fn with_connection_mut<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = self
            .inner
            .connection
            .lock()
            .map_err(|_| Error::DatabasePoisoned)?;
        operation(&mut connection)
    }
}

fn configure(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
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
