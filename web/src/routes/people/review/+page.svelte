<script lang="ts">
  import { onMount } from 'svelte';
  import { getApi, type Candidate } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import FaceCrop from '$lib/components/FaceCrop.svelte';

  const api = getApi();

  let queue = $state<Candidate[]>([]);
  let pos = $state(0);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state(false);

  const current = $derived(queue[pos] ?? null);

  async function load() {
    loading = true;
    error = null;
    try {
      queue = await api.getReviewCandidates();
      pos = 0;
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  async function decide(action: 'accept' | 'reject') {
    if (!current || busy) return;
    busy = true;
    try {
      await api.reviewCandidate(current.face_id, action);
      pos += 1;
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '更新に失敗しました');
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'y' || e.key === 'Y') decide('accept');
    else if (e.key === 'n' || e.key === 'N') decide('reject');
  }

  onMount(load);
</script>

<svelte:head><title>確認キュー - Illumia</title></svelte:head>
<svelte:window onkeydown={onKeydown} />

<div class="page">
  <div class="head">
    <a class="back" href="/people">← 人物</a>
    <h1>確認キュー</h1>
    {#if !loading && queue.length > 0}
      <span class="muted">{Math.min(pos + 1, queue.length)} / {queue.length}</span>
    {/if}
  </div>

  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else if error}
    <p class="err">{error}</p>
  {:else if !current}
    <p class="muted done">確認する候補はありません。</p>
  {:else}
    <div class="card">
      <div class="crop">
        <FaceCrop {api} assetId={current.asset_id} bbox={current.bbox} alt="候補顔" />
      </div>
      <p class="q">
        {#if current.cluster_name}
          「{current.cluster_name}」と同一人物ですか?
        {:else if current.cluster_id}
          この候補クラスタに追加しますか?
        {:else}
          新しい人物として登録しますか?
        {/if}
      </p>
      {#if current.similarity != null}
        <p class="muted small">類似度: {(current.similarity * 100).toFixed(0)}%</p>
      {/if}
      <div class="btns">
        <button class="reject" disabled={busy} onclick={() => decide('reject')}>
          却下 <kbd>N</kbd>
        </button>
        <button class="accept" disabled={busy} onclick={() => decide('accept')}>
          承認 <kbd>Y</kbd>
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .page {
    height: 100%;
    overflow-y: auto;
    padding: 1.5rem;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 1rem;
    margin-bottom: 1.5rem;
  }
  .back {
    color: #a1a1aa;
    text-decoration: none;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
    flex: 1;
  }
  .muted {
    color: #a1a1aa;
  }
  .small {
    font-size: 0.85rem;
  }
  .err {
    color: #f87171;
  }
  .done {
    text-align: center;
    margin-top: 3rem;
  }
  .card {
    max-width: 360px;
    margin: 0 auto;
    background: #16161c;
    border: 1px solid #26262e;
    border-radius: 12px;
    padding: 1.5rem;
    text-align: center;
  }
  .crop {
    width: 220px;
    height: 220px;
    margin: 0 auto 1rem;
    border-radius: 10px;
    overflow: hidden;
    background: #1c1c22;
  }
  .q {
    font-size: 1.05rem;
    margin: 0 0 0.5rem;
  }
  .btns {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.25rem;
  }
  .btns button {
    flex: 1;
    padding: 0.7rem;
    border-radius: 8px;
    border: none;
    font-size: 1rem;
    cursor: pointer;
    color: #fff;
  }
  .btns button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .reject {
    background: #b3341f;
  }
  .accept {
    background: #16794a;
  }
  kbd {
    background: rgba(255, 255, 255, 0.2);
    border-radius: 4px;
    padding: 1px 6px;
    font-size: 0.8rem;
  }
</style>
