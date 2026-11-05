//! The profile screens: opening, the keychain locked or unreachable (Try
//! again), the data's key gone (Start over), something else (Try again).
//! The sentences are `familychat-store`'s, through the `StartupApi`; the
//! headings are the window's (`familychat-ui`'s `profile_screens`).

use familychat_core::identity::APP_NAME;
use familychat_security::terminal::sanitize;
use ratatui::{Frame, layout::Rect, text::Span};

use super::{Theme, centred, draw_pane, paragraph};
use crate::{
    app::{App, ProfileScreen, Screen},
    pane::{PaneId, PaneLine},
};

pub(super) fn draw(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let Screen::Profile(screen) = &app.screen else {
        return;
    };
    let width = usize::from(body.width.min(76));
    let indent = usize::from(body.width.saturating_sub(76) / 2) + 2;
    let mut lines = Vec::new();
    let heading = |text: &str| PaneLine {
        spans: vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(theme.words(text), theme.bold()),
        ],
        prefix: indent,
        ..PaneLine::default()
    };
    let text = |text: &str, style| paragraph(&theme.words(text), style, indent, width + indent - 2);
    match screen {
        ProfileScreen::Opening => {
            lines.push(heading("Opening your data…"));
        }
        ProfileScreen::Keychain(message) => {
            lines.push(heading(&format!("{APP_NAME} can't reach the keychain")));
            lines.push(PaneLine::default());
            lines.extend(text(&sanitize(message), Default::default()));
            lines.push(PaneLine::default());
            lines.extend(text(
                "Press t to try again once the keychain is unlocked or running.",
                theme.faint(),
            ));
        }
        ProfileScreen::LostKey(message) => {
            lines.push(heading(&format!("{APP_NAME} can't open your data")));
            lines.push(PaneLine::default());
            lines.extend(text(&sanitize(message), Default::default()));
            lines.push(PaneLine::default());
            lines.extend(text(
                "Start over keeps the old data in a folder beside the new one, in case the key \
                 turns up again. Press s to start over.",
                theme.faint(),
            ));
        }
        ProfileScreen::Failed(message) => {
            lines.push(heading(&format!("{APP_NAME} couldn't open its data")));
            lines.push(PaneLine::default());
            lines.extend(text(
                "Nothing was changed. Fix the problem below (free some disk space, for \
                 example) and press t to try again.",
                theme.faint(),
            ));
            lines.push(PaneLine::default());
            lines.extend(text(&sanitize(message), theme.danger()));
        }
    }
    let lines = centred(lines, usize::from(body.height));
    draw_pane(app, frame, PaneId::Profile, body, lines, None);
}
