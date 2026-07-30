<script lang="ts">
  import DuplicatesView from '$lib/components/DuplicatesView.svelte';
  import { getVaultApi } from '$lib/api/vault';
</script>

<svelte:head><title>Vault 重複 - Illumia</title></svelte:head>

<DuplicatesView api={getVaultApi()} mode="vault" />
