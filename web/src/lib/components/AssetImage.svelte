<script lang="ts">
  // 認証付き画像 + thumbhash プレースホルダ。
  // プレースホルダ (thumbhash data URL or 色) を背景に敷き、実画像を
  // 認証付き fetch で取得して重ねる。docs/04: thumbhash → 240px サムネの順。
  import { getApi } from '$lib/api';
  import { authedObjectUrl, thumbhashToDataUrl } from '$lib/api/image';

  interface Props {
    id: string;
    variant?: 'thumbnail' | 'preview';
    thumbhash?: string | null;
    alt?: string;
    /** object-fit。タイルは cover、ビューアは contain。 */
    fit?: 'cover' | 'contain';
    /** true なら可視域近傍まで読み込みを遅延する (縦読みリーダー等)。 */
    lazy?: boolean;
  }

  const {
    id,
    variant = 'thumbnail',
    thumbhash = null,
    alt = '',
    fit = 'cover',
    lazy = false
  }: Props = $props();
  const api = getApi();

  const placeholder = $derived(thumbhashToDataUrl(thumbhash));
  const placeholderColor = $derived.by(() => {
    let h = 0;
    for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) % 360;
    return `hsl(${h} 25% 22%)`;
  });

  const lazyProp = $derived(lazy);
  let frame: HTMLElement | undefined;
  let visible = $state(false);
  let src = $state<string | null>(null);
  let failed = $state(false);

  // 非 lazy は即座に可視扱い。
  $effect(() => {
    if (!lazyProp) visible = true;
  });

  // lazy 時は IntersectionObserver で可視域近傍に来たら読み込む。
  $effect(() => {
    if (!lazyProp || visible || !frame) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          visible = true;
          observer.disconnect();
        }
      },
      { rootMargin: '400px' }
    );
    observer.observe(frame);
    return () => observer.disconnect();
  });

  // id / variant が変わるたびに読み込み直す。alive フラグで競合を無視する。
  $effect(() => {
    if (!visible) return;
    const url = variant === 'preview' ? api.previewUrl(id) : api.thumbnailUrl(id);
    let alive = true;
    src = null;
    failed = false;
    authedObjectUrl(url)
      .then((resolved) => {
        if (alive) src = resolved;
      })
      .catch(() => {
        if (alive) failed = true;
      });
    return () => {
      alive = false;
    };
  });
</script>

<div
  bind:this={frame}
  class="frame"
  style="background-image: {placeholder
    ? `url(${placeholder})`
    : 'none'}; background-color: {placeholderColor}"
>
  {#if src}
    <img {src} {alt} style="object-fit: {fit}" draggable="false" />
  {:else if failed}
    <span class="broken" aria-label="読み込み失敗">⚠</span>
  {/if}
</div>

<style>
  .frame {
    width: 100%;
    height: 100%;
    background-size: cover;
    background-position: center;
    overflow: hidden;
    display: block;
  }
  img {
    width: 100%;
    height: 100%;
    display: block;
  }
  .broken {
    display: grid;
    place-items: center;
    width: 100%;
    height: 100%;
    color: #a1a1aa;
    font-size: 1.5rem;
  }
</style>
