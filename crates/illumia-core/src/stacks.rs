//! 漫画スタックの構造編集とタイムライン可視性の維持。

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use crate::{
    assets::{Asset, asset_from_row, timestamp},
    db::{Database, Error, Result},
};

pub const MAX_STACK_TITLE_CHARS: usize = 512;
pub const MAX_STACK_TITLE_BYTES: usize = 2048;
pub const MAX_STACK_CHAPTERS: usize = 1000;
pub const MAX_STACK_PAGES: usize = 10_000;
pub const MAX_STACK_SEARCH_RESULTS: usize = 200;
pub const MAX_SEARCH_CHARS: usize = 256;
pub const MAX_SEARCH_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct StackSummary {
    pub id: String,
    pub title: String,
    pub cover_asset_id: Option<String>,
    pub chapter_count: u32,
    pub page_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MangaStack {
    pub id: String,
    pub title: String,
    pub cover_asset_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub chapters: Vec<StackChapter>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackChapter {
    pub id: String,
    pub chapter_no: u32,
    pub title: Option<String>,
    pub pages: Vec<StackPage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StackPage {
    pub page_no: u32,
    pub show_in_timeline: bool,
    pub asset: Asset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChapterInput {
    pub title: Option<String>,
    pub pages: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StackService {
    database: Database,
}

impl StackService {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn create(&self, title: &str, asset_ids: &[String]) -> Result<MangaStack> {
        validate_title(title)?;
        validate_page_count(asset_ids.len())?;
        validate_unique_asset_ids(asset_ids)?;
        let stack_id = Uuid::now_v7().to_string();
        let chapter_id = Uuid::now_v7().to_string();
        let now = timestamp(Utc::now());
        self.database.with_connection_mut(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            validate_assets(&transaction, asset_ids)?;
            transaction.execute(
                "INSERT INTO manga_stacks(id, title, cover_asset_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![stack_id, title, asset_ids.first(), now],
            )?;
            transaction.execute(
                "INSERT INTO stack_chapters(id, stack_id, chapter_no, title)
                 VALUES (?1, ?2, 1, NULL)",
                params![chapter_id, stack_id],
            )?;
            insert_pages(
                &transaction,
                &stack_id,
                &chapter_id,
                asset_ids,
                1,
                &HashMap::new(),
            )?;
            promote_duplicates(&transaction, asset_ids)?;
            recompute_visibility(&transaction, asset_ids)?;
            transaction.commit()?;
            get_with_connection(connection, &stack_id)?.ok_or(Error::StackNotFound)
        })
    }

    pub fn list(&self) -> Result<Vec<StackSummary>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT s.id, s.title, s.cover_asset_id,
                        (SELECT count(*) FROM stack_chapters c WHERE c.stack_id = s.id),
                        (SELECT count(*) FROM stack_pages p WHERE p.stack_id = s.id),
                        s.created_at, s.updated_at
                 FROM manga_stacks s
                 ORDER BY s.updated_at DESC, s.id DESC
                 LIMIT 1000",
            )?;
            let stacks = statement
                .query_map([], |row| {
                    Ok(StackSummary {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        cover_asset_id: row.get(2)?,
                        chapter_count: row.get(3)?,
                        page_count: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(stacks)
        })
    }

    pub fn search(&self, query: &str) -> Result<Vec<StackSummary>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        validate_search_query(query)?;
        self.database.with_connection(|connection| {
            let short_query = query.chars().count() < 3;
            let predicate = if short_query {
                "f.text LIKE '%' || ?1 || '%' ESCAPE '\\'"
            } else {
                "search_fts MATCH ?1"
            };
            let sql = format!(
                "SELECT s.id, s.title, s.cover_asset_id,
                        (SELECT count(*) FROM stack_chapters c WHERE c.stack_id = s.id),
                        (SELECT count(*) FROM stack_pages p WHERE p.stack_id = s.id),
                        s.created_at, s.updated_at
                 FROM search_fts f
                 JOIN manga_stacks s ON s.id = f.entity_id
                 WHERE f.entity_type = 'stack' AND {predicate}
                 ORDER BY s.updated_at DESC, s.id DESC
                 LIMIT {MAX_STACK_SEARCH_RESULTS}"
            );
            let parameter = if short_query {
                escape_like(query)
            } else {
                format!("\"{}\"", query.replace('"', "\"\""))
            };
            let mut statement = connection.prepare(&sql)?;
            let stacks = statement
                .query_map([parameter], |row| {
                    Ok(StackSummary {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        cover_asset_id: row.get(2)?,
                        chapter_count: row.get(3)?,
                        page_count: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(stacks)
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<MangaStack>> {
        self.database
            .with_connection(|connection| get_with_connection(connection, id))
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<MangaStack> {
        self.update_metadata(id, Some(title), None)
    }

    pub fn set_cover(&self, id: &str, asset_id: &str) -> Result<MangaStack> {
        self.update_metadata(id, None, Some(asset_id))
    }

    pub fn update_metadata(
        &self,
        id: &str,
        title: Option<&str>,
        cover_asset_id: Option<&str>,
    ) -> Result<MangaStack> {
        if let Some(title) = title {
            validate_title(title)?;
        }
        self.update_stack(id, |transaction, now| {
            ensure_stack_exists(transaction, id)?;
            if let Some(cover_asset_id) = cover_asset_id {
                let belongs: bool = transaction.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM stack_pages WHERE stack_id = ?1 AND asset_id = ?2
                     )",
                    params![id, cover_asset_id],
                    |row| row.get(0),
                )?;
                if !belongs {
                    return Err(Error::InvalidStack(
                        "cover asset must be a page in the stack".to_owned(),
                    ));
                }
            }
            transaction.execute(
                "UPDATE manga_stacks
                 SET title = coalesce(?2, title),
                     cover_asset_id = coalesce(?3, cover_asset_id),
                     updated_at = ?4
                 WHERE id = ?1",
                params![id, title, cover_asset_id, now],
            )?;
            Ok(())
        })
    }

    pub fn delete_stack(&self, id: &str) -> Result<()> {
        self.database.with_connection_mut(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let affected = stack_asset_ids(&transaction, id)?;
            let changed = transaction.execute("DELETE FROM manga_stacks WHERE id = ?1", [id])?;
            ensure_stack_changed(changed)?;
            recompute_visibility(&transaction, &affected)?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn replace_structure(&self, id: &str, chapters: &[ChapterInput]) -> Result<MangaStack> {
        if chapters.is_empty() {
            return Err(Error::InvalidStack(
                "a stack must contain at least one chapter".to_owned(),
            ));
        }
        if chapters.len() > MAX_STACK_CHAPTERS {
            return Err(Error::InvalidStack("too many chapters".to_owned()));
        }
        for chapter in chapters {
            if let Some(title) = chapter.title.as_deref() {
                validate_optional_title(title)?;
            }
        }
        let new_asset_ids = chapters
            .iter()
            .flat_map(|chapter| chapter.pages.iter().cloned())
            .collect::<Vec<_>>();
        validate_page_count(new_asset_ids.len())?;
        validate_unique_asset_ids(&new_asset_ids)?;

        self.update_stack(id, |transaction, now| {
            ensure_stack_exists(transaction, id)?;
            validate_assets(transaction, &new_asset_ids)?;
            let old_asset_ids = stack_asset_ids(transaction, id)?;
            let old_flags = stack_page_flags(transaction, id)?;

            transaction.execute("DELETE FROM stack_chapters WHERE stack_id = ?1", [id])?;
            for (chapter_index, chapter) in chapters.iter().enumerate() {
                let chapter_id = Uuid::now_v7().to_string();
                let chapter_no = one_based(chapter_index)?;
                transaction.execute(
                    "INSERT INTO stack_chapters(id, stack_id, chapter_no, title)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![chapter_id, id, chapter_no, chapter.title],
                )?;
                insert_pages(transaction, id, &chapter_id, &chapter.pages, 1, &old_flags)?;
            }

            promote_duplicates(transaction, &new_asset_ids)?;
            let affected = union_ids(&old_asset_ids, &new_asset_ids);
            recompute_visibility(transaction, &affected)?;
            transaction.execute(
                "UPDATE manga_stacks
                 SET cover_asset_id = CASE
                       WHEN cover_asset_id IN (
                         SELECT asset_id FROM stack_pages WHERE stack_id = ?1
                       ) THEN cover_asset_id
                       ELSE (
                         SELECT p.asset_id
                         FROM stack_pages p
                         JOIN stack_chapters c ON c.id = p.chapter_id
                         WHERE p.stack_id = ?1
                         ORDER BY c.chapter_no, p.page_no
                         LIMIT 1
                       )
                     END,
                     updated_at = ?2
                 WHERE id = ?1",
                params![id, now],
            )?;
            Ok(())
        })
    }

    pub fn add_pages(
        &self,
        id: &str,
        asset_ids: &[String],
        chapter_id: Option<&str>,
    ) -> Result<MangaStack> {
        if asset_ids.is_empty() {
            return Err(Error::InvalidStack(
                "at least one asset is required".to_owned(),
            ));
        }
        validate_page_count(asset_ids.len())?;
        validate_unique_asset_ids(asset_ids)?;
        self.update_stack(id, |transaction, now| {
            ensure_stack_exists(transaction, id)?;
            let existing_pages: i64 = transaction.query_row(
                "SELECT count(*) FROM stack_pages WHERE stack_id = ?1",
                [id],
                |row| row.get(0),
            )?;
            let existing_pages = usize::try_from(existing_pages)
                .map_err(|_| Error::InvalidStack("invalid page count".to_owned()))?;
            let total_pages = existing_pages
                .checked_add(asset_ids.len())
                .ok_or_else(|| Error::InvalidStack("too many pages in a stack".to_owned()))?;
            validate_page_count(total_pages)?;
            validate_assets(transaction, asset_ids)?;
            let target_chapter = match chapter_id {
                Some(chapter_id) => {
                    let belongs: bool = transaction.query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM stack_chapters WHERE id = ?1 AND stack_id = ?2
                         )",
                        params![chapter_id, id],
                        |row| row.get(0),
                    )?;
                    if !belongs {
                        return Err(Error::StackChapterNotFound);
                    }
                    chapter_id.to_owned()
                }
                None => transaction
                    .query_row(
                        "SELECT id FROM stack_chapters
                         WHERE stack_id = ?1 ORDER BY chapter_no DESC LIMIT 1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(Error::StackChapterNotFound)?,
            };
            for asset_id in asset_ids {
                let existing: bool = transaction.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM stack_pages WHERE stack_id = ?1 AND asset_id = ?2
                     )",
                    params![id, asset_id],
                    |row| row.get(0),
                )?;
                if existing {
                    return Err(Error::InvalidStack(
                        "an asset can appear only once in a stack".to_owned(),
                    ));
                }
            }
            let first_page: u32 = transaction.query_row(
                "SELECT coalesce(max(page_no), 0) + 1
                 FROM stack_pages WHERE chapter_id = ?1",
                [&target_chapter],
                |row| row.get(0),
            )?;
            insert_pages(
                transaction,
                id,
                &target_chapter,
                asset_ids,
                first_page,
                &HashMap::new(),
            )?;
            promote_duplicates(transaction, asset_ids)?;
            recompute_visibility(transaction, asset_ids)?;
            transaction.execute(
                "UPDATE manga_stacks
                 SET cover_asset_id = coalesce(cover_asset_id, ?2), updated_at = ?3
                 WHERE id = ?1",
                params![id, asset_ids.first(), now],
            )?;
            Ok(())
        })
    }

    pub fn remove_page(&self, id: &str, asset_id: &str) -> Result<MangaStack> {
        self.update_stack(id, |transaction, now| {
            ensure_stack_exists(transaction, id)?;
            let changed = transaction.execute(
                "DELETE FROM stack_pages WHERE stack_id = ?1 AND asset_id = ?2",
                params![id, asset_id],
            )?;
            if changed == 0 {
                return Err(Error::AssetNotFound);
            }
            renumber_pages(transaction, id)?;
            recompute_visibility(transaction, &[asset_id.to_owned()])?;
            transaction.execute(
                "UPDATE manga_stacks
                 SET cover_asset_id = CASE
                       WHEN cover_asset_id = ?2 THEN (
                         SELECT p.asset_id
                         FROM stack_pages p
                         JOIN stack_chapters c ON c.id = p.chapter_id
                         WHERE p.stack_id = ?1
                         ORDER BY c.chapter_no, p.page_no LIMIT 1
                       )
                       ELSE cover_asset_id
                     END,
                     updated_at = ?3
                 WHERE id = ?1",
                params![id, asset_id, now],
            )?;
            Ok(())
        })
    }

    pub fn set_page_flag(
        &self,
        id: &str,
        asset_id: &str,
        show_in_timeline: bool,
    ) -> Result<MangaStack> {
        self.update_stack(id, |transaction, now| {
            ensure_stack_exists(transaction, id)?;
            let changed = transaction.execute(
                "UPDATE stack_pages SET show_in_timeline = ?3
                 WHERE stack_id = ?1 AND asset_id = ?2",
                params![id, asset_id, show_in_timeline],
            )?;
            if changed == 0 {
                return Err(Error::AssetNotFound);
            }
            recompute_visibility(transaction, &[asset_id.to_owned()])?;
            transaction.execute(
                "UPDATE manga_stacks SET updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            Ok(())
        })
    }

    fn update_stack(
        &self,
        id: &str,
        operation: impl FnOnce(&Transaction<'_>, &str) -> Result<()>,
    ) -> Result<MangaStack> {
        self.database.with_connection_mut(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = timestamp(Utc::now());
            operation(&transaction, &now)?;
            transaction.commit()?;
            get_with_connection(connection, id)?.ok_or(Error::StackNotFound)
        })
    }
}

fn get_with_connection(connection: &rusqlite::Connection, id: &str) -> Result<Option<MangaStack>> {
    let header = connection
        .query_row(
            "SELECT id, title, cover_asset_id, created_at, updated_at
             FROM manga_stacks WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, title, cover_asset_id, created_at, updated_at)) = header else {
        return Ok(None);
    };
    let mut chapter_statement = connection.prepare(
        "SELECT id, chapter_no, title
         FROM stack_chapters WHERE stack_id = ?1 ORDER BY chapter_no",
    )?;
    let chapter_rows = chapter_statement
        .query_map([&id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut chapters = Vec::with_capacity(chapter_rows.len());
    for (chapter_id, chapter_no, chapter_title) in chapter_rows {
        let mut page_statement = connection.prepare(
            "SELECT p.page_no, p.show_in_timeline,
                    a.id, a.hash, a.original_name, a.ext, a.size, a.width, a.height,
                    a.aspect_ratio, a.taken_at, a.taken_at_local_date, a.uploaded_at,
                    a.thumbhash, a.in_timeline, a.visible_in_timeline, a.lifecycle,
                    a.duplicate_of, a.trashed_at, a.purge_after, a.library_path
             FROM stack_pages p
             JOIN assets a ON a.id = p.asset_id
             WHERE p.stack_id = ?1 AND p.chapter_id = ?2
             ORDER BY p.page_no",
        )?;
        let pages = page_statement
            .query_map(params![id, chapter_id], |row| {
                Ok(StackPage {
                    page_no: row.get(0)?,
                    show_in_timeline: row.get(1)?,
                    asset: asset_from_row(row, 2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        chapters.push(StackChapter {
            id: chapter_id,
            chapter_no,
            title: chapter_title,
            pages,
        });
    }
    Ok(Some(MangaStack {
        id,
        title,
        cover_asset_id,
        created_at,
        updated_at,
        chapters,
    }))
}

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(Error::InvalidStack("title must not be empty".to_owned()));
    }
    validate_title_metadata(title)
}

fn validate_optional_title(title: &str) -> Result<()> {
    if title.is_empty() {
        Ok(())
    } else {
        validate_title_metadata(title)
    }
}

fn validate_title_metadata(title: &str) -> Result<()> {
    if title.len() > MAX_STACK_TITLE_BYTES
        || title.chars().count() > MAX_STACK_TITLE_CHARS
        || title.chars().any(char::is_control)
    {
        Err(Error::InvalidStack("stack title is invalid".to_owned()))
    } else {
        Ok(())
    }
}

fn validate_page_count(count: usize) -> Result<()> {
    if count <= MAX_STACK_PAGES {
        Ok(())
    } else {
        Err(Error::InvalidStack("too many pages in a stack".to_owned()))
    }
}

fn validate_unique_asset_ids(asset_ids: &[String]) -> Result<()> {
    let unique = asset_ids.iter().collect::<HashSet<_>>();
    if unique.len() == asset_ids.len() {
        Ok(())
    } else {
        Err(Error::InvalidStack(
            "an asset can appear only once in a stack".to_owned(),
        ))
    }
}

fn validate_search_query(query: &str) -> Result<()> {
    if query.len() > MAX_SEARCH_BYTES || query.chars().count() > MAX_SEARCH_CHARS {
        Err(Error::InvalidSearch)
    } else {
        Ok(())
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn validate_assets(transaction: &Transaction<'_>, asset_ids: &[String]) -> Result<()> {
    for asset_id in asset_ids {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM assets WHERE id = ?1 AND lifecycle != 'purging'
             )",
            [asset_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(Error::InvalidStack(
                "one or more assets do not exist".to_owned(),
            ));
        }
    }
    Ok(())
}

fn insert_pages(
    transaction: &Transaction<'_>,
    stack_id: &str,
    chapter_id: &str,
    asset_ids: &[String],
    first_page: u32,
    old_flags: &HashMap<String, bool>,
) -> Result<()> {
    for (index, asset_id) in asset_ids.iter().enumerate() {
        let page_no = first_page
            .checked_add(
                u32::try_from(index)
                    .map_err(|_| Error::InvalidStack("too many pages in a chapter".to_owned()))?,
            )
            .ok_or_else(|| Error::InvalidStack("too many pages in a chapter".to_owned()))?;
        let show_in_timeline = old_flags.get(asset_id).copied().unwrap_or(false);
        transaction.execute(
            "INSERT INTO stack_pages(
               stack_id, chapter_id, asset_id, page_no, show_in_timeline
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![stack_id, chapter_id, asset_id, page_no, show_in_timeline],
        )?;
    }
    Ok(())
}

fn promote_duplicates(transaction: &Transaction<'_>, asset_ids: &[String]) -> Result<()> {
    for asset_id in asset_ids {
        transaction.execute(
            "UPDATE assets
             SET lifecycle = 'active', purge_after = NULL
             WHERE id = ?1 AND lifecycle = 'duplicate'",
            [asset_id],
        )?;
    }
    Ok(())
}

fn recompute_visibility(transaction: &Transaction<'_>, asset_ids: &[String]) -> Result<()> {
    for asset_id in asset_ids {
        transaction.execute(
            "UPDATE assets
             SET visible_in_timeline = CASE
                   WHEN lifecycle = 'active'
                    AND in_timeline = 1
                    AND NOT EXISTS (
                      SELECT 1 FROM stack_pages
                      WHERE asset_id = assets.id AND show_in_timeline = 0
                    )
                   THEN 1 ELSE 0
                 END
             WHERE id = ?1",
            [asset_id],
        )?;
    }
    Ok(())
}

fn ensure_stack_exists(transaction: &Transaction<'_>, id: &str) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM manga_stacks WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(Error::StackNotFound)
    }
}

fn ensure_stack_changed(changed: usize) -> Result<()> {
    if changed == 0 {
        Err(Error::StackNotFound)
    } else {
        Ok(())
    }
}

fn stack_asset_ids(transaction: &Transaction<'_>, id: &str) -> Result<Vec<String>> {
    let mut statement =
        transaction.prepare("SELECT asset_id FROM stack_pages WHERE stack_id = ?1")?;
    let ids = statement
        .query_map([id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn stack_page_flags(transaction: &Transaction<'_>, id: &str) -> Result<HashMap<String, bool>> {
    let mut statement = transaction
        .prepare("SELECT asset_id, show_in_timeline FROM stack_pages WHERE stack_id = ?1")?;
    let flags = statement
        .query_map([id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<std::result::Result<HashMap<_, _>, _>>()?;
    Ok(flags)
}

fn union_ids(left: &[String], right: &[String]) -> Vec<String> {
    left.iter()
        .chain(right)
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

fn one_based(index: usize) -> Result<u32> {
    u32::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| Error::InvalidStack("too many chapters".to_owned()))
}

fn renumber_pages(transaction: &Transaction<'_>, stack_id: &str) -> Result<()> {
    let mut chapters = transaction.prepare("SELECT id FROM stack_chapters WHERE stack_id = ?1")?;
    let chapter_ids = chapters
        .query_map([stack_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(chapters);
    for chapter_id in chapter_ids {
        let mut pages = transaction.prepare(
            "SELECT asset_id FROM stack_pages
             WHERE chapter_id = ?1 ORDER BY page_no",
        )?;
        let asset_ids = pages
            .query_map([&chapter_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(pages);
        for (index, asset_id) in asset_ids.iter().enumerate() {
            transaction.execute(
                "UPDATE stack_pages SET page_no = ?3
                 WHERE chapter_id = ?1 AND asset_id = ?2",
                params![chapter_id, asset_id, one_based(index)?],
            )?;
        }
    }
    Ok(())
}
