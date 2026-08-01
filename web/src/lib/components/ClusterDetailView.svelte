<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { getApi, type Cluster, type ClusterAsset, type IllumiaApi } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import FaceCrop from './FaceCrop.svelte';

  interface Props {
    clusterId: string;
    api?: IllumiaApi;
    basePath?: string;
  }
  const { clusterId, api = getApi(), basePath = '/people' }: Props = $props();

  let cluster = $state<Cluster | null>(null);
  let items = $state<ClusterAsset[]>([]);
  let others = $state<Cluster[]>([]);
  let nameDraft = $state('');
  let loading = $state(true);
  let error = $state<string | null>(null);

  let mergeOpen = $state(false);
  let splitMode = $state(false);
  let selected = $state<string[]>([]); // face_id
  const selectedSet = $derived(new Set(selected));

  async function load() {
    loading = true;
    error = null;
    try {
      const [all, assets] = await Promise.all([
        api.listClusters(),
        api.getClusterAssets(clusterId)
      ]);
      cluster = all.find((c) => c.id === clusterId) ?? null;
      others = all.filter((c) => c.id !== clusterId);
      items = assets;
      nameDraft = cluster?.name ?? '';
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  async function saveName() {
    const name = nameDraft.trim();
    if (!cluster || name === (cluster.name ?? '')) return;
    try {
      cluster = await api.renameCluster(clusterId, name);
      toasts.success('名前を変更しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '改名に失敗しました');
    }
  }

  async function mergeInto(targetId: string) {
    try {
      await api.mergeClusters(clusterId, targetId);
      toasts.success('マージしました');
      mergeOpen = false;
      await goto(`${basePath}/${targetId}`);
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : 'マージに失敗しました');
    }
  }

  function toggleFace(faceId: string) {
    selected = selectedSet.has(faceId)
      ? selected.filter((f) => f !== faceId)
      : [...selected, faceId];
  }

  async function doSplit() {
    if (selected.length === 0) return;
    try {
      const nc = await api.splitCluster(clusterId, selected);
      toasts.success('分割しました');
      splitMode = false;
      selected = [];
      await goto(`${basePath}/${nc.id}`);
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '分割に失敗しました');
    }
  }

  onMount(load);
</script>

<div class="page">
  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else}
    <header>
      <a class="back" href={basePath}>← 一覧</a>
      <input
        class="name"
        bind:value={nameDraft}
        onblur={saveName}
        placeholder="未命名"
        aria-label="クラスタ名"
      />
      <span class="muted count">{cluster?.count ?? 0} 枚</span>
      <div class="actions">
        {#if splitMode}
          <button class="btn primary" disabled={selected.length === 0} onclick={doSplit}>
            選択を分割 ({selected.length})
          </button>
          <button
            class="btn"
            onclick={() => {
              splitMode = false;
              selected = [];
            }}
          >
            やめる
          </button>
        {:else}
          <button class="btn" onclick={() => (mergeOpen = true)} disabled={others.length === 0}>
            マージ
          </button>
          <button class="btn" onclick={() => (splitMode = true)} disabled={items.length === 0}>
            分割
          </button>
        {/if}
      </div>
    </header>

    {#if splitMode}
      <p class="hint muted small">新しいクラスタへ移す顔を選択してください。</p>
    {/if}

    <div class="grid">
      {#each items as it (it.face.id)}
        <button
          class="tile"
          class:selectable={splitMode}
          class:selected={selectedSet.has(it.face.id)}
          onclick={() => splitMode && toggleFace(it.face.id)}
          disabled={!splitMode}
          aria-label={`${it.asset.filename} の顔`}
        >
          <FaceCrop {api} assetId={it.asset.id} bbox={it.face.bbox} alt={it.asset.filename} />
          {#if splitMode && selectedSet.has(it.face.id)}
            <span class="check">✓</span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

{#if mergeOpen}
  <div class="overlay">
    <button class="backdrop" aria-label="閉じる" onclick={() => (mergeOpen = false)}></button>
    <div class="dialog" role="dialog" aria-modal="true">
      <h2>マージ先を選択</h2>
      <p class="muted small">このクラスタを選んだクラスタに統合します。</p>
      <ul>
        {#each others as o (o.id)}
          <li>
            <button onclick={() => mergeInto(o.id)}>
              <span class="mname" class:unnamed={!o.name}>{o.name ?? '未命名'}</span>
              <span class="muted small">{o.count} 枚</span>
            </button>
          </li>
        {/each}
      </ul>
      <div class="foot">
        <button class="close" onclick={() => (mergeOpen = false)}>閉じる</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    padding: 1.25rem 1.5rem 3rem;
  }
  header {
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }
  .back {
    color: #a1a1aa;
    text-decoration: none;
    white-space: nowrap;
  }
  .name {
    font-size: 1.3rem;
    font-weight: 700;
    background: none;
    border: 1px solid transparent;
    border-radius: 8px;
    padding: 0.3rem 0.5rem;
    color: #f4f4f5;
    min-width: 10rem;
  }
  .name:hover,
  .name:focus {
    border-color: #3f3f46;
    background: #16161c;
    outline: none;
  }
  .count {
    flex: 1;
    font-size: 0.9rem;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  .btn {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.45rem 0.9rem;
    border-radius: 8px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .btn.primary {
    background: #6d5bd0;
    border-color: #6d5bd0;
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: default;
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
  .hint {
    margin: 0 0 0.75rem;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(110px, 1fr));
    gap: 0.6rem;
  }
  .tile {
    position: relative;
    aspect-ratio: 1;
    padding: 0;
    border: none;
    border-radius: 8px;
    overflow: hidden;
    background: #1c1c22;
    cursor: default;
  }
  .tile.selectable {
    cursor: pointer;
  }
  .tile.selected {
    outline: 3px solid #6d5bd0;
    outline-offset: -3px;
  }
  .check {
    position: absolute;
    top: 4px;
    left: 4px;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: #6d5bd0;
    color: #fff;
    font-size: 14px;
    line-height: 22px;
    text-align: center;
  }
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 120;
    display: grid;
    place-items: center;
  }
  .backdrop {
    position: absolute;
    inset: 0;
    border: none;
    background: rgba(8, 8, 12, 0.7);
    cursor: pointer;
  }
  .dialog {
    position: relative;
    width: min(90vw, 380px);
    max-height: 80vh;
    overflow-y: auto;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 1.5rem;
  }
  .dialog h2 {
    margin: 0 0 0.5rem;
    font-size: 1.1rem;
  }
  .dialog ul {
    list-style: none;
    margin: 0.75rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .dialog li button {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    background: #1c1c22;
    border: 1px solid #26262e;
    border-radius: 8px;
    padding: 0.6rem 0.8rem;
    color: #f4f4f5;
    cursor: pointer;
  }
  .dialog li button:hover {
    border-color: #6d5bd0;
  }
  .mname.unnamed {
    color: #71717a;
    font-style: italic;
  }
  .foot {
    margin-top: 1rem;
    text-align: right;
  }
  .close {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }

  @media (max-width: 640px) {
    .page {
      padding: 1rem 1rem 2rem;
    }
    header {
      flex-wrap: wrap;
      gap: 0.5rem;
    }
    .name {
      flex: 1;
      min-width: 0;
    }
    .count {
      flex: 0 0 auto;
    }
    .actions {
      flex-basis: 100%;
      justify-content: flex-end;
    }
  }
</style>
