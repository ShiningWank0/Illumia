<script lang="ts">
  import { page } from '$app/stores';
  import SearchView from '$lib/components/SearchView.svelte';
  import { getVaultApi } from '$lib/api/vault';

  const query = $derived($page.url.searchParams.get('q') ?? '');
</script>

<svelte:head><title>Vault 検索: {query} - Illumia</title></svelte:head>

<SearchView {query} api={getVaultApi()} basePath="/vault/stacks" peopleBase="/vault/people" />
