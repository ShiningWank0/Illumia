<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type DuplicatePair, type IllumiaApi } from '$lib/api';
  import { downloadOriginal } from '$lib/api/image';
  import { toasts } from '$lib/toast.svelte';
  import AssetImage from './AssetImage.svelte';
  import StackPicker from './StackPicker.svelte';

  interface Props {
    api?: IllumiaApi;
    mode?: 'main' | 'vault';
  }
  const { api = getApi(), mode = 'main' }: Props = $props();

  let pairs = $state<DuplicatePair[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let pickerForAsset = $state<string | null>(null);

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

  async function addToStack(stackId: string) {
    const assetId = pickerForAsset;
    if (!assetId) return;
    try {
      await api.addStackPages(stackId, [assetId]);
      pairs = pairs.filter((p) => p.dup.id !== assetId);
      toasts.success('スタックへ追加しました。自動削除対象から外れました。');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '追加に失敗しました');
    } finally {
      pickerForAsset = null;
    }
  }

  async function download(id: string) {
    try {
      await downloadOriginal(api.originalUrl(id), id);
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : 'ダウンロードに失敗しました');
    }
  }

  function fmt(iso?: string): string {
    if (!iso) return '-';
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? iso : d.toLocaleString('ja-JP');
  }

  onMount(load);
</script>

<div class="page">
  <h1>重複{mode === 'vault' ? ' (Vault)' : ''}</h1>
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
            <div class="thumb"><AssetImage {api} id={p.dup.id} thumbhash={p.dup.thumbhash} /></div>
            <span class="name">{p.dup.filename}</span>
            <div class="side-actions">
              <button class="link" onclick={() => download(p.dup.id)}>ダウンロード</button>
              <button class="add" onclick={() => (pickerForAsset = p.dup.id)}>スタックへ追加</button
              >
            </div>
          </div>
          <div class="arrow">↔</div>
          <div class="side">
            <span class="tag original">オリジナル</span>
            <div class="thumb">
              <AssetImage {api} id={p.original.id} thumbhash={p.original.thumbhash} />
            </div>
            <span class="name">{p.original.filename}</span>
            <button class="link" onclick={() => download(p.original.id)}>ダウンロード</button>
          </div>
          <div class="purge muted small">完全削除予定: {fmt(p.purge_after)}</div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if pickerForAsset}
  <StackPicker {api} onPick={addToStack} onClose={() => (pickerForAsset = null)} />
{/if}

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
  .side-actions {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    align-items: center;
  }
  .link {
    background: none;
    border: none;
    color: #c4b5fd;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .add {
    border: 1px solid #6d5bd0;
    background: none;
    color: #c4b5fd;
    padding: 0.3rem 0.7rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.8rem;
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
