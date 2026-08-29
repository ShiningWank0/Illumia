// 実サーバー向け実装。docs/03 の REST を叩く。
// HttpOnly Cookie 認証を使い、エラー封筒を ApiError に変換する。
// 画像 (thumbnail/preview/original) は認証必須のため URL のみ返し、
// 実取得は image.ts の authedObjectUrl が Bearer 付き fetch で行う。

import { blake3 } from 'hash-wasm';

import { isTauri, nativeFetch } from '$lib/platform/tauri';
import { getActiveBaseUrl } from '$lib/platform/connection';
import { mapCluster, mapClusterAssets, mapClusters, mapSearchResult } from './mappers';
import {
  ApiError,
  type ApiErrorBody,
  type AppSettings,
  type Asset,
  type AuthRequest,
  type Bbox,
  type Bucket,
  type BucketItem,
  type Candidate,
  type ChapterInput,
  type Cluster,
  type ClusterAsset,
  type DuplicatePair,
  type Granularity,
  type IllumiaApi,
  type Job,
  type MlStatus,
  type SearchResult,
  type ServerInfo,
  type StackDetail,
  type StackSummary,
  type UploadResult
} from './types';

export interface ClientConfig {
  baseUrl?: string;
}

interface ServerFaceRecord {
  id: string;
  asset_id: string;
  bbox: Bbox;
  cluster_id: string | null;
  state?: string;
  similarity?: number | null;
}
interface ServerReviewCandidate {
  face: ServerFaceRecord;
  asset: Asset;
}
interface ServerMlStatus {
  enabled: boolean;
  sidecar: {
    backend: string;
    model_bundle?: { name: string; version: string; sha256: string } | null;
  } | null;
}

function toCandidate(c: ServerReviewCandidate): Candidate {
  return {
    face_id: c.face.id,
    asset_id: c.face.asset_id,
    bbox: c.face.bbox,
    cluster_id: c.face.cluster_id,
    cluster_name: null,
    similarity: c.face.similarity ?? null
  };
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
 * サーバーは device token を body で返すが、Android Rust bridge が response を捕捉し、
 * WebView へは token を除いた成功応答だけを返す。
 */
export async function nativeSetup(req: AuthRequest, setupToken?: string): Promise<void> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (setupToken) headers['X-Illumia-Setup-Token'] = setupToken;
  await request<Record<string, never>>(resolveBaseUrl(), '/api/auth/setup', {
    method: 'POST',
    headers,
    body: JSON.stringify(req)
  });
}

/** ネイティブ用 login。device token は Rust bridge が捕捉し、JS へ返さない。 */
export async function nativeLogin(req: AuthRequest): Promise<void> {
  await request<Record<string, never>>(resolveBaseUrl(), '/api/auth/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req)
  });
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

  if (!native && typeof location !== 'undefined') {
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

    // --- キャラクター (クラスタ) ---
    // サーバーの ClusterSummary / FaceRecord / AssetResponse を UI 型へマップする。
    async listClusters(): Promise<Cluster[]> {
      return mapClusters(await request<unknown>(base(), '/api/clusters'));
    },
    async getClusterAssets(id: string): Promise<ClusterAsset[]> {
      return mapClusterAssets(await request<unknown>(base(), `/api/clusters/${enc(id)}/assets`));
    },
    async renameCluster(id: string, name: string): Promise<Cluster> {
      const response = await request<unknown>(base(), `/api/clusters/${enc(id)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      return mapCluster(response);
    },
    async mergeClusters(fromId: string, intoId: string): Promise<void> {
      await request<Response>(base(), '/api/clusters/merge', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ from_id: fromId, into_id: intoId }),
        raw: true
      });
    },
    async splitCluster(id: string, faceIds: string[]): Promise<Cluster> {
      const response = await request<unknown>(base(), `/api/clusters/${enc(id)}/split`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: faceIds })
      });
      return mapCluster(response);
    },
    async getReviewCandidates(): Promise<Candidate[]> {
      const rows = await request<ServerReviewCandidate[]>(base(), '/api/review/candidates');
      return rows.map(toCandidate);
    },
    async reviewCandidate(faceId: string, action: 'accept' | 'reject'): Promise<void> {
      await request<Response>(base(), `/api/review/candidates/${enc(faceId)}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action }),
        raw: true
      });
    },

    // --- ML 制御 ---
    async mlStatus(): Promise<MlStatus> {
      const raw = await request<ServerMlStatus>(base(), '/api/ml/status');
      const backend = raw.sidecar?.backend === 'onnx' ? 'onnx' : 'mock';
      return {
        enabled: Boolean(raw.enabled),
        backend,
        bundle_version: raw.sidecar?.model_bundle?.version ?? null,
        model_ready: backend === 'onnx'
      };
    },
    async analyzeAll(): Promise<void> {
      await request<Response>(base(), '/api/ml/analyze-all', { method: 'POST', raw: true });
    },
    async recluster(): Promise<void> {
      await request<Response>(base(), '/api/ml/recluster', { method: 'POST', raw: true });
    },
    getJobs(state?: string): Promise<Job[]> {
      const q = state ? `?state=${enc(state)}` : '';
      return request<Job[]>(base(), `/api/jobs${q}`);
    },

    // --- 検索 ---
    async search(q: string): Promise<SearchResult> {
      return mapSearchResult(await request<unknown>(base(), `/api/search?q=${enc(q)}`));
    }
  };
}
