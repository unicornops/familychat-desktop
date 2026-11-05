//! Compile-time embedded assets for Family Chat: icons and the bundled UI
//! font. Copied from Zmail's `zmail-assets`.
//!
//! The icons are Lucide's (<https://lucide.dev>), under the ISC License,
//! with the ones Lucide derives from Feather also under MIT
//! (`assets/icons/LICENSE.txt`). The font is Inter 4.1, under the SIL Open
//! Font License 1.1 (`assets/fonts/inter/LICENSE.txt`). Both licences ship
//! in every release archive; `about.toml` records them.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;

/// Embedded Lucide icon SVGs from the workspace-level `assets/icons/` directory.
///
/// Paths are prefixed with `icons/` so callers use paths like `"icons/send.svg"`.
#[derive(RustEmbed)]
#[folder = "../../assets/icons"]
#[prefix = "icons/"]
struct Icons;

/// GPUI [`AssetSource`] that serves compile-time embedded assets.
///
/// Pass an instance to [`gpui::Application::with_assets`] before calling `.run()`:
///
/// ```rust,ignore
/// gpui_platform::application()
///     .with_assets(FamilyChatAssets)
///     .run(|cx| { /* … */ });
/// ```
pub struct FamilyChatAssets;

impl AssetSource for FamilyChatAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Icons::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Icons::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| SharedString::from(p.into_owned()))
            .collect())
    }
}

/// The bundled UI font's family name.
pub const UI_FONT_FAMILY: &str = "Inter";

/// The bundled Inter faces: Regular, Medium, SemiBold and Bold.
///
/// macOS and Windows use the system UI font (`.SystemUIFont`: SF Pro and
/// Segoe UI Variable). GPUI's Linux text system maps `.SystemUIFont` to IBM
/// Plex Sans, which it does not ship, so Linux builds bundle Inter instead
/// (it matches GNOME's Adwaita Sans). Only the four static weights are
/// bundled: GPUI picks faces by their named weight, so a variable font
/// would render every weight as Regular.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const INTER_FACES: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Bold.ttf"),
];

#[cfg(test)]
mod tests {
    use gpui::AssetSource;

    use super::FamilyChatAssets;

    #[test]
    fn serves_the_embedded_icons() {
        let icon = FamilyChatAssets
            .load("icons/send.svg")
            .expect("loading never fails")
            .expect("the send icon is embedded");
        assert!(icon.starts_with(b"<svg"));
        assert!(
            FamilyChatAssets
                .load("icons/no-such-icon.svg")
                .expect("loading never fails")
                .is_none()
        );
        let listed = FamilyChatAssets
            .list("icons/")
            .expect("listing never fails");
        assert!(listed.iter().any(|path| path.as_ref() == "icons/send.svg"));
    }
}
