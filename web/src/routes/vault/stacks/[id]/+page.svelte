<script lang="ts">
  import { page } from '$app/stores';
  import StackDetailView from '$lib/components/StackDetailView.svelte';
  import { getVaultApi, getVaultLifecycle } from '$lib/api/vault';

  const stackId = $derived($page.params.id ?? '');

  async function exportStack(id: string) {
    await getVaultLifecycle().exportItems({ stack_id: id });
  }
</script>

<svelte:head><title>Vault 漫画 - Illumia</title></svelte:head>

<StackDetailView
  {stackId}
  api={getVaultApi()}
  basePath="/vault/stacks"
  onMoveStack={exportStack}
  moveLabel="Vault から出す"
  moveDoneMessage="スタックを Vault から出しました"
/>
