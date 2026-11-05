//! Family Chat's GPUI frontend.
//!
//! Phase 0 opens one empty Family Chat window in the bundled theme that
//! matches the OS appearance. Sign-in screens, the room list and the
//! timeline arrive in later phases (PLAN.md, section 13). This crate never
//! depends on matrix-sdk or ratatui: it draws `familychat-core` view models
//! (PLAN.md, section 5.1).

use familychat_core::{
    AppBootstrap, ThemeAppearance, ThemeSummary,
    identity::{APP_ID, APP_NAME},
};
use gpui::{
    App, AssetSource, Bounds, Context, FontWeight, Hsla, IntoElement, Render, Rgba, Role,
    SharedString, Subscription, TitlebarOptions, Window, WindowAppearance, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};

mod fonts;

/// The window's size when it opens.
const WINDOW_WIDTH: f32 = 960.0;
const WINDOW_HEIGHT: f32 = 640.0;
/// The smallest size the window can be resized to.
const WINDOW_MIN_WIDTH: f32 = 480.0;
const WINDOW_MIN_HEIGHT: f32 = 360.0;

/// Starts GPUI and opens the main window; returns when the app quits.
pub fn run(asset_source: impl AssetSource, bootstrap: AppBootstrap) {
    // `gpui_platform::application()` picks the OS backend (macOS, Windows,
    // or Linux Wayland/X11) and leaves AccessKit enabled; never switch to
    // `Application::new_inaccessible`, which turns screen readers off.
    gpui_platform::application()
        .with_assets(asset_source)
        .run(move |cx: &mut App| {
            init_app(cx);
            if let Err(error) = open_main_window(bootstrap.clone(), cx) {
                eprintln!("{}: failed to open the window: {error:#}", APP_NAME);
                cx.quit();
            }
        });
}

/// App-wide setup, before the first window opens.
fn init_app(cx: &mut App) {
    fonts::register_ui_font(cx);
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

/// Opens the main window on the shell view.
fn open_main_window(
    bootstrap: AppBootstrap,
    cx: &mut App,
) -> anyhow::Result<gpui::WindowHandle<ShellView>> {
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let window = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(APP_NAME.into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
            // Matches the macOS bundle id and, from Phase 12, the packaged
            // desktop file, so Linux docks (StartupWMClass / Wayland app id)
            // find the window.
            app_id: Some(APP_ID.to_owned()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| ShellView::new(bootstrap, window, cx)),
    )?;
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

/// The empty main window: the app's name, its alpha status and what it was
/// built from, in the bundled theme that matches the OS appearance.
pub struct ShellView {
    bootstrap: AppBootstrap,
    appearance: ThemeAppearance,
    _appearance_changes: Subscription,
}

impl ShellView {
    /// The shell for `window`, following its appearance.
    pub fn new(bootstrap: AppBootstrap, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance_changes = cx.observe_window_appearance(window, |this, window, cx| {
            this.appearance = appearance_for(window.appearance());
            cx.notify();
        });
        Self {
            bootstrap,
            appearance: appearance_for(window.appearance()),
            _appearance_changes: appearance_changes,
        }
    }

    /// The theme the window draws with.
    pub fn theme(&self) -> &ThemeSummary {
        self.bootstrap.theme(self.appearance)
    }

    /// The build line under the title: the version and the GPUI pin.
    pub fn build_line(&self) -> SharedString {
        let upstream = &self.bootstrap.upstream;
        let commit = upstream
            .zed_commit
            .get(..12)
            .unwrap_or(&upstream.zed_commit);
        format!(
            "Version {} · GPUI from Zed {} ({commit})",
            self.bootstrap.app_version, upstream.zed_tag
        )
        .into()
    }
}

/// A theme colour, or `fallback` when the theme leaves it out or it does
/// not parse.
fn color(value: Option<&String>, fallback: u32) -> Hsla {
    value
        .and_then(|hex| Rgba::try_from(hex.as_str()).ok())
        .unwrap_or_else(|| gpui::rgb(fallback))
        .into()
}

impl Render for ShellView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let background = color(theme.background.as_ref(), 0xFFFFFF);
        let foreground = color(theme.foreground.as_ref(), 0x1C1917);
        let muted = color(theme.muted_foreground.as_ref(), 0x57534E);
        let accent = color(theme.accent.as_ref(), 0x0D9488);
        let surface = color(theme.surface.as_ref(), 0xFAFAF9);
        let border = color(theme.border.as_ref(), 0xE7E5E4);

        let root = div()
            .id("shell")
            .role(Role::Main)
            .aria_label(APP_NAME)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .bg(background)
            .text_color(foreground);
        fonts::with_ui_font(root)
            .child(
                div()
                    .text_3xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(accent)
                    .child(APP_NAME),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(surface)
                    .child("Alpha software: signing in arrives in a later release."),
            )
            .child(div().text_sm().text_color(muted).child(self.build_line()))
    }
}

#[cfg(test)]
mod tests;
