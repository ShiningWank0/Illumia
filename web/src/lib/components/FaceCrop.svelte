<script lang="ts">
  // bbox から顔部分を CSS で切り出して表示する (docs: サムネ + bbox スタイル)。
  // 認証付き object URL を背景画像にし、background-size/position で bbox 領域を拡大表示。
  import { getApi, type Bbox, type IllumiaApi } from '$lib/api';
  import { authedObjectUrl } from '$lib/api/image';

  interface Props {
    assetId: string;
    bbox: Bbox;
    api?: IllumiaApi;
    alt?: string;
  }
  const { assetId, bbox, api = getApi(), alt = '' }: Props = $props();

  let src = $state<string | null>(null);

  $effect(() => {
    const url = api.thumbnailUrl(assetId);
    let alive = true;
    src = null;
    authedObjectUrl(url)
      .then((r) => {
        if (alive) src = r;
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  });

  const clamp = (n: number) => Math.min(1, Math.max(0, n));
  // bbox = [x, y, w, h] (正規化)。領域が要素いっぱいになるよう拡大し、左上位置を合わせる。
  const style = $derived.by(() => {
    const [x, y, w, h] = bbox;
    const sizeW = (100 / Math.max(w, 0.02)).toFixed(2);
    const sizeH = (100 / Math.max(h, 0.02)).toFixed(2);
    const posX = (clamp(x / Math.max(1 - w, 1e-6)) * 100).toFixed(2);
    const posY = (clamp(y / Math.max(1 - h, 1e-6)) * 100).toFixed(2);
    return `background-size:${sizeW}% ${sizeH}%;background-position:${posX}% ${posY}%`;
  });
</script>

<div
  class="face"
  role="img"
  aria-label={alt}
  style={src ? `background-image:url("${src}");${style}` : ''}
></div>

<style>
  .face {
    width: 100%;
    height: 100%;
    background-color: #1c1c22;
    background-repeat: no-repeat;
    display: block;
  }
</style>
