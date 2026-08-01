//! ML result persistence, clustering orchestration, and character review operations.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Component, Path},
};

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    assets::{Asset, AssetService, timestamp},
    db::{Database, Error, Result},
    jobs::{Job, JobQueue},
    ml_client::{Analysis, Assignment, ClusterMode, ClusterParams, ClusterRequest, MlClient},
    settings::{QualityGate, Settings},
};

pub const ML_ANALYZE_JOB_KIND: &str = "ml_analyze";
pub const ML_RECLUSTER_JOB_KIND: &str = "ml_recluster";
pub const ML_ANALYZE_PRIORITY: i64 = 10;
pub const ML_RECLUSTER_PRIORITY: i64 = 5;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MlAnalyzePayload {
    pub asset_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MlReclusterPayload {}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterSummary {
    pub id: String,
    pub name: Option<String>,
    pub cover_face_id: Option<String>,
    pub asset_count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FaceRecord {
    pub id: String,
    pub asset_id: String,
    pub kind: String,
    pub bbox: [f64; 4],
    pub det_conf: f64,
    pub quality_flags: Vec<String>,
    pub model_version: String,
    pub cluster_id: Option<String>,
    pub state: String,
    pub similarity: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewCandidate {
    pub face: FaceRecord,
    pub asset: Asset,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClusterMedoid {
    pub face_id: String,
    pub cluster_id: String,
    pub embedding: Vec<f32>,
    pub confirmed: bool,
}

#[derive(Clone, Debug)]
pub struct MlService {
    database: Database,
    client: MlClient,
}

impl MlService {
    #[must_use]
    pub fn new(database: Database, client: MlClient) -> Self {
        Self { database, client }
    }

    #[must_use]
    pub fn client(&self) -> &MlClient {
        &self.client
    }

    pub fn handle_analyze_job(&self, job: &Job) -> Result<()> {
        let payload: MlAnalyzePayload = serde_json::from_str(&job.payload)?;
        let Some(asset) = AssetService::new(self.database.clone()).get(&payload.asset_id)? else {
            return Ok(());
        };
        if let Some(model_version) = self
            .client
            .health()?
            .model_bundle
            .map(|bundle| bundle.version)
            && self.has_model_analysis(&payload.asset_id, &model_version)?
        {
            return Ok(());
        }
        let relative = Path::new(&asset.library_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(Error::InvalidAssetPath);
        }
        let bytes = fs::read(self.database.data_root().join(relative))?;
        self.analyze_bytes(&payload.asset_id, &bytes)
    }

    /// Persists an anonymous image analysis. Suitable for an unlocked vault.
    ///
    /// `vault: no-log` — callers must not log the asset identifier or image metadata.
    pub fn analyze_bytes(&self, asset_id: &str, bytes: &[u8]) -> Result<()> {
        if AssetService::new(self.database.clone())
            .get(asset_id)?
            .is_none()
        {
            return Err(Error::AssetNotFound);
        }
        let analysis = self.client.analyze(bytes)?;
        if self.has_model_analysis(asset_id, &analysis.model_version)? {
            return Ok(());
        }
        let inserted = self.persist_analysis(asset_id, analysis)?;
        if !inserted.is_empty() {
            self.assign_faces(&inserted)?;
        }
        Ok(())
    }

    pub fn handle_recluster_job(&self, job: &Job) -> Result<()> {
        let _: MlReclusterPayload = serde_json::from_str(&job.payload)?;
        self.recluster()
    }

    pub fn recluster(&self) -> Result<()> {
        let Some(model_version) = self.current_model_version()? else {
            return Ok(());
        };
        let rows = self.embedding_rows(&model_version, None)?;
        if rows.is_empty() {
            return Ok(());
        }
        let dimension = common_dimension(&rows)?;
        let ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let embeddings = rows
            .iter()
            .map(|row| row.embedding.clone())
            .collect::<Vec<_>>();
        let confirmed = rows
            .iter()
            .filter(|row| matches!(row.state.as_str(), "confirmed" | "rejected"))
            .map(|row| row.id.clone())
            .collect();
        let response = self.client.cluster(&ClusterRequest {
            mode: ClusterMode::Full,
            params: self.cluster_params()?,
            embeddings,
            shape: [ids.len(), dimension],
            ids,
            medoids: None,
            rejections: self.rejections(None)?,
            confirmed,
        })?;
        self.apply_full_clustering(response.assignments, response.new_clusters)
    }

    pub fn cluster_medoids(&self, model_version: &str) -> Result<Vec<ClusterMedoid>> {
        let rows = self.embedding_rows(model_version, Some(true))?;
        let mut grouped = BTreeMap::<String, Vec<EmbeddingRow>>::new();
        for row in rows {
            if let Some(cluster_id) = &row.cluster_id {
                grouped.entry(cluster_id.clone()).or_default().push(row);
            }
        }
        let mut output = Vec::new();
        for (cluster_id, rows) in grouped {
            let dimension = common_dimension(&rows)?;
            let mut center = vec![0.0_f32; dimension];
            for row in &rows {
                for (sum, value) in center.iter_mut().zip(&row.embedding) {
                    *sum += *value;
                }
            }
            let divisor = rows.len() as f32;
            for value in &mut center {
                *value /= divisor;
            }
            let mut ranked = rows;
            ranked.sort_by(|left, right| {
                let left_confirmed = left.state == "confirmed";
                let right_confirmed = right.state == "confirmed";
                right_confirmed.cmp(&left_confirmed).then_with(|| {
                    cosine(&right.embedding, &center)
                        .partial_cmp(&cosine(&left.embedding, &center))
                        .unwrap_or(Ordering::Equal)
                })
            });
            output.extend(ranked.into_iter().take(5).map(|row| ClusterMedoid {
                face_id: row.id,
                cluster_id: cluster_id.clone(),
                embedding: row.embedding,
                confirmed: row.state == "confirmed",
            }));
        }
        Ok(output)
    }

    pub fn list_clusters(&self) -> Result<Vec<ClusterSummary>> {
        self.cluster_summaries(None, true)
    }

    pub fn search_named_clusters(&self, query: &str) -> Result<Vec<ClusterSummary>> {
        self.cluster_summaries(Some(query), true)
    }

    pub fn cluster_assets(&self, cluster_id: &str) -> Result<Vec<Asset>> {
        self.ensure_cluster(cluster_id)?;
        let ids = self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT DISTINCT asset_id FROM faces
                 WHERE cluster_id = ?1 AND state IN ('auto','confirmed')
                 ORDER BY asset_id LIMIT 10000",
            )?;
            Ok(statement
                .query_map([cluster_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })?;
        let assets = AssetService::new(self.database.clone());
        ids.into_iter()
            .filter_map(|id| match assets.get(&id) {
                Ok(Some(asset)) => Some(Ok(asset)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub fn rename_cluster(&self, cluster_id: &str, name: &str) -> Result<ClusterSummary> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 256 {
            return Err(Error::InvalidMl(
                "cluster name must contain 1 to 256 characters".into(),
            ));
        }
        let changed = self.database.with_connection(|connection| {
            Ok(connection.execute(
                "UPDATE clusters SET name = ?2 WHERE id = ?1",
                params![cluster_id, name],
            )?)
        })?;
        if changed == 0 {
            return Err(Error::ClusterNotFound);
        }
        self.cluster_summary(cluster_id)
    }

    pub fn merge_clusters(&self, from_id: &str, into_id: &str) -> Result<ClusterSummary> {
        if from_id == into_id {
            return Err(Error::InvalidMl("clusters must be different".into()));
        }
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            for id in [from_id, into_id] {
                if !transaction
                    .query_row("SELECT 1 FROM clusters WHERE id = ?1", [id], |_| Ok(()))
                    .optional()?
                    .is_some()
                {
                    return Err(Error::ClusterNotFound);
                }
            }
            transaction.execute(
                "DELETE FROM cluster_rejections
                 WHERE cluster_id = ?1 AND face_id IN
                   (SELECT id FROM faces WHERE cluster_id = ?2)",
                params![into_id, from_id],
            )?;
            transaction.execute(
                "UPDATE faces SET cluster_id = ?1 WHERE cluster_id = ?2",
                params![into_id, from_id],
            )?;
            transaction.execute("DELETE FROM clusters WHERE id = ?1", [from_id])?;
            transaction.commit()?;
            Ok(())
        })?;
        self.refresh_cluster_cover(into_id)?;
        self.cluster_summary(into_id)
    }

    pub fn split_cluster(&self, cluster_id: &str, face_ids: &[String]) -> Result<ClusterSummary> {
        if face_ids.is_empty() || face_ids.len() > 10_000 {
            return Err(Error::InvalidMl("face_ids must not be empty".into()));
        }
        let unique = face_ids.iter().collect::<HashSet<_>>();
        if unique.len() != face_ids.len() {
            return Err(Error::InvalidMl("face_ids must be unique".into()));
        }
        let new_id = Uuid::now_v7().to_string();
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            if transaction
                .query_row("SELECT 1 FROM clusters WHERE id = ?1", [cluster_id], |_| {
                    Ok(())
                })
                .optional()?
                .is_none()
            {
                return Err(Error::ClusterNotFound);
            }
            let mut found = 0_usize;
            {
                let mut statement =
                    transaction.prepare("SELECT cluster_id FROM faces WHERE id = ?1")?;
                for face_id in face_ids {
                    let current = statement
                        .query_row([face_id], |row| row.get::<_, Option<String>>(0))
                        .optional()?
                        .ok_or(Error::FaceNotFound)?;
                    if current.as_deref() != Some(cluster_id) {
                        return Err(Error::InvalidMl("face does not belong to cluster".into()));
                    }
                    found += 1;
                }
            }
            if found != face_ids.len() {
                return Err(Error::FaceNotFound);
            }
            transaction.execute(
                "INSERT INTO clusters(id, name, cover_face_id, created_by, created_at)
                 VALUES (?1, NULL, ?2, 'user', ?3)",
                params![new_id, face_ids[0], timestamp(Utc::now())],
            )?;
            for face_id in face_ids {
                transaction.execute(
                    "UPDATE faces SET cluster_id = ?2, state = 'confirmed', similarity = NULL
                     WHERE id = ?1",
                    params![face_id, new_id],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })?;
        self.refresh_cluster_cover(cluster_id)?;
        self.cluster_summary(&new_id)
    }

    pub fn review_candidates(&self) -> Result<Vec<ReviewCandidate>> {
        let faces = self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, asset_id, kind, bbox, det_conf, quality_flags, model_version,
                        cluster_id, state, similarity
                 FROM faces WHERE state = 'candidate'
                 ORDER BY similarity DESC, id LIMIT 1000",
            )?;
            Ok(statement
                .query_map([], face_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })?;
        let assets = AssetService::new(self.database.clone());
        faces
            .into_iter()
            .map(|face| {
                let asset = assets.get(&face.asset_id)?.ok_or(Error::AssetNotFound)?;
                Ok(ReviewCandidate { face, asset })
            })
            .collect()
    }

    pub fn review_candidate(&self, face_id: &str, accept: bool) -> Result<FaceRecord> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            let cluster_id = transaction
                .query_row(
                    "SELECT cluster_id FROM faces WHERE id = ?1 AND state = 'candidate'",
                    [face_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or(Error::FaceNotFound)?;
            let cluster_id = cluster_id.ok_or_else(|| {
                Error::InvalidMl("candidate does not have a proposed cluster".into())
            })?;
            if accept {
                transaction.execute(
                    "UPDATE faces SET state = 'confirmed' WHERE id = ?1 AND state = 'candidate'",
                    [face_id],
                )?;
            } else {
                transaction.execute(
                    "INSERT OR IGNORE INTO cluster_rejections(face_id, cluster_id)
                     VALUES (?1, ?2)",
                    params![face_id, cluster_id],
                )?;
                transaction.execute(
                    "UPDATE faces SET state = 'rejected', cluster_id = NULL
                     WHERE id = ?1 AND state = 'candidate'",
                    [face_id],
                )?;
            }
            let face = transaction.query_row(
                "SELECT id, asset_id, kind, bbox, det_conf, quality_flags, model_version,
                        cluster_id, state, similarity FROM faces WHERE id = ?1",
                [face_id],
                face_from_row,
            )?;
            transaction.commit()?;
            Ok(face)
        })
    }

    fn has_model_analysis(&self, asset_id: &str, model_version: &str) -> Result<bool> {
        self.database.with_connection(|connection| {
            Ok(connection
                .query_row(
                    "SELECT 1 FROM faces WHERE asset_id = ?1 AND model_version = ?2 LIMIT 1",
                    params![asset_id, model_version],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
    }

    fn persist_analysis(&self, asset_id: &str, analysis: Analysis) -> Result<Vec<String>> {
        let quality_gate = Settings::new(self.database.clone()).quality_gate()?;
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            if transaction
                .query_row(
                    "SELECT 1 FROM faces WHERE asset_id = ?1 AND model_version = ?2 LIMIT 1",
                    params![asset_id, analysis.model_version],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
            {
                transaction.commit()?;
                return Ok(Vec::new());
            }
            let mut inserted = Vec::new();
            for instance in analysis.instances {
                if !matches!(instance.kind.as_str(), "person" | "head" | "face") {
                    return Err(Error::InvalidMl("unsupported detection kind".into()));
                }
                if quality_gate == QualityGate::Strict && !instance.quality_passed {
                    continue;
                }
                let id = Uuid::now_v7().to_string();
                transaction.execute(
                    "INSERT INTO faces(
                       id, asset_id, kind, bbox, det_conf, quality_flags, embedding,
                       model_version, cluster_id, state, similarity
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'unassigned', NULL)",
                    params![
                        id,
                        asset_id,
                        instance.kind,
                        serde_json::to_string(&instance.bbox)?,
                        instance.det_conf,
                        serde_json::to_string(&instance.quality_flags)?,
                        instance.embedding,
                        analysis.model_version,
                    ],
                )?;
                inserted.push(id);
            }
            transaction.commit()?;
            Ok(inserted)
        })
    }

    fn assign_faces(&self, face_ids: &[String]) -> Result<()> {
        let rows = self.rows_by_ids(face_ids)?;
        if rows.is_empty() {
            return Ok(());
        }
        let model_version = rows[0].model_version.clone();
        if rows.iter().any(|row| row.model_version != model_version) {
            return Err(Error::InvalidMl("mixed model versions".into()));
        }
        let dimension = common_dimension(&rows)?;
        let medoid_rows = self.cluster_medoids(&model_version)?;
        let mut medoids = BTreeMap::new();
        for medoid in medoid_rows {
            medoids.entry(medoid.cluster_id).or_insert(medoid.embedding);
        }
        let ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let response = self.client.cluster(&ClusterRequest {
            mode: ClusterMode::Assign,
            params: self.cluster_params()?,
            embeddings: rows.into_iter().map(|row| row.embedding).collect(),
            shape: [ids.len(), dimension],
            ids,
            medoids: Some(medoids),
            rejections: self.rejections(Some(face_ids))?,
            confirmed: Vec::new(),
        })?;
        self.apply_assignments(response.assignments, &HashMap::new())
    }

    fn apply_full_clustering(
        &self,
        assignments: Vec<Assignment>,
        new_clusters: Vec<crate::ml_client::NewCluster>,
    ) -> Result<()> {
        let minimum = usize::try_from(Settings::new(self.database.clone()).min_cluster_size()?)
            .map_err(|_| Error::InvalidMl("cluster size overflow".into()))?;
        let retained = new_clusters
            .into_iter()
            .filter(|cluster| cluster.member_ids.len() >= minimum)
            .collect::<Vec<_>>();
        let mut mapping = HashMap::new();
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            for cluster in &retained {
                let id = Uuid::now_v7().to_string();
                let cover = cluster.medoid_ids.first().cloned();
                transaction.execute(
                    "INSERT INTO clusters(id, name, cover_face_id, created_by, created_at)
                     VALUES (?1, NULL, ?2, 'auto', ?3)",
                    params![id, cover, timestamp(Utc::now())],
                )?;
                mapping.insert(cluster.tmp_id.clone(), id);
            }
            transaction.commit()?;
            Ok(())
        })?;
        self.apply_assignments(assignments, &mapping)?;
        for cluster_id in mapping.values() {
            self.refresh_cluster_cover(cluster_id)?;
        }
        Ok(())
    }

    fn apply_assignments(
        &self,
        assignments: Vec<Assignment>,
        mapping: &HashMap<String, String>,
    ) -> Result<()> {
        self.database.with_connection_mut(|connection| {
            let transaction = connection.transaction()?;
            for assignment in assignments {
                let current = transaction
                    .query_row(
                        "SELECT state FROM faces WHERE id = ?1",
                        [&assignment.id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                let Some(current) = current else { continue };
                if matches!(current.as_str(), "confirmed" | "rejected") {
                    continue;
                }
                let mut cluster_id = assignment
                    .cluster
                    .as_ref()
                    .map(|id| mapping.get(id).cloned().unwrap_or_else(|| id.clone()));
                if let Some(candidate) = &cluster_id {
                    let exists = transaction
                        .query_row("SELECT 1 FROM clusters WHERE id = ?1", [candidate], |_| {
                            Ok(())
                        })
                        .optional()?
                        .is_some();
                    let rejected = transaction
                        .query_row(
                            "SELECT 1 FROM cluster_rejections
                             WHERE face_id = ?1 AND cluster_id = ?2",
                            params![assignment.id, candidate],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some();
                    if !exists || rejected {
                        cluster_id = None;
                    }
                }
                let state = if cluster_id.is_none()
                    || !matches!(assignment.state.as_str(), "auto" | "candidate")
                {
                    "unassigned"
                } else {
                    assignment.state.as_str()
                };
                transaction.execute(
                    "UPDATE faces SET cluster_id = ?2, state = ?3, similarity = ?4
                     WHERE id = ?1 AND state NOT IN ('confirmed','rejected')",
                    params![assignment.id, cluster_id, state, assignment.similarity],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
    }

    fn cluster_params(&self) -> Result<ClusterParams> {
        let settings = Settings::new(self.database.clone());
        Ok(ClusterParams {
            tau_high: settings.tau_high_override()?,
            tau_low: settings.tau_low_override()?,
            min_cluster_size: Some(settings.min_cluster_size()?),
        })
    }

    fn current_model_version(&self) -> Result<Option<String>> {
        self.database.with_connection(|connection| {
            Ok(connection
                .query_row(
                    "SELECT model_version FROM faces WHERE embedding IS NOT NULL
                     GROUP BY model_version ORDER BY max(rowid) DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    fn embedding_rows(
        &self,
        model_version: &str,
        clustered_only: Option<bool>,
    ) -> Result<Vec<EmbeddingRow>> {
        self.database.with_connection(|connection| {
            let predicate = if clustered_only == Some(true) {
                " AND cluster_id IS NOT NULL AND state IN ('auto','confirmed')"
            } else {
                ""
            };
            let sql = format!(
                "SELECT id, embedding, model_version, cluster_id, state FROM faces
                 WHERE embedding IS NOT NULL AND model_version = ?1{predicate} ORDER BY id"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement
                .query_map([model_version], embedding_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter().map(decode_embedding_row).collect()
        })
    }

    fn rows_by_ids(&self, face_ids: &[String]) -> Result<Vec<EmbeddingRow>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, embedding, model_version, cluster_id, state FROM faces
                 WHERE id = ?1 AND embedding IS NOT NULL",
            )?;
            face_ids
                .iter()
                .map(|id| {
                    statement
                        .query_row([id], embedding_from_row)
                        .optional()?
                        .ok_or(Error::FaceNotFound)
                        .and_then(decode_embedding_row)
                })
                .collect()
        })
    }

    fn rejections(&self, face_ids: Option<&[String]>) -> Result<Vec<[String; 2]>> {
        self.database.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT face_id, cluster_id FROM cluster_rejections ORDER BY face_id, cluster_id",
            )?;
            let filter = face_ids.map(|ids| ids.iter().map(String::as_str).collect::<HashSet<_>>());
            Ok(statement
                .query_map([], |row| {
                    Ok([row.get::<_, String>(0)?, row.get::<_, String>(1)?])
                })?
                .filter_map(|row| match row {
                    Ok(pair)
                        if filter
                            .as_ref()
                            .is_none_or(|ids| ids.contains(pair[0].as_str())) =>
                    {
                        Some(Ok(pair))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    fn ensure_cluster(&self, cluster_id: &str) -> Result<()> {
        if self.database.with_connection(|connection| {
            Ok(connection
                .query_row("SELECT 1 FROM clusters WHERE id = ?1", [cluster_id], |_| {
                    Ok(())
                })
                .optional()?
                .is_some())
        })? {
            Ok(())
        } else {
            Err(Error::ClusterNotFound)
        }
    }

    fn cluster_summary(&self, cluster_id: &str) -> Result<ClusterSummary> {
        self.database.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT c.id, c.name, c.cover_face_id, COUNT(DISTINCT f.asset_id)
                     FROM clusters c LEFT JOIN faces f ON f.cluster_id = c.id
                       AND f.state IN ('auto','confirmed')
                     WHERE c.id = ?1 GROUP BY c.id",
                    [cluster_id],
                    cluster_summary_from_row,
                )
                .optional()?
                .ok_or(Error::ClusterNotFound)
        })
    }

    fn cluster_summaries(
        &self,
        query: Option<&str>,
        apply_minimum: bool,
    ) -> Result<Vec<ClusterSummary>> {
        let minimum = if apply_minimum {
            Settings::new(self.database.clone()).min_cluster_size()?
        } else {
            0
        };
        self.database.with_connection(|connection| {
            let mut output = Vec::new();
            let mut statement = connection.prepare(
                "SELECT c.id, c.name, c.cover_face_id, COUNT(DISTINCT f.asset_id) AS asset_count
                 FROM clusters c LEFT JOIN faces f ON f.cluster_id = c.id
                   AND f.state IN ('auto','confirmed')
                 WHERE (?1 IS NULL OR c.name IS NOT NULL)
                   AND (?1 IS NULL OR c.id IN (
                     SELECT entity_id FROM search_fts
                     WHERE entity_type = 'cluster' AND text LIKE '%' || ?1 || '%' ESCAPE '\\'
                   ))
                 GROUP BY c.id HAVING asset_count >= ?2
                 ORDER BY c.name IS NULL, c.name, c.id LIMIT 1000",
            )?;
            let escaped = query.map(escape_like);
            let rows = statement.query_map(params![escaped, minimum], cluster_summary_from_row)?;
            for row in rows {
                output.push(row?);
            }
            Ok(output)
        })
    }

    fn refresh_cluster_cover(&self, cluster_id: &str) -> Result<()> {
        let model_version = self.database.with_connection(|connection| {
            Ok(connection
                .query_row(
                    "SELECT model_version FROM faces WHERE cluster_id = ?1
                     ORDER BY state = 'confirmed' DESC, id LIMIT 1",
                    [cluster_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })?;
        let cover = if let Some(model_version) = model_version {
            self.cluster_medoids(&model_version)?
                .into_iter()
                .find(|medoid| medoid.cluster_id == cluster_id)
                .map(|medoid| medoid.face_id)
        } else {
            None
        };
        self.database.with_connection(|connection| {
            connection.execute(
                "UPDATE clusters SET cover_face_id = ?2 WHERE id = ?1",
                params![cluster_id, cover],
            )?;
            Ok(())
        })
    }
}

pub fn enqueue_analyze(database: &Database, asset_id: &str) -> Result<Job> {
    let payload = serde_json::to_string(&MlAnalyzePayload {
        asset_id: asset_id.to_owned(),
    })?;
    JobQueue::new(database.clone()).enqueue(ML_ANALYZE_JOB_KIND, &payload, ML_ANALYZE_PRIORITY)
}

pub fn enqueue_analyze_all(database: &Database) -> Result<Vec<Job>> {
    let model_version = Settings::new(database.clone())
        .ml_socket_path()?
        .and_then(|path| MlClient::new(path).health().ok())
        .and_then(|health| health.model_bundle.map(|bundle| bundle.version));
    enqueue_analyze_all_for_model(database, model_version.as_deref())
}

pub fn enqueue_analyze_all_for_model(
    database: &Database,
    model_version: Option<&str>,
) -> Result<Vec<Job>> {
    let ids = database.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT a.id FROM assets a
             WHERE a.lifecycle IN ('active','duplicate')
               AND NOT EXISTS (
                 SELECT 1 FROM faces f WHERE f.asset_id = a.id
                   AND (?2 IS NULL OR f.model_version = ?2)
               )
               AND NOT EXISTS (
                 SELECT 1 FROM jobs j WHERE j.kind = ?1
                   AND j.state IN ('queued','running')
                   AND json_extract(j.payload, '$.asset_id') = a.id
               )
             ORDER BY a.id",
        )?;
        Ok(statement
            .query_map(params![ML_ANALYZE_JOB_KIND, model_version], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    })?;
    ids.into_iter()
        .map(|asset_id| enqueue_analyze(database, &asset_id))
        .collect()
}

pub fn enqueue_recluster(database: &Database) -> Result<Job> {
    JobQueue::new(database.clone()).enqueue(
        ML_RECLUSTER_JOB_KIND,
        &serde_json::to_string(&MlReclusterPayload::default())?,
        ML_RECLUSTER_PRIORITY,
    )
}

pub fn search_named_clusters(database: &Database, query: &str) -> Result<Vec<ClusterSummary>> {
    MlService::new(database.clone(), MlClient::new("/dev/null")).search_named_clusters(query)
}

#[derive(Clone, Debug)]
struct EmbeddingRow {
    id: String,
    embedding: Vec<f32>,
    model_version: String,
    cluster_id: Option<String>,
    state: String,
}

type RawEmbeddingRow = (String, Vec<u8>, String, Option<String>, String);

fn embedding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEmbeddingRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn decode_embedding_row(
    (id, bytes, model_version, cluster_id, state): RawEmbeddingRow,
) -> Result<EmbeddingRow> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(Error::InvalidMl("invalid stored embedding".into()));
    }
    let embedding = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    if embedding.iter().any(|value| !value.is_finite()) {
        return Err(Error::InvalidMl("invalid stored embedding".into()));
    }
    Ok(EmbeddingRow {
        id,
        embedding,
        model_version,
        cluster_id,
        state,
    })
}

fn common_dimension(rows: &[EmbeddingRow]) -> Result<usize> {
    let dimension = rows
        .first()
        .map(|row| row.embedding.len())
        .ok_or_else(|| Error::InvalidMl("missing embeddings".into()))?;
    if dimension == 0 || rows.iter().any(|row| row.embedding.len() != dimension) {
        Err(Error::InvalidMl("mixed embedding dimensions".into()))
    } else {
        Ok(dimension)
    }
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        f32::NEG_INFINITY
    } else {
        dot / (left_norm * right_norm)
    }
}

fn face_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FaceRecord> {
    let bbox: String = row.get(3)?;
    let flags: String = row.get(5)?;
    Ok(FaceRecord {
        id: row.get(0)?,
        asset_id: row.get(1)?,
        kind: row.get(2)?,
        bbox: serde_json::from_str(&bbox).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        det_conf: row.get(4)?,
        quality_flags: serde_json::from_str(&flags).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        model_version: row.get(6)?,
        cluster_id: row.get(7)?,
        state: row.get(8)?,
        similarity: row.get(9)?,
    })
}

fn cluster_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClusterSummary> {
    let count: i64 = row.get(3)?;
    Ok(ClusterSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        cover_face_id: row.get(2)?,
        asset_count: u64::try_from(count).unwrap_or(0),
    })
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
