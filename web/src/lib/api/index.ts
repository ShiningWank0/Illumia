// API のエントリポイント。VITE_USE_MOCK=1 でモック実装に切替える。
//
//   実サーバー: (既定) createHttpClient(baseUrl)
//   モック:      VITE_USE_MOCK=1 → createMockClient()  (docs/03 と同一インタフェース)

import { createHttpClient, defaultBaseUrl } from './client';
import { createMockClient } from './mock';
import type { IllumiaApi } from './types';

export * from './types';
export { defaultBaseUrl } from './client';

/** モックモードか。VITE_USE_MOCK=1 で有効。 */
export function isMock(): boolean {
  return import.meta.env?.VITE_USE_MOCK === '1' || import.meta.env?.VITE_USE_MOCK === 'true';
}

let singleton: IllumiaApi | null = null;

/** アプリ全体で共有する IllumiaApi を返す。 */
export function getApi(): IllumiaApi {
  if (singleton) return singleton;
  singleton = isMock() ? createMockClient() : createHttpClient({ baseUrl: defaultBaseUrl() });
  return singleton;
}
