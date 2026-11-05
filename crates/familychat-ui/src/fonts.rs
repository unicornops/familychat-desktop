//! The bundled UI font (copied from Zmail's `zmail-ui`).
//!
//! macOS and Windows use the system UI font (`.SystemUIFont`: SF Pro and
//! Segoe UI Variable). GPUI's Linux text system maps `.SystemUIFont` to
//! IBM Plex Sans, which it does not ship, so text would fall back to
//! whatever fontconfig picks. Linux builds bundle Inter instead
//! (`familychat_assets::INTER_FACES`) and register it before the first
//! window opens.

use gpui::App;

/// Registers the platform's bundled UI font, if it has one. Call once at
/// startup, before opening a window. A failure is logged and the UI falls
/// back to the system's default sans-serif font.
pub(crate) fn register_ui_font(cx: &mut App) {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let faces = familychat_assets::INTER_FACES
            .iter()
            .map(|face| std::borrow::Cow::Borrowed(*face))
            .collect();
        if let Err(error) = cx.text_system().add_fonts(faces) {
            eprintln!(
                "familychat: could not register the bundled {} font: {error:#}",
                familychat_assets::UI_FONT_FAMILY
            );
        }
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // Only Linux bundles a font.
        let _ = cx;
    }
}
