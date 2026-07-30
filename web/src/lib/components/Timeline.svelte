<script lang="ts">
  // タイムライン本体: バケット単位の仮想スクロール + 3 段ズーム。
  // docs/04 の方式 (Immich 系 time-bucket) を踏襲する。
  //  - buckets API で {key,count} を取得 → 高さ推定 → 全体スクロール高を構成
  //  - 可視域 ±2 バケットのみ実データを取得しレイアウト・描画
  //  - 画面外バケットは DOM から外す。データは LRU キャッシュに残す
  import { onMount } from 'svelte';
  import { getApi, type Bucket, type BucketItem, type Granularity } from '$lib/api';
  import { LruCache } from '$lib/timeline/lru';
  import { bucketLabel } from '$lib/timeline/format';
  import { estimateContentHeight, place, type PlacedTile } from '$lib/timeline/place';
  import Viewer from './Viewer.svelte';

  const api = getApi();

  /** 見出しの高さ (推定・配置で加算)。 */
  const HEADER_HEIGHT = 44;
  /** 可視域から前後に余分に描画するバケット数。 */
  const OVERSCAN = 2;

  interface BucketMeta {
    key: string;
    count: number;
    top: number;
    height: number; // HEADER_HEIGHT + contentHeight
    contentHeight: number;
    measured: boolean;
    loading: boolean;
    tiles: PlacedTile[];
    itemIds: string[];
  }

  // 粒度別に LRU を分けず、キーに粒度を含めて 1 つで管理する。
  const cache = new LruCache<BucketItem[]>(48);
  const cacheKey = (g: Granularity, key: string) => `${g}:${key}`;

  let granularity = $state<Granularity>('day');
  let buckets = $state<BucketMeta[]>([]);
  let totalHeight = $state(0);
  let containerWidth = $state(0);
  let viewportHeight = $state(0);
  let scrollTop = $state(0);
  let renderStart = $state(0);
  let renderEnd = $state(-1);
  let loadingBuckets = $state(false);
  let errorMsg = $state<string | null>(null);

  let scrollEl: HTMLDivElement | undefined;

  // ビューア状態。
  let viewerOpen = $state(false);
  let viewerIds = $state<string[]>([]);
  let viewerIndex = $state(0);

  const visibleBuckets = $derived(buckets.slice(renderStart, renderEnd + 1));
  const zoomOrder: Granularity[] = ['year', 'month', 'day'];

  function recomputeTops() {
    let acc = 0;
    for (const b of buckets) {
      b.top = acc;
      acc += b.height;
    }
    totalHeight = acc;
  }

  function estimateHeight(count: number): number {
    return HEADER_HEIGHT + estimateContentHeight(granularity, count, containerWidth || 1280);
  }

  async function loadBuckets() {
    loadingBuckets = true;
    errorMsg = null;
    let list: Bucket[] = [];
    try {
      list = await api.getBuckets(granularity);
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : 'バケット取得に失敗しました';
      loadingBuckets = false;
      buckets = [];
      totalHeight = 0;
      return;
    }
    buckets = list.map((b) => ({
      key: b.key,
      count: b.count,
      top: 0,
      height: estimateHeight(b.count),
      contentHeight: 0,
      measured: false,
      loading: false,
      tiles: [],
      itemIds: []
    }));
    recomputeTops();
    if (scrollEl) scrollEl.scrollTop = 0;
    scrollTop = 0;
    loadingBuckets = false;
    updateVisible();
  }

  function computeVisibleRange(): [number, number] {
    const top = scrollTop;
    const bottom = scrollTop + viewportHeight;
    let start = -1;
    let end = -1;
    for (let i = 0; i < buckets.length; i++) {
      const b = buckets[i];
      if (b.top + b.height > top && b.top < bottom) {
        if (start < 0) start = i;
        end = i;
      }
    }
    if (start < 0) return [0, -1];
    return [start, end];
  }

  function updateVisible() {
    if (buckets.length === 0 || viewportHeight === 0) {
      renderStart = 0;
      renderEnd = -1;
      return;
    }
    const [vs, ve] = computeVisibleRange();
    renderStart = Math.max(0, vs - OVERSCAN);
    renderEnd = Math.min(buckets.length - 1, (ve < 0 ? vs : ve) + OVERSCAN);
    for (let i = renderStart; i <= renderEnd; i++) {
      void ensureLoaded(i);
    }
  }

  async function ensureLoaded(i: number) {
    const b = buckets[i];
    if (!b || b.measured || b.loading || containerWidth <= 0) return;
    b.loading = true;

    const ck = cacheKey(granularity, b.key);
    let items = cache.get(ck);
    if (!items) {
      try {
        items = await api.getBucketItems(granularity, b.key);
      } catch {
        b.loading = false;
        return;
      }
      cache.set(ck, items);
    }

    // 取得中に粒度が変わっていたら破棄 (キーで判定)。
    if (buckets[i] !== b) return;

    const placement = place(granularity, items, containerWidth);
    const oldHeight = b.height;
    const wasAbove = b.top + oldHeight <= scrollTop;

    b.tiles = placement.tiles;
    b.contentHeight = placement.contentHeight;
    b.height = HEADER_HEIGHT + placement.contentHeight;
    b.itemIds = items.map((x) => x.id);
    b.measured = true;
    b.loading = false;

    recomputeTops();

    // 画面外 (上方) のバケット高さが変わった場合はスクロール位置を補正し、
    // 可視域のガタつきを防ぐ (docs/04: スクロール補正)。
    if (wasAbove && scrollEl) {
      scrollEl.scrollTop += b.height - oldHeight;
    }
  }

  function relayout() {
    if (containerWidth <= 0 || buckets.length === 0) return;
    for (const b of buckets) {
      if (b.measured) {
        const items = cache.get(cacheKey(granularity, b.key));
        if (items) {
          const p = place(granularity, items, containerWidth);
          b.tiles = p.tiles;
          b.contentHeight = p.contentHeight;
          b.height = HEADER_HEIGHT + p.contentHeight;
        } else {
          b.measured = false;
          b.tiles = [];
          b.height = estimateHeight(b.count);
        }
      } else {
        b.height = estimateHeight(b.count);
      }
    }
    recomputeTops();
    updateVisible();
  }

  function onScroll() {
    if (!scrollEl) return;
    scrollTop = scrollEl.scrollTop;
    updateVisible();
  }

  // ---- ズーム操作 ----
  let lastZoom = 0;
  function changeZoom(dir: number) {
    const idx = zoomOrder.indexOf(granularity);
    const ni = Math.min(zoomOrder.length - 1, Math.max(0, idx + dir));
    if (ni !== idx) setGranularity(zoomOrder[ni]);
  }

  function setGranularity(g: Granularity) {
    if (g === granularity) return;
    granularity = g;
    loadBuckets();
  }

  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey) return; // Ctrl+ホイール / トラックパッドのピンチ
    e.preventDefault();
    const now = Date.now();
    if (now - lastZoom < 250) return;
    lastZoom = now;
    changeZoom(e.deltaY < 0 ? 1 : -1); // 拡大 = より細かい粒度へ
  }

  // ---- タッチ ピンチ ----
  let pinchStart = 0;
  function touchDist(t: TouchList): number {
    const dx = t[0].clientX - t[1].clientX;
    const dy = t[0].clientY - t[1].clientY;
    return Math.hypot(dx, dy);
  }
  function onTouchMove(e: TouchEvent) {
    if (e.touches.length !== 2) return;
    const d = touchDist(e.touches);
    if (pinchStart === 0) {
      pinchStart = d;
      return;
    }
    const ratio = d / pinchStart;
    const now = Date.now();
    if (now - lastZoom < 250) return;
    if (ratio > 1.3) {
      lastZoom = now;
      changeZoom(1);
      pinchStart = d;
    } else if (ratio < 0.77) {
      lastZoom = now;
      changeZoom(-1);
      pinchStart = d;
    }
  }
  function onTouchEnd(e: TouchEvent) {
    if (e.touches.length < 2) pinchStart = 0;
  }

  // ---- ビューア ----
  function openViewer(id: string) {
    // 読み込み済みバケットを順に連結した id 列で前後移動する。
    viewerIds = buckets.flatMap((b) => b.itemIds);
    const idx = viewerIds.indexOf(id);
    viewerIndex = idx >= 0 ? idx : 0;
    viewerOpen = true;
  }

  // 画像プレースホルダ色 (thumbhash デコードは将来対応。ここは id ハッシュ由来)。
  function placeholderColor(id: string): string {
    let h = 0;
    for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) % 360;
    return `hsl(${h} 25% 22%)`;
  }

  // 幅変化に追従して再レイアウト。relayout() が containerWidth を読むので依存に入る。
  $effect(() => {
    relayout();
  });

  onMount(() => {
    loadBuckets();
    // wheel は preventDefault のため非パッシブで自前登録。
    const el = scrollEl;
    if (el) {
      el.addEventListener('wheel', onWheel, { passive: false });
      el.addEventListener('touchmove', onTouchMove, { passive: true });
      el.addEventListener('touchend', onTouchEnd, { passive: true });
    }
    return () => {
      if (el) {
        el.removeEventListener('wheel', onWheel);
        el.removeEventListener('touchmove', onTouchMove);
        el.removeEventListener('touchend', onTouchEnd);
      }
    };
  });
</script>

<div class="toolbar">
  <div class="zoom">
    {#each zoomOrder as g (g)}
      <button
        class:active={granularity === g}
        onclick={() => setGranularity(g)}
        aria-pressed={granularity === g}
      >
        {g === 'day' ? '日' : g === 'month' ? '月' : '年'}
      </button>
    {/each}
  </div>
  <div class="hint">Ctrl+ホイール / ピンチでズーム</div>
</div>

<div class="scroller" bind:this={scrollEl} onscroll={onScroll} bind:clientHeight={viewportHeight}>
  {#if errorMsg}
    <p class="status error">{errorMsg}</p>
  {:else if loadingBuckets}
    <p class="status">読み込み中…</p>
  {:else if buckets.length === 0}
    <p class="status">画像がありません。</p>
  {/if}

  <div class="spacer" style="height:{totalHeight}px" bind:clientWidth={containerWidth}>
    {#each visibleBuckets as b (b.key)}
      <section class="bucket" style="transform: translateY({b.top}px)">
        <h2 class="bucket-header" style="height:{HEADER_HEIGHT}px">
          {bucketLabel(granularity, b.key)}
          <span class="count">{b.count}</span>
        </h2>
        <div class="bucket-body" style="height:{b.contentHeight}px; top:{HEADER_HEIGHT}px">
          {#each b.tiles as t (t.id)}
            <button
              class="tile"
              style="left:{t.x}px; top:{t.y}px; width:{t.width}px; height:{t.height}px; background:{placeholderColor(
                t.id
              )}"
              onclick={() => openViewer(t.id)}
              aria-label="画像を開く"
            >
              <img src={api.thumbnailUrl(t.id)} alt="" loading="lazy" draggable="false" />
            </button>
          {/each}
        </div>
      </section>
    {/each}
  </div>
</div>

{#if viewerOpen}
  <Viewer
    ids={viewerIds}
    index={viewerIndex}
    onClose={() => (viewerOpen = false)}
    onIndex={(i) => (viewerIndex = i)}
  />
{/if}

<style>
  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid #26262e;
    background: #16161c;
  }
  .zoom {
    display: inline-flex;
    gap: 2px;
    background: #26262e;
    border-radius: 8px;
    padding: 2px;
  }
  .zoom button {
    border: none;
    background: none;
    color: #d4d4d8;
    padding: 6px 14px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .zoom button.active {
    background: #6d5bd0;
    color: #fff;
  }
  .hint {
    color: #71717a;
    font-size: 0.8rem;
  }
  .scroller {
    position: relative;
    height: calc(100vh - 49px);
    overflow-y: auto;
    overflow-x: hidden;
    padding: 0 12px;
    background: #101116;
  }
  .spacer {
    position: relative;
    width: 100%;
  }
  .status {
    position: absolute;
    top: 40%;
    left: 0;
    right: 0;
    text-align: center;
    color: #a1a1aa;
  }
  .status.error {
    color: #f87171;
  }
  .bucket {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    will-change: transform;
  }
  .bucket-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0;
    font-size: 1rem;
    font-weight: 700;
    color: #e4e4e7;
  }
  .bucket-header .count {
    font-size: 0.75rem;
    font-weight: 500;
    color: #71717a;
  }
  .bucket-body {
    position: absolute;
    left: 0;
    right: 0;
  }
  .tile {
    position: absolute;
    padding: 0;
    border: none;
    border-radius: 3px;
    overflow: hidden;
    cursor: pointer;
    display: block;
  }
  .tile img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
</style>
