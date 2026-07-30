// device_token の保持。localStorage に置く (SPA モード, ssr:false 前提)。

const TOKEN_KEY = 'illumia.device_token';

function hasStorage(): boolean {
  return typeof localStorage !== 'undefined';
}

/** 保存済みトークンを取得 (無ければ null)。 */
export function getToken(): string | null {
  if (!hasStorage()) return null;
  return localStorage.getItem(TOKEN_KEY);
}

/** トークンを保存 / null で消去。 */
export function setToken(token: string | null): void {
  if (!hasStorage()) return;
  if (token === null) {
    localStorage.removeItem(TOKEN_KEY);
  } else {
    localStorage.setItem(TOKEN_KEY, token);
  }
}
