//! Domain types and view models that both Family Chat frontends draw.
//!
//! This crate has no GPUI, ratatui or network dependency, and no matrix-sdk
//! type in its API: only ruma's identifiers (PLAN.md, section 5.1;
//! `scripts/check-dependency-boundaries.sh` checks it). It holds:
//!
//! - [`identity`]: the app's names and identifiers;
//! - [`commands`]: the commands table both frontends build their keys,
//!   menus and palettes from, with [`fuzzy`], the palettes' matcher;
//! - [`startup`]: opening the profile, as the window draws it
//!   ([`StartupApi`](startup::StartupApi));
//! - [`session`]: the [`SessionApi`](session::SessionApi) both frontends
//!   consume, and the channels its answers arrive on (ADR 0005);
//! - [`signin`]: the sign-in steps and answers both frontends exchange
//!   with `familychat-signin`, and the [`SigninApi`](signin::SigninApi)
//!   that starts them;
//! - [`sync`], [`rooms`], [`timeline`]: the view models (sync state, room
//!   summaries and members, timeline items and their rows) with their
//!   wording;
//! - [`rich_text`]: the rich-text AST a message's text becomes (ADR 0007);
//! - [`links`]: which links open in the app and which ask first;
//! - [`media`]: images and files, as the session hands them over;
//! - [`notifications`]: the notifications the session reports, for
//!   `familychat-notify` to compose;
//! - [`settings`]: the preferences a frontend reads and changes
//!   ([`SettingsApi`](settings::SettingsApi));
//! - [`requests`]: what reaches the running window from outside it (a
//!   sign-in link from a second launch or the OS);
//! - [`messaging`]: what a frontend asks the session to do in a room
//!   (sends, edits, reactions, files, invitations, drafts), as
//!   `familychat-actions` prepares it;
//! - [`diff`]: [`ListDiff`](diff::ListDiff), the changes the room list and
//!   timelines arrive as;
//! - what the shell shows about itself.

pub mod commands;
pub mod diff;
pub mod fuzzy;
pub mod identity;
pub mod links;
pub mod media;
pub mod notifications;
pub mod requests;
pub mod messaging;
pub mod rich_text;
pub mod rooms;
pub mod session;
pub mod settings;
pub mod signin;
pub mod startup;
pub mod sync;
pub mod timeline;

/// ruma's identifier types, as the view models use them, so a frontend
/// does not need its own ruma dependency.
pub use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UserId};

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
