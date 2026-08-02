// justified レイアウトの共有テストベクタを生成する (docs/08: egui 版と結果一致を検証)。
//
// TS 実装を正として ../testdata/justified_layout.json を書き出し、
// web (vitest) と Rust (illumia-desktop) の双方がこの同一ファイルを読んで検証する。
// 実装を変更したら `npm run gen:layout-vectors` で再生成し、差分をレビューする。

import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  justifiedLayout,
  type JustifiedOptions,
  type LayoutItem
} from '../src/lib/layout/justified';

interface Case {
  name: string;
  items: LayoutItem[];
  options: JustifiedOptions;
}

/** 決定論的な擬似乱数 (seed 固定)。生成のたびに揺れないようにする。 */
function makeRng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0x100000000;
  };
}

function ratios(count: number, seed: number, min: number, max: number): LayoutItem[] {
  const rng = makeRng(seed);
  return Array.from({ length: count }, (_, i) => ({
    id: `a${i}`,
    ratio: Number((min + rng() * (max - min)).toFixed(4))
  }));
}

const CASES: Case[] = [
  {
    name: 'empty',
    items: [],
    options: { containerWidth: 1200, targetRowHeight: 220, gap: 4 }
  },
  {
    name: 'single-last-row',
    items: [{ id: 'a0', ratio: 1.5 }],
    options: { containerWidth: 1200, targetRowHeight: 220, gap: 4 }
  },
  {
    name: 'exact-landscape',
    items: ratios(20, 1, 1.2, 1.8),
    options: { containerWidth: 1200, targetRowHeight: 220, gap: 4 }
  },
  {
    name: 'portrait-heavy',
    items: ratios(25, 7, 0.4, 0.9),
    options: { containerWidth: 1000, targetRowHeight: 200, gap: 8 }
  },
  {
    name: 'extreme-ratios-clamped',
    items: ratios(30, 13, 0.2, 5.0),
    options: { containerWidth: 800, targetRowHeight: 160, gap: 6 }
  },
  {
    name: 'no-gap',
    items: ratios(15, 21, 0.8, 2.4),
    options: { containerWidth: 1600, targetRowHeight: 300, gap: 0 }
  },
  {
    name: 'narrow-container',
    items: ratios(12, 33, 1.0, 3.0),
    options: { containerWidth: 240, targetRowHeight: 120, gap: 4 }
  },
  {
    name: 'wide-container-large-gap',
    items: ratios(40, 41, 0.5, 3.5),
    options: { containerWidth: 3840, targetRowHeight: 400, gap: 24 }
  }
];

const vectors = CASES.map((testCase) => ({
  ...testCase,
  rows: justifiedLayout(testCase.items, testCase.options)
}));

const target = fileURLToPath(new URL('../../testdata/justified_layout.json', import.meta.url));
writeFileSync(target, `${JSON.stringify({ cases: vectors }, null, 2)}\n`, 'utf8');
console.log(`wrote ${vectors.length} cases to ${target}`);
