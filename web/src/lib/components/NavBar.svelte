<script lang="ts">
  import { page } from '$app/stores';
  import { session } from '$lib/session.svelte';

  const links = [
    { href: '/', label: 'タイムライン' },
    { href: '/trash', label: 'ゴミ箱' },
    { href: '/duplicates', label: '重複' },
    { href: '/settings', label: '設定' }
  ];

  function isActive(href: string, pathname: string): boolean {
    return href === '/' ? pathname === '/' : pathname.startsWith(href);
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
  <button class="logout" onclick={() => session.logout()}>ログアウト</button>
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
