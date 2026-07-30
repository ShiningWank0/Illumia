// バケット内タイルの配置計算。day/month は justified、year は正方形グリッド。
// レイアウトの純粋部分はここに集約し、コンポーネントは描画に専念する。

import { justifiedLayout, type LayoutItem } from '$lib/layout/justified';
import type { BucketItem, Granularity } from '$lib/api/types';

/** 配置済みタイル (バケット本体の左上を原点とする物理ピクセル座標)。 */
export interface PlacedTile {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Placement {
  tiles: PlacedTile[];
  /** タイル領域の高さ (見出しを含まない)。 */
  contentHeight: number;
}

/** justified 配置 (day / month)。 */
export function placeJustified(
  items: LayoutItem[],
  containerWidth: number,
  targetRowHeight: number,
  gap: number
): Placement {
  const rows = justifiedLayout(items, { containerWidth, targetRowHeight, gap });
  const tiles: PlacedTile[] = [];
  let y = 0;
  for (const row of rows) {
    for (const t of row.tiles) {
      tiles.push({ id: t.id, x: t.x, y, width: t.width, height: row.height });
    }
    y += row.height + gap;
  }
  const contentHeight = rows.length > 0 ? y - gap : 0;
  return { tiles, contentHeight };
}

/** 正方形グリッド配置 (year)。cell は 1 辺の目標サイズ。 */
export function placeSquareGrid(
  ids: { id: string }[],
  containerWidth: number,
  cell: number,
  gap: number
): Placement {
  const perRow = Math.max(1, Math.floor((containerWidth + gap) / (cell + gap)));
  // 端数を width に均等分配せず、cell 固定 (docs/04: 均一グリッド)。
  const tiles: PlacedTile[] = ids.map((it, i) => {
    const col = i % perRow;
    const row = Math.floor(i / perRow);
    return {
      id: it.id,
      x: col * (cell + gap),
      y: row * (cell + gap),
      width: cell,
      height: cell
    };
  });
  const rowCount = Math.ceil(ids.length / perRow);
  const contentHeight = rowCount > 0 ? rowCount * (cell + gap) - gap : 0;
  return { tiles, contentHeight };
}

/** 粒度と画面幅から目標行高 / セルサイズを求める (docs/04 基準値をスケール)。 */
export function targetRowHeightFor(granularity: Granularity, containerWidth: number): number {
  const base = granularity === 'day' ? 240 : granularity === 'month' ? 140 : 90;
  // 基準 1280px。広いほど少し大きく、狭いほど小さく (0.85〜1.3 倍)。
  const scale = Math.min(1.3, Math.max(0.85, containerWidth / 1280));
  return Math.round(base * scale);
}

/** タイル間 gap (粒度共通)。 */
export const TILE_GAP = 4;

/**
 * バケット高さの推定 (実データ取得前)。平均 ratio=1 と仮定する (docs/04)。
 * 見出し高は含めない (呼び出し側で加算)。
 */
export function estimateContentHeight(
  granularity: Granularity,
  count: number,
  containerWidth: number
): number {
  if (count <= 0 || containerWidth <= 0) return 0;
  const target = targetRowHeightFor(granularity, containerWidth);
  const gap = TILE_GAP;
  if (granularity === 'year') {
    const perRow = Math.max(1, Math.floor((containerWidth + gap) / (target + gap)));
    const rows = Math.ceil(count / perRow);
    return rows * (target + gap) - gap;
  }
  // justified: 平均 ratio=1 として 1 行あたりのタイル数を見積もる。
  const perRow = Math.max(1, Math.floor((containerWidth + gap) / (target + gap)));
  const rows = Math.ceil(count / perRow);
  return rows * (target + gap) - gap;
}

/** 配置の実行 (実データ取得後)。 */
export function place(
  granularity: Granularity,
  items: BucketItem[],
  containerWidth: number
): Placement {
  const target = targetRowHeightFor(granularity, containerWidth);
  if (granularity === 'year') {
    return placeSquareGrid(items, containerWidth, target, TILE_GAP);
  }
  const layoutItems: LayoutItem[] = items.map((it) => ({ id: it.id, ratio: it.ratio }));
  return placeJustified(layoutItems, containerWidth, target, TILE_GAP);
}
