//! Web 版 justified レイアウトとの結果一致テスト (docs/08)。
//!
//! `testdata/justified_layout.json` は TS 実装から生成した共有ベクタで、
//! web (vitest) と本テストが同じファイルを読む。どちらかの実装がずれたら
//! 両方が落ちる。

use illumia_desktop::layout::{JustifiedOptions, LayoutItem, Row, justified_layout};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    items: Vec<LayoutItem>,
    options: JustifiedOptions,
    rows: Vec<Row>,
}

fn load() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/justified_layout.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("共有テストベクタを読めない ({path}): {error}"));
    serde_json::from_str(&raw).expect("共有テストベクタは JSON として妥当であること")
}

#[test]
fn rust_layout_matches_web_reference_vectors() {
    let vectors = load();
    assert!(
        vectors.cases.len() >= 8,
        "ベクタが想定より少ない (生成漏れの疑い)"
    );

    for case in &vectors.cases {
        let actual = justified_layout(&case.items, &case.options);
        assert_eq!(
            actual.len(),
            case.rows.len(),
            "[{}] 行数が Web 版と一致しない",
            case.name
        );
        for (index, (actual_row, expected_row)) in actual.iter().zip(&case.rows).enumerate() {
            assert_eq!(
                actual_row, expected_row,
                "[{}] 行 {index} が Web 版と一致しない",
                case.name
            );
        }
    }
}

/// docs/04 の不変条件: justified 行はタイルがはみ出さず、順に左から並ぶ。
#[test]
fn tiles_are_ordered_and_non_overlapping() {
    let vectors = load();
    for case in &vectors.cases {
        for row in &justified_layout(&case.items, &case.options) {
            // gap = 0 のケースでは隣接タイルが接する (x == 前タイルの右端)。
            // 接触は重なりではないので >= で判定する。
            let mut previous_right: Option<i64> = None;
            for tile in &row.tiles {
                if let Some(right) = previous_right {
                    assert!(
                        tile.x >= right,
                        "[{}] タイルが重なっている (x={}, 前タイル右端={right})",
                        case.name,
                        tile.x
                    );
                }
                assert!(tile.width >= 0, "[{}] タイル幅が負", case.name);
                previous_right = Some(tile.x + tile.width);
            }
        }
    }
}
