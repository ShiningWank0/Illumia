<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type IllumiaApi, type StackSummary } from '$lib/api';
  import AssetImage from './AssetImage.svelte';

  interface Props {
    api?: IllumiaApi;
    basePath?: string;
  }
  const { api = getApi(), basePath = '/stacks' }: Props = $props();

  let stacks = $state<StackSummary[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load() {
    loading = true;
    error = null;
    try {
      stacks = await api.listStacks();
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<div class="page">
  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else if stacks.length === 0}
    <p class="muted">スタックはまだありません。画像を選択して作成できます。</p>
  {:else}
    <div class="grid">
      {#each stacks as s (s.id)}
        <a class="card" href={`${basePath}/${s.id}`}>
          <div class="cover">
            {#if s.cover_asset_id}
              <AssetImage {api} id={s.cover_asset_id} />
            {:else}
              <div class="nocover">表紙なし</div>
            {/if}
          </div>
          <div class="info">
            <span class="title">{s.title}</span>
            <span class="muted small">{s.chapter_count} 話 / {s.page_count} ページ</span>
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    padding: 1.5rem;
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
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 1rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 10px;
    overflow: hidden;
    text-decoration: none;
    color: inherit;
  }
  .cover {
    aspect-ratio: 3 / 4;
    background: #1c1c22;
  }
  .nocover {
    display: grid;
    place-items: center;
    height: 100%;
    color: #71717a;
    font-size: 0.85rem;
  }
  .info {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.6rem 0.7rem;
  }
  .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
