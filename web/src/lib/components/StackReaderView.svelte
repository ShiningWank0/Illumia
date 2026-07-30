<script lang="ts">
  import { onMount } from 'svelte';
  import { goto, replaceState } from '$app/navigation';
  import { getApi, type Asset, type IllumiaApi } from '$lib/api';
  import AssetImage from './AssetImage.svelte';

  interface Props {
    stackId: string;
    initialPage?: number;
    api?: IllumiaApi;
    basePath?: string;
  }
  const { stackId, initialPage = 1, api = getApi(), basePath = '/stacks' }: Props = $props();

  type Mode = 'vertical' | 'rtl' | 'ltr';
  interface RPage {
    asset: Asset;
    chapterNo: number;
    chapterTitle: string;
    isChapterStart: boolean;
  }

  let title = $state('');
  let pages = $state<RPage[]>([]);
  let current = $state(0);
  let mode = $state<Mode>('rtl');
  let loading = $state(true);
  let error = $state<string | null>(null);

  function loadMode(): Mode {
    if (typeof localStorage === 'undefined') return 'rtl';
    const m = localStorage.getItem('illumia.readerMode');
    return m === 'vertical' || m === 'ltr' || m === 'rtl' ? m : 'rtl';
  }
  function setMode(m: Mode) {
    mode = m;
    if (typeof localStorage !== 'undefined') localStorage.setItem('illumia.readerMode', m);
  }

  function clamp(v: number, lo: number, hi: number): number {
    return Math.min(Math.max(v, lo), hi);
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const stack = await api.getStack(stackId);
      title = stack.title;
      const flat: RPage[] = [];
      for (const c of stack.chapters) {
        c.pages.forEach((p, i) => {
          flat.push({
            asset: p.asset,
            chapterNo: c.chapter_no,
            chapterTitle: c.title && c.title.trim() !== '' ? c.title : `第${c.chapter_no}話`,
            isChapterStart: i === 0
          });
        });
      }
      pages = flat;
      current = clamp(initialPage - 1, 0, Math.max(0, flat.length - 1));
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  function go(delta: number) {
    current = clamp(current + delta, 0, pages.length - 1);
    if (typeof history !== 'undefined') replaceState(`?page=${current + 1}`, {});
  }

  const currentPage = $derived(pages[current]);

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      void goto(`${basePath}/${stackId}`);
      return;
    }
    if (mode === 'vertical') return;
    if (e.key === 'ArrowLeft') go(mode === 'rtl' ? 1 : -1);
    else if (e.key === 'ArrowRight') go(mode === 'rtl' ? -1 : 1);
  }

  function tapNext() {
    go(1);
  }
  function tapPrev() {
    go(-1);
  }

  let touchX = 0;
  function onTouchStart(e: TouchEvent) {
    touchX = e.changedTouches[0].clientX;
  }
  function onTouchEnd(e: TouchEvent) {
    if (mode === 'vertical') return;
    const dx = e.changedTouches[0].clientX - touchX;
    if (Math.abs(dx) < 50) return;
    const swipeLeft = dx < 0;
    const next = (mode === 'ltr' && swipeLeft) || (mode === 'rtl' && !swipeLeft);
    go(next ? 1 : -1);
  }

  onMount(() => {
    mode = loadMode();
    load();
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="reader">
  <header>
    <button class="back" onclick={() => goto(`${basePath}/${stackId}`)}>← 戻る (Esc)</button>
    <span class="title">{title}</span>
    {#if currentPage}
      <span class="muted">{currentPage.chapterTitle} · {current + 1}/{pages.length}</span>
    {/if}
    <div class="modes">
      <button class:active={mode === 'vertical'} onclick={() => setMode('vertical')}>縦</button>
      <button class:active={mode === 'rtl'} onclick={() => setMode('rtl')}>右→左</button>
      <button class:active={mode === 'ltr'} onclick={() => setMode('ltr')}>左→右</button>
    </div>
  </header>

  {#if loading}
    <p class="status">読み込み中…</p>
  {:else if error}
    <p class="status err">{error}</p>
  {:else if pages.length === 0}
    <p class="status">ページがありません。</p>
  {:else if mode === 'vertical'}
    <div class="vertical">
      {#each pages as p, i (p.asset.id + i)}
        {#if p.isChapterStart}
          <h3 class="chap-sep">{p.chapterTitle}</h3>
        {/if}
        <div class="v-page">
          {#if p.asset.status === 'trashed'}
            <div class="placeholder">削除された画像</div>
          {:else}
            <AssetImage
              {api}
              id={p.asset.id}
              variant="preview"
              thumbhash={p.asset.thumbhash}
              fit="contain"
              lazy
            />
          {/if}
        </div>
      {/each}
    </div>
  {:else}
    <div
      class="paged"
      role="group"
      aria-label="ページ表示"
      ontouchstart={onTouchStart}
      ontouchend={onTouchEnd}
    >
      {#if currentPage}
        <div class="stage">
          {#key currentPage.asset.id}
            {#if currentPage.asset.status === 'trashed'}
              <div class="placeholder big">削除された画像</div>
            {:else}
              <AssetImage
                {api}
                id={currentPage.asset.id}
                variant="preview"
                thumbhash={currentPage.asset.thumbhash}
                fit="contain"
              />
            {/if}
          {/key}
        </div>
      {/if}

      <button
        class="zone left"
        aria-label={mode === 'rtl' ? '次へ' : '前へ'}
        onclick={mode === 'rtl' ? tapNext : tapPrev}
      ></button>
      <button
        class="zone right"
        aria-label={mode === 'rtl' ? '前へ' : '次へ'}
        onclick={mode === 'rtl' ? tapPrev : tapNext}
      ></button>

      <div class="prefetch" aria-hidden="true">
        {#each [1, 2] as d (d)}
          {#if pages[current + d] && pages[current + d].asset.status !== 'trashed'}
            <AssetImage {api} id={pages[current + d].asset.id} variant="preview" />
          {/if}
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .reader {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: #08080c;
  }
  header {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem 1rem;
    background: #0c0c10;
    border-bottom: 1px solid #26262e;
    flex-shrink: 0;
  }
  .back {
    background: none;
    border: 1px solid #3f3f46;
    color: #f4f4f5;
    padding: 0.35rem 0.7rem;
    border-radius: 7px;
    cursor: pointer;
  }
  .title {
    font-weight: 600;
  }
  .muted {
    color: #a1a1aa;
    font-size: 0.85rem;
  }
  .modes {
    margin-left: auto;
    display: inline-flex;
    gap: 2px;
    background: #26262e;
    border-radius: 8px;
    padding: 2px;
  }
  .modes button {
    border: none;
    background: none;
    color: #d4d4d8;
    padding: 5px 12px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .modes button.active {
    background: #6d5bd0;
    color: #fff;
  }
  .status {
    margin: auto;
    color: #a1a1aa;
  }
  .status.err {
    color: #f87171;
  }
  .vertical {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    padding: 1rem 0 3rem;
  }
  .chap-sep {
    color: #c4b5fd;
    margin: 1.5rem 0 0.5rem;
  }
  .v-page {
    width: min(900px, 96vw);
    min-height: 200px;
    display: flex;
    justify-content: center;
  }
  .v-page :global(.frame) {
    height: auto;
    aspect-ratio: auto;
  }
  .paged {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .stage {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 0.5rem;
  }
  .stage :global(.frame) {
    width: min(900px, 96vw);
    height: 100%;
    background: transparent;
  }
  .zone {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 35%;
    border: none;
    background: transparent;
    cursor: pointer;
  }
  .zone.left {
    left: 0;
  }
  .zone.right {
    right: 0;
  }
  .placeholder {
    display: grid;
    place-items: center;
    width: min(600px, 90vw);
    aspect-ratio: 3 / 4;
    background: #16161c;
    border: 1px dashed #3f3f46;
    color: #71717a;
    border-radius: 8px;
  }
  .placeholder.big {
    height: 80%;
  }
  .prefetch {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    opacity: 0;
    pointer-events: none;
  }
</style>
