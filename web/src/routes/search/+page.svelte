<script lang="ts">
  import { page } from '$app/stores';
  import { getApi, type SearchResult } from '$lib/api';
  import AssetImage from '$lib/components/AssetImage.svelte';

  const api = getApi();
  const query = $derived($page.url.searchParams.get('q') ?? '');

  let result = $state<SearchResult>({ assets: [], stacks: [], clusters: [] });
  let loading = $state(false);
  let error = $state<string | null>(null);

  // クエリが変わるたびに検索する。
  $effect(() => {
    const q = query;
    if (q.trim() === '') {
      result = { assets: [], stacks: [], clusters: [] };
      return;
    }
    let alive = true;
    loading = true;
    error = null;
    api
      .search(q)
      .then((r) => {
        if (alive) result = r;
      })
      .catch((e) => {
        if (alive) error = e instanceof Error ? e.message : '検索に失敗しました';
      })
      .finally(() => {
        if (alive) loading = false;
      });
    return () => {
      alive = false;
    };
  });
</script>

<svelte:head><title>検索: {query} - Illumia</title></svelte:head>

<div class="page">
  <h1>検索: {query}</h1>
  {#if loading}
    <p class="muted">検索中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else}
    <section>
      <h2>漫画スタック ({result.stacks.length})</h2>
      {#if result.stacks.length === 0}
        <p class="muted small">該当なし</p>
      {:else}
        <div class="stack-grid">
          {#each result.stacks as s (s.id)}
            <a class="stack-card" href={`/stacks/${s.id}`}>
              <div class="cover">
                {#if s.cover_asset_id}<AssetImage id={s.cover_asset_id} />{/if}
              </div>
              <span class="name">{s.title}</span>
              <span class="muted small">{s.chapter_count} 話 / {s.page_count} ページ</span>
            </a>
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <h2>画像 ({result.assets.length})</h2>
      {#if result.assets.length === 0}
        <p class="muted small">該当なし</p>
      {:else}
        <div class="asset-grid">
          {#each result.assets as a (a.id)}
            <div class="asset"><AssetImage id={a.id} thumbhash={a.thumbhash} /></div>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    padding: 1.5rem;
  }
  h1 {
    margin: 0 0 1rem;
    font-size: 1.3rem;
  }
  h2 {
    font-size: 1rem;
    margin: 1.5rem 0 0.75rem;
  }
  .muted {
    color: #a1a1aa;
  }
  .small {
    font-size: 0.85rem;
  }
  .err {
    color: #f87171;
  }
  .stack-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 1rem;
  }
  .stack-card {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 10px;
    padding: 0.5rem;
    text-decoration: none;
    color: inherit;
  }
  .cover {
    aspect-ratio: 3 / 4;
    border-radius: 6px;
    overflow: hidden;
    background: #1c1c22;
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .asset-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 0.5rem;
  }
  .asset {
    aspect-ratio: 1;
    border-radius: 6px;
    overflow: hidden;
    background: #1c1c22;
  }
</style>
