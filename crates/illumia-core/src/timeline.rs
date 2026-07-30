//! タイムラインの DB 取得ロジック。

use rusqlite::params;

use crate::db::{Database, Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Granularity {
    Day,
    Month,
    Year,
}

impl Granularity {
    fn key_length(self) -> i64 {
        match self {
            Self::Day => 10,
            Self::Month => 7,
            Self::Year => 4,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bucket {
    pub key: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BucketItem {
    pub id: String,
    pub ratio: f64,
    pub thumbhash: Option<Vec<u8>>,
    pub taken_at: String,
}

#[derive(Clone, Debug)]
pub struct TimelineService {
    database: Database,
}

impl TimelineService {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn buckets(&self, granularity: Granularity) -> Result<Vec<(String, u64)>> {
        Ok(self
            .bucket_records(granularity)?
            .into_iter()
            .map(|bucket| (bucket.key, bucket.count))
            .collect())
    }

    pub fn bucket_records(&self, granularity: Granularity) -> Result<Vec<Bucket>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT substr(taken_at_local_date, 1, ?1) AS bucket_key, count(*)
                 FROM assets
                 WHERE visible_in_timeline = 1
                 GROUP BY bucket_key
                 ORDER BY bucket_key DESC",
            )?;
            let buckets = statement
                .query_map([granularity.key_length()], |row| {
                    let count = row.get::<_, i64>(1)?;
                    Ok(Bucket {
                        key: row.get(0)?,
                        count: u64::try_from(count)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, count))?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(buckets)
        })
    }

    /// キー長 (`YYYY` / `YYYY-MM` / `YYYY-MM-DD`) から粒度を判定する。
    pub fn bucket_items(&self, key: &str) -> Result<Vec<BucketItem>> {
        let length = match key.len() {
            4 | 7 | 10 if valid_bucket_key(key) => {
                i64::try_from(key.len()).map_err(|_| Error::InvalidBucketKey)?
            }
            _ => return Err(Error::InvalidBucketKey),
        };
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, aspect_ratio, thumbhash, taken_at
                 FROM assets
                 WHERE visible_in_timeline = 1
                   AND substr(taken_at_local_date, 1, ?1) = ?2
                 ORDER BY taken_at DESC",
            )?;
            let items = statement
                .query_map(params![length, key], |row| {
                    Ok(BucketItem {
                        id: row.get(0)?,
                        ratio: row.get(1)?,
                        thumbhash: row.get(2)?,
                        taken_at: row.get(3)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(items)
        })
    }
}

fn valid_bucket_key(key: &str) -> bool {
    key.bytes().enumerate().all(|(index, byte)| {
        if index == 4 || index == 7 {
            byte == b'-'
        } else {
            byte.is_ascii_digit()
        }
    })
}
