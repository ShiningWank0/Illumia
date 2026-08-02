<script lang="ts">
  import { onMount } from 'svelte';
  import { session } from '$lib/session.svelte';
  import { appMode } from '$lib/appMode.svelte';
  import { confirmInsecureLocal } from '$lib/platform/insecurePrompt';
  import AuthGate from '$lib/components/AuthGate.svelte';
  import ConnectionSetup from '$lib/components/ConnectionSetup.svelte';
  import NavBar from '$lib/components/NavBar.svelte';
  import Toaster from '$lib/components/Toaster.svelte';

  const { children } = $props();

  onMount(async () => {
    // アプリモード (Tauri) は接続プロファイルのプローブを先に済ませる。
    // 平文 HTTP の local は自動選択せず、毎回明示確認を取る (docs/12: SEC-002)。
    await appMode.init(confirmInsecureLocal);
    if (appMode.status === 'ready') session.init();
  });

  function onConnected() {
    session.init();
  }
</script>

{#if appMode.status === 'loading'}
  <div class="center">読み込み中…</div>
{:else if appMode.status === 'needs-connection'}
  <ConnectionSetup {onConnected} />
{:else if session.status === 'loading'}
  <div class="center">読み込み中…</div>
{:else if session.status === 'error'}
  <div class="center">
    <p>サーバーに接続できません: {session.error}</p>
    <button onclick={() => session.init()}>再試行</button>
  </div>
{:else if session.status === 'needs-setup' || session.status === 'needs-login'}
  <AuthGate />
{:else}
  <div class="shell">
    <NavBar />
    <main>{@render children()}</main>
  </div>
{/if}

<Toaster />

<style>
  :global(*) {
    box-sizing: border-box;
  }
  :global(html) {
    font-family:
      Inter,
      ui-sans-serif,
      system-ui,
      -apple-system,
      BlinkMacSystemFont,
      'Segoe UI',
      sans-serif;
    color-scheme: dark;
    background: #101116;
    color: #f4f4f5;
  }
  :global(body) {
    margin: 0;
  }
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  main {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .center {
    min-height: 100vh;
    display: grid;
    place-items: center;
    gap: 1rem;
    text-align: center;
    color: #a1a1aa;
  }
  .center button {
    padding: 0.5rem 1rem;
    border-radius: 8px;
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    cursor: pointer;
  }
</style>
