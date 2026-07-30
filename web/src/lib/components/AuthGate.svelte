<script lang="ts">
  // セットアップ / ログイン画面。session.status に応じて分岐する。
  import { session } from '$lib/session.svelte';

  let password = $state('');
  let deviceName = $state('');
  let setupToken = $state('');
  let submitting = $state(false);
  let formError = $state<string | null>(null);

  const mode = $derived(session.status === 'needs-setup' ? 'setup' : 'login');

  // デバイス名の既定値。
  $effect(() => {
    if (deviceName === '') {
      deviceName = typeof navigator !== 'undefined' ? navigator.platform || 'web' : 'web';
    }
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    formError = null;
    submitting = true;
    try {
      if (mode === 'setup') await session.setup(password, deviceName, setupToken || undefined);
      else await session.login(password, deviceName);
      password = '';
      setupToken = '';
    } catch (err) {
      formError = err instanceof Error ? err.message : 'failed';
    } finally {
      submitting = false;
    }
  }
</script>

<div class="wrap">
  <form onsubmit={submit}>
    <h1>Illumia</h1>
    <p class="sub">
      {mode === 'setup' ? '初回セットアップ: パスワードを設定します' : 'ログイン'}
    </p>

    <label>
      パスワード
      <input
        type="password"
        bind:value={password}
        required
        minlength={mode === 'setup' ? 12 : undefined}
        maxlength="1024"
        autocomplete={mode === 'setup' ? 'new-password' : 'current-password'}
      />
    </label>
    {#if mode === 'setup' && session.setupTokenRequired}
      <label>
        初期セットアップトークン
        <input
          type="password"
          bind:value={setupToken}
          required
          minlength="32"
          maxlength="256"
          autocomplete="off"
        />
      </label>
    {/if}
    <label>
      デバイス名
      <input type="text" bind:value={deviceName} required maxlength="128" />
    </label>

    {#if formError}<p class="err">{formError}</p>{/if}

    <button type="submit" disabled={submitting}>
      {submitting ? '処理中…' : mode === 'setup' ? 'セットアップ' : 'ログイン'}
    </button>
  </form>
</div>

<style>
  .wrap {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 2rem;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    width: min(90vw, 360px);
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 2rem;
  }
  h1 {
    margin: 0;
    font-size: 1.75rem;
  }
  .sub {
    margin: 0;
    color: #a1a1aa;
    font-size: 0.9rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: 0.85rem;
    color: #d4d4d8;
  }
  input {
    padding: 0.6rem 0.7rem;
    border-radius: 8px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 1rem;
  }
  button {
    margin-top: 0.5rem;
    padding: 0.7rem;
    border: none;
    border-radius: 8px;
    background: #6d5bd0;
    color: #fff;
    font-size: 1rem;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .err {
    margin: 0;
    color: #f87171;
    font-size: 0.85rem;
  }
</style>
