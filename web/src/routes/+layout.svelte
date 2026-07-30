<script lang="ts">
  import { onMount } from 'svelte';
  import { session } from '$lib/session.svelte';
  import AuthGate from '$lib/components/AuthGate.svelte';
  import NavBar from '$lib/components/NavBar.svelte';
  import Toaster from '$lib/components/Toaster.svelte';

  const { children } = $props();

  onMount(() => {
    session.init();
  });
</script>

{#if session.status === 'loading'}
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
