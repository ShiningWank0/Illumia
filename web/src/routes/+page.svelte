<script lang="ts">
  import { appName } from '$lib/app';
  import Timeline from '$lib/components/Timeline.svelte';
</script>

<svelte:head>
  <title>{appName}</title>
  <meta name="description" content="アニメ・2次元イラスト特化のセルフホスト画像閲覧アプリ" />
</svelte:head>

<Timeline />

<style>
  :global(*) {
    box-sizing: border-box;
  }

  :global(html) {
    font-family:
      Inter,
      ui-sans-serif,
      system-ui,
      -apple-system,
      BlinkMacSystemFont,
      'Segoe UI',
      sans-serif;
    color-scheme: dark;
    background: #101116;
    color: #f4f4f5;
  }

  :global(body) {
    margin: 0;
  }
</style>
