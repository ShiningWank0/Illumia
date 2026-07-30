//! `settings` テーブルの型付きアクセス。

use std::str::FromStr;

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

pub(crate) const DEFAULT_RETENTION_DAYS: u32 = 30;

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
        self.get_u32(TRASH_RETENTION_DAYS, DEFAULT_RETENTION_DAYS)
    }

    pub fn set_trash_retention_days(&self, value: u32) -> Result<()> {
        self.set_u32(TRASH_RETENTION_DAYS, value)
    }

    pub fn dedup_retention_days(&self) -> Result<u32> {
        self.get_u32(DEDUP_RETENTION_DAYS, DEFAULT_RETENTION_DAYS)
    }

    pub fn set_dedup_retention_days(&self, value: u32) -> Result<()> {
        self.set_u32(DEDUP_RETENTION_DAYS, value)
    }

    pub fn thumbnail_concurrency(&self) -> Result<Option<u32>> {
        self.get_optional(THUMBNAIL_CONCURRENCY)
    }

    pub fn set_thumbnail_concurrency(&self, value: u32) -> Result<()> {
        self.set_u32(THUMBNAIL_CONCURRENCY, value)
    }

    pub fn ml_concurrency(&self) -> Result<Option<u32>> {
        self.get_optional(ML_CONCURRENCY)
    }

    pub fn set_ml_concurrency(&self, value: u32) -> Result<()> {
        self.set_u32(ML_CONCURRENCY, value)
    }

    pub fn tau_high_override(&self) -> Result<Option<f64>> {
        self.get_optional(TAU_HIGH_OVERRIDE)
    }

    pub fn set_tau_high_override(&self, value: f64) -> Result<()> {
        self.set(TAU_HIGH_OVERRIDE, &value.to_string())
    }

    pub fn tau_low_override(&self) -> Result<Option<f64>> {
        self.get_optional(TAU_LOW_OVERRIDE)
    }

    pub fn set_tau_low_override(&self, value: f64) -> Result<()> {
        self.set(TAU_LOW_OVERRIDE, &value.to_string())
    }

    pub fn min_cluster_size(&self) -> Result<Option<u32>> {
        self.get_optional(MIN_CLUSTER_SIZE)
    }

    pub fn set_min_cluster_size(&self, value: u32) -> Result<()> {
        self.set_u32(MIN_CLUSTER_SIZE, value)
    }

    pub fn quality_gate(&self) -> Result<Option<QualityGate>> {
        self.database.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [QUALITY_GATE],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|value| value.parse())
                .transpose()
        })
    }

    pub fn set_quality_gate(&self, value: QualityGate) -> Result<()> {
        self.set(QUALITY_GATE, value.as_str())
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
}

pub(crate) fn trash_retention_days(transaction: &rusqlite::Transaction<'_>) -> Result<u32> {
    retention_days(transaction, TRASH_RETENTION_DAYS)
}

pub(crate) fn dedup_retention_days(transaction: &rusqlite::Transaction<'_>) -> Result<u32> {
    retention_days(transaction, DEDUP_RETENTION_DAYS)
}
