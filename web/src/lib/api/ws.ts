// WebSocket 購読 (/api/ws)。HttpOnly 認証 Cookie は browser が自動送信する。

import { defaultBaseUrl } from './client';
import type { WsMessage } from './types';

/** サーバーが同一オリジン Cookie 認証に対応済みのため有効。 */
export const WS_SUPPORTED = true;

export interface WsHandle {
  close(): void;
}

/**
 * /api/ws に接続し assets_added を購読する。
 */
export function connectAssetsWs(onAssetsAdded: (bucketKeys: string[]) => void): WsHandle {
  const base = defaultBaseUrl();
  const httpOrigin = base || (typeof location !== 'undefined' ? location.origin : '');
  const wsUrl = httpOrigin.replace(/^http/, 'ws');
  const socket = new WebSocket(`${wsUrl}/api/ws`);

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
