// justified タイルレイアウト (Flickr 方式の行詰め)。
// docs/04_timeline_layout.md の「justified アルゴリズム」を自前実装したもの。
// 外部依存を増やさず、将来 egui/Rust へ移植しやすいよう純関数として実装する。
//
// 入力: items[{id, ratio}], containerWidth, targetRowHeight, gap
// 出力: rows[{height, tiles[{id, x, width}]}]
//
// アルゴリズム (docs/04):
//   1. 行バッファに item を追加していき、
//      sum(ratio) * targetRowHeight + gap*(n-1) >= containerWidth となったら行確定
//   2. 行高 h = (containerWidth - gap*(n-1)) / sum(ratio) で等比スケール
//      (h は targetRowHeight * [0.6, 1.6] にクランプ。極端な行を防ぐ)
//   3. 最終行は詰めずに targetRowHeight のまま左寄せ
//   4. 幅は物理ピクセルに丸め、行内の丸め誤差は最後のタイルで吸収 (誤差 ≤ 1px)

/** レイアウト入力 1 件。ratio = width / height (アスペクト比)。 */
export interface LayoutItem {
  id: string;
  ratio: number;
}

/** 配置済みタイル。x は行内左端からの物理ピクセル座標。 */
export interface Tile {
  id: string;
  x: number;
  width: number;
}

/** 1 行分の配置結果。height は行内全タイル共通。 */
export interface Row {
  height: number;
  tiles: Tile[];
  /** 最終行 (justified せず targetRowHeight のまま左寄せ) の場合 true。 */
  isLastRow: boolean;
}

export interface JustifiedOptions {
  /** 描画領域の幅 (物理ピクセル)。 */
  containerWidth: number;
  /** 目標行高 (物理ピクセル)。粒度に応じて呼び出し側が渡す。 */
  targetRowHeight: number;
  /** タイル間の間隔 (物理ピクセル)。 */
  gap: number;
}

/** 行高クランプ下限係数。 */
export const MIN_HEIGHT_SCALE = 0.6;
/** 行高クランプ上限係数。 */
export const MAX_HEIGHT_SCALE = 1.6;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/**
 * 確定した 1 行を組み立てる。
 * @param fill true なら justified 行 (横幅を targetTotal に合わせる)、
 *             false なら最終行 (targetRowHeight のまま左寄せ・伸縮しない)。
 */
function buildRow(
  rowItems: LayoutItem[],
  sumRatio: number,
  options: JustifiedOptions,
  fill: boolean
): Row {
  const { containerWidth, targetRowHeight, gap } = options;
  const n = rowItems.length;
  const totalGap = gap * (n - 1);

  let height: number;
  let innerWidth: number; // タイル幅の合計 (gap を除く) の目標値

  if (fill) {
    // 等比スケールした行高を [0.6, 1.6]*target にクランプ。
    const naturalHeight = (containerWidth - totalGap) / sumRatio;
    const clampedHeight = clamp(
      naturalHeight,
      targetRowHeight * MIN_HEIGHT_SCALE,
      targetRowHeight * MAX_HEIGHT_SCALE
    );
    height = Math.round(clampedHeight);

    if (clampedHeight === naturalHeight) {
      // クランプされていない: 行はちょうど containerWidth を埋める。
      innerWidth = containerWidth - totalGap;
    } else {
      // クランプされた (極端な行): クランプ後の行高での自然な横幅を採用する。
      // これにより containerWidth からはみ出す / 余ることを許容し、タイルの
      // アスペクト比 (= ratio) を保つ。
      innerWidth = Math.round(clampedHeight * sumRatio);
    }
  } else {
    // 最終行は targetRowHeight のまま、伸縮せず左寄せ。
    height = Math.round(targetRowHeight);
    innerWidth = Math.round(targetRowHeight * sumRatio);
  }

  // 幅を物理ピクセルに丸めつつ、丸め誤差は最後のタイルで吸収する。
  // 各タイル幅は innerWidth を ratio 比で按分する。
  const tiles: Tile[] = [];
  let assigned = 0;
  let x = 0;
  for (let i = 0; i < n; i++) {
    let width: number;
    if (i === n - 1) {
      // 最後のタイルが残り全部を受け取る (= 丸め誤差の吸収)。
      width = innerWidth - assigned;
    } else {
      width = Math.round((innerWidth * rowItems[i].ratio) / sumRatio);
      assigned += width;
    }
    if (width < 0) width = 0; // 極端な按分でも負にはしない。
    tiles.push({ id: rowItems[i].id, x, width });
    x += width + gap;
  }

  return { height, tiles, isLastRow: !fill };
}

/**
 * justified レイアウトを計算する純関数。O(n)。
 * day / month 粒度で使用する (year は正方形グリッドなので呼び出さない)。
 */
export function justifiedLayout(items: LayoutItem[], options: JustifiedOptions): Row[] {
  const { containerWidth, targetRowHeight, gap } = options;

  // 不正な入力は空行で返す (呼び出し側の初期化タイミング等)。
  if (items.length === 0 || containerWidth <= 0 || targetRowHeight <= 0) {
    return [];
  }

  const rows: Row[] = [];
  let buffer: LayoutItem[] = [];
  let sumRatio = 0;

  for (const item of items) {
    // ratio は 0 以下を許容しない (0 除算防止)。呼び出し側で保証されるが念のため。
    const ratio = item.ratio > 0 ? item.ratio : 1;
    buffer.push({ id: item.id, ratio });
    sumRatio += ratio;

    const totalGap = gap * (buffer.length - 1);
    if (sumRatio * targetRowHeight + totalGap >= containerWidth) {
      rows.push(buildRow(buffer, sumRatio, options, true));
      buffer = [];
      sumRatio = 0;
    }
  }

  // 残りは最終行として左寄せ (justified しない)。
  if (buffer.length > 0) {
    rows.push(buildRow(buffer, sumRatio, options, false));
  }

  return rows;
}
