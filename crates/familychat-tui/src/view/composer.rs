//! The composer under the timeline (ADR 0008, "Writing"): the `@` list,
//! what a reply or an edit is about, the files waiting, why the last one
//! couldn't go, and the field itself, `ratatui-textarea` drawn with this
//! terminal's styles (no colour under `NO_COLOR`). While the account is
//! paused there is no field: "Chat is paused" and why, read-only.

use familychat_actions::{ComposerState, files::size_label};
use familychat_security::terminal::sanitize_line;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
};

use super::{Theme, paragraph};
use crate::{
    composer::{Composer, MAX_ROWS, Target},
    pane::PaneLine,
    rooms::{Focus, RoomsState},
    text::{self, fit, wrap},
};

/// The most waiting files listed one a line.
const FILES_SHOWN: usize = 3;

/// What the composer draws, worked out before the panes are.
pub(super) struct ComposerParts {
    /// The lines above the field.
    pub(super) lines: Vec<PaneLine>,
    /// The field's rows; none while paused.
    pub(super) rows: u16,
    /// Where all of it goes.
    pub(super) area: Rect,
}

/// The field's rows for its text at `width` cells.
fn rows_for(composer: &Composer, width: usize) -> usize {
    composer
        .editor
        .lines()
        .iter()
        .map(|line| wrap(line, width.max(1)).len().max(1))
        .sum::<usize>()
        .clamp(1, MAX_ROWS)
}

/// The composer of the open room, for a pane `width` cells wide.
pub(super) fn parts(rooms: &RoomsState, theme: Theme, width: u16) -> Option<(Vec<PaneLine>, u16)> {
    let open = rooms.open.as_ref()?;
    let composer = open.composer.as_ref()?;
    let state = ComposerState::of(&rooms.status, &open.room);
    let width_cells = usize::from(width);
    let mut lines = vec![PaneLine::new(Span::styled(
        theme.horizontal().repeat(width_cells),
        theme.faint(),
    ))];
    if state == ComposerState::Paused {
        lines.push(PaneLine::new(Span::styled(
            format!(" {}", ComposerState::PAUSED_TITLE),
            theme.info().add_modifier(Modifier::BOLD),
        )));
        let sentence = Composer::placeholder(state, &open.room.name);
        let sentence = sentence
            .strip_prefix(&format!("{}. ", ComposerState::PAUSED_TITLE))
            .unwrap_or(&sentence);
        lines.extend(paragraph(
            &theme.words(sentence),
            theme.faint(),
            1,
            width_cells,
        ));
        return Some((lines, 0));
    }
    let suggestions = composer.suggestions();
    for (index, member) in suggestions.iter().enumerate() {
        let name = sanitize_line(&member.name());
        let id = sanitize_line(member.user_id.as_str());
        let style = if index == composer.highlight {
            theme.reversed()
        } else {
            Style::new()
        };
        let pointer = if index == composer.highlight {
            theme.pointer()
        } else {
            " "
        };
        lines.push(PaneLine {
            spans: vec![
                Span::styled(
                    fit(
                        &format!(" {pointer} {name}"),
                        width_cells / 2,
                        theme.ellipsis(),
                    ),
                    style,
                ),
                Span::styled(
                    fit(
                        &format!("  {id}"),
                        width_cells - width_cells / 2,
                        theme.ellipsis(),
                    ),
                    theme.faint(),
                ),
            ],
            ..PaneLine::default()
        });
    }
    if !suggestions.is_empty() {
        lines.push(PaneLine::new(Span::styled(
            theme.words(" Tab or Enter picks, Up and Down move, Esc closes the list"),
            theme.faint(),
        )));
    }
    let preview = match &composer.target {
        Target::New => None,
        Target::Reply { sender, text, .. } => Some(format!("Replying to {sender}: {text}")),
        Target::Edit { text, .. } => Some(format!("Editing: {text}")),
    };
    if let Some(preview) = preview {
        let bar = if theme.unicode { "┃" } else { "|" };
        lines.push(PaneLine {
            spans: vec![
                Span::styled(
                    fit(
                        &format!(" {bar} {preview}"),
                        width_cells.saturating_sub(16),
                        theme.ellipsis(),
                    ),
                    theme.bold(),
                ),
                Span::styled("  (Esc cancels)".to_owned(), theme.faint()),
            ],
            ..PaneLine::default()
        });
    }
    for file in composer.files.iter().take(FILES_SHOWN) {
        lines.push(PaneLine::new(Span::styled(
            fit(
                &format!(" [file] {}, {}", file.name, size_label(file.size)),
                width_cells,
                theme.ellipsis(),
            ),
            theme.link(),
        )));
    }
    if composer.files.len() > FILES_SHOWN {
        lines.push(PaneLine::new(Span::styled(
            format!(" and {} more", composer.files.len() - FILES_SHOWN),
            theme.faint(),
        )));
    }
    if !composer.files.is_empty() {
        lines.push(PaneLine::new(Span::styled(
            theme.words(" Backspace in an empty message takes the last file back"),
            theme.faint(),
        )));
    }
    if !composer.checks.is_empty() {
        lines.push(PaneLine::new(Span::styled(
            theme.words(" Checking the file's size against the server's limit…"),
            theme.faint(),
        )));
    }
    if let Some(error) = &composer.error {
        lines.extend(paragraph(
            &theme.words(error),
            theme.danger(),
            1,
            width_cells,
        ));
    }
    let rows = rows_for(composer, width_cells.saturating_sub(2));
    Some((lines, rows as u16))
}

/// Draws the composer's lines and its field in `area`.
pub(super) fn draw(
    app: &mut crate::app::App,
    frame: &mut Frame,
    theme: Theme,
    parts: ComposerParts,
) {
    let area = parts.area;
    let lines_height = area.height.saturating_sub(parts.rows);
    let buffer = frame.buffer_mut();
    for (row, line) in parts
        .lines
        .iter()
        .skip(parts.lines.len().saturating_sub(usize::from(lines_height)))
        .take(usize::from(lines_height))
        .enumerate()
    {
        buffer.set_line(
            area.x,
            area.y + row as u16,
            &ratatui::text::Line::from(line.spans.clone()),
            area.width,
        );
    }
    if parts.rows == 0 {
        return;
    }
    let Some(rooms) = app.rooms_state() else {
        return;
    };
    let focused = rooms.focus == Focus::Composer;
    let Some(open) = rooms.open.as_mut() else {
        return;
    };
    let state = ComposerState::of(&rooms.status, &open.room);
    let placeholder = Composer::placeholder(state, &open.room.name);
    let Some(composer) = open.composer.as_mut() else {
        return;
    };
    let field_y = area.y + lines_height;
    let prompt = if theme.unicode { "› " } else { "> " };
    let prompt_style = if focused { theme.bold() } else { theme.faint() };
    frame
        .buffer_mut()
        .set_string(area.x, field_y, prompt, prompt_style);
    composer
        .editor
        .set_placeholder_text(theme.words(&text::truncate(
            &placeholder,
            usize::from(area.width.saturating_sub(4)),
            theme.ellipsis(),
        )));
    composer.editor.set_placeholder_style(theme.faint());
    composer.editor.set_cursor_style(if focused {
        theme.reversed()
    } else {
        Style::new()
    });
    let field = Rect::new(
        area.x + 2,
        field_y,
        area.width.saturating_sub(2),
        parts.rows.min(area.height),
    );
    frame.render_widget(&composer.editor, field);
}

/// The names Tab found for the attach question, over the bottom of the
/// body.
pub(super) fn draw_choices(frame: &mut Frame, theme: Theme, body: Rect, choices: &[String]) {
    let height = (choices.len() as u16 + 1).min(body.height);
    let top = body.y + body.height - height;
    let area = Rect::new(body.x, top, body.width, height);
    super::clear(frame, area);
    let buffer = frame.buffer_mut();
    buffer.set_string(
        body.x,
        top,
        fit(
            &theme.words(" Files that match (type more of the name, then Tab):"),
            usize::from(body.width),
            theme.ellipsis(),
        ),
        theme.faint(),
    );
    for (row, choice) in choices.iter().enumerate() {
        buffer.set_string(
            body.x,
            top + 1 + row as u16,
            fit(
                &format!("   {choice}"),
                usize::from(body.width),
                theme.ellipsis(),
            ),
            Style::new(),
        );
    }
}
