// アプリモードのサーバー接続プロファイル (docs/08 §サーバー接続設定)。
// ネイティブ (Tauri) でのみ使う。ブラウザは同一オリジン固定なので不要。
//
// セキュリティ方針 (docs/12_security.md, SEC-002):
//  - external は https のみ。平文 HTTP への例外は設けない。
//  - **external を先に試す**。local を先に試すと、別ネットワーク上の攻撃者が同じ
//    private IP で偽サーバーを立てるだけで採用され、共有パスワード / setup token /
//    device token を奪える。
//  - local が平文 HTTP の場合は自動選択しない。利用者の明示確認を**毎回**取る。
//  - 到達性プローブは 2xx だけでは信用しない。`/api/server/info` の schema を検証し、
//    初回接続で pin した `instance_id` と一致することを要求する。pin と異なる
//    サーバーへは credential を一切送らない。
//  - プロファイルは load / save の双方で URL を検証する。

import { bindNativeServer, probeNativeServer } from './tauri';
import { parseServerUrl, ServerUrlError } from './serverUrl';

const STORAGE_KEY = 'illumia.connection';

export interface ConnectionProfile {
  /** 例: https://illumia.example.com (既定・外部)。https のみ。 */
  external: string;
  /** 例: http://192.168.1.10:2283 (特定ネットワーク内)。 */
  local?: string;
  /**
   * local を試す Wi-Fi SSID (任意)。現状 SSID 自動取得プラグインが無いため
   * 判定には使わず、手動メモ用途 (docs/08 の縮退動作)。
   */
  ssid?: string;
  /**
   * 初回接続時に pin したサーバーインスタンス ID。以後、これと一致しない
   * サーバーは採用しない (偽サーバーへの credential 送信を防ぐ)。
   */
  instanceId?: string;
}

/** `/api/server/info` の最低限の schema。形が違えば Illumia サーバーとみなさない。 */
interface ServerInfoShape {
  setup_completed: boolean;
  authenticated: boolean;
  setup_token_required: boolean;
  instance_id: string;
}

function isServerInfo(value: unknown): value is ServerInfoShape {
  if (typeof value !== 'object' || value === null) return false;
  const info = value as Record<string, unknown>;
  return (
    typeof info.setup_completed === 'boolean' &&
    typeof info.authenticated === 'boolean' &&
    typeof info.setup_token_required === 'boolean' &&
    typeof info.instance_id === 'string' &&
    info.instance_id.length > 0
  );
}

/** プロファイルの URL を検証して正規化する。不正なら ServerUrlError を投げる。 */
export function validateProfile(profile: ConnectionProfile): ConnectionProfile {
  const external = parseServerUrl(profile.external, { label: '外部 URL' });
  const local = profile.local?.trim()
    ? parseServerUrl(profile.local, { label: 'ローカル URL', allowInsecurePrivate: true })
    : undefined;
  const ssid = profile.ssid?.trim() || undefined;
  if (ssid !== undefined && ssid.length > 64) {
    throw new ServerUrlError('SSID が長すぎます');
  }
  return {
    external: external.url,
    local: local?.url,
    ssid,
    instanceId: profile.instanceId
  };
}

/** local が平文 HTTP か (= 自動選択せず明示確認が必要か)。 */
export function isInsecureLocal(profile: ConnectionProfile): boolean {
  if (!profile.local) return false;
  try {
    return parseServerUrl(profile.local, { allowInsecurePrivate: true }).insecure;
  } catch {
    return false;
  }
}

export function loadProfile(): ConnectionProfile | null {
  if (typeof localStorage === 'undefined') return null;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as ConnectionProfile;
    // 保存後に localStorage が改ざんされている可能性があるため読み出し時も検証する。
    return validateProfile(parsed);
  } catch {
    return null;
  }
}

/** 検証を通ったプロファイルだけを保存する。不正なら ServerUrlError を投げる。 */
export function saveProfile(profile: ConnectionProfile): ConnectionProfile {
  const cleaned = validateProfile(profile);
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(cleaned));
  }
  return cleaned;
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
  activeBaseUrl = url ? parseServerUrl(url, { allowInsecurePrivate: true }).url : null;
}

export type ProbeOutcome =
  | { ok: true; instanceId: string }
  | { ok: false; reason: 'unreachable' | 'not-illumia' | 'identity-mismatch' };

/**
 * 1 つの URL に `GET /api/server/info` を timeout 付きで投げ、
 * Illumia サーバーであることと pin 済み instance_id との一致を確認する。
 */
async function probe(
  url: string,
  pinnedInstanceId: string | undefined,
  timeoutMs = 2000
): Promise<ProbeOutcome> {
  const timeout = new Promise<ProbeOutcome>((resolve) =>
    setTimeout(() => resolve({ ok: false, reason: 'unreachable' }), timeoutMs)
  );
  const attempt = (async (): Promise<ProbeOutcome> => {
    let body: unknown;
    try {
      body = await probeNativeServer(url);
    } catch {
      return { ok: false, reason: 'unreachable' };
    }
    // 2xx だけでは偽サーバーを排除できないので schema を確認する。
    if (!isServerInfo(body)) return { ok: false, reason: 'not-illumia' };
    // pin 済みなら同一サーバーであることを要求する。
    if (pinnedInstanceId !== undefined && body.instance_id !== pinnedInstanceId) {
      return { ok: false, reason: 'identity-mismatch' };
    }
    return { ok: true, instanceId: body.instance_id };
  })();
  return Promise.race([attempt, timeout]);
}

export interface SelectOptions {
  /**
   * local が平文 HTTP のときに呼ばれる確認コールバック。true を返した場合だけ
   * local を試す。結果は保存せず、接続のたびに確認する。
   */
  confirmInsecureLocal?: () => Promise<boolean>;
}

export interface SelectResult {
  baseUrl: string | null;
  /** pin 済み ID と異なるサーバーが応答した (中間者・偽サーバーの疑い)。 */
  identityMismatch: boolean;
  /** 初回接続で新たに pin した instance_id。 */
  pinned?: string;
}

/**
 * **external → local** の順にプローブし、最初に検証を通った URL を active にする。
 *
 * local が平文 HTTP の場合は自動選択せず、`confirmInsecureLocal` が true を
 * 返したときだけ試す (docs/12: SSID 判定または暗号学的 identity 確認が入るまでの措置)。
 */
export async function probeAndSelect(
  profile: ConnectionProfile,
  options: SelectOptions = {}
): Promise<SelectResult> {
  const validated = validateProfile(profile);
  const pinned = validated.instanceId;

  const candidates: { url: string; insecure: boolean }[] = [
    { url: validated.external, insecure: false }
  ];
  if (validated.local) {
    const local = parseServerUrl(validated.local, { allowInsecurePrivate: true });
    candidates.push({ url: local.url, insecure: local.insecure });
  }

  let identityMismatch = false;
  for (const candidate of candidates) {
    if (candidate.insecure) {
      const confirmed = options.confirmInsecureLocal ? await options.confirmInsecureLocal() : false;
      if (!confirmed) continue;
    }
    const outcome = await probe(candidate.url, pinned);
    if (outcome.ok) {
      try {
        // Rust 側でも同じ identity を再 probe してから origin をプロセス中固定する。
        await bindNativeServer(candidate.url, outcome.instanceId);
      } catch {
        identityMismatch = true;
        continue;
      }
      activeBaseUrl = candidate.url;
      return {
        baseUrl: candidate.url,
        identityMismatch: false,
        pinned: pinned === undefined ? outcome.instanceId : undefined
      };
    }
    if (outcome.reason === 'identity-mismatch') identityMismatch = true;
  }

  activeBaseUrl = null;
  return { baseUrl: null, identityMismatch };
}
