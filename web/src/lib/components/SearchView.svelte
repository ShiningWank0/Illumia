<script lang="ts">
  import { getApi, type IllumiaApi, type SearchResult } from '$lib/api';
  import AssetImage from './AssetImage.svelte';
  import FaceCrop from './FaceCrop.svelte';

  interface Props {
    query: string;
    api?: IllumiaApi;
    /** スタックリンクのベース (/stacks or /vault/stacks)。 */
    basePath?: string;
    /** クラスタリンクのベース (/people or /vault/people)。 */
    peopleBase?: string;
  }
  const { query, api = getApi(), basePath = '/stacks', peopleBase = '/people' }: Props = $props();

  let result = $state<SearchResult>({ assets: [], stacks: [], clusters: [] });
  let loading = $state(false);
  let error = $state<string | null>(null);

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
            <a class="stack-card" href={`${basePath}/${s.id}`}>
              <div class="cover">
                {#if s.cover_asset_id}<AssetImage {api} id={s.cover_asset_id} />{/if}
              </div>
              <span class="name">{s.title}</span>
              <span class="muted small">{s.chapter_count} 話 / {s.page_count} ページ</span>
            </a>
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <h2>人物 ({result.clusters.length})</h2>
      {#if result.clusters.length === 0}
        <p class="muted small">該当なし</p>
      {:else}
        <div class="people-grid">
          {#each result.clusters as c (c.id)}
            <a class="person" href={`${peopleBase}/${c.id}`}>
              <div class="face">
                {#if c.cover}
                  <FaceCrop
                    {api}
                    assetId={c.cover.asset_id}
                    bbox={c.cover.bbox}
                    alt={c.name ?? '未命名'}
                  />
                {/if}
              </div>
              <span class="name" class:unnamed={!c.name}>{c.name ?? '未命名'}</span>
              <span class="muted small">{c.count} 枚</span>
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
            <div class="asset"><AssetImage {api} id={a.id} thumbhash={a.thumbhash} /></div>
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
  .people-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 1rem;
  }
  .person {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    text-decoration: none;
    color: inherit;
  }
  .person .face {
    aspect-ratio: 1;
    border-radius: 8px;
    overflow: hidden;
    background: #1c1c22;
  }
  .person .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .person .name.unnamed {
    color: #71717a;
    font-style: italic;
  }
</style>
