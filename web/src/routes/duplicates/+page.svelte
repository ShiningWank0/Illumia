<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type DuplicatePair } from '$lib/api';
  import AssetImage from '$lib/components/AssetImage.svelte';

  const api = getApi();

  let pairs = $state<DuplicatePair[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load() {
    loading = true;
    error = null;
    try {
      pairs = await api.getDuplicates();
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  function fmt(iso?: string): string {
    if (!iso) return '-';
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? iso : d.toLocaleString('ja-JP');
  }

  onMount(load);
</script>

<svelte:head><title>重複 - Illumia</title></svelte:head>

<div class="page">
  <h1>重複</h1>
  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else if pairs.length === 0}
    <p class="muted">重複はありません。</p>
  {:else}
    <ul class="list">
      {#each pairs as p (p.dup.id)}
        <li>
          <div class="side">
            <span class="tag">重複</span>
            <div class="thumb"><AssetImage id={p.dup.id} thumbhash={p.dup.thumbhash} /></div>
            <span class="name">{p.dup.filename}</span>
            <a href={api.originalUrl(p.dup.id)} download>ダウンロード</a>
          </div>
          <div class="arrow">↔</div>
          <div class="side">
            <span class="tag original">オリジナル</span>
            <div class="thumb">
              <AssetImage id={p.original.id} thumbhash={p.original.thumbhash} />
            </div>
            <span class="name">{p.original.filename}</span>
            <a href={api.originalUrl(p.original.id)} download>ダウンロード</a>
          </div>
          <div class="purge muted small">完全削除予定: {fmt(p.purge_after)}</div>
        </li>
      {/each}
    </ul>
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
    font-size: 1.4rem;
  }
  .muted {
    color: #a1a1aa;
  }
  .small {
    font-size: 0.8rem;
  }
  .err {
    color: #f87171;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  li {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    gap: 1rem;
    padding: 0.75rem;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 10px;
  }
  .side {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
    min-width: 0;
  }
  .thumb {
    width: 120px;
    height: 120px;
    border-radius: 8px;
    overflow: hidden;
    background: #1c1c22;
  }
  .tag {
    font-size: 0.75rem;
    color: #fca5a5;
  }
  .tag.original {
    color: #86efac;
  }
  .name {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.85rem;
  }
  a {
    color: #c4b5fd;
    font-size: 0.85rem;
  }
  .arrow {
    color: #71717a;
    font-size: 1.5rem;
  }
  .purge {
    grid-column: 1 / -1;
    text-align: center;
  }
</style>
