<script lang="ts">
  // スタック選択モーダル。重複ビュー等から「スタックへ追加」に使う。
  import { onMount } from 'svelte';
  import { getApi, type StackSummary } from '$lib/api';

  interface Props {
    onPick: (stackId: string) => void | Promise<void>;
    onClose: () => void;
  }
  const { onPick, onClose }: Props = $props();
  const api = getApi();

  let stacks = $state<StackSummary[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      stacks = await api.listStacks();
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  async function pick(id: string) {
    busy = true;
    try {
      await onPick(id);
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<div class="overlay">
  <button class="backdrop" aria-label="閉じる" onclick={onClose}></button>
  <div class="dialog" role="dialog" aria-modal="true">
    <h2>スタックへ追加</h2>
    {#if loading}
      <p class="muted">読み込み中…</p>
    {:else if error}
      <p class="err">{error}</p>
    {:else if stacks.length === 0}
      <p class="muted">追加先のスタックがありません。先にスタックを作成してください。</p>
    {:else}
      <ul>
        {#each stacks as s (s.id)}
          <li>
            <button disabled={busy} onclick={() => pick(s.id)}>
              <span class="name">{s.title}</span>
              <span class="muted small">{s.chapter_count} 話 / {s.page_count} ページ</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="foot"><button class="close" onclick={onClose}>閉じる</button></div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 130;
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
    width: min(90vw, 420px);
    max-height: 80vh;
    overflow-y: auto;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 1.5rem;
  }
  h2 {
    margin: 0 0 1rem;
    font-size: 1.1rem;
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
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  li button {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.15rem;
    text-align: left;
    background: #1c1c22;
    border: 1px solid #26262e;
    border-radius: 8px;
    padding: 0.6rem 0.8rem;
    color: #f4f4f5;
    cursor: pointer;
  }
  li button:hover {
    border-color: #6d5bd0;
  }
  li button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .name {
    font-weight: 600;
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
</style>
