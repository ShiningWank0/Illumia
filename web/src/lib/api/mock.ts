// 開発用モック。サーバー未完成のため、TimelineApi と同一インタフェースで
// 決定的な擬似データ (縦長 / 横長 / 正方形を混ぜた数千件) を返す。
// VITE_USE_MOCK=1 で選択される (index.ts)。

import { type Bucket, type BucketItem, type Granularity, type TimelineApi } from './types';

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

/** モック TimelineApi を生成する。count 件を保持する。 */
export function createMockClient(count = 3000): TimelineApi {
  const assets = generateAssets(count);
  const byId = new Map(assets.map((a) => [a.id, a]));

  // 疑似ネットワーク遅延 (レイアウト/仮想スクロールの挙動確認用)。
  const delay = <T>(value: T): Promise<T> =>
    new Promise((resolve) => setTimeout(() => resolve(value), 30 + Math.random() * 60));

  return {
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
          thumbhash: `mockhash-${a.id}`,
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
    }
  };
}
