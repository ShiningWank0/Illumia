//! パージ対象選定と物理削除。
//!
//! 物理削除関数はこのモジュール内かつ crate 内可視性に限定する。

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, named_params};
use uuid::Uuid;

use crate::{
    assets::timestamp,
    db::{Database, Error, Result},
};

const DUE_ASSETS_SQL: &str = "
SELECT id FROM assets
WHERE lifecycle IN ('duplicate','trashed')
  AND purge_after IS NOT NULL
  AND purge_after < :now
  AND NOT EXISTS (SELECT 1 FROM stack_pages sp WHERE sp.asset_id = assets.id)
  AND NOT EXISTS (SELECT 1 FROM assets d WHERE d.duplicate_of = assets.id);
";

#[derive(Clone, Debug)]
pub struct PurgeService {
    database: Database,
}

impl PurgeService {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn run_due(&self) -> Result<usize> {
        self.run_due_at(Utc::now())
    }

    pub fn run_due_at(&self, now: DateTime<Utc>) -> Result<usize> {
        self.resume_purging()?;
        let ids = self.mark_due_as_purging(now)?;
        let count = ids.len();
        for id in ids {
            self.finish_purging(&id)?;
        }
        Ok(count)
    }

    /// 起動時に `purging` tombstone を手順 2 から再開する。
    pub fn resume_purging(&self) -> Result<usize> {
        let ids = self.database.with_connection(|connection| {
            let mut statement =
                connection.prepare("SELECT id FROM assets WHERE lifecycle = 'purging'")?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(ids)
        })?;
        let count = ids.len();
        for id in ids {
            self.finish_purging(&id)?;
        }
        Ok(count)
    }

    /// ゴミ箱 UI からの明示操作専用。スタック参照がある行は拒否する。
    pub fn purge_now(&self, id: &str) -> Result<()> {
        let changed = self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let changed = transaction.execute(
                "UPDATE assets
                 SET lifecycle = 'purging', visible_in_timeline = 0
                 WHERE id = ?1
                   AND lifecycle IN ('duplicate','trashed')
                   AND NOT EXISTS (
                     SELECT 1 FROM stack_pages sp WHERE sp.asset_id = assets.id
                   )
                   AND NOT EXISTS (
                     SELECT 1 FROM assets d WHERE d.duplicate_of = assets.id
                   )",
                [id],
            )?;
            transaction.commit()?;
            Ok(changed)
        })?;
        if changed == 0 {
            return Err(Error::AssetNotFound);
        }
        self.finish_purging(id)
    }

    fn mark_due_as_purging(&self, now: DateTime<Utc>) -> Result<Vec<String>> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let ids = {
                let mut statement = transaction.prepare(DUE_ASSETS_SQL)?;
                statement
                    .query_map(named_params! {":now": timestamp(now)}, |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for id in &ids {
                transaction.execute(
                    "UPDATE assets
                     SET lifecycle = 'purging', visible_in_timeline = 0
                     WHERE id = ?1",
                    [id],
                )?;
            }
            transaction.commit()?;
            Ok(ids)
        })
    }

    fn finish_purging(&self, id: &str) -> Result<()> {
        if self.restore_referenced_legacy_tombstone(id)? {
            return Ok(());
        }
        let (library_path, blob_ids) = self.database.with_connection(|connection| {
            let library_path = connection
                .query_row(
                    "SELECT library_path FROM assets
                     WHERE id = ?1 AND lifecycle = 'purging'",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or(Error::AssetNotFound)?;
            let mut statement =
                connection.prepare("SELECT blob_id FROM vault_blobs WHERE asset_id = ?1")?;
            let blob_ids = statement
                .query_map([id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok((library_path, blob_ids))
        })?;

        let blob_paths = checked_vault_blob_paths(self.database.data_root(), &blob_ids)?;
        purge_asset_files(self.database.data_root(), id, &library_path)?;
        for path in blob_paths {
            remove_owned_file(&path)?;
        }

        self.database.with_connection(|connection| {
            connection.execute(
                "DELETE FROM assets WHERE id = ?1 AND lifecycle = 'purging'",
                [id],
            )?;
            Ok(())
        })
    }

    /// Older builds could mark a canonical row as `purging` even while duplicate rows
    /// still referenced it. Never resume physical deletion for that state: recover the
    /// original lifecycle from the retained trash/dedup metadata instead.
    fn restore_referenced_legacy_tombstone(&self, id: &str) -> Result<bool> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let changed = transaction.execute(
                "UPDATE assets
                 SET lifecycle = CASE
                       WHEN trashed_at IS NOT NULL THEN 'trashed'
                       WHEN duplicate_of IS NOT NULL THEN 'duplicate'
                       ELSE 'trashed'
                     END,
                     visible_in_timeline = 0
                 WHERE id = ?1
                   AND lifecycle = 'purging'
                   AND EXISTS (SELECT 1 FROM assets d WHERE d.duplicate_of = assets.id)",
                [id],
            )?;
            transaction.commit()?;
            Ok(changed != 0)
        })
    }
}

pub(crate) fn purge_asset_files(data_root: &Path, id: &str, library_path: &str) -> Result<()> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidAssetPath)?;
    let library_path = checked_relative_path(library_path)?;
    remove_owned_file(&data_root.join(library_path))?;
    remove_owned_file(&data_root.join("thumbs").join(format!("{id}_t.webp")))?;
    remove_owned_file(&data_root.join("thumbs").join(format!("{id}_p.webp")))?;
    Ok(())
}

/// `vault: no-log`
fn checked_vault_blob_paths(data_root: &Path, blob_ids: &[String]) -> Result<Vec<PathBuf>> {
    blob_ids
        .iter()
        .map(|blob_id| {
            Uuid::parse_str(blob_id).map_err(|_| Error::InvalidVaultBlob)?;
            Ok(data_root.join("vault").join("blobs").join(blob_id))
        })
        .collect()
}

fn checked_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidAssetPath);
    }
    Ok(path.to_path_buf())
}

fn remove_owned_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
