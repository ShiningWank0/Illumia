//! Bound-parameter-only cross-entity search.

use crate::{
    assets::{Asset, AssetService},
    db::{Database, Error, Result},
    stacks::{MAX_SEARCH_BYTES, MAX_SEARCH_CHARS, StackService, StackSummary},
};

const MAX_ASSET_RESULTS: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    pub assets: Vec<Asset>,
    pub stacks: Vec<StackSummary>,
}

#[derive(Clone, Debug)]
pub struct SearchService {
    database: Database,
}

impl SearchService {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn search(&self, query: &str) -> Result<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchResult {
                assets: Vec::new(),
                stacks: Vec::new(),
            });
        }
        if query.len() > MAX_SEARCH_BYTES || query.chars().count() > MAX_SEARCH_CHARS {
            return Err(Error::InvalidSearch);
        }

        let ids = self.asset_ids(query)?;
        let assets = {
            let service = AssetService::new(self.database.clone());
            let mut assets = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(asset) = service.get(&id)? {
                    assets.push(asset);
                }
            }
            assets
        };
        let stacks = StackService::new(self.database.clone()).search(query)?;
        Ok(SearchResult { assets, stacks })
    }

    fn asset_ids(&self, query: &str) -> Result<Vec<String>> {
        self.database.with_connection(|connection| {
            let short_query = query.chars().count() < 3;
            let predicate = if short_query {
                "f.text LIKE '%' || ?1 || '%' ESCAPE '\\'"
            } else {
                "search_fts MATCH ?1"
            };
            let sql = format!(
                "SELECT a.id
                 FROM search_fts f
                 JOIN assets a ON a.id = f.entity_id
                 WHERE f.entity_type = 'asset'
                   AND {predicate}
                   AND a.lifecycle = 'active'
                   AND a.visible_in_timeline = 1
                 ORDER BY a.taken_at DESC
                 LIMIT {MAX_ASSET_RESULTS}"
            );
            let parameter = if short_query {
                escape_like(query)
            } else {
                fts_phrase(query)
            };
            let mut statement = connection.prepare(&sql)?;
            let ids = statement
                .query_map([parameter], |row| row.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(ids)
        })
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
