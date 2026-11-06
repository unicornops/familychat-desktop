//! The encryption screens and dialogs (ADR 0008, "Encryption"; ADR 0009):
//! "Confirm it's you" in the rooms' place (the ways to confirm, the
//! recovery key's masked field), the page asking for the password the
//! server wants, and the dialogs over the body (the verification with the
//! seven emoji and their names, skipping, recovery with the key shown
//! once). The wording is `familychat-core`'s; a device's name and every
//! error go through the terminal sanitiser.

use familychat_core::encryption::{SasEmoji, VerificationStep, wording};
use familychat_security::terminal::sanitize_line;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::Span,
};

use super::{Theme, answers, centred, draw_box, draw_pane, draw_question, paragraph};
use crate::{
    app::App,
    encryption::{Dialog, EncryptionState, Page},
    pane::{PaneId, PaneLine},
    text,
};

/// The widest the verification dialog gets, so the emoji fit in a row.
const VERIFY_WIDTH: u16 = 96;
/// One emoji's column in the row.
const EMOJI_COLUMN: usize = 12;

/// "Confirm it's you" or the password page, in the rooms' place.
pub(super) fn draw_screen(app: &mut App, frame: &mut Frame, theme: Theme, area: Rect) {
    let Some(state) = app.rooms_ref().map(|rooms| &rooms.encryption) else {
        return;
    };
    let width = usize::from(area.width.min(76)).saturating_sub(4);
    let indent = usize::from(area.width.saturating_sub(width as u16) / 2);
    let mut lines = Vec::new();
    let mut field_line = None;
    let heading = |lines: &mut Vec<PaneLine>, title: &str, detail: &str| {
        lines.push(PaneLine::new(Span::styled(
            format!("{}{}", " ".repeat(indent), theme.words(title)),
            theme.heading(),
        )));
        lines.extend(paragraph(
            &theme.words(detail),
            Style::new(),
            indent,
            indent + width,
        ));
        lines.push(PaneLine::default());
    };
    let problem = |lines: &mut Vec<PaneLine>, state: &EncryptionState| {
        if let Some(problem) = &state.problem {
            lines.extend(paragraph(
                &theme.words(problem),
                theme.danger(),
                indent,
                indent + width,
            ));
            lines.push(PaneLine::default());
        }
    };
    let field = |lines: &mut Vec<PaneLine>, label: &str, state: &EncryptionState| {
        let masked: String = std::iter::repeat_n(theme.masked(), state.field.len()).collect();
        lines.push(PaneLine {
            spans: vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(format!("{label}: "), theme.bold()),
                Span::styled(masked, Style::new().add_modifier(Modifier::UNDERLINED)),
            ],
            ..PaneLine::default()
        });
    };
    if state.asks_for_password() {
        heading(
            &mut lines,
            wording::PASSWORD_TITLE,
            wording::PASSWORD_DETAIL,
        );
        problem(&mut lines, state);
        field_line = Some((lines.len(), indent + 10 + state.field.len()));
        field(&mut lines, "Password", state);
        lines.push(PaneLine::default());
        let hint = if state.working {
            "Setting up encryption…"
        } else {
            "Enter sets up encryption. Esc skips it for now."
        };
        lines.extend(paragraph(
            &theme.words(hint),
            theme.faint(),
            indent,
            indent + width,
        ));
    } else if state.page == Page::RecoveryKey {
        heading(
            &mut lines,
            wording::ENTER_KEY_TITLE,
            wording::ENTER_KEY_DETAIL,
        );
        problem(&mut lines, state);
        field_line = Some((lines.len(), indent + 14 + state.field.len()));
        field(&mut lines, "Recovery key", state);
        lines.push(PaneLine::default());
        let hint = if state.working {
            "Checking your recovery key…"
        } else {
            "Enter confirms. Esc goes back to the other ways."
        };
        lines.extend(paragraph(
            &theme.words(hint),
            theme.faint(),
            indent,
            indent + width,
        ));
    } else {
        heading(&mut lines, wording::CONFIRM_TITLE, wording::CONFIRM_DETAIL);
        problem(&mut lines, state);
        let device_detail = if state.status.other_devices {
            "Compare emoji with a phone or computer that's already signed in."
        } else {
            "Sign in on your phone or another computer first, then verify from there."
        };
        let mut choices = vec![("d", wording::USE_ANOTHER_DEVICE, device_detail)];
        if state.status.can_use_recovery_key() {
            choices.push((
                "r",
                wording::USE_RECOVERY_KEY,
                wording::USE_RECOVERY_KEY_DETAIL,
            ));
        }
        choices.push((
            "s",
            wording::SKIP,
            "You can confirm later: V, or the command palette.",
        ));
        for (key, title, detail) in choices {
            lines.push(PaneLine {
                spans: vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(key.to_uppercase(), theme.reversed()),
                    Span::styled(format!(" {}", theme.words(title)), theme.bold()),
                ],
                ..PaneLine::default()
            });
            lines.extend(paragraph(
                &theme.words(detail),
                theme.faint(),
                indent + 2,
                indent + width,
            ));
            lines.push(PaneLine::default());
        }
    }
    let top = usize::from(area.height).saturating_sub(lines.len()) / 2;
    let lines = centred(lines, usize::from(area.height));
    let takes_text = state.takes_text();
    draw_pane(app, frame, PaneId::Profile, area, lines, None);
    if takes_text && let Some((line, column)) = field_line {
        let y = area.y + (top + line) as u16;
        let x = area.x + column.min(usize::from(area.width.saturating_sub(1))) as u16;
        if y < area.y + area.height {
            frame.set_cursor_position(Position::new(x, y));
        }
    }
}

/// The encryption dialog over the body, if one is open.
pub(super) fn draw_dialog(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let Some(state) = app.rooms_ref().map(|rooms| &rooms.encryption) else {
        return;
    };
    match state.dialog {
        Dialog::None => {}
        Dialog::SkipWarning => draw_question(
            app,
            frame,
            theme,
            body,
            wording::SKIP_WARNING_TITLE,
            &[(wording::SKIP_WARNING.to_owned(), Style::new())],
            &[("Y", "Skip for now"), ("N", "Go back")],
        ),
        Dialog::SetUpRecovery => {
            let mut paragraphs = vec![(wording::RECOVERY_DETAIL.to_owned(), Style::new())];
            if let Some(problem) = &state.problem {
                paragraphs.push((problem.clone(), theme.danger()));
            }
            let keys: &[(&str, &str)] = if state.working {
                paragraphs.push((
                    "Making your recovery key and backing up your message keys…".to_owned(),
                    theme.faint(),
                ));
                &[]
            } else {
                &[("Y", "Make my recovery key"), ("N", "Not now")]
            };
            draw_question(
                app,
                frame,
                theme,
                body,
                wording::RECOVERY_TITLE,
                &paragraphs,
                keys,
            );
        }
        Dialog::RecoveryKey => {
            let key = state
                .recovery_key
                .as_ref()
                .map(|key| sanitize_line(key.expose()))
                .unwrap_or_default();
            let copied = state.key_copied;
            let mut keys = vec![("C", "Copy"), ("W", "I wrote it down")];
            if copied {
                keys.push(("Enter", "Done"));
            }
            draw_question(
                app,
                frame,
                theme,
                body,
                wording::RECOVERY_KEY_TITLE,
                &[
                    (key, theme.bold()),
                    (wording::RECOVERY_KEY_DETAIL.to_owned(), Style::new()),
                ],
                &keys,
            );
        }
        Dialog::Verification => draw_verification(app, frame, theme, body),
    }
}

fn draw_verification(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let Some(state) = app.rooms_ref().map(|rooms| &rooms.encryption) else {
        return;
    };
    let with = state
        .flow
        .as_ref()
        .map(|flow| sanitize_line(&flow.with.label()))
        .unwrap_or_else(|| "your other device".to_owned());
    let incoming = state.step == VerificationStep::Incoming;
    let title = if incoming {
        wording::INCOMING_TITLE
    } else {
        wording::VERIFY_TITLE
    };
    let width = body.width.min(VERIFY_WIDTH).saturating_sub(4);
    let inner = usize::from(width.saturating_sub(4));
    let mut lines = vec![
        PaneLine::new(Span::styled(theme.words(title), theme.bold())),
        PaneLine::default(),
    ];
    let say = |lines: &mut Vec<PaneLine>, text: &str, style: Style| {
        lines.extend(paragraph(&theme.words(text), style, 0, inner));
    };
    let keys: Vec<(&str, &str)> = match &state.step {
        VerificationStep::Waiting => {
            say(
                &mut lines,
                if state.working {
                    "Sending the request…"
                } else {
                    wording::WAITING
                },
                Style::new(),
            );
            vec![("N", "Cancel")]
        }
        VerificationStep::Incoming => {
            say(
                &mut lines,
                &format!(
                    "{with} wants to verify this device. Accept only if it's yours and you're \
                     using it now."
                ),
                Style::new(),
            );
            vec![("Y", "Accept"), ("N", "Decline")]
        }
        VerificationStep::Ready { .. } => {
            // The terminal shows no QR code: emoji it is.
            say(&mut lines, wording::READY_NO_QR, Style::new());
            vec![("E", "Compare emoji"), ("N", "Cancel")]
        }
        VerificationStep::Starting => {
            say(&mut lines, "Getting the emoji ready…", theme.faint());
            vec![("N", "Cancel")]
        }
        VerificationStep::CompareEmoji(emoji) => {
            say(&mut lines, wording::COMPARE, Style::new());
            lines.push(PaneLine::default());
            lines.extend(emoji_lines(emoji, theme, inner));
            vec![("Y", "They match"), ("N", "They don't match")]
        }
        VerificationStep::Confirming => {
            say(&mut lines, wording::CONFIRMING, theme.faint());
            vec![("N", "Cancel")]
        }
        VerificationStep::Scanned => {
            say(&mut lines, wording::SCANNED, Style::new());
            vec![("Y", "Yes, it does"), ("N", "No")]
        }
        VerificationStep::Done => {
            say(
                &mut lines,
                wording::DONE,
                theme.info().add_modifier(Modifier::BOLD),
            );
            vec![("Enter", "Done")]
        }
        VerificationStep::Cancelled(why) => {
            say(&mut lines, &sanitize_line(why), theme.danger());
            vec![("Enter", "Close")]
        }
    };
    lines.push(PaneLine::default());
    lines.push(answers(theme, &keys));
    draw_box(app, frame, theme, body, width, lines);
}

/// The seven emoji: in a row, each over its name, where the dialog is wide
/// enough, else one a line; ASCII terminals show the names only.
fn emoji_lines(emoji: &[SasEmoji], theme: Theme, width: usize) -> Vec<PaneLine> {
    let symbol = |emoji: &SasEmoji| sanitize_line(&emoji.symbol);
    let name = |emoji: &SasEmoji| sanitize_line(&emoji.description);
    if width >= EMOJI_COLUMN * emoji.len() {
        let centre = |text: &str| {
            let room = EMOJI_COLUMN.saturating_sub(text::width(text));
            format!(
                "{}{text}{}",
                " ".repeat(room / 2),
                " ".repeat(room - room / 2)
            )
        };
        let mut lines = Vec::new();
        if theme.unicode {
            lines.push(PaneLine::new(Span::styled(
                emoji.iter().map(|e| centre(&symbol(e))).collect::<String>(),
                theme.bold(),
            )));
        }
        lines.push(PaneLine::new(Span::styled(
            emoji
                .iter()
                .map(|e| {
                    centre(&text::truncate(
                        &name(e),
                        EMOJI_COLUMN - 1,
                        theme.ellipsis(),
                    ))
                })
                .collect::<String>(),
            theme.bold(),
        )));
        return lines;
    }
    emoji
        .iter()
        .enumerate()
        .map(|(index, emoji)| {
            let shown = if theme.unicode {
                format!("{}. {}  {}", index + 1, symbol(emoji), name(emoji))
            } else {
                format!("{}. {}", index + 1, name(emoji))
            };
            PaneLine::new(Span::styled(shown, theme.bold()))
        })
        .collect()
}
