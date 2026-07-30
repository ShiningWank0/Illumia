// WebSocket 購読 (/api/ws)。assets_added を受けてバケットを再取得する想定。
//
// 【重要 / 未解決】サーバーの /api/ws は require_auth ミドルウェア配下で
// `Authorization: Bearer` ヘッダ検証を行う (crates/illumia-server: lib.rs の
// protected ルータ + auth.rs bearer_token)。ブラウザの WebSocket API は
// ハンドシェイクに任意ヘッダを付けられないため、この方式ではブラウザから接続
// できない (401)。`?token=` クエリや Sec-WebSocket-Protocol 経由のトークン
// 受け渡しにサーバー側を対応させる必要がある。
//
// それまで WS 配線は無効化する。サーバーが `?token=` に対応したら
// WS_SUPPORTED を true にし、connectAssetsWs を呼べば動くよう実装してある。

import { defaultBaseUrl } from './client';
import { getToken } from './token';
import type { WsMessage } from './types';

/** サーバーがブラウザから通る WS 認証に対応したら true にする。 */
export const WS_SUPPORTED = false;

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
