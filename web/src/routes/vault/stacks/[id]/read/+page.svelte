<script lang="ts">
  import { page } from '$app/stores';
  import StackReaderView from '$lib/components/StackReaderView.svelte';
  import { getVaultApi } from '$lib/api/vault';

  const stackId = $derived($page.params.id ?? '');
  const initialPage = $derived(Number($page.url.searchParams.get('page') ?? '1') || 1);
</script>

<svelte:head><title>Vault リーダー - Illumia</title></svelte:head>

<StackReaderView {stackId} {initialPage} api={getVaultApi()} basePath="/vault/stacks" />
