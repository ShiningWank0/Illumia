# 02. データモデル

メイン DB は `<data_root>/illumia.db` (SQLite, WAL モード)。Vault は同型スキーマの
別ファイル `vault/vault.db` (SQLCipher) を持つ (→ docs/06)。ORM は使わず、
`crates/illumia-core` に versioned マイグレーション (`migrations/000N_*.sql`) を置く。

## 方針

- ID は **UUIDv7 の TEXT** (時系列ソート可能・クライアント側生成可)。
- 時刻は UTC の RFC3339 TEXT (`taken_at` はローカル日付バケットに影響するため
  `taken_at_local_date` を生成列で持つ)。
- ML 結果 (faces/clusters) も本 DB が正。サイドカーは状態を持たない。
- 削除まわりのライフサイクルと不変条件の詳細は **docs/11_dedup_and_trash.md が正**。

## スキーマ

```sql
CREATE TABLE assets (
  id                TEXT PRIMARY KEY,              -- UUIDv7
  hash              BLOB NOT NULL,                 -- BLAKE3 32B
  original_name     TEXT NOT NULL,
  ext               TEXT NOT NULL,                 -- 小文字拡張子 (jpg/png/webp/avif/gif)
  size              INTEGER NOT NULL,
  width             INTEGER NOT NULL,
  height            INTEGER NOT NULL,
  aspect_ratio      REAL GENERATED ALWAYS AS (CAST(width AS REAL)/height) STORED,
  taken_at          TEXT NOT NULL,                 -- EXIF/ファイル日時/アップロード時刻の優先順
  taken_at_local_date TEXT NOT NULL,               -- 'YYYY-MM-DD' (バケットキー用)
  uploaded_at       TEXT NOT NULL,
  thumbhash         BLOB,                          -- プレースホルダ (≤ 25B 程度)
  in_timeline       INTEGER NOT NULL DEFAULT 1,    -- ユーザーによる個別非表示用の予備
  lifecycle         TEXT NOT NULL DEFAULT 'active'
                    CHECK (lifecycle IN ('active','duplicate','trashed','purging')),
  duplicate_of      TEXT REFERENCES assets(id),    -- 重複元 (本体)。昇格後も保持
  trashed_at        TEXT,                          -- lifecycle='trashed' の間のみ非 NULL
  purge_after       TEXT,                          -- duplicate/trashed の自動削除期限
  library_path      TEXT NOT NULL                  -- data_root からの相対パス
);

-- 「本体」の一意性: active かつ重複由来でない asset は hash ごとに 1 つ。
-- 重複から昇格した asset は duplicate_of が非 NULL のままなので衝突しない (→ docs/11)。
CREATE UNIQUE INDEX ux_assets_hash_primary ON assets(hash)
  WHERE lifecycle = 'active' AND duplicate_of IS NULL;

CREATE INDEX ix_assets_timeline ON assets(taken_at_local_date, taken_at)
  WHERE lifecycle = 'active';
CREATE INDEX ix_assets_purge ON assets(purge_after) WHERE purge_after IS NOT NULL;

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
  chapter_no  INTEGER NOT NULL,                    -- 1 始まり、スタック内で一意
  title       TEXT,                                -- 「第1話」等。NULL 可
  UNIQUE (stack_id, chapter_no)
);

CREATE TABLE stack_pages (
  stack_id    TEXT NOT NULL REFERENCES manga_stacks(id) ON DELETE CASCADE,
  chapter_id  TEXT NOT NULL REFERENCES stack_chapters(id) ON DELETE CASCADE,
  asset_id    TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  page_no     INTEGER NOT NULL,                    -- 章内の順序 (1 始まり)
  show_in_timeline INTEGER NOT NULL DEFAULT 0,     -- スタック追加時に非表示 (→ docs/05)
  PRIMARY KEY (stack_id, asset_id),
  UNIQUE (chapter_id, page_no)
);

-- ML (→ docs/07)
CREATE TABLE clusters (
  id             TEXT PRIMARY KEY,
  name           TEXT,                             -- ユーザー命名。NULL = 未命名
  cover_face_id  TEXT,
  created_by     TEXT NOT NULL CHECK (created_by IN ('auto','user')),
  created_at     TEXT NOT NULL
);

CREATE TABLE faces (
  id            TEXT PRIMARY KEY,
  asset_id      TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('person','head','face')),
  bbox          TEXT NOT NULL,                     -- JSON [x,y,w,h] 正規化座標
  det_conf      REAL NOT NULL,
  quality_flags TEXT NOT NULL DEFAULT '[]',        -- JSON。品質ゲート結果
  embedding     BLOB,                              -- f32 LE。model_version とペア
  model_version TEXT NOT NULL,
  cluster_id    TEXT REFERENCES clusters(id) ON DELETE SET NULL,
  state         TEXT NOT NULL DEFAULT 'unassigned'
                CHECK (state IN ('auto','confirmed','candidate','rejected','unassigned')),
  similarity    REAL                               -- 割り当て時の類似度 (監査用)
);
CREATE INDEX ix_faces_cluster ON faces(cluster_id, state);

CREATE TABLE cluster_rejections (                  -- 「同一ではない」というユーザー判定
  face_id     TEXT NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
  cluster_id  TEXT NOT NULL REFERENCES clusters(id) ON DELETE CASCADE,
  PRIMARY KEY (face_id, cluster_id)
);

CREATE TABLE jobs (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,       -- thumbnail / ml_analyze / ml_cluster / purge / ...
  payload     TEXT NOT NULL,       -- JSON
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
-- 主なキー: trash.retention_days (既定 30) / dedup.retention_days (既定 30)
--          jobs.thumbnail_concurrency / jobs.ml_concurrency
--          ml.tau_high_override / ml.tau_low_override / ml.min_cluster_size
--          ml.quality_gate ('review_only' | 'strict')

CREATE TABLE auth_tokens (
  id          TEXT PRIMARY KEY,
  device_name TEXT NOT NULL,
  token_hash  BLOB NOT NULL,       -- SHA-256(token)。平文は保存しない
  created_at  TEXT NOT NULL,
  last_used   TEXT
);

-- 検索 (日本語対応必須): trigram tokenizer
CREATE VIRTUAL TABLE search_fts USING fts5(
  entity_type,                     -- 'asset' | 'stack' | 'cluster'
  entity_id UNINDEXED,
  text,                            -- original_name / stack title / cluster name
  tokenize = 'trigram'
);
```

## タイムライン可視条件 (非正規化)

タイムラインに出る asset の条件:

```
lifecycle = 'active'
AND in_timeline = 1
AND (どのスタックにも属さない OR 属す全 stack_pages の show_in_timeline が 1)
```

bucket 集計を単純なインデックススキャンにするため、`assets.visible_in_timeline INTEGER`
を非正規化して持ち、`stack_pages` の INSERT/UPDATE/DELETE と `assets.lifecycle` 遷移の
トリガ (またはサービス層の同一トランザクション内更新) で維持する。
**維持ロジックはサービス層に一元化**し、直接 SQL で assets を書き換えるコードを書かないこと。

```sql
CREATE INDEX ix_assets_visible ON assets(taken_at_local_date, taken_at)
  WHERE visible_in_timeline = 1;
```

## ライフサイクル遷移 (概要 — 詳細と不変条件は docs/11)

```
                    upload (hash 未登録)
                          │
                          ▼
     ┌──────────────── active ◄──────────────┐
     │ user delete        │  ▲               │ restore
     ▼                    │  │ stack 追加で昇格 │
  trashed ──期限──► purge │  │               │
                          ▼  │               │
   upload (hash 一致) → duplicate ──期限──► purge
```

- `purging` はパージ処理中の tombstone 状態 (クラッシュ耐性用 → docs/11 §手順)。
- Vault への出し入れは「別 DB への移動 + 元 DB からの完全削除」であり、
  ライフサイクル遷移ではない (→ docs/06)。

## Vault DB との関係

- `vault.db` は assets / manga_stacks / stack_chapters / stack_pages / clusters /
  faces / search_fts / jobs(vault 専用) を **同一定義**で持つ。auth_tokens / settings は持たない。
- マイグレーションは共通の SQL を両 DB に適用する実装にする (スキーマ乖離を防ぐ)。
- 平文側 DB / ログ / ジョブ payload に vault 内のファイル名・ID 等を記録しないこと。
