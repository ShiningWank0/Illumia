// ネイティブ (アプリモード) のデバイストークン保持。
// docs/08: ネイティブは Bearer token を OS secure storage に保存する。
// v1 はセキュアストレージ抽象がメモリ内実装のため、再起動で失われ再ログインが必要
// (Keystore 連携は将来タスク → docs/10)。Web (Cookie 認証) では一切使わない。

import { secureDelete, secureGet, secureSet } from './tauri';

const TOKEN_KEY = 'illumia.device_token';

export function getNativeToken(): string | null {
  return secureGet(TOKEN_KEY);
}

export function setNativeToken(token: string | null): void {
  if (token) secureSet(TOKEN_KEY, token);
  else secureDelete(TOKEN_KEY);
}
