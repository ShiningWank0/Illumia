<script lang="ts">
  // 自動アップロード設定 (アプリモードのみ)。フォルダ選択 + 手動同期。
  import { autoUpload } from '$lib/platform/autoUpload.svelte';
</script>

<section class="auto">
  <h2>自動アップロード (アプリ)</h2>
  <p class="muted small">
    起動中のフォアグラウンド同期です。対象フォルダの新規画像を、ハッシュ照合で重複を
    避けてアップロードします。バックグラウンド常駐は今後対応します。
  </p>

  <div class="folders">
    {#if autoUpload.folders.length === 0}
      <p class="muted small">対象フォルダが未設定です。</p>
    {:else}
      <ul>
        {#each autoUpload.folders as f (f)}
          <li>
            <span class="path">{f}</span>
            <button class="rm" onclick={() => autoUpload.removeFolder(f)}>削除</button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="actions">
    <button onclick={() => autoUpload.addFolder()}>フォルダを追加</button>
    <button class="primary" disabled={autoUpload.syncing} onclick={() => autoUpload.syncNow()}>
      {autoUpload.syncing ? '同期中…' : '今すぐ同期'}
    </button>
  </div>

  {#if autoUpload.summary}<p class="ok small">{autoUpload.summary}</p>{/if}
  {#if autoUpload.error}<p class="err small">{autoUpload.error}</p>{/if}
</section>

<style>
  .auto {
    margin-top: 2rem;
    padding-top: 1.5rem;
    border-top: 1px solid #26262e;
    max-width: 480px;
  }
  h2 {
    margin: 0 0 0.5rem;
    font-size: 1.1rem;
  }
  .muted {
    color: #a1a1aa;
  }
  .small {
    font-size: 0.85rem;
  }
  .folders ul {
    list-style: none;
    margin: 0.75rem 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .folders li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 7px;
    padding: 0.4rem 0.6rem;
  }
  .path {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.85rem;
  }
  .rm {
    border: 1px solid #7f1d1d;
    background: none;
    color: #fca5a5;
    padding: 0.25rem 0.6rem;
    border-radius: 6px;
    cursor: pointer;
    font-size: 0.8rem;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  .actions button {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }
  .actions button.primary {
    background: #6d5bd0;
    border-color: #6d5bd0;
  }
  .actions button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .ok {
    color: #86efac;
  }
  .err {
    color: #f87171;
  }
</style>
