//! Decoding message images within limits (PLAN.md, section 5.8; ADR 0007).
//!
//! An image in a message comes from someone else, through their homeserver:
//! its bytes may be crafted to make a decoder allocate gigabytes (a small
//! file declaring enormous dimensions, a decompression bomb, an animation
//! of thousands of frames). [`decode_image`] reads it in this order, so a
//! hostile file costs a header read, never the allocation:
//!
//! 1. Over [`ImageLimits::max_bytes`] is refused before anything is read.
//! 2. Text that looks like SVG or XML is refused ([`MediaError::Svg`]):
//!    remote SVG is never drawn, whatever the sender says it is.
//! 3. The format is sniffed from the bytes (never the sender's type) and
//!    only PNG (including APNG), JPEG, GIF and WebP are read.
//! 4. The decoder reads the header; dimensions over
//!    [`ImageLimits::max_side`] or [`ImageLimits::max_pixels`] are refused
//!    before any pixel is decoded. `image`'s own [`image::Limits`] are set
//!    from the same numbers as a second line.
//! 5. Animations are read a frame at a time and refused past
//!    [`ImageLimits::max_frames`] frames or
//!    [`ImageLimits::max_total_pixels`] pixels in all.
//! 6. Frames larger than [`ImageLimits::display_side`] are scaled down to
//!    fit, keeping their shape, so what the window keeps is bounded too.
//!
//! The result is plain RGBA frames (`familychat-core`'s
//! [`DecodedImage`]): there is no format left for a frontend to parse.

use std::io::Cursor;

use familychat_core::media::{DecodedImage, ImageFrame, MediaError};
use image::{
    AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, Limits, RgbaImage,
    codecs::{gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder},
    imageops::{self, FilterType},
};

/// The limits an image is decoded within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    /// The most encoded bytes read.
    pub max_bytes: u64,
    /// The widest or tallest image decoded, in pixels.
    pub max_side: u32,
    /// The most pixels one frame may have.
    pub max_pixels: u64,
    /// The most frames an animation may have.
    pub max_frames: usize,
    /// The most pixels all frames together may have.
    pub max_total_pixels: u64,
    /// Frames larger than this on either side are scaled down to fit.
    pub display_side: u32,
}

/// The limits for images in the timeline: 32 MiB of file (Family Chat's
/// server takes uploads of 11 MiB), 8,192 pixels a side, 32 megapixels a
/// frame (128 MiB of RGBA at most while decoding), 120 frames and 48
/// megapixels across an animation, kept at most 1,600 pixels a side.
pub const TIMELINE_IMAGE_LIMITS: ImageLimits = ImageLimits {
    max_bytes: 32 * 1024 * 1024,
    max_side: 8_192,
    max_pixels: 32 * 1024 * 1024,
    max_frames: 120,
    max_total_pixels: 48 * 1024 * 1024,
    display_side: 1_600,
};

/// The delay an animation frame gets when it asks for less than
/// [`MIN_FRAME_DELAY_MS`], as browsers do.
pub const DEFAULT_FRAME_DELAY_MS: u32 = 100;
/// Shorter frame delays than this are read as [`DEFAULT_FRAME_DELAY_MS`].
pub const MIN_FRAME_DELAY_MS: u32 = 20;

/// How far into the file the SVG check looks.
const SVG_SNIFF_BYTES: usize = 1024;

/// Whether `bytes` look like SVG or other XML text: an optional byte order
/// mark and whitespace, then `<svg`, `<?xml` or a doctype, or `<svg`
/// anywhere in the first kilobyte of text.
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let head = &bytes[start..bytes.len().min(start + SVG_SNIFF_BYTES)];
    if head.first() != Some(&b'<') {
        return false;
    }
    let lower = head.to_ascii_lowercase();
    lower.starts_with(b"<svg")
        || lower.starts_with(b"<?xml")
        || lower.starts_with(b"<!doctype svg")
        || lower.windows(4).any(|window| window == b"<svg")
}

/// Decodes `bytes` (PNG, JPEG, GIF or WebP, sniffed from the bytes, never
/// the sender's type) into RGBA frames within `limits`. SVG is refused.
pub fn decode_image(bytes: &[u8], limits: &ImageLimits) -> Result<DecodedImage, MediaError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limits.max_bytes {
        return Err(MediaError::TooLarge {
            limit: limits.max_bytes,
        });
    }
    if looks_like_svg(bytes) {
        return Err(MediaError::Svg);
    }
    let format = image::guess_format(bytes).map_err(|_| MediaError::NotAnImage)?;
    let reader = || Cursor::new(bytes);
    // A header over `image`'s own limits is too many pixels; anything
    // else it can't read is not an image.
    let not_an_image = |error: image::ImageError| match error {
        image::ImageError::Limits(_) => MediaError::TooManyPixels,
        _ => MediaError::NotAnImage,
    };
    match format {
        ImageFormat::Png => {
            let mut decoder =
                PngDecoder::with_limits(reader(), decoder_limits(limits)).map_err(not_an_image)?;
            check_dimensions(decoder.dimensions(), limits)?;
            if decoder.is_apng().map_err(not_an_image)? {
                let canvas = decoder.dimensions();
                let apng = decoder.apng().map_err(not_an_image)?;
                frames(apng, canvas, limits)
            } else {
                decoder
                    .set_limits(decoder_limits(limits))
                    .map_err(not_an_image)?;
                still(decoder, limits)
            }
        }
        ImageFormat::Jpeg => {
            let mut decoder = JpegDecoder::new(reader()).map_err(not_an_image)?;
            check_dimensions(decoder.dimensions(), limits)?;
            decoder
                .set_limits(decoder_limits(limits))
                .map_err(not_an_image)?;
            still(decoder, limits)
        }
        ImageFormat::Gif => {
            let mut decoder = GifDecoder::new(reader()).map_err(not_an_image)?;
            let canvas = decoder.dimensions();
            check_dimensions(canvas, limits)?;
            decoder
                .set_limits(decoder_limits(limits))
                .map_err(not_an_image)?;
            frames(decoder, canvas, limits)
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(reader()).map_err(not_an_image)?;
            let canvas = decoder.dimensions();
            check_dimensions(canvas, limits)?;
            decoder
                .set_limits(decoder_limits(limits))
                .map_err(not_an_image)?;
            if decoder.has_animation() {
                frames(decoder, canvas, limits)
            } else {
                still(decoder, limits)
            }
        }
        _ => Err(MediaError::NotAnImage),
    }
}

/// `image`'s own limits from ours: the second line behind
/// [`check_dimensions`].
fn decoder_limits(limits: &ImageLimits) -> Limits {
    let mut decoder = Limits::default();
    decoder.max_image_width = Some(limits.max_side);
    decoder.max_image_height = Some(limits.max_side);
    // One RGBA frame of the largest size, plus the decoder's own buffers.
    decoder.max_alloc = Some(limits.max_pixels.saturating_mul(4).saturating_mul(2));
    decoder
}

/// Refuses dimensions over the limits (or empty ones).
fn check_dimensions((width, height): (u32, u32), limits: &ImageLimits) -> Result<(), MediaError> {
    if width == 0 || height == 0 {
        return Err(MediaError::NotAnImage);
    }
    if width > limits.max_side
        || height > limits.max_side
        || u64::from(width) * u64::from(height) > limits.max_pixels
    {
        return Err(MediaError::TooManyPixels);
    }
    Ok(())
}

/// Decodes a still image.
fn still(decoder: impl ImageDecoder, limits: &ImageLimits) -> Result<DecodedImage, MediaError> {
    let image = DynamicImage::from_decoder(decoder).map_err(|error| match error {
        image::ImageError::Limits(_) => MediaError::TooManyPixels,
        _ => MediaError::NotAnImage,
    })?;
    let frame = fit(image.into_rgba8(), limits.display_side);
    Ok(DecodedImage {
        width: frame.width(),
        height: frame.height(),
        frames: vec![ImageFrame {
            rgba: frame.into_raw(),
            delay_ms: 0,
        }],
    })
}

/// Decodes an animation a frame at a time, within the frame limits. Each
/// frame is composited onto the `canvas` (the animation's own size), so
/// the next frame's cost is known before it is decoded.
fn frames<'a>(
    decoder: impl AnimationDecoder<'a>,
    canvas: (u32, u32),
    limits: &ImageLimits,
) -> Result<DecodedImage, MediaError> {
    let canvas_pixels = u64::from(canvas.0) * u64::from(canvas.1);
    let mut frames = Vec::new();
    let mut total_pixels: u64 = 0;
    let mut size = None;
    for frame in decoder.into_frames() {
        if frames.len() >= limits.max_frames
            || total_pixels.saturating_add(canvas_pixels) > limits.max_total_pixels
        {
            return Err(MediaError::TooManyFrames);
        }
        let frame = frame.map_err(|error| match error {
            image::ImageError::Limits(_) => MediaError::TooManyPixels,
            _ => MediaError::NotAnImage,
        })?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let delay_ms = numerator.checked_div(denominator).unwrap_or(0);
        let delay_ms = if delay_ms < MIN_FRAME_DELAY_MS {
            DEFAULT_FRAME_DELAY_MS
        } else {
            delay_ms
        };
        let buffer = frame.into_buffer();
        check_dimensions(buffer.dimensions(), limits)?;
        total_pixels =
            total_pixels.saturating_add(u64::from(buffer.width()) * u64::from(buffer.height()));
        if total_pixels > limits.max_total_pixels {
            return Err(MediaError::TooManyFrames);
        }
        let buffer = fit(buffer, limits.display_side);
        // Every frame the size of the first (a composited animation's
        // frames all are; anything else is broken).
        match size {
            None => size = Some(buffer.dimensions()),
            Some(first) if first != buffer.dimensions() => return Err(MediaError::NotAnImage),
            Some(_) => {}
        }
        frames.push(ImageFrame {
            rgba: buffer.into_raw(),
            delay_ms,
        });
    }
    let Some((width, height)) = size else {
        return Err(MediaError::NotAnImage);
    };
    if frames.len() == 1
        && let Some(frame) = frames.first_mut()
    {
        frame.delay_ms = 0;
    }
    Ok(DecodedImage {
        width,
        height,
        frames,
    })
}

/// `image` scaled down to fit `side` on both sides, keeping its shape;
/// unchanged when it fits.
fn fit(image: RgbaImage, side: u32) -> RgbaImage {
    let (width, height) = image.dimensions();
    if width <= side && height <= side {
        return image;
    }
    let scale = f64::from(side) / f64::from(width.max(height));
    let scaled = |value: u32| ((f64::from(value) * scale).round() as u32).clamp(1, side);
    imageops::resize(&image, scaled(width), scaled(height), FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_recognised_however_it_starts() {
        for text in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
            b"  \n<?xml version='1.0'?><svg/>",
            b"\xEF\xBB\xBF<SVG/>",
            b"<!DOCTYPE svg PUBLIC><svg/>",
            b"<!-- c --><svg/>",
        ] {
            assert!(looks_like_svg(text), "{:?}", String::from_utf8_lossy(text));
            assert_eq!(
                decode_image(text, &TIMELINE_IMAGE_LIMITS),
                Err(MediaError::Svg)
            );
        }
        assert!(!looks_like_svg(b"\x89PNG\r\n"));
        assert!(!looks_like_svg(b"hello <svg"));
    }

    #[test]
    fn large_frames_are_scaled_to_fit() {
        let image = RgbaImage::new(4_000, 1_000);
        let fitted = fit(image, 1_600);
        assert_eq!(fitted.dimensions(), (1_600, 400));
        let small = fit(RgbaImage::new(10, 20), 1_600);
        assert_eq!(small.dimensions(), (10, 20));
    }
}
