<script lang="ts">
  import { appName, getWelcomeMessage } from '$lib/app';
</script>

<svelte:head>
  <title>{appName}</title>
  <meta name="description" content="アニメ・2次元イラスト特化のセルフホスト画像閲覧アプリ" />
</svelte:head>

<main>
  <section aria-labelledby="page-title">
    <p class="eyebrow">{appName}</p>
    <h1 id="page-title">{getWelcomeMessage(appName)}</h1>
    <p>アニメ・2次元イラストに特化した画像ライブラリを、ここから閲覧できます。</p>
  </section>
</main>

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
    color-scheme: light dark;
    background: #101116;
    color: #f4f4f5;
  }

  :global(body) {
    margin: 0;
  }

  main {
    display: grid;
    min-height: 100vh;
    place-items: center;
    padding: 2rem;
  }

  section {
    max-width: 42rem;
  }

  .eyebrow {
    margin: 0 0 0.75rem;
    color: #c4b5fd;
    font-size: 0.875rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  h1 {
    margin: 0;
    font-size: clamp(2rem, 7vw, 4rem);
    line-height: 1.05;
  }

  h1 + p {
    margin: 1.25rem 0 0;
    color: #d4d4d8;
    font-size: 1.125rem;
    line-height: 1.7;
  }
</style>
