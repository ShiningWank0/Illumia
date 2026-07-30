// 画像取得ヘルパ。
// thumbnail/preview/original は Bearer 認証必須で、ブラウザの <img src> は
// Authorization ヘッダを付けられないため、fetch で取得して object URL 化する。
// data: URL (モック) はそのまま返す。取得済み object URL は上限付きで再利用する。

import { getToken } from './token';
import { thumbHashToDataURL } from 'thumbhash';
import { ApiError } from './types';

// url -> object URL。挿入順で最古を revoke する簡易 LRU。
const objectUrls = new Map<string, string>();
const MAX_OBJECT_URLS = 400;

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

  const token = getToken();
  const res = await fetch(url, {
    headers: token ? { Authorization: `Bearer ${token}` } : {}
  });
  if (!res.ok) {
    throw new ApiError(res.status, `http_${res.status}`, `image fetch failed: ${url}`);
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
