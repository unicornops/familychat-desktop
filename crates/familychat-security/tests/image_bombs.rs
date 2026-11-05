//! Image bombs (PLAN.md, sections 5.8 and 9; ADR 0007): files crafted to
//! make a decoder allocate far more than they weigh, refused by
//! `decode_image` from their headers, quickly; SVG refused whatever it is
//! called; broken files refused; and real images decoded.

use std::{
    io::Cursor,
    time::{Duration, Instant},
};

use familychat_core::media::{DecodedImage, MediaError};
use familychat_security::images::{ImageLimits, TIMELINE_IMAGE_LIMITS, decode_image};
use image::{
    Delay, Frame, ImageBuffer, ImageEncoder, Luma, Rgba, RgbaImage,
    codecs::{
        gif::{GifEncoder, Repeat},
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType, PngEncoder},
        webp::WebPEncoder,
    },
};

/// How long refusing a bomb may take, in a debug build.
const REFUSAL_BUDGET: Duration = Duration::from_millis(500);

fn decode(bytes: &[u8]) -> Result<DecodedImage, MediaError> {
    decode_image(bytes, &TIMELINE_IMAGE_LIMITS)
}

/// Asserts `bytes` are refused with `expected`, within the budget.
fn assert_refused(what: &str, bytes: &[u8], expected: MediaError) {
    let started = Instant::now();
    let result = decode(bytes);
    let elapsed = started.elapsed();
    assert_eq!(result, Err(expected), "{what}");
    assert!(
        elapsed < REFUSAL_BUDGET,
        "{what} took {elapsed:?} to refuse"
    );
}

// --- PNG by hand -------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).expect("small").to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    png.extend_from_slice(&body);
    png.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A PNG whose header claims `width` by `height` (8-bit greyscale), with a
/// token image data chunk.
fn png_claiming(width: u32, height: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    // An empty zlib stream: a stored block of nothing.
    chunk(
        &mut png,
        b"IDAT",
        &[
            0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01,
        ],
    );
    chunk(&mut png, b"IEND", &[]);
    png
}

// --- GIF by hand -------------------------------------------------------------

/// A GIF with a `width` by `height` logical screen and `frames` frames,
/// each one 1×1 pixel in the corner (the classic smallest image data).
fn gif_with(width: u16, height: u16, frames: usize) -> Vec<u8> {
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&width.to_le_bytes());
    gif.extend_from_slice(&height.to_le_bytes());
    // A global colour table of two colours, black and white.
    gif.extend_from_slice(&[0x80, 0, 0]);
    gif.extend_from_slice(&[0, 0, 0, 0xFF, 0xFF, 0xFF]);
    for _ in 0..frames {
        // A graphic control extension: 10 ms, no transparency.
        gif.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // The image descriptor: at 0,0, 1×1, no local table.
        gif.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
        // LZW, minimum code size 2: clear, 0, end.
        gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
    }
    gif.push(0x3B);
    gif
}

// --- Encoded images ----------------------------------------------------------

fn sample(width: u32, height: u32) -> RgbaImage {
    ImageBuffer::from_fn(width, height, |x, y| {
        Rgba([(x * 40) as u8, (y * 60) as u8, 128, 255])
    })
}

fn png_of(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .expect("the PNG encodes");
    bytes
}

// --- The bombs ---------------------------------------------------------------

#[test]
fn a_png_claiming_huge_dimensions_is_refused_from_its_header() {
    for (width, height) in [
        (100_000, 100_000),
        (8_193, 1),
        (1, 1_000_000),
        (u32::MAX >> 1, 2),
    ] {
        let png = png_claiming(width, height);
        assert!(png.len() < 100);
        assert_refused(
            &format!("{width}×{height} PNG"),
            &png,
            MediaError::TooManyPixels,
        );
    }
}

#[test]
fn a_decompression_bomb_is_refused_before_it_inflates() {
    // A real, valid PNG: 6,000×6,000 grey pixels (36 megapixels, within
    // 8,192 a side but over 32 megapixels), a few kilobytes of file.
    let side = 6_000;
    let grey: ImageBuffer<Luma<u8>, Vec<u8>> = ImageBuffer::new(side, side);
    let mut bomb = Vec::new();
    PngEncoder::new_with_quality(&mut bomb, CompressionType::Fast, FilterType::NoFilter)
        .write_image(grey.as_raw(), side, side, image::ExtendedColorType::L8)
        .expect("the bomb encodes");
    drop(grey);
    assert!(
        bomb.len() < 2 * 1024 * 1024,
        "the bomb is {} bytes",
        bomb.len()
    );
    assert_refused("a 36 megapixel PNG", &bomb, MediaError::TooManyPixels);

    // The same rule on tighter limits, with a file that would otherwise
    // decode: 2,000×2,000 against one megapixel.
    let tight = ImageLimits {
        max_pixels: 1_000_000,
        ..TIMELINE_IMAGE_LIMITS
    };
    let small_bomb = png_of(&RgbaImage::new(2_000, 2_000));
    assert_eq!(
        decode_image(&small_bomb, &tight),
        Err(MediaError::TooManyPixels)
    );
}

#[test]
fn a_gif_with_an_enormous_screen_is_refused() {
    let gif = gif_with(65_535, 65_535, 1);
    assert!(gif.len() < 64);
    assert_refused("a 65535×65535 GIF", &gif, MediaError::TooManyPixels);
}

#[test]
fn an_animation_with_too_many_frames_is_refused() {
    let frames = TIMELINE_IMAGE_LIMITS.max_frames + 50;
    let gif = gif_with(1, 1, frames);
    assert_refused(
        &format!("a {frames}-frame GIF"),
        &gif,
        MediaError::TooManyFrames,
    );
    // Exactly the limit is fine.
    let gif = gif_with(1, 1, TIMELINE_IMAGE_LIMITS.max_frames);
    let decoded = decode(&gif).expect("the limit decodes");
    assert_eq!(decoded.frames.len(), TIMELINE_IMAGE_LIMITS.max_frames);
}

#[test]
fn an_animation_over_the_pixel_budget_is_refused() {
    // A 1,000×1,000 screen (each composited frame is a megapixel) with 100
    // tiny frames: 100 megapixels in all, over 48, in a file of 2.3 KB.
    let gif = gif_with(1_000, 1_000, 100);
    assert!(gif.len() < 4_096);
    // On the timeline's limits it decodes 48 frames (as a real 48-frame
    // animation of that size would) and stops before the 49th; on tighter
    // ones the same file stops after four, quickly.
    let tight = ImageLimits {
        max_total_pixels: 4_000_000,
        ..TIMELINE_IMAGE_LIMITS
    };
    let started = Instant::now();
    assert_eq!(decode_image(&gif, &tight), Err(MediaError::TooManyFrames));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(decode(&gif), Err(MediaError::TooManyFrames));
}

#[test]
fn svg_is_refused_whatever_it_is_called() {
    for (what, bytes) in [
        (
            "<svg>",
            &b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"[..],
        ),
        (
            "<?xml>",
            b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ),
        ("a BOM", b"\xEF\xBB\xBF  <svg/>"),
        ("a doctype", b"<!DOCTYPE svg><svg/>"),
    ] {
        // The sender's file name and type play no part: only the bytes.
        assert_refused(what, bytes, MediaError::Svg);
    }
}

#[test]
fn broken_and_unknown_files_are_refused() {
    let png = png_of(&sample(8, 8));
    assert_refused(
        "a truncated PNG",
        &png[..png.len() / 2],
        MediaError::NotAnImage,
    );
    assert_refused("a PNG signature alone", &png[..8], MediaError::NotAnImage);
    assert_refused("nothing", b"", MediaError::NotAnImage);
    assert_refused(
        "text",
        b"hello, this is not an image",
        MediaError::NotAnImage,
    );
    let mut noise = vec![0u8; 4096];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for byte in &mut noise {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    assert_refused("noise", &noise, MediaError::NotAnImage);
    // A BMP: a format GPUI could read, but messages never use.
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0; 52]);
    assert_refused("a BMP", &bmp, MediaError::NotAnImage);
    // A zero-sized image.
    assert_refused("a 0×0 PNG", &png_claiming(0, 0), MediaError::NotAnImage);
}

#[test]
fn files_over_the_byte_limit_are_refused_unread() {
    let limits = ImageLimits {
        max_bytes: 64,
        ..TIMELINE_IMAGE_LIMITS
    };
    let png = png_of(&sample(64, 64));
    assert!(png.len() > 64);
    assert_eq!(
        decode_image(&png, &limits),
        Err(MediaError::TooLarge { limit: 64 })
    );
    let max = usize::try_from(TIMELINE_IMAGE_LIMITS.max_bytes).expect("fits");
    let oversized = vec![0u8; max + 1];
    assert_refused(
        "a file over 32 MiB",
        &oversized,
        MediaError::TooLarge {
            limit: TIMELINE_IMAGE_LIMITS.max_bytes,
        },
    );
}

// --- Real images ---------------------------------------------------------------

#[test]
fn real_png_jpeg_and_webp_images_decode() {
    let image = sample(3, 2);
    let png = decode(&png_of(&image)).expect("the PNG decodes");
    assert_eq!((png.width, png.height, png.frames.len()), (3, 2, 1));
    assert_eq!(png.frames[0].rgba, image.as_raw().clone());
    assert_eq!(png.frames[0].delay_ms, 0);

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&image::DynamicImage::ImageRgba8(sample(16, 9)).to_rgb8())
        .expect("the JPEG encodes");
    let jpeg = decode(&jpeg).expect("the JPEG decodes");
    assert_eq!((jpeg.width, jpeg.height), (16, 9));
    assert_eq!(jpeg.frames[0].rgba.len(), 16 * 9 * 4);

    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .encode(image.as_raw(), 3, 2, image::ExtendedColorType::Rgba8)
        .expect("the WebP encodes");
    let webp = decode(&webp).expect("the WebP decodes");
    assert_eq!((webp.width, webp.height), (3, 2));
    assert_eq!(webp.frames[0].rgba, image.as_raw().clone());
}

#[test]
fn an_animated_gif_decodes_every_frame() {
    let mut gif = Vec::new();
    {
        let mut encoder = GifEncoder::new(Cursor::new(&mut gif));
        encoder.set_repeat(Repeat::Infinite).expect("repeat");
        for colour in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]] {
            let frame = Frame::from_parts(
                ImageBuffer::from_pixel(4, 4, Rgba(colour)),
                0,
                0,
                Delay::from_numer_denom_ms(80, 1),
            );
            encoder.encode_frame(frame).expect("the frame encodes");
        }
    }
    let decoded = decode(&gif).expect("the GIF decodes");
    assert!(decoded.is_animated());
    assert_eq!((decoded.width, decoded.height), (4, 4));
    assert_eq!(decoded.frames.len(), 3);
    assert!(decoded.frames.iter().all(|frame| frame.delay_ms == 80));
    assert_eq!(&decoded.frames[1].rgba[..4], &[0, 255, 0, 255]);

    // A one-frame GIF is a still image.
    let still = decode(&gif_with(1, 1, 1)).expect("one frame decodes");
    assert_eq!(still.frames.len(), 1);
    assert_eq!(still.frames[0].delay_ms, 0);
}

#[test]
fn large_images_are_scaled_down_to_the_display_size() {
    let wide = png_of(&RgbaImage::new(4_000, 100));
    let decoded = decode(&wide).expect("the wide PNG decodes");
    assert_eq!((decoded.width, decoded.height), (1_600, 40));
    assert_eq!(decoded.frames[0].rgba.len(), 1_600 * 40 * 4);
}
