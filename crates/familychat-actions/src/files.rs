//! Files on their way to a room (PLAN.md, sections 5.8 and 13.7; ADR
//! 0012): checked against the server's upload limit before anything is
//! read or uploaded, and pictures without their location.

use std::{fmt, path::PathBuf, sync::Arc};

use familychat_core::messaging::{MediaLimits, OutgoingFile};
use familychat_security::{
    file_names::sanitize_filename,
    image_metadata::{self, MetadataError},
};

use crate::Refusal;

/// Where a file comes from.
#[derive(Clone, PartialEq, Eq)]
pub enum FileSource {
    /// A file the person picked or dropped.
    Path(PathBuf),
    /// Bytes from the clipboard (a pasted picture), with the name to send
    /// them under.
    Bytes {
        /// The name.
        name: String,
        /// The bytes.
        data: Arc<[u8]>,
    },
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(_) => f.write_str("FileSource::Path(..)"),
            Self::Bytes { data, .. } => write!(f, "FileSource::Bytes({} bytes)", data.len()),
        }
    }
}

impl FileSource {
    /// The name the file is sent under.
    pub fn name(&self) -> String {
        let name = match self {
            Self::Path(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Self::Bytes { name, .. } => name.clone(),
        };
        let name = sanitize_filename(&name);
        if name.is_empty() {
            "file".to_owned()
        } else {
            name
        }
    }

    /// Its size in bytes, without reading it.
    pub fn size(&self) -> std::io::Result<u64> {
        match self {
            Self::Path(path) => std::fs::metadata(path).map(|metadata| metadata.len()),
            Self::Bytes { data, .. } => Ok(u64::try_from(data.len()).unwrap_or(u64::MAX)),
        }
    }

    fn read(&self) -> std::io::Result<Arc<[u8]>> {
        match self {
            Self::Path(path) => std::fs::read(path).map(Arc::from),
            Self::Bytes { data, .. } => Ok(data.clone()),
        }
    }
}

/// How files are prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOptions {
    /// Take the location out of pictures (on by default; a setting to keep
    /// it is Phase 13's).
    pub remove_location: bool,
}

impl Default for FileOptions {
    fn default() -> Self {
        Self {
            remove_location: true,
        }
    }
}

/// `bytes` shown the way the refusals say sizes: "800 KB", "11 MB",
/// "1.5 GB" (powers of 1,024, as the server's limit is).
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else if value < 10.0 && value.fract() >= 0.05 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{} {}", value.round(), UNITS[unit])
    }
}

/// Refuses a file of `size` bytes over the server's limit.
pub fn check_size(size: u64, limits: &MediaLimits) -> Result<(), Refusal> {
    match limits.max_upload_bytes {
        Some(limit) if size > limit => Err(Refusal::TooLarge { size, limit }),
        _ => Ok(()),
    }
}

/// The file ready to send: its size checked against `limits` before it is
/// read, a picture's location taken out (`options`), and checked again
/// after (a re-encoded picture can grow).
pub fn prepare(
    source: &FileSource,
    limits: &MediaLimits,
    options: FileOptions,
) -> Result<OutgoingFile, Refusal> {
    let size = source.size().map_err(|_| Refusal::Unreadable)?;
    check_size(size, limits)?;
    let data = source.read().map_err(|_| Refusal::Unreadable)?;
    let mut name = source.name();
    let (data, mime_type, image_size) = if options.remove_location {
        match image_metadata::remove_location(&data) {
            Ok(Some(cleaned)) => {
                // A picture encoded again may have a new format.
                let lower = name.to_ascii_lowercase();
                if let Some(extension) = cleaned.extensions.first()
                    && !cleaned
                        .extensions
                        .iter()
                        .any(|known| lower.ends_with(&format!(".{known}")))
                {
                    let stem = name
                        .rsplit_once('.')
                        .map_or(name.as_str(), |(stem, _)| stem);
                    name = format!("{stem}.{extension}");
                }
                (
                    Arc::from(cleaned.data),
                    cleaned.mime_type.to_owned(),
                    cleaned.size,
                )
            }
            Ok(None) => (data, mime_for(&name).to_owned(), None),
            Err(error) => return Err(Refusal::Location(error)),
        }
    } else {
        let size = image_metadata::dimensions(&data);
        let mime = image_metadata::picture_mime(&data)
            .unwrap_or_else(|| mime_for(&name))
            .to_owned();
        (data, mime, size)
    };
    check_size(u64::try_from(data.len()).unwrap_or(u64::MAX), limits)?;
    Ok(OutgoingFile {
        file_name: name,
        mime_type,
        data,
        image_size,
    })
}

/// The MIME type a file that isn't a picture is sent with, from its name.
fn mime_for(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// The refusal for a picture whose location can't be taken out.
impl From<MetadataError> for Refusal {
    fn from(error: MetadataError) -> Self {
        Self::Location(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_person_would_say_them() {
        assert_eq!(size_label(800), "800 bytes");
        assert_eq!(size_label(11 * 1024 * 1024), "11 MB");
        assert_eq!(size_label(1536 * 1024), "1.5 MB");
        assert_eq!(size_label(14_900_000), "14 MB");
    }

    #[test]
    fn a_file_over_the_limit_is_refused_before_it_is_read() {
        // A path that doesn't exist can't be read, but its size is asked
        // first: bytes from the clipboard show the order.
        let source = FileSource::Bytes {
            name: "big.bin".to_owned(),
            data: Arc::from(vec![0u8; 2048]),
        };
        let limits = MediaLimits {
            max_upload_bytes: Some(1024),
        };
        assert_eq!(
            prepare(&source, &limits, FileOptions::default()),
            Err(Refusal::TooLarge {
                size: 2048,
                limit: 1024
            })
        );
        let unlimited = MediaLimits::default();
        let file = prepare(&source, &unlimited, FileOptions::default()).expect("prepared");
        assert_eq!(file.mime_type, "application/octet-stream");
        assert_eq!(file.file_name, "big.bin");
    }

    #[test]
    fn a_missing_file_is_unreadable() {
        let source = FileSource::Path(PathBuf::from("/nonexistent/familychat/test.png"));
        assert_eq!(
            prepare(&source, &MediaLimits::default(), FileOptions::default()),
            Err(Refusal::Unreadable)
        );
    }

    #[test]
    fn names_are_made_safe() {
        let source = FileSource::Bytes {
            name: "../../etc/passwd".to_owned(),
            data: Arc::from(b"x".as_slice()),
        };
        assert!(!source.name().contains('/'));
        assert_eq!(mime_for("Report.PDF"), "application/pdf");
    }
}
