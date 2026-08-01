<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { session } from '$lib/session.svelte';
  import { vaultSession } from '$lib/vaultSession.svelte';

  const links = [
    { href: '/', label: 'タイムライン' },
    { href: '/stacks', label: '漫画' },
    { href: '/people', label: '人物' },
    { href: '/trash', label: 'ゴミ箱' },
    { href: '/duplicates', label: '重複' },
    { href: '/settings', label: '設定' }
  ];

  let query = $state('');

  const vaultIcon = $derived(vaultSession.status === 'unlocked' ? '🔓' : '🔒');

  function isActive(href: string, pathname: string): boolean {
    return href === '/' ? pathname === '/' : pathname.startsWith(href);
  }

  function onSearch(e: SubmitEvent) {
    e.preventDefault();
    const q = query.trim();
    if (q) goto(`/search?q=${encodeURIComponent(q)}`);
  }
</script>

<nav>
  <span class="brand">Illumia</span>
  <ul>
    {#each links as link (link.href)}
      <li>
        <a href={link.href} class:active={isActive(link.href, $page.url.pathname)}>
          {link.label}
        </a>
      </li>
    {/each}
  </ul>
  <a
    class="vault-link"
    class:unlocked={vaultSession.status === 'unlocked'}
    class:active={$page.url.pathname.startsWith('/vault')}
    href="/vault"
    title={vaultSession.status === 'unlocked' ? 'Vault (アンロック中)' : 'Vault (ロック中)'}
  >
    {vaultIcon} Vault
  </a>
  <form class="search" onsubmit={onSearch}>
    <input type="search" placeholder="検索…" bind:value={query} aria-label="検索" />
  </form>
  <button class="logout" onclick={() => void session.logout()}>ログアウト</button>
</nav>

<style>
  nav {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem 1rem;
    background: #0c0c10;
    border-bottom: 1px solid #26262e;
    flex-shrink: 0;
  }
  .brand {
    font-weight: 800;
    letter-spacing: 0.02em;
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
    padding: 0.4rem 0.8rem;
    border-radius: 7px;
    color: #d4d4d8;
    text-decoration: none;
    font-size: 0.9rem;
  }
  a.active {
    background: #6d5bd0;
    color: #fff;
  }
  .vault-link {
    display: block;
    padding: 0.4rem 0.8rem;
    border-radius: 7px;
    color: #d4d4d8;
    text-decoration: none;
    font-size: 0.9rem;
    border: 1px solid #3f3f46;
  }
  .vault-link.unlocked {
    color: #86efac;
    border-color: #2f5d43;
  }
  .vault-link.active {
    background: #6d5bd0;
    color: #fff;
    border-color: #6d5bd0;
  }
  .search input {
    padding: 0.4rem 0.7rem;
    border-radius: 7px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 0.85rem;
    width: 12rem;
    max-width: 30vw;
  }
  .logout {
    border: 1px solid #3f3f46;
    background: none;
    color: #a1a1aa;
    padding: 0.4rem 0.8rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.85rem;
  }
</style>
