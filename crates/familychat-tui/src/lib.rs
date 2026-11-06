//! Family Chat's terminal UI, `familychat tui` (ADR 0008; PLAN.md,
//! section 13.10).
//!
//! A second frontend in Zmail's terminal style: a title bar, the screen,
//! a status line and an always-visible command bar built from the shared
//! commands table, on every screen; the `:` palette; the mouse, with
//! Zellij-style selection and copy. It opens the profile through the same
//! `StartupApi` as the window (the keychain and lost-key states included),
//! signs in through `SigninApi` (a pasted link, always confirmed before
//! its code is used, or a password), and reads: the room list and the
//! rooms side by side from 120 columns, timelines drawn from
//! `familychat-core`'s rows and rich text, numbered links that ask first
//! with the whole address, files saved where the person types, and new
//! messages in the status line with an optional bell. It writes too
//! (Phase 11): a composer with Markdown, mentions and files by path,
//! replies, edits, reactions and deletions through `familychat-actions`,
//! invitations answered, "Chat is paused" while the account is, and an
//! opt-in external editor; and its encryption: "Confirm it's you" by
//! emoji or the recovery key, requests from other devices, and recovery
//! set up with the key shown once.
//!
//! - **Terminal safety.** Every string from a server goes through
//!   [`familychat_security::terminal`] before it is drawn. The only escape
//!   sequences written are ratatui's drawing, the bell and OSC 52 for a
//!   copy the person made.
//! - **Off the network.** Everything arrives on `familychat-core`'s
//!   channels, polled on each tick; nothing here blocks on the network,
//!   the disk or the keychain.
//! - **Boundaries.** No GPUI and no matrix-sdk in this crate's graph
//!   (`scripts/check-dependency-boundaries.sh`).

mod app;
mod clipboard;
mod commands;
mod composer;
mod editor;
mod encryption;
#[cfg(any(test, feature = "fixture-views"))]
pub mod fixture_views;
mod links;
mod palette;
mod pane;
mod rooms;
mod signin;
mod tasks;
pub mod terminal;
mod text;
mod view;
mod writing;

#[cfg(test)]
mod tests;

use std::{
    io::{self, Write},
    sync::Arc,
};

use anyhow::{Context, Result};
use crossterm::event;
use familychat_core::startup::StartupApi;
use familychat_security::terminal::sanitize_line;
use ratatui::{Terminal, backend::CrosstermBackend};

pub use app::{
    App, AppOptions, Clock, CommandEntry, Effect, Environment, InitialView, RoomsLaunch,
    SIDE_BY_SIDE_FROM, Settings,
};
pub use clipboard::{CopyClipboard, CopySettings};
pub use commands::Cmd;
pub use view::draw;

use crate::clipboard::{CopyTarget, copy_target, osc52, run_copy_command};

/// What [`run`] needs from the app.
pub struct Launch {
    /// Opens the profile (the app's `LiveStartup`, or a fixture's).
    pub startup: Arc<dyn StartupApi>,
    /// The app's version, for the title bar.
    pub version: String,
    /// The person's choices.
    pub settings: Settings,
    /// Where it starts.
    pub initial: InitialView,
}

/// Takes the terminal over, runs the UI until the person quits, and gives
/// the terminal back.
pub fn run(launch: Launch) -> Result<()> {
    let mut app = App::new(AppOptions {
        startup: launch.startup,
        env: Environment::detect(),
        clock: Clock::local(),
        settings: launch.settings,
        version: launch.version,
        initial: launch.initial,
        downloads: downloads_dir(),
    });
    let guard = terminal::TerminalGuard::enter(app.settings().mouse)
        .context("couldn't set up the terminal")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("couldn't set up the terminal")?;
    let result = event_loop(&mut app, &mut terminal);
    drop(terminal);
    drop(guard);
    result
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    loop {
        terminal.draw(|frame| view::draw(app, frame))?;
        run_effects(app, terminal)?;
        if app.should_quit() {
            return Ok(());
        }
        if event::poll(app.tick_interval())? {
            app.handle_event(event::read()?);
            // Take everything already waiting before drawing again, so a
            // fast drag or a burst of keys doesn't queue up frames.
            while event::poll(std::time::Duration::ZERO)? {
                app.handle_event(event::read()?);
            }
        }
        app.tick();
    }
}

fn run_effects(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    for effect in app.take_effects() {
        match effect {
            Effect::Copy(mut text) => {
                let env = Environment::detect();
                match copy_target(&app.settings().copy, env.macos, env.over_ssh) {
                    CopyTarget::Osc52(clipboard) => {
                        let backend = terminal.backend_mut();
                        let mut sequence = osc52(&text, clipboard);
                        backend.write_all(sequence.as_bytes())?;
                        backend.flush()?;
                        // A copy may be the recovery key: no copy of it
                        // stays behind in memory.
                        zeroize::Zeroize::zeroize(&mut sequence);
                        zeroize::Zeroize::zeroize(&mut text);
                    }
                    CopyTarget::Command(command) => {
                        let notices = app.notice_sender();
                        std::thread::spawn(move || {
                            if let Err(error) = run_copy_command(&command, &text) {
                                let _ = notices.send(format!(
                                    "Copy failed: {}",
                                    sanitize_line(&format!("{error:#}"))
                                ));
                            }
                        });
                    }
                }
            }
            Effect::Bell => {
                let backend = terminal.backend_mut();
                backend.write_all(b"\x07")?;
                backend.flush()?;
            }
            Effect::MouseCapture(on) => terminal::set_mouse_capture(on)?,
            Effect::Open(url) => {
                if let Err(error) = links::open(&url) {
                    let _ = app.notice_sender().send(format!("{error:#}"));
                }
            }
            Effect::Redraw => terminal.clear()?,
            Effect::Edit { command, text } => {
                // The editor gets the terminal to itself, then the TUI
                // takes it back and draws everything again (Zmail's).
                terminal::restore();
                let result = editor::edit(&std::env::temp_dir(), &text, &command);
                let mut text = text;
                zeroize::Zeroize::zeroize(&mut text);
                terminal::resume(app.settings().mouse)?;
                terminal.clear()?;
                app.external_edit_done(result);
            }
        }
    }
    Ok(())
}

/// Where the save question starts: the Downloads folder, else the home
/// folder (as the window's save dialog).
pub fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}
