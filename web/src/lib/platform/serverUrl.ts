// サーバー URL の検証 (docs/12_security.md, SEC-002)。
//
// ネイティブクライアントは任意の self-hosted URL へ接続するため、保存・読み出し・
// 接続のいずれの時点でも URL を厳密に検証する。ここを緩めると、偽サーバーへ
// 共有パスワード / setup token / device token を送る経路になる。
//
// 方針:
//  - external は https のみ。平文 HTTP への例外は設けない。
//  - local は既定で https。http はプライベート宛先に限り、かつ利用者の明示確認を
//    毎回取った場合だけ使う (自動選択しない)。
//  - credential 埋め込み・fragment・query・path・制御文字は一切許可しない。

/** 検証済みのサーバー URL。文字列は正規化済み (末尾スラッシュなし)。 */
export interface ValidatedServerUrl {
  url: string;
  /** 平文 HTTP か。true の場合は自動選択せず明示確認を要求する。 */
  insecure: boolean;
}

export class ServerUrlError extends Error {}

/** RFC3986 で許されない制御文字・空白。URL パーサが黙って受ける前に弾く。 */
// eslint-disable-next-line no-control-regex
const FORBIDDEN_CHARS = /[\u0000-\u0020\u007f-\u009f\s]/;

/**
 * IPv4 / IPv6 / .local のプライベート宛先か。
 * 平文 HTTP を条件付きで許すのはこの範囲だけに限定する。
 */
export function isPrivateHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, '');
  if (host === 'localhost' || host.endsWith('.localhost') || host.endsWith('.local')) return true;
  if (host === '::1') return true;
  // fc00::/7 (ULA) と fe80::/10 (link-local)
  if (/^f[cd][0-9a-f]{2}:/.test(host)) return true;
  if (/^fe[89ab][0-9a-f]:/.test(host)) return true;

  const v4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (!v4) return false;
  const [a, b] = v4.slice(1).map(Number);
  if (v4.slice(1).some((part) => Number(part) > 255)) return false;
  if (a === 10 || a === 127) return true;
  if (a === 192 && b === 168) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 169 && b === 254) return true;
  return false;
}

export interface ParseOptions {
  /** local 用: プライベート宛先に限り平文 HTTP を受理する。 */
  allowInsecurePrivate?: boolean;
  /** エラーメッセージ用のラベル。 */
  label?: string;
}

/**
 * サーバー URL を検証して正規化する。受理できない場合は ServerUrlError を投げる。
 */
export function parseServerUrl(raw: string, options: ParseOptions = {}): ValidatedServerUrl {
  const label = options.label ?? 'URL';
  const trimmed = raw.trim();
  if (trimmed === '') throw new ServerUrlError(`${label} が空です`);
  if (FORBIDDEN_CHARS.test(trimmed)) {
    throw new ServerUrlError(`${label} に使用できない文字 (空白・制御文字) が含まれています`);
  }

  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new ServerUrlError(`${label} の形式が正しくありません`);
  }

  if (parsed.username !== '' || parsed.password !== '') {
    throw new ServerUrlError(`${label} に認証情報を埋め込むことはできません`);
  }
  if (parsed.hash !== '') throw new ServerUrlError(`${label} にフラグメントは指定できません`);
  if (parsed.search !== '') throw new ServerUrlError(`${label} にクエリは指定できません`);
  if (parsed.pathname !== '/' && parsed.pathname !== '') {
    throw new ServerUrlError(`${label} にパスは指定できません (オリジンのみ)`);
  }
  if (parsed.hostname === '') throw new ServerUrlError(`${label} のホストが空です`);
  if (parsed.port !== '') {
    const port = Number(parsed.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new ServerUrlError(`${label} のポート番号が不正です`);
    }
  }

  if (parsed.protocol === 'https:') {
    return { url: normalizeOrigin(parsed), insecure: false };
  }
  if (parsed.protocol === 'http:') {
    if (!options.allowInsecurePrivate) {
      throw new ServerUrlError(`${label} は https のみ使用できます`);
    }
    if (!isPrivateHost(parsed.hostname)) {
      throw new ServerUrlError(
        `${label} で平文 HTTP を使えるのはプライベートアドレスのみです (https を使用してください)`
      );
    }
    return { url: normalizeOrigin(parsed), insecure: true };
  }
  throw new ServerUrlError(`${label} のスキームは https のみ使用できます`);
}

function normalizeOrigin(parsed: URL): string {
  return `${parsed.protocol}//${parsed.host}`;
}
