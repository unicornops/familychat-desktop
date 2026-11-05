//! Drawing: Zmail's three bands on every screen (the title bar, the
//! screen's body, the status line and the two-line command bar), with
//! colours unless `NO_COLOR` and box drawing unless ASCII.
//!
//! The frame, [`draw_pane`], [`styled`], the palette's lines and the
//! command bar are adapted from Zmail's `zmail-tui` (`src/view.rs`),
//! Copyright UnicornOps, GPL-3.0-only like this project; keep this
//! notice. Every string from a server is filtered with
//! `familychat_security::terminal` where it is drawn (`view/*.rs`); the
//! rest is Family Chat's own text.

use std::ops::Range;

use familychat_core::identity::APP_NAME;
use familychat_security::terminal::sanitize_line;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

mod profile;
mod rooms;
mod signin;
pub(crate) mod timeline;

use crate::{
    app::{App, ProfileScreen, Prompt, Screen},
    palette::Palette,
    pane::{DrawnPane, PaneId, PaneLine},
    text::{self, fit, fit_right, wrap},
};

/// Styles and characters for this terminal: colours unless `NO_COLOR`,
/// box drawing unless ASCII.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    pub(crate) color: bool,
    pub(crate) unicode: bool,
}

impl Theme {
    pub(crate) fn of(app: &App) -> Self {
        Self {
            color: app.env.color,
            unicode: app.env.unicode && !app.settings.ascii,
        }
    }

    pub(crate) fn reversed(self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    pub(crate) fn bold(self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    pub(crate) fn heading(self) -> Style {
        self.bold().add_modifier(Modifier::UNDERLINED)
    }

    fn colored(self, color: Color, without: Style) -> Style {
        if self.color {
            Style::new().fg(color)
        } else {
            without
        }
    }

    /// Secondary text: grey, or plain without colour.
    pub(crate) fn faint(self) -> Style {
        self.colored(Color::DarkGray, Style::new())
    }

    /// A link: underlined, and cyan with colour.
    pub(crate) fn link(self) -> Style {
        let style = Style::new().add_modifier(Modifier::UNDERLINED);
        if self.color {
            style.fg(Color::Cyan)
        } else {
            style
        }
    }

    /// Inline code and code blocks.
    pub(crate) fn code(self) -> Style {
        self.colored(Color::Green, Style::new())
    }

    /// A mention badge and the read marker: red, or bold.
    pub(crate) fn mention(self) -> Style {
        self.colored(Color::Red, self.bold())
            .add_modifier(Modifier::BOLD)
    }

    /// Something the person should know (not encrypted, offline): yellow,
    /// or bold.
    pub(crate) fn warning(self) -> Style {
        self.colored(Color::Yellow, self.bold())
            .add_modifier(Modifier::BOLD)
    }

    /// Something went wrong: red, or bold.
    pub(crate) fn danger(self) -> Style {
        self.mention()
    }

    /// Information, not a problem (the suspended state, a notice): cyan,
    /// or plain.
    pub(crate) fn info(self) -> Style {
        self.colored(Color::Cyan, Style::new())
    }

    pub(crate) fn vertical(self) -> &'static str {
        if self.unicode { "│" } else { "|" }
    }

    pub(crate) fn horizontal(self) -> &'static str {
        if self.unicode { "─" } else { "-" }
    }

    pub(crate) fn ellipsis(self) -> &'static str {
        if self.unicode { "…" } else { "..." }
    }

    pub(crate) fn bullet(self) -> &'static str {
        if self.unicode { "•" } else { "*" }
    }

    pub(crate) fn pointer(self) -> &'static str {
        if self.unicode { "›" } else { ">" }
    }

    pub(crate) fn hidden(self) -> char {
        if self.unicode { '▒' } else { '#' }
    }

    pub(crate) fn masked(self) -> char {
        if self.unicode { '•' } else { '*' }
    }

    /// Text of ours that may hold typographic characters, for an ASCII
    /// terminal.
    pub(crate) fn words(self, text: &str) -> String {
        if self.unicode {
            return text.to_owned();
        }
        text.replace('…', "...")
            .replace(['’', '‘'], "'")
            .replace(['“', '”'], "\"")
            .replace('·', "-")
            .replace('×', "x")
            .replace(['–', '—'], "-")
    }
}

/// Draws the whole screen and records what was drawn where.
pub fn draw(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    app.size = (area.width, area.height);
    app.panes.clear();
    app.command_targets.clear();
    let theme = Theme::of(app);
    if area.height < 6 || area.width < 20 {
        frame.buffer_mut().set_stringn(
            0,
            0,
            format!("{APP_NAME} needs a bigger terminal"),
            usize::from(area.width),
            Style::new(),
        );
        return;
    }
    let width = area.width;
    let body = Rect::new(0, 1, width, area.height - 4);
    draw_title(app, frame, theme, Rect::new(0, 0, width, 1));
    match &app.screen {
        Screen::Profile(_) => profile::draw(app, frame, theme, body),
        Screen::Signin(_) => signin::draw(app, frame, theme, body),
        Screen::Rooms(_) => rooms::draw(app, frame, theme, body),
    }
    match app.prompt.clone() {
        Some(Prompt::Palette(palette)) => {
            clear(frame, body);
            let lines = palette_lines(&palette, theme, width);
            app.reveal.insert(PaneId::Palette);
            draw_pane(
                app,
                frame,
                PaneId::Palette,
                body,
                lines,
                Some(palette.cursor + 3),
            );
        }
        Some(Prompt::Link(prompt)) => rooms::draw_link_dialog(app, frame, theme, body, &prompt),
        _ => {}
    }
    draw_status(app, frame, theme, Rect::new(0, area.height - 3, width, 1));
    draw_commands(app, frame, theme, Rect::new(0, area.height - 2, width, 2));
}

/// The command palette over the body: each command that matches, best
/// first, with its key, its name and what it does, the letters that
/// matched in bold.
fn palette_lines(palette: &Palette, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit("  COMMANDS ON THIS SCREEN", width, theme.ellipsis()),
            theme.heading(),
        )),
        PaneLine::new(Span::styled(
            fit(
                "  Type to narrow the list. Enter runs the one under the cursor, Esc closes.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::default(),
    ];
    if palette.shown.is_empty() {
        lines.push(PaneLine::new(Span::raw(fit(
            &format!("  Nothing matches \"{}\"", palette.query),
            width,
            theme.ellipsis(),
        ))));
        return lines;
    }
    let name_width = palette
        .items
        .iter()
        .map(|item| item.name.len())
        .max()
        .unwrap_or(0);
    for (row, (index, positions)) in palette.shown.iter().enumerate() {
        let item = &palette.items[*index];
        let name = item.name;
        let prefix = format!("    {:<6} ", item.entry.key);
        let line = format!("{prefix}{name:<name_width$}  {}", item.description);
        // Matched offsets are in "name description".
        let description_start = prefix.len() + name_width + 2;
        let marks: Vec<(Range<usize>, Style)> = positions
            .iter()
            .map(|&offset| {
                let at = if offset < name.len() {
                    prefix.len() + offset
                } else {
                    description_start + offset - name.len() - 1
                };
                let end = at + line[at..].chars().next().map_or(1, char::len_utf8);
                (at..end, theme.bold().add_modifier(Modifier::UNDERLINED))
            })
            .collect();
        let base = if row == palette.cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        let line = fit(&line, width, theme.ellipsis());
        lines.push(PaneLine {
            spans: styled(&line, base, &marks),
            target: Some(row),
            ..PaneLine::default()
        });
    }
    lines
}

/// Blanks `area`, for what is drawn over the body.
pub(crate) fn clear(frame: &mut Frame, area: Rect) {
    let buffer = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buffer.cell_mut(Position::new(x, y)) {
                cell.reset();
            }
        }
    }
}

/// Draws `lines` in `area` from the pane's scroll position, keeping
/// `cursor` in view when it moved, and the selection in reverse video.
pub(crate) fn draw_pane(
    app: &mut App,
    frame: &mut Frame,
    id: PaneId,
    area: Rect,
    lines: Vec<PaneLine>,
    cursor: Option<usize>,
) {
    let height = usize::from(area.height);
    let mut scroll = app.scroll.get(&id).copied().unwrap_or(0);
    if let Some(cursor) = cursor
        && app.reveal.remove(&id)
    {
        if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + height {
            scroll = cursor + 1 - height;
        }
    }
    scroll = scroll.min(lines.len().saturating_sub(height));
    app.scroll.insert(id, scroll);
    let buffer = frame.buffer_mut();
    for (row, line) in lines.iter().skip(scroll).take(height).enumerate() {
        buffer.set_line(
            area.x,
            area.y + row as u16,
            &Line::from(line.spans.clone()),
            area.width,
        );
    }
    if let Some(selection) = app.selection.filter(|selection| selection.pane == id) {
        for row in 0..height.min(lines.len().saturating_sub(scroll)) {
            for column in 0..usize::from(area.width) {
                if selection.contains(scroll + row, column)
                    && let Some(cell) =
                        buffer.cell_mut(Position::new(area.x + column as u16, area.y + row as u16))
                {
                    cell.modifier.toggle(Modifier::REVERSED);
                }
            }
        }
    }
    app.panes.push(DrawnPane {
        id,
        area,
        scroll,
        lines,
    });
}

fn draw_title(app: &App, frame: &mut Frame, theme: Theme, area: Rect) {
    let width = usize::from(area.width);
    let buffer = frame.buffer_mut();
    buffer.set_style(area, theme.reversed());
    let left = format!("  {} {}", APP_NAME.to_uppercase(), app.version);
    let name = match &app.screen {
        Screen::Profile(ProfileScreen::Opening) => "OPENING",
        Screen::Profile(ProfileScreen::Keychain(_)) => "KEYCHAIN",
        Screen::Profile(ProfileScreen::LostKey(_)) => "CAN'T OPEN YOUR DATA",
        Screen::Profile(ProfileScreen::Failed(_)) => "CAN'T OPEN YOUR DATA",
        Screen::Signin(signin) => signin::title(signin),
        Screen::Rooms(rooms) if rooms.room_focused() && !app.side_by_side() => "ROOM",
        Screen::Rooms(_) => "ROOMS",
    };
    let detail = match &app.screen {
        Screen::Rooms(rooms) => {
            let position = if rooms.rooms.is_empty() {
                String::new()
            } else {
                format!("  Room {} of {}", rooms.cursor + 1, rooms.rooms.len())
            };
            format!(
                "{}{position}",
                sanitize_line(rooms.account.user_id.as_str())
            )
        }
        _ => String::new(),
    };
    let right = fit_right(
        &detail,
        width.saturating_sub(text::width(&left) + 2),
        theme.ellipsis(),
    );
    buffer.set_stringn(area.x, area.y, &left, width, theme.reversed());
    let center = (width.saturating_sub(name.len())) / 2;
    if center > text::width(&left) + 1
        && center + name.len() + text::width(right.trim_start()) + 3 < width
    {
        buffer.set_string(area.x + center as u16, area.y, name, theme.reversed());
    }
    let right = format!("{} ", right.trim_start());
    let x = width.saturating_sub(text::width(&right));
    buffer.set_stringn(
        area.x + x as u16,
        area.y,
        &right,
        width - x,
        theme.reversed(),
    );
}

fn draw_status(app: &App, frame: &mut Frame, theme: Theme, area: Rect) {
    let width = usize::from(area.width);
    if let Some((prompt, field)) = app.prompt_text() {
        let prompt = theme.words(&prompt);
        // A field shows its end, where the typing is.
        let shown = if field {
            text::truncate_start(&prompt, width.saturating_sub(1), theme.ellipsis())
        } else {
            text::truncate(&prompt, width.saturating_sub(1), theme.ellipsis())
        };
        frame
            .buffer_mut()
            .set_stringn(area.x, area.y, &shown, width, theme.bold());
        if field {
            let x = text::width(&shown).min(width.saturating_sub(1));
            frame.set_cursor_position(Position::new(area.x + x as u16, area.y));
        }
        return;
    }
    let (note, note_style) = rooms::sync_note(app, theme).unwrap_or_default();
    let note = theme.words(&note);
    let note_width = text::width(&note);
    if let Some(status) = &app.status {
        let room = width.saturating_sub(if note.is_empty() { 0 } else { note_width + 2 });
        let shown = text::truncate(
            &format!("[{}]", theme.words(status)),
            room,
            theme.ellipsis(),
        );
        let x = (room.saturating_sub(text::width(&shown))) / 2;
        frame
            .buffer_mut()
            .set_stringn(area.x + x as u16, area.y, &shown, room, theme.reversed());
    }
    if !note.is_empty() && note_width < width {
        frame.buffer_mut().set_stringn(
            area.x + (width - note_width) as u16,
            area.y,
            &note,
            note_width,
            note_style,
        );
    }
}

fn draw_commands(app: &mut App, frame: &mut Frame, theme: Theme, area: Rect) {
    let entries = app.command_bar(area.width);
    let columns = usize::from(area.width / 13).clamp(2, 8);
    let slot = usize::from(area.width) / columns;
    for (index, entry) in entries.iter().enumerate() {
        let x = area.x + ((index / 2) * slot) as u16;
        let y = area.y + (index % 2) as u16;
        let buffer = frame.buffer_mut();
        let key = entry.key.clone();
        buffer.set_stringn(x, y, &key, slot, theme.reversed());
        // A space after the label too, so the next key stands apart.
        let label = fit(
            entry.label,
            slot.saturating_sub(key.len() + 2),
            theme.ellipsis(),
        );
        buffer.set_stringn(x + key.len() as u16 + 1, y, &label, slot, Style::new());
        app.command_targets
            .push((Rect::new(x, y, slot as u16, 1), entry.cmd));
    }
}

/// `text` as spans: `base`, with each mark's style laid over its byte
/// range.
pub(crate) fn styled(
    text: &str,
    base: Style,
    marks: &[(Range<usize>, Style)],
) -> Vec<Span<'static>> {
    let mut cuts: Vec<usize> = marks
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .chain([0, text.len()])
        .filter(|&cut| cut <= text.len() && text.is_char_boundary(cut))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter(|pair| pair[0] < pair[1])
        .map(|pair| {
            let style = marks
                .iter()
                .filter(|(range, _)| range.start <= pair[0] && pair[1] <= range.end)
                .fold(base, |style, (_, mark)| style.patch(*mark));
            Span::styled(text[pair[0]..pair[1]].to_owned(), style)
        })
        .collect()
}

/// `text` wrapped to `width` cells after `indent` spaces, in `style`, as
/// pane lines that copy as the text they came from.
pub(crate) fn paragraph(text: &str, style: Style, indent: usize, width: usize) -> Vec<PaneLine> {
    let pad = " ".repeat(indent);
    wrap(text, width.saturating_sub(indent).max(1))
        .into_iter()
        .map(|row| PaneLine {
            spans: vec![Span::raw(pad.clone()), Span::styled(row.text, style)],
            joiner: row.joiner,
            prefix: indent,
            ..PaneLine::default()
        })
        .collect()
}

/// A screen of our own text, centred: `lines` start `indent` cells in and
/// below as many blank lines as centre them in `height`.
pub(crate) fn centred(mut lines: Vec<PaneLine>, height: usize) -> Vec<PaneLine> {
    let top = height.saturating_sub(lines.len()) / 2;
    let mut out = vec![PaneLine::default(); top];
    out.append(&mut lines);
    out
}
