// docs/03_api.md の型定義。M1 (illumia-server) が公開する範囲を UI 向けに型付けする。

/** タイムラインのズーム粒度。docs/04 の 3 段階。 */
export type Granularity = 'day' | 'month' | 'year';

/** `GET /api/server/info` (未認証可)。 */
export interface ServerInfo {
  version: string | null;
  setup_completed: boolean;
  authenticated: boolean;
  setup_token_required: boolean;
}

/** setup / login リクエスト body。 */
export interface AuthRequest {
  password: string;
  device_name: string;
}

/** ネイティブクライアント向け setup / login レスポンス。Web は受け取らない。 */
export interface TokenResponse {
  token: string;
}

/**
 * bucket 集計の 1 エントリ (`GET /api/timeline/buckets`)。
 * key: day=`YYYY-MM-DD`, month=`YYYY-MM`, year=`YYYY` (taken_at_local_date 基準)。
 */
export interface Bucket {
  key: string;
  count: number;
}

/**
 * bucket 内アイテム (`GET /api/timeline/buckets/{key}`)。
 * ratio = width / height。thumbhash は base64 プレースホルダ。
 * レスポンスは taken_at DESC 順。
 */
export interface BucketItem {
  id: string;
  ratio: number;
  thumbhash: string | null;
  taken_at: string; // ISO 8601
}

/** アセット詳細 / trash / duplicate で共有する形 (`AssetResponse`)。 */
export interface Asset {
  id: string;
  filename: string;
  width: number;
  height: number;
  ratio: number;
  thumbhash: string | null;
  taken_at: string;
  created_at: string;
  status: 'created' | 'duplicate' | 'trashed' | 'purging';
  duplicate_of?: string;
  trashed_at?: string;
  purge_after?: string;
}

/** アップロード結果 (`POST /api/assets`)。 */
export interface UploadResult {
  id: string;
  status: 'created' | 'duplicate';
  duplicate_of?: string;
}

/** 重複ペア (`GET /api/duplicates`)。 */
export interface DuplicatePair {
  dup: Asset;
  original: Asset;
  purge_after?: string;
}

/** 設定 (`GET/PATCH /api/settings`)。UI が扱うキーのみ抜粋。 */
export interface AppSettings {
  'trash.retention_days': number;
  'dedup.retention_days': number;
  'jobs.thumbnail_concurrency': number;
  'jobs.ml_concurrency': number;
  // ML (docs/02 / docs/07)
  'ml.enabled': boolean;
  'ml.tau_high_override': number | null;
  'ml.tau_low_override': number | null;
  'ml.min_cluster_size': number;
  'ml.quality_gate': string | null; // 'review_only' | 'strict'
  [key: string]: number | string | boolean | null;
}

// ---- 漫画スタック (docs/05) ----

/** スタック一覧の 1 件 (`GET /api/stacks`)。 */
export interface StackSummary {
  id: string;
  title: string;
  cover_asset_id: string | null;
  chapter_count: number;
  page_count: number;
  created_at: string;
  updated_at: string;
}

/** スタック内 1 ページ。 */
export interface StackPage {
  page_no: number;
  show_in_timeline: boolean;
  asset: Asset;
}

/** スタックの章 (話)。 */
export interface StackChapter {
  id: string;
  chapter_no: number;
  title: string | null;
  pages: StackPage[];
}

/** スタック詳細 (`GET /api/stacks/{id}`)。 */
export interface StackDetail {
  id: string;
  title: string;
  cover_asset_id: string | null;
  created_at: string;
  updated_at: string;
  chapters: StackChapter[];
}

/** structure 一括置換の章入力 (`PUT /api/stacks/{id}/structure`)。 */
export interface ChapterInput {
  title: string | null;
  pages: string[]; // asset_id の順序付き列
}

// ---- キャラクター (クラスタ) (docs/03 §キャラクター / docs/02 / docs/07) ----

/** 正規化 bbox [x, y, w, h] (0..1)。 */
export type Bbox = [number, number, number, number];

export type FaceState = 'auto' | 'confirmed' | 'candidate' | 'rejected' | 'unassigned';

/** 顔 (faces テーブル相当。crop 表示に必要な最小情報)。 */
export interface Face {
  id: string;
  asset_id: string;
  bbox: Bbox;
  state?: FaceState;
  similarity?: number | null;
}

/** クラスタ一覧の 1 件 (`GET /api/clusters`)。cover = 代表顔。 */
export interface Cluster {
  id: string;
  name: string | null; // null = 未命名
  count: number; // クラスタに属するアセット数
  cover: Face | null; // 代表顔
}

/**
 * クラスタ内の顔タイル 1 件。API の 1 asset + faces[] を mapper が
 * 1 face = 1 件へ flatten するため、同じ asset が複数回現れることがある。
 */
export interface ClusterAsset {
  asset: Asset;
  face: Face;
}

/** 確認キューの候補顔 (`GET /api/review/candidates`)。 */
export interface Candidate {
  face_id: string;
  asset_id: string;
  bbox: Bbox;
  cluster_id: string | null;
  cluster_name: string | null;
  similarity: number | null;
}

/** ML サイドカー状態 (`GET /api/ml/status`)。 */
export interface MlStatus {
  enabled: boolean;
  backend: 'onnx' | 'mock';
  bundle_version: string | null;
  model_ready: boolean;
}

/** ジョブ (`GET /api/jobs`)。進捗表示に使う。 */
export interface Job {
  id: string;
  kind: string;
  state: 'queued' | 'running' | 'done' | 'failed' | 'cancelled';
  progress: number;
  error: string | null;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
}

/** 横断検索結果 (`GET /api/search`)。 */
export interface SearchResult {
  assets: Asset[];
  stacks: StackSummary[];
  clusters: Cluster[];
}

// ---- Vault (docs/06) ----

/** `GET /api/vault/status`。 */
export interface VaultStatusResponse {
  initialized: boolean;
  unlocked: boolean;
}

/** `POST /api/vault/unlock` 成功。 */
export interface VaultUnlockResponse {
  vault_session: string;
  expires_at: string;
}

/** import / export のペイロード (どちらか一方)。 */
export interface VaultTransfer {
  asset_ids?: string[];
  stack_id?: string;
}

/** WS メッセージ (docs/03)。タイムラインは assets_added を購読する。 */
export type WsMessage =
  | { type: 'job'; id: string; state: string; progress: number }
  | { type: 'assets_added'; bucket_keys: string[] };

/** サーバーのエラー封筒 (`{ error: { code, message } }`)。 */
export interface ApiErrorBody {
  error: { code: string; message: string };
}

/** fetch クライアントが throw するエラー型。 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

/**
 * UI が依存する API 抽象。実サーバー実装 (client.ts) と
 * モック実装 (mock.ts) の両方がこれを満たす。
 */
export interface IllumiaApi {
  // --- メタ / 認証 ---
  serverInfo(): Promise<ServerInfo>;
  setup(req: AuthRequest, setupToken?: string): Promise<void>;
  login(req: AuthRequest): Promise<void>;
  logout(): Promise<void>;

  // --- タイムライン ---
  getBuckets(granularity: Granularity): Promise<Bucket[]>;
  getBucketItems(granularity: Granularity, key: string): Promise<BucketItem[]>;
  /** 240px サムネイル URL (要認証。取得は image.ts 経由)。 */
  thumbnailUrl(id: string): string;
  /** 1440px プレビュー URL (全画面ビューア用)。 */
  previewUrl(id: string): string;
  /** 原本ダウンロード URL。 */
  originalUrl(id: string): string;

  // --- アセット操作 ---
  uploadAsset(file: File): Promise<UploadResult>;
  /** ハッシュ照合。`{hex: asset_id}` を返す (自動アップロードの事前スキップ判定)。 */
  assetsExist(hashes: string[]): Promise<Record<string, string>>;
  trashAsset(id: string): Promise<void>;
  restoreAsset(id: string): Promise<void>;

  // --- ゴミ箱 / 重複 ---
  getTrash(): Promise<Asset[]>;
  getDuplicates(): Promise<DuplicatePair[]>;
  purgeNow(id: string): Promise<void>;

  // --- 設定 ---
  getSettings(): Promise<AppSettings>;
  patchSettings(patch: Partial<AppSettings>): Promise<AppSettings>;

  // --- 漫画スタック (docs/05) ---
  listStacks(): Promise<StackSummary[]>;
  createStack(title: string, assetIds: string[]): Promise<StackDetail>;
  getStack(id: string): Promise<StackDetail>;
  patchStack(id: string, patch: { title?: string; cover_asset_id?: string }): Promise<StackDetail>;
  deleteStack(id: string): Promise<void>;
  replaceStructure(id: string, chapters: ChapterInput[]): Promise<StackDetail>;
  addStackPages(id: string, assetIds: string[], chapterId?: string): Promise<StackDetail>;
  removeStackPage(id: string, assetId: string): Promise<void>;
  setPageFlag(id: string, assetId: string, showInTimeline: boolean): Promise<StackDetail>;

  // --- キャラクター (クラスタ) (docs/03 §キャラクター) ---
  listClusters(): Promise<Cluster[]>;
  getClusterAssets(id: string): Promise<ClusterAsset[]>;
  renameCluster(id: string, name: string): Promise<Cluster>;
  mergeClusters(fromId: string, intoId: string): Promise<void>;
  splitCluster(id: string, faceIds: string[]): Promise<Cluster>;
  getReviewCandidates(): Promise<Candidate[]>;
  reviewCandidate(faceId: string, action: 'accept' | 'reject'): Promise<void>;

  // --- ML 制御 (docs/07。docs/03 のジョブ・設定を拡張) ---
  mlStatus(): Promise<MlStatus>;
  analyzeAll(): Promise<void>;
  recluster(): Promise<void>;
  getJobs(state?: string): Promise<Job[]>;

  // --- 検索 ---
  search(q: string): Promise<SearchResult>;
}
