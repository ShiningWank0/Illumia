// 画像取得ヘルパ。
// thumbnail/preview/original は Bearer 認証必須で、ブラウザの <img src> は
// Cookie 認証付き fetch で取得して object URL 化する。
// data: URL (モック) はそのまま返す。取得済み object URL は上限付きで再利用する。

import { getVaultToken } from '$lib/vaultSession.svelte';
import { downloadNativeOriginal, isTauri, nativeFetch } from '$lib/platform/tauri';
import { thumbHashToDataURL } from 'thumbhash';
import { ApiError } from './types';

interface CachedObjectUrl {
  objectUrl: string;
  bytes: number;
}

// url -> object URL。挿入順で最古を revoke する簡易 LRU。
const objectUrls = new Map<string, CachedObjectUrl>();
const inFlightObjectUrls = new Map<string, Promise<string>>();
const MAX_OBJECT_URLS = 400;
const MAX_OBJECT_URL_BYTES = 96 * 1024 * 1024;
const MAX_THUMBNAIL_BYTES = 2 * 1024 * 1024;
const MAX_PREVIEW_BYTES = 16 * 1024 * 1024;
let objectUrlBytes = 0;
let cacheEpoch = 0;
let vaultCacheEpoch = 0;

function validatedAppUrl(url: string): URL {
  const base = typeof location === 'undefined' ? 'http://illumia.invalid' : location.origin;
  const parsed = new URL(url, base);
  if (typeof location !== 'undefined' && parsed.origin !== location.origin) {
    throw new ApiError(0, 'cross_origin_url', 'cross-origin image URLs are not allowed');
  }
  return parsed;
}

/** vault 配下の URL か (X-Vault-Session を付ける対象)。相対・絶対どちらも判定。 */
function isVaultUrl(url: string): boolean {
  return url.includes('/api/vault/');
}

function imageResponseLimit(url: string): number {
  const base = typeof location === 'undefined' ? 'http://illumia.invalid' : location.origin;
  const pathname = new URL(url, base).pathname;
  if (pathname.endsWith('/thumbnail')) return MAX_THUMBNAIL_BYTES;
  if (pathname.endsWith('/preview')) return MAX_PREVIEW_BYTES;
  throw new ApiError(0, 'invalid_image_endpoint', 'only thumbnail/preview URLs can be cached');
}

function removeCachedObjectUrl(key: string): void {
  const cached = objectUrls.get(key);
  if (!cached) return;
  objectUrls.delete(key);
  objectUrlBytes = Math.max(0, objectUrlBytes - cached.bytes);
  if (cached.objectUrl.startsWith('blob:')) URL.revokeObjectURL(cached.objectUrl);
}

function enforceObjectUrlLimits(): void {
  while (objectUrls.size > MAX_OBJECT_URLS || objectUrlBytes > MAX_OBJECT_URL_BYTES) {
    const oldestKey = objectUrls.keys().next().value as string | undefined;
    if (oldestKey === undefined) break;
    removeCachedObjectUrl(oldestKey);
  }
}

/** vault URL のセッションヘッダ。device token は native Rust bridge が付与する。 */
function authHeaders(url: string): Record<string, string> {
  const headers: Record<string, string> = {};
  if (isVaultUrl(url)) {
    const vt = getVaultToken();
    if (vt) headers['X-Vault-Session'] = vt;
  }
  return headers;
}

/** ネイティブは専用ブリッジ (illumia_request)、ブラウザは同一オリジン Cookie。 */
async function fetchAsset(url: string): Promise<Response> {
  const headers = authHeaders(url);
  if (isTauri()) {
    return nativeFetch(url, { headers, credentials: 'omit' });
  }
  const target = validatedAppUrl(url);
  return fetch(target, { headers, credentials: 'same-origin' });
}

async function readCappedImageBlob(res: Response, limit: number): Promise<Blob> {
  const contentLength = res.headers?.get('content-length');
  if (contentLength && /^\d+$/.test(contentLength) && Number(contentLength) > limit) {
    await res.body?.cancel().catch(() => undefined);
    throw new ApiError(413, 'image_too_large', 'image response exceeds client limit');
  }

  // 実ブラウザの fetch Response は stream を持つ。上限超過時点で cancel し、
  // `Response.blob()` が攻撃者の全responseを先にbufferする経路を避ける。
  if (res.body) {
    const reader = res.body.getReader();
    const chunks: Uint8Array<ArrayBuffer>[] = [];
    let total = 0;
    try {
      let result = await reader.read();
      while (!result.done) {
        const { value } = result;
        total += value.byteLength;
        if (total > limit) {
          await reader.cancel().catch(() => undefined);
          throw new ApiError(413, 'image_too_large', 'image response exceeds client limit');
        }
        chunks.push(value.slice());
        result = await reader.read();
      }
    } finally {
      reader.releaseLock();
    }
    return new Blob(chunks, { type: res.headers.get('content-type') ?? undefined });
  }

  // Unit-test/legacy Response fallback。取得後にも必ず同じ上限を検証する。
  const blob = await res.blob();
  if (blob.size > limit) {
    throw new ApiError(413, 'image_too_large', 'image response exceeds client limit');
  }
  return blob;
}

/**
 * 認証付きで画像を取得し object URL を返す。data:/blob: はそのまま返す。
 */
export async function authedObjectUrl(url: string): Promise<string> {
  if (url === '') throw new ApiError(0, 'empty_url', 'empty image url');
  if (url.startsWith('data:') || url.startsWith('blob:')) return url;

  const cached = objectUrls.get(url);
  if (cached) {
    // 参照されたので最近使用側へ。
    objectUrls.delete(url);
    objectUrls.set(url, cached);
    return cached.objectUrl;
  }

  const inFlight = inFlightObjectUrls.get(url);
  if (inFlight) return inFlight;

  const responseLimit = imageResponseLimit(url);
  const requestCacheEpoch = cacheEpoch;
  const requestVaultEpoch = vaultCacheEpoch;
  const request = (async () => {
    const res = await fetchAsset(url);
    if (!res.ok) {
      // vault URL のログにパス (asset id を含む) を残さない。
      throw new ApiError(res.status, `http_${res.status}`, 'image fetch failed');
    }
    const blob = await readCappedImageBlob(res, responseLimit);
    // logout/Vault lock と競合した response を後から cache に復活させない。
    if (
      requestCacheEpoch !== cacheEpoch ||
      (isVaultUrl(url) && requestVaultEpoch !== vaultCacheEpoch)
    ) {
      throw new ApiError(0, 'image_cache_revoked', 'image cache was revoked while loading');
    }
    const objectUrl = URL.createObjectURL(blob);
    objectUrls.set(url, { objectUrl, bytes: blob.size });
    objectUrlBytes += blob.size;
    enforceObjectUrlLimits();
    return objectUrl;
  })();
  inFlightObjectUrls.set(url, request);
  try {
    return await request;
  } finally {
    if (inFlightObjectUrls.get(url) === request) inFlightObjectUrls.delete(url);
  }
}

/**
 * vault 画像の object URL をすべて revoke してキャッシュから外す。
 * ロック時・vault 画面離脱時に呼び、メモリに残さない (docs/06 脅威モデル)。
 */
export function revokeVaultObjectUrls(): void {
  vaultCacheEpoch += 1;
  for (const key of [...objectUrls.keys()]) {
    if (isVaultUrl(key)) {
      removeCachedObjectUrl(key);
    }
  }
}

/** logout/session失効時に全認証済みBlobを破棄する。 */
export function revokeAllObjectUrls(): void {
  cacheEpoch += 1;
  vaultCacheEpoch += 1;
  for (const key of [...objectUrls.keys()]) removeCachedObjectUrl(key);
  // 浮動小数ではなく整数byteだが、防御的に空cacheとの整合を固定する。
  objectUrlBytes = 0;
}

/**
 * 原本を認証付きで取得し、ファイルとして保存する (vault でもダウンロード可)。
 */
export async function downloadOriginal(url: string, filename: string): Promise<void> {
  if (isTauri()) {
    await downloadNativeOriginal(url, filename, authHeaders(url));
    return;
  }
  const res = await fetchAsset(url);
  if (!res.ok) throw new ApiError(res.status, `http_${res.status}`, 'download failed');
  const blob = await res.blob();
  const objectUrl = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = objectUrl;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // 少し待ってから revoke (保存ダイアログが掴むため)。
  setTimeout(() => URL.revokeObjectURL(objectUrl), 10_000);
}

/** 標準 base64 文字列を Uint8Array に復号する (thumbhash 用)。 */
function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/**
 * thumbhash (base64) をプレースホルダ data URL に復号する。
 * 不正な入力や null は null を返す (呼び出し側は色フォールバック)。
 */
export function thumbhashToDataUrl(thumbhash: string | null | undefined): string | null {
  if (!thumbhash) return null;
  try {
    const bytes = base64ToBytes(thumbhash);
    if (bytes.length < 5) return null; // thumbhash は最低 5 バイト
    return thumbHashToDataURL(bytes);
  } catch {
    return null;
  }
}
