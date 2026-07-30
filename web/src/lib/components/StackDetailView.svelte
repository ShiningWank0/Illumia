<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import {
    getApi,
    type Asset,
    type ChapterInput,
    type IllumiaApi,
    type StackDetail
  } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import AssetImage from './AssetImage.svelte';

  interface Props {
    stackId: string;
    api?: IllumiaApi;
    basePath?: string;
    /** スタックごとの移動 (メイン→vault の import / vault→メイン の export)。 */
    onMoveStack?: (stackId: string) => Promise<void>;
    moveLabel?: string;
    moveDoneMessage?: string;
  }
  const {
    stackId,
    api = getApi(),
    basePath = '/stacks',
    onMoveStack,
    moveLabel = 'Vault へ移動',
    moveDoneMessage = 'スタックを Vault へ移動しました'
  }: Props = $props();

  interface EditPage {
    asset: Asset;
    show_in_timeline: boolean;
  }
  interface EditChapter {
    title: string | null;
    pages: EditPage[];
  }

  let stack = $state<StackDetail | null>(null);
  let editChapters = $state<EditChapter[]>([]);
  let savedSig = $state('');
  let loading = $state(true);
  let error = $state<string | null>(null);
  let saving = $state(false);

  let titleDraft = $state('');
  let menuFor = $state<string | null>(null);
  let showDissolve = $state(false);
  let moving = $state(false);

  function signature(chapters: EditChapter[]): string {
    return JSON.stringify(
      chapters.map((c) => ({ t: c.title ?? '', p: c.pages.map((p) => p.asset.id) }))
    );
  }
  const currentSig = $derived(signature(editChapters));
  const dirty = $derived(currentSig !== savedSig);

  function apply(s: StackDetail) {
    stack = s;
    titleDraft = s.title;
    editChapters = s.chapters.map((c) => ({
      title: c.title,
      pages: c.pages.map((p) => ({ asset: p.asset, show_in_timeline: p.show_in_timeline }))
    }));
    savedSig = signature(editChapters);
  }

  async function load() {
    loading = true;
    error = null;
    try {
      apply(await api.getStack(stackId));
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  // ---- ドラッグ&ドロップ (マウス優先) ----
  let drag = $state<{ ci: number; pi: number } | null>(null);
  let dropTarget = $state<string | null>(null);

  function onDragStart(ci: number, pi: number, e: DragEvent) {
    drag = { ci, pi };
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }
  function onDragEnd() {
    drag = null;
    dropTarget = null;
  }
  function movePage(dstCi: number, dstPi: number) {
    if (!drag) return;
    const src = drag;
    const [pageItem] = editChapters[src.ci].pages.splice(src.pi, 1);
    let di = dstPi;
    if (src.ci === dstCi && src.pi < dstPi) di -= 1;
    editChapters[dstCi].pages.splice(di, 0, pageItem);
    editChapters = editChapters;
    drag = null;
    dropTarget = null;
  }
  function onDropPage(ci: number, pi: number, e: DragEvent) {
    e.preventDefault();
    movePage(ci, pi);
  }
  function onDropChapterEnd(ci: number, e: DragEvent) {
    e.preventDefault();
    movePage(ci, editChapters[ci].pages.length);
  }

  // ---- 話区切り ----
  function splitBefore(ci: number, pi: number) {
    if (pi <= 0) return;
    const ch = editChapters[ci];
    const after = ch.pages.slice(pi);
    ch.pages = ch.pages.slice(0, pi);
    editChapters.splice(ci + 1, 0, { title: null, pages: after });
    editChapters = editChapters;
    menuFor = null;
  }
  function mergeIntoPrev(ci: number) {
    if (ci <= 0) return;
    editChapters[ci - 1].pages.push(...editChapters[ci].pages);
    editChapters.splice(ci, 1);
    editChapters = editChapters;
  }
  function removePage(ci: number, assetId: string) {
    editChapters[ci].pages = editChapters[ci].pages.filter((p) => p.asset.id !== assetId);
    editChapters = editChapters;
    menuFor = null;
  }

  // ---- 即時 API ----
  async function toggleFlag(p: EditPage) {
    try {
      await api.setPageFlag(stackId, p.asset.id, !p.show_in_timeline);
      p.show_in_timeline = !p.show_in_timeline;
      toasts.success(p.show_in_timeline ? 'タイムラインに表示します' : 'タイムラインから隠します');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '更新に失敗しました');
    }
    menuFor = null;
  }
  async function setCover(assetId: string) {
    try {
      apply(await api.patchStack(stackId, { cover_asset_id: assetId }));
      toasts.success('表紙を変更しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '表紙変更に失敗しました');
    }
    menuFor = null;
  }
  async function saveTitle() {
    const t = titleDraft.trim();
    if (t === '' || t === stack?.title) return;
    try {
      apply(await api.patchStack(stackId, { title: t }));
      toasts.success('タイトルを変更しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '改名に失敗しました');
    }
  }

  async function saveStructure() {
    const chapters: ChapterInput[] = editChapters
      .filter((c) => c.pages.length > 0)
      .map((c) => ({
        title: c.title && c.title.trim() !== '' ? c.title.trim() : null,
        pages: c.pages.map((p) => p.asset.id)
      }));
    if (chapters.length === 0) {
      toasts.error('少なくとも 1 ページ必要です');
      return;
    }
    saving = true;
    try {
      apply(await api.replaceStructure(stackId, chapters));
      toasts.success('構成を保存しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '保存に失敗しました');
    } finally {
      saving = false;
    }
  }

  async function dissolve() {
    try {
      await api.deleteStack(stackId);
      toasts.success('スタックを解散しました (画像は削除されていません)');
      await goto(basePath);
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '解散に失敗しました');
    }
  }

  async function moveStack() {
    if (!onMoveStack) return;
    moving = true;
    try {
      await onMoveStack(stackId);
      toasts.success(moveDoneMessage);
      await goto(basePath);
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '移動に失敗しました');
    } finally {
      moving = false;
    }
  }

  onMount(load);
</script>

<div class="page">
  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else if stack}
    <header>
      <a class="back" href={basePath}>← 一覧</a>
      <input class="title" bind:value={titleDraft} onblur={saveTitle} aria-label="タイトル" />
      <div class="head-actions">
        <a class="btn" href={`${basePath}/${stackId}/read?page=1`}>読む</a>
        <button class="btn primary" disabled={!dirty || saving} onclick={saveStructure}>
          {saving ? '保存中…' : dirty ? '構成を保存' : '保存済み'}
        </button>
        {#if onMoveStack}
          <button class="btn" disabled={moving} onclick={moveStack}>{moveLabel}</button>
        {/if}
        <button class="btn danger" onclick={() => (showDissolve = true)}>解散</button>
      </div>
    </header>

    <p class="hint muted small">
      ページをドラッグして並べ替え
      (章をまたいで移動可)。ページの「⋯」から分割・表紙・表示切替・除外。
    </p>

    {#each editChapters as ch, ci (ci)}
      <section class="chapter">
        <div class="chapter-head">
          <input
            class="chapter-title"
            placeholder={`第${ci + 1}話`}
            value={ch.title ?? ''}
            oninput={(e) => (editChapters[ci].title = (e.target as HTMLInputElement).value)}
            aria-label="話タイトル"
          />
          <span class="muted small">{ch.pages.length} ページ</span>
          {#if ci > 0}
            <button class="link" onclick={() => mergeIntoPrev(ci)}>← 前の話に統合</button>
          {/if}
        </div>

        <div
          class="pages"
          role="list"
          ondragover={(e) => e.preventDefault()}
          ondrop={(e) => onDropChapterEnd(ci, e)}
        >
          {#each ch.pages as p, pi (p.asset.id)}
            <div
              class="page-tile"
              class:drop={dropTarget === `${ci}:${pi}`}
              role="listitem"
              draggable="true"
              ondragstart={(e) => onDragStart(ci, pi, e)}
              ondragend={onDragEnd}
              ondragover={(e) => {
                e.preventDefault();
                dropTarget = `${ci}:${pi}`;
              }}
              ondragleave={() => (dropTarget === `${ci}:${pi}` ? (dropTarget = null) : null)}
              ondrop={(e) => onDropPage(ci, pi, e)}
            >
              <div class="thumb" class:trashed={p.asset.status === 'trashed'}>
                <AssetImage {api} id={p.asset.id} thumbhash={p.asset.thumbhash} />
                {#if p.asset.status === 'trashed'}<span class="badge">削除済</span>{/if}
                {#if p.show_in_timeline}<span class="badge tl">TL</span>{/if}
              </div>
              <div class="page-foot">
                <span class="pno">{pi + 1}</span>
                <button
                  class="menu-btn"
                  aria-label="ページ操作"
                  onclick={() => (menuFor = menuFor === p.asset.id ? null : p.asset.id)}
                >
                  ⋯
                </button>
              </div>

              {#if menuFor === p.asset.id}
                <div class="menu">
                  <button onclick={() => splitBefore(ci, pi)} disabled={pi === 0}>
                    ここで話を分割
                  </button>
                  <button onclick={() => setCover(p.asset.id)}>表紙にする</button>
                  <button onclick={() => toggleFlag(p)}>
                    {p.show_in_timeline ? 'タイムラインから隠す' : 'タイムラインに表示'}
                  </button>
                  <button class="danger" onclick={() => removePage(ci, p.asset.id)}>
                    スタックから外す
                  </button>
                </div>
              {/if}
            </div>
          {/each}
          {#if ch.pages.length === 0}
            <p class="muted small empty">ここにドロップ</p>
          {/if}
        </div>
      </section>
    {/each}
  {/if}
</div>

{#if showDissolve}
  <div class="overlay">
    <button class="backdrop" aria-label="閉じる" onclick={() => (showDissolve = false)}></button>
    <div class="dialog" role="dialog" aria-modal="true">
      <h2>スタックを解散しますか?</h2>
      <p class="muted">
        構成 (話・ページ) は削除されますが、<strong>画像そのものは削除されません</strong>。
        所属が外れるため、対象画像はタイムラインに再表示されます。
      </p>
      <div class="dialog-actions">
        <button onclick={() => (showDissolve = false)}>キャンセル</button>
        <button class="danger" onclick={dissolve}>解散する</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    padding: 1.25rem 1.5rem 4rem;
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
  .title {
    flex: 1;
    font-size: 1.3rem;
    font-weight: 700;
    background: none;
    border: 1px solid transparent;
    border-radius: 8px;
    padding: 0.3rem 0.5rem;
    color: #f4f4f5;
  }
  .title:hover,
  .title:focus {
    border-color: #3f3f46;
    background: #16161c;
    outline: none;
  }
  .head-actions {
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
    text-decoration: none;
    font-size: 0.9rem;
  }
  .btn.primary {
    background: #6d5bd0;
    border-color: #6d5bd0;
  }
  .btn.danger {
    border-color: #7f1d1d;
    color: #fca5a5;
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
    margin: 0 0 1rem;
  }
  .chapter {
    margin-bottom: 1.5rem;
  }
  .chapter-head {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.5rem;
  }
  .chapter-title {
    font-size: 1rem;
    font-weight: 600;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 7px;
    padding: 0.35rem 0.6rem;
    color: #f4f4f5;
    min-width: 12rem;
  }
  .link {
    background: none;
    border: none;
    color: #c4b5fd;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .pages {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 0.75rem;
    min-height: 60px;
    padding: 0.5rem;
    border: 1px dashed #26262e;
    border-radius: 10px;
  }
  .page-tile {
    position: relative;
    background: #16161c;
    border-radius: 8px;
    overflow: visible;
    cursor: grab;
  }
  .page-tile.drop {
    outline: 2px solid #6d5bd0;
    outline-offset: 2px;
  }
  .thumb {
    position: relative;
    aspect-ratio: 3 / 4;
    border-radius: 8px 8px 0 0;
    overflow: hidden;
    background: #1c1c22;
  }
  .thumb.trashed {
    opacity: 0.5;
  }
  .badge {
    position: absolute;
    top: 4px;
    left: 4px;
    background: rgba(0, 0, 0, 0.6);
    color: #fff;
    font-size: 0.7rem;
    padding: 1px 5px;
    border-radius: 4px;
  }
  .badge.tl {
    left: auto;
    right: 4px;
    background: #6d5bd0;
  }
  .page-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.2rem 0.4rem;
  }
  .pno {
    font-size: 0.8rem;
    color: #a1a1aa;
  }
  .menu-btn {
    background: none;
    border: none;
    color: #d4d4d8;
    cursor: pointer;
    font-size: 1.1rem;
    line-height: 1;
    padding: 0 4px;
  }
  .menu {
    position: absolute;
    top: 100%;
    right: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: #1c1c22;
    border: 1px solid #3f3f46;
    border-radius: 8px;
    overflow: hidden;
    min-width: 160px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.5);
  }
  .menu button {
    text-align: left;
    background: none;
    border: none;
    color: #f4f4f5;
    padding: 0.5rem 0.75rem;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .menu button:hover {
    background: #26262e;
  }
  .menu button.danger {
    color: #fca5a5;
  }
  .menu button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .empty {
    grid-column: 1 / -1;
    text-align: center;
    align-self: center;
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
    width: min(90vw, 420px);
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 1.5rem;
  }
  .dialog h2 {
    margin: 0 0 0.75rem;
    font-size: 1.1rem;
  }
  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1rem;
  }
  .dialog-actions button {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }
  .dialog-actions button.danger {
    border-color: #7f1d1d;
    color: #fca5a5;
  }
</style>
