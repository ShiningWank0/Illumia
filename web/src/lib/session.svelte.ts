// 認証セッションのグローバル状態。ルートレイアウトの認可ゲートで使う。
// GET /api/server/info の setup_completed / authenticated で状態を決める。

import { getApi } from '$lib/api';
import { nativeLogin, nativeSetup } from '$lib/api/client';
import { clearNativeAuth, isTauri } from '$lib/platform/tauri';
import { ApiError } from '$lib/api/types';
import { revokeAllObjectUrls } from '$lib/api/image';

export type SessionStatus = 'loading' | 'needs-setup' | 'needs-login' | 'authed' | 'error';

function messageOf(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return 'unknown error';
}

class Session {
  status = $state<SessionStatus>('loading');
  version = $state('');
  error = $state<string | null>(null);
  setupTokenRequired = $state(false);

  /** 起動時: サーバー状態とトークン有無から初期状態を決める。 */
  async init(): Promise<void> {
    this.status = 'loading';
    this.error = null;
    try {
      const info = await getApi().serverInfo();
      this.version = info.version ?? '';
      this.setupTokenRequired = info.setup_token_required;
      if (!info.setup_completed) {
        revokeAllObjectUrls();
        this.status = 'needs-setup';
      } else {
        this.status = info.authenticated ? 'authed' : 'needs-login';
        if (!info.authenticated) revokeAllObjectUrls();
      }
    } catch (e) {
      revokeAllObjectUrls();
      this.status = 'error';
      this.error = messageOf(e);
    }
  }

  async setup(password: string, deviceName: string, setupToken?: string): Promise<void> {
    if (isTauri()) {
      await nativeSetup({ password, device_name: deviceName }, setupToken);
    } else {
      await getApi().setup({ password, device_name: deviceName }, setupToken);
    }
    this.status = 'authed';
  }

  async login(password: string, deviceName: string): Promise<void> {
    if (isTauri()) {
      await nativeLogin({ password, device_name: deviceName });
    } else {
      await getApi().login({ password, device_name: deviceName });
    }
    this.status = 'authed';
  }

  /** Server 側 token を失効してログイン画面へ。 */
  async logout(): Promise<void> {
    try {
      await getApi().logout();
    } finally {
      try {
        if (isTauri()) await clearNativeAuth();
      } finally {
        revokeAllObjectUrls();
        this.status = 'needs-login';
      }
    }
  }
}

export const session = new Session();
