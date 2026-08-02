//! justified タイルレイアウト (docs/04_timeline_layout.md)。
//!
//! Web 版 (`web/src/lib/layout/justified.ts`) の移植。docs/08 の要求どおり、
//! 双方が同一のテストベクタ (`testdata/justified_layout.json`) を読んで
//! 結果一致を検証する。アルゴリズムを変更する場合は TS 側を正として更新し、
//! `npm run gen:layout-vectors` でベクタを再生成すること。
//!
//! アルゴリズム:
//!   1. 行バッファに item を追加し、`sum(ratio)*target + gap*(n-1) >= width` で行確定
//!   2. 行高 `h = (width - gap*(n-1)) / sum(ratio)` を `target*[0.6, 1.6]` にクランプ
//!   3. 最終行は詰めず `target` のまま左寄せ
//!   4. 幅は物理ピクセルへ丸め、丸め誤差は行末タイルで吸収

use serde::{Deserialize, Serialize};

/// 行高クランプ下限係数。
pub const MIN_HEIGHT_SCALE: f64 = 0.6;
/// 行高クランプ上限係数。
pub const MAX_HEIGHT_SCALE: f64 = 1.6;

/// レイアウト入力 1 件。`ratio` = width / height。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LayoutItem {
    pub id: String,
    pub ratio: f64,
}

/// 配置済みタイル。`x` は行内左端からの物理ピクセル座標。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Tile {
    pub id: String,
    pub x: i64,
    pub width: i64,
}

/// 1 行分の配置結果。`height` は行内全タイル共通。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Row {
    pub height: i64,
    pub tiles: Vec<Tile>,
    /// 最終行 (justified せず target のまま左寄せ) なら true。
    #[serde(rename = "isLastRow")]
    pub is_last_row: bool,
}

/// レイアウトのパラメータ。いずれも物理ピクセル。
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct JustifiedOptions {
    #[serde(rename = "containerWidth")]
    pub container_width: f64,
    #[serde(rename = "targetRowHeight")]
    pub target_row_height: f64,
    pub gap: f64,
}

/// JS の `Math.round` と同じ丸め (0.5 は常に +∞ 方向)。
///
/// Rust の `f64::round` は 0 から遠い方向へ丸めるため、負値で JS と食い違う。
/// テストベクタ一致のためここを揃える。
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

/// 確定した 1 行を組み立てる。`fill` が false なら最終行 (伸縮しない)。
fn build_row(
    row_items: &[LayoutItem],
    sum_ratio: f64,
    options: &JustifiedOptions,
    fill: bool,
) -> Row {
    let n = row_items.len();
    let total_gap = options.gap * (n as f64 - 1.0);

    let (height, inner_width) = if fill {
        let natural_height = (options.container_width - total_gap) / sum_ratio;
        let clamped_height = clamp(
            natural_height,
            options.target_row_height * MIN_HEIGHT_SCALE,
            options.target_row_height * MAX_HEIGHT_SCALE,
        );
        let inner_width = if clamped_height == natural_height {
            // クランプされていない: 行はちょうど container_width を埋める。
            options.container_width - total_gap
        } else {
            // クランプされた極端な行: アスペクト比を保ち、はみ出し/余りを許容する。
            js_round(clamped_height * sum_ratio)
        };
        (js_round(clamped_height), inner_width)
    } else {
        (
            js_round(options.target_row_height),
            js_round(options.target_row_height * sum_ratio),
        )
    };

    // 幅を丸めつつ、丸め誤差は最後のタイルで吸収する。
    let mut tiles = Vec::with_capacity(n);
    let mut assigned = 0.0_f64;
    let mut x = 0.0_f64;
    for (index, item) in row_items.iter().enumerate() {
        let mut width = if index == n - 1 {
            inner_width - assigned
        } else {
            let width = js_round(inner_width * item.ratio / sum_ratio);
            assigned += width;
            width
        };
        if width < 0.0 {
            width = 0.0;
        }
        tiles.push(Tile {
            id: item.id.clone(),
            x: x as i64,
            width: width as i64,
        });
        x += width + options.gap;
    }

    Row {
        height: height as i64,
        tiles,
        is_last_row: !fill,
    }
}

/// justified レイアウトを計算する純関数。O(n)。
///
/// day / month 粒度で使う (year は正方形グリッドなので呼ばない)。
pub fn justified_layout(items: &[LayoutItem], options: &JustifiedOptions) -> Vec<Row> {
    if items.is_empty() || options.container_width <= 0.0 || options.target_row_height <= 0.0 {
        return Vec::new();
    }

    let mut rows = Vec::new();
    let mut buffer: Vec<LayoutItem> = Vec::new();
    let mut sum_ratio = 0.0_f64;

    for item in items {
        // ratio は 0 以下を許容しない (0 除算防止)。
        let ratio = if item.ratio > 0.0 { item.ratio } else { 1.0 };
        buffer.push(LayoutItem {
            id: item.id.clone(),
            ratio,
        });
        sum_ratio += ratio;

        let total_gap = options.gap * (buffer.len() as f64 - 1.0);
        if sum_ratio * options.target_row_height + total_gap >= options.container_width {
            rows.push(build_row(&buffer, sum_ratio, options, true));
            buffer.clear();
            sum_ratio = 0.0;
        }
    }

    if !buffer.is_empty() {
        rows.push(build_row(&buffer, sum_ratio, options, false));
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_round_matches_javascript_semantics() {
        // JS の Math.round は 0.5 を常に +∞ 方向へ丸める。
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(1.5), 2.0);
        assert_eq!(js_round(-0.5), 0.0);
        assert_eq!(js_round(-1.5), -1.0);
        assert_eq!(js_round(2.4), 2.0);
    }

    #[test]
    fn empty_input_yields_no_rows() {
        let options = JustifiedOptions {
            container_width: 1200.0,
            target_row_height: 220.0,
            gap: 4.0,
        };
        assert!(justified_layout(&[], &options).is_empty());
    }

    #[test]
    fn invalid_dimensions_yield_no_rows() {
        let items = vec![LayoutItem {
            id: "a".into(),
            ratio: 1.5,
        }];
        let zero_width = JustifiedOptions {
            container_width: 0.0,
            target_row_height: 220.0,
            gap: 4.0,
        };
        assert!(justified_layout(&items, &zero_width).is_empty());
    }

    #[test]
    fn last_row_is_not_justified() {
        let options = JustifiedOptions {
            container_width: 1200.0,
            target_row_height: 220.0,
            gap: 4.0,
        };
        let items = vec![LayoutItem {
            id: "a".into(),
            ratio: 1.5,
        }];
        let rows = justified_layout(&items, &options);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_last_row);
        assert_eq!(rows[0].height, 220);
    }
}
