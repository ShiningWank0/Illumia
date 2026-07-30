// docs/03_api.md の型定義。REST + WS のうち、タイムライン UI で使う範囲を定義する。

/** タイムラインのズーム粒度。docs/04 の 3 段階。 */
export type Granularity = 'day' | 'month' | 'year';

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
  thumbhash: string;
  taken_at: string; // ISO 8601
}

/** アセット詳細 (`GET /api/assets/{id}`)。将来のビューア/詳細で使用。 */
export interface Asset {
  id: string;
  filename: string;
  width: number;
  height: number;
  ratio: number;
  thumbhash: string;
  taken_at: string;
  created_at: string;
  status: 'created' | 'duplicate' | 'trashed';
  duplicate_of?: string;
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
 * タイムライン UI が依存する API 抽象。実サーバー実装 (client.ts) と
 * モック実装 (mock.ts) の両方がこれを満たす。
 */
export interface TimelineApi {
  /** 全バケットの件数集計を取得。 */
  getBuckets(granularity: Granularity): Promise<Bucket[]>;
  /** 指定バケットの実データ (taken_at DESC) を取得。 */
  getBucketItems(granularity: Granularity, key: string): Promise<BucketItem[]>;
  /** 240px サムネイル URL。 */
  thumbnailUrl(id: string): string;
  /** 1440px プレビュー URL (全画面ビューア用)。 */
  previewUrl(id: string): string;
}
