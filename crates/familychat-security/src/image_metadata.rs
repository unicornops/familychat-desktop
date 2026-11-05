//! Taking the location out of a picture before it is sent (PLAN.md,
//! sections 5.8 and 13.7; ADR 0012).
//!
//! Phones write where a photo was taken into it: the EXIF GPS block, and
//! often an XMP packet with the same coordinates. A child sending a photo
//! of their bedroom must not send their address with it, so every picture
//! goes through [`remove_location`] before upload, by default.
//!
//! The work is lossless where the format allows it: the file's own
//! structure is walked and the metadata blocks are left out, so the pixels
//! are the sender's bytes, untouched.
//!
//! | Format | Removed | Kept |
//! | --- | --- | --- |
//! | JPEG | EXIF (`APP1`, rebuilt with the orientation only), XMP (`APP1`), Photoshop and IPTC (`APP13`), FlashPix and multi-picture (`APP2` other than the colour profile), comments, and anything after the image's end (the extra pictures a multi-picture file carries) | the image data, JFIF, the colour profile, Adobe's colour transform |
//! | PNG | `eXIf`, `tEXt`, `zTXt`, `iTXt` (where XMP lives), `tIME`, and anything after `IEND` | every other chunk |
//! | WebP | the `EXIF` and `XMP ` chunks, and their flags in `VP8X` | every other chunk |
//! | GIF | XMP application extensions and comments | every other block |
//!
//! A file that says it is one of these but can't be walked (damaged, or
//! built to confuse a parser) is decoded within `images`' limits and
//! encoded again, which keeps no metadata at all. Pictures in formats this
//! module can't rewrite (HEIC, AVIF, TIFF) are refused: their location
//! can't be taken out, so they are not sent. Other files are not pictures
//! and pass unchanged.
//!
//! [`has_location`] is the test side: it finds a GPS block or an XMP
//! packet mentioning GPS in any of these formats, for the tests that
//! inspect what was uploaded.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits};

/// What [`remove_location`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleaning {
    /// The metadata blocks were left out; the image data is the original.
    Lossless,
    /// The picture couldn't be walked, so it was decoded and encoded again.
    ReEncoded,
}

/// A picture without its location.
#[derive(Clone, PartialEq, Eq)]
pub struct CleanedImage {
    /// Its bytes.
    pub data: Vec<u8>,
    /// Its MIME type.
    pub mime_type: &'static str,
    /// The file name extensions its format uses, the usual one first.
    pub extensions: &'static [&'static str],
    /// How the metadata went.
    pub cleaning: Cleaning,
    /// Its width and height, when they could be read.
    pub size: Option<(u32, u32)>,
}

impl std::fmt::Debug for CleanedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanedImage")
            .field("bytes", &self.data.len())
            .field("mime_type", &self.mime_type)
            .field("cleaning", &self.cleaning)
            .field("size", &self.size)
            .finish()
    }
}

/// Why a picture's location couldn't be taken out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// A picture format whose metadata this app can't remove (HEIC, AVIF,
    /// TIFF).
    #[error(
        "Family Chat can't take the location out of {0} pictures, so it won't send one. Save it as a JPEG or PNG first."
    )]
    Unsupported(&'static str),
    /// The picture is damaged and couldn't be decoded to clean it.
    #[error("That picture is damaged, so Family Chat can't take its location out.")]
    Damaged,
}

/// The JPEG `APP1` EXIF header.
const EXIF_HEADER: &[u8] = b"Exif\0\0";
/// The EXIF tag of the orientation, kept so photos stay the right way up.
const TAG_ORIENTATION: u16 = 0x0112;
/// The EXIF tag of the GPS block.
const TAG_GPS_IFD: u16 = 0x8825;
/// The most entries an IFD is read for.
const MAX_IFD_ENTRIES: usize = 512;

/// The picture `bytes` without its location and camera metadata, or `None`
/// when it isn't a picture (a PDF, a text file: sent unchanged).
pub fn remove_location(bytes: &[u8]) -> Result<Option<CleanedImage>, MetadataError> {
    if let Some(unsupported) = unsupported_picture(bytes) {
        return Err(MetadataError::Unsupported(unsupported));
    }
    let Ok(format) = image::guess_format(bytes) else {
        return Ok(None);
    };
    let lossless = match format {
        ImageFormat::Jpeg => jpeg::clean(bytes),
        ImageFormat::Png => png::clean(bytes),
        ImageFormat::WebP => webp::clean(bytes),
        ImageFormat::Gif => gif::clean(bytes),
        ImageFormat::Tiff => return Err(MetadataError::Unsupported("TIFF")),
        ImageFormat::Avif => return Err(MetadataError::Unsupported("AVIF")),
        // Formats that carry no location (BMP, ICO and the like) are sent
        // as they are only if they decode; anything else is re-encoded.
        _ => None,
    };
    let (data, cleaning) = match lossless {
        Some(data) => (data, Cleaning::Lossless),
        None => (re_encode(bytes, format)?, Cleaning::ReEncoded),
    };
    let format = image::guess_format(&data).unwrap_or(format);
    let size = dimensions(&data);
    Ok(Some(CleanedImage {
        data,
        mime_type: format.to_mime_type(),
        extensions: format.extensions_str(),
        cleaning,
        size,
    }))
}

/// The MIME type of the picture in `bytes`, when it is one `image` knows.
pub fn picture_mime(bytes: &[u8]) -> Option<&'static str> {
    image::guess_format(bytes)
        .ok()
        .map(|format| format.to_mime_type())
}

/// The width and height of the picture in `bytes`, from its header.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Whether `bytes` carries a location: an EXIF GPS block, or an XMP packet
/// that mentions GPS, in a JPEG, PNG, WebP or GIF.
pub fn has_location(bytes: &[u8]) -> bool {
    let exif_has_gps = |tiff: &[u8]| tiff_has_tag(tiff, TAG_GPS_IFD);
    match image::guess_format(bytes) {
        Ok(ImageFormat::Jpeg) => jpeg::segments(bytes).iter().any(|(marker, data)| {
            *marker == 0xE1
                && (data.strip_prefix(EXIF_HEADER).is_some_and(exif_has_gps)
                    || xmp_mentions_gps(data))
        }),
        Ok(ImageFormat::Png) => png::chunks(bytes).iter().any(|(kind, data)| match kind {
            b"eXIf" => exif_has_gps(data),
            b"iTXt" | b"tEXt" | b"zTXt" => xmp_mentions_gps(data),
            _ => false,
        }),
        Ok(ImageFormat::WebP) => webp::chunks(bytes).iter().any(|(kind, data)| match kind {
            b"EXIF" => exif_has_gps(data.strip_prefix(EXIF_HEADER).unwrap_or(data)),
            b"XMP " => xmp_mentions_gps(data),
            _ => false,
        }),
        Ok(ImageFormat::Gif) => xmp_mentions_gps(bytes),
        _ => false,
    }
}

/// Whether an XMP packet (or any metadata text) names a GPS property.
fn xmp_mentions_gps(data: &[u8]) -> bool {
    data.windows(7).any(|window| window == b"exif:GP")
        || data.windows(13).any(|window| window == b"GPSLongitude")
        || data.windows(12).any(|window| window == b"GPSLatitude")
}

/// The name of a picture format that can't be cleaned, when `bytes` is
/// one: ISO media files (HEIC, AVIF) and TIFF.
fn unsupported_picture(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return Some("TIFF");
    }
    let brand = bytes.get(4..12)?;
    if &brand[..4] != b"ftyp" {
        return None;
    }
    match &brand[4..] {
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => {
            Some("HEIC")
        }
        b"avif" | b"avis" => Some("AVIF"),
        _ => None,
    }
}

/// Decodes the picture within the timeline's limits and encodes it again
/// (PNG for lossless inputs, JPEG for JPEGs), which keeps no metadata.
fn re_encode(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>, MetadataError> {
    let limits = crate::images::TIMELINE_IMAGE_LIMITS;
    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.set_format(format);
    let mut image_limits = Limits::default();
    image_limits.max_image_width = Some(limits.max_side);
    image_limits.max_image_height = Some(limits.max_side);
    image_limits.max_alloc = Some(limits.max_pixels.saturating_mul(4));
    reader.limits(image_limits);
    let image = reader.decode().map_err(|_| MetadataError::Damaged)?;
    let mut out = Cursor::new(Vec::new());
    let target = if format == ImageFormat::Jpeg {
        image::DynamicImage::ImageRgb8(image.to_rgb8()).write_with_encoder(
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92),
        )
    } else {
        image.write_to(&mut out, ImageFormat::Png)
    };
    target.map_err(|_| MetadataError::Damaged)?;
    Ok(out.into_inner())
}

/// Reads a `u16` at `at` in the byte order `big`.
fn read_u16(data: &[u8], at: usize, big: bool) -> Option<u16> {
    let bytes: [u8; 2] = data.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(if big {
        u16::from_be_bytes(bytes)
    } else {
        u16::from_le_bytes(bytes)
    })
}

/// Reads a `u32` at `at` in the byte order `big`.
fn read_u32(data: &[u8], at: usize, big: bool) -> Option<u32> {
    let bytes: [u8; 4] = data.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

/// One IFD entry: its tag, type, count, and where its value (or the
/// value's offset) is.
type IfdEntry = (u16, u16, u32, usize);

/// The byte order (`true`: big-endian) and the entries of a TIFF
/// structure's first IFD.
fn ifd0(tiff: &[u8]) -> Option<(bool, Vec<IfdEntry>)> {
    let big = match tiff.get(..4)? {
        b"MM\0*" => true,
        b"II*\0" => false,
        _ => return None,
    };
    let offset = usize::try_from(read_u32(tiff, 4, big)?).ok()?;
    let count = usize::from(read_u16(tiff, offset, big)?).min(MAX_IFD_ENTRIES);
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let at = offset.checked_add(2)?.checked_add(index.checked_mul(12)?)?;
        entries.push((
            read_u16(tiff, at, big)?,
            read_u16(tiff, at + 2, big)?,
            read_u32(tiff, at + 4, big)?,
            at + 8,
        ));
    }
    Some((big, entries))
}

/// Whether a TIFF structure's first IFD has `tag`.
fn tiff_has_tag(tiff: &[u8], tag: u16) -> bool {
    ifd0(tiff).is_some_and(|(_, entries)| entries.iter().any(|entry| entry.0 == tag))
}

/// The EXIF orientation (1 to 8) in a TIFF structure, when it has one.
fn orientation(tiff: &[u8]) -> Option<u16> {
    let (big, entries) = ifd0(tiff)?;
    let (_, kind, count, at) = entries
        .into_iter()
        .find(|entry| entry.0 == TAG_ORIENTATION)?;
    // SHORT, one value, held in the entry itself.
    if kind != 3 || count != 1 {
        return None;
    }
    read_u16(tiff, at, big).filter(|value| (1..=8).contains(value))
}

/// A TIFF structure with only the orientation in it.
fn orientation_only(orientation: u16) -> Vec<u8> {
    let mut tiff = b"MM\0*\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&TAG_ORIENTATION.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff
}

mod jpeg {
    //! JPEG's segments (ITU T.81, B.1.1).

    use super::{EXIF_HEADER, orientation, orientation_only};

    /// Markers with no length after them.
    fn standalone(marker: u8) -> bool {
        matches!(marker, 0x01 | 0xD0..=0xD7)
    }

    /// Whether a segment is metadata to leave out.
    fn dropped(marker: u8, data: &[u8]) -> bool {
        match marker {
            // XMP, extended XMP and EXIF (rebuilt separately).
            0xE1 => true,
            // The colour profile stays; FlashPix and the multi-picture
            // index (whose extra pictures are cut off at the end) go.
            0xE2 => !data.starts_with(b"ICC_PROFILE\0"),
            // Photoshop resources and IPTC.
            0xED => true,
            // Comments.
            0xFE => true,
            // APP3 to APP15 except Adobe's (APP14, needed to decode CMYK).
            0xE3..=0xEF => marker != 0xEE,
            _ => false,
        }
    }

    /// Every marker segment before the first scan, with its data.
    pub(super) fn segments(bytes: &[u8]) -> Vec<(u8, &[u8])> {
        let mut found = Vec::new();
        let mut at = 2;
        while let (Some(0xFF), Some(&marker)) = (bytes.get(at), bytes.get(at + 1)) {
            if marker == 0xFF {
                at += 1;
                continue;
            }
            if standalone(marker) {
                at += 2;
                continue;
            }
            let Some(length) = bytes
                .get(at + 2..at + 4)
                .map(|length| usize::from(u16::from_be_bytes([length[0], length[1]])))
            else {
                break;
            };
            let Some(data) = bytes.get(at + 4..at + 2 + length) else {
                break;
            };
            found.push((marker, data));
            if marker == 0xDA {
                break;
            }
            at += 2 + length;
        }
        found
    }

    /// The JPEG without its metadata segments, or `None` when it can't be
    /// walked.
    pub(super) fn clean(bytes: &[u8]) -> Option<Vec<u8>> {
        if !bytes.starts_with(&[0xFF, 0xD8]) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len());
        out.extend_from_slice(&[0xFF, 0xD8]);
        let mut orientation_kept = None;
        let mut at = 2;
        loop {
            if *bytes.get(at)? != 0xFF {
                return None;
            }
            let marker = *bytes.get(at + 1)?;
            match marker {
                // Fill bytes before a marker.
                0xFF => {
                    at += 1;
                    continue;
                }
                0xD9 => {
                    out.extend_from_slice(&[0xFF, 0xD9]);
                    // Anything after the end (multi-picture files' extra
                    // images, with their own metadata) is left out.
                    return Some(out);
                }
                0xD8 => return None,
                marker if standalone(marker) => {
                    out.extend_from_slice(&[0xFF, marker]);
                    at += 2;
                    continue;
                }
                _ => {}
            }
            let length = usize::from(u16::from_be_bytes([
                *bytes.get(at + 2)?,
                *bytes.get(at + 3)?,
            ]));
            if length < 2 {
                return None;
            }
            let end = at.checked_add(2)?.checked_add(length)?;
            let segment = bytes.get(at..end)?;
            let data = &segment[4..];
            if marker == 0xE1
                && let Some(tiff) = data.strip_prefix(EXIF_HEADER)
            {
                orientation_kept = orientation_kept.or_else(|| orientation(tiff));
            }
            if !dropped(marker, data) {
                // The rebuilt EXIF goes in just before the first segment
                // that isn't APP0 (JFIF), where readers look for it.
                if marker != 0xE0 {
                    if let Some(value) = orientation_kept.take().filter(|value| *value != 1) {
                        let mut exif = EXIF_HEADER.to_vec();
                        exif.extend_from_slice(&orientation_only(value));
                        let length = u16::try_from(exif.len() + 2).ok()?;
                        out.extend_from_slice(&[0xFF, 0xE1]);
                        out.extend_from_slice(&length.to_be_bytes());
                        out.extend_from_slice(&exif);
                    }
                    orientation_kept = orientation_kept.or(Some(1));
                }
                out.extend_from_slice(segment);
            }
            at = end;
            if marker == 0xDA {
                // The entropy-coded data, up to the next marker that isn't
                // a stuffed byte, a restart or a fill byte.
                let start = at;
                loop {
                    let byte = *bytes.get(at)?;
                    if byte == 0xFF {
                        let next = *bytes.get(at + 1)?;
                        if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                            at += 2;
                            continue;
                        }
                        if next == 0xFF {
                            at += 1;
                            continue;
                        }
                        break;
                    }
                    at += 1;
                }
                out.extend_from_slice(&bytes[start..at]);
            }
        }
    }
}

mod png {
    //! PNG's chunks (ISO/IEC 15948, section 5).

    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// Chunks that are metadata to leave out.
    const DROPPED: &[&[u8; 4]] = &[b"eXIf", b"tEXt", b"zTXt", b"iTXt", b"tIME"];

    /// A chunk's type, data and whole bytes (length, type, data, CRC).
    type Chunk<'a> = (&'a [u8; 4], &'a [u8], &'a [u8]);

    /// Every chunk up to `IEND`; `None` when the structure is broken.
    fn walk(bytes: &[u8]) -> Option<Vec<Chunk<'_>>> {
        let mut found = Vec::new();
        let mut at = SIGNATURE.len();
        loop {
            let length =
                usize::try_from(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
                    .ok()?;
            let kind: &[u8; 4] = bytes.get(at + 4..at + 8)?.try_into().ok()?;
            let end = at.checked_add(12)?.checked_add(length)?;
            let whole = bytes.get(at..end)?;
            found.push((kind, &whole[8..8 + length], whole));
            at = end;
            if kind == b"IEND" {
                return Some(found);
            }
        }
    }

    /// Every chunk's type and data (empty when the file is broken).
    pub(super) fn chunks(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
        walk(bytes)
            .unwrap_or_default()
            .into_iter()
            .map(|(kind, data, _)| (*kind, data))
            .collect()
    }

    pub(super) fn clean(bytes: &[u8]) -> Option<Vec<u8>> {
        if !bytes.starts_with(SIGNATURE) {
            return None;
        }
        let mut out = SIGNATURE.to_vec();
        for (kind, _, whole) in walk(bytes)? {
            if !DROPPED.contains(&kind) {
                out.extend_from_slice(whole);
            }
        }
        Some(out)
    }
}

mod webp {
    //! WebP's RIFF chunks (RFC 9649, section 2).

    /// The `VP8X` flags of the EXIF and XMP chunks.
    const EXIF_FLAG: u8 = 0x08;
    const XMP_FLAG: u8 = 0x04;

    fn walk(bytes: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
        if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
            return None;
        }
        let riff = usize::try_from(u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?)).ok()?;
        let end = riff.checked_add(8)?.min(bytes.len());
        let mut found = Vec::new();
        let mut at = 12;
        while at < end {
            let kind: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
            let size = usize::try_from(u32::from_le_bytes(
                bytes.get(at + 4..at + 8)?.try_into().ok()?,
            ))
            .ok()?;
            let data = bytes.get(at + 8..at.checked_add(8)?.checked_add(size)?)?;
            found.push((kind, data));
            at = at + 8 + size + (size & 1);
        }
        Some(found)
    }

    pub(super) fn chunks(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
        walk(bytes).unwrap_or_default()
    }

    pub(super) fn clean(bytes: &[u8]) -> Option<Vec<u8>> {
        let mut body = Vec::with_capacity(bytes.len());
        for (kind, data) in walk(bytes)? {
            if &kind == b"EXIF" || &kind == b"XMP " {
                continue;
            }
            body.extend_from_slice(&kind);
            body.extend_from_slice(&u32::try_from(data.len()).ok()?.to_le_bytes());
            if &kind == b"VP8X" {
                let (flags, rest) = data.split_first()?;
                body.push(flags & !(EXIF_FLAG | XMP_FLAG));
                body.extend_from_slice(rest);
            } else {
                body.extend_from_slice(data);
            }
            if data.len() & 1 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&u32::try_from(body.len() + 4).ok()?.to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&body);
        Some(out)
    }
}

mod gif {
    //! GIF's blocks (GIF89a, sections 17 to 27).

    /// The size of a colour table from a packed field, when it has one.
    fn table(packed: u8) -> usize {
        if packed & 0x80 == 0 {
            0
        } else {
            3 << ((packed & 0x07) + 1)
        }
    }

    /// The end of the data sub-blocks starting at `at` (after their
    /// terminator).
    fn sub_blocks(bytes: &[u8], mut at: usize) -> Option<usize> {
        loop {
            let size = usize::from(*bytes.get(at)?);
            at += 1;
            if size == 0 {
                return Some(at);
            }
            at = at.checked_add(size)?;
            bytes.get(..at)?;
        }
    }

    pub(super) fn clean(bytes: &[u8]) -> Option<Vec<u8>> {
        if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
            return None;
        }
        let header_end = 13 + table(*bytes.get(10)?);
        let mut out = bytes.get(..header_end)?.to_vec();
        let mut at = header_end;
        loop {
            match *bytes.get(at)? {
                0x3B => {
                    out.push(0x3B);
                    return Some(out);
                }
                0x21 => {
                    let label = *bytes.get(at + 1)?;
                    let end = sub_blocks(bytes, at + 2)?;
                    let first = bytes.get(at + 2..).unwrap_or_default();
                    let xmp = label == 0xFF && first.get(1..12) == Some(b"XMP DataXMP".as_slice());
                    if !(xmp || label == 0xFE) {
                        out.extend_from_slice(bytes.get(at..end)?);
                    }
                    at = end;
                }
                0x2C => {
                    let packed = *bytes.get(at + 9)?;
                    let data = at + 10 + table(packed);
                    // The LZW minimum code size, then the image data.
                    let end = sub_blocks(bytes, data + 1)?;
                    out.extend_from_slice(bytes.get(at..end)?);
                    at = end;
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TIFF structure with an orientation and a GPS block holding
    /// Dublin's latitude.
    fn exif_with_gps(orientation: u16) -> Vec<u8> {
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&2u16.to_le_bytes());
        // Orientation.
        tiff.extend_from_slice(&TAG_ORIENTATION.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        // The GPS IFD pointer, to the IFD after this one.
        tiff.extend_from_slice(&TAG_GPS_IFD.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&38u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        // The GPS IFD: GPSLatitudeRef "N".
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&2u32.to_le_bytes());
        tiff.extend_from_slice(b"N\0\0\0");
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff
    }

    const XMP: &[u8] = b"<x:xmpmeta><rdf:Description exif:GPSLatitude=\"53,20.5N\" exif:GPSLongitude=\"6,15.6W\"/></x:xmpmeta>";

    fn segment(marker: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend_from_slice(&u16::try_from(data.len() + 2).expect("short").to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    /// A real 16x8 JPEG, with GPS in EXIF and XMP, a comment, and a second
    /// "picture" after the end.
    fn jpeg_with_location(orientation: u16) -> Vec<u8> {
        let mut plain = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(16, 8)
            .write_to(&mut plain, ImageFormat::Jpeg)
            .expect("encodes");
        let plain = plain.into_inner();
        let mut exif = EXIF_HEADER.to_vec();
        exif.extend_from_slice(&exif_with_gps(orientation));
        let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        xmp.extend_from_slice(XMP);
        let mut out = plain[..2].to_vec();
        out.extend(segment(0xE1, &exif));
        out.extend(segment(0xE1, &xmp));
        out.extend(segment(0xFE, b"taken at 12 Main Street"));
        out.extend_from_slice(&plain[2..]);
        out.extend_from_slice(&plain);
        out
    }

    #[test]
    fn a_jpeg_loses_its_location_and_keeps_its_pixels_and_orientation() {
        let original = jpeg_with_location(6);
        assert!(has_location(&original));
        let cleaned = remove_location(&original)
            .expect("cleans")
            .expect("a picture");
        assert_eq!(cleaned.cleaning, Cleaning::Lossless);
        assert_eq!(cleaned.mime_type, "image/jpeg");
        assert_eq!(cleaned.size, Some((16, 8)));
        assert!(!has_location(&cleaned.data));
        let text = String::from_utf8_lossy(&cleaned.data);
        assert!(!text.contains("Main Street"));
        assert!(!text.contains("GPS"));
        // The orientation survives, alone.
        let exif: Vec<_> = jpeg::segments(&cleaned.data)
            .into_iter()
            .filter(|(marker, _)| *marker == 0xE1)
            .collect();
        assert_eq!(exif.len(), 1);
        let tiff = exif[0].1.strip_prefix(EXIF_HEADER).expect("EXIF");
        assert_eq!(orientation(tiff), Some(6));
        assert!(!tiff_has_tag(tiff, TAG_GPS_IFD));
        // The same pixels: the scan data is byte for byte the original's.
        let decoded = image::load_from_memory(&cleaned.data).expect("decodes");
        let before = image::load_from_memory(&original).expect("decodes");
        assert_eq!(decoded.to_rgb8(), before.to_rgb8());
        // The second picture after the end is gone.
        assert!(cleaned.data.len() < original.len() / 2 + 64);
    }

    #[test]
    fn an_upright_jpeg_gets_no_exif_at_all() {
        let cleaned = remove_location(&jpeg_with_location(1))
            .expect("cleans")
            .expect("a picture");
        assert!(
            jpeg::segments(&cleaned.data)
                .iter()
                .all(|(marker, _)| *marker != 0xE1)
        );
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
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
        let mut out = u32::try_from(data.len())
            .expect("short")
            .to_be_bytes()
            .to_vec();
        let mut typed = kind.to_vec();
        typed.extend_from_slice(data);
        out.extend_from_slice(&typed);
        out.extend_from_slice(&crc32(&typed).to_be_bytes());
        out
    }

    #[test]
    fn a_png_loses_its_exif_and_xmp_chunks() {
        let mut plain = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4, 4)
            .write_to(&mut plain, ImageFormat::Png)
            .expect("encodes");
        let plain = plain.into_inner();
        // Metadata goes after IHDR (8 + 25 bytes).
        let mut original = plain[..33].to_vec();
        original.extend(png_chunk(b"eXIf", &exif_with_gps(1)));
        let mut itxt = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
        itxt.extend_from_slice(XMP);
        original.extend(png_chunk(b"iTXt", &itxt));
        original.extend_from_slice(&plain[33..]);
        assert!(has_location(&original));
        let cleaned = remove_location(&original)
            .expect("cleans")
            .expect("a picture");
        assert_eq!(cleaned.cleaning, Cleaning::Lossless);
        assert_eq!(cleaned.data, plain);
        assert!(!has_location(&cleaned.data));
    }

    #[test]
    fn a_webp_loses_its_exif_and_xmp_chunks_and_flags() {
        let mut vp8l = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4, 4)
            .write_to(&mut vp8l, ImageFormat::WebP)
            .expect("encodes");
        let simple = vp8l.into_inner();
        let image_chunk = &simple[12..];
        let mut body = b"VP8X".to_vec();
        body.extend_from_slice(&10u32.to_le_bytes());
        body.extend_from_slice(&[0x08 | 0x04 | 0x10, 0, 0, 0, 3, 0, 0, 3, 0, 0]);
        body.extend_from_slice(image_chunk);
        let exif = exif_with_gps(1);
        body.extend_from_slice(b"EXIF");
        body.extend_from_slice(&u32::try_from(exif.len()).expect("short").to_le_bytes());
        body.extend_from_slice(&exif);
        body.extend_from_slice(b"XMP ");
        body.extend_from_slice(&u32::try_from(XMP.len()).expect("short").to_le_bytes());
        body.extend_from_slice(XMP);
        if XMP.len() & 1 == 1 {
            body.push(0);
        }
        let mut original = b"RIFF".to_vec();
        original.extend_from_slice(&u32::try_from(body.len() + 4).expect("short").to_le_bytes());
        original.extend_from_slice(b"WEBP");
        original.extend_from_slice(&body);
        assert!(has_location(&original));
        let cleaned = remove_location(&original)
            .expect("cleans")
            .expect("a picture");
        assert_eq!(cleaned.cleaning, Cleaning::Lossless);
        assert!(!has_location(&cleaned.data));
        let flags = webp::chunks(&cleaned.data)
            .into_iter()
            .find(|(kind, _)| kind == b"VP8X")
            .map(|(_, data)| data[0])
            .expect("VP8X");
        assert_eq!(flags, 0x10);
        image::load_from_memory(&cleaned.data).expect("still decodes");
    }

    #[test]
    fn a_gif_loses_its_xmp_and_comments() {
        let mut plain = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut plain, ImageFormat::Gif)
            .expect("encodes");
        let plain = plain.into_inner();
        let header_end = 13 + gif_table(plain[10]);
        let mut original = plain[..header_end].to_vec();
        original.extend_from_slice(&[0x21, 0xFF, 11]);
        original.extend_from_slice(b"XMP DataXMP");
        for chunk in XMP.chunks(255) {
            original.push(u8::try_from(chunk.len()).expect("short"));
            original.extend_from_slice(chunk);
        }
        original.push(0);
        original.extend_from_slice(&[0x21, 0xFE, 5]);
        original.extend_from_slice(b"hello");
        original.push(0);
        original.extend_from_slice(&plain[header_end..]);
        assert!(has_location(&original));
        let cleaned = remove_location(&original)
            .expect("cleans")
            .expect("a picture");
        assert_eq!(cleaned.cleaning, Cleaning::Lossless);
        assert_eq!(cleaned.data, plain);
    }

    fn gif_table(packed: u8) -> usize {
        if packed & 0x80 == 0 {
            0
        } else {
            3 << ((packed & 0x07) + 1)
        }
    }

    #[test]
    fn a_damaged_jpeg_is_re_encoded_without_metadata() {
        let mut original = jpeg_with_location(1);
        // A segment whose length runs past the end makes the walk give up
        // after the image's end marker is gone; decoding still works.
        let end = original.len() / 2;
        original.truncate(end);
        let mut broken = original[..2].to_vec();
        broken.extend_from_slice(&[0xFF, 0xE1, 0xFF, 0xFF]);
        broken.extend_from_slice(&original[2..]);
        match remove_location(&broken) {
            Ok(Some(cleaned)) => {
                assert_eq!(cleaned.cleaning, Cleaning::ReEncoded);
                assert!(!has_location(&cleaned.data));
            }
            Err(MetadataError::Damaged) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pictures_that_cannot_be_cleaned_are_refused_and_other_files_pass() {
        let mut heic = vec![0, 0, 0, 24];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        assert_eq!(
            remove_location(&heic),
            Err(MetadataError::Unsupported("HEIC"))
        );
        assert_eq!(
            remove_location(b"II*\0\x08\0\0\0"),
            Err(MetadataError::Unsupported("TIFF"))
        );
        assert_eq!(remove_location(b"%PDF-1.7\n"), Ok(None));
        assert_eq!(remove_location(b"just some notes"), Ok(None));
        let message = MetadataError::Unsupported("HEIC").to_string();
        assert!(message.contains("HEIC"), "{message}");
    }
}
