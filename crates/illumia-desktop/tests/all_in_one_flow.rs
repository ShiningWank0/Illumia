//! all-in-one モードのデータ経路を実データで検証する。
//!
//! HTTP を介さず `illumia-core` のサービス層を直接呼ぶ構成が、実際に
//! 取り込み → タイムライン → サムネイル取得まで通ることを確認する
//! (docs/01: all-in-one は in-process 直接呼び出し)。

use illumia_core::{assets::AssetService, db::Database, thumbnails, timeline::Granularity};
use illumia_desktop::backend::{Backend, LocalBackend, Variant};

/// テスト用の小さな PNG を生成する (外部ファイルに依存しない)。
fn sample_png(width: u32, height: u32) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut image = image::RgbaImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
    }
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut buffer, image::ImageFormat::Png)
        .expect("PNG をエンコードできる");
    buffer.into_inner()
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ingested_assets_are_visible_through_the_local_backend() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "illumia-desktop-flow-{}-{}",
        std::process::id(),
        illumia_core::uuid::Uuid::now_v7()
    )));
    std::fs::create_dir_all(&dir.0).expect("一時ディレクトリを作れる");

    // core 側で取り込み + サムネイル生成を済ませる (server 経由ではない)。
    let database = Database::open(&dir.0).expect("DB を開ける");
    let assets = AssetService::new(database.clone());
    let ingested = assets
        .ingest(&sample_png(640, 480), "sample.png", None)
        .expect("取り込みできる");
    thumbnails::generate_thumbnails(&database, &ingested.asset.id).expect("サムネイルを生成できる");
    drop(database);

    // ここから先は UI が使うのと同じ経路。
    let backend = LocalBackend::open(&dir.0).expect("ローカル backend を開ける");

    let buckets = backend
        .buckets(Granularity::Day)
        .expect("バケットを取得できる");
    assert_eq!(buckets.len(), 1, "取り込んだ 1 件のバケットが見える");
    assert_eq!(buckets[0].count, 1);

    let items = backend
        .items(&buckets[0].key)
        .expect("バケット内のアセットを取得できる");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, ingested.asset.id);
    // 640x480 → 4:3
    assert!(
        (items[0].ratio - 4.0 / 3.0).abs() < 0.01,
        "アスペクト比が取り込み時と一致する: {}",
        items[0].ratio
    );

    // サムネイル / プレビューが実バイト列として読め、WebP としてデコードできる。
    for variant in [Variant::Thumbnail, Variant::Preview] {
        let bytes = backend
            .image(&ingested.asset.id, variant)
            .unwrap_or_else(|error| panic!("{variant:?} を取得できる: {error}"));
        assert!(!bytes.is_empty(), "{variant:?} が空でない");
        let decoded = image::load_from_memory(&bytes)
            .unwrap_or_else(|error| panic!("{variant:?} をデコードできる: {error}"));
        assert!(decoded.width() > 0 && decoded.height() > 0);
    }

    // 存在しない ID は拒否される (パス組み立ての安全性)。
    assert!(backend.image("does-not-exist", Variant::Thumbnail).is_err());
}
