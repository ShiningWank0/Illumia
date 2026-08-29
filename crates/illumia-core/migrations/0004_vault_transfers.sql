CREATE TABLE vault_transfers (
  id          TEXT PRIMARY KEY,
  direction   TEXT NOT NULL
              CHECK (direction IN ('import','export','direct_ingest')),
  role        TEXT NOT NULL
              CHECK (role IN ('source','destination')),
  asset_ids   TEXT NOT NULL CHECK (json_valid(asset_ids)),
  state       TEXT NOT NULL
              CHECK (state IN (
                'preparing','destination_ready','source_files_deleted',
                'source_database_deleted'
              )),
  created_at  TEXT NOT NULL
);

CREATE INDEX ix_vault_transfers_direction_role
  ON vault_transfers(direction, role, created_at);
