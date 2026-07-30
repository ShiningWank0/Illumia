<script lang="ts">
  import StackListView from '$lib/components/StackListView.svelte';
  import { getVaultApi } from '$lib/api/vault';
</script>

<svelte:head><title>Vault 漫画 - Illumia</title></svelte:head>

<StackListView api={getVaultApi()} basePath="/vault/stacks" />
