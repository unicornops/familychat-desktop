//! `ImageCache`: the timeline's pictures (ADR 0007).
//!
//! The window never fetches or decodes an image. It asks the session
//! (`SessionApi::image`), which fetches the file from the account's
//! homeserver through authenticated media and decodes it within
//! `familychat-security`'s limits on its own runtime; what arrives is RGBA
//! pixels, never a file format, so nothing here could hand an SVG to a
//! renderer. The cache turns those pixels into GPUI images (BGRA) on the
//! background executor and keeps the most recent ones, dropping the
//! others' textures, so scrolling through a room of photos never holds
//! more than [`CACHE_LIMIT`] of them.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use familychat_core::{
    media::{DecodedImage, MediaError, MediaHandle, MediaInfo},
    session::{AccountKey, SessionApi},
};
use gpui::{AppContext as _, Context, RenderImage, Task};
use image::{Delay, Frame, RgbaImage};

/// How many pictures the cache keeps drawn.
pub(crate) const CACHE_LIMIT: usize = 48;

/// Where a picture is.
#[derive(Clone)]
pub(crate) enum ImageState {
    /// Being fetched and decoded.
    Loading,
    /// Ready to draw.
    Ready(Arc<RenderImage>),
    /// It can't be shown, and why.
    Failed(MediaError),
}

/// The pictures of one account's timelines.
pub(crate) struct ImageCache {
    session: Arc<dyn SessionApi>,
    account: AccountKey,
    images: HashMap<MediaHandle, ImageState>,
    /// Ready pictures, least recently drawn first.
    order: VecDeque<MediaHandle>,
    tasks: HashMap<MediaHandle, Task<()>>,
}

impl ImageCache {
    /// An empty cache for `account`.
    pub(crate) fn new(session: Arc<dyn SessionApi>, account: AccountKey) -> Self {
        Self {
            session,
            account,
            images: HashMap::new(),
            order: VecDeque::new(),
            tasks: HashMap::new(),
        }
    }

    /// The picture for `media`, asking the session for it the first time.
    pub(crate) fn get(&mut self, media: &MediaInfo, cx: &mut Context<Self>) -> ImageState {
        let handle = media.source.clone();
        if let Some(state) = self.images.get(&handle) {
            let state = state.clone();
            if matches!(state, ImageState::Ready(_)) {
                self.touch(&handle);
            }
            return state;
        }
        if media.is_svg() {
            // Never fetched: remote SVG is never drawn (PLAN.md, section 5.8).
            let state = ImageState::Failed(MediaError::Svg);
            self.images.insert(handle, state.clone());
            return state;
        }
        self.images.insert(handle.clone(), ImageState::Loading);
        let reply = self.session.image(&self.account, media);
        let key = handle.clone();
        let task = cx.spawn(async move |this, cx| {
            let decoded = reply.await.unwrap_or(Err(MediaError::Unavailable));
            let state = match decoded {
                Ok(decoded) => {
                    let image = cx
                        .background_spawn(async move { render_image(&decoded) })
                        .await;
                    match image {
                        Some(image) => ImageState::Ready(Arc::new(image)),
                        None => ImageState::Failed(MediaError::NotAnImage),
                    }
                }
                Err(error) => ImageState::Failed(error),
            };
            let _ = this.update(cx, |cache, cx| {
                cache.tasks.remove(&key);
                if matches!(state, ImageState::Ready(_)) {
                    cache.order.push_back(key.clone());
                }
                cache.images.insert(key, state);
                cache.evict(cx);
                cx.notify();
            });
        });
        self.tasks.insert(handle, task);
        ImageState::Loading
    }

    /// Forgets a failed picture, so the next look asks again.
    pub(crate) fn retry(&mut self, media: &MediaInfo, cx: &mut Context<Self>) {
        if matches!(
            self.images.get(&media.source),
            Some(ImageState::Failed(MediaError::Unavailable))
        ) {
            self.images.remove(&media.source);
            cx.notify();
        }
    }

    fn touch(&mut self, handle: &MediaHandle) {
        if let Some(position) = self.order.iter().position(|kept| kept == handle) {
            self.order.remove(position);
            self.order.push_back(handle.clone());
        }
    }

    /// Drops the least recently drawn pictures past [`CACHE_LIMIT`], and
    /// their textures.
    fn evict(&mut self, cx: &mut Context<Self>) {
        while self.order.len() > CACHE_LIMIT {
            let Some(handle) = self.order.pop_front() else {
                break;
            };
            if let Some(ImageState::Ready(image)) = self.images.remove(&handle) {
                cx.drop_image(image, None);
            }
        }
    }

    /// How many pictures are ready (tests).
    #[cfg(test)]
    pub(crate) fn ready_count(&self) -> usize {
        self.order.len()
    }
}

/// GPUI's image for decoded RGBA frames: BGRA, frame by frame.
pub(crate) fn render_image(decoded: &DecodedImage) -> Option<RenderImage> {
    let frames: Vec<Frame> = decoded
        .frames
        .iter()
        .map(|frame| {
            let mut bgra = frame.rgba.clone();
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let buffer = RgbaImage::from_raw(decoded.width, decoded.height, bgra)?;
            Some(Frame::from_parts(
                buffer,
                0,
                0,
                Delay::from_numer_denom_ms(frame.delay_ms, 1),
            ))
        })
        .collect::<Option<_>>()?;
    if frames.is_empty() {
        return None;
    }
    Some(RenderImage::new(frames))
}

#[cfg(test)]
mod tests {
    use familychat_core::media::ImageFrame;

    use super::*;

    #[test]
    fn pixels_become_bgra_frames_and_bad_sizes_are_refused() {
        let decoded = DecodedImage {
            width: 1,
            height: 1,
            frames: vec![ImageFrame {
                rgba: vec![10, 20, 30, 255],
                delay_ms: 0,
            }],
        };
        let image = render_image(&decoded).expect("an image");
        assert_eq!(image.as_bytes(0), Some(&[30, 20, 10, 255][..]));
        let wrong = DecodedImage {
            width: 2,
            ..decoded
        };
        assert!(
            render_image(&wrong).is_none(),
            "too few pixels for the size"
        );
    }
}
