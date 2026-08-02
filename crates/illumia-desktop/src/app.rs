//! egui アプリ本体。タイムライン (justified タイル) とビューアを提供する。
//!
//! docs/04 のとおりレイアウト計算はクライアント側で行う。タイル配置は
//! [`crate::layout`] の justified 実装を使い、Web 版と同一結果になる。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use illumia_core::timeline::Granularity;

use crate::{
    backend::{Backend, Bucket, Item, Variant},
    layout::{JustifiedOptions, LayoutItem, justified_layout},
};

/// タイル間の間隔 (論理ピクセル)。
const GAP: f32 = 4.0;
/// 目標行高 (論理ピクセル)。
const TARGET_ROW_HEIGHT: f32 = 200.0;
/// 同時に保持するテクスチャの上限 (メモリ上限 → docs/12)。
const MAX_TEXTURES: usize = 512;

pub struct IllumiaApp {
    backend: Arc<dyn Backend>,
    granularity: Granularity,
    buckets: Vec<Bucket>,
    selected_bucket: Option<String>,
    items: Vec<Item>,
    /// asset id -> テクスチャ。読み込み済みのサムネイル。
    textures: HashMap<String, TextureHandle>,
    /// 読み込み中 / 失敗した asset id (再試行の抑制)。
    pending: Arc<Mutex<HashMap<String, Option<Vec<u8>>>>>,
    viewer: Option<String>,
    error: Option<String>,
}

impl IllumiaApp {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let mut app = Self {
            backend,
            granularity: Granularity::Day,
            buckets: Vec::new(),
            selected_bucket: None,
            items: Vec::new(),
            textures: HashMap::new(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            viewer: None,
            error: None,
        };
        app.reload_buckets();
        app
    }

    fn reload_buckets(&mut self) {
        match self.backend.buckets(self.granularity) {
            Ok(buckets) => {
                self.buckets = buckets;
                self.error = None;
                if self.selected_bucket.is_none()
                    && let Some(first) = self.buckets.first().map(|bucket| bucket.key.clone())
                {
                    self.select_bucket(first);
                }
            }
            Err(error) => self.error = Some(format!("バケットを取得できません: {error}")),
        }
    }

    fn select_bucket(&mut self, key: String) {
        match self.backend.items(&key) {
            Ok(items) => {
                self.items = items;
                self.selected_bucket = Some(key);
                self.error = None;
                // バケットを移ったらテクスチャを解放する。
                self.textures.clear();
                self.pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
            }
            Err(error) => self.error = Some(format!("アセットを取得できません: {error}")),
        }
    }

    /// サムネイルを (必要なら) 読み込んでテクスチャを返す。
    fn texture(&mut self, context: &egui::Context, asset_id: &str) -> Option<TextureHandle> {
        if let Some(handle) = self.textures.get(asset_id) {
            return Some(handle.clone());
        }
        if self.textures.len() >= MAX_TEXTURES {
            // 単純な上限制御。超えたら以降は読まない (スクロール時の暴走防止)。
            return None;
        }

        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = pending.get(asset_id) {
            // None = 読み込み失敗済み。再試行しない。
            let bytes = entry.clone()?;
            drop(pending);
            let image = decode_webp(&bytes)?;
            let handle = context.load_texture(asset_id, image, TextureOptions::default());
            self.textures.insert(asset_id.to_owned(), handle.clone());
            return Some(handle);
        }

        let result = self.backend.image(asset_id, Variant::Thumbnail).ok();
        pending.insert(asset_id.to_owned(), result.clone());
        drop(pending);

        let bytes = result?;
        let image = decode_webp(&bytes)?;
        let handle = context.load_texture(asset_id, image, TextureOptions::default());
        self.textures.insert(asset_id.to_owned(), handle.clone());
        Some(handle)
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        if width <= 0.0 || self.items.is_empty() {
            ui.label("表示できる画像がありません。");
            return;
        }

        let layout_items: Vec<LayoutItem> = self
            .items
            .iter()
            .map(|item| LayoutItem {
                id: item.id.clone(),
                ratio: item.ratio,
            })
            .collect();
        let options = JustifiedOptions {
            container_width: f64::from(width),
            target_row_height: f64::from(TARGET_ROW_HEIGHT),
            gap: f64::from(GAP),
        };
        let rows = justified_layout(&layout_items, &options);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for row in &rows {
                let height = row.height as f32;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for tile in &row.tiles {
                        let size = egui::vec2(tile.width as f32, height);
                        let context = ui.ctx().clone();
                        let response = match self.texture(&context, &tile.id) {
                            Some(handle) => ui.add(
                                egui::Image::new(&handle)
                                    .fit_to_exact_size(size)
                                    .sense(egui::Sense::click()),
                            ),
                            None => {
                                // サムネ未生成・読み込み失敗はプレースホルダで埋める。
                                let (rect, response) =
                                    ui.allocate_exact_size(size, egui::Sense::click());
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    Color32::from_rgb(0x2a, 0x2a, 0x33),
                                );
                                response
                            }
                        };
                        if response.clicked() {
                            self.viewer = Some(tile.id.clone());
                        }
                    }
                });
                ui.add_space(GAP);
            }
        });
    }

    fn viewer_ui(&mut self, context: &egui::Context) {
        let Some(asset_id) = self.viewer.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("ビューア")
            .open(&mut open)
            .default_size([900.0, 700.0])
            .show(context, |ui| {
                match self.backend.image(&asset_id, Variant::Preview) {
                    Ok(bytes) => match decode_webp(&bytes) {
                        Some(image) => {
                            let handle = context.load_texture(
                                format!("preview-{asset_id}"),
                                image,
                                TextureOptions::default(),
                            );
                            ui.add(
                                egui::Image::new(&handle)
                                    .max_size(ui.available_size())
                                    .maintain_aspect_ratio(true),
                            );
                        }
                        None => {
                            ui.label("プレビューを表示できません。");
                        }
                    },
                    Err(error) => {
                        ui.label(format!("プレビューを取得できません: {error}"));
                    }
                }
                if let Some(item) = self.items.iter().find(|item| item.id == asset_id) {
                    ui.separator();
                    ui.label(format!("撮影日時: {}", item.taken_at));
                }
            });
        if !open {
            self.viewer = None;
        }
    }
}

impl eframe::App for IllumiaApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.draw(context);
    }
}

impl IllumiaApp {
    /// UI 全体を 1 フレーム分描画する。
    ///
    /// `eframe::Frame` に依存しないため、ヘッドレスな `egui::Context` からも
    /// 呼べる (→ tests/headless_ui.rs で実描画を検証する)。
    pub fn draw(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Illumia");
                ui.separator();
                ui.label(self.backend.mode_label());
                ui.separator();
                for (granularity, label) in [
                    (Granularity::Day, "日"),
                    (Granularity::Month, "月"),
                    (Granularity::Year, "年"),
                ] {
                    if ui
                        .selectable_label(self.granularity == granularity, label)
                        .clicked()
                        && self.granularity != granularity
                    {
                        self.granularity = granularity;
                        self.selected_bucket = None;
                        self.reload_buckets();
                    }
                }
            });
        });

        egui::SidePanel::left("buckets")
            .default_width(180.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let selected = self.selected_bucket.clone();
                    let entries: Vec<(String, u64)> = self
                        .buckets
                        .iter()
                        .map(|bucket| (bucket.key.clone(), bucket.count))
                        .collect();
                    for (key, count) in entries {
                        let is_selected = selected.as_deref() == Some(key.as_str());
                        if ui
                            .selectable_label(is_selected, format!("{key}  ({count})"))
                            .clicked()
                            && !is_selected
                        {
                            self.select_bucket(key);
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(context, |ui| {
            if let Some(error) = &self.error {
                ui.colored_label(Color32::from_rgb(0xf8, 0x71, 0x71), error);
                ui.separator();
            }
            self.timeline_ui(ui);
        });

        self.viewer_ui(context);
    }
}

/// サーバーが生成した WebP サムネイルを egui の画像へ変換する。
fn decode_webp(bytes: &[u8]) -> Option<ColorImage> {
    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, decoded.as_raw()))
}
