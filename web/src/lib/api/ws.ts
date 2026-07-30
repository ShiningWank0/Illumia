// WebSocket 購読 (/api/ws)。assets_added を受けてバケットを再取得する。
// サーバーは `?token=` クエリ認証に対応済み (crates/illumia-server api.rs websocket)。

import { defaultBaseUrl } from './client';
import { getToken } from './token';
import type { WsMessage } from './types';

/** サーバーがブラウザから通る WS 認証 (?token=) に対応済みのため有効。 */
export const WS_SUPPORTED = true;

export interface WsHandle {
  close(): void;
}

/**
 * /api/ws に接続し assets_added を購読する。
 * サーバーが `?token=` に対応する前提の実装 (現状 WS_SUPPORTED=false)。
 */
export function connectAssetsWs(onAssetsAdded: (bucketKeys: string[]) => void): WsHandle {
  const base = defaultBaseUrl();
  const httpOrigin = base || (typeof location !== 'undefined' ? location.origin : '');
  const wsUrl = httpOrigin.replace(/^http/, 'ws');
  const token = getToken() ?? '';
  // 将来のサーバー対応を見越し token をクエリに載せる。
  const socket = new WebSocket(`${wsUrl}/api/ws?token=${encodeURIComponent(token)}`);

  socket.addEventListener('message', (ev) => {
    try {
      const msg = JSON.parse(ev.data as string) as WsMessage;
      if (msg.type === 'assets_added') onAssetsAdded(msg.bucket_keys);
    } catch {
      // 非 JSON は無視。
    }
  });

  return {
    close() {
      socket.close();
    }
  };
}
