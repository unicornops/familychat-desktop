//! Domain types and view models that both Family Chat frontends draw.
//!
//! This crate has no GPUI, ratatui or network dependency (PLAN.md, section
//! 5.1). In Phase 0 it holds the app's identifiers and what the shell shows
//! about itself; the Matrix view models arrive with the session in Phase 3.

pub mod identity;

/// Facts about the dedicated background runtime, shown by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSummary {
    /// Number of Tokio worker threads configured for the dedicated background runtime.
    pub worker_threads: usize,
    /// Whether a background task probe completed successfully during startup.
    pub bridge_verified: bool,
}

/// Light-or-dark appearance for a bundled UI theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeAppearance {
    /// Light theme variant.
    Light,
    /// Dark theme variant.
    Dark,
}

/// The colours a frontend draws with, from a Zed-format theme variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSummary {
    /// Theme family name from the unchanged Zed-format theme file.
    pub family_name: String,
    /// Selected theme name within the family.
    pub theme_name: String,
    /// Light or dark appearance of the active theme.
    pub appearance: ThemeAppearance,
    /// App background color.
    pub background: Option<String>,
    /// Surface color for cards and panes.
    pub surface: Option<String>,
    /// Border color.
    pub border: Option<String>,
    /// Primary text color.
    pub foreground: Option<String>,
    /// Muted / secondary text color.
    pub muted_foreground: Option<String>,
    /// Accent color.
    pub accent: Option<String>,
    /// Selected item background color.
    pub selected_background: Option<String>,
    /// Hovered item background color.
    pub hover_background: Option<String>,
    /// Danger / error semantic color.
    pub danger: Option<String>,
    /// Warning semantic color.
    pub warning: Option<String>,
    /// Success semantic color.
    pub success: Option<String>,
    /// Info semantic color.
    pub info: Option<String>,
    /// Button / interactive element background color.
    pub button_background: Option<String>,
    /// Error banner background color.
    pub danger_background: Option<String>,
    /// Warning banner background color.
    pub warning_background: Option<String>,
    /// Success banner background color.
    pub success_background: Option<String>,
}

/// The upstream GPUI the binary was built from (docs/upstream/zed.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPins {
    /// Zed commit GPUI is built from, as `Cargo.lock` records it.
    pub zed_commit: String,
    /// Zed stable release tag the GPUI git dependency is pinned to.
    pub zed_tag: String,
}

/// What the GUI shell starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppBootstrap {
    /// The app's version, as released.
    pub app_version: String,
    /// Background runtime summary.
    pub runtime: RuntimeSummary,
    /// The bundled dark theme.
    pub dark_theme: ThemeSummary,
    /// The bundled light theme.
    pub light_theme: ThemeSummary,
    /// Upstream provenance.
    pub upstream: UpstreamPins,
}

impl AppBootstrap {
    /// The bundled theme for `appearance`; the window follows the OS
    /// appearance until the user can pick one.
    pub fn theme(&self, appearance: ThemeAppearance) -> &ThemeSummary {
        match appearance {
            ThemeAppearance::Dark => &self.dark_theme,
            ThemeAppearance::Light => &self.light_theme,
        }
    }
}
