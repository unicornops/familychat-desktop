//! The terminal UI on ratatui's `TestBackend`: a snapshot of the screen at
//! 80x24 and 160x48 (in `src/snapshots`), and the keys that quit.
//!
//! `FAMILYCHAT_UPDATE_SNAPSHOTS=1 cargo test -p familychat-tui` rewrites
//! the snapshots; review the diff.

use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::{App, Launch, draw};

fn app() -> App {
    App::new(Launch {
        version: "0.0.0-test".to_owned(),
    })
}

fn render(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| draw(app, frame))
        .expect("the frame draws");
    terminal.backend().buffer().clone()
}

/// The buffer's text, one line per row. Phase 0 draws only ASCII.
fn text_of(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let line: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Compares the screen with `src/snapshots/<name>.txt`.
fn assert_snapshot(name: &str, buffer: &Buffer) {
    let actual = text_of(buffer);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("FAMILYCHAT_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(&path, &actual).expect("the snapshot is written");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no snapshot {}; run with FAMILYCHAT_UPDATE_SNAPSHOTS=1 and review it\n{actual}",
            path.display()
        )
    });
    // Git may check text files out with CRLF on Windows.
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "screen {name} differs from its snapshot:\n{actual}"
    );
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
}

#[test]
fn the_placeholder_matches_its_snapshots() {
    let app = app();
    assert_snapshot("welcome-80x24", &render(&app, 80, 24));
    assert_snapshot("welcome-160x48", &render(&app, 160, 48));
}

#[test]
fn a_tiny_terminal_says_it_is_too_small() {
    let text = text_of(&render(&app(), 30, 4));
    assert!(text.starts_with("Family Chat needs a bigger ter"));
}

#[test]
fn q_quits() {
    for code in [KeyCode::Char('q'), KeyCode::Char('Q')] {
        let mut app = app();
        press(&mut app, code, KeyModifiers::NONE);
        assert!(app.should_quit());
    }
}

#[test]
fn control_c_quits() {
    let mut app = app();
    press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app.should_quit());
}

#[test]
fn other_keys_and_key_releases_do_not_quit() {
    let mut app = app();
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
    let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    app.handle_event(Event::Key(release));
    app.handle_event(Event::Resize(100, 30));
    assert!(!app.should_quit());
}
