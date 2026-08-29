ALTER TABLE jobs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0
  CHECK (cancel_requested IN (0, 1));

CREATE UNIQUE INDEX ux_jobs_active_ml_vault_analyze_asset
ON jobs(json_extract(payload, '$.asset_id'))
WHERE kind = 'ml_vault_analyze' AND state IN ('queued', 'running');
