//! UI をヘッドレスで実描画し、実データが画面に出ることを検証する。
//!
//! ウィンドウを開かずに `egui::Context` を回すことで、レイアウト計算・
//! テクスチャ読み込み・ウィジェット構築まで実際のコードパスを通す。
//! GUI の目視確認が難しい CI でも回帰を検出できる。

use std::sync::Arc;

use illumia_core::{assets::AssetService, db::Database, thumbnails};
use illumia_desktop::{
    app::IllumiaApp,
    backend::{Backend, LocalBackend},
};

fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut image = image::RgbaImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([
            ((x * 3 + u32::from(seed) * 40) % 256) as u8,
            ((y * 5 + u32::from(seed) * 70) % 256) as u8,
            (u32::from(seed) * 30 % 256) as u8,
            255,
        ]);
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

/// 実データ入りのデータディレクトリを用意する。
fn seeded(count: u8) -> TempDir {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "illumia-desktop-ui-{}-{}",
        std::process::id(),
        illumia_core::uuid::Uuid::now_v7()
    )));
    std::fs::create_dir_all(&dir.0).expect("一時ディレクトリを作れる");
    let database = Database::open(&dir.0).expect("DB を開ける");
    let assets = AssetService::new(database.clone());
    for index in 0..count {
        let width = 600 + u32::from(index) * 100;
        let height = 800 - u32::from(index) * 50;
        let result = assets
            .ingest(
                &png(width, height, index + 1),
                &format!("s{index}.png"),
                None,
            )
            .expect("取り込みできる");
        thumbnails::generate_thumbnails(&database, &result.asset.id).expect("サムネイル生成");
    }
    dir
}

/// 1 フレーム描画し、生成された描画プリミティブ数を返す。
fn render_once(app: &mut IllumiaApp, context: &egui::Context) -> usize {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 860.0),
        )),
        ..Default::default()
    };
    let output = context.run(input, |context| app.draw(context));
    let primitives = context.tessellate(output.shapes, output.pixels_per_point);
    primitives
        .iter()
        .map(|primitive| match &primitive.primitive {
            egui::epaint::Primitive::Mesh(mesh) => mesh.indices.len(),
            egui::epaint::Primitive::Callback(_) => 1,
        })
        .sum()
}

#[test]
fn ui_renders_seeded_assets_without_panicking() {
    let dir = seeded(6);
    let backend: Arc<dyn Backend> =
        Arc::new(LocalBackend::open(&dir.0).expect("ローカル backend を開ける"));

    let context = egui::Context::default();
    let mut app = IllumiaApp::new(Arc::clone(&backend));

    // 複数フレーム回して、テクスチャ読み込み後も安定して描画できることを見る。
    let mut counts = Vec::new();
    for _ in 0..3 {
        counts.push(render_once(&mut app, &context));
    }

    eprintln!("描画プリミティブ数 (フレーム毎): {counts:?}");
    for (frame, count) in counts.iter().enumerate() {
        assert!(
            *count > 0,
            "フレーム {frame} で何も描画されていない (UI が空)"
        );
    }
    // タイル 6 枚 + サイドバー + ツールバーが出る以上、単なる背景より十分多い。
    assert!(
        counts[2] > 100,
        "描画量が少なすぎる: {} (タイルが出ていない疑い)",
        counts[2]
    );
}

#[test]
fn ui_renders_when_the_library_is_empty() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "illumia-desktop-ui-empty-{}-{}",
        std::process::id(),
        illumia_core::uuid::Uuid::now_v7()
    )));
    std::fs::create_dir_all(&dir.0).expect("一時ディレクトリを作れる");

    let backend: Arc<dyn Backend> =
        Arc::new(LocalBackend::open(&dir.0).expect("ローカル backend を開ける"));
    let context = egui::Context::default();
    let mut app = IllumiaApp::new(backend);

    // 空ライブラリでも panic せず、空状態の案内が描画される。
    assert!(render_once(&mut app, &context) > 0);
}
