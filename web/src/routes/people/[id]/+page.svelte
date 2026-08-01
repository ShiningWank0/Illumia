<script lang="ts">
  import { page } from '$app/stores';
  import ClusterDetailView from '$lib/components/ClusterDetailView.svelte';

  const clusterId = $derived($page.params.id ?? '');
</script>

<svelte:head><title>人物 - Illumia</title></svelte:head>

<ClusterDetailView {clusterId} basePath="/people" />
