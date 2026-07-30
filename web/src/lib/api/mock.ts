// 開発用モック。サーバー未完成でも UI を動かせるよう、IllumiaApi と同一
// インタフェースで決定的な擬似データ (縦長 / 横長 / 正方形を混ぜた数千件) を返す。
// VITE_USE_MOCK=1 で選択される (index.ts)。認証・ゴミ箱・重複・設定はスタブ。

import {
  ApiError,
  type AppSettings,
  type Asset,
  type AuthRequest,
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
  type TokenResponse,
  type UploadResult
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
  taken_at: string; // ISO
  dayKey: string; // YYYY-MM-DD
  monthKey: string; // YYYY-MM
  yearKey: string; // YYYY
  hue: number; // プレースホルダ色
}

/** アスペクト比の候補 (縦長 / 正方形 / 横長)。 */
const RATIO_BUCKETS = [
  0.5,
  0.66,
  0.75, // 縦長
  1.0, // 正方形
  1.33,
  1.5,
  1.78,
  2.0 // 横長
];

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/**
 * 決定的にモックアセットを生成する。
 * 直近 base 日から遡って count 件を、taken_at DESC で並べて返す。
 */
function generateAssets(count: number): MockAsset[] {
  const rand = mulberry32(0x111a1a); // 固定 seed で再現可能
  const assets: MockAsset[] = [];
  // 起点日: 2026-07-30 (docs の currentDate) から過去へ。
  const base = Date.UTC(2026, 6, 30, 12, 0, 0);
  const dayMs = 24 * 60 * 60 * 1000;

  let cursor = base;
  for (let i = 0; i < count; i++) {
    // 同日に複数枚固まるよう、確率的に日付を戻す。
    if (rand() < 0.4) {
      // 同じ日に留まる (0〜数時間戻す)
      cursor -= Math.floor(rand() * 3 * 60 * 60 * 1000);
    } else {
      // 1〜4 日戻す
      cursor -= Math.floor(1 + rand() * 3) * dayMs;
    }
    const d = new Date(cursor);
    const y = d.getUTCFullYear();
    const m = d.getUTCMonth() + 1;
    const day = d.getUTCDate();
    const ratio = RATIO_BUCKETS[Math.floor(rand() * RATIO_BUCKETS.length)];
    assets.push({
      id: `mock-${String(i).padStart(5, '0')}`,
      ratio,
      taken_at: d.toISOString(),
      dayKey: `${y}-${pad2(m)}-${pad2(day)}`,
      monthKey: `${y}-${pad2(m)}`,
      yearKey: String(y),
      hue: Math.floor(rand() * 360)
    });
  }
  // 既に cursor を遡りながら生成しているので taken_at DESC 済み。
  return assets;
}

function keyOf(a: MockAsset, g: Granularity): string {
  return g === 'day' ? a.dayKey : g === 'month' ? a.monthKey : a.yearKey;
}

/** SVG data URI のプレースホルダ画像を返す (実サムネの代替)。 */
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

/** MockAsset を API の Asset 形へ変換する。 */
function toAsset(a: MockAsset): Asset {
  const width = 1000;
  const height = Math.max(1, Math.round(width / a.ratio));
  return {
    id: a.id,
    filename: `${a.id}.png`,
    width,
    height,
    ratio: a.ratio,
    thumbhash: null,
    taken_at: a.taken_at,
    created_at: a.taken_at,
    status: 'created'
  };
}

let mockStackSeq = 1;

/** モック IllumiaApi を生成する。count 件を保持する。 */
export function createMockClient(count = 3000): IllumiaApi {
  const assets = generateAssets(count);
  const byId = new Map(assets.map((a) => [a.id, a]));
  const stacks: StackDetail[] = [];

  const findStack = (id: string): StackDetail | undefined => stacks.find((s) => s.id === id);

  const summaryOf = (s: StackDetail): StackSummary => ({
    id: s.id,
    title: s.title,
    cover_asset_id: s.cover_asset_id,
    chapter_count: s.chapters.length,
    page_count: s.chapters.reduce((n, c) => n + c.pages.length, 0),
    created_at: s.created_at,
    updated_at: s.updated_at
  });

  const assetFor = (id: string): Asset => {
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

  const touch = (s: StackDetail) => {
    s.updated_at = new Date().toISOString();
  };
  const settings: AppSettings = {
    'trash.retention_days': 30,
    'dedup.retention_days': 14,
    'jobs.thumbnail_concurrency': 3,
    'jobs.ml_concurrency': 1
  };

  // 疑似ネットワーク遅延 (レイアウト/仮想スクロールの挙動確認用)。
  const delay = <T>(value: T): Promise<T> =>
    new Promise((resolve) => setTimeout(() => resolve(value), 30 + Math.random() * 60));

  return {
    // --- 認証 (モックは常にセットアップ済み・任意パスワードでログイン成功) ---
    async serverInfo(): Promise<ServerInfo> {
      return delay({ version: 'mock', setup_completed: true });
    },
    async setup(_req: AuthRequest): Promise<TokenResponse> {
      return delay({ token: 'mock-token' });
    },
    async login(_req: AuthRequest): Promise<TokenResponse> {
      return delay({ token: 'mock-token' });
    },

    async getBuckets(g: Granularity): Promise<Bucket[]> {
      const counts = new Map<string, number>();
      for (const a of assets) {
        const k = keyOf(a, g);
        counts.set(k, (counts.get(k) ?? 0) + 1);
      }
      // taken_at DESC → キー降順。
      const buckets: Bucket[] = [...counts.entries()]
        .map(([key, c]) => ({ key, count: c }))
        .sort((x, y) => (x.key < y.key ? 1 : x.key > y.key ? -1 : 0));
      return delay(buckets);
    },

    async getBucketItems(g: Granularity, key: string): Promise<BucketItem[]> {
      const items: BucketItem[] = assets
        .filter((a) => keyOf(a, g) === key)
        .map((a) => ({
          id: a.id,
          ratio: a.ratio,
          thumbhash: null, // モックはプレースホルダ色で代替 (実 thumbhash ではない)
          taken_at: a.taken_at
        }));
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
    },

    // --- 操作系スタブ ---
    async uploadAsset(_file: File): Promise<UploadResult> {
      return delay({ id: 'mock-upload', status: 'created' });
    },
    async trashAsset(_id: string): Promise<void> {
      return delay(undefined);
    },
    async restoreAsset(_id: string): Promise<void> {
      return delay(undefined);
    },
    async getTrash(): Promise<Asset[]> {
      return delay([]);
    },
    async getDuplicates(): Promise<DuplicatePair[]> {
      return delay([]);
    },
    async purgeNow(_id: string): Promise<void> {
      return delay(undefined);
    },
    async getSettings(): Promise<AppSettings> {
      return delay({ ...settings });
    },
    async patchSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
      Object.assign(settings, patch);
      return delay({ ...settings });
    },

    // --- 漫画スタック ---
    async listStacks(): Promise<StackSummary[]> {
      const list = [...stacks]
        .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
        .map(summaryOf);
      return delay(list);
    },
    async createStack(title: string, assetIds: string[]): Promise<StackDetail> {
      const now = new Date().toISOString();
      const id = `mock-stack-${mockStackSeq++}`;
      const chapter: StackChapter = {
        id: `${id}-c1`,
        chapter_no: 1,
        title: null,
        pages: assetIds.map((aid, i) => ({
          page_no: i + 1,
          show_in_timeline: false,
          asset: assetFor(aid)
        }))
      };
      const stack: StackDetail = {
        id,
        title,
        cover_asset_id: assetIds[0] ?? null,
        created_at: now,
        updated_at: now,
        chapters: [chapter]
      };
      stacks.unshift(stack);
      return delay(structuredClone(stack));
    },
    async getStack(id: string): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      return delay(structuredClone(s));
    },
    async patchStack(id, patch): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      if (patch.title !== undefined) s.title = patch.title;
      if (patch.cover_asset_id !== undefined) s.cover_asset_id = patch.cover_asset_id;
      touch(s);
      return delay(structuredClone(s));
    },
    async deleteStack(id): Promise<void> {
      const i = stacks.findIndex((s) => s.id === id);
      if (i >= 0) stacks.splice(i, 1);
      return delay(undefined);
    },
    async replaceStructure(id: string, chapters: ChapterInput[]): Promise<StackDetail> {
      const s = findStack(id);
      if (!s) throw new ApiError(404, 'not_found', 'stack not found');
      // 既存フラグを引き継ぐ。
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
      for (const aid of assetIds) {
        chapter.pages.push({ page_no: ++no, show_in_timeline: false, asset: assetFor(aid) });
      }
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
      for (const c of s.chapters) {
        for (const p of c.pages) if (p.asset.id === assetId) p.show_in_timeline = showInTimeline;
      }
      touch(s);
      return delay(structuredClone(s));
    },

    // --- 検索 ---
    async search(q: string): Promise<SearchResult> {
      const query = q.trim().toLowerCase();
      if (query === '') return delay({ assets: [], stacks: [], clusters: [] });
      const matchedAssets = assets
        .filter((a) => a.id.toLowerCase().includes(query))
        .slice(0, 50)
        .map(toAsset);
      const matchedStacks = stacks
        .filter((s) => s.title.toLowerCase().includes(query))
        .map(summaryOf);
      return delay({ assets: matchedAssets, stacks: matchedStacks, clusters: [] });
    }
  };
}
