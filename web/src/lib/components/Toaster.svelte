<script lang="ts">
  import { toasts } from '$lib/toast.svelte';
</script>

<div class="toaster" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <button class="toast {t.kind}" onclick={() => toasts.dismiss(t.id)}>{t.message}</button>
  {/each}
</div>

<style>
  .toaster {
    position: fixed;
    bottom: 16px;
    right: 16px;
    z-index: 200;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: min(90vw, 380px);
  }
  .toast {
    text-align: left;
    border: none;
    border-radius: 8px;
    padding: 10px 14px;
    color: #fff;
    font-size: 0.9rem;
    cursor: pointer;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
    background: #3f3f46;
  }
  .toast.success {
    background: #16794a;
  }
  .toast.error {
    background: #b3341f;
  }
</style>
