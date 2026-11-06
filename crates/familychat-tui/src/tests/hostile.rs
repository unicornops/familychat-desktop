//! Hostile text on screen: rooms, members, messages, links, file names and
//! reactions full of terminal escape sequences (`familychat-test-support`'s
//! `with_hostile_at`) are drawn, and nothing that could reach the terminal
//! as a control survives, nor any payload the attacks carry. The
//! sanitiser's own corpus is `familychat-security`'s
//! `tests/terminal_corpus.rs`.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;

use super::{Screen, Variant, app_for, text_of};
use crate::{app::Prompt, rooms::Reference};

/// Bidirectional and zero-width characters, listed independently of the
/// sanitiser's own list.
const INVISIBLE: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}',
    '\u{202D}', '\u{202E}', '\u{2060}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}', '\u{2066}',
    '\u{2067}', '\u{2068}', '\u{2069}', '\u{FEFF}',
];

/// What the hostile fixture's attacks carry, which must never show.
const PAYLOADS: &[&str] = &[
    "pwned", "ZXZpbA", "]8;;", "52;c", "?1049", "tmux;", "Gf=100",
];

/// Fails when `text` (a drawn screen) holds a control character, an
/// escape, a C1 control or an invisible format character.
pub(crate) fn assert_clean(label: &str, text: &str) {
    for (index, ch) in text.char_indices() {
        let code = ch as u32;
        let bad = match ch {
            '\n' => None,
            _ if code < 0x20 => Some("a C0 control"),
            '\u{7F}' => Some("DEL"),
            '\u{80}'..='\u{9F}' => Some("a C1 control"),
            _ if INVISIBLE.contains(&ch) => Some("an invisible format character"),
            _ => None,
        };
        assert!(
            bad.is_none(),
            "{label}: {} (U+{code:04X}) at byte {index}:\n{text}",
            bad.unwrap_or_default()
        );
    }
}

fn assert_hostile_screen(label: &str, buffer: &Buffer) {
    // Every cell, not only the text a row trims to.
    for cell in &buffer.content {
        assert_clean(label, cell.symbol());
    }
    let text = text_of(buffer);
    assert_clean(label, &text);
    for payload in PAYLOADS {
        assert!(
            !text.contains(payload),
            "{label}: the payload {payload:?} survived:\n{text}"
        );
    }
}

#[test]
fn hostile_room_names_display_names_and_bodies_draw_clean() {
    for (width, height) in [(80, 24), (160, 48), (200, 60)] {
        for variant in [Variant::Default, Variant::Ascii] {
            // The list, with the hostile rooms' names, previews and inviter.
            let (mut app, _startup) = app_for("hostile", variant);
            let mut screen = Screen::new(width, height);
            let buffer = screen.settle(&mut app);
            assert_hostile_screen("the hostile room list", &buffer);
            assert!(text_of(&buffer).contains("Smith family"));

            // The room: senders, bodies, a formatted link, a reply, a file
            // name, reactions, a room change, the topic and the members.
            let (mut app, _startup) = app_for("hostile-room", variant);
            let mut screen = Screen::new(width, height);
            let buffer = screen.settle(&mut app);
            assert_hostile_screen("the hostile room and its members", &buffer);
            // The room's commands, not the composer, take the keys below.
            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            let text = text_of(&buffer);
            assert!(text.contains("Mum Dad"), "{text}");
            if width < 120 {
                // The members take the room's place here; m shows the
                // timeline again.
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('m'),
                    KeyModifiers::NONE,
                )));
            }
            let buffer = screen.settle(&mut app);
            assert_hostile_screen("the hostile room", &buffer);
            let text = text_of(&buffer);
            assert!(text.contains("Visit bank"), "{text}");
            // The oldest message, above the composer's rows.
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::NONE,
            )));
            let buffer = screen.settle(&mut app);
            assert_hostile_screen("the hostile room, older", &buffer);
            let text = text_of(&buffer);
            assert!(text.contains("Hello red"), "{text}");

            // The link's question, with its address.
            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
            let mut found_link = false;
            for _ in 0..8 {
                screen.draw(&mut app);
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )));
                screen.draw(&mut app);
                if matches!(app.prompt, Some(Prompt::Link(_))) {
                    found_link = true;
                    break;
                }
                app.prompt = None;
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::BackTab,
                    KeyModifiers::SHIFT,
                )));
            }
            assert!(found_link, "the hostile link asks first");
            let buffer = screen.draw(&mut app);
            assert_hostile_screen("the hostile link's question", &buffer);
            app.prompt = None;

            // The save question, with the file's name.
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('l'),
                KeyModifiers::NONE,
            )));
            let file = app
                .rooms_ref()
                .and_then(|rooms| rooms.open.as_ref())
                .and_then(|open| open.layout.as_ref())
                .and_then(|layout| {
                    layout
                        .refs
                        .iter()
                        .position(|reference| matches!(reference, Reference::File(_)))
                })
                .expect("the hostile file is numbered");
            app.handle_event(Event::Paste((file + 1).to_string()));
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
            let buffer = screen.draw(&mut app);
            assert!(
                matches!(app.prompt, Some(Prompt::Save { .. })),
                "the file asks where"
            );
            assert_hostile_screen("the save question", &buffer);
            app.prompt = None;
        }
    }
}

#[test]
fn hostile_status_lines_and_titles_are_filtered_too() {
    let (mut app, _startup) = app_for("hostile-room", Variant::Default);
    let mut screen = Screen::new(120, 30);
    screen.settle(&mut app);
    // Tab through every reference: each status line names a link's
    // address or a file's name.
    for _ in 0..6 {
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        let buffer = screen.draw(&mut app);
        assert_hostile_screen("a reference on the status line", &buffer);
        if let Some(status) = app.status() {
            assert_clean("the status", status);
        }
    }
}
