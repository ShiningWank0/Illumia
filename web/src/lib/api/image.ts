// 画像取得ヘルパ。
// thumbnail/preview/original は Bearer 認証必須で、ブラウザの <img src> は
// Cookie 認証付き fetch で取得して object URL 化する。
// data: URL (モック) はそのまま返す。取得済み object URL は上限付きで再利用する。

import { getVaultToken } from '$lib/vaultSession.svelte';
import { isTauri, nativeFetch } from '$lib/platform/tauri';
import { getNativeToken } from '$lib/platform/nativeAuth';
import { thumbHashToDataURL } from 'thumbhash';
import { ApiError } from './types';

// url -> object URL。挿入順で最古を revoke する簡易 LRU。
const objectUrls = new Map<string, string>();
const MAX_OBJECT_URLS = 400;

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

/** 認証ヘッダ。ネイティブは Bearer、vault URL には X-Vault-Session も付与。 */
function authHeaders(url: string): Record<string, string> {
  const headers: Record<string, string> = {};
  if (isTauri()) {
    const t = getNativeToken();
    if (t) headers.Authorization = `Bearer ${t}`;
  }
  if (isVaultUrl(url)) {
    const vt = getVaultToken();
    if (vt) headers['X-Vault-Session'] = vt;
  }
  return headers;
}

/** ネイティブは CORS 回避のため plugin-http、ブラウザは同一オリジン Cookie。 */
async function fetchAsset(url: string): Promise<Response> {
  const headers = authHeaders(url);
  if (isTauri()) {
    return nativeFetch(url, { headers, credentials: 'omit' });
  }
  const target = validatedAppUrl(url);
  return fetch(target, { headers, credentials: 'same-origin' });
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
    return cached;
  }

  const res = await fetchAsset(url);
  if (!res.ok) {
    // vault URL のログにパス (asset id を含む) を残さない。
    throw new ApiError(res.status, `http_${res.status}`, 'image fetch failed');
  }
  const blob = await res.blob();
  const objectUrl = URL.createObjectURL(blob);
  objectUrls.set(url, objectUrl);

  while (objectUrls.size > MAX_OBJECT_URLS) {
    const oldestKey = objectUrls.keys().next().value as string | undefined;
    if (oldestKey === undefined) break;
    const oldest = objectUrls.get(oldestKey);
    objectUrls.delete(oldestKey);
    if (oldest && oldest.startsWith('blob:')) URL.revokeObjectURL(oldest);
  }
  return objectUrl;
}

/**
 * vault 画像の object URL をすべて revoke してキャッシュから外す。
 * ロック時・vault 画面離脱時に呼び、メモリに残さない (docs/06 脅威モデル)。
 */
export function revokeVaultObjectUrls(): void {
  for (const [key, value] of [...objectUrls.entries()]) {
    if (isVaultUrl(key)) {
      objectUrls.delete(key);
      if (value.startsWith('blob:')) URL.revokeObjectURL(value);
    }
  }
}

/**
 * 原本を認証付きで取得し、ファイルとして保存する (vault でもダウンロード可)。
 */
export async function downloadOriginal(url: string, filename: string): Promise<void> {
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
