<script lang="ts">
  // 全画面ビューアの骨格。左右キー/スワイプで前後、Esc で閉じる。preview URL を使う。
  import { getApi } from '$lib/api';

  interface Props {
    ids: string[]; // 表示順の asset id 列 (読み込み済み範囲)
    index: number; // 現在位置
    onClose: () => void;
    onIndex: (i: number) => void;
  }

  const { ids, index, onClose, onIndex }: Props = $props();
  const api = getApi();

  const current = $derived(ids[index]);
  const canPrev = $derived(index > 0);
  const canNext = $derived(index < ids.length - 1);

  function prev() {
    if (canPrev) onIndex(index - 1);
  }
  function next() {
    if (canNext) onIndex(index + 1);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose();
    else if (e.key === 'ArrowLeft') prev();
    else if (e.key === 'ArrowRight') next();
  }

  // スワイプ検出。
  let touchStartX = 0;
  function onTouchStart(e: TouchEvent) {
    touchStartX = e.changedTouches[0].clientX;
  }
  function onTouchEnd(e: TouchEvent) {
    const dx = e.changedTouches[0].clientX - touchStartX;
    if (Math.abs(dx) > 50) {
      if (dx > 0) prev();
      else next();
    }
  }
</script>

<svelte:window on:keydown={onKeydown} />

<div
  class="overlay"
  role="dialog"
  aria-modal="true"
  aria-label="画像ビューア"
  tabindex="-1"
  ontouchstart={onTouchStart}
  ontouchend={onTouchEnd}
>
  <button class="close" onclick={onClose} aria-label="閉じる">×</button>
  <button class="nav prev" onclick={prev} disabled={!canPrev} aria-label="前へ">‹</button>

  {#if current}
    <img class="stage" src={api.previewUrl(current)} alt={current} draggable="false" />
  {/if}

  <button class="nav next" onclick={next} disabled={!canNext} aria-label="次へ">›</button>
  <div class="counter">{index + 1} / {ids.length}</div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    background: rgba(8, 8, 12, 0.94);
    display: grid;
    place-items: center;
    user-select: none;
  }
  .stage {
    max-width: 92vw;
    max-height: 92vh;
    object-fit: contain;
    box-shadow: 0 8px 40px rgba(0, 0, 0, 0.5);
  }
  .close {
    position: absolute;
    top: 12px;
    right: 16px;
    font-size: 28px;
    line-height: 1;
    background: none;
    border: none;
    color: #f4f4f5;
    cursor: pointer;
  }
  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    font-size: 48px;
    line-height: 1;
    background: none;
    border: none;
    color: #f4f4f5;
    cursor: pointer;
    padding: 0 12px;
  }
  .nav:disabled {
    opacity: 0.25;
    cursor: default;
  }
  .prev {
    left: 8px;
  }
  .next {
    right: 8px;
  }
  .counter {
    position: absolute;
    bottom: 16px;
    left: 50%;
    transform: translateX(-50%);
    color: #d4d4d8;
    font-size: 0.9rem;
    font-variant-numeric: tabular-nums;
  }
</style>
