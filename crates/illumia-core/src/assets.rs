//! アセット取り込みと論理ライフサイクル遷移。

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use image::GenericImageView;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::{
    db::{Database, Error, Result},
    settings,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Active,
    Duplicate,
    Trashed,
    Purging,
}

impl Lifecycle {
    fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "duplicate" => Ok(Self::Duplicate),
            "trashed" => Ok(Self::Trashed),
            "purging" => Ok(Self::Purging),
            _ => Err(rusqlite::Error::InvalidColumnType(
                0,
                "lifecycle".to_owned(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub id: String,
    pub hash: Vec<u8>,
    pub original_name: String,
    pub ext: String,
    pub size: i64,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: f64,
    pub taken_at: String,
    pub taken_at_local_date: String,
    pub uploaded_at: String,
    pub thumbhash: Option<Vec<u8>>,
    pub in_timeline: bool,
    pub visible_in_timeline: bool,
    pub lifecycle: Lifecycle,
    pub duplicate_of: Option<String>,
    pub trashed_at: Option<String>,
    pub purge_after: Option<String>,
    pub library_path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IngestResult {
    pub asset: Asset,
    pub duplicate_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DuplicatePair {
    pub duplicate: Asset,
    pub original: Asset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackLocation {
    pub stack_id: String,
    pub chapter_id: String,
}

#[derive(Clone, Debug)]
pub struct AssetService {
    database: Database,
}

impl AssetService {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn ingest(
        &self,
        bytes: &[u8],
        original_name: &str,
        taken_at: Option<DateTime<Utc>>,
    ) -> Result<IngestResult> {
        self.ingest_at(bytes, original_name, taken_at, Utc::now())
    }

    pub fn ingest_at(
        &self,
        bytes: &[u8],
        original_name: &str,
        taken_at: Option<DateTime<Utc>>,
        uploaded_at: DateTime<Utc>,
    ) -> Result<IngestResult> {
        let ext = normalized_extension(original_name)?;
        let image = image::load_from_memory(bytes)?;
        let (width, height) = image.dimensions();
        let hash = blake3::hash(bytes);
        let id = Uuid::now_v7().to_string();
        let taken_at = taken_at.unwrap_or(uploaded_at);
        let relative_path = library_path(
            &id,
            &ext,
            taken_at.format("%Y").to_string(),
            taken_at.format("%m").to_string(),
        );
        let absolute_path = self.database.data_root().join(&relative_path);

        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let result = self.database.with_connection_mut(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let duplicate_of = transaction
                .query_row(
                    "SELECT id FROM assets
                     WHERE hash = ?1
                       AND lifecycle = 'active'
                       AND duplicate_of IS NULL",
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
            let visible_in_timeline = i64::from(duplicate_of.is_none());

            fs::write(&absolute_path, bytes)?;
            transaction.execute(
                "INSERT INTO assets(
                    id, hash, original_name, ext, size, width, height,
                    taken_at, taken_at_local_date, uploaded_at, thumbhash,
                    in_timeline, visible_in_timeline, lifecycle, duplicate_of,
                    trashed_at, purge_after, library_path
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                    ?8, ?9, ?10, NULL,
                    1, ?11, ?12, ?13,
                    NULL, ?14, ?15
                 )",
                params![
                    id,
                    hash.as_bytes().as_slice(),
                    original_name,
                    ext,
                    i64::try_from(bytes.len()).map_err(|_| Error::InvalidAssetPath)?,
                    i64::from(width),
                    i64::from(height),
                    timestamp(taken_at),
                    taken_at.format("%Y-%m-%d").to_string(),
                    timestamp(uploaded_at),
                    visible_in_timeline,
                    lifecycle,
                    duplicate_of,
                    purge_after,
                    relative_path_to_string(&relative_path)?,
                ],
            )?;
            transaction.commit()?;

            let asset = self
                .get_with_connection(connection, &id)?
                .ok_or(Error::AssetNotFound)?;
            Ok(IngestResult {
                asset,
                duplicate_of,
            })
        });
        if result.is_err() {
            // DB 登録に失敗した場合、書き込み済みファイルを孤児にしない
            let _ = fs::remove_file(&absolute_path);
        }
        result
    }

    pub fn get(&self, id: &str) -> Result<Option<Asset>> {
        self.database
            .with_connection(|connection| self.get_with_connection(connection, id))
    }

    pub fn trash(&self, id: &str) -> Result<Asset> {
        self.trash_at(id, Utc::now())
    }

    pub fn trash_at(&self, id: &str, now: DateTime<Utc>) -> Result<Asset> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let purge_after = add_days(now, settings::trash_retention_days(&transaction)?);
            let changed = transaction.execute(
                "UPDATE assets
                 SET lifecycle = 'trashed',
                     trashed_at = ?2,
                     purge_after = ?3,
                     visible_in_timeline = 0
                 WHERE id = ?1
                   AND lifecycle IN ('active','duplicate','trashed')",
                params![id, timestamp(now), purge_after],
            )?;
            if changed == 0 {
                return Err(Error::AssetNotFound);
            }
            transaction.commit()?;
            self.get_with_connection(connection, id)?
                .ok_or(Error::AssetNotFound)
        })
    }

    pub fn restore(&self, id: &str) -> Result<Asset> {
        self.restore_at(id, Utc::now())
    }

    pub fn restore_at(&self, id: &str, now: DateTime<Utc>) -> Result<Asset> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let (duplicate_of, hash) = transaction
                .query_row(
                    "SELECT duplicate_of, hash FROM assets
                     WHERE id = ?1 AND lifecycle = 'trashed'",
                    [id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?
                .ok_or(Error::AssetNotFound)?;

            let (lifecycle, purge_after) = if duplicate_of.is_some() {
                (
                    "duplicate",
                    Some(add_days(now, settings::dedup_retention_days(&transaction)?)),
                )
            } else {
                // 元本の trash 中に同 hash が再取り込みされていても、古い元本を
                // canonical に戻して部分 UNIQUE と復元の両方を成立させる。
                let replacement_primary = transaction
                    .query_row(
                        "SELECT id FROM assets
                         WHERE hash = ?1
                           AND lifecycle = 'active'
                           AND duplicate_of IS NULL
                           AND id != ?2",
                        params![hash, id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if let Some(replacement_primary) = replacement_primary {
                    transaction.execute(
                        "UPDATE assets
                         SET duplicate_of = ?1
                         WHERE duplicate_of = ?2",
                        params![id, replacement_primary],
                    )?;
                    transaction.execute(
                        "UPDATE assets
                         SET duplicate_of = ?1
                         WHERE id = ?2",
                        params![id, replacement_primary],
                    )?;
                }
                ("active", None)
            };

            transaction.execute(
                "UPDATE assets
                 SET lifecycle = ?2,
                     trashed_at = NULL,
                     purge_after = ?3,
                     visible_in_timeline = CASE
                       WHEN ?2 = 'active'
                        AND in_timeline = 1
                        AND NOT EXISTS (
                          SELECT 1 FROM stack_pages sp
                          WHERE sp.asset_id = assets.id
                            AND sp.show_in_timeline = 0
                        )
                       THEN 1 ELSE 0
                     END
                 WHERE id = ?1 AND lifecycle = 'trashed'",
                params![id, lifecycle, purge_after],
            )?;
            transaction.commit()?;
            self.get_with_connection(connection, id)?
                .ok_or(Error::AssetNotFound)
        })
    }

    pub fn list_trash(&self) -> Result<Vec<Asset>> {
        self.list_assets(
            "SELECT
               id, hash, original_name, ext, size, width, height, aspect_ratio,
               taken_at, taken_at_local_date, uploaded_at, thumbhash, in_timeline,
               visible_in_timeline, lifecycle, duplicate_of, trashed_at,
               purge_after, library_path
             FROM assets
             WHERE lifecycle = 'trashed'
             ORDER BY trashed_at DESC",
        )
    }

    pub fn list_duplicates(&self) -> Result<Vec<DuplicatePair>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT
                   d.id, d.hash, d.original_name, d.ext, d.size, d.width, d.height,
                   d.aspect_ratio, d.taken_at, d.taken_at_local_date, d.uploaded_at,
                   d.thumbhash, d.in_timeline, d.visible_in_timeline, d.lifecycle,
                   d.duplicate_of, d.trashed_at, d.purge_after, d.library_path,
                   o.id, o.hash, o.original_name, o.ext, o.size, o.width, o.height,
                   o.aspect_ratio, o.taken_at, o.taken_at_local_date, o.uploaded_at,
                   o.thumbhash, o.in_timeline, o.visible_in_timeline, o.lifecycle,
                   o.duplicate_of, o.trashed_at, o.purge_after, o.library_path
                 FROM assets d
                 JOIN assets o ON o.id = d.duplicate_of
                 WHERE d.lifecycle = 'duplicate'
                   AND o.lifecycle != 'purging'
                 ORDER BY d.uploaded_at DESC",
            )?;
            let pairs = statement
                .query_map([], |row| {
                    Ok(DuplicatePair {
                        duplicate: asset_from_row(row, 0)?,
                        original: asset_from_row(row, 19)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(pairs)
        })
    }

    /// 最小のスタックと第 1 章を作る。ページ追加は [`Self::add_to_stack`] で行う。
    pub fn create_stack(&self, title: &str) -> Result<StackLocation> {
        let stack_id = Uuid::now_v7().to_string();
        let chapter_id = Uuid::now_v7().to_string();
        let now = timestamp(Utc::now());
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO manga_stacks(id, title, cover_asset_id, created_at, updated_at)
                 VALUES (?1, ?2, NULL, ?3, ?3)",
                params![stack_id, title, now],
            )?;
            transaction.execute(
                "INSERT INTO stack_chapters(id, stack_id, chapter_no, title)
                 VALUES (?1, ?2, 1, NULL)",
                params![chapter_id, stack_id],
            )?;
            transaction.commit()?;
            Ok(StackLocation {
                stack_id,
                chapter_id,
            })
        })
    }

    /// ページ追加と duplicate の active 昇格、可視フラグ更新を同一 transaction で行う。
    pub fn add_to_stack(
        &self,
        location: &StackLocation,
        asset_id: &str,
        page_no: u32,
        show_in_timeline: bool,
    ) -> Result<()> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO stack_pages(
                    stack_id, chapter_id, asset_id, page_no, show_in_timeline
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    location.stack_id,
                    location.chapter_id,
                    asset_id,
                    page_no,
                    show_in_timeline
                ],
            )?;
            let changed = transaction.execute(
                "UPDATE assets
                 SET lifecycle = CASE
                       WHEN lifecycle = 'duplicate' THEN 'active'
                       ELSE lifecycle
                     END,
                     purge_after = CASE
                       WHEN lifecycle = 'duplicate' THEN NULL
                       ELSE purge_after
                     END,
                     visible_in_timeline = CASE
                       WHEN lifecycle IN ('active','duplicate')
                        AND in_timeline = 1
                        AND NOT EXISTS (
                          SELECT 1 FROM stack_pages sp
                          WHERE sp.asset_id = assets.id
                            AND sp.show_in_timeline = 0
                        )
                       THEN 1 ELSE 0
                     END
                 WHERE id = ?1
                   AND lifecycle IN ('active','duplicate','trashed')",
                [asset_id],
            )?;
            if changed == 0 {
                return Err(Error::AssetNotFound);
            }
            transaction.commit()?;
            Ok(())
        })
    }

    fn get_with_connection(
        &self,
        connection: &rusqlite::Connection,
        id: &str,
    ) -> Result<Option<Asset>> {
        connection
            .query_row(
                "SELECT
                   id, hash, original_name, ext, size, width, height, aspect_ratio,
                   taken_at, taken_at_local_date, uploaded_at, thumbhash, in_timeline,
                   visible_in_timeline, lifecycle, duplicate_of, trashed_at,
                   purge_after, library_path
                 FROM assets
                 WHERE id = ?1 AND lifecycle != 'purging'",
                [id],
                |row| asset_from_row(row, 0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn list_assets(&self, sql: &str) -> Result<Vec<Asset>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(sql)?;
            let assets = statement
                .query_map([], |row| asset_from_row(row, 0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(assets)
        })
    }
}

fn asset_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Asset> {
    let lifecycle: String = row.get(offset + 14)?;
    Ok(Asset {
        id: row.get(offset)?,
        hash: row.get(offset + 1)?,
        original_name: row.get(offset + 2)?,
        ext: row.get(offset + 3)?,
        size: row.get(offset + 4)?,
        width: row.get::<_, u32>(offset + 5)?,
        height: row.get::<_, u32>(offset + 6)?,
        aspect_ratio: row.get(offset + 7)?,
        taken_at: row.get(offset + 8)?,
        taken_at_local_date: row.get(offset + 9)?,
        uploaded_at: row.get(offset + 10)?,
        thumbhash: row.get(offset + 11)?,
        in_timeline: row.get(offset + 12)?,
        visible_in_timeline: row.get(offset + 13)?,
        lifecycle: Lifecycle::from_db(&lifecycle)?,
        duplicate_of: row.get(offset + 15)?,
        trashed_at: row.get(offset + 16)?,
        purge_after: row.get(offset + 17)?,
        library_path: row.get(offset + 18)?,
    })
}

fn normalized_extension(original_name: &str) -> Result<String> {
    let extension = Path::new(original_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpeg" => Ok("jpg".to_owned()),
        "jpg" | "png" | "webp" | "avif" | "gif" => Ok(extension),
        _ => Err(Error::UnsupportedExtension(extension)),
    }
}

fn library_path(id: &str, ext: &str, year: String, month: String) -> PathBuf {
    PathBuf::from("library")
        .join(year)
        .join(month)
        .join(format!("{id}.{ext}"))
}

fn relative_path_to_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or(Error::InvalidAssetPath)
}

pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn add_days(value: DateTime<Utc>, days: u32) -> String {
    timestamp(value + Duration::days(i64::from(days)))
}
