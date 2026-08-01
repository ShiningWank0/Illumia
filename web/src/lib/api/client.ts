// 実サーバー向け実装。docs/03 の REST を叩く。
// HttpOnly Cookie 認証を使い、エラー封筒を ApiError に変換する。
// 画像 (thumbnail/preview/original) は認証必須のため URL のみ返し、
// 実取得は image.ts の authedObjectUrl が Bearer 付き fetch で行う。

import { blake3 } from 'hash-wasm';

import { isTauri, nativeFetch } from '$lib/platform/tauri';
import { getNativeToken } from '$lib/platform/nativeAuth';
import { getActiveBaseUrl } from '$lib/platform/connection';
import {
  ApiError,
  type ApiErrorBody,
  type AppSettings,
  type Asset,
  type AuthRequest,
  type Bucket,
  type BucketItem,
  type ChapterInput,
  type DuplicatePair,
  type Granularity,
  type IllumiaApi,
  type SearchResult,
  type ServerInfo,
  type StackDetail,
  type StackSummary,
  type UploadResult
} from './types';

export interface ClientConfig {
  baseUrl?: string;
}

/** 既定の baseUrl。VITE_API_BASE_URL があれば使用、無ければ同一オリジン。 */
export function defaultBaseUrl(): string {
  const fromEnv = import.meta.env?.VITE_API_BASE_URL as string | undefined;
  if (fromEnv && fromEnv.length > 0) return fromEnv.replace(/\/$/, '');
  return '';
}

/**
 * 実効 baseUrl を解決する。
 *  - アプリモード (Tauri): 接続プロファイルのプローブで選んだ URL (未確定なら空)。
 *  - ブラウザ: 同一オリジン (defaultBaseUrl)。
 */
export function resolveBaseUrl(): string {
  if (isTauri()) return getActiveBaseUrl() ?? '';
  return defaultBaseUrl();
}

/**
 * ネイティブ (アプリモード) 用 setup。X-Illumia-Auth-Mode: cookie を付けないため
 * サーバーは device token を body で返す。呼び出し側が secure storage に保存する。
 */
export async function nativeSetup(req: AuthRequest, setupToken?: string): Promise<string> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (setupToken) headers['X-Illumia-Setup-Token'] = setupToken;
  const res = await request<{ token: string }>(resolveBaseUrl(), '/api/auth/setup', {
    method: 'POST',
    headers,
    body: JSON.stringify(req)
  });
  return res.token;
}

/** ネイティブ用 login。device token を body で受け取る。 */
export async function nativeLogin(req: AuthRequest): Promise<string> {
  const res = await request<{ token: string }>(resolveBaseUrl(), '/api/auth/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req)
  });
  return res.token;
}

export interface RequestOptions {
  method?: string;
  body?: BodyInit | null;
  headers?: Record<string, string>;
  /** JSON パースせず Response を返す (204 等)。 */
  raw?: boolean;
}

export async function request<T>(
  base: string,
  path: string,
  opts: RequestOptions = {}
): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json', ...opts.headers };
  const target = `${base}${path}`;
  const native = isTauri();

  if (native) {
    // ネイティブは Bearer 認証 (クロスオリジンのリモートサーバーへ接続)。
    const token = getNativeToken();
    if (token) headers.Authorization = `Bearer ${token}`;
  } else if (typeof location !== 'undefined') {
    // ブラウザは同一オリジン Cookie 認証を厳守する (docs/12)。
    const resolved = new URL(target, location.origin);
    if (resolved.origin !== location.origin) {
      throw new ApiError(
        0,
        'cross_origin_configuration',
        'the Web client requires a same-origin API'
      );
    }
  }

  let res: Response;
  try {
    const doFetch = native ? nativeFetch : fetch;
    res = await doFetch(target, {
      method: opts.method ?? 'GET',
      headers,
      body: opts.body ?? null,
      credentials: native ? 'omit' : 'same-origin'
    });
  } catch (e) {
    throw new ApiError(0, 'network_error', e instanceof Error ? e.message : 'network error');
  }

  if (!res.ok) {
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

  if (opts.raw) return res as unknown as T;
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

/** 実サーバー実装の IllumiaApi を生成する。 */
export function createHttpClient(config: ClientConfig = {}): IllumiaApi {
  // baseUrl は呼び出しごとに解決する (アプリモードでプローブ後に変わるため)。
  const base = (): string =>
    (config.baseUrl != null ? config.baseUrl : resolveBaseUrl()).replace(/\/$/, '');
  const enc = encodeURIComponent;

  return {
    serverInfo(): Promise<ServerInfo> {
      return request<ServerInfo>(base(), '/api/server/info');
    },
    setup(req: AuthRequest, setupToken?: string): Promise<void> {
      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
        'X-Illumia-Auth-Mode': 'cookie'
      };
      if (setupToken) headers['X-Illumia-Setup-Token'] = setupToken;
      return request<void>(base(), '/api/auth/setup', {
        method: 'POST',
        headers,
        body: JSON.stringify(req)
      });
    },
    login(req: AuthRequest): Promise<void> {
      return request<void>(base(), '/api/auth/login', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-Illumia-Auth-Mode': 'cookie'
        },
        body: JSON.stringify(req)
      });
    },
    async logout(): Promise<void> {
      await request<Response>(base(), '/api/auth/logout', { method: 'POST', raw: true });
    },

    getBuckets(granularity: Granularity): Promise<Bucket[]> {
      return request<Bucket[]>(base(), `/api/timeline/buckets?granularity=${granularity}`);
    },
    getBucketItems(granularity: Granularity, key: string): Promise<BucketItem[]> {
      return request<BucketItem[]>(
        base(),
        `/api/timeline/buckets/${enc(key)}?granularity=${granularity}`
      );
    },
    thumbnailUrl(id: string): string {
      return `${base()}/api/assets/${enc(id)}/thumbnail`;
    },
    previewUrl(id: string): string {
      return `${base()}/api/assets/${enc(id)}/preview`;
    },
    originalUrl(id: string): string {
      return `${base()}/api/assets/${enc(id)}/original`;
    },

    async uploadAsset(file: File): Promise<UploadResult> {
      // blake3 チェックサムを算出して X-Illumia-Checksum に付与。
      const buffer = new Uint8Array(await file.arrayBuffer());
      const checksum = await blake3(buffer); // 64 桁 hex (256bit)
      const takenAt = new Date(file.lastModified).toISOString(); // RFC3339
      const form = new FormData();
      form.append('file', file, file.name);
      // Content-Type はブラウザが boundary 付きで設定するため指定しない。
      return request<UploadResult>(base(), '/api/assets', {
        method: 'POST',
        headers: { 'X-Illumia-Checksum': checksum, 'X-Illumia-Taken-At': takenAt },
        body: form
      });
    },
    async assetsExist(hashes: string[]): Promise<Record<string, string>> {
      const res = await request<{ exists: Record<string, string> }>(base(), '/api/assets/exists', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ hashes })
      });
      return res.exists;
    },
    async trashAsset(id: string): Promise<void> {
      await request<Asset>(base(), `/api/assets/${enc(id)}`, { method: 'DELETE' });
    },
    async restoreAsset(id: string): Promise<void> {
      await request<Asset>(base(), `/api/assets/${enc(id)}/restore`, { method: 'POST' });
    },

    getTrash(): Promise<Asset[]> {
      return request<Asset[]>(base(), '/api/trash');
    },
    getDuplicates(): Promise<DuplicatePair[]> {
      return request<DuplicatePair[]>(base(), '/api/duplicates');
    },
    async purgeNow(id: string): Promise<void> {
      await request<Response>(base(), `/api/trash/${enc(id)}`, { method: 'DELETE', raw: true });
    },

    getSettings(): Promise<AppSettings> {
      return request<AppSettings>(base(), '/api/settings');
    },
    patchSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
      return request<AppSettings>(base(), '/api/settings', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(patch)
      });
    },

    // --- 漫画スタック ---
    listStacks(): Promise<StackSummary[]> {
      return request<StackSummary[]>(base(), '/api/stacks');
    },
    createStack(title: string, assetIds: string[]): Promise<StackDetail> {
      return request<StackDetail>(base(), '/api/stacks', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, asset_ids: assetIds })
      });
    },
    getStack(id: string): Promise<StackDetail> {
      return request<StackDetail>(base(), `/api/stacks/${enc(id)}`);
    },
    patchStack(
      id: string,
      patch: { title?: string; cover_asset_id?: string }
    ): Promise<StackDetail> {
      return request<StackDetail>(base(), `/api/stacks/${enc(id)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(patch)
      });
    },
    async deleteStack(id: string): Promise<void> {
      await request<Response>(base(), `/api/stacks/${enc(id)}`, { method: 'DELETE', raw: true });
    },
    replaceStructure(id: string, chapters: ChapterInput[]): Promise<StackDetail> {
      return request<StackDetail>(base(), `/api/stacks/${enc(id)}/structure`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ chapters })
      });
    },
    addStackPages(id: string, assetIds: string[], chapterId?: string): Promise<StackDetail> {
      return request<StackDetail>(base(), `/api/stacks/${enc(id)}/pages`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ asset_ids: assetIds, chapter_id: chapterId ?? null })
      });
    },
    async removeStackPage(id: string, assetId: string): Promise<void> {
      await request<Response>(base(), `/api/stacks/${enc(id)}/pages/${enc(assetId)}`, {
        method: 'DELETE',
        raw: true
      });
    },
    setPageFlag(id: string, assetId: string, showInTimeline: boolean): Promise<StackDetail> {
      return request<StackDetail>(base(), `/api/stacks/${enc(id)}/pages/${enc(assetId)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ show_in_timeline: showInTimeline })
      });
    },

    // --- 検索 ---
    search(q: string): Promise<SearchResult> {
      return request<SearchResult>(base(), `/api/search?q=${enc(q)}`);
    }
  };
}
