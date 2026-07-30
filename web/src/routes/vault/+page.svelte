<script lang="ts">
  import Timeline from '$lib/components/Timeline.svelte';
  import { getVaultApi } from '$lib/api/vault';
  import { getVaultLifecycle } from '$lib/api/vault';

  const vaultApi = getVaultApi();

  async function exportFromVault(ids: string[]) {
    await getVaultLifecycle().exportItems({ asset_ids: ids });
  }
</script>

<svelte:head><title>Vault - Illumia</title></svelte:head>

<Timeline
  api={vaultApi}
  mode="vault"
  stacksBase="/vault/stacks"
  onExportFromVault={exportFromVault}
/>
