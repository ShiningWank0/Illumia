// Tauri (アプリモード) 検出とネイティブ機能ブリッジ。
// ブラウザでは何も読み込まない。すべて動的 import で、Tauri 実行時のみ解決する。
//
// 縮退方針 (docs/08):
//  - デバイストークン / vault パスワードの永続保存には Android Keystore を使うのが理想だが、
//    Tauri 公式の汎用 Keystore プラグインが無いため v1 は**メモリ内保持**とする
//    (再起動で失われ、再ログイン / 再アンロックが必要)。Keystore 連携は将来タスク (docs/10)。

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

function base64ToBytes(encoded: string): Uint8Array {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

async function bytesToBase64(body: BodyInit): Promise<string> {
  const buffer = await new Response(body).arrayBuffer();
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
 * ブリッジへ接続先サーバーを登録する。Rust 側でも URL を検証する。
 * 未登録のまま `illumia_request` を呼ぶと拒否される。
 */
export async function bindNativeServer(baseUrl: string | null): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('illumia_set_server', { url: baseUrl });
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

  // input は絶対 URL。base 部分はブリッジ側の登録値と突き合わせるので path だけ渡す。
  const url = new URL(input);
  const base = `${url.protocol}//${url.host}`;
  if (boundBaseUrl !== base) await bindNativeServer(base);

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

// ---- 生体認証 (vault アンロックの代替) ----

export interface BiometricAvailability {
  available: boolean;
  reason?: string;
}

/** 端末が生体認証に対応しているか。 */
export async function biometricStatus(): Promise<BiometricAvailability> {
  if (!isTauri()) return { available: false, reason: 'not a native app' };
  try {
    const mod = await import('@tauri-apps/plugin-biometric');
    const status = await mod.checkStatus();
    return { available: status.isAvailable, reason: status.error };
  } catch (e) {
    return { available: false, reason: e instanceof Error ? e.message : 'biometric unavailable' };
  }
}

/**
 * 生体認証を要求する。成功で true。
 * ここでは「認証の可否」だけを扱い、鍵素材は扱わない (docs/06 の脅威モデル)。
 */
export async function biometricAuthenticate(reason: string): Promise<boolean> {
  if (!isTauri()) return false;
  try {
    const mod = await import('@tauri-apps/plugin-biometric');
    await mod.authenticate(reason, { allowDeviceCredential: true });
    return true;
  } catch {
    return false;
  }
}

// ---- セキュアストレージ (現状はメモリ内。将来 Keystore 連携) ----

const memoryStore = new Map<string, string>();

export function secureSet(key: string, value: string): void {
  memoryStore.set(key, value);
}
export function secureGet(key: string): string | null {
  return memoryStore.get(key) ?? null;
}
export function secureDelete(key: string): void {
  memoryStore.delete(key);
}
