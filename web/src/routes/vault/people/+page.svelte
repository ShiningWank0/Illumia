<script lang="ts">
  import PeopleListView from '$lib/components/PeopleListView.svelte';
  import { getVaultApi } from '$lib/api/vault';
</script>

<svelte:head><title>Vault 人物 - Illumia</title></svelte:head>

<PeopleListView api={getVaultApi()} basePath="/vault/people" reviewPath={null} />
