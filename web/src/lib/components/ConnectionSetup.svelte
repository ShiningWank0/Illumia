<script lang="ts">
  // アプリモードのサーバー接続設定 (docs/08)。external / local の複数 URL を登録し、
  // external → local の順で到達性と server identity を確認して自動選択する。
  // 平文 HTTP の local は自動選択せず、毎回この画面で明示確認を取る (docs/12: SEC-002)。
  import { appMode } from '$lib/appMode.svelte';
  import { loadProfile } from '$lib/platform/connection';
  import { parseServerUrl, ServerUrlError } from '$lib/platform/serverUrl';
  import { confirmInsecureLocal } from '$lib/platform/insecurePrompt';

  interface Props {
    onConnected: () => void;
  }
  const { onConnected }: Props = $props();

  const existing = loadProfile();
  let external = $state(existing?.external ?? '');
  let local = $state(existing?.local ?? '');
  let ssid = $state(existing?.ssid ?? '');
  let connecting = $state(false);
  let validationError = $state<string | null>(null);

  /** 入力中の local が平文 HTTP かどうか (警告表示用)。 */
  const localIsInsecure = $derived.by(() => {
    if (local.trim() === '') return false;
    try {
      return parseServerUrl(local, { allowInsecurePrivate: true }).insecure;
    } catch {
      return false;
    }
  });

  async function connect(e: SubmitEvent) {
    e.preventDefault();
    validationError = null;
    if (external.trim() === '') return;

    // 送信前にクライアント側でも検証し、理由を具体的に表示する。
    try {
      parseServerUrl(external, { label: '外部 URL' });
      if (local.trim() !== '') {
        parseServerUrl(local, { label: 'ローカル URL', allowInsecurePrivate: true });
      }
    } catch (err) {
      validationError = err instanceof ServerUrlError ? err.message : '入力が不正です';
      return;
    }

    connecting = true;
    const ok = await appMode.connect(
      {
        external: external.trim(),
        local: local.trim() || undefined,
        ssid: ssid.trim() || undefined,
        instanceId: existing?.instanceId
      },
      confirmInsecureLocal
    );
    connecting = false;
    if (ok) onConnected();
  }
</script>

<div class="wrap">
  <form class="card" onsubmit={connect}>
    <h1>サーバーに接続</h1>
    <p class="muted">
      Illumia サーバーの URL を登録します。ネットワークに応じて自動で切り替えます。
    </p>

    <label>
      外部 URL (external)
      <input type="url" placeholder="https://illumia.example.com" bind:value={external} required />
      <span class="hint">https のみ使用できます。</span>
    </label>
    <label>
      ローカル URL (local, 任意)
      <input type="url" placeholder="https://192.168.1.10:2283" bind:value={local} />
      {#if localIsInsecure}
        <span class="warn">
          暗号化されていない HTTP です。自動では選択せず、接続のたびに確認します。可能なら https
          を設定してください。
        </span>
      {/if}
    </label>
    <label>
      ローカル用 Wi-Fi SSID (任意)
      <input type="text" placeholder="MyHomeWiFi" bind:value={ssid} />
      <span class="hint">
        現状 SSID の自動取得プラグインが無いため判定には使いません。外部 URL を先に試し、
        サーバー識別子が初回接続時と一致した場合のみ接続します。
      </span>
    </label>

    {#if validationError}<p class="err">{validationError}</p>{/if}
    {#if appMode.error}<p class="err">{appMode.error}</p>{/if}

    <button class="primary" type="submit" disabled={connecting || external.trim() === ''}>
      {connecting ? '接続中…' : '接続'}
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
    font-size: 1.4rem;
  }
  .muted {
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
  .hint {
    color: #71717a;
    font-size: 0.75rem;
  }
  .warn {
    color: #fbbf24;
    font-size: 0.75rem;
  }
  input {
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
  .err {
    margin: 0;
    color: #f87171;
    font-size: 0.85rem;
  }
</style>
