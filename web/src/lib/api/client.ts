// 実サーバー向け fetch クライアント。docs/03 の REST を叩く。
// Bearer トークンを付与し、エラー封筒を ApiError に変換する。

import { getToken } from './token';
import {
  ApiError,
  type ApiErrorBody,
  type Bucket,
  type BucketItem,
  type Granularity,
  type TimelineApi
} from './types';

/** クライアント設定。baseUrl は末尾スラッシュ無しを想定。 */
export interface ClientConfig {
  baseUrl: string;
}

/** 既定の baseUrl。VITE_API_BASE_URL があれば使用、無ければ同一オリジン。 */
export function defaultBaseUrl(): string {
  const fromEnv = import.meta.env?.VITE_API_BASE_URL as string | undefined;
  if (fromEnv && fromEnv.length > 0) return fromEnv.replace(/\/$/, '');
  return '';
}

async function request<T>(config: ClientConfig, path: string): Promise<T> {
  const token = getToken();
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;

  let res: Response;
  try {
    res = await fetch(`${config.baseUrl}${path}`, { headers });
  } catch (e) {
    // ネットワーク断など。
    throw new ApiError(0, 'network_error', e instanceof Error ? e.message : 'network error');
  }

  if (!res.ok) {
    // エラー封筒 { error: { code, message } } を解釈。取れなければ status で代替。
    let code = `http_${res.status}`;
    let message = res.statusText;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body?.error) {
        code = body.error.code ?? code;
        message = body.error.message ?? message;
      }
    } catch {
      // JSON でない / 空ボディはそのまま。
    }
    throw new ApiError(res.status, code, message);
  }

  return (await res.json()) as T;
}

/** 実サーバー実装の TimelineApi を生成する。 */
export function createHttpClient(config: ClientConfig): TimelineApi {
  const base = config.baseUrl.replace(/\/$/, '');
  return {
    getBuckets(granularity: Granularity): Promise<Bucket[]> {
      return request<Bucket[]>(
        { baseUrl: base },
        `/api/timeline/buckets?granularity=${granularity}`
      );
    },
    getBucketItems(granularity: Granularity, key: string): Promise<BucketItem[]> {
      return request<BucketItem[]>(
        { baseUrl: base },
        `/api/timeline/buckets/${encodeURIComponent(key)}?granularity=${granularity}`
      );
    },
    thumbnailUrl(id: string): string {
      return `${base}/api/assets/${encodeURIComponent(id)}/thumbnail`;
    },
    previewUrl(id: string): string {
      return `${base}/api/assets/${encodeURIComponent(id)}/preview`;
    }
  };
}
