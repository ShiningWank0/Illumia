// アプリモード (Tauri) の接続状態。ブラウザでは常に ready。
// ネイティブでは接続プロファイルをプローブして baseUrl を確定してから
// 認証フロー (session) に入る。
//
// SEC-002 対策 (docs/12_security.md):
//  - 選択順は external → local。local が平文 HTTP の場合は自動選択せず、
//    `confirmInsecureLocal` で毎回明示確認を取る。
//  - 初回接続で server の instance_id を pin し、以後一致しないサーバーは採用しない。
//    不一致は「偽サーバーの疑い」として credential を送らずに接続を中止する。

import { isTauri } from '$lib/platform/tauri';
import {
  isInsecureLocal,
  loadProfile,
  probeAndSelect,
  saveProfile,
  type ConnectionProfile
} from '$lib/platform/connection';
import { ServerUrlError } from '$lib/platform/serverUrl';

export type AppModeStatus = 'loading' | 'needs-connection' | 'ready';

class AppMode {
  native = $state(false);
  status = $state<AppModeStatus>('loading');
  error = $state<string | null>(null);
  /** 平文 HTTP の local を使ってよいか確認を待っている状態。 */
  pendingInsecureConfirm = $state(false);

  /**
   * 起動時: ブラウザは即 ready、ネイティブはプロファイルをプローブ。
   * `confirm` を渡さない場合、平文 HTTP の local は一切試さない。
   */
  async init(confirm?: () => Promise<boolean>): Promise<void> {
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
    await this.connect(profile, confirm);
  }

  /**
   * プロファイルを検証・保存し、到達性と identity の確認で baseUrl を確定する。
   * 平文 HTTP の local を試す前に `confirm` で明示確認を取る。
   */
  async connect(profile: ConnectionProfile, confirm?: () => Promise<boolean>): Promise<boolean> {
    this.status = 'loading';
    this.error = null;

    let saved: ConnectionProfile;
    try {
      saved = saveProfile(profile);
    } catch (e) {
      this.status = 'needs-connection';
      this.error = e instanceof ServerUrlError ? e.message : '接続設定が不正です。';
      return false;
    }

    const confirmInsecureLocal = async () => {
      if (!isInsecureLocal(saved)) return false;
      this.pendingInsecureConfirm = true;
      try {
        return confirm ? await confirm() : false;
      } finally {
        this.pendingInsecureConfirm = false;
      }
    };

    const result = await probeAndSelect(saved, { confirmInsecureLocal });

    if (result.baseUrl) {
      // 初回接続なら instance_id を pin して以後の接続先を固定する。
      if (result.pinned) saveProfile({ ...saved, instanceId: result.pinned });
      this.status = 'ready';
      return true;
    }

    this.status = 'needs-connection';
    this.error = result.identityMismatch
      ? '登録済みのサーバーと異なるサーバーが応答しました。偽サーバーの可能性があるため接続を中止しました。ネットワークを確認してください。'
      : 'サーバーに到達できませんでした。URL とネットワークを確認してください。';
    return false;
  }
}

export const appMode = new AppMode();
