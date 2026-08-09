-- Preserve the oldest active copy before enforcing idempotent ML admission.
UPDATE jobs
SET state = 'cancelled',
    error = 'superseded by active-job deduplication',
    finished_at = COALESCE(finished_at, created_at)
WHERE kind = 'ml_analyze'
  AND state IN ('queued', 'running')
  AND id NOT IN (
    SELECT MIN(id)
    FROM jobs
    WHERE kind = 'ml_analyze' AND state IN ('queued', 'running')
    GROUP BY json_extract(payload, '$.asset_id')
  );

UPDATE jobs
SET state = 'cancelled',
    error = 'superseded by active-job deduplication',
    finished_at = COALESCE(finished_at, created_at)
WHERE kind = 'ml_recluster'
  AND state IN ('queued', 'running')
  AND id != (
    SELECT MIN(id)
    FROM jobs
    WHERE kind = 'ml_recluster' AND state IN ('queued', 'running')
  );

CREATE UNIQUE INDEX ux_jobs_active_ml_analyze_asset
ON jobs(json_extract(payload, '$.asset_id'))
WHERE kind = 'ml_analyze' AND state IN ('queued', 'running');

CREATE UNIQUE INDEX ux_jobs_active_ml_recluster
ON jobs(kind)
WHERE kind = 'ml_recluster' AND state IN ('queued', 'running');
