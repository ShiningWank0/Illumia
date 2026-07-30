// docs/03_api.md の型定義。M1 (illumia-server) が公開する範囲を UI 向けに型付けする。

/** タイムラインのズーム粒度。docs/04 の 3 段階。 */
export type Granularity = 'day' | 'month' | 'year';

/** `GET /api/server/info` (未認証可)。 */
export interface ServerInfo {
  version: string;
  setup_completed: boolean;
  authenticated: boolean;
  setup_token_required: boolean;
}

/** setup / login リクエスト body。 */
export interface AuthRequest {
  password: string;
  device_name: string;
}

/** setup / login レスポンス。 */
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
  [key: string]: number | string | null;
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

/** 横断検索結果 (`GET /api/search`)。 */
export interface SearchResult {
  assets: Asset[];
  stacks: StackSummary[];
  clusters: unknown[];
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
  setup(req: AuthRequest, setupToken?: string): Promise<TokenResponse>;
  login(req: AuthRequest): Promise<TokenResponse>;
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

  // --- 検索 ---
  search(q: string): Promise<SearchResult>;
}
