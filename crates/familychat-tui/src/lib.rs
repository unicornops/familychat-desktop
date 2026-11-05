//! Family Chat's terminal UI, `familychat tui`.
//!
//! Phase 0 draws a placeholder in Zmail's terminal frame (a title bar, the
//! body, a status line and an always-visible command bar) and quits on `q`.
//! Signing in and reading arrive in Phase 10 (PLAN.md, section 13.10).
//! This crate never depends on GPUI or matrix-sdk (PLAN.md, section 5.1).

use std::{io, time::Duration};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use familychat_core::identity::APP_NAME;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

pub mod terminal;

/// What the terminal UI starts with.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The app's version, shown in the title bar.
    pub version: String,
}

/// Takes the terminal over, runs the UI until the user quits, and gives
/// the terminal back.
pub fn run(launch: Launch) -> Result<()> {
    let mut app = App::new(launch);
    let guard = terminal::TerminalGuard::enter(false).context("couldn't set up the terminal")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("couldn't set up the terminal")?;
    let result = event_loop(&mut app, &mut terminal);
    drop(terminal);
    drop(guard);
    result
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(app, frame))?;
        if app.should_quit() {
            return Ok(());
        }
        if event::poll(TICK)? {
            app.handle_event(event::read()?);
            // Take everything already waiting before drawing again.
            while event::poll(Duration::ZERO)? {
                app.handle_event(event::read()?);
            }
        }
    }
}

/// How long the loop waits for input before drawing again.
const TICK: Duration = Duration::from_millis(250);

/// The terminal UI's state.
#[derive(Debug)]
pub struct App {
    version: String,
    quit: bool,
}

impl App {
    /// A new UI for `launch`.
    pub fn new(launch: Launch) -> Self {
        Self {
            version: launch.version,
            quit: false,
        }
    }

    /// Whether the user asked to quit.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Handles one terminal event: `q`, `Q` or Ctrl-C quits.
    pub fn handle_event(&mut self, event: Event) {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            return;
        };
        // Windows reports releases as well as presses.
        if kind == KeyEventKind::Release {
            return;
        }
        match code {
            KeyCode::Char('q' | 'Q') if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            _ => {}
        }
    }
}

/// The lines the body shows, centred.
const BODY: [&str; 3] = [
    "Family Chat for the terminal is on its way.",
    "Signing in and reading your chats arrive in a later release.",
    "This is alpha software.",
];

/// Draws the whole screen.
pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();
    if area.height < 6 || area.width < 20 {
        frame.buffer_mut().set_stringn(
            0,
            0,
            "Family Chat needs a bigger terminal",
            usize::from(area.width),
            Style::new(),
        );
        return;
    }
    let width = area.width;
    let reversed = Style::new().add_modifier(Modifier::REVERSED);
    let bold = Style::new().add_modifier(Modifier::BOLD);

    // Title bar: the app and version on the left, the screen in the middle.
    let title = Rect::new(0, 0, width, 1);
    frame.buffer_mut().set_style(title, reversed);
    frame.render_widget(
        Paragraph::new(format!("  {} {}", APP_NAME.to_uppercase(), app.version)).style(reversed),
        title,
    );
    frame.render_widget(
        Paragraph::new("WELCOME")
            .alignment(Alignment::Center)
            .style(reversed),
        title,
    );

    // The body, centred vertically in the rows between the title bar and
    // the status line.
    let body = Rect::new(0, 1, width, area.height - 4);
    let lines: Vec<Line> = BODY
        .iter()
        .enumerate()
        .flat_map(|(index, text)| {
            let line = if index == 0 {
                Line::styled(*text, bold)
            } else {
                Line::raw(*text)
            };
            // A blank line between the heading and the rest.
            let gap = (index == 0).then(Line::default);
            std::iter::once(line).chain(gap)
        })
        .collect();
    let top = body.y + body.height.saturating_sub(lines.len() as u16) / 2;
    let text_area = Rect::new(0, top, width, (lines.len() as u16).min(body.height));
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        text_area,
    );

    // The command bar: the keys this screen takes, always on show.
    let command_bar = Rect::new(0, area.height - 2, width, 2);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Q", reversed),
            Span::raw(" Quit"),
        ])),
        command_bar,
    );
}

#[cfg(test)]
mod tests;
