<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type Cluster, type IllumiaApi } from '$lib/api';
  import FaceCrop from './FaceCrop.svelte';

  interface Props {
    api?: IllumiaApi;
    basePath?: string;
    /** 確認キューへのリンク (メインのみ)。 */
    reviewPath?: string | null;
  }
  const { api = getApi(), basePath = '/people', reviewPath = null }: Props = $props();

  let clusters = $state<Cluster[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load() {
    loading = true;
    error = null;
    try {
      clusters = await api.listClusters();
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<div class="page">
  <div class="head">
    <h1>人物</h1>
    {#if reviewPath}
      <a class="review-link" href={reviewPath}>確認キュー →</a>
    {/if}
  </div>

  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else if clusters.length === 0}
    <p class="muted">
      クラスタがありません。設定の「全アセットを解析」で顔検出を実行してください。
    </p>
  {:else}
    <div class="grid">
      {#each clusters as c (c.id)}
        <a class="card" href={`${basePath}/${c.id}`}>
          <div class="cover">
            {#if c.cover}
              <FaceCrop
                {api}
                assetId={c.cover.asset_id}
                bbox={c.cover.bbox}
                alt={c.name ?? '未命名'}
              />
            {:else}
              <div class="nocover">顔なし</div>
            {/if}
          </div>
          <div class="info">
            <span class="name" class:unnamed={!c.name}>{c.name ?? '未命名'}</span>
            <span class="muted small">{c.count} 枚</span>
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
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
  }
  .review-link {
    color: #c4b5fd;
    text-decoration: none;
    font-size: 0.9rem;
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
    grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
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
    aspect-ratio: 1;
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
    gap: 0.15rem;
    padding: 0.5rem 0.6rem;
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name.unnamed {
    color: #71717a;
    font-style: italic;
    font-weight: 500;
  }
</style>
