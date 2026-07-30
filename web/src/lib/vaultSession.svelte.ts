// Vault セッションのグローバル状態。
// vault_session トークンは **メモリのみ** に保持する (localStorage 禁止)。
// タブを閉じる / リロードするとトークンは失われ、ロック扱いになる (docs/06)。

import { revokeVaultObjectUrls } from '$lib/api/image';

export type VaultStatus = 'loading' | 'uninitialized' | 'locked' | 'unlocked' | 'error';

class VaultSession {
  status = $state<VaultStatus>('loading');
  /** メモリ内のみのセッショントークン。永続化しない。 */
  token = $state<string | null>(null);
  expiresAt = $state<string | null>(null);
  error = $state<string | null>(null);

  private timer: ReturnType<typeof setTimeout> | undefined;

  /** サーバー状態と手元トークンから status を確定する。 */
  setInitialized(initialized: boolean): void {
    if (!initialized) {
      this.status = 'uninitialized';
    } else {
      this.status = this.token ? 'unlocked' : 'locked';
    }
  }

  /** アンロック成功。トークンをメモリに保持し、失効タイマーを張る。 */
  setUnlocked(token: string, expiresAt: string): void {
    this.token = token;
    this.expiresAt = expiresAt;
    this.status = 'unlocked';
    this.error = null;
    this.armTimer();
  }

  /** ローカルでロック状態へ (lock ボタン / 失効 / API 404 検出時)。 */
  lockLocal(): void {
    this.token = null;
    this.expiresAt = null;
    this.status = 'locked';
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    // vault 画像の object URL をメモリから消す。
    revokeVaultObjectUrls();
  }

  /** vault API から 404 を受けたら (ロック扱い) 呼ぶ。 */
  onVaultNotFound(): void {
    if (this.status === 'unlocked') this.lockLocal();
  }

  private armTimer(): void {
    if (this.timer) clearTimeout(this.timer);
    if (!this.expiresAt) return;
    const ms = new Date(this.expiresAt).getTime() - Date.now();
    if (!Number.isFinite(ms)) return;
    this.timer = setTimeout(() => this.lockLocal(), Math.max(0, ms));
  }
}

export const vaultSession = new VaultSession();

/** 画像 fetch 等から現在の vault トークンを取得する (非リアクティブ)。 */
export function getVaultToken(): string | null {
  return vaultSession.token;
}
