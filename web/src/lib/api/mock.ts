// 開発用モック。サーバー未完成でも UI を動かせるよう、IllumiaApi と同一
// インタフェースで決定的な擬似データ (縦長 / 横長 / 正方形を混ぜた数千件) を返す。
// VITE_USE_MOCK=1 で選択される (index.ts)。メイン用と vault 用の両方を提供する。

import type { VaultLifecycle } from './vault';
import {
  ApiError,
  type AppSettings,
  type Asset,
  type Bucket,
  type BucketItem,
  type ChapterInput,
  type DuplicatePair,
  type Granularity,
  type IllumiaApi,
  type SearchResult,
  type ServerInfo,
  type StackChapter,
  type StackDetail,
  type StackSummary,
  type UploadResult,
  type VaultStatusResponse,
  type VaultTransfer,
  type VaultUnlockResponse
} from './types';

/** mulberry32: 決定的な擬似乱数 (seed から再現可能)。 */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface MockAsset {
  id: string;
  ratio: number;
  taken_at: string;
  dayKey: string;
  monthKey: string;
  yearKey: string;
  hue: number;
}

const RATIO_BUCKETS = [0.5, 0.66, 0.75, 1.0, 1.33, 1.5, 1.78, 2.0];

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/** 決定的にモックアセットを生成する (taken_at DESC)。 */
function generateAssets(count: number, seed = 0x111a1a, prefix = 'mock'): MockAsset[] {
  const rand = mulberry32(seed);
  const assets: MockAsset[] = [];
  const base = Date.UTC(2026, 6, 30, 12, 0, 0);
  const dayMs = 24 * 60 * 60 * 1000;
  let cursor = base;
  for (let i = 0; i < count; i++) {
    if (rand() < 0.4) cursor -= Math.floor(rand() * 3 * 60 * 60 * 1000);
    else cursor -= Math.floor(1 + rand() * 3) * dayMs;
    const d = new Date(cursor);
    const y = d.getUTCFullYear();
    const m = d.getUTCMonth() + 1;
    const day = d.getUTCDate();
    assets.push({
      id: `${prefix}-${String(i).padStart(5, '0')}`,
      ratio: RATIO_BUCKETS[Math.floor(rand() * RATIO_BUCKETS.length)],
      taken_at: d.toISOString(),
      dayKey: `${y}-${pad2(m)}-${pad2(day)}`,
      monthKey: `${y}-${pad2(m)}`,
      yearKey: String(y),
      hue: Math.floor(rand() * 360)
    });
  }
  return assets;
}

function keyOf(a: MockAsset, g: Granularity): string {
  return g === 'day' ? a.dayKey : g === 'month' ? a.monthKey : a.yearKey;
}

function svgDataUri(a: MockAsset, size: number): string {
  const w = a.ratio >= 1 ? size : Math.round(size * a.ratio);
  const h = a.ratio >= 1 ? Math.round(size / a.ratio) : size;
  const bg = `hsl(${a.hue} 55% 45%)`;
  const fg = `hsl(${a.hue} 55% 85%)`;
  const svg =
    `<svg xmlns='http://www.w3.org/2000/svg' width='${w}' height='${h}' viewBox='0 0 ${w} ${h}'>` +
    `<rect width='100%' height='100%' fill='${bg}'/>` +
    `<text x='50%' y='50%' fill='${fg}' font-family='sans-serif' font-size='${Math.round(h / 8)}' ` +
    `text-anchor='middle' dominant-baseline='middle'>${a.id}</text>` +
    `</svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

function toAsset(a: MockAsset): Asset {
  const width = 1000;
  return {
    id: a.id,
    filename: `${a.id}.png`,
    width,
    height: Math.max(1, Math.round(width / a.ratio)),
    ratio: a.ratio,
    thumbhash: null,
    taken_at: a.taken_at,
    created_at: a.taken_at,
    status: 'created'
  };
}

/** 疑似ネットワーク遅延。 */
const delay = <T>(value: T): Promise<T> =>
  new Promise((resolve) => setTimeout(() => resolve(value), 30 + Math.random() * 60));

let mockStackSeq = 1;

/** タイムライン / 画像 URL 系メソッド (assets 配列に対して閉じる)。 */
function makeAssetViews(assets: MockAsset[]) {
  const byId = new Map(assets.map((a) => [a.id, a]));
  return {
    byId,
    async getBuckets(g: Granularity): Promise<Bucket[]> {
      const counts = new Map<string, number>();
      for (const a of assets) {
        const k = keyOf(a, g);
        counts.set(k, (counts.get(k) ?? 0) + 1);
      }
      const buckets = [...counts.entries()]
        .map(([key, c]) => ({ key, count: c }))
        .sort((x, y) => (x.key < y.key ? 1 : x.key > y.key ? -1 : 0));
      return delay(buckets);
    },
    async getBucketItems(g: Granularity, key: string): Promise<BucketItem[]> {
      const items: BucketItem[] = assets
        .filter((a) => keyOf(a, g) === key)
        .map((a) => ({ id: a.id, ratio: a.ratio, thumbhash: null, taken_at: a.taken_at }));
      return delay(items);
    },
    thumbnailUrl(id: string): string {
      const a = byId.get(id);
      return a ? svgDataUri(a, 240) : '';
    },
    previewUrl(id: string): string {
      const a = byId.get(id);
      return a ? svgDataUri(a, 1440) : '';
    },
    originalUrl(id: string): string {
      const a = byId.get(id);
      return a ? svgDataUri(a, 1440) : '';
    }
  };
}

/** スタック CRUD (stacks 配列に対して閉じる)。 */
function makeStackMethods(stacks: StackDetail[], assetFor: (id: string) => Asset) {
  const findStack = (id: string) => stacks.find((s) => s.id === id);
  const summaryOf = (s: StackDetail): StackSummary => ({
    id: s.id,
    title: s.title,
    cover_asset_id: s.cover_asset_id,
    chapter_count: s.chapters.length,
    page_count: s.chapters.reduce((n, c) => n + c.pages.length, 0),
    created_at: s.created_at,
    updated_at: s.updated_at
  });
  const touch = (s: StackDetail) => {
    s.updated_at = new Date().toISOString();
  };

  return {
    summaryOf,
    async listStacks(): Promise<StackSummary[]> {
      return delay(
        [...stacks].sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1)).map(summaryOf)
      );
    },
    async createStack(title: string, assetIds: string[]): Promise<StackDetail> {
      const now = new Date().toISOString();
      const id = `mock-stack-${mockStackSeq++}`;
      const stack: StackDetail = {
        id,
        title,
        cover_asset_id: assetIds[0] ?? null,
        created_at: now,
        updated_at: now,
        chapters: [
          {
            id: `${id}-c1`,
            chapter_no: 1,
            title: null,
            pages: assetIds.map((aid, i) => ({
              page_no: i + 1,
              show_in_timeline: false,
              asset: assetFor(aid)
            }))
          } satisfies StackChapter
        ]
      };
      stacks.unshift(stack);
      return delay(structuredClone(stack));
    },
    async getStack(id: string): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      return delay(structuredClone(s));
    },
    async patchStack(
      id: string,
      patch: { title?: string; cover_asset_id?: string }
    ): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      if (patch.title !== undefined) s.title = patch.title;
      if (patch.cover_asset_id !== undefined) s.cover_asset_id = patch.cover_asset_id;
      touch(s);
      return delay(structuredClone(s));
    },
    async deleteStack(id: string): Promise<void> {
      const i = stacks.findIndex((s) => s.id === id);
      if (i >= 0) stacks.splice(i, 1);
      return delay(undefined);
    },
    async replaceStructure(id: string, chapters: ChapterInput[]): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      const flags = new Map<string, boolean>();
      for (const c of s.chapters)
        for (const p of c.pages) flags.set(p.asset.id, p.show_in_timeline);
      s.chapters = chapters.map((c, ci) => ({
        id: `${id}-c${ci + 1}`,
        chapter_no: ci + 1,
        title: c.title,
        pages: c.pages.map((aid, pi) => ({
          page_no: pi + 1,
          show_in_timeline: flags.get(aid) ?? false,
          asset: assetFor(aid)
        }))
      }));
      const allIds = new Set(chapters.flatMap((c) => c.pages));
      if (!s.cover_asset_id || !allIds.has(s.cover_asset_id)) {
        s.cover_asset_id = chapters[0]?.pages[0] ?? null;
      }
      touch(s);
      return delay(structuredClone(s));
    },
    async addStackPages(id: string, assetIds: string[], chapterId?: string): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      const chapter = chapterId
        ? s.chapters.find((c) => c.id === chapterId)
        : s.chapters[s.chapters.length - 1];
      if (!chapter) throw new ApiError(404, 'not_found', 'chapter not found');
      let no = chapter.pages.length;
      for (const aid of assetIds)
        chapter.pages.push({ page_no: ++no, show_in_timeline: false, asset: assetFor(aid) });
      s.cover_asset_id = s.cover_asset_id ?? assetIds[0] ?? null;
      touch(s);
      return delay(structuredClone(s));
    },
    async removeStackPage(id: string, assetId: string): Promise<void> {
      const s = findStack(id);
      if (!s) return delay(undefined);
      for (const c of s.chapters) c.pages = c.pages.filter((p) => p.asset.id !== assetId);
      for (const c of s.chapters) c.pages.forEach((p, i) => (p.page_no = i + 1));
      touch(s);
      return delay(undefined);
    },
    async setPageFlag(id: string, assetId: string, showInTimeline: boolean): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      for (const c of s.chapters)
        for (const p of c.pages) if (p.asset.id === assetId) p.show_in_timeline = showInTimeline;
      touch(s);
      return delay(structuredClone(s));
    }
  };
}

function assetForFrom(byId: Map<string, MockAsset>): (id: string) => Asset {
  return (id: string) => {
    const m = byId.get(id);
    if (m) return toAsset(m);
    const now = new Date().toISOString();
    return {
      id,
      filename: `${id}.png`,
      width: 1000,
      height: 1000,
      ratio: 1,
      thumbhash: null,
      taken_at: now,
      created_at: now,
      status: 'created'
    };
  };
}

function searchOver(
  assets: MockAsset[],
  stacks: StackDetail[],
  summaryOf: (s: StackDetail) => StackSummary,
  q: string
): SearchResult {
  const query = q.trim().toLowerCase();
  if (query === '') return { assets: [], stacks: [], clusters: [] };
  return {
    assets: assets
      .filter((a) => a.id.toLowerCase().includes(query))
      .slice(0, 50)
      .map(toAsset),
    stacks: stacks.filter((s) => s.title.toLowerCase().includes(query)).map(summaryOf),
    clusters: []
  };
}

/** メイン用モック IllumiaApi。 */
export function createMockClient(count = 3000): IllumiaApi {
  const assets = generateAssets(count);
  const views = makeAssetViews(assets);
  const stacks: StackDetail[] = [];
  const stackMethods = makeStackMethods(stacks, assetForFrom(views.byId));
  const settings: AppSettings = {
    'trash.retention_days': 30,
    'dedup.retention_days': 14,
    'jobs.thumbnail_concurrency': 3,
    'jobs.ml_concurrency': 1
  };

  return {
    async serverInfo(): Promise<ServerInfo> {
      return delay({
        version: 'mock',
        setup_completed: true,
        authenticated: true,
        setup_token_required: false
      });
    },
    async setup(): Promise<void> {
      return delay(undefined);
    },
    async login(): Promise<void> {
      return delay(undefined);
    },
    async logout(): Promise<void> {
      return delay(undefined);
    },

    getBuckets: views.getBuckets,
    getBucketItems: views.getBucketItems,
    thumbnailUrl: views.thumbnailUrl,
    previewUrl: views.previewUrl,
    originalUrl: views.originalUrl,

    async uploadAsset(): Promise<UploadResult> {
      return delay({ id: 'mock-upload', status: 'created' });
    },
    async trashAsset(): Promise<void> {
      return delay(undefined);
    },
    async restoreAsset(): Promise<void> {
      return delay(undefined);
    },
    async getTrash(): Promise<Asset[]> {
      return delay([]);
    },
    async getDuplicates(): Promise<DuplicatePair[]> {
      return delay([]);
    },
    async purgeNow(): Promise<void> {
      return delay(undefined);
    },
    async getSettings(): Promise<AppSettings> {
      return delay({ ...settings });
    },
    async patchSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
      Object.assign(settings, patch);
      return delay({ ...settings });
    },

    listStacks: stackMethods.listStacks,
    createStack: stackMethods.createStack,
    getStack: stackMethods.getStack,
    patchStack: stackMethods.patchStack,
    deleteStack: stackMethods.deleteStack,
    replaceStructure: stackMethods.replaceStructure,
    addStackPages: stackMethods.addStackPages,
    removeStackPage: stackMethods.removeStackPage,
    setPageFlag: stackMethods.setPageFlag,

    async search(q: string): Promise<SearchResult> {
      return delay(searchOver(assets, stacks, stackMethods.summaryOf, q));
    }
  };
}

// ---- Vault モック ----

interface MockVault {
  initialized: boolean;
  assets: MockAsset[];
  stacks: StackDetail[];
  trashed: MockAsset[];
}

const mockVault: MockVault = { initialized: false, assets: [], stacks: [], trashed: [] };

function seedVault() {
  if (mockVault.assets.length === 0) {
    const seeded = generateAssets(180, 0x5eed11, 'vault');
    // 末尾 2 件をゴミ箱に入れて復元/完全削除を試せるようにする。
    mockVault.trashed = seeded.slice(0, 2);
    mockVault.assets = seeded.slice(2);
  }
}

export const mockVaultLifecycle: VaultLifecycle = {
  async status(): Promise<VaultStatusResponse> {
    return delay({ initialized: mockVault.initialized, unlocked: false });
  },
  async init(): Promise<{ recovery_key: string }> {
    mockVault.initialized = true;
    seedVault();
    return delay({ recovery_key: 'MOCK-ABCD-EFGH-IJKL-MNOP-QRST' });
  },
  async unlock(): Promise<VaultUnlockResponse> {
    if (!mockVault.initialized) throw new ApiError(404, 'not_found', 'not found');
    seedVault();
    const expires = new Date(Date.now() + 15 * 60 * 1000).toISOString();
    return delay({ vault_session: 'mock-vault-session', expires_at: expires });
  },
  async lock(): Promise<void> {
    return delay(undefined);
  },
  async importItems(payload: VaultTransfer): Promise<void> {
    // メインから移動された想定で vault に擬似アセットを追加する。
    const ids = payload.asset_ids ?? [];
    const now = new Date();
    ids.forEach((id, i) => {
      const d = new Date(now.getTime() - i * 1000);
      mockVault.assets.unshift({
        id,
        ratio: RATIO_BUCKETS[i % RATIO_BUCKETS.length],
        taken_at: d.toISOString(),
        dayKey: `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`,
        monthKey: `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}`,
        yearKey: String(d.getUTCFullYear()),
        hue: (i * 47) % 360
      });
    });
    return delay(undefined);
  },
  async exportItems(payload: VaultTransfer): Promise<void> {
    const ids = new Set(payload.asset_ids ?? []);
    mockVault.assets = mockVault.assets.filter((a) => !ids.has(a.id));
    return delay(undefined);
  }
};

/** Vault 用モック IllumiaApi (ミラー。未対応操作は投げる)。 */
export function createMockVaultClient(): IllumiaApi {
  seedVault();
  const views = makeAssetViews(mockVault.assets);
  const stackMethods = makeStackMethods(mockVault.stacks, assetForFrom(views.byId));
  const nope = (name: string): never => {
    throw new ApiError(0, 'unsupported', `${name} not available in vault`);
  };

  return {
    serverInfo: () => nope('serverInfo'),
    setup: () => nope('setup'),
    login: () => nope('login'),
    logout: () => nope('logout'),

    // vault.assets は import で増減するため都度読み直す。
    async getBuckets(g: Granularity): Promise<Bucket[]> {
      return makeAssetViews(mockVault.assets).getBuckets(g);
    },
    async getBucketItems(g: Granularity, key: string): Promise<BucketItem[]> {
      return makeAssetViews(mockVault.assets).getBucketItems(g, key);
    },
    thumbnailUrl(id: string): string {
      return makeAssetViews(mockVault.assets).thumbnailUrl(id);
    },
    previewUrl(id: string): string {
      return makeAssetViews(mockVault.assets).previewUrl(id);
    },
    originalUrl(id: string): string {
      return makeAssetViews(mockVault.assets).originalUrl(id);
    },

    uploadAsset: () => nope('uploadAsset'),
    async trashAsset(id: string): Promise<void> {
      const i = mockVault.assets.findIndex((a) => a.id === id);
      if (i >= 0) mockVault.trashed.unshift(...mockVault.assets.splice(i, 1));
      return delay(undefined);
    },
    async restoreAsset(id: string): Promise<void> {
      const i = mockVault.trashed.findIndex((a) => a.id === id);
      if (i >= 0) mockVault.assets.unshift(...mockVault.trashed.splice(i, 1));
      return delay(undefined);
    },
    async getTrash(): Promise<Asset[]> {
      const purgeAfter = new Date(Date.now() + 30 * 86400_000).toISOString();
      return delay(
        mockVault.trashed.map((a) => ({
          ...toAsset(a),
          status: 'trashed',
          trashed_at: new Date().toISOString(),
          purge_after: purgeAfter
        }))
      );
    },
    async getDuplicates(): Promise<DuplicatePair[]> {
      return delay([]);
    },
    async purgeNow(id: string): Promise<void> {
      mockVault.trashed = mockVault.trashed.filter((a) => a.id !== id);
      return delay(undefined);
    },
    getSettings: () => nope('getSettings'),
    patchSettings: () => nope('patchSettings'),

    listStacks: stackMethods.listStacks,
    createStack: stackMethods.createStack,
    getStack: stackMethods.getStack,
    patchStack: stackMethods.patchStack,
    deleteStack: stackMethods.deleteStack,
    replaceStructure: stackMethods.replaceStructure,
    addStackPages: stackMethods.addStackPages,
    removeStackPage: stackMethods.removeStackPage,
    setPageFlag: stackMethods.setPageFlag,

    async search(q: string): Promise<SearchResult> {
      return delay(searchOver(mockVault.assets, mockVault.stacks, stackMethods.summaryOf, q));
    }
  };
}
