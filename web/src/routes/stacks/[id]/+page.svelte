<script lang="ts">
  import { page } from '$app/stores';
  import StackDetailView from '$lib/components/StackDetailView.svelte';
  import { vaultSession } from '$lib/vaultSession.svelte';
  import { getVaultLifecycle } from '$lib/api/vault';

  const stackId = $derived($page.params.id ?? '');

  async function moveStackToVault(id: string) {
    await getVaultLifecycle().importItems({ stack_id: id });
  }
</script>

<svelte:head><title>漫画スタック - Illumia</title></svelte:head>

<StackDetailView
  {stackId}
  basePath="/stacks"
  onMoveStack={vaultSession.status === 'unlocked' ? moveStackToVault : undefined}
  moveLabel="Vault へ移動"
  moveDoneMessage="スタックを Vault へ移動しました"
/>
