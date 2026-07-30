<script lang="ts">
  import { goto } from '$app/navigation';
  import Timeline from '$lib/components/Timeline.svelte';
  import { vaultSession } from '$lib/vaultSession.svelte';
  import { getVaultLifecycle } from '$lib/api/vault';

  async function importToVault(ids: string[]) {
    await getVaultLifecycle().importItems({ asset_ids: ids });
  }
</script>

<svelte:head>
  <title>Illumia</title>
  <meta name="description" content="アニメ・2次元イラスト特化のセルフホスト画像閲覧アプリ" />
</svelte:head>

<Timeline
  mode="main"
  stacksBase="/stacks"
  onImportToVault={vaultSession.status === 'unlocked' ? importToVault : undefined}
  onVaultLocked={() => goto('/vault')}
/>
