//! The signed-in screens: the sync banner, the room list (filters, unread
//! and mention badges, invitations), the open room (its header with the
//! encryption state, the not-encrypted banner, the timeline, the members)
//! and the link question with the whole address.

use familychat_core::{
    links::LinkPrompt,
    rooms::{Encryption, Membership, RoomListFilter, RoomListLoading, RoomSummary},
    sync::SyncState,
};
use familychat_security::terminal::sanitize_line;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
};

use super::{
    Theme,
    composer::{self, ComposerParts},
    draw_pane, encryption, paragraph, timeline,
};
use crate::{
    app::{App, Screen},
    pane::{PaneId, PaneLine},
    rooms::{Focus, MembersState, RoomsState, paginate_if_at_top},
    text::{self, fit, fit_right, truncate},
};

/// The room list's width beside the room.
fn list_width(width: u16) -> u16 {
    (width / 3).clamp(30, 44)
}

/// The members' width beside the timeline.
const MEMBERS_WIDTH: u16 = 26;
/// The narrowest room pane with the members beside the timeline.
const MEMBERS_BESIDE_FROM: u16 = 90;

/// The status line's right side: the sync state's label, in its tone.
pub(super) fn sync_note(app: &App, theme: Theme) -> Option<(String, Style)> {
    let rooms = app.rooms_ref()?;
    let sync = &rooms.status.sync;
    let style = match sync {
        SyncState::Idle | SyncState::Connecting | SyncState::Running => theme.faint(),
        SyncState::Suspended { .. } => theme.info(),
        SyncState::Offline { .. } => theme.warning(),
        SyncState::NotSupported | SyncState::SignedOut { .. } | SyncState::Error { .. } => {
            theme.danger()
        }
    };
    let bell = if app.settings.bell { "Bell on  " } else { "" };
    Some((format!("{bell}{} ", sync.label()), style))
}

pub(super) fn draw(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let side_by_side = app.side_by_side();
    let clock = app.clock;
    let Screen::Rooms(rooms) = &mut app.screen else {
        return;
    };
    let width = body.width;
    // The sync banner: the state and its sentence, above everything.
    let mut banner = Vec::new();
    if let Some(message) = rooms.status.sync.message() {
        let style = match rooms.status.sync {
            SyncState::Suspended { .. } => theme.info(),
            SyncState::Offline { .. } => theme.warning(),
            _ => theme.danger(),
        };
        let retry = if rooms.can_retry() {
            " Press t to try again."
        } else {
            ""
        };
        banner.extend(paragraph(
            &theme.words(&format!(
                "{}: {}{retry}",
                rooms.status.sync.label(),
                sanitize_line(&message)
            )),
            style.add_modifier(Modifier::BOLD),
            1,
            usize::from(width),
        ));
    }
    if let Some(problem) = &rooms.status.problem {
        banner.extend(paragraph(
            &theme.words(&problem.message()),
            theme.warning(),
            1,
            usize::from(width),
        ));
    }
    let takes_over = rooms.encryption.takes_over();
    if !takes_over && let Some((sentence, caution)) = rooms.encryption.banner() {
        banner.extend(paragraph(
            &theme.words(&sentence),
            if caution {
                theme.warning()
            } else {
                theme.info()
            },
            1,
            usize::from(width),
        ));
    }
    let banner_height = (banner.len() as u16).min(body.height / 3);
    let buffer = frame.buffer_mut();
    for (row, line) in banner.iter().take(usize::from(banner_height)).enumerate() {
        buffer.set_line(
            body.x,
            body.y + row as u16,
            &ratatui::text::Line::from(line.spans.clone()),
            body.width,
        );
    }
    let area = Rect::new(
        body.x,
        body.y + banner_height,
        body.width,
        body.height - banner_height,
    );
    if takes_over {
        encryption::draw_screen(app, frame, theme, area);
        return;
    }
    let show_room = rooms.open.is_some() && (side_by_side || rooms.focus != Focus::List);
    let show_list = side_by_side || !show_room;
    let (list_area, room_area) = if side_by_side {
        let left = list_width(width);
        for y in area.y..area.y + area.height {
            frame
                .buffer_mut()
                .set_string(left, y, theme.vertical(), theme.faint());
        }
        (
            Rect::new(area.x, area.y, left, area.height),
            Rect::new(
                left + 2,
                area.y,
                width.saturating_sub(left + 2),
                area.height,
            ),
        )
    } else {
        (area, area)
    };
    rooms.list_height = usize::from(list_area.height);
    let list_lines = if show_list {
        Some(list_lines(rooms, theme, list_area.width, clock))
    } else {
        None
    };
    let cursor_line = 2 + rooms.cursor * 2 + 1;
    let room = show_room.then(|| room_lines(rooms, theme, room_area, clock));
    if let Some(lines) = list_lines {
        draw_pane(
            app,
            frame,
            PaneId::Rooms,
            list_area,
            lines,
            Some(cursor_line),
        );
    }
    if let Some(parts) = room {
        draw_room(app, frame, parts);
    }
}

/// The room list: the filters, then two lines a room.
fn list_lines(
    rooms: &RoomsState,
    theme: Theme,
    width: u16,
    clock: crate::app::Clock,
) -> Vec<PaneLine> {
    let width = usize::from(width);
    let mut tabs = vec![Span::raw(" ")];
    for (index, filter) in RoomListFilter::ALL.iter().enumerate() {
        let style = if *filter == rooms.filter {
            theme.reversed().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        tabs.push(Span::styled(format!("{} ", index + 1), theme.faint()));
        tabs.push(Span::styled(filter.label().to_owned(), style));
        tabs.push(Span::raw("  "));
    }
    let mut lines = vec![
        PaneLine {
            spans: tabs,
            ..PaneLine::default()
        },
        PaneLine::new(Span::styled(
            theme.horizontal().repeat(width),
            theme.faint(),
        )),
    ];
    if let Some(error) = &rooms.list_error {
        lines.push(PaneLine::new(Span::styled(
            " Your rooms couldn't be loaded".to_owned(),
            theme.bold(),
        )));
        lines.extend(paragraph(error, theme.danger(), 1, width));
        return lines;
    }
    if rooms.rooms.is_empty() {
        if rooms.loading == RoomListLoading::NotLoaded {
            lines.push(PaneLine::new(Span::styled(
                theme.words(" Loading your rooms…"),
                theme.faint(),
            )));
        } else {
            lines.push(PaneLine::new(Span::styled(
                format!(" {}", rooms.filter.empty_title()),
                theme.bold(),
            )));
            lines.extend(paragraph(
                rooms.filter.empty_detail(),
                theme.faint(),
                1,
                width,
            ));
        }
        return lines;
    }
    let now = (clock.now_ms)();
    let list_focused = rooms.focus == Focus::List;
    for (index, room) in rooms.rooms.iter().enumerate() {
        let selected = index == rooms.cursor;
        let (first, second) = room_row(room, theme, width, now, clock, selected);
        let highlight = if selected && list_focused {
            theme.reversed()
        } else if selected {
            theme.bold()
        } else {
            Style::new()
        };
        for spans in [first, second] {
            lines.push(PaneLine {
                spans: spans
                    .into_iter()
                    .map(|span| {
                        let style = span.style.patch(highlight);
                        Span::styled(span.content, style)
                    })
                    .collect(),
                target: Some(index),
                ..PaneLine::default()
            });
        }
    }
    lines
}

/// A room's two lines: its name with the badges and the time, then the
/// latest message or the invitation.
fn room_row(
    room: &RoomSummary,
    theme: Theme,
    width: usize,
    now: u64,
    clock: crate::app::Clock,
    selected: bool,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let pointer = if selected { theme.pointer() } else { " " };
    let unread = room.unread;
    let mut badges: Vec<(String, Style)> = Vec::new();
    if room.is_invite() {
        badges.push(("invited".to_owned(), theme.info()));
    } else {
        if unread.mentions > 0 {
            badges.push((format!("@{}", unread.mentions), theme.mention()));
        }
        if unread.notifications > 0 {
            badges.push((unread.notifications.to_string(), theme.bold()));
        } else if unread.marked_unread {
            badges.push((theme.bullet().to_owned(), theme.bold()));
        }
    }
    let when = room
        .last_activity_ms
        .map(|at| {
            let day = (clock.day)(at, now);
            if day == "Today" {
                (clock.time)(at)
            } else {
                day
            }
        })
        .unwrap_or_default();
    let right_width: usize = badges
        .iter()
        .map(|(text, _)| text::width(text) + 1)
        .sum::<usize>()
        + text::width(&when);
    let name = sanitize_line(&room.name);
    let name_room = width.saturating_sub(right_width + 4);
    let name_style = if unread.is_unread() || room.is_invite() {
        theme.bold()
    } else {
        Style::new()
    };
    let mut first = vec![
        Span::raw(format!("{pointer} ")),
        Span::styled(fit(&name, name_room, theme.ellipsis()), name_style),
        Span::raw(" "),
    ];
    for (text, style) in badges {
        first.push(Span::styled(text, style));
        first.push(Span::raw(" "));
    }
    first.push(Span::styled(when, theme.faint()));
    let detail = if room.membership == Membership::Invited {
        match &room.invited_by {
            Some(inviter) => format!("Invitation from {}", sanitize_line(inviter)),
            None => "You're invited".to_owned(),
        }
    } else {
        match &room.preview {
            Some(preview) => {
                let who = if preview.is_own {
                    "You".to_owned()
                } else {
                    sanitize_line(&preview.sender)
                };
                format!("{who}: {}", sanitize_line(preview.text.as_str()))
            }
            None => "No messages yet".to_owned(),
        }
    };
    let second = vec![Span::styled(
        fit(&format!("    {detail}"), width, theme.ellipsis()),
        theme.faint(),
    )];
    (first, second)
}

/// What the room pane draws, worked out before the panes are.
struct RoomParts {
    header: Vec<PaneLine>,
    header_area: Rect,
    timeline: Vec<PaneLine>,
    timeline_area: Rect,
    timeline_top: Option<usize>,
    members: Option<(Vec<PaneLine>, Rect)>,
    separator: Option<(u16, Rect)>,
    composer: Option<ComposerParts>,
}

fn room_lines(
    rooms: &mut RoomsState,
    theme: Theme,
    area: Rect,
    clock: crate::app::Clock,
) -> RoomParts {
    let members_shown = rooms.members_shown;
    let composer_parts = composer::parts(rooms, theme, area.width);
    let Some(open) = rooms.open.as_mut() else {
        return RoomParts {
            header: Vec::new(),
            header_area: area,
            timeline: Vec::new(),
            timeline_area: area,
            timeline_top: None,
            members: None,
            separator: None,
            composer: None,
        };
    };
    let width = usize::from(area.width);
    let room = open.room.clone();
    let room = &room;
    let name = sanitize_line(&room.name);
    let mut facts = Vec::new();
    if room.joined_members > 0 {
        facts.push(format!(
            "{} {}",
            room.joined_members,
            if room.joined_members == 1 {
                "member"
            } else {
                "members"
            }
        ));
    }
    let encryption_style = match room.encryption {
        Encryption::NotEncrypted => theme.warning(),
        _ => theme.faint(),
    };
    let mut header_spans = vec![Span::styled(
        truncate(&name, width / 2, theme.ellipsis()),
        theme.bold(),
    )];
    header_spans.push(Span::styled(
        format!("  {}", room.encryption.label()),
        encryption_style,
    ));
    if !facts.is_empty() {
        header_spans.push(Span::styled(
            format!("  {}", facts.join(", ")),
            theme.faint(),
        ));
    }
    let mut header = vec![PaneLine {
        spans: header_spans,
        ..PaneLine::default()
    }];
    if let Some(topic) = &room.topic {
        header.push(PaneLine::new(Span::styled(
            fit(&sanitize_line(topic), width, theme.ellipsis()),
            theme.faint(),
        )));
    }
    if room.encryption == Encryption::NotEncrypted {
        header.extend(paragraph(
            room.encryption.description(),
            theme.warning(),
            0,
            width,
        ));
    }
    for (index, warning) in open.warnings.iter().enumerate() {
        let style = match warning.kind {
            familychat_core::encryption::IdentityWarningKind::Changed => theme.warning(),
            familychat_core::encryption::IdentityWarningKind::VerificationLost => theme.danger(),
        };
        let answer = if index == 0 {
            format!(" Press w: {}.", warning.action_label())
        } else {
            String::new()
        };
        header.extend(paragraph(
            &theme.words(&format!(
                "Identity changed: {}{answer}",
                sanitize_line(&warning.message())
            )),
            style,
            0,
            width,
        ));
    }
    header.push(PaneLine::new(Span::styled(
        theme.horizontal().repeat(width),
        theme.faint(),
    )));
    let header_height = (header.len() as u16).min(area.height / 2);
    let header_area = Rect::new(area.x, area.y, area.width, header_height);
    // The composer along the bottom, at most half of what is left.
    let composer = composer_parts.map(|(lines, rows)| {
        let wanted = lines.len() as u16 + rows;
        let height = wanted.min((area.height - header_height) / 2).max(1);
        ComposerParts {
            lines,
            rows: rows.min(height.saturating_sub(1).max(1)),
            area: Rect::new(area.x, area.y + area.height - height, area.width, height),
        }
    });
    let composer_height = composer.as_ref().map_or(0, |parts| parts.area.height);
    let rest = Rect::new(
        area.x,
        area.y + header_height,
        area.width,
        area.height - header_height - composer_height,
    );
    let beside = members_shown && area.width >= MEMBERS_BESIDE_FROM;
    let (timeline_area, members_area, separator) = if beside {
        let left = area.width - MEMBERS_WIDTH - 2;
        (
            Rect::new(rest.x, rest.y, left, rest.height),
            Some(Rect::new(
                rest.x + left + 2,
                rest.y,
                MEMBERS_WIDTH,
                rest.height,
            )),
            Some((rest.x + left, rest)),
        )
    } else if members_shown {
        (Rect::new(rest.x, rest.y, rest.width, 0), Some(rest), None)
    } else {
        (rest, None, None)
    };
    let members = members_area
        .map(|members_area| (member_lines(open, theme, members_area.width), members_area));
    if room.is_invite() {
        let inviter = room
            .invited_by
            .as_deref()
            .map(sanitize_line)
            .unwrap_or_else(|| "Someone".to_owned());
        let mut lines = vec![PaneLine::default()];
        lines.extend(paragraph(
            &format!("{inviter} invited you to {name}."),
            theme.bold(),
            2,
            usize::from(timeline_area.width),
        ));
        lines.push(PaneLine::default());
        lines.extend(paragraph(
            "Press a to join it, or x to decline. Until you join, you can't read or write \
             in it.",
            theme.faint(),
            2,
            usize::from(timeline_area.width),
        ));
        return RoomParts {
            header,
            header_area,
            timeline: lines,
            timeline_area,
            timeline_top: None,
            members,
            separator,
            composer: None,
        };
    }
    if let Some(error) = &open.error {
        let mut lines = vec![PaneLine::new(Span::styled(
            "  This room couldn't be opened".to_owned(),
            theme.bold(),
        ))];
        lines.extend(paragraph(
            error,
            theme.danger(),
            2,
            usize::from(timeline_area.width),
        ));
        return RoomParts {
            header,
            header_area,
            timeline: lines,
            timeline_area,
            timeline_top: None,
            members,
            separator,
            composer: None,
        };
    }
    if !open.loaded {
        return RoomParts {
            header,
            header_area,
            timeline: vec![PaneLine::new(Span::styled(
                theme.words("  Loading messages…"),
                theme.faint(),
            ))],
            timeline_area,
            timeline_top: None,
            members,
            separator,
            composer: None,
        };
    }
    let key = (
        timeline_area.width,
        open.generation,
        open.selected_ref,
        open.spoilers,
    );
    if open.layout.as_ref().is_none_or(|layout| layout.key != key) {
        let old = open.layout.take();
        let layout = timeline::layout(open, timeline_area.width, theme, clock);
        // Reading older messages: new ones arriving at the bottom keep
        // the view where it is.
        if let Some(old) = old
            && open.from_bottom > 0
            && old.key.0 == key.0
            && old.last_item != layout.last_item
        {
            open.from_bottom += layout.lines.len().saturating_sub(old.lines.len());
        }
        open.layout = Some(layout);
    }
    rooms.timeline_height = usize::from(timeline_area.height);
    let Some(layout) = open.layout.as_ref() else {
        unreachable!("laid out above")
    };
    let height = usize::from(timeline_area.height);
    let total = layout.lines.len();
    // A message just selected with [ or ] comes into view.
    if std::mem::take(&mut open.reveal_selected)
        && let Some(line) = layout.selected_line
    {
        let top = total.saturating_sub(height + open.from_bottom);
        if line < top || line >= top + height {
            open.from_bottom = total.saturating_sub(line + height / 2 + 1);
        }
    }
    open.from_bottom = open.from_bottom.min(total.saturating_sub(height));
    let top = total.saturating_sub(height + open.from_bottom);
    let lines = layout.lines.clone();
    paginate_if_at_top(open, top);
    // A short timeline sits at the bottom of the pane, as chat does.
    let (lines, top) = if total < height {
        let mut padded = vec![PaneLine::default(); height - total];
        padded.extend(lines);
        (padded, 0)
    } else {
        (lines, top)
    };
    RoomParts {
        header,
        header_area,
        timeline: lines,
        timeline_area,
        timeline_top: Some(top),
        members,
        separator,
        composer,
    }
}

fn member_lines(open: &crate::rooms::OpenRoom, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let mut lines = Vec::new();
    match &open.members {
        MembersState::NotAsked | MembersState::Loading => lines.push(PaneLine::new(Span::styled(
            theme.words("Loading the members…"),
            theme.faint(),
        ))),
        MembersState::Failed(message) => lines.extend(paragraph(message, theme.danger(), 0, width)),
        MembersState::Ready(members) => {
            let joined: Vec<_> = members
                .iter()
                .filter(|member| member.membership == Membership::Joined)
                .collect();
            let invited: Vec<_> = members
                .iter()
                .filter(|member| member.membership == Membership::Invited)
                .collect();
            for (title, group) in [("Joined", joined), ("Invited", invited)] {
                if group.is_empty() {
                    continue;
                }
                lines.push(PaneLine::new(Span::styled(
                    format!("{title}, {}", group.len()),
                    theme.heading(),
                )));
                for member in group {
                    let mut name = sanitize_line(&member.name());
                    if member.is_own {
                        name.push_str(" (you)");
                    }
                    let role = member.role.label().unwrap_or("");
                    let room = width.saturating_sub(text::width(role) + 1);
                    lines.push(PaneLine {
                        spans: vec![
                            Span::raw(fit(&name, room, theme.ellipsis())),
                            Span::styled(fit_right(role, width - room, ""), theme.faint()),
                        ],
                        ..PaneLine::default()
                    });
                }
                lines.push(PaneLine::default());
            }
        }
    }
    lines
}

fn draw_room(app: &mut App, frame: &mut Frame, parts: RoomParts) {
    let theme = Theme::of(app);
    let buffer = frame.buffer_mut();
    for (row, line) in parts
        .header
        .iter()
        .take(usize::from(parts.header_area.height))
        .enumerate()
    {
        buffer.set_line(
            parts.header_area.x,
            parts.header_area.y + row as u16,
            &ratatui::text::Line::from(line.spans.clone()),
            parts.header_area.width,
        );
    }
    if let Some((x, area)) = parts.separator {
        for y in area.y..area.y + area.height {
            frame
                .buffer_mut()
                .set_string(x, y, theme.vertical(), theme.faint());
        }
    }
    if parts.timeline_area.height > 0 {
        if let Some(top) = parts.timeline_top {
            app.scroll.insert(PaneId::Timeline, top);
        } else {
            app.scroll.insert(PaneId::Timeline, 0);
        }
        draw_pane(
            app,
            frame,
            PaneId::Timeline,
            parts.timeline_area,
            parts.timeline,
            None,
        );
    }
    if let Some((lines, area)) = parts.members {
        draw_pane(app, frame, PaneId::Members, area, lines, None);
    }
    if let Some(composer) = parts.composer {
        composer::draw(app, frame, theme, composer);
    }
}

/// The link question over the body: the whole address, what opening it
/// does, and the look-alike warning when its text shows another site.
pub(super) fn draw_link_dialog(
    app: &mut App,
    frame: &mut Frame,
    theme: Theme,
    body: Rect,
    prompt: &LinkPrompt,
) {
    let width = body.width.min(72).saturating_sub(4);
    let inner = usize::from(width.saturating_sub(4));
    let mut lines = vec![PaneLine::new(Span::styled(
        prompt.title().to_owned(),
        theme.bold(),
    ))];
    lines.push(PaneLine::default());
    // The whole address, however long: never cut.
    lines.extend(paragraph(
        &sanitize_line(&prompt.target),
        theme.link(),
        0,
        inner,
    ));
    lines.push(PaneLine::default());
    lines.extend(paragraph(
        &sanitize_line(&prompt.description()),
        Style::new(),
        0,
        inner,
    ));
    if let Some((title, sentence)) = prompt.warning() {
        lines.push(PaneLine::default());
        lines.push(PaneLine::new(Span::styled(
            title.to_owned(),
            theme.danger(),
        )));
        lines.extend(paragraph(
            &sanitize_line(&sentence),
            theme.danger(),
            0,
            inner,
        ));
    }
    lines.push(PaneLine::default());
    lines.push(PaneLine {
        spans: vec![
            Span::styled("Y".to_owned(), theme.reversed()),
            Span::raw(format!(" {}   ", prompt.open_label())),
            Span::styled("C".to_owned(), theme.reversed()),
            Span::raw(" Copy the address   ".to_owned()),
            Span::styled("N".to_owned(), theme.reversed()),
            Span::raw(" Cancel".to_owned()),
        ],
        ..PaneLine::default()
    });
    super::draw_box(app, frame, theme, body, width, lines);
}
