// Vault 用 API。
//  - ライフサイクル: init / unlock / lock / status / import / export
//  - ミラークライアント: /api/vault/... を叩き X-Vault-Session を付与する IllumiaApi
//
// vault: no-log — asset id / ファイル名 / トークン / 検索語をログに出さない。

import { request, resolveBaseUrl } from './client';
import { getVaultToken, vaultSession } from '$lib/vaultSession.svelte';
import { isMock } from './index';
import { createMockVaultClient, mockVaultLifecycle } from './mock';
import { mapCluster, mapClusterAssets, mapClusters, mapSearchResult } from './mappers';
import {
  ApiError,
  type AppSettings,
  type Asset,
  type Bucket,
  type BucketItem,
  type ChapterInput,
  type Cluster,
  type ClusterAsset,
  type DuplicatePair,
  type Granularity,
  type IllumiaApi,
  type SearchResult,
  type ServerInfo,
  type StackDetail,
  type StackSummary,
  type UploadResult,
  type VaultStatusResponse,
  type VaultTransfer,
  type VaultUnlockResponse
} from './types';

const base = () => resolveBaseUrl();

/** X-Vault-Session を付けたヘッダを返す。 */
function vaultHeaders(extra: Record<string, string> = {}): Record<string, string> {
  const t = getVaultToken();
  return t ? { 'X-Vault-Session': t, ...extra } : extra;
}

// vault API が 404 を返したら「ロックされた」とみなす。
function guard<T>(p: Promise<T>): Promise<T> {
  return p.catch((e) => {
    if (e instanceof ApiError && e.status === 404) vaultSession.onVaultNotFound();
    throw e;
  });
}

export interface VaultLifecycle {
  status(): Promise<VaultStatusResponse>;
  init(password: string): Promise<{ recovery_key: string }>;
  unlock(payload: { password?: string; recovery_key?: string }): Promise<VaultUnlockResponse>;
  lock(): Promise<void>;
  importItems(payload: VaultTransfer): Promise<void>;
  exportItems(payload: VaultTransfer): Promise<void>;
}

/** 実サーバー用の vault ライフサイクル。 */
const httpLifecycle: VaultLifecycle = {
  status() {
    return request<VaultStatusResponse>(base(), '/api/vault/status');
  },
  init(password: string) {
    return request<{ recovery_key: string }>(base(), '/api/vault/init', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password })
    });
  },
  unlock(payload) {
    return request<VaultUnlockResponse>(base(), '/api/vault/unlock', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    });
  },
  async lock() {
    await request<Response>(base(), '/api/vault/lock', {
      method: 'POST',
      headers: vaultHeaders(),
      raw: true
    });
  },
  async importItems(payload: VaultTransfer) {
    await guard(
      request<Response>(base(), '/api/vault/import', {
        method: 'POST',
        headers: vaultHeaders({ 'Content-Type': 'application/json' }),
        body: JSON.stringify(payload),
        raw: true
      })
    );
  },
  async exportItems(payload: VaultTransfer) {
    await guard(
      request<Response>(base(), '/api/vault/export', {
        method: 'POST',
        headers: vaultHeaders({ 'Content-Type': 'application/json' }),
        body: JSON.stringify(payload),
        raw: true
      })
    );
  }
};

/** VITE_USE_MOCK に応じてライフサイクル実装を返す。 */
export function getVaultLifecycle(): VaultLifecycle {
  return isMock() ? mockVaultLifecycle : httpLifecycle;
}

function unsupported(name: string): never {
  throw new ApiError(0, 'unsupported', `${name} is not available in vault`);
}

/**
 * vault ミラー用の IllumiaApi。Timeline / スタック / 検索コンポーネントを
 * そのまま再利用するため、同じインタフェースで /api/vault/... を叩く。
 * vault に存在しない操作 (upload / settings / auth) は投げる。
 */
export function createHttpVaultClient(): IllumiaApi {
  const enc = encodeURIComponent;
  const b = () => base();
  const vreq = <T>(path: string, opts: Parameters<typeof request>[2] = {}): Promise<T> =>
    guard(request<T>(b(), path, { ...opts, headers: vaultHeaders(opts?.headers) }));

  return {
    serverInfo(): Promise<ServerInfo> {
      return unsupported('serverInfo');
    },
    setup(): Promise<void> {
      return unsupported('setup');
    },
    login(): Promise<void> {
      return unsupported('login');
    },
    logout(): Promise<void> {
      return unsupported('logout');
    },

    getBuckets(g: Granularity): Promise<Bucket[]> {
      return vreq<Bucket[]>(`/api/vault/timeline/buckets?granularity=${g}`);
    },
    getBucketItems(g: Granularity, key: string): Promise<BucketItem[]> {
      return vreq<BucketItem[]>(`/api/vault/timeline/buckets/${enc(key)}?granularity=${g}`);
    },
    thumbnailUrl(id: string): string {
      return `${b()}/api/vault/assets/${enc(id)}/thumbnail`;
    },
    previewUrl(id: string): string {
      return `${b()}/api/vault/assets/${enc(id)}/preview`;
    },
    originalUrl(id: string): string {
      return `${b()}/api/vault/assets/${enc(id)}/original`;
    },

    uploadAsset(): Promise<UploadResult> {
      return unsupported('uploadAsset');
    },
    assetsExist(): Promise<Record<string, string>> {
      return unsupported('assetsExist');
    },
    async trashAsset(id: string): Promise<void> {
      await vreq<Asset>(`/api/vault/assets/${enc(id)}`, { method: 'DELETE' });
    },
    async restoreAsset(id: string): Promise<void> {
      await vreq<Asset>(`/api/vault/assets/${enc(id)}/restore`, { method: 'POST' });
    },

    getTrash(): Promise<Asset[]> {
      return vreq<Asset[]>('/api/vault/trash');
    },
    getDuplicates(): Promise<DuplicatePair[]> {
      return vreq<DuplicatePair[]>('/api/vault/duplicates');
    },
    async purgeNow(id: string): Promise<void> {
      await vreq<Response>(`/api/vault/trash/${enc(id)}`, { method: 'DELETE', raw: true });
    },

    getSettings(): Promise<AppSettings> {
      return unsupported('getSettings');
    },
    patchSettings(): Promise<AppSettings> {
      return unsupported('patchSettings');
    },

    listStacks(): Promise<StackSummary[]> {
      return vreq<StackSummary[]>('/api/vault/stacks');
    },
    createStack(title: string, assetIds: string[]): Promise<StackDetail> {
      return vreq<StackDetail>('/api/vault/stacks', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, asset_ids: assetIds })
      });
    },
    getStack(id: string): Promise<StackDetail> {
      return vreq<StackDetail>(`/api/vault/stacks/${enc(id)}`);
    },
    patchStack(id, patch): Promise<StackDetail> {
      return vreq<StackDetail>(`/api/vault/stacks/${enc(id)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(patch)
      });
    },
    async deleteStack(id: string): Promise<void> {
      await vreq<Response>(`/api/vault/stacks/${enc(id)}`, { method: 'DELETE', raw: true });
    },
    replaceStructure(id: string, chapters: ChapterInput[]): Promise<StackDetail> {
      return vreq<StackDetail>(`/api/vault/stacks/${enc(id)}/structure`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ chapters })
      });
    },
    addStackPages(id: string, assetIds: string[], chapterId?: string): Promise<StackDetail> {
      return vreq<StackDetail>(`/api/vault/stacks/${enc(id)}/pages`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ asset_ids: assetIds, chapter_id: chapterId ?? null })
      });
    },
    async removeStackPage(id: string, assetId: string): Promise<void> {
      await vreq<Response>(`/api/vault/stacks/${enc(id)}/pages/${enc(assetId)}`, {
        method: 'DELETE',
        raw: true
      });
    },
    setPageFlag(id: string, assetId: string, showInTimeline: boolean): Promise<StackDetail> {
      return vreq<StackDetail>(`/api/vault/stacks/${enc(id)}/pages/${enc(assetId)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ show_in_timeline: showInTimeline })
      });
    },

    // --- キャラクター (クラスタ) --- サーバー DTO を UI 型へマップ。
    async listClusters(): Promise<Cluster[]> {
      return mapClusters(await vreq<unknown>('/api/vault/clusters'));
    },
    async getClusterAssets(id: string): Promise<ClusterAsset[]> {
      return mapClusterAssets(await vreq<unknown>(`/api/vault/clusters/${enc(id)}/assets`));
    },
    async renameCluster(id: string, name: string): Promise<Cluster> {
      const response = await vreq<unknown>(`/api/vault/clusters/${enc(id)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      return mapCluster(response);
    },
    async mergeClusters(fromId: string, intoId: string): Promise<void> {
      await vreq<Response>('/api/vault/clusters/merge', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ from_id: fromId, into_id: intoId }),
        raw: true
      });
    },
    async splitCluster(id: string, faceIds: string[]): Promise<Cluster> {
      const response = await vreq<unknown>(`/api/vault/clusters/${enc(id)}/split`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: faceIds })
      });
      return mapCluster(response);
    },
    // 確認キュー / ML 制御 / ジョブは vault では提供しない (ML 設定は全体設定)。
    getReviewCandidates: () => unsupported('getReviewCandidates'),
    reviewCandidate: () => unsupported('reviewCandidate'),
    mlStatus: () => unsupported('mlStatus'),
    analyzeAll: () => unsupported('analyzeAll'),
    recluster: () => unsupported('recluster'),
    getJobs: () => unsupported('getJobs'),

    async search(q: string): Promise<SearchResult> {
      return mapSearchResult(await vreq<unknown>(`/api/vault/search?q=${enc(q)}`));
    }
  };
}

let vaultClientSingleton: IllumiaApi | null = null;

/** vault ミラークライアント (mock 切替対応)。 */
export function getVaultApi(): IllumiaApi {
  if (vaultClientSingleton) return vaultClientSingleton;
  vaultClientSingleton = isMock() ? createMockVaultClient() : createHttpVaultClient();
  return vaultClientSingleton;
}
