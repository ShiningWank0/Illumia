<script lang="ts">
  import PeopleListView from '$lib/components/PeopleListView.svelte';
</script>

<svelte:head><title>人物 - Illumia</title></svelte:head>

<PeopleListView basePath="/people" reviewPath="/people/review" />
