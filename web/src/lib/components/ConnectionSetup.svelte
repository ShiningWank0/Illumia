<script lang="ts">
  // アプリモードのサーバー接続設定 (docs/08)。external / local の複数 URL を登録し、
  // local → external の到達性プローブで自動選択する。
  import { appMode } from '$lib/appMode.svelte';
  import { loadProfile } from '$lib/platform/connection';

  interface Props {
    onConnected: () => void;
  }
  const { onConnected }: Props = $props();

  const existing = loadProfile();
  let external = $state(existing?.external ?? '');
  let local = $state(existing?.local ?? '');
  let ssid = $state(existing?.ssid ?? '');
  let connecting = $state(false);

  async function connect(e: SubmitEvent) {
    e.preventDefault();
    if (external.trim() === '') return;
    connecting = true;
    const ok = await appMode.connect({
      external: external.trim(),
      local: local.trim() || undefined,
      ssid: ssid.trim() || undefined
    });
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
    </label>
    <label>
      ローカル URL (local, 任意)
      <input type="url" placeholder="http://192.168.1.10:2283" bind:value={local} />
    </label>
    <label>
      ローカル用 Wi-Fi SSID (任意)
      <input type="text" placeholder="MyHomeWiFi" bind:value={ssid} />
      <span class="hint">
        現状 SSID の自動取得プラグインが無いため判定には使いません。到達性プローブ (local→external,
        各 2 秒) で自動選択します。
      </span>
    </label>

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
