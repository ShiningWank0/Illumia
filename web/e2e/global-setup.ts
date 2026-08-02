// E2E 開始前に初回セットアップを済ませ、Cookie セッションを storageState として保存する。
// Web は Cookie 認証 (device token を body で受け取らない) なので、
// `X-Illumia-Auth-Mode: cookie` と同一 authority の Origin を付ける (→ docs/12_security.md)。
import { request, type FullConfig } from '@playwright/test';

/** 検証専用の使い捨てパスワード。毎回新しい一時データディレクトリに対して設定する。 */
const E2E_PASSWORD = 'illumia-e2e-setup-passphrase';

export default async function globalSetup(config: FullConfig) {
  const baseURL = config.projects[0].use.baseURL as string;
  const context = await request.newContext({ baseURL });

  const info = await (await context.get('/api/server/info')).json();
  if (!info.setup_completed) {
    const response = await context.post('/api/auth/setup', {
      headers: { 'x-illumia-auth-mode': 'cookie', origin: baseURL },
      data: { password: E2E_PASSWORD, device_name: 'e2e' }
    });
    if (!response.ok()) {
      throw new Error(`初回セットアップに失敗した: ${response.status()}`);
    }
  } else {
    const response = await context.post('/api/auth/login', {
      headers: { 'x-illumia-auth-mode': 'cookie', origin: baseURL },
      data: { password: E2E_PASSWORD, device_name: 'e2e' }
    });
    if (!response.ok()) {
      throw new Error(`ログインに失敗した: ${response.status()}`);
    }
  }

  await context.storageState({ path: 'e2e/.auth/state.json' });
  await context.dispose();
}
