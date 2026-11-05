//! The open room's timeline as terminal lines: `familychat-core`'s rows
//! (runs of room changes folded, a sender's messages grouped), its rich
//! text as terminal styles (bold, italic, underline, strike-through, code,
//! links, spoilers; quotes, lists, headings, code blocks and rules), its
//! wording for room changes, days and times, and every link and file
//! numbered so the keyboard can reach it.
//!
//! Every string from the server goes through
//! `familychat_security::terminal` here: senders, bodies (run by run, so
//! the styles stay on their text), link addresses and text, file names,
//! reactions and the names inside room changes. Laid out once per width
//! and change (the [`Layout`] cache), so a 10,000-event room costs nothing
//! while it doesn't change.

use std::ops::Range;

use familychat_core::{
    media::MediaInfo,
    rich_text::{BlockKind, RichText, SpanStyle},
    timeline::{
        self, MessageKind, PaginationState, Reaction, SendState, TimelineContent, TimelineEvent,
        TimelineItem, TimelineItemKind, TimelineRow,
    },
};
use familychat_security::terminal::{sanitize, sanitize_line};
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use super::{Theme, styled};
use crate::{
    app::Clock,
    pane::PaneLine,
    rooms::{Layout, OpenRoom, Reference},
    text::{self, fit, wrap},
};

/// Cells a message's text starts in by.
const INDENT: usize = 2;

/// Lays the open room's timeline out for `width` cells.
pub(crate) fn layout(open: &OpenRoom, width: u16, theme: Theme, clock: Clock) -> Layout {
    let mut builder = Builder {
        lines: Vec::new(),
        refs: Vec::new(),
        ref_lines: Vec::new(),
        read_marker: None,
        width: usize::from(width.max(8)),
        theme,
        selected: open.selected_ref,
        spoilers: open.spoilers,
    };
    match &open.pagination {
        PaginationState::Loading => builder.note("Loading older messages…", theme.faint()),
        PaginationState::Failed { message } => builder.note(
            &format!(
                "Older messages couldn't be loaded: {}",
                sanitize_line(message)
            ),
            theme.danger(),
        ),
        PaginationState::Idle | PaginationState::ReachedStart => {}
    }
    let now = (clock.now_ms)();
    let has_events = open
        .items
        .iter()
        .any(|item| matches!(item.kind, TimelineItemKind::Event(_)));
    for row in timeline::rows(&open.items) {
        match row {
            TimelineRow::StateGroup { range } => {
                let first = open.items[range.start..]
                    .iter()
                    .find_map(|item| match &item.kind {
                        TimelineItemKind::Event(event) => change_text(event),
                        _ => None,
                    })
                    .unwrap_or_default();
                let summary = timeline::summary_label(&first, range.len());
                builder.change(&summary);
            }
            TimelineRow::Item { index, continues } => {
                builder.item(&open.items[index], continues, &open.room.name, now, clock);
            }
        }
    }
    if !has_events && open.loaded {
        builder.note(
            "No messages yet. What people send here shows up below.",
            theme.faint(),
        );
    }
    Layout {
        key: (width, open.generation, open.selected_ref, open.spoilers),
        lines: builder.lines,
        refs: builder.refs,
        ref_lines: builder.ref_lines,
        read_marker: builder.read_marker,
        last_item: open.items.last().map(|item| item.id.0.clone()),
    }
}

/// The sentence for a room change, its names filtered.
fn change_text(event: &TimelineEvent) -> Option<String> {
    let who = sanitize_line(&event.sender_label());
    timeline::describe(&event.content, &who).map(|text| sanitize_line(&text))
}

struct Builder {
    lines: Vec<PaneLine>,
    refs: Vec<Reference>,
    ref_lines: Vec<usize>,
    read_marker: Option<usize>,
    width: usize,
    theme: Theme,
    selected: Option<usize>,
    spoilers: bool,
}

/// A run of display text with its style, and the reference it opens.
struct Piece {
    text: String,
    style: Style,
    reference: Option<usize>,
}

impl Builder {
    /// A line of our own wording.
    fn note(&mut self, text: &str, style: Style) {
        let text = self.theme.words(text);
        for row in wrap(&text, self.width.saturating_sub(INDENT).max(1)) {
            self.lines.push(PaneLine {
                spans: vec![Span::raw(" ".repeat(INDENT)), Span::styled(row.text, style)],
                joiner: row.joiner,
                prefix: INDENT,
                ..PaneLine::default()
            });
        }
    }

    /// A rule with a word in it: a day, the read marker.
    fn divider(&mut self, label: &str, style: Style, centre: bool) {
        let h = self.theme.horizontal();
        let label = format!(" {label} ");
        let room = self.width.saturating_sub(text::width(&label));
        let left = if centre { room / 2 } else { 2.min(room) };
        let line = format!("{}{label}{}", h.repeat(left), h.repeat(room - left));
        self.lines.push(PaneLine::new(Span::styled(
            fit(&line, self.width, self.theme.ellipsis()),
            style,
        )));
    }

    /// A room change, or a run of them folded into one line.
    fn change(&mut self, text: &str) {
        let marker = if self.theme.unicode { "·" } else { "-" };
        self.note(&format!("{marker} {text}"), self.theme.faint());
    }

    fn item(&mut self, item: &TimelineItem, continues: bool, room: &str, now: u64, clock: Clock) {
        let theme = self.theme;
        match &item.kind {
            TimelineItemKind::DateDivider { timestamp_ms } => {
                self.divider(&(clock.day)(*timestamp_ms, now), theme.faint(), true);
            }
            TimelineItemKind::ReadMarker => {
                self.read_marker = Some(self.lines.len());
                self.divider("New messages", theme.mention(), false);
            }
            TimelineItemKind::TimelineStart => {
                self.note(
                    &format!("This is the start of {}.", sanitize_line(room)),
                    theme.faint(),
                );
            }
            TimelineItemKind::Event(event) => {
                if event.content.is_state() {
                    let text = change_text(event).unwrap_or_default();
                    self.change(&text);
                } else {
                    self.message(event, continues, clock);
                }
            }
        }
    }

    fn message(&mut self, event: &TimelineEvent, continues: bool, clock: Clock) {
        let theme = self.theme;
        let sender = sanitize_line(&event.sender_label());
        if !continues {
            let time = (clock.time)(event.timestamp_ms);
            let sender_style = if event.is_own {
                theme.bold().patch(theme.info())
            } else {
                theme.bold()
            };
            let room = self.width.saturating_sub(text::width(&time) + 2);
            self.lines.push(PaneLine {
                spans: vec![
                    Span::styled(
                        text::truncate(&sender, room, theme.ellipsis()),
                        sender_style,
                    ),
                    Span::raw("  "),
                    Span::styled(time, theme.faint()),
                ],
                ..PaneLine::default()
            });
        }
        if let Some(reply) = &event.in_reply_to {
            let bar = if theme.unicode { "┃" } else { "|" };
            let who = reply
                .sender
                .as_deref()
                .map(sanitize_line)
                .unwrap_or_else(|| "a message".to_owned());
            let quoted = reply.text.as_ref().map_or_else(
                || theme.words("Loading the message…"),
                |text| sanitize_line(text.as_str()),
            );
            let line = format!("{}{bar} {who}: {quoted}", " ".repeat(INDENT));
            self.lines.push(PaneLine {
                prefix: INDENT,
                ..PaneLine::new(Span::styled(
                    fit(&line, self.width, theme.ellipsis()),
                    theme.faint(),
                ))
            });
        }
        let meta = match event.send_state {
            Some(SendState::Sending) => Some(theme.words("sending…")),
            Some(SendState::Failed { .. }) => Some("not sent".to_owned()),
            _ => None,
        };
        let faint_italic = theme.faint().add_modifier(Modifier::ITALIC);
        let mut edited = false;
        match &event.content {
            TimelineContent::Message {
                kind,
                body,
                rich,
                edited: was_edited,
                media,
            } => {
                edited = *was_edited;
                let base = if *kind == MessageKind::Notice {
                    theme.faint()
                } else {
                    Style::new()
                };
                if let Some(media) = media {
                    self.media(*kind, media);
                    let caption = rich.plain_text();
                    if !rich.is_empty() && caption.trim() != media.filename.trim() {
                        self.rich(rich, base, None);
                    }
                } else if rich.is_empty() {
                    let text = sanitize(body.as_str());
                    self.rich(&RichText::plain(&text), base, None);
                } else {
                    let emote = (*kind == MessageKind::Emote).then(|| format!("* {sender} "));
                    self.rich(rich, base, emote);
                }
            }
            TimelineContent::Sticker { body } => {
                let text = format!("[sticker] {}", sanitize_line(body.as_str()));
                self.note(&text, faint_italic);
            }
            other => {
                let text = timeline::placeholder(other).unwrap_or("");
                self.note(text, faint_italic);
            }
        }
        let mut marks = Vec::new();
        if edited {
            marks.push("edited".to_owned());
        }
        marks.extend(meta);
        if !marks.is_empty() {
            self.meta(&format!("({})", marks.join(", ")));
        }
        if !event.reactions.is_empty() {
            self.reactions(&event.reactions);
        }
    }

    /// "(edited)" after the last line, or on its own when it doesn't fit.
    fn meta(&mut self, text: &str) {
        let style = self.theme.faint();
        if let Some(last) = self.lines.last_mut()
            && text::width(&last.text()) + text::width(text) < self.width
            && last.prefix >= INDENT
        {
            last.spans.push(Span::styled(format!(" {text}"), style));
            return;
        }
        self.note(text, style);
    }

    fn reactions(&mut self, reactions: &[Reaction]) {
        let theme = self.theme;
        let mut spans = vec![Span::raw(" ".repeat(INDENT))];
        let mut used = INDENT;
        for reaction in reactions {
            let label = format!("{} {}", sanitize_line(&reaction.key), reaction.count);
            if used + text::width(&label) + 2 > self.width {
                break;
            }
            used += text::width(&label) + 2;
            let style = if reaction.own {
                theme.reversed()
            } else {
                theme.faint()
            };
            spans.push(Span::styled(label, style));
            spans.push(Span::raw("  "));
        }
        self.lines.push(PaneLine {
            spans,
            prefix: INDENT,
            ..PaneLine::default()
        });
    }

    /// A reference: its number, and the style its text is drawn in.
    fn reference(&mut self, reference: Reference) -> (usize, Style) {
        let number = self.refs.len();
        self.refs.push(reference);
        self.ref_lines.push(self.lines.len());
        let mut style = self.theme.link();
        if self.selected == Some(number) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        (number, style)
    }

    /// An image or a file: what it is, its name, size and number.
    fn media(&mut self, kind: MessageKind, media: &MediaInfo) {
        let theme = self.theme;
        let what = match kind {
            MessageKind::Image => "image",
            MessageKind::Video => "video",
            MessageKind::Audio => "audio",
            _ => "file",
        };
        let name = sanitize_line(&media.filename);
        let mut details = Vec::new();
        if let Some((width, height)) = media.declared_size()
            && matches!(kind, MessageKind::Image | MessageKind::Video)
        {
            details.push(theme.words(&format!("{width}×{height}")));
        }
        details.extend(media.size_label());
        let (number, style) = self.reference(Reference::File(media.clone()));
        let pieces = vec![
            Piece {
                text: format!("[{what}] "),
                style: theme.faint(),
                reference: None,
            },
            Piece {
                text: name,
                style,
                reference: Some(number),
            },
            Piece {
                text: format!(" [{}]", number + 1),
                style,
                reference: Some(number),
            },
            Piece {
                text: if details.is_empty() {
                    String::new()
                } else {
                    format!("  {}", details.join(", "))
                },
                style: theme.faint(),
                reference: None,
            },
        ];
        self.pieces(&pieces, " ".repeat(INDENT), None, Style::new());
    }

    /// A message's rich text, block by block.
    fn rich(&mut self, rich: &RichText, base: Style, mut lead: Option<String>) {
        let theme = self.theme;
        // Each link's reference, made when it is first drawn.
        let mut numbers: Vec<Option<usize>> = vec![None; rich.links.len()];
        for block in &rich.blocks {
            let quote: String = (0..block.quote_depth)
                .map(|_| if theme.unicode { "│ " } else { "| " })
                .collect();
            let indent = format!("{}{quote}", " ".repeat(INDENT));
            let (marker, block_style) = match block.kind {
                BlockKind::Rule => {
                    let room = self.width.saturating_sub(text::width(&indent));
                    self.lines.push(PaneLine {
                        spans: vec![
                            Span::styled(indent.clone(), theme.faint()),
                            Span::styled(theme.horizontal().repeat(room), theme.faint()),
                        ],
                        prefix: text::width(&indent),
                        ..PaneLine::default()
                    });
                    continue;
                }
                BlockKind::Code => {
                    let text = sanitize(&block.text);
                    for line in text.split('\n') {
                        let pieces = [Piece {
                            text: line.to_owned(),
                            style: theme.code(),
                            reference: None,
                        }];
                        self.pieces(&pieces, format!("{indent}  "), None, base);
                    }
                    continue;
                }
                BlockKind::Heading(level) => {
                    let mut style = theme.bold();
                    if level <= 2 {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    (None, style)
                }
                BlockKind::ListItem { depth, marker } => {
                    let label = match marker {
                        familychat_core::rich_text::ListMarker::Bullet => theme.bullet().to_owned(),
                        other => other.label(),
                    };
                    let nesting = "  ".repeat(usize::from(depth.saturating_sub(1)));
                    (Some(format!("{nesting}{label} ")), Style::new())
                }
                BlockKind::Paragraph => (None, Style::new()),
            };
            let mut pieces = Vec::new();
            if let Some(lead) = lead.take() {
                pieces.push(Piece {
                    text: lead,
                    style: base.patch(theme.bold()),
                    reference: None,
                });
            }
            let runs = block.runs();
            for (index, (range, style)) in runs.iter().enumerate() {
                let raw = &block.text[range.clone()];
                let mut text = sanitize(raw);
                if style.spoiler && !self.spoilers {
                    text = text
                        .chars()
                        .map(|ch| if ch == '\n' { ch } else { theme.hidden() })
                        .collect();
                }
                if style.code {
                    text = format!("`{text}`");
                }
                let mut look = base.patch(block_style).patch(span_style(*style, theme));
                let mut reference = None;
                if let Some(link) = style.link
                    && let Some(href) = rich.link(link)
                {
                    let number = match numbers.get(link).copied().flatten() {
                        Some(number) => number,
                        None => {
                            let (number, _) = self.reference(Reference::Link {
                                href: href.href.clone(),
                                text: sanitize_line(&link_text(block, &runs, link)),
                            });
                            if let Some(slot) = numbers.get_mut(link) {
                                *slot = Some(number);
                            }
                            number
                        }
                    };
                    look = look.patch(self.theme.link());
                    if self.selected == Some(number) {
                        look = look.add_modifier(Modifier::REVERSED);
                    }
                    reference = Some(number);
                }
                pieces.push(Piece {
                    text,
                    style: look,
                    reference,
                });
                // The link's number after its last run.
                if let Some(number) = reference {
                    let next_link = runs.get(index + 1).and_then(|(_, next)| next.link);
                    if next_link != style.link {
                        pieces.push(Piece {
                            text: format!("[{}]", number + 1),
                            style: look,
                            reference: Some(number),
                        });
                    }
                }
            }
            self.pieces(&pieces, indent, marker, base);
        }
    }

    /// Wraps `pieces` after `indent` (and `marker` on the first row, its
    /// width in spaces after), as lines whose cells keep the pieces'
    /// styles and references.
    fn pieces(&mut self, pieces: &[Piece], indent: String, marker: Option<String>, base: Style) {
        let theme = self.theme;
        let mut display = String::new();
        let mut marks: Vec<(Range<usize>, Style)> = Vec::new();
        let mut links: Vec<(Range<usize>, usize)> = Vec::new();
        for piece in pieces {
            let start = display.len();
            display.push_str(&piece.text);
            let range = start..display.len();
            marks.push((range.clone(), piece.style));
            if let Some(reference) = piece.reference {
                links.push((range, reference));
            }
        }
        let marker_width = marker.as_deref().map_or(0, text::width);
        let prefix = text::width(&indent) + marker_width;
        let room = self.width.saturating_sub(prefix).max(1);
        for (row_index, row) in wrap(&display, room).into_iter().enumerate() {
            let row_range = row.start..row.start + row.text.len();
            let clip = |range: &Range<usize>| -> Option<Range<usize>> {
                let start = range.start.max(row_range.start);
                let end = range.end.min(row_range.end);
                (start < end).then(|| start - row.start..end - row.start)
            };
            let row_marks: Vec<(Range<usize>, Style)> = marks
                .iter()
                .filter_map(|(range, style)| clip(range).map(|range| (range, *style)))
                .collect();
            let mut spans = vec![Span::styled(indent.clone(), theme.faint())];
            match (&marker, row_index) {
                (Some(marker), 0) => spans.push(Span::styled(marker.clone(), base)),
                (Some(_), _) => spans.push(Span::raw(" ".repeat(marker_width))),
                (None, _) => {}
            }
            spans.extend(styled(&row.text, base, &row_marks));
            let cells = links
                .iter()
                .filter_map(|(range, reference)| {
                    let local = clip(range)?;
                    Some((
                        prefix + text::column_of(&row.text, local.start)
                            ..prefix + text::column_of(&row.text, local.end),
                        *reference,
                    ))
                })
                .collect();
            self.lines.push(PaneLine {
                spans,
                joiner: row.joiner,
                target: None,
                links: cells,
                prefix,
            });
        }
    }
}

/// A link's text: all the runs that point at it.
fn link_text(
    block: &familychat_core::rich_text::Block,
    runs: &[(Range<usize>, SpanStyle)],
    link: usize,
) -> String {
    runs.iter()
        .filter(|(_, style)| style.link == Some(link))
        .map(|(range, _)| &block.text[range.clone()])
        .collect()
}

/// A span's style as terminal attributes.
fn span_style(style: SpanStyle, theme: Theme) -> Style {
    let mut out = Style::new();
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.italic {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if style.strikethrough {
        out = out.add_modifier(Modifier::CROSSED_OUT);
    }
    if style.code {
        out = out.patch(theme.code());
    }
    if style.spoiler {
        out = out.patch(theme.faint());
    }
    out
}
