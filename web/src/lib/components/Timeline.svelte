<script lang="ts">
  // タイムライン本体: バケット単位の仮想スクロール + 3 段ズーム。
  // docs/04 の方式 (Immich 系 time-bucket) を踏襲する。
  //  - buckets API で {key,count} を取得 → 高さ推定 → 全体スクロール高を構成
  //  - 可視域 ±2 バケットのみ実データを取得しレイアウト・描画
  //  - 画面外バケットは DOM から外す。データは LRU キャッシュに残す
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { getApi, type Bucket, type BucketItem, type Granularity } from '$lib/api';
  import { WS_SUPPORTED, connectAssetsWs, type WsHandle } from '$lib/api/ws';
  import { LruCache } from '$lib/timeline/lru';
  import { bucketLabel } from '$lib/timeline/format';
  import { estimateContentHeight, place, type PlacedTile } from '$lib/timeline/place';
  import { toasts } from '$lib/toast.svelte';
  import AssetImage from './AssetImage.svelte';
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
  let fileInput: HTMLInputElement | undefined;
  let wsHandle: WsHandle | undefined;

  // アップロード / ドラッグ状態。
  let dragging = $state(false);
  let uploading = $state(false);

  // ビューアで参照するため、id → thumbhash / ratio を保持する。
  const itemMeta = new Map<string, { thumbhash: string | null }>();

  // ビューア状態。
  let viewerOpen = $state(false);
  let viewerIds = $state<string[]>([]);
  let viewerIndex = $state(0);

  // 複数選択 (スタック作成用)。selected は選択順を保持する。
  let selecting = $state(false);
  let selected = $state<string[]>([]);
  const selectedSet = $derived(new Set(selected));
  let showCreateDialog = $state(false);
  let newStackTitle = $state('');
  let creating = $state(false);

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

    for (const it of items) itemMeta.set(it.id, { thumbhash: it.thumbhash });
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

  // ---- 複数選択 / スタック作成 ----
  function toggleSelect(id: string) {
    selected = selectedSet.has(id) ? selected.filter((x) => x !== id) : [...selected, id];
  }

  function onTileClick(id: string) {
    if (selecting) toggleSelect(id);
    else openViewer(id);
  }

  // 長押しで選択モードに入る (マウス/タッチ共通の pointer events)。
  let longPressTimer: ReturnType<typeof setTimeout> | undefined;
  function onTilePointerDown(id: string) {
    if (selecting) return;
    longPressTimer = setTimeout(() => {
      selecting = true;
      toggleSelect(id);
    }, 450);
  }
  function cancelLongPress() {
    if (longPressTimer) clearTimeout(longPressTimer);
    longPressTimer = undefined;
  }

  function exitSelection() {
    selecting = false;
    selected = [];
    showCreateDialog = false;
  }

  async function createStackFromSelection() {
    const title = newStackTitle.trim();
    if (title === '' || selected.length === 0) return;
    creating = true;
    try {
      const stack = await api.createStack(title, selected);
      toasts.success(`「${title}」を作成しました`);
      exitSelection();
      await goto(`/stacks/${stack.id}`);
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : 'スタック作成に失敗しました');
    } finally {
      creating = false;
    }
  }

  // ---- アップロード ----
  function pad2(n: number): string {
    return n < 10 ? `0${n}` : String(n);
  }

  /** ファイルのローカル日付から各粒度のバケットキーを返す。 */
  function bucketKeysOf(file: File): Record<Granularity, string> {
    const d = new Date(file.lastModified);
    const y = d.getFullYear();
    const day = `${y}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
    return { day, month: day.slice(0, 7), year: day.slice(0, 4) };
  }

  function invalidateFile(file: File) {
    const keys = bucketKeysOf(file);
    cache.delete(cacheKey('day', keys.day));
    cache.delete(cacheKey('month', keys.month));
    cache.delete(cacheKey('year', keys.year));
  }

  async function handleFiles(files: FileList | File[]) {
    const arr = [...files].filter((f) => f.type.startsWith('image/'));
    if (arr.length === 0) return;
    uploading = true;
    let created = 0;
    let dup = 0;
    let failed = 0;
    const affected = new Set<string>();
    for (const f of arr) {
      try {
        const r = await api.uploadAsset(f);
        if (r.status === 'duplicate') dup++;
        else created++;
        affected.add(bucketKeysOf(f)[granularity]);
        invalidateFile(f);
      } catch (e) {
        failed++;
        toasts.error(`${f.name}: ${e instanceof Error ? e.message : 'アップロード失敗'}`);
      }
    }
    uploading = false;
    const parts = [`新規 ${created}`, `重複 ${dup}`];
    if (failed) parts.push(`失敗 ${failed}`);
    toasts.push(`アップロード完了: ${parts.join(' / ')}`, failed ? 'error' : 'success');
    if (created > 0 || dup > 0) await syncBuckets(affected);
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    if (e.dataTransfer?.files) void handleFiles(e.dataTransfer.files);
  }
  function onDragOver(e: DragEvent) {
    e.preventDefault();
    dragging = true;
  }
  function onDragLeave() {
    dragging = false;
  }
  function onFilePicked(e: Event) {
    const input = e.target as HTMLInputElement;
    if (input.files) void handleFiles(input.files);
    input.value = '';
  }

  /**
   * バケット一覧を再取得し、スクロール位置を保ちつつ差分だけ反映する。
   * 変更のないバケットは計測済みレイアウトを再利用する (全体再計算を避ける)。
   */
  async function syncBuckets(invalidateKeys: Set<string>) {
    let list: Bucket[];
    try {
      list = await api.getBuckets(granularity);
    } catch {
      return;
    }
    const prev = new Map(buckets.map((b) => [b.key, b]));
    const [anchorIdx] = computeVisibleRange();
    const anchor = buckets[anchorIdx];
    const anchorKey = anchor?.key;
    const anchorDelta = anchor ? scrollTop - anchor.top : 0;

    buckets = list.map((b) => {
      const old = prev.get(b.key);
      const reuse = old && old.measured && old.count === b.count && !invalidateKeys.has(b.key);
      if (reuse) return old;
      return {
        key: b.key,
        count: b.count,
        top: 0,
        height: estimateHeight(b.count),
        contentHeight: 0,
        measured: false,
        loading: false,
        tiles: [],
        itemIds: []
      };
    });
    recomputeTops();
    if (anchorKey && scrollEl) {
      const na = buckets.find((x) => x.key === anchorKey);
      if (na) {
        scrollEl.scrollTop = na.top + anchorDelta;
        scrollTop = scrollEl.scrollTop;
      }
    }
    updateVisible();
  }

  /** WS assets_added (現状 WS_SUPPORTED=false のため未使用パス)。 */
  function handleAssetsAdded(dayKeys: string[]) {
    const affected = new Set<string>();
    for (const dayKey of dayKeys) {
      const gk = granularity === 'day' ? dayKey : dayKey.slice(0, granularity === 'month' ? 7 : 4);
      cache.delete(cacheKey('day', dayKey));
      cache.delete(cacheKey('month', dayKey.slice(0, 7)));
      cache.delete(cacheKey('year', dayKey.slice(0, 4)));
      affected.add(gk);
    }
    void syncBuckets(affected);
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
    // WS は require_auth (Bearer ヘッダ) 配下でブラウザから接続不可のため、
    // WS_SUPPORTED=false の間は接続しない (→ src/lib/api/ws.ts の注記)。
    if (WS_SUPPORTED) {
      wsHandle = connectAssetsWs(handleAssetsAdded);
    }
    return () => {
      if (el) {
        el.removeEventListener('wheel', onWheel);
        el.removeEventListener('touchmove', onTouchMove);
        el.removeEventListener('touchend', onTouchEnd);
      }
      wsHandle?.close();
    };
  });
</script>

<div class="timeline">
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
    <button
      class="select-toggle"
      class:active={selecting}
      onclick={() => (selecting ? exitSelection() : (selecting = true))}
    >
      {selecting ? '選択解除' : '選択'}
    </button>
    <button class="upload" onclick={() => fileInput?.click()} disabled={uploading}>
      {uploading ? 'アップロード中…' : 'アップロード'}
    </button>
    <input
      bind:this={fileInput}
      type="file"
      accept="image/*"
      multiple
      hidden
      onchange={onFilePicked}
    />
  </div>

  <div
    class="scroller"
    class:dragging
    bind:this={scrollEl}
    onscroll={onScroll}
    bind:clientHeight={viewportHeight}
    ondrop={onDrop}
    ondragover={onDragOver}
    ondragleave={onDragLeave}
    role="list"
  >
    {#if errorMsg}
      <p class="status error">{errorMsg}</p>
    {:else if loadingBuckets}
      <p class="status">読み込み中…</p>
    {:else if buckets.length === 0}
      <p class="status">画像がありません。ドラッグ&ドロップかアップロードで追加できます。</p>
    {/if}

    {#if dragging}
      <div class="drop-hint">ここにドロップしてアップロード</div>
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
                class:selected={selectedSet.has(t.id)}
                style="left:{t.x}px; top:{t.y}px; width:{t.width}px; height:{t.height}px"
                onclick={() => onTileClick(t.id)}
                onpointerdown={() => onTilePointerDown(t.id)}
                onpointerup={cancelLongPress}
                onpointerleave={cancelLongPress}
                aria-label={selecting ? '選択の切替' : '画像を開く'}
              >
                <AssetImage id={t.id} thumbhash={t.thumbhash} />
                {#if selecting}
                  <span class="check" class:on={selectedSet.has(t.id)}>
                    {selectedSet.has(t.id) ? '✓' : ''}
                  </span>
                {/if}
              </button>
            {/each}
          </div>
        </section>
      {/each}
    </div>
  </div>
</div>

{#if selecting}
  <div class="select-bar">
    <span>{selected.length} 枚選択中</span>
    <div class="spacer-x"></div>
    <button
      class="primary"
      disabled={selected.length === 0}
      onclick={() => {
        newStackTitle = '';
        showCreateDialog = true;
      }}
    >
      漫画スタックを作成
    </button>
    <button onclick={exitSelection}>キャンセル</button>
  </div>
{/if}

{#if showCreateDialog}
  <div class="overlay">
    <button class="backdrop" aria-label="閉じる" onclick={() => (showCreateDialog = false)}
    ></button>
    <div class="dialog" role="dialog" aria-modal="true">
      <h2>漫画スタックを作成</h2>
      <p class="muted">{selected.length} 枚を選択順にページとして登録します。</p>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        type="text"
        placeholder="作品タイトル"
        autofocus
        bind:value={newStackTitle}
        onkeydown={(e) => e.key === 'Enter' && createStackFromSelection()}
      />
      <div class="dialog-actions">
        <button onclick={() => (showCreateDialog = false)}>閉じる</button>
        <button
          class="primary"
          disabled={creating || newStackTitle.trim() === ''}
          onclick={createStackFromSelection}
        >
          {creating ? '作成中…' : '作成'}
        </button>
      </div>
    </div>
  </div>
{/if}

{#if viewerOpen}
  <Viewer
    ids={viewerIds}
    index={viewerIndex}
    thumbhashOf={(id) => itemMeta.get(id)?.thumbhash ?? null}
    onClose={() => (viewerOpen = false)}
    onIndex={(i) => (viewerIndex = i)}
  />
{/if}

<style>
  .timeline {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid #26262e;
    background: #16161c;
    flex-shrink: 0;
  }
  .toolbar .hint {
    flex: 1;
  }
  .upload,
  .select-toggle {
    border: none;
    border-radius: 8px;
    background: #6d5bd0;
    color: #fff;
    padding: 6px 14px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .select-toggle {
    background: #3f3f46;
  }
  .select-toggle.active {
    background: #b3341f;
  }
  .upload:disabled {
    opacity: 0.6;
    cursor: default;
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
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 0 12px;
    background: #101116;
  }
  .scroller.dragging {
    outline: 2px dashed #6d5bd0;
    outline-offset: -6px;
  }
  .drop-hint {
    position: sticky;
    top: 0;
    z-index: 5;
    text-align: center;
    padding: 0.75rem;
    color: #c4b5fd;
    background: rgba(109, 91, 208, 0.15);
    pointer-events: none;
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
    background: #1c1c22;
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
    background: rgba(0, 0, 0, 0.5);
    border: 2px solid #fff;
    color: #fff;
    font-size: 14px;
    line-height: 18px;
    text-align: center;
  }
  .check.on {
    background: #6d5bd0;
    border-color: #6d5bd0;
  }
  .select-bar {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    background: #16161c;
    border-top: 1px solid #26262e;
  }
  .select-bar .spacer-x {
    flex: 1;
  }
  .select-bar button {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }
  .select-bar button.primary,
  .dialog button.primary {
    background: #6d5bd0;
    border-color: #6d5bd0;
    color: #fff;
  }
  .select-bar button:disabled,
  .dialog button:disabled {
    opacity: 0.5;
    cursor: default;
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
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .dialog h2 {
    margin: 0;
    font-size: 1.1rem;
  }
  .dialog .muted {
    margin: 0;
    color: #a1a1aa;
    font-size: 0.85rem;
  }
  .dialog input {
    padding: 0.6rem 0.7rem;
    border-radius: 8px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 1rem;
  }
  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .dialog-actions button {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }
</style>
