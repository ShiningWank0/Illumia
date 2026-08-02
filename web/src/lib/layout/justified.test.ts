import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';
import fc from 'fast-check';

import {
  justifiedLayout,
  MAX_HEIGHT_SCALE,
  MIN_HEIGHT_SCALE,
  type JustifiedOptions,
  type LayoutItem,
  type Row
} from './justified';

// ratio 0.2〜5.0 の極端値を含む item 列。
const itemArb = fc.record({
  id: fc.string({ minLength: 1, maxLength: 8 }),
  ratio: fc.double({ min: 0.2, max: 5.0, noNaN: true })
});

const optionsArb = fc.record({
  containerWidth: fc.integer({ min: 200, max: 4000 }),
  targetRowHeight: fc.integer({ min: 40, max: 400 }),
  gap: fc.integer({ min: 0, max: 24 })
});

/** 行の実効幅 = 最後のタイルの右端。 */
function rowWidth(row: Row): number {
  if (row.tiles.length === 0) return 0;
  const last = row.tiles[row.tiles.length - 1];
  return last.x + last.width;
}

/** x 座標が gap を挟んで単調・整合しているか検証。 */
function assertPositions(row: Row, gap: number): void {
  let x = 0;
  for (const tile of row.tiles) {
    expect(tile.x).toBe(x);
    expect(tile.width).toBeGreaterThanOrEqual(0);
    x += tile.width + gap;
  }
}

describe('justifiedLayout: property tests', () => {
  it('順序保存: 全タイルの id 列が入力順と一致する', () => {
    fc.assert(
      fc.property(fc.array(itemArb), optionsArb, (items, options) => {
        const rows = justifiedLayout(items, options);
        const flat = rows.flatMap((r) => r.tiles.map((t) => t.id));
        expect(flat).toEqual(items.map((i) => i.id));
      })
    );
  });

  it('x 座標は行内で単調・非重複 (gap を挟んで整合)', () => {
    fc.assert(
      fc.property(fc.array(itemArb), optionsArb, (items, options) => {
        const rows = justifiedLayout(items, options);
        for (const row of rows) {
          assertPositions(row, options.gap);
        }
      })
    );
  });

  it('行幅誤差 ≤1px: クランプされていない justified 行は containerWidth を埋める', () => {
    fc.assert(
      fc.property(fc.array(itemArb), optionsArb, (items, options: JustifiedOptions) => {
        const rows = justifiedLayout(items, options);
        for (const row of rows) {
          if (row.isLastRow) continue; // 最終行は伸縮しない
          const sumRatio = sumRowRatio(row, items);
          const totalGap = options.gap * (row.tiles.length - 1);
          const natural = (options.containerWidth - totalGap) / sumRatio;
          const min = options.targetRowHeight * MIN_HEIGHT_SCALE;
          const max = options.targetRowHeight * MAX_HEIGHT_SCALE;
          const clamped = natural < min || natural > max;
          if (!clamped) {
            // クランプなし → ちょうど containerWidth (丸め誤差 ≤1px)。
            expect(Math.abs(rowWidth(row) - options.containerWidth)).toBeLessThanOrEqual(1);
          }
        }
      })
    );
  });

  it('justified 行の行高は [0.6, 1.6]*target にクランプされる', () => {
    fc.assert(
      fc.property(fc.array(itemArb), optionsArb, (items, options) => {
        const rows = justifiedLayout(items, options);
        for (const row of rows) {
          if (row.isLastRow) continue;
          const min = Math.round(options.targetRowHeight * MIN_HEIGHT_SCALE);
          const max = Math.round(options.targetRowHeight * MAX_HEIGHT_SCALE);
          expect(row.height).toBeGreaterThanOrEqual(min);
          expect(row.height).toBeLessThanOrEqual(max);
        }
      })
    );
  });

  it('丸め整合: 行のタイル幅合計 + gap = 実効幅 (内部整合)', () => {
    fc.assert(
      fc.property(fc.array(itemArb, { minLength: 1 }), optionsArb, (items, options) => {
        const rows = justifiedLayout(items, options);
        for (const row of rows) {
          const totalGap = options.gap * (row.tiles.length - 1);
          const sumWidth = row.tiles.reduce((s, t) => s + t.width, 0);
          expect(sumWidth + totalGap).toBe(rowWidth(row));
        }
      })
    );
  });
});

// 行内タイルの id から元 item の ratio を引いて合計する (ratio<=0 は 1 に補正済み)。
function sumRowRatio(row: Row, items: LayoutItem[]): number {
  const byId = new Map(items.map((i) => [i.id, i.ratio > 0 ? i.ratio : 1]));
  // 同一 id が複数あり得る (fast-check の string 生成) ため、順序で消費する。
  let sum = 0;
  for (const t of row.tiles) sum += byId.get(t.id) ?? 1;
  return sum;
}

describe('justifiedLayout: edge cases', () => {
  const options: JustifiedOptions = { containerWidth: 1000, targetRowHeight: 200, gap: 8 };

  it('空入力 → 空配列', () => {
    expect(justifiedLayout([], options)).toEqual([]);
  });

  it('1 枚 (小さく行を埋めない) → 最終行として targetRowHeight 左寄せ', () => {
    const rows = justifiedLayout([{ id: 'a', ratio: 1 }], options);
    expect(rows).toHaveLength(1);
    expect(rows[0].isLastRow).toBe(true);
    expect(rows[0].height).toBe(200);
    expect(rows[0].tiles).toHaveLength(1);
    expect(rows[0].tiles[0].x).toBe(0);
    expect(rows[0].tiles[0].width).toBe(200); // ratio 1 * height
  });

  it('1 枚 (超横長) で行を満たす → justified 行になる', () => {
    const rows = justifiedLayout([{ id: 'wide', ratio: 5 }], options);
    expect(rows).toHaveLength(1);
    expect(rows[0].isLastRow).toBe(false);
    expect(rows[0].tiles[0].x).toBe(0);
    // クランプ: natural = 1000/5 = 200 → [120,320] 内なので containerWidth を埋める。
    expect(rows[0].tiles[0].width).toBe(1000);
  });

  it('containerWidth <= 0 → 空配列', () => {
    expect(justifiedLayout([{ id: 'a', ratio: 1 }], { ...options, containerWidth: 0 })).toEqual([]);
  });

  it('複数行: 各 justified 行が containerWidth を埋め、最終行だけ左寄せ', () => {
    // 21 枚 (ratio 1.5) は 1 行 4 枚で 5 行 + 余り 1 枚 = 最終行が余りになる。
    const items: LayoutItem[] = Array.from({ length: 21 }, (_, i) => ({
      id: `img-${i}`,
      ratio: 1.5
    }));
    const rows = justifiedLayout(items, options);
    expect(rows.length).toBeGreaterThan(1);
    for (let i = 0; i < rows.length - 1; i++) {
      const w = rows[i].tiles[rows[i].tiles.length - 1];
      expect(Math.abs(w.x + w.width - 1000)).toBeLessThanOrEqual(1);
    }
    expect(rows[rows.length - 1].isLastRow).toBe(true);
  });
});

// 共有テストベクタ (testdata/justified_layout.json) との一致。
// egui 版 (crates/illumia-desktop) が同じファイルを読んで検証するため、
// ここが通り Rust 側も通れば、両実装の結果が一致していることになる (docs/08)。
describe('共有テストベクタ', () => {
  const vectors = JSON.parse(
    readFileSync(new URL('../../../../testdata/justified_layout.json', import.meta.url), 'utf8')
  ) as {
    cases: { name: string; items: LayoutItem[]; options: JustifiedOptions; rows: Row[] }[];
  };

  it('ベクタが生成済みである', () => {
    expect(vectors.cases.length).toBeGreaterThanOrEqual(8);
  });

  for (const testCase of vectors.cases) {
    it(`${testCase.name} が記録済みの結果と一致する`, () => {
      expect(justifiedLayout(testCase.items, testCase.options)).toEqual(testCase.rows);
    });
  }
});
