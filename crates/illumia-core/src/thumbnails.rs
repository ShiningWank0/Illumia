//! `thumbnail` job payload, enqueue helper, and image generation handler.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use fast_image_resize::{
    PixelType, Resizer,
    images::{Image, ImageRef},
};
use image::GenericImageView;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    db::{Database, Error, Result},
    images,
    jobs::{Job, JobQueue},
};

pub const THUMBNAIL_JOB_KIND: &str = "thumbnail";
pub const THUMBNAIL_PRIORITY: i64 = 100;

const THUMBNAIL_LONG_EDGE: u32 = 240;
const PREVIEW_LONG_EDGE: u32 = 1440;
const THUMBHASH_LONG_EDGE: u32 = 100;
const WEBP_QUALITY: f32 = 80.0;

pub(crate) struct InMemoryVariants {
    pub thumbnail: Vec<u8>,
    pub preview: Vec<u8>,
    pub thumbhash: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ThumbnailPayload {
    pub asset_id: String,
}

/// Enqueues thumbnail generation after the server-layer ingest caller succeeds.
pub fn enqueue_thumbnail(database: &Database, asset_id: &str) -> Result<Job> {
    let payload = serde_json::to_string(&ThumbnailPayload {
        asset_id: asset_id.to_owned(),
    })?;
    JobQueue::new(database.clone()).enqueue(THUMBNAIL_JOB_KIND, &payload, THUMBNAIL_PRIORITY)
}

/// Handler suitable for [`crate::jobs::JobRunner::register_handler`].
pub fn handle_thumbnail_job(database: &Database, job: &Job) -> Result<()> {
    let payload: ThumbnailPayload = serde_json::from_str(&job.payload)?;
    generate_thumbnails(database, &payload.asset_id)
}

/// Generates missing thumbnail artifacts and the database ThumbHash.
pub fn generate_thumbnails(database: &Database, asset_id: &str) -> Result<()> {
    let Some(asset) = thumbnail_asset(database, asset_id)? else {
        return Ok(());
    };
    Uuid::parse_str(asset_id).map_err(|_| Error::InvalidAssetPath)?;

    let thumbs_dir = database.data_root().join("thumbs");
    let thumbnail_path = thumbs_dir.join(format!("{asset_id}_t.webp"));
    let preview_path = thumbs_dir.join(format!("{asset_id}_p.webp"));
    let needs_thumbnail = !thumbnail_path.is_file();
    let needs_preview = !preview_path.is_file();
    let needs_thumbhash = !asset.has_thumbhash;
    if !needs_thumbnail && !needs_preview && !needs_thumbhash {
        return Ok(());
    }

    let source_path = database.data_root().join(asset.library_path);
    let source_bytes = match fs::read(source_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if thumbnail_asset(database, asset_id)?.is_none() {
                return Ok(());
            }
            return Err(error.into());
        }
        Err(error) => return Err(error.into()),
    };
    let decoded = images::decode(&source_bytes, &asset.ext)?;
    let (source_width, source_height) = decoded.dimensions();
    let rgba = decoded.to_rgba8().into_raw();

    let thumbnail = needs_thumbnail
        .then(|| encode_variant(&rgba, source_width, source_height, THUMBNAIL_LONG_EDGE))
        .transpose()?;
    let preview = needs_preview
        .then(|| encode_variant(&rgba, source_width, source_height, PREVIEW_LONG_EDGE))
        .transpose()?;
    let hash = needs_thumbhash
        .then(|| make_thumbhash(&rgba, source_width, source_height))
        .transpose()?;

    // Recheck the tombstone while holding the shared DB lock. Purge cannot mark
    // the asset between this check, the writes, and the ThumbHash update.
    database.with_connection(|connection| {
        let state = connection
            .query_row(
                "SELECT lifecycle, thumbhash IS NOT NULL
                 FROM assets WHERE id = ?1",
                [asset_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .optional()?;
        let Some((lifecycle, has_thumbhash)) = state else {
            return Ok(());
        };
        if lifecycle == "purging" {
            return Ok(());
        }

        fs::create_dir_all(&thumbs_dir)?;
        if let Some(bytes) = thumbnail.as_ref().filter(|_| !thumbnail_path.is_file()) {
            fs::write(&thumbnail_path, bytes)?;
        }
        if let Some(bytes) = preview.as_ref().filter(|_| !preview_path.is_file()) {
            fs::write(&preview_path, bytes)?;
        }
        if !has_thumbhash && let Some(hash) = &hash {
            connection.execute(
                "UPDATE assets SET thumbhash = ?2
                 WHERE id = ?1 AND lifecycle != 'purging'",
                rusqlite::params![asset_id, hash],
            )?;
        }
        Ok(())
    })
}

/// Vault 向けに平文ファイルを作らず、全派生画像をメモリ内で生成する。
pub(crate) fn generate_variants_in_memory(
    source_bytes: &[u8],
    extension: &str,
) -> Result<InMemoryVariants> {
    let decoded = images::decode(source_bytes, extension)?;
    let (source_width, source_height) = decoded.dimensions();
    let rgba = decoded.to_rgba8().into_raw();
    Ok(InMemoryVariants {
        thumbnail: encode_variant(&rgba, source_width, source_height, THUMBNAIL_LONG_EDGE)?,
        preview: encode_variant(&rgba, source_width, source_height, PREVIEW_LONG_EDGE)?,
        thumbhash: make_thumbhash(&rgba, source_width, source_height)?,
    })
}

#[derive(Debug)]
struct ThumbnailAsset {
    library_path: PathBuf,
    ext: String,
    has_thumbhash: bool,
}

fn thumbnail_asset(database: &Database, asset_id: &str) -> Result<Option<ThumbnailAsset>> {
    database.with_connection(|connection| {
        let record = connection
            .query_row(
                "SELECT library_path, ext, thumbhash IS NOT NULL, lifecycle
                 FROM assets WHERE id = ?1",
                [asset_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((library_path, ext, has_thumbhash, lifecycle)) = record else {
            return Ok(None);
        };
        if lifecycle == "purging" {
            return Ok(None);
        }
        Ok(Some(ThumbnailAsset {
            library_path: checked_relative_path(&library_path)?,
            ext,
            has_thumbhash,
        }))
    })
}

fn checked_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidAssetPath);
    }
    Ok(path.to_path_buf())
}

fn encode_variant(
    rgba: &[u8],
    source_width: u32,
    source_height: u32,
    long_edge: u32,
) -> Result<Vec<u8>> {
    let (width, height) = scaled_dimensions(source_width, source_height, long_edge);
    let pixels = resize_rgba(rgba, source_width, source_height, width, height)?;
    let encoded = webp::Encoder::from_rgba(&pixels, width, height)
        .encode_simple(false, WEBP_QUALITY)
        .map_err(|error| Error::WebpEncoding(format!("{error:?}")))?;
    Ok(encoded.to_vec())
}

fn make_thumbhash(rgba: &[u8], source_width: u32, source_height: u32) -> Result<Vec<u8>> {
    let (width, height) = scaled_dimensions(source_width, source_height, THUMBHASH_LONG_EDGE);
    let pixels = resize_rgba(rgba, source_width, source_height, width, height)?;
    let width =
        usize::try_from(width).map_err(|error| Error::ImageProcessing(error.to_string()))?;
    let height =
        usize::try_from(height).map_err(|error| Error::ImageProcessing(error.to_string()))?;
    Ok(thumbhash::rgba_to_thumb_hash(width, height, &pixels))
}

fn resize_rgba(
    rgba: &[u8],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    if source_width == width && source_height == height {
        return Ok(rgba.to_vec());
    }
    let source = ImageRef::new(source_width, source_height, rgba, PixelType::U8x4)
        .map_err(|error| Error::ImageProcessing(error.to_string()))?;
    let mut destination = Image::new(width, height, PixelType::U8x4);
    Resizer::new()
        .resize(&source, &mut destination, None)
        .map_err(|error| Error::ImageProcessing(error.to_string()))?;
    Ok(destination.into_vec())
}

fn scaled_dimensions(width: u32, height: u32, long_edge: u32) -> (u32, u32) {
    if width.max(height) <= long_edge {
        return (width, height);
    }
    if width >= height {
        let scaled_height = ((u64::from(height) * u64::from(long_edge) + u64::from(width) / 2)
            / u64::from(width))
        .max(1);
        (long_edge, u32::try_from(scaled_height).unwrap_or(long_edge))
    } else {
        let scaled_width = ((u64::from(width) * u64::from(long_edge) + u64::from(height) / 2)
            / u64::from(height))
        .max(1);
        (u32::try_from(scaled_width).unwrap_or(long_edge), long_edge)
    }
}
