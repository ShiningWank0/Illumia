<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { vaultSession } from '$lib/vaultSession.svelte';
  import { getVaultLifecycle } from '$lib/api/vault';
  import { revokeVaultObjectUrls } from '$lib/api/image';
  import VaultGate from '$lib/components/VaultGate.svelte';

  const { children } = $props();
  const lifecycle = getVaultLifecycle();

  let query = $state('');

  const links = [
    { href: '/vault', label: 'タイムライン', exact: true },
    { href: '/vault/stacks', label: '漫画', exact: false },
    { href: '/vault/trash', label: 'ゴミ箱', exact: false },
    { href: '/vault/duplicates', label: '重複', exact: false }
  ];
  function isActive(href: string, exact: boolean, pathname: string): boolean {
    return exact ? pathname === href : pathname.startsWith(href);
  }

  async function refresh() {
    try {
      const s = await lifecycle.status();
      vaultSession.setInitialized(s.initialized);
    } catch {
      vaultSession.status = 'error';
    }
  }

  async function lock() {
    try {
      await lifecycle.lock();
    } catch {
      // 失敗しても手元はロック扱いにする。
    }
    vaultSession.lockLocal();
  }

  function onSearch(e: SubmitEvent) {
    e.preventDefault();
    const q = query.trim();
    if (q) goto(`/vault/search?q=${encodeURIComponent(q)}`);
  }

  onMount(refresh);
  // vault 画面を離れたら vault 画像 object URL をメモリから消す。
  onDestroy(() => revokeVaultObjectUrls());
</script>

{#if vaultSession.status === 'loading'}
  <div class="center">Vault の状態を確認中…</div>
{:else if vaultSession.status === 'error'}
  <div class="center">
    <p>Vault の状態を取得できません: {vaultSession.error}</p>
    <button onclick={refresh}>再試行</button>
  </div>
{:else if vaultSession.status !== 'unlocked'}
  <VaultGate />
{:else}
  <div class="vault-shell">
    <div class="subnav">
      <span class="badge">🔓 Vault</span>
      <ul>
        {#each links as link (link.href)}
          <li>
            <a href={link.href} class:active={isActive(link.href, link.exact, $page.url.pathname)}>
              {link.label}
            </a>
          </li>
        {/each}
      </ul>
      <form class="search" onsubmit={onSearch}>
        <input
          type="search"
          placeholder="Vault 内検索…"
          bind:value={query}
          aria-label="Vault 内検索"
        />
      </form>
      <button class="lock" onclick={lock}>ロック</button>
    </div>
    <div class="vault-content">
      {@render children()}
    </div>
  </div>
{/if}

<style>
  .center {
    height: 100%;
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
  .vault-shell {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .subnav {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.4rem 0.9rem;
    background: #120f1a;
    border-bottom: 1px solid #2a2440;
    flex-shrink: 0;
  }
  .badge {
    font-weight: 700;
    color: #c4b5fd;
  }
  ul {
    display: flex;
    gap: 0.25rem;
    list-style: none;
    margin: 0;
    padding: 0;
    flex: 1;
  }
  a {
    display: block;
    padding: 0.35rem 0.75rem;
    border-radius: 7px;
    color: #d4d4d8;
    text-decoration: none;
    font-size: 0.85rem;
  }
  a.active {
    background: #6d5bd0;
    color: #fff;
  }
  .search input {
    padding: 0.35rem 0.6rem;
    border-radius: 7px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 0.85rem;
    width: 11rem;
    max-width: 28vw;
  }
  .lock {
    border: 1px solid #7f1d1d;
    background: none;
    color: #fca5a5;
    padding: 0.35rem 0.8rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .vault-content {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
</style>
