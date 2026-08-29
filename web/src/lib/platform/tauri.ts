// Tauri (アプリモード) 検出とネイティブ機能ブリッジ。
// ブラウザでは何も読み込まない。すべて動的 import で、Tauri 実行時のみ解決する。
//
// Android の device token は Rust bridge の process memory に閉じ込め、WebView へ返さない。
// Keystore 永続化前の縮退として、再起動後は再ログインが必要 (docs/08, docs/10)。

/** Tauri (ネイティブアプリ) 上で動いているか。 */
export function isTauri(): boolean {
  return (
    typeof window !== 'undefined' && ('__TAURI_INTERNALS__' in window || '__TAURI__' in window)
  );
}

/** Rust 側ブリッジの応答 (apps/android/src-tauri/src/bridge.rs)。 */
interface BridgeResponse {
  status: number;
  headers: Record<string, string>;
  body_base64: string;
}

// multipart overhead を含む native bridge request envelope の上限。
const MAX_NATIVE_REQUEST_BODY = 17 * 1024 * 1024;

export interface NativeProbeResponse {
  setup_completed: boolean;
  authenticated: boolean;
  setup_token_required: boolean;
  instance_id: string;
}

function base64ToBytes(encoded: string): Uint8Array {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

async function bytesToBase64(body: BodyInit): Promise<string> {
  const buffer = await new Response(body).arrayBuffer();
  if (buffer.byteLength > MAX_NATIVE_REQUEST_BODY) {
    throw new Error('native request body exceeds the 17 MiB limit');
  }
  const bytes = new Uint8Array(buffer);
  let binary = '';
  // 大きな body で引数上限に当たらないよう分割する。
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

/** 現在ブリッジに登録済みの base URL (重複登録を避けるためのキャッシュ)。 */
let boundBaseUrl: string | null = null;

/**
 * credential 無しで native 側から server identity/schema を probe する。
 */
export async function probeNativeServer(baseUrl: string): Promise<NativeProbeResponse> {
  if (!isTauri()) {
    const response = await fetch(`${baseUrl}/api/server/info`, { method: 'GET' });
    if (!response.ok) throw new Error('server probe failed');
    return (await response.json()) as NativeProbeResponse;
  }
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<NativeProbeResponse>('illumia_probe_server', { url: baseUrl });
}

/**
 * probe 済み origin と identity を Rust bridge に固定する。Rust 側は同じ値の再指定だけを
 * 許し、別 origin への再 bind はアプリ再起動まで拒否する。
 */
export async function bindNativeServer(baseUrl: string, instanceId: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('illumia_bind_server', { url: baseUrl, instanceId });
  boundBaseUrl = baseUrl;
}

/**
 * ネイティブ HTTP。汎用 plugin-http は capability から外しているため
 * (docs/12: SEC-004)、登録済み Illumia サーバー宛だけを通す Rust 側の
 * `illumia_request` command を使う。ブラウザではグローバル fetch を使う。
 */
export async function nativeFetch(input: string, init?: RequestInit): Promise<Response> {
  if (!isTauri()) return fetch(input, init);

  const { invoke } = await import('@tauri-apps/api/core');

  // input は絶対 URL。WebView から origin を変更しても自動再 bind しない。
  const url = new URL(input);
  const base = `${url.protocol}//${url.host}`;
  if (boundBaseUrl !== base) {
    throw new Error('native server origin is not bound; reconnect and restart the app');
  }

  const headers: Record<string, string> = {};
  new Headers(init?.headers ?? {}).forEach((value, key) => {
    headers[key] = value;
  });

  const response = await invoke<BridgeResponse>('illumia_request', {
    request: {
      path: `${url.pathname}${url.search}`,
      method: init?.method ?? 'GET',
      headers,
      body_base64: init?.body ? await bytesToBase64(init.body) : null
    }
  });

  return new Response(
    response.status === 204 || response.status === 304
      ? null
      : (base64ToBytes(response.body_base64) as unknown as BodyInit),
    { status: response.status, headers: response.headers }
  );
}

/**
 * Androidの原本をBase64 IPCへ載せず、native保存先へ直接streamする。
 * Rust側がmain/Vaultのoriginal endpoint、origin、headersを再検証する。
 */
export async function downloadNativeOriginal(
  input: string,
  filename: string,
  headers: Record<string, string>
): Promise<boolean> {
  if (!isTauri()) throw new Error('native download is unavailable');
  const { invoke } = await import('@tauri-apps/api/core');
  const url = new URL(input);
  const base = `${url.protocol}//${url.host}`;
  if (boundBaseUrl !== base) {
    throw new Error('native server origin is not bound; reconnect and restart the app');
  }
  return invoke<boolean>('illumia_download_original', {
    path: `${url.pathname}${url.search}`,
    headers,
    filename
  });
}

/** logout の成否にかかわらず Rust process memory の device token を破棄する。 */
export async function clearNativeAuth(): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('illumia_clear_auth');
}
