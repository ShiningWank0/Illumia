CREATE TABLE assets (
  id                TEXT PRIMARY KEY,
  hash              BLOB NOT NULL,
  original_name     TEXT NOT NULL,
  ext               TEXT NOT NULL,
  size              INTEGER NOT NULL,
  width             INTEGER NOT NULL,
  height            INTEGER NOT NULL,
  aspect_ratio      REAL GENERATED ALWAYS AS (CAST(width AS REAL)/height) STORED,
  taken_at          TEXT NOT NULL,
  taken_at_local_date TEXT NOT NULL,
  uploaded_at       TEXT NOT NULL,
  thumbhash         BLOB,
  in_timeline       INTEGER NOT NULL DEFAULT 1,
  visible_in_timeline INTEGER NOT NULL DEFAULT 1,
  lifecycle         TEXT NOT NULL DEFAULT 'active'
                    CHECK (lifecycle IN ('active','duplicate','trashed','purging')),
  duplicate_of      TEXT REFERENCES assets(id),
  trashed_at        TEXT,
  purge_after       TEXT,
  library_path      TEXT NOT NULL
);

CREATE UNIQUE INDEX ux_assets_hash_primary ON assets(hash)
  WHERE lifecycle = 'active' AND duplicate_of IS NULL;
CREATE INDEX ix_assets_timeline ON assets(taken_at_local_date, taken_at)
  WHERE lifecycle = 'active';
CREATE INDEX ix_assets_purge ON assets(purge_after) WHERE purge_after IS NOT NULL;
CREATE INDEX ix_assets_visible ON assets(taken_at_local_date, taken_at)
  WHERE visible_in_timeline = 1;

CREATE TABLE manga_stacks (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL,
  cover_asset_id TEXT REFERENCES assets(id),
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE stack_chapters (
  id          TEXT PRIMARY KEY,
  stack_id    TEXT NOT NULL REFERENCES manga_stacks(id) ON DELETE CASCADE,
  chapter_no  INTEGER NOT NULL,
  title       TEXT,
  UNIQUE (stack_id, chapter_no)
);

CREATE TABLE stack_pages (
  stack_id    TEXT NOT NULL REFERENCES manga_stacks(id) ON DELETE CASCADE,
  chapter_id  TEXT NOT NULL REFERENCES stack_chapters(id) ON DELETE CASCADE,
  asset_id    TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  page_no     INTEGER NOT NULL,
  show_in_timeline INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (stack_id, asset_id),
  UNIQUE (chapter_id, page_no)
);

CREATE TABLE clusters (
  id             TEXT PRIMARY KEY,
  name           TEXT,
  cover_face_id  TEXT,
  created_by     TEXT NOT NULL CHECK (created_by IN ('auto','user')),
  created_at     TEXT NOT NULL
);

CREATE TABLE faces (
  id            TEXT PRIMARY KEY,
  asset_id      TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('person','head','face')),
  bbox          TEXT NOT NULL,
  det_conf      REAL NOT NULL,
  quality_flags TEXT NOT NULL DEFAULT '[]',
  embedding     BLOB,
  model_version TEXT NOT NULL,
  cluster_id    TEXT REFERENCES clusters(id) ON DELETE SET NULL,
  state         TEXT NOT NULL DEFAULT 'unassigned'
                CHECK (state IN ('auto','confirmed','candidate','rejected','unassigned')),
  similarity    REAL
);
CREATE INDEX ix_faces_cluster ON faces(cluster_id, state);

CREATE TABLE cluster_rejections (
  face_id     TEXT NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
  cluster_id  TEXT NOT NULL REFERENCES clusters(id) ON DELETE CASCADE,
  PRIMARY KEY (face_id, cluster_id)
);

CREATE TABLE jobs (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,
  payload     TEXT NOT NULL,
  state       TEXT NOT NULL DEFAULT 'queued'
              CHECK (state IN ('queued','running','done','failed','cancelled')),
  priority    INTEGER NOT NULL DEFAULT 0,
  progress    REAL NOT NULL DEFAULT 0,
  error       TEXT,
  created_at  TEXT NOT NULL,
  started_at  TEXT,
  finished_at TEXT
);
CREATE INDEX ix_jobs_queue ON jobs(state, priority DESC, created_at);

CREATE TABLE settings ( key TEXT PRIMARY KEY, value TEXT NOT NULL );

CREATE TABLE auth_tokens (
  id          TEXT PRIMARY KEY,
  device_name TEXT NOT NULL,
  token_hash  BLOB NOT NULL,
  created_at  TEXT NOT NULL,
  last_used   TEXT
);

CREATE VIRTUAL TABLE search_fts USING fts5(
  entity_type,
  entity_id UNINDEXED,
  text,
  tokenize = 'trigram'
);

CREATE TRIGGER assets_search_insert AFTER INSERT ON assets BEGIN
  INSERT INTO search_fts(entity_type, entity_id, text)
  VALUES ('asset', new.id, new.original_name);
END;
CREATE TRIGGER assets_search_update AFTER UPDATE OF original_name ON assets BEGIN
  DELETE FROM search_fts WHERE entity_type = 'asset' AND entity_id = old.id;
  INSERT INTO search_fts(entity_type, entity_id, text)
  VALUES ('asset', new.id, new.original_name);
END;
CREATE TRIGGER assets_search_delete AFTER DELETE ON assets BEGIN
  DELETE FROM search_fts WHERE entity_type = 'asset' AND entity_id = old.id;
END;

CREATE TRIGGER stacks_search_insert AFTER INSERT ON manga_stacks BEGIN
  INSERT INTO search_fts(entity_type, entity_id, text)
  VALUES ('stack', new.id, new.title);
END;
CREATE TRIGGER stacks_search_update AFTER UPDATE OF title ON manga_stacks BEGIN
  DELETE FROM search_fts WHERE entity_type = 'stack' AND entity_id = old.id;
  INSERT INTO search_fts(entity_type, entity_id, text)
  VALUES ('stack', new.id, new.title);
END;
CREATE TRIGGER stacks_search_delete AFTER DELETE ON manga_stacks BEGIN
  DELETE FROM search_fts WHERE entity_type = 'stack' AND entity_id = old.id;
END;

CREATE TRIGGER clusters_search_insert AFTER INSERT ON clusters
WHEN new.name IS NOT NULL BEGIN
  INSERT INTO search_fts(entity_type, entity_id, text)
  VALUES ('cluster', new.id, new.name);
END;
CREATE TRIGGER clusters_search_update AFTER UPDATE OF name ON clusters BEGIN
  DELETE FROM search_fts WHERE entity_type = 'cluster' AND entity_id = old.id;
  INSERT INTO search_fts(entity_type, entity_id, text)
  SELECT 'cluster', new.id, new.name WHERE new.name IS NOT NULL;
END;
CREATE TRIGGER clusters_search_delete AFTER DELETE ON clusters BEGIN
  DELETE FROM search_fts WHERE entity_type = 'cluster' AND entity_id = old.id;
END;
