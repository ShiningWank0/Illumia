<script lang="ts">
  // Vault の初期化 / アンロック UI。
  //  - 未初期化 → パスワード設定 → recovery_key を 1 度だけ表示 → 「保存した」で先へ
  //  - ロック中 → パスワード or リカバリーキーでアンロック
  import { onMount } from 'svelte';
  import { vaultSession } from '$lib/vaultSession.svelte';
  import { getVaultLifecycle } from '$lib/api/vault';
  import {
    isTauri,
    biometricStatus,
    biometricAuthenticate,
    secureGet,
    secureSet
  } from '$lib/platform/tauri';

  const lifecycle = getVaultLifecycle();
  // 生体認証成功時に取り出す vault パスワードの secure storage キー (現状メモリ内)。
  const VAULT_PW_KEY = 'illumia.vault_password';

  // 初期化フォーム
  let initPassword = $state('');
  let initConfirm = $state('');
  let recoveryKey = $state<string | null>(null);
  let savedChecked = $state(false);

  // アンロックフォーム
  let useRecovery = $state(false);
  let unlockPassword = $state('');
  let unlockRecovery = $state('');
  let enableBiometric = $state(false);

  let biometricAvailable = $state(false);
  let hasStoredCredential = $state(false);
  let submitting = $state(false);
  let formError = $state<string | null>(null);

  onMount(async () => {
    if (!isTauri()) return;
    biometricAvailable = (await biometricStatus()).available;
    hasStoredCredential = secureGet(VAULT_PW_KEY) !== null;
  });

  async function biometricUnlock() {
    formError = null;
    submitting = true;
    try {
      if (!(await biometricAuthenticate('Vault をアンロック'))) {
        formError = '生体認証に失敗しました';
        return;
      }
      const pw = secureGet(VAULT_PW_KEY);
      if (!pw) {
        formError = '保存された認証情報がありません。パスワードで開いてください。';
        return;
      }
      const res = await lifecycle.unlock({ password: pw });
      vaultSession.setUnlocked(res.vault_session, res.expires_at);
    } catch (err) {
      formError = err instanceof Error ? err.message : 'アンロックに失敗しました';
    } finally {
      submitting = false;
    }
  }

  async function doInit(e: SubmitEvent) {
    e.preventDefault();
    formError = null;
    if (initPassword.length < 8) {
      formError = 'パスワードは 8 文字以上にしてください';
      return;
    }
    if (initPassword !== initConfirm) {
      formError = 'パスワードが一致しません';
      return;
    }
    submitting = true;
    try {
      const res = await lifecycle.init(initPassword);
      recoveryKey = res.recovery_key;
      initPassword = '';
      initConfirm = '';
    } catch (err) {
      formError = err instanceof Error ? err.message : '初期化に失敗しました';
    } finally {
      submitting = false;
    }
  }

  function finishInit() {
    recoveryKey = null;
    savedChecked = false;
    // 初期化済み・未アンロック → ロック画面へ。
    vaultSession.setInitialized(true);
  }

  async function doUnlock(e: SubmitEvent) {
    e.preventDefault();
    formError = null;
    submitting = true;
    try {
      const payload = useRecovery
        ? { recovery_key: unlockRecovery.trim() }
        : { password: unlockPassword };
      const res = await lifecycle.unlock(payload);
      // 生体認証を有効化する場合、パスワードを secure storage に保存する。
      if (enableBiometric && !useRecovery && isTauri() && unlockPassword) {
        secureSet(VAULT_PW_KEY, unlockPassword);
        hasStoredCredential = true;
      }
      vaultSession.setUnlocked(res.vault_session, res.expires_at);
      unlockPassword = '';
      unlockRecovery = '';
    } catch (err) {
      formError = err instanceof Error ? err.message : 'アンロックに失敗しました';
    } finally {
      submitting = false;
    }
  }
</script>

<div class="wrap">
  {#if recoveryKey}
    <!-- リカバリーキーを 1 度だけ表示 -->
    <div class="card">
      <h1>🔑 リカバリーキー</h1>
      <p class="warn">
        これは <strong>一度だけ</strong> 表示されます。パスワードを忘れると、このキーが vault を開く唯一の手段です。安全な場所に保管してください。
      </p>
      <code class="recovery">{recoveryKey}</code>
      <label class="check">
        <input type="checkbox" bind:checked={savedChecked} />
        リカバリーキーを安全に保存しました
      </label>
      <button class="primary" disabled={!savedChecked} onclick={finishInit}>続ける</button>
    </div>
  {:else if vaultSession.status === 'uninitialized'}
    <!-- 初期化 -->
    <form class="card" onsubmit={doInit}>
      <h1>🔒 Vault を作成</h1>
      <p class="muted">
        非表示フォルダを作成します。中身はタイムライン・検索から完全に消え、閲覧のたびに
        パスワードが必要になります。
      </p>
      <label>
        パスワード
        <input type="password" bind:value={initPassword} autocomplete="new-password" required />
      </label>
      <label>
        パスワード (確認)
        <input type="password" bind:value={initConfirm} autocomplete="new-password" required />
      </label>
      {#if formError}<p class="err">{formError}</p>{/if}
      <button class="primary" type="submit" disabled={submitting}>
        {submitting ? '作成中…' : 'Vault を作成'}
      </button>
    </form>
  {:else}
    <!-- アンロック -->
    <form class="card" onsubmit={doUnlock}>
      <h1>🔒 Vault をアンロック</h1>
      {#if useRecovery}
        <label>
          リカバリーキー
          <input type="text" bind:value={unlockRecovery} required />
        </label>
      {:else}
        <label>
          パスワード
          <input
            type="password"
            bind:value={unlockPassword}
            autocomplete="current-password"
            required
          />
        </label>
        {#if biometricAvailable}
          <label class="check">
            <input type="checkbox" bind:checked={enableBiometric} />
            この端末で次回から生体認証を使う
          </label>
        {/if}
      {/if}
      {#if formError}<p class="err">{formError}</p>{/if}
      <button class="primary" type="submit" disabled={submitting}>
        {submitting ? '確認中…' : 'アンロック'}
      </button>
      {#if biometricAvailable && hasStoredCredential}
        <button type="button" class="link" disabled={submitting} onclick={biometricUnlock}>
          🔓 生体認証でアンロック
        </button>
      {/if}
      <button type="button" class="link" onclick={() => (useRecovery = !useRecovery)}>
        {useRecovery ? 'パスワードで開く' : 'リカバリーキーで開く'}
      </button>
    </form>
  {/if}
</div>

<style>
  .wrap {
    min-height: 100%;
    display: grid;
    place-items: center;
    padding: 2rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    width: min(92vw, 420px);
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 2rem;
  }
  h1 {
    margin: 0;
    font-size: 1.5rem;
  }
  .muted {
    margin: 0;
    color: #a1a1aa;
    font-size: 0.9rem;
  }
  .warn {
    margin: 0;
    color: #fcd34d;
    font-size: 0.9rem;
    line-height: 1.6;
  }
  .recovery {
    display: block;
    background: #0c0c10;
    border: 1px solid #3f3f46;
    border-radius: 8px;
    padding: 1rem;
    font-size: 1.1rem;
    letter-spacing: 0.05em;
    word-break: break-all;
    color: #86efac;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.9rem;
    color: #d4d4d8;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: 0.85rem;
    color: #d4d4d8;
  }
  input[type='password'],
  input[type='text'] {
    padding: 0.6rem 0.7rem;
    border-radius: 8px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 1rem;
  }
  .primary {
    padding: 0.7rem;
    border: none;
    border-radius: 8px;
    background: #6d5bd0;
    color: #fff;
    font-size: 1rem;
    cursor: pointer;
  }
  .primary:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .link {
    background: none;
    border: none;
    color: #c4b5fd;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .err {
    margin: 0;
    color: #f87171;
    font-size: 0.85rem;
  }
</style>
