<script lang="ts">
  import { page } from '$app/stores';
  import SearchView from '$lib/components/SearchView.svelte';

  const query = $derived($page.url.searchParams.get('q') ?? '');
</script>

<svelte:head><title>検索: {query} - Illumia</title></svelte:head>

<SearchView {query} basePath="/stacks" peopleBase="/people" />
