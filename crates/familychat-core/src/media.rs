//! Media in messages (PLAN.md, section 5.8; ADR 0007): what a frontend
//! knows of an image or a file, the decoded pixels it draws, and why a
//! piece of media can't be shown.
//!
//! A frontend never fetches or decodes anything itself. It hands a
//! [`MediaInfo`] back to the session (`SessionApi::image`,
//! `SessionApi::save_file`), which fetches it from the account's homeserver
//! through authenticated media, decrypts it, and decodes images off the UI
//! thread with `familychat-security`'s limits. What comes back is plain
//! RGBA pixels ([`DecodedImage`]): there is no format left for a frontend
//! to parse, and nothing it could hand to an SVG renderer.

use std::fmt;

/// Where a piece of media lives, as only the session reads it: the
/// homeserver's `mxc://` address and, in an encrypted room, the file's key.
/// Opaque to frontends, and `Debug` prints nothing of it (the key decrypts
/// the file).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MediaHandle(String);

impl MediaHandle {
    /// A handle over the session's own encoding of the source.
    pub fn new(encoded: impl Into<String>) -> Self {
        Self(encoded.into())
    }

    /// The session's encoding. Never log it.
    pub fn encoded(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MediaHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaHandle([redacted])")
    }
}

/// An image, video, audio clip or file attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaInfo {
    /// The file.
    pub source: MediaHandle,
    /// A smaller copy the sender attached (encrypted rooms send one, since
    /// the server can't make one).
    pub thumbnail: Option<MediaHandle>,
    /// The file name the sender gave (or the message's body), already made
    /// safe by `familychat-security`'s file-name rules: one name, no path,
    /// no control or bidirectional characters. A frontend suggests it when
    /// the person saves the file.
    pub filename: String,
    /// The type the sender declared (`image/png`), which the session does
    /// not trust: it sniffs the bytes.
    pub mimetype: Option<String>,
    /// The size the sender declared, in bytes.
    pub size: Option<u64>,
    /// The width the sender declared, in pixels.
    pub width: Option<u32>,
    /// The height the sender declared, in pixels.
    pub height: Option<u32>,
}

impl MediaInfo {
    /// The size the sender declared, for people: "2.4 MB".
    pub fn size_label(&self) -> Option<String> {
        self.size.map(size_label)
    }

    /// Whether the sender says it is an SVG image, which is never drawn.
    pub fn is_svg(&self) -> bool {
        self.mimetype
            .as_deref()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("image/svg+xml"))
            || self.filename.to_ascii_lowercase().ends_with(".svg")
    }

    /// The width and height to reserve before the image arrives, when the
    /// sender said both.
    pub fn declared_size(&self) -> Option<(u32, u32)> {
        match (self.width, self.height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => Some((width, height)),
            _ => None,
        }
    }
}

/// A size in bytes, for people: "512 bytes", "12 KB", "2.4 MB".
pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} bytes")
    } else if value < MB {
        format!("{:.0} KB", value / KB)
    } else if value < GB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{:.1} GB", value / GB)
    }
}

/// One frame of a decoded image.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageFrame {
    /// Pixels, four bytes each (red, green, blue, alpha), row by row.
    pub rgba: Vec<u8>,
    /// How long it shows before the next frame (0 for a still image).
    pub delay_ms: u32,
}

impl fmt::Debug for ImageFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ImageFrame({} bytes, {} ms)",
            self.rgba.len(),
            self.delay_ms
        )
    }
}

/// A decoded image: every frame the same size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The frames, at least one; more for an animation.
    pub frames: Vec<ImageFrame>,
}

impl DecodedImage {
    /// Whether it animates.
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
}

/// Why a piece of media can't be shown or saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    /// The file is larger than the app reads.
    TooLarge {
        /// The limit, in bytes.
        limit: u64,
    },
    /// The image is wider, taller or has more pixels than the app decodes.
    TooManyPixels,
    /// The animation has more frames than the app decodes.
    TooManyFrames,
    /// An SVG image: never drawn.
    Svg,
    /// Not an image the app decodes (PNG, JPEG, GIF and WebP), or a broken
    /// one.
    NotAnImage,
    /// The homeserver doesn't offer authenticated media (Matrix 1.11), the
    /// only way the app fetches media.
    NoAuthenticatedMedia,
    /// Fetching it failed; asking again may work.
    Unavailable,
    /// Writing the file failed.
    NotSaved(String),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(
                f,
                "This file is too large to show here (over {}).",
                size_label(*limit)
            ),
            Self::TooManyPixels => f.write_str("This image is too large to show here."),
            Self::TooManyFrames => f.write_str("This animation is too long to show here."),
            Self::Svg => f.write_str("SVG images aren't shown, for safety. Save it to open it."),
            Self::NotAnImage => f.write_str("This image can't be shown."),
            Self::NoAuthenticatedMedia => f.write_str(
                "Your server doesn't support authenticated media, so Family Chat doesn't \
                 download it.",
            ),
            Self::Unavailable => f.write_str("This couldn't be downloaded. Try again later."),
            Self::NotSaved(reason) => write!(f, "The file couldn't be saved: {reason}"),
        }
    }
}

impl std::error::Error for MediaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_and_frames_never_print_their_contents() {
        let handle = MediaHandle::new(r#"{"key":"secret-key"}"#);
        assert!(!format!("{handle:?}").contains("secret"));
        let frame = ImageFrame {
            rgba: vec![1, 2, 3, 4],
            delay_ms: 0,
        };
        assert_eq!(format!("{frame:?}"), "ImageFrame(4 bytes, 0 ms)");
    }

    #[test]
    fn sizes_read_like_people_say_them() {
        assert_eq!(size_label(512), "512 bytes");
        assert_eq!(size_label(12_400), "12 KB");
        assert_eq!(size_label(2_400_000), "2.4 MB");
        assert_eq!(size_label(3_100_000_000), "3.1 GB");
    }

    #[test]
    fn svg_is_known_by_type_or_name() {
        let info = |mimetype: Option<&str>, filename: &str| MediaInfo {
            source: MediaHandle::new("x"),
            thumbnail: None,
            filename: filename.to_owned(),
            mimetype: mimetype.map(str::to_owned),
            size: None,
            width: None,
            height: None,
        };
        assert!(info(Some("image/svg+xml"), "a.png").is_svg());
        assert!(info(None, "drawing.SVG").is_svg());
        assert!(!info(Some("image/png"), "a.png").is_svg());
        assert_eq!(info(None, "a").declared_size(), None);
    }
}
