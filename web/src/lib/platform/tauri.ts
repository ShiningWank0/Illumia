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

type FetchFn = (input: string, init?: RequestInit) => Promise<Response>;

let nativeFetchImpl: FetchFn | null = null;

/**
 * ネイティブ HTTP。Tauri では tauri-plugin-http の fetch を使い、WebView の CORS 制約と
 * クロスオリジン Cookie の問題を回避する (docs/12: ネイティブに CORS は不要)。
 * ブラウザではグローバル fetch にフォールバックする。
 */
export async function nativeFetch(input: string, init?: RequestInit): Promise<Response> {
  if (!isTauri()) return fetch(input, init);
  if (!nativeFetchImpl) {
    try {
      const mod = await import('@tauri-apps/plugin-http');
      nativeFetchImpl = mod.fetch as unknown as FetchFn;
    } catch {
      nativeFetchImpl = fetch;
    }
  }
  return nativeFetchImpl(input, init);
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
