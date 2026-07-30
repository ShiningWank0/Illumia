<script lang="ts">
  import TrashView from '$lib/components/TrashView.svelte';
  import { getVaultApi } from '$lib/api/vault';
</script>

<svelte:head><title>Vault ゴミ箱 - Illumia</title></svelte:head>

<TrashView api={getVaultApi()} mode="vault" />
