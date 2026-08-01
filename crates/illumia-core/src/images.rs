//! Resource-bounded decoding for untrusted uploaded images.

use std::{
    io::Cursor,
    path::Path,
    sync::{Condvar, LazyLock, Mutex},
};

use image::{DynamicImage, ImageFormat, ImageReader, Limits};

use crate::db::{Error, Result};

pub const MAX_ASSET_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 32_768;
pub const MAX_IMAGE_PIXELS: u64 = 100_000_000;
pub const MAX_DECODE_ALLOCATION: u64 = 512 * 1024 * 1024;
pub const MAX_CONCURRENT_IMAGE_DECODES: usize = 2;
pub const MAX_ORIGINAL_NAME_BYTES: usize = 1024;
pub const MAX_ORIGINAL_NAME_CHARS: usize = 255;

static DECODE_LIMITER: LazyLock<DecodeLimiter> = LazyLock::new(DecodeLimiter::new);

struct DecodeLimiter {
    active: Mutex<usize>,
    available: Condvar,
}

struct DecodePermit(&'static DecodeLimiter);

impl DecodeLimiter {
    const fn new() -> Self {
        Self {
            active: Mutex::new(0),
            available: Condvar::new(),
        }
    }

    fn acquire(&'static self) -> DecodePermit {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while *active >= MAX_CONCURRENT_IMAGE_DECODES {
            active = self
                .available
                .wait(active)
                .unwrap_or_else(|error| error.into_inner());
        }
        *active += 1;
        DecodePermit(self)
    }
}

impl Drop for DecodePermit {
    fn drop(&mut self) {
        let mut active = self
            .0
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active = active.saturating_sub(1);
        self.0.available.notify_one();
    }
}

/// Returns the canonical extension after validating metadata that can later be
/// surfaced in HTTP headers and the UI.
pub fn normalized_extension(original_name: &str) -> Result<String> {
    validate_original_name(original_name)?;
    let extension = Path::new(original_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpeg" => Ok("jpg".to_owned()),
        "jpg" | "png" | "webp" | "gif" => Ok(extension),
        _ => Err(Error::UnsupportedExtension(extension)),
    }
}

/// Decodes an image only with the parser selected by its validated extension.
///
/// The initial format check prevents a file named as one supported type from
/// reaching another decoder through content sniffing. Dimensions are inspected
/// before the full pixel buffer is allocated.
pub fn decode(bytes: &[u8], extension: &str) -> Result<DynamicImage> {
    let _permit = DECODE_LIMITER.acquire();
    if bytes.is_empty() {
        return Err(Error::InvalidImage("image is empty".to_owned()));
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(Error::InvalidImage(
            "image exceeds the size limit".to_owned(),
        ));
    }
    let expected = format_for_extension(extension)?;
    let detected = image::guess_format(bytes)
        .map_err(|_| Error::InvalidImage("unrecognized image format".to_owned()))?;
    if detected != expected {
        return Err(Error::InvalidImage(
            "file extension does not match image format".to_owned(),
        ));
    }

    let (width, height) = reader(bytes, expected)
        .into_dimensions()
        .map_err(|_| Error::InvalidImage("image header is invalid".to_owned()))?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::InvalidImage("image dimensions overflow".to_owned()))?;
    if width == 0 || height == 0 || pixels > MAX_IMAGE_PIXELS {
        return Err(Error::InvalidImage(
            "image dimensions exceed the limit".to_owned(),
        ));
    }

    reader(bytes, expected)
        .decode()
        .map_err(|_| Error::InvalidImage("image data is invalid".to_owned()))
}

fn reader(bytes: &[u8], format: ImageFormat) -> ImageReader<Cursor<&[u8]>> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOCATION);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    reader
}

fn format_for_extension(extension: &str) -> Result<ImageFormat> {
    match extension {
        "jpg" => Ok(ImageFormat::Jpeg),
        "png" => Ok(ImageFormat::Png),
        "webp" => Ok(ImageFormat::WebP),
        "gif" => Ok(ImageFormat::Gif),
        other => Err(Error::UnsupportedExtension(other.to_owned())),
    }
}

fn validate_original_name(original_name: &str) -> Result<()> {
    if original_name.is_empty()
        || original_name.len() > MAX_ORIGINAL_NAME_BYTES
        || original_name.chars().count() > MAX_ORIGINAL_NAME_CHARS
        || original_name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(Error::InvalidImage(
            "invalid original filename metadata".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, RgbaImage};

    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::new(width, height);
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut output, ImageFormat::Png)
            .expect("test PNG should encode");
        output.into_inner()
    }

    #[test]
    fn extension_selects_the_only_allowed_decoder() {
        let bytes = png(1, 1);
        assert!(decode(&bytes, "png").is_ok());
        assert!(matches!(decode(&bytes, "jpg"), Err(Error::InvalidImage(_))));
    }

    #[test]
    fn dimensions_and_filename_metadata_are_bounded() {
        let bytes = png(MAX_IMAGE_DIMENSION + 1, 1);
        assert!(matches!(decode(&bytes, "png"), Err(Error::InvalidImage(_))));
        assert!(normalized_extension("../../secret.png").is_err());
        assert!(normalized_extension("bad\nname.png").is_err());
        assert_eq!(
            normalized_extension("作品.JPEG").expect("valid name"),
            "jpg"
        );
    }
}
