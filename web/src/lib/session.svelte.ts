// 認証セッションのグローバル状態。ルートレイアウトの認可ゲートで使う。
// GET /api/server/info の setup_completed とトークン有無で状態を決める。

import { getApi, getToken, setToken } from '$lib/api';
import { ApiError } from '$lib/api/types';

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

  /** 起動時: サーバー状態とトークン有無から初期状態を決める。 */
  async init(): Promise<void> {
    this.status = 'loading';
    this.error = null;
    try {
      const info = await getApi().serverInfo();
      this.version = info.version;
      if (!info.setup_completed) {
        this.status = 'needs-setup';
      } else {
        this.status = getToken() ? 'authed' : 'needs-login';
      }
    } catch (e) {
      this.status = 'error';
      this.error = messageOf(e);
    }
  }

  async setup(password: string, deviceName: string): Promise<void> {
    const { token } = await getApi().setup({ password, device_name: deviceName });
    setToken(token);
    this.status = 'authed';
  }

  async login(password: string, deviceName: string): Promise<void> {
    const { token } = await getApi().login({ password, device_name: deviceName });
    setToken(token);
    this.status = 'authed';
  }

  /** トークンを破棄してログイン画面へ (401 検出時にも使う)。 */
  logout(): void {
    setToken(null);
    this.status = 'needs-login';
  }
}

export const session = new Session();
