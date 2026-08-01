// アプリモード (Tauri) の接続状態。ブラウザでは常に ready。
// ネイティブでは接続プロファイルをプローブして baseUrl を確定してから
// 認証フロー (session) に入る。

import { isTauri } from '$lib/platform/tauri';
import {
  loadProfile,
  probeAndSelect,
  saveProfile,
  type ConnectionProfile
} from '$lib/platform/connection';

export type AppModeStatus = 'loading' | 'needs-connection' | 'ready';

class AppMode {
  native = $state(false);
  status = $state<AppModeStatus>('loading');
  error = $state<string | null>(null);

  /** 起動時: ブラウザは即 ready、ネイティブはプロファイルをプローブ。 */
  async init(): Promise<void> {
    this.native = isTauri();
    if (!this.native) {
      this.status = 'ready';
      return;
    }
    const profile = loadProfile();
    if (!profile) {
      this.status = 'needs-connection';
      return;
    }
    await this.connect(profile);
  }

  /** プロファイルを保存し、到達性プローブで baseUrl を確定する。 */
  async connect(profile: ConnectionProfile): Promise<boolean> {
    this.status = 'loading';
    this.error = null;
    saveProfile(profile);
    const base = await probeAndSelect(profile);
    if (base) {
      this.status = 'ready';
      return true;
    }
    this.status = 'needs-connection';
    this.error = 'サーバーに到達できませんでした。URL とネットワークを確認してください。';
    return false;
  }
}

export const appMode = new AppMode();
