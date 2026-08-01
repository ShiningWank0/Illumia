<script lang="ts">
  // ML 設定セクション (docs/07 / docs/02 settings キー)。
  import { onMount, onDestroy } from 'svelte';
  import { getApi, type AppSettings, type Job, type MlStatus } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';

  const api = getApi();

  let status = $state<MlStatus | null>(null);
  let enabled = $state(true);
  let tauHigh = $state<string>('');
  let tauLow = $state<string>('');
  let minCluster = $state(4);
  let qualityGate = $state('review_only');
  let loading = $state(true);
  let saving = $state(false);
  let error = $state<string | null>(null);

  let jobs = $state<Job[]>([]);
  let poll: ReturnType<typeof setInterval> | undefined;

  function applySettings(s: AppSettings) {
    enabled = Boolean(s['ml.enabled'] ?? true);
    tauHigh = s['ml.tau_high_override'] == null ? '' : String(s['ml.tau_high_override']);
    tauLow = s['ml.tau_low_override'] == null ? '' : String(s['ml.tau_low_override']);
    minCluster = Number(s['ml.min_cluster_size'] ?? 4);
    qualityGate = String(s['ml.quality_gate'] ?? 'review_only');
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const [st, settings] = await Promise.all([
        api.mlStatus().catch(() => null),
        api.getSettings()
      ]);
      status = st;
      applySettings(settings);
    } catch (e) {
      error = e instanceof Error ? e.message : '取得に失敗しました';
    } finally {
      loading = false;
    }
  }

  function numOrNull(v: string): number | null {
    const t = v.trim();
    if (t === '') return null;
    const n = Number(t);
    return Number.isFinite(n) ? n : null;
  }

  async function save() {
    saving = true;
    try {
      await api.patchSettings({
        'ml.enabled': enabled,
        'ml.tau_high_override': numOrNull(tauHigh),
        'ml.tau_low_override': numOrNull(tauLow),
        'ml.min_cluster_size': minCluster,
        'ml.quality_gate': qualityGate
      });
      toasts.success('ML 設定を保存しました');
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '保存に失敗しました');
    } finally {
      saving = false;
    }
  }

  async function refreshJobs() {
    try {
      const all = await api.getJobs();
      jobs = all.filter((j) => j.kind.startsWith('ml_'));
    } catch {
      // ジョブ取得失敗は無視 (WS/ポーリングの縮退)。
    }
  }

  async function analyzeAll() {
    try {
      await api.analyzeAll();
      toasts.success('全アセットの解析を開始しました');
      refreshJobs();
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '開始に失敗しました');
    }
  }
  async function recluster() {
    try {
      await api.recluster();
      toasts.success('再クラスタリングを開始しました');
      refreshJobs();
    } catch (e) {
      toasts.error(e instanceof Error ? e.message : '開始に失敗しました');
    }
  }

  onMount(() => {
    load();
    refreshJobs();
    poll = setInterval(refreshJobs, 1500);
  });
  onDestroy(() => {
    if (poll) clearInterval(poll);
  });
</script>

<section class="ml">
  <h2>機械学習 (人物クラスタリング)</h2>

  {#if loading}
    <p class="muted">読み込み中…</p>
  {:else}
    {#if error}<p class="err">{error}</p>{/if}

    {#if status?.backend === 'mock' || !status?.model_ready}
      <div class="banner">
        モデル未設定 — サイドカーは mock バックエンドです。実モデルの導入は
        <strong>docs/13</strong> を参照してください。
      </div>
    {/if}

    {#if status}
      <p class="muted small">
        backend: <code>{status.backend}</code>
        {#if status.bundle_version}/ bundle: {status.bundle_version}{/if}
      </p>
    {/if}

    <label class="row check">
      <input type="checkbox" bind:checked={enabled} />
      ML 機能を有効にする (ml.enabled)
    </label>
    <label class="row">
      tau_high override (空 = 既定)
      <input type="number" step="0.01" min="0" max="1" bind:value={tauHigh} />
    </label>
    <label class="row">
      tau_low override (空 = 既定)
      <input type="number" step="0.01" min="0" max="1" bind:value={tauLow} />
    </label>
    <label class="row">
      min_cluster_size
      <input type="number" min="1" bind:value={minCluster} />
    </label>
    <label class="row">
      quality_gate
      <select bind:value={qualityGate}>
        <option value="review_only">review_only</option>
        <option value="strict">strict</option>
      </select>
    </label>

    <div class="save-row">
      <button class="primary" disabled={saving} onclick={save}>{saving ? '保存中…' : '保存'}</button
      >
    </div>

    <div class="jobs-actions">
      <button onclick={analyzeAll}>全アセットを解析</button>
      <button onclick={recluster}>再クラスタリング</button>
    </div>

    {#if jobs.length > 0}
      <ul class="jobs">
        {#each jobs as j (j.id)}
          <li>
            <span class="jkind">{j.kind}</span>
            <div class="bar">
              <div class="fill" style="width:{Math.round(j.progress * 100)}%"></div>
            </div>
            <span class="muted small">{j.state} {Math.round(j.progress * 100)}%</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .ml {
    margin-top: 2rem;
    padding-top: 1.5rem;
    border-top: 1px solid #26262e;
    max-width: 480px;
  }
  h2 {
    margin: 0 0 0.75rem;
    font-size: 1.1rem;
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
  .banner {
    background: rgba(252, 211, 77, 0.12);
    border: 1px solid #7c5e12;
    color: #fcd34d;
    border-radius: 8px;
    padding: 0.6rem 0.8rem;
    font-size: 0.85rem;
    margin-bottom: 0.75rem;
  }
  code {
    background: #0c0c10;
    padding: 1px 5px;
    border-radius: 4px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin: 0.75rem 0;
    font-size: 0.85rem;
    color: #d4d4d8;
  }
  .row.check {
    flex-direction: row;
    align-items: center;
    gap: 0.5rem;
  }
  input[type='number'],
  select {
    padding: 0.5rem 0.6rem;
    border-radius: 8px;
    border: 1px solid #3f3f46;
    background: #101116;
    color: #f4f4f5;
    font-size: 1rem;
  }
  .save-row {
    margin: 0.5rem 0 1rem;
  }
  button {
    border: 1px solid #3f3f46;
    background: none;
    color: #f4f4f5;
    padding: 0.5rem 1rem;
    border-radius: 8px;
    cursor: pointer;
  }
  button.primary {
    background: #6d5bd0;
    border-color: #6d5bd0;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .jobs-actions {
    display: flex;
    gap: 0.5rem;
  }
  .jobs {
    list-style: none;
    margin: 1rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .jobs li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-size: 0.85rem;
  }
  .jkind {
    min-width: 6rem;
  }
  .bar {
    flex: 1;
    height: 8px;
    background: #26262e;
    border-radius: 4px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: #6d5bd0;
    transition: width 0.3s;
  }
</style>
