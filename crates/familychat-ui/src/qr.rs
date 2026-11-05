//! Reading a sign-in QR code from an image (PLAN.md, section 6.7): a parent
//! shows the code in the Family Chat panel, which a desktop without a
//! camera gets as a copied image or a screenshot. The person pastes it or
//! drops the file on the window, and [`decode_signin_qr`] finds the QR code
//! in it with `rqrr`, on this computer: the image is never uploaded or
//! kept, and the text it decodes to goes straight to the sign-in as a link
//! (`SigninStart::Link`), where every rule about it is applied.
//!
//! Decoding is CPU work, so the window runs it on GPUI's background
//! executor, never on the UI thread. The image is bounded before it is
//! decoded: at most [`MAX_IMAGE_BYTES`] of file, [`MAX_IMAGE_SIDE`] pixels a
//! side and [`MAX_DECODED_BYTES`] of pixels, so a hostile file cannot make
//! the decoder allocate without limit. Large screenshots are scaled down to
//! [`SCAN_SIDE`] before the scan, which a QR code on a panel page survives.

use std::io::Cursor;

use image::{DynamicImage, ImageReader, Limits, imageops::FilterType};
use zeroize::Zeroize;

/// The largest image file read: a full-screen screenshot of a 6K display
/// as PNG fits.
pub(crate) const MAX_IMAGE_BYTES: usize = 40 * 1024 * 1024;

/// The longest side of an image that is decoded.
pub(crate) const MAX_IMAGE_SIDE: u32 = 8192;

/// The most memory the decoded pixels may take.
pub(crate) const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// Images with a longer side are scaled down to this before the scan.
pub(crate) const SCAN_SIDE: u32 = 2000;

/// Why an image gave no sign-in link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QrError {
    /// The file is too large to read.
    TooLarge,
    /// It is not an image Family Chat can read (or it is damaged).
    NotAnImage,
    /// There is no QR code in it that could be read.
    NoCode,
}

impl QrError {
    /// What the window says.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::TooLarge => "That image is too large to read. Try a smaller screenshot.",
            Self::NotAnImage => "Family Chat can't read that file as an image.",
            Self::NoCode => {
                "Family Chat couldn't find a QR code in that image. Make sure the whole code is \
                 in the picture, or paste the sign-in link instead."
            }
        }
    }
}

/// The text of the QR code in the image `bytes` (PNG, JPEG, GIF, BMP,
/// WebP, or TIFF as macOS may copy a screenshot). The caller wipes the
/// bytes; the text is the caller's to wipe.
pub(crate) fn decode_signin_qr(bytes: &[u8]) -> Result<String, QrError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(QrError::TooLarge);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| QrError::NotAnImage)?;
    reader.limits(limits);
    let image = reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => QrError::TooLarge,
        _ => QrError::NotAnImage,
    })?;
    scan(image)
}

/// Finds and decodes the first QR code in `image`.
fn scan(image: DynamicImage) -> Result<String, QrError> {
    let longest = image.width().max(image.height());
    let image = if longest > SCAN_SIDE {
        image.resize(SCAN_SIDE, SCAN_SIDE, FilterType::Triangle)
    } else {
        image
    };
    let mut grey = image.into_luma8();
    let (width, height) = (grey.width() as usize, grey.height() as usize);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        grey.get_pixel(x as u32, y as u32).0[0]
    });
    // The pixels may show the code; they are not kept.
    grey.as_mut().zeroize();
    for grid in prepared.detect_grids() {
        if let Ok((_, text)) = grid.decode() {
            return Ok(text);
        }
    }
    Err(QrError::NoCode)
}

#[cfg(test)]
mod tests {
    use familychat_test_support::fixtures::FIXTURE_LINK;
    use image::{ImageFormat, Rgb, RgbImage};

    use super::*;

    /// A screenshot of a panel page with the sign-in QR code for
    /// `FIXTURE_LINK` on it, made with python-qrcode and Pillow for this
    /// test (error correction M, 6px modules).
    const SCREENSHOT: &[u8] = include_bytes!("testdata/signin-qr-screenshot.png");

    fn encode(image: &RgbImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(image.clone())
            .write_to(&mut Cursor::new(&mut bytes), format)
            .expect("the image encodes");
        bytes
    }

    #[test]
    fn a_screenshot_of_the_code_decodes_to_its_link() {
        assert_eq!(decode_signin_qr(SCREENSHOT).as_deref(), Ok(FIXTURE_LINK));
    }

    #[test]
    fn a_large_screenshot_is_scaled_down_and_still_reads() {
        let small = image::load_from_memory(SCREENSHOT).expect("the fixture loads");
        let large = small
            .resize_exact(3600, 2560, FilterType::Nearest)
            .into_rgb8();
        let bytes = encode(&large, ImageFormat::Png);
        assert_eq!(decode_signin_qr(&bytes).as_deref(), Ok(FIXTURE_LINK));
    }

    #[test]
    fn other_images_and_files_say_why() {
        let blank = RgbImage::from_pixel(200, 120, Rgb([255, 255, 255]));
        assert_eq!(
            decode_signin_qr(&encode(&blank, ImageFormat::Png)),
            Err(QrError::NoCode)
        );
        assert_eq!(
            decode_signin_qr(b"https://safechat.family/app/login"),
            Err(QrError::NotAnImage)
        );
        assert_eq!(
            decode_signin_qr(&vec![0; MAX_IMAGE_BYTES + 1]),
            Err(QrError::TooLarge)
        );
    }

    #[test]
    fn an_image_beyond_the_limits_is_not_decoded() {
        // A tiny PNG that claims to be 9000 pixels wide.
        let wide = RgbImage::from_pixel(MAX_IMAGE_SIDE + 1, 1, Rgb([0, 0, 0]));
        assert_eq!(
            decode_signin_qr(&encode(&wide, ImageFormat::Png)),
            Err(QrError::TooLarge)
        );
    }

    #[test]
    fn the_messages_say_what_to_do() {
        for error in [QrError::TooLarge, QrError::NotAnImage, QrError::NoCode] {
            assert!(error.message().ends_with('.'));
        }
        assert!(QrError::NoCode.message().contains("paste the sign-in link"));
    }
}
