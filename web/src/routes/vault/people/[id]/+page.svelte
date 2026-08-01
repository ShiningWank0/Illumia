<script lang="ts">
  import { page } from '$app/stores';
  import ClusterDetailView from '$lib/components/ClusterDetailView.svelte';
  import { getVaultApi } from '$lib/api/vault';

  const clusterId = $derived($page.params.id ?? '');
</script>

<svelte:head><title>Vault 人物 - Illumia</title></svelte:head>

<ClusterDetailView {clusterId} api={getVaultApi()} basePath="/vault/people" />
