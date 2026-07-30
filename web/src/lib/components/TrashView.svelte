<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type Asset, type IllumiaApi } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import AssetImage from './AssetImage.svelte';

  interface Props {
    api?: IllumiaApi;
    /** vault では復元/完全削除 API が無いため read-only。 */
    mode?: 'main' | 'vault';
  }
  const { api = getApi(), mode = 'main' }: Props = $props();

  let items = $state<Asset[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);

  async function load() {
    loading = true;
    error = null;
    try {
      items = await api.getTrash();
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  async function restore(id: string) {
    busy = id;
    try {
      await api.restoreAsset(id);
      items = items.filter((a) => a.id !== id);
      toasts.success('復元しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '復元に失敗しました');
    } finally {
      busy = null;
    }
  }

  async function purge(id: string) {
    if (!confirm('完全に削除します。元に戻せません。よろしいですか?')) return;
    busy = id;
    try {
      await api.purgeNow(id);
      items = items.filter((a) => a.id !== id);
      toasts.success('完全に削除しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '削除に失敗しました');
    } finally {
      busy = null;
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
  <h1>ゴミ箱{mode === 'vault' ? ' (Vault)' : ''}</h1>
  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else if items.length === 0}
    <p class="muted">ゴミ箱は空です。</p>
  {:else}
    <ul class="list">
      {#each items as a (a.id)}
        <li>
          <div class="thumb"><AssetImage {api} id={a.id} thumbhash={a.thumbhash} /></div>
          <div class="meta">
            <span class="name">{a.filename}</span>
            <span class="muted small">完全削除予定: {fmt(a.purge_after)}</span>
          </div>
          <div class="actions">
            <button onclick={() => restore(a.id)} disabled={busy === a.id}>復元</button>
            <button class="danger" onclick={() => purge(a.id)} disabled={busy === a.id}>
              完全に削除
            </button>
          </div>
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
    gap: 0.5rem;
  }
  li {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 8px;
  }
  .thumb {
    width: 64px;
    height: 64px;
    flex-shrink: 0;
    border-radius: 6px;
    overflow: hidden;
    background: #1c1c22;
  }
  .meta {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    flex: 1;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    flex-shrink: 0;
  }
  button {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.4rem 0.8rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.85rem;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  button.danger {
    border-color: #7f1d1d;
    color: #fca5a5;
  }
</style>
