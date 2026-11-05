//! Family Chat's GPUI frontend.
//!
//! The window is one [`ShellView`]: it opens the profile through the
//! app's [`StartupApi`], shows the keychain and lost-key screens when the
//! profile is not ready, the sign-in screens while nobody is signed in,
//! and the account's rooms and timelines after, with its sync state. It is built from Zmail's
//! component set and follows its patterns: every command is a GPUI action
//! from the commands table, bound in the keymap files, and searchable in
//! the command palette.
//!
//! This crate never depends on matrix-sdk or ratatui (PLAN.md, section
//! 5.1): it draws `familychat-core` view models and sends answers back. It
//! decides no sign-in rule: the sign-in screens draw the `SigninStep`s the
//! sign-in sends and answer with `SigninAnswer`s (ADR 0006, ADR 0011).
//!
//! | Module | Content |
//! |--------|---------|
//! | `shell` | `ShellView`: the window root, the screen it shows, the theme, the palette and toasts |
//! | `profile_screens` | opening the profile, the keychain and lost-key screens |
//! | `signin` | `SigninView`: welcome, the code page (link, pasted or dropped QR image), the confirmation, the password forms, the rate-limit countdown |
//! | `signed_in` | `SignedInView`: the sync banner over the rooms, and the sync-state indicator |
//! | `rooms` | `RoomsView`: the room list, the room header, the member list, the timeline (`TimelineView`), rich text, the image cache and the link dialog (ADR 0007) |
//! | `qr` | reading a sign-in QR code from an image, locally, with `rqrr` |
//! | `command_palette` | `CommandPalette`: Secondary+K's search over the commands table |
//! | `actions` | every command as a GPUI action |
//! | `keymap` | loads `keymaps/default.json` (and `keymaps/macos.json` on macOS) |
//! | `menus` | the macOS menu bar |
//! | `fonts` | registers the bundled Inter font on Linux |
//! | `motion` | reduced motion: the system setting |
//! | `a11y` | screen-reader roles and names (`AccessibleExt`), live regions |
//! | `components` | Zmail's component set: `Button`, `Banner`, `Modal`, `TextInput`, `TitleBar`, `window_frame`, … |
//! | `styles` | design tokens, `PlatformStyle`, `UiColors`, contrast and high-contrast helpers |
//! | `widgets` | `icon`, `empty_state`, `input_card`, … |

use std::sync::Arc;

use familychat_core::{
    AppBootstrap, ThemeAppearance,
    identity::{APP_ID, APP_NAME},
    startup::StartupApi,
};
use gpui::{
    App, AppContext as _, AssetSource, Bounds, TitlebarOptions, WindowAppearance, WindowBounds,
    WindowDecorations, WindowOptions, point, px, size,
};

mod a11y;
mod actions;
mod command_palette;
mod components;
mod fonts;
mod keymap;
mod menus;
mod motion;
mod profile_screens;
mod qr;
mod rooms;
mod shell;
mod signed_in;
mod signin;
mod styles;
mod widgets;

#[cfg(test)]
mod view_tests;

pub use rooms::RoomsLaunch;
pub use shell::ShellView;

use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
use crate::styles::high_contrast::ContrastSetting;

/// The window's size when it opens.
pub const WINDOW_WIDTH: f32 = 1180.0;
/// The window's height when it opens.
pub const WINDOW_HEIGHT: f32 = 780.0;
/// The smallest size the window can be resized to.
const WINDOW_MIN_WIDTH: f32 = 560.0;
const WINDOW_MIN_HEIGHT: f32 = 520.0;

/// What the window shows first, for `familychat launch-ui-fixture` (the
/// screenshot matrix) and the view tests. A normal launch uses
/// [`InitialView::Default`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum InitialView {
    /// Whatever the profile gives: its screen when it is not ready, the
    /// signed-in account, or the welcome screen.
    #[default]
    Default,
    /// The code page, empty.
    CodeEntry,
    /// The code page after an image with no QR code in it.
    CodeEntryNoQrCode,
    /// A sign-in started with this text pasted as a link.
    PastedLink(String),
    /// "Sign in with your password" started.
    Password,
    /// "Use another Matrix server" started.
    OtherServer,
    /// The welcome screen with the command palette open.
    CommandPalette,
    /// The signed-in account's rooms, opened as described.
    Rooms(RoomsLaunch),
}

/// How [`run`] opens the window.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// What the window shows first.
    pub initial_view: InitialView,
    /// Draw in this theme instead of following the OS appearance (the
    /// screenshot fixture).
    pub appearance: Option<ThemeAppearance>,
}

/// Starts GPUI and opens the main window over `startup`; returns when the
/// app quits.
pub fn run(
    asset_source: impl AssetSource,
    bootstrap: AppBootstrap,
    startup: Arc<dyn StartupApi>,
    options: LaunchOptions,
) {
    // `gpui_platform::application()` picks the OS backend (macOS, Windows,
    // or Linux Wayland/X11) and leaves AccessKit enabled; never switch to
    // `Application::new_inaccessible`, which turns screen readers off.
    gpui_platform::application()
        .with_assets(asset_source)
        .run(move |cx: &mut App| {
            init_app(cx);
            if let Err(error) =
                open_main_window(bootstrap.clone(), startup.clone(), options.clone(), cx)
            {
                eprintln!("{APP_NAME}: failed to open the window: {error:#}");
                cx.quit();
            }
        });
}

/// App-wide setup, before the first window opens: the font, the actions
/// and their keys, the menu bar, the contrast and motion settings.
pub(crate) fn init_app(cx: &mut App) {
    fonts::register_ui_font(cx);
    actions::register(cx);
    keymap::bind_keymap(cx);
    menus::init(cx);
    ContrastSetting::refresh(cx);
    motion::follow_system(cx);
    // Before any window or notification: Windows groups windows and
    // attributes toasts by this id, and Linux names notifications by it.
    cx.set_app_identity(APP_ID, APP_NAME);
    // Closing the last window quits, on every platform.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

/// The main window's options: Family Chat draws its own title bar
/// (`components::title_bar`): transparent over the traffic lights on
/// macOS, custom caption buttons on Windows, and a client-side header bar
/// on Linux when the compositor supports it (GNOME on Wayland has no
/// server-side title bar at all).
pub(crate) fn window_options(bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(APP_NAME.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(
                px(TRAFFIC_LIGHT_POSITION.0),
                px(TRAFFIC_LIGHT_POSITION.1),
            )),
        }),
        // The title bar moves the window itself (`start_window_move`), so
        // AppKit must not also drag it or delay its clicks (macOS only).
        app_owns_titlebar_drag: true,
        window_decorations: Some(window_decorations()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
        // Matches the macOS bundle id and, from Phase 12, the packaged
        // desktop file, so Linux docks (StartupWMClass / Wayland app id)
        // find the window.
        app_id: Some(APP_ID.to_owned()),
        ..Default::default()
    }
}

/// Opens the main window on the shell view.
fn open_main_window(
    bootstrap: AppBootstrap,
    startup: Arc<dyn StartupApi>,
    options: LaunchOptions,
    cx: &mut App,
) -> anyhow::Result<gpui::WindowHandle<ShellView>> {
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let window = cx.open_window(window_options(bounds), move |window, cx| {
        cx.new(|cx| ShellView::new(bootstrap, startup, options, window, cx))
    })?;
    window.update(cx, |_, window, _| window.activate_window())?;
    Ok(window)
}

/// The light or dark appearance for an OS window appearance.
pub fn appearance_for(window_appearance: WindowAppearance) -> ThemeAppearance {
    match window_appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
    }
}

/// The decorations to ask the window system for (only Linux reads this;
/// macOS and Windows ignore it), as Zmail asks. `Window::window_decorations`
/// says which the window actually got.
///
/// - GNOME on Wayland has no server-side decorations, so GPUI always draws
///   client-side ones there, whatever is asked for: the title bar
///   component is the window's only title bar.
/// - KDE draws its own (Breeze) title bars and supports them on Wayland and
///   X11, so Family Chat asks for server-side decorations there.
/// - Elsewhere it asks for client-side ones; X11 without a compositor (or a
///   window manager without `_GTK_FRAME_EXTENTS`) falls back to the window
///   manager's title bar.
///
/// `FAMILYCHAT_WINDOW_DECORATIONS=server` or `=client` overrides the choice.
pub(crate) fn window_decorations() -> WindowDecorations {
    decorations_for(
        std::env::var("FAMILYCHAT_WINDOW_DECORATIONS")
            .ok()
            .as_deref(),
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
    )
}

/// [`window_decorations`] for the given override and desktop.
fn decorations_for(requested: Option<&str>, desktop: Option<&str>) -> WindowDecorations {
    match requested {
        Some("server") => WindowDecorations::Server,
        Some("client") => WindowDecorations::Client,
        _ if desktop
            .unwrap_or_default()
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("KDE")) =>
        {
            WindowDecorations::Server
        }
        _ => WindowDecorations::Client,
    }
}
