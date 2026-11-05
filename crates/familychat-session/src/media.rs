//! Media, fetched safely (PLAN.md, section 5.8; ADR 0007).
//!
//! Every account's client fetches media through [`AuthenticatedMediaFetcher`]
//! instead of the SDK's default fetcher:
//!
//! - only the authenticated media endpoints (Matrix 1.11, MSC3916:
//!   `/_matrix/client/v1/media/download` and `/thumbnail`), with the
//!   account's access token; a server without them gets no media request
//!   at all ([`MediaError::NoAuthenticatedMedia`]), so media never leaves
//!   the homeserver unauthenticated and never goes to anyone else;
//! - through the account's own HTTP client (`SessionConfig`), which
//!   follows no redirects;
//! - with a timeout ([`DOWNLOAD_TIMEOUT`]) and a byte cap enforced while
//!   the body streams in: [`MAX_DOWNLOAD_BYTES`], or less for images (the
//!   decoder's own limit), so a server can't make the app hold more than
//!   that;
//! - encrypted files decrypted with the SDK's `AttachmentDecryptor`.
//!
//! The SDK still caches what the fetcher returns in the account's encrypted
//! media store (`Media::get_media_content`). Images are decoded with
//! `familychat-security`'s limits on Tokio's blocking pool, never on a UI
//! thread, and SVG is refused before anything is fetched. Nothing here logs
//! an address, a key or a byte of content.

use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use familychat_core::media::{DecodedImage, MediaError, MediaInfo};
use familychat_security::images::{TIMELINE_IMAGE_LIMITS, decode_image};
use matrix_sdk::{
    BoxFuture, Client,
    media::{MediaFetcher, MediaFormat, MediaRequestParameters, MediaThumbnailSettings},
    ruma::{
        MxcUri, UInt,
        api::{Metadata as _, client::authenticated_media},
        events::room::MediaSource,
    },
};
use reqwest::{StatusCode, Url};
use tracing::info;

use crate::mapping::source;

/// The most bytes any download may have.
pub const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// How long one download may take, from connecting to the last byte.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// The thumbnail size asked of the server for a timeline image.
const THUMBNAIL_SIZE: (u32, u32) = (800, 600);

tokio::task_local! {
    /// The byte cap of the downloads made in this task (images ask for
    /// less than [`MAX_DOWNLOAD_BYTES`]).
    static DOWNLOAD_CAP: u64;
}

/// Why the fetcher refused or failed, carried through the SDK's error as
/// `matrix_sdk::Error::UnknownError` and read back by [`media_error`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum FetchError {
    /// The server has no authenticated media endpoints.
    #[error("the homeserver does not offer authenticated media")]
    NoAuthenticatedMedia,
    /// The body is larger than the cap.
    #[error("the media is larger than {limit} bytes")]
    TooLarge {
        /// The cap.
        limit: u64,
    },
    /// The server answered with an error status.
    #[error("the homeserver answered {0}")]
    Status(u16),
    /// The request failed (network, timeout).
    #[error("the request failed")]
    Request,
    /// The address or the file could not be used.
    #[error("the media source is not usable")]
    BadSource,
}

impl FetchError {
    fn into_sdk(self) -> matrix_sdk::Error {
        matrix_sdk::Error::UnknownError(Box::new(self))
    }
}

/// The [`MediaFetcher`] every account's client is built with; see the
/// module documentation.
#[derive(Debug, Clone)]
pub struct AuthenticatedMediaFetcher {
    http: reqwest::Client,
}

impl AuthenticatedMediaFetcher {
    /// A fetcher over the account's HTTP client.
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    async fn fetch(
        &self,
        client: &Client,
        request: &MediaRequestParameters,
    ) -> Result<Vec<u8>, FetchError> {
        let versions = client
            .supported_versions()
            .await
            .map_err(|_| FetchError::Request)?;
        if !authenticated_media::get_content::v1::Request::PATH_BUILDER.is_supported(&versions) {
            return Err(FetchError::NoAuthenticatedMedia);
        }
        let cap = DOWNLOAD_CAP
            .try_with(|cap| *cap)
            .unwrap_or(MAX_DOWNLOAD_BYTES)
            .min(MAX_DOWNLOAD_BYTES);
        match &request.source {
            MediaSource::Plain(uri) => {
                let url = media_url(&client.homeserver(), uri, &request.format)?;
                self.download(client, url, cap).await
            }
            MediaSource::Encrypted(file) => {
                // An encrypted file is always the file: the server can't
                // make a thumbnail of what it can't read.
                let url = media_url(&client.homeserver(), &file.url, &MediaFormat::File)?;
                let encrypted = self.download(client, url, cap).await?;
                let mut cursor = std::io::Cursor::new(encrypted);
                let mut decryptor = matrix_sdk_base::crypto::AttachmentDecryptor::new(
                    &mut cursor,
                    file.as_ref().clone().into(),
                )
                .map_err(|_| FetchError::BadSource)?;
                let mut plain = Vec::new();
                decryptor
                    .read_to_end(&mut plain)
                    .map_err(|_| FetchError::BadSource)?;
                Ok(plain)
            }
        }
    }

    /// Downloads `url` with the account's token, at most `cap` bytes; on a
    /// token the server no longer accepts, refreshes it once and asks
    /// again.
    async fn download(&self, client: &Client, url: Url, cap: u64) -> Result<Vec<u8>, FetchError> {
        let mut refreshed = false;
        loop {
            let token = client.access_token().ok_or(FetchError::Request)?;
            let response = self
                .http
                .get(url.clone())
                .bearer_auth(token)
                .timeout(DOWNLOAD_TIMEOUT)
                .send()
                .await
                .map_err(|_| FetchError::Request)?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                if client.refresh_access_token().await.is_ok() {
                    continue;
                }
                return Err(FetchError::Status(status.as_u16()));
            }
            if !status.is_success() {
                return Err(FetchError::Status(status.as_u16()));
            }
            return read_capped(response, cap).await;
        }
    }
}

impl MediaFetcher for AuthenticatedMediaFetcher {
    fn fetch_media_content<'a>(
        &'a self,
        client: &'a Client,
        request: &'a MediaRequestParameters,
    ) -> BoxFuture<'a, Result<Vec<u8>, matrix_sdk::Error>> {
        Box::pin(async move {
            self.fetch(client, request)
                .await
                .map_err(FetchError::into_sdk)
        })
    }
}

/// Reads a response body, refusing it as soon as it passes `cap` bytes
/// (or says it will).
async fn read_capped(mut response: reqwest::Response, cap: u64) -> Result<Vec<u8>, FetchError> {
    if response.content_length().is_some_and(|length| length > cap) {
        return Err(FetchError::TooLarge { limit: cap });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| FetchError::Request)? {
        if body.len() as u64 + chunk.len() as u64 > cap {
            return Err(FetchError::TooLarge { limit: cap });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The authenticated media address of `uri` on `homeserver`.
fn media_url(homeserver: &Url, uri: &MxcUri, format: &MediaFormat) -> Result<Url, FetchError> {
    let (server, media) = uri.parts().map_err(|_| FetchError::BadSource)?;
    let mut url = homeserver.clone();
    let endpoint = match format {
        MediaFormat::File => "download",
        MediaFormat::Thumbnail(_) => "thumbnail",
    };
    url.path_segments_mut()
        .map_err(|()| FetchError::BadSource)?
        .pop_if_empty()
        .extend([
            "_matrix",
            "client",
            "v1",
            "media",
            endpoint,
            server.as_str(),
            media,
        ]);
    if let MediaFormat::Thumbnail(settings) = format {
        url.query_pairs_mut()
            .append_pair("width", &settings.width.to_string())
            .append_pair("height", &settings.height.to_string())
            .append_pair("method", settings.method.as_str())
            .append_pair("animated", if settings.animated { "true" } else { "false" });
    }
    Ok(url)
}

/// A media failure as the frontends see it.
pub(crate) fn media_error(error: &matrix_sdk::Error) -> MediaError {
    if let matrix_sdk::Error::UnknownError(inner) = error
        && let Some(fetch) = inner.downcast_ref::<FetchError>()
    {
        return match fetch {
            FetchError::NoAuthenticatedMedia => MediaError::NoAuthenticatedMedia,
            FetchError::TooLarge { limit } => MediaError::TooLarge { limit: *limit },
            FetchError::Status(_) | FetchError::Request | FetchError::BadSource => {
                MediaError::Unavailable
            }
        };
    }
    MediaError::Unavailable
}

/// Fetches `request` within `cap` bytes, through the client's media cache.
async fn fetch(
    client: &Client,
    request: MediaRequestParameters,
    cap: u64,
) -> Result<Vec<u8>, MediaError> {
    DOWNLOAD_CAP
        .scope(cap, async {
            client
                .media()
                .get_media_content(&request, true)
                .await
                .map_err(|error| media_error(&error))
        })
        .await
}

/// The image to draw for `media`: a server thumbnail of a plain image
/// (falling back to the file), the sender's thumbnail or the file of an
/// encrypted one; decoded within the timeline's limits on the blocking
/// pool. SVG is refused before anything is fetched.
pub(crate) async fn image(
    client: &Client,
    media: &MediaInfo,
) -> Result<Arc<DecodedImage>, MediaError> {
    if media.is_svg() {
        return Err(MediaError::Svg);
    }
    let file = source(&media.source).ok_or(MediaError::Unavailable)?;
    let cap = TIMELINE_IMAGE_LIMITS.max_bytes;
    let bytes = match &file {
        MediaSource::Plain(_) => {
            let thumbnail = MediaRequestParameters {
                source: file.clone(),
                format: MediaFormat::Thumbnail(MediaThumbnailSettings::new(
                    UInt::from(THUMBNAIL_SIZE.0),
                    UInt::from(THUMBNAIL_SIZE.1),
                )),
            };
            match fetch(client, thumbnail, cap).await {
                Ok(bytes) => bytes,
                Err(MediaError::NoAuthenticatedMedia) => {
                    return Err(MediaError::NoAuthenticatedMedia);
                }
                // No thumbnail (or not one this server makes): the file.
                Err(_) => fetch(client, file_request(file), cap).await?,
            }
        }
        MediaSource::Encrypted(_) => {
            let preferred = media.thumbnail.as_ref().and_then(source).unwrap_or(file);
            fetch(client, file_request(preferred), cap).await?
        }
    };
    let decoded = tokio::task::spawn_blocking(move || decode_image(&bytes, &TIMELINE_IMAGE_LIMITS))
        .await
        .map_err(|_| MediaError::NotAnImage)??;
    Ok(Arc::new(decoded))
}

fn file_request(source: MediaSource) -> MediaRequestParameters {
    MediaRequestParameters {
        source,
        format: MediaFormat::File,
    }
}

/// Downloads `media`'s file and writes it to `path`: into a temporary file
/// beside it, then renamed, so a failure never leaves half a file under the
/// chosen name. Nothing is opened.
pub(crate) async fn save_file(
    client: &Client,
    media: &MediaInfo,
    path: PathBuf,
) -> Result<(), MediaError> {
    let file = source(&media.source).ok_or(MediaError::Unavailable)?;
    let bytes = fetch(client, file_request(file), MAX_DOWNLOAD_BYTES).await?;
    tokio::task::spawn_blocking(move || write_atomically(&path, &bytes))
        .await
        .map_err(|_| MediaError::NotSaved("the save was interrupted".to_owned()))?
        .map_err(|error| MediaError::NotSaved(io_sentence(&error)))?;
    info!("saved a file");
    Ok(())
}

/// Writes `bytes` to `path` through a temporary file in the same
/// directory.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!(".{}.part", std::process::id()));
    let temporary = directory.join(temporary_name);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// An I/O error as a short sentence, without the path.
fn io_sentence(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "Family Chat isn't allowed to write there.".into(),
        std::io::ErrorKind::NotFound => "that folder doesn't exist.".into(),
        std::io::ErrorKind::StorageFull => "the disk is full.".into(),
        _ => "the disk refused the file.".into(),
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{OwnedMxcUri, mxc_uri};

    use super::*;

    #[test]
    fn media_addresses_are_the_authenticated_endpoints() {
        let homeserver = Url::parse("https://hs.example/").expect("a URL");
        let uri: OwnedMxcUri = mxc_uri!("mxc://hs.example/abc123").to_owned();
        let url = media_url(&homeserver, &uri, &MediaFormat::File).expect("an address");
        assert_eq!(
            url.as_str(),
            "https://hs.example/_matrix/client/v1/media/download/hs.example/abc123"
        );
        let url = media_url(
            &homeserver,
            &uri,
            &MediaFormat::Thumbnail(MediaThumbnailSettings::new(
                UInt::from(800u32),
                UInt::from(600u32),
            )),
        )
        .expect("an address");
        assert_eq!(
            url.as_str(),
            "https://hs.example/_matrix/client/v1/media/thumbnail/hs.example/abc123\
             ?width=800&height=600&method=scale&animated=false"
        );
        let bad: OwnedMxcUri = "mxc://".into();
        assert!(media_url(&homeserver, &bad, &MediaFormat::File).is_err());
    }

    #[test]
    fn fetch_failures_read_as_media_errors() {
        assert_eq!(
            media_error(&FetchError::NoAuthenticatedMedia.into_sdk()),
            MediaError::NoAuthenticatedMedia
        );
        assert_eq!(
            media_error(&FetchError::TooLarge { limit: 7 }.into_sdk()),
            MediaError::TooLarge { limit: 7 }
        );
        assert_eq!(
            media_error(&FetchError::Status(404).into_sdk()),
            MediaError::Unavailable
        );
        assert_eq!(
            media_error(&matrix_sdk::Error::InsufficientData),
            MediaError::Unavailable
        );
    }

    #[test]
    fn a_saved_file_replaces_nothing_on_failure_and_lands_whole() {
        let dir = std::env::temp_dir().join(format!(
            "familychat-save-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a directory");
        let path = dir.join("photo.png");
        write_atomically(&path, b"pixels").expect("written");
        assert_eq!(std::fs::read(&path).expect("read back"), b"pixels");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("listed")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
        let missing = dir.join("no-such-folder").join("x.png");
        let error = write_atomically(&missing, b"x").expect_err("no folder");
        assert_eq!(io_sentence(&error), "that folder doesn't exist.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
