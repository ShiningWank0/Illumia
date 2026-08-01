// アプリモードのサーバー接続プロファイル (docs/08 §サーバー接続設定)。
// ネイティブ (Tauri) でのみ使う。ブラウザは同一オリジン固定なので不要。
//
// 1 つのサーバー登録に external / local の複数 URL を持てる。接続時に
// local → external の順で到達性プローブ (GET /api/server/info, timeout 2s) し、
// 最初に成功した URL を採用する。プロファイルは localStorage に保存する
// (device token 等の秘密は含めない)。

import { nativeFetch } from './tauri';

const STORAGE_KEY = 'illumia.connection';

export interface ConnectionProfile {
  /** 例: https://illumia.example.com (既定・外部) */
  external: string;
  /** 例: http://192.168.1.10:2283 (特定ネットワーク内) */
  local?: string;
  /**
   * local を試す Wi-Fi SSID (任意)。現状 SSID 自動取得プラグインが無いため
   * 判定には使わず、到達性プローブで代替する (docs/08 の縮退動作)。手動メモ用途。
   */
  ssid?: string;
}

function normalize(url: string): string {
  return url.trim().replace(/\/$/, '');
}

export function loadProfile(): ConnectionProfile | null {
  if (typeof localStorage === 'undefined') return null;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) return null;
  try {
    const p = JSON.parse(raw) as ConnectionProfile;
    if (!p.external) return null;
    return p;
  } catch {
    return null;
  }
}

export function saveProfile(profile: ConnectionProfile): void {
  if (typeof localStorage === 'undefined') return;
  const cleaned: ConnectionProfile = {
    external: normalize(profile.external),
    local: profile.local ? normalize(profile.local) : undefined,
    ssid: profile.ssid?.trim() || undefined
  };
  localStorage.setItem(STORAGE_KEY, JSON.stringify(cleaned));
}

export function clearProfile(): void {
  if (typeof localStorage === 'undefined') return;
  localStorage.removeItem(STORAGE_KEY);
  activeBaseUrl = null;
}

/** 現在採用している baseUrl (プローブ後に確定)。 */
let activeBaseUrl: string | null = null;

export function getActiveBaseUrl(): string | null {
  return activeBaseUrl;
}

export function setActiveBaseUrl(url: string | null): void {
  activeBaseUrl = url ? normalize(url) : null;
}

/** 1 つの URL に GET /api/server/info を timeout 付きで投げ、到達すれば true。 */
async function probe(url: string, timeoutMs = 2000): Promise<boolean> {
  const base = normalize(url);
  const timeout = new Promise<boolean>((resolve) => setTimeout(() => resolve(false), timeoutMs));
  const attempt = (async () => {
    try {
      const res = await nativeFetch(`${base}/api/server/info`, { method: 'GET' });
      return res.ok;
    } catch {
      return false;
    }
  })();
  return Promise.race([attempt, timeout]);
}

/**
 * local → external の順にプローブし、最初に到達した URL を active に設定して返す。
 * どちらも失敗したら null。
 */
export async function probeAndSelect(profile: ConnectionProfile): Promise<string | null> {
  const candidates = [profile.local, profile.external].filter((u): u is string => !!u);
  for (const candidate of candidates) {
    if (await probe(candidate)) {
      setActiveBaseUrl(candidate);
      return normalize(candidate);
    }
  }
  activeBaseUrl = null;
  return null;
}
