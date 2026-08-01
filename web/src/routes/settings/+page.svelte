<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type AppSettings } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import { isTauri } from '$lib/platform/tauri';
  import { appMode } from '$lib/appMode.svelte';
  import AutoUploadSettings from '$lib/components/AutoUploadSettings.svelte';

  const api = getApi();
  const native = isTauri();

  function reconnect() {
    // 接続プロファイル画面へ戻す (再プローブ / 変更)。
    appMode.status = 'needs-connection';
  }

  let trashDays = $state(30);
  let dedupDays = $state(14);
  let thumbConcurrency = $state(3);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state<string | null>(null);

  function apply(s: AppSettings) {
    trashDays = Number(s['trash.retention_days'] ?? 30);
    dedupDays = Number(s['dedup.retention_days'] ?? 14);
    thumbConcurrency = Number(s['jobs.thumbnail_concurrency'] ?? 3);
  }

  async function load() {
    loading = true;
    error = null;
    try {
      apply(await api.getSettings());
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    saving = true;
    try {
      const updated = await api.patchSettings({
        'trash.retention_days': trashDays,
        'dedup.retention_days': dedupDays,
        'jobs.thumbnail_concurrency': thumbConcurrency
      });
      apply(updated);
      toasts.success('設定を保存しました');
    } catch (err) {
      toasts.error(err instanceof Error ? err.message : '保存に失敗しました');
    } finally {
      saving = false;
    }
  }

  onMount(load);
</script>

<svelte:head><title>設定 - Illumia</title></svelte:head>

<div class="page">
  <h1>設定</h1>
  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else}
    <form onsubmit={save}>
      <label>
        ゴミ箱の保持期間 (日)
        <input type="number" min="0" bind:value={trashDays} />
      </label>
      <label>
        重複の保持期間 (日)
        <input type="number" min="0" bind:value={dedupDays} />
      </label>
      <label>
        サムネイル生成の並列度
        <input type="number" min="1" bind:value={thumbConcurrency} />
      </label>
      <button type="submit" disabled={saving}>{saving ? '保存中…' : '保存'}</button>
    </form>
  {/if}

  {#if native}
    <AutoUploadSettings />
    <section class="conn">
      <h2>サーバー接続 (アプリ)</h2>
      <button class="reconnect" onclick={reconnect}>接続設定を変更 / 再接続</button>
    </section>
  {/if}
</div>

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    padding: 1.5rem;
  }
  h1 {
    margin: 0 0 1rem;
    font-size: 1.4rem;
  }
  .muted {
    color: #a1a1aa;
  }
  .err {
    color: #f87171;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    max-width: 380px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: 0.9rem;
    color: #d4d4d8;
  }
  input {
    padding: 0.55rem 0.7rem;
    border-radius: 8px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 1rem;
  }
  button {
    align-self: flex-start;
    padding: 0.6rem 1.4rem;
    border: none;
    border-radius: 8px;
    background: #6d5bd0;
    color: #fff;
    font-size: 1rem;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .conn {
    margin-top: 2rem;
    padding-top: 1.5rem;
    border-top: 1px solid #26262e;
  }
  .conn h2 {
    margin: 0 0 0.75rem;
    font-size: 1.1rem;
  }
  .reconnect {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }
</style>
