//! `settings` テーブルの型付きアクセス。

use std::{path::PathBuf, str::FromStr};

use rusqlite::{OptionalExtension, params};

use crate::db::{Database, Error, Result};

const TRASH_RETENTION_DAYS: &str = "trash.retention_days";
const DEDUP_RETENTION_DAYS: &str = "dedup.retention_days";
const THUMBNAIL_CONCURRENCY: &str = "jobs.thumbnail_concurrency";
const ML_CONCURRENCY: &str = "jobs.ml_concurrency";
const TAU_HIGH_OVERRIDE: &str = "ml.tau_high_override";
const TAU_LOW_OVERRIDE: &str = "ml.tau_low_override";
const MIN_CLUSTER_SIZE: &str = "ml.min_cluster_size";
const QUALITY_GATE: &str = "ml.quality_gate";
const ML_ENABLED: &str = "ml.enabled";
const ML_SOCKET_PATH: &str = "ml.socket_path";

pub(crate) const DEFAULT_RETENTION_DAYS: u32 = 30;
pub const MAX_RETENTION_DAYS: u32 = 36_500;
pub const MIN_JOB_CONCURRENCY: u32 = 1;
pub const MAX_JOB_CONCURRENCY: u32 = 64;
pub const MIN_CLUSTER_SIZE_VALUE: u32 = 2;
pub const MAX_CLUSTER_SIZE_VALUE: u32 = 100_000;
pub const DEFAULT_ML_CONCURRENCY: u32 = 1;
pub const DEFAULT_MIN_CLUSTER_SIZE: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityGate {
    ReviewOnly,
    Strict,
}

impl QualityGate {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReviewOnly => "review_only",
            Self::Strict => "strict",
        }
    }
}

impl FromStr for QualityGate {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "review_only" => Ok(Self::ReviewOnly),
            "strict" => Ok(Self::Strict),
            _ => Err(Error::InvalidSetting(QUALITY_GATE)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    database: Database,
}

impl Settings {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn trash_retention_days(&self) -> Result<u32> {
        bounded_u32(
            TRASH_RETENTION_DAYS,
            self.get_u32(TRASH_RETENTION_DAYS, DEFAULT_RETENTION_DAYS)?,
            0,
            MAX_RETENTION_DAYS,
        )
    }

    pub fn set_trash_retention_days(&self, value: u32) -> Result<()> {
        bounded_u32(TRASH_RETENTION_DAYS, value, 0, MAX_RETENTION_DAYS)?;
        self.set_u32(TRASH_RETENTION_DAYS, value)
    }

    pub fn dedup_retention_days(&self) -> Result<u32> {
        bounded_u32(
            DEDUP_RETENTION_DAYS,
            self.get_u32(DEDUP_RETENTION_DAYS, DEFAULT_RETENTION_DAYS)?,
            0,
            MAX_RETENTION_DAYS,
        )
    }

    pub fn set_dedup_retention_days(&self, value: u32) -> Result<()> {
        bounded_u32(DEDUP_RETENTION_DAYS, value, 0, MAX_RETENTION_DAYS)?;
        self.set_u32(DEDUP_RETENTION_DAYS, value)
    }

    pub fn thumbnail_concurrency(&self) -> Result<Option<u32>> {
        self.get_optional(THUMBNAIL_CONCURRENCY)?
            .map(|value| {
                bounded_u32(
                    THUMBNAIL_CONCURRENCY,
                    value,
                    MIN_JOB_CONCURRENCY,
                    MAX_JOB_CONCURRENCY,
                )
            })
            .transpose()
    }

    pub fn set_thumbnail_concurrency(&self, value: u32) -> Result<()> {
        bounded_u32(
            THUMBNAIL_CONCURRENCY,
            value,
            MIN_JOB_CONCURRENCY,
            MAX_JOB_CONCURRENCY,
        )?;
        self.set_u32(THUMBNAIL_CONCURRENCY, value)
    }

    pub fn ml_concurrency(&self) -> Result<u32> {
        bounded_u32(
            ML_CONCURRENCY,
            self.get_u32(ML_CONCURRENCY, DEFAULT_ML_CONCURRENCY)?,
            MIN_JOB_CONCURRENCY,
            MAX_JOB_CONCURRENCY,
        )
    }

    pub fn set_ml_concurrency(&self, value: u32) -> Result<()> {
        bounded_u32(
            ML_CONCURRENCY,
            value,
            MIN_JOB_CONCURRENCY,
            MAX_JOB_CONCURRENCY,
        )?;
        self.set_u32(ML_CONCURRENCY, value)
    }

    pub fn tau_high_override(&self) -> Result<Option<f64>> {
        self.get_optional(TAU_HIGH_OVERRIDE)?
            .map(|value| bounded_ratio(TAU_HIGH_OVERRIDE, value))
            .transpose()
    }

    pub fn set_tau_high_override(&self, value: f64) -> Result<()> {
        bounded_ratio(TAU_HIGH_OVERRIDE, value)?;
        self.set(TAU_HIGH_OVERRIDE, &value.to_string())
    }

    pub fn tau_low_override(&self) -> Result<Option<f64>> {
        self.get_optional(TAU_LOW_OVERRIDE)?
            .map(|value| bounded_ratio(TAU_LOW_OVERRIDE, value))
            .transpose()
    }

    pub fn set_tau_low_override(&self, value: f64) -> Result<()> {
        bounded_ratio(TAU_LOW_OVERRIDE, value)?;
        self.set(TAU_LOW_OVERRIDE, &value.to_string())
    }

    pub fn min_cluster_size(&self) -> Result<u32> {
        bounded_u32(
            MIN_CLUSTER_SIZE,
            self.get_u32(MIN_CLUSTER_SIZE, DEFAULT_MIN_CLUSTER_SIZE)?,
            MIN_CLUSTER_SIZE_VALUE,
            MAX_CLUSTER_SIZE_VALUE,
        )
    }

    pub fn set_min_cluster_size(&self, value: u32) -> Result<()> {
        bounded_u32(
            MIN_CLUSTER_SIZE,
            value,
            MIN_CLUSTER_SIZE_VALUE,
            MAX_CLUSTER_SIZE_VALUE,
        )?;
        self.set_u32(MIN_CLUSTER_SIZE, value)
    }

    pub fn quality_gate(&self) -> Result<QualityGate> {
        self.database.with_connection(|connection| {
            let value = connection
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [QUALITY_GATE],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            value.map_or(Ok(QualityGate::ReviewOnly), |value| value.parse())
        })
    }

    pub fn set_quality_gate(&self, value: QualityGate) -> Result<()> {
        self.set(QUALITY_GATE, value.as_str())
    }

    pub fn ml_enabled(&self) -> Result<bool> {
        self.get_optional::<bool>(ML_ENABLED)
            .map(|value| value.unwrap_or(true))
    }

    pub fn set_ml_enabled(&self, value: bool) -> Result<()> {
        self.set(ML_ENABLED, if value { "true" } else { "false" })
    }

    pub fn ml_socket_path(&self) -> Result<Option<PathBuf>> {
        self.database.with_connection(|connection| {
            let value = connection
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [ML_SOCKET_PATH],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            value
                .map(|raw| validate_socket_path(&raw).map(PathBuf::from))
                .transpose()
        })
    }

    pub fn set_ml_socket_path(&self, value: &str) -> Result<()> {
        validate_socket_path(value)?;
        self.set(ML_SOCKET_PATH, value)
    }

    fn get_u32(&self, key: &'static str, default: u32) -> Result<u32> {
        Ok(self.get_optional(key)?.unwrap_or(default))
    }

    fn get_optional<T>(&self, key: &'static str) -> Result<Option<T>>
    where
        T: FromStr,
    {
        self.database.with_connection(|connection| {
            let value = connection
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?;
            value
                .map(|raw| raw.parse().map_err(|_| Error::InvalidSetting(key)))
                .transpose()
        })
    }

    fn set_u32(&self, key: &'static str, value: u32) -> Result<()> {
        self.set(key, &value.to_string())
    }

    fn set(&self, key: &'static str, value: &str) -> Result<()> {
        self.database.with_connection(|connection| {
            connection.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }
}

pub(crate) fn retention_days(
    transaction: &rusqlite::Transaction<'_>,
    key: &'static str,
) -> Result<u32> {
    let raw = transaction
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    raw.map(|value| value.parse().map_err(|_| Error::InvalidSetting(key)))
        .transpose()
        .map(|value| value.unwrap_or(DEFAULT_RETENTION_DAYS))
        .and_then(|value| bounded_u32(key, value, 0, MAX_RETENTION_DAYS))
}

pub(crate) fn trash_retention_days(transaction: &rusqlite::Transaction<'_>) -> Result<u32> {
    retention_days(transaction, TRASH_RETENTION_DAYS)
}

pub(crate) fn dedup_retention_days(transaction: &rusqlite::Transaction<'_>) -> Result<u32> {
    retention_days(transaction, DEDUP_RETENTION_DAYS)
}

fn bounded_u32(key: &'static str, value: u32, minimum: u32, maximum: u32) -> Result<u32> {
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(Error::InvalidSetting(key))
    }
}

fn bounded_ratio(key: &'static str, value: f64) -> Result<f64> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::InvalidSetting(key))
    }
}

fn validate_socket_path(value: &str) -> Result<&str> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        Err(Error::InvalidSetting(ML_SOCKET_PATH))
    } else {
        Ok(value)
    }
}
