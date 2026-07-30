CREATE TABLE vault_blobs (
  blob_id      TEXT PRIMARY KEY,
  wrapped_key  BLOB NOT NULL,
  kind         TEXT NOT NULL
               CHECK (kind IN ('standalone','original','thumbnail','preview')),
  asset_id     TEXT REFERENCES assets(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX ux_vault_blobs_asset_kind
  ON vault_blobs(asset_id, kind)
  WHERE asset_id IS NOT NULL;

-- FTS5 segment updates must overwrite deleted terms instead of leaving historical
-- entries in old segment blobs. PRAGMA secure_delete alone cannot guarantee this.
INSERT INTO search_fts(search_fts, rank) VALUES('secure-delete', 1);
