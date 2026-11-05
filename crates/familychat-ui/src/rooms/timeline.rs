//! `TimelineView`: one room's timeline (PLAN.md, section 13.6; ADR 0007).
//!
//! The view keeps its own copy of the room's items, kept in step by the
//! session's `ListDiff` batches (several batches waiting are applied
//! before one redraw), and draws them through GPUI's `list`: virtualised
//! (only the rows on screen are laid out) and bottom-aligned, following
//! new messages while the reader is at the end. `familychat-core`'s
//! `rows` folds runs of room changes into one summary row and groups a
//! sender's consecutive messages; only the rows a batch touched are
//! measured again (`row_fingerprint` and `changed_rows`), so a reaction
//! arriving in a room of ten thousand messages lays out one row.
//!
//! Scrolling near the top asks for older messages. Images come from the
//! account's [`ImageCache`]; links and saves go to the room view as
//! [`TimelineViewEvent`]s, which opens Matrix links in the app and asks
//! before any other.
//!
//! Each message has its actions (Phase 7, ADR 0012): hovering it, or
//! selecting it from the keyboard (`SelectPreviousMessage`,
//! `SelectNextMessage`), shows a small bar with React (a quick picker of
//! six reactions), Reply, and for the account's own messages Edit and
//! Delete; a reaction under a message adds or takes back the account's
//! own. The view only reports what was asked for; the room view does it
//! through `familychat-actions` (and asks before deleting).

use std::{
    collections::HashSet,
    rc::Rc,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use familychat_core::{
    OwnedEventId,
    diff::apply_all,
    encryption::wording,
    media::{MediaError, MediaInfo},
    messaging::MessageRef,
    rich_text::RichText,
    rooms::{Encryption, RoomSummary},
    session::{AccountKey, SessionApi, TimelineControls},
    timeline::{
        MessageKind, PaginationState, Reaction, ReplyPreview, SendState, TimelineContent,
        TimelineEvent, TimelineItem, TimelineItemId, TimelineItemKind, TimelineRow, TimelineUpdate,
        UtdReason, changed_rows, day_label, describe, placeholder, row_fingerprint, rows,
        summary_label, time_label,
    },
};
use gpui::{
    AnyElement, Context, ElementId, Entity, EventEmitter, FollowMode, FontWeight, ImageSource,
    IntoElement, ListAlignment, ListState, ObjectFit, Render, Role, SharedString, Subscription,
    Task, Window, div, img, list, prelude::*, px, rgb,
};

use super::media::{ImageCache, ImageState};
use super::rich_text::{TextClick, TextTone, render_rich_text};
use crate::a11y::{AccessibleExt, readable};
use crate::components::avatar::avatar_color;
use crate::components::tooltip::Tooltip;
use crate::components::{Avatar, Button, ButtonSize, ComponentTheme, HitTargetExt, IconButton};
use crate::signin::spinner;
use crate::styles::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE, TypeStyleExt, UiColors,
    contrast::mix,
};
use crate::widgets::{empty_state, icon};

/// How many older events one load asks for.
pub(crate) const PAGE_EVENTS: u16 = 30;
/// Rows from the top at which scrolling asks for older messages.
const LOAD_OLDER_WITHIN: usize = 4;
/// The avatar beside a sender's first message.
const AVATAR: f32 = 36.0;
/// The widest and tallest an image is drawn in the timeline.
const IMAGE_MAX_WIDTH: f32 = 380.0;
const IMAGE_MAX_HEIGHT: f32 = 300.0;
/// Laid out beyond the viewport, so scrolling has rows ready.
const OVERDRAW: f32 = 600.0;

/// What the timeline asks its room view to do.
#[derive(Debug, Clone)]
pub(crate) enum TimelineViewEvent {
    /// A link in a message was clicked: `href` and the link's text.
    Link {
        /// Where it goes.
        href: String,
        /// What it says.
        text: String,
    },
    /// Save a message's file.
    Save(MediaInfo),
    /// Reply to a message.
    Reply(MessageChoice),
    /// Edit one of the account's messages.
    Edit(MessageChoice),
    /// Add or take back the account's reaction `key`.
    React {
        /// The message.
        target: MessageRef,
        /// The reaction.
        key: String,
    },
    /// Delete one of the account's messages (the room view asks first).
    Delete(MessageChoice),
    /// The newest message is on screen: the room is read.
    ReadToEnd,
    /// Send one of the account's messages the encryption stopped, anyway
    /// (ADR 0009).
    SendAnyway(MessageRef),
}

/// A message an action is about, with what its previews show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageChoice {
    /// The message.
    pub(crate) target: MessageRef,
    /// Who sent it.
    pub(crate) sender: String,
    /// Its text (or what it is).
    pub(crate) text: String,
}

/// The reactions the quick picker offers.
pub(crate) const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🎉"];

/// What a message's actions may be: it is a message (not a room change),
/// and whether it is the account's own and can be edited.
fn actions_for(event: &TimelineEvent) -> Option<(bool, bool)> {
    let editable = matches!(
        &event.content,
        TimelineContent::Message {
            kind: MessageKind::Text | MessageKind::Emote | MessageKind::Notice,
            ..
        }
    ) && event.is_own
        && event.event_id.is_some();
    match &event.content {
        TimelineContent::Message { .. } | TimelineContent::Sticker { .. } => {
            Some((event.is_own, editable))
        }
        _ => None,
    }
}

/// The text of a message, for previews and for editing.
fn message_text(event: &TimelineEvent) -> String {
    match &event.content {
        TimelineContent::Message { body, .. } | TimelineContent::Sticker { body } => {
            body.as_str().to_owned()
        }
        _ => placeholder(&event.content).unwrap_or_default().to_owned(),
    }
}

/// The timeline of one room.
pub(crate) struct TimelineView {
    room: RoomSummary,
    items: Vec<TimelineItem>,
    rows: Vec<TimelineRow>,
    fingerprints: Vec<u64>,
    list: ListState,
    controls: Option<TimelineControls>,
    pagination: PaginationState,
    loaded: bool,
    error: Option<String>,
    /// Folded runs of room changes the reader opened, by their first item.
    expanded: HashSet<TimelineItemId>,
    /// Every folded run is open (the screenshot of the room changes).
    expand_all: bool,
    /// Messages whose spoilers the reader showed.
    revealed: HashSet<TimelineItemId>,
    images: Entity<ImageCache>,
    /// An event to show once it is loaded (a `matrix:` link to it).
    pending_event: Option<OwnedEventId>,
    /// The message selected from the keyboard, whose actions show.
    selected: Option<TimelineItemId>,
    /// The message whose quick reaction picker is open.
    picker: Option<TimelineItemId>,
    _task: Task<()>,
    _images: Subscription,
    /// Rows laid out since the last look (tests and the benchmark).
    #[cfg(test)]
    rendered_rows: usize,
}

impl EventEmitter<TimelineViewEvent> for TimelineView {}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

impl TimelineView {
    /// The timeline of `room`, opened through `session`.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountKey,
        room: RoomSummary,
        images: Entity<ImageCache>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let this = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            if event.visible_range.start <= LOAD_OLDER_WITHIN && event.count > 0 {
                let _ = this.update(cx, |this, cx| this.load_older(cx));
            }
        });
        let reply = session.timeline(&account, &room.room_id);
        let task = cx.spawn_in(window, async move |this, cx| {
            let subscription = match reply.await {
                Ok(Ok(subscription)) => subscription,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| this.failed(error.to_string(), cx));
                    return;
                }
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.failed("Family Chat is closing.".to_owned(), cx);
                    });
                    return;
                }
            };
            let (mut updates, controls) = subscription.split();
            if this
                .update(cx, |this, _| this.controls = Some(controls))
                .is_err()
            {
                return;
            }
            while let Some(first) = updates.recv().await {
                // Everything waiting is drawn in one frame.
                let mut batch = vec![first];
                while let Ok(more) = updates.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |this, cx| this.apply(batch, cx)).is_err() {
                    break;
                }
            }
        });
        let observer = cx.observe(&images, |_, _, cx| cx.notify());
        Self {
            room,
            items: Vec::new(),
            rows: Vec::new(),
            fingerprints: Vec::new(),
            list,
            controls: None,
            pagination: PaginationState::Idle,
            loaded: false,
            error: None,
            expanded: HashSet::new(),
            expand_all: false,
            revealed: HashSet::new(),
            images,
            pending_event: None,
            selected: None,
            picker: None,
            _task: task,
            _images: observer,
            #[cfg(test)]
            rendered_rows: 0,
        }
    }

    fn failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message);
        self.loaded = true;
        cx.notify();
    }

    /// The room.
    pub(crate) fn room(&self) -> &RoomSummary {
        &self.room
    }

    /// Updates the room's summary (its name, encryption, members).
    pub(crate) fn set_room(&mut self, room: RoomSummary, cx: &mut Context<Self>) {
        if self.room != room {
            self.room = room;
            cx.notify();
        }
    }

    /// The items (tests).
    #[cfg(test)]
    pub(crate) fn items(&self) -> &[TimelineItem] {
        &self.items
    }

    /// The rows (tests and the benchmark).
    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[TimelineRow] {
        &self.rows
    }

    /// The list's state (tests and the benchmark).
    #[cfg(test)]
    pub(crate) fn list_state(&self) -> &ListState {
        &self.list
    }

    /// How many rows were laid out since the last call (tests).
    #[cfg(test)]
    pub(crate) fn take_rendered_rows(&mut self) -> usize {
        std::mem::take(&mut self.rendered_rows)
    }

    /// Applies updates as the session's would arrive (tests and the
    /// benchmark).
    #[cfg(test)]
    pub(crate) fn apply_for_test(&mut self, batch: Vec<TimelineUpdate>, cx: &mut Context<Self>) {
        self.apply(batch, cx);
    }

    /// Whether the folded run on row `row` is open (tests).
    #[cfg(test)]
    pub(crate) fn is_expanded(&self, row: usize) -> bool {
        match self.rows.get(row) {
            Some(TimelineRow::StateGroup { range }) => {
                self.expand_all || self.expanded.contains(&self.items[range.start].id)
            }
            _ => false,
        }
    }

    /// Whether the first items have arrived.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The message `item`, with what its previews show.
    fn choice(&self, item: &TimelineItemId) -> Option<(MessageChoice, &TimelineEvent)> {
        self.items.iter().find_map(|found| match &found.kind {
            TimelineItemKind::Event(event) if &found.id == item => Some((
                MessageChoice {
                    target: MessageRef {
                        item: found.id.clone(),
                        event_id: event.event_id.clone(),
                    },
                    sender: event.sender_label(),
                    text: message_text(event),
                },
                event,
            )),
            _ => None,
        })
    }

    /// The message with `event_id`, with what its previews show.
    pub(crate) fn choice_for_event(&self, event_id: &OwnedEventId) -> Option<MessageChoice> {
        let item = self.items.iter().find_map(|item| match &item.kind {
            TimelineItemKind::Event(event) if event.event_id.as_ref() == Some(event_id) => {
                Some(item.id.clone())
            }
            _ => None,
        })?;
        self.choice(&item).map(|(choice, _)| choice)
    }

    /// The messages that have actions, oldest first: their items, and
    /// whether each is the account's own and editable.
    fn actionable(&self) -> Vec<(TimelineItemId, bool, bool)> {
        self.items
            .iter()
            .filter_map(|item| match &item.kind {
                TimelineItemKind::Event(event) => {
                    actions_for(event).map(|(own, editable)| (item.id.clone(), own, editable))
                }
                _ => None,
            })
            .collect()
    }

    /// The selected message.
    pub(crate) fn selected(&self) -> Option<&TimelineItemId> {
        self.selected.as_ref()
    }

    /// Selects the message `step` places from the selected one (from the
    /// newest when none is), and scrolls to it.
    pub(crate) fn select_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let actionable = self.actionable();
        if actionable.is_empty() {
            return;
        }
        let last = actionable.len() as isize - 1;
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| actionable.iter().position(|(item, _, _)| item == selected));
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, last),
            None if step < 0 => last,
            None => return,
        };
        let item = actionable[usize::try_from(next).unwrap_or_default()]
            .0
            .clone();
        self.select(Some(item), cx);
    }

    /// Selects `item` (or nothing), and scrolls to it.
    pub(crate) fn select(&mut self, item: Option<TimelineItemId>, cx: &mut Context<Self>) {
        self.picker = None;
        self.selected = item;
        if let Some(selected) = &self.selected
            && let Some(row) = self.row_of(selected)
        {
            self.reveal_row(row);
        }
        cx.notify();
    }

    /// Scrolls to `row`; the newest row keeps the timeline following new
    /// messages.
    fn reveal_row(&self, row: usize) {
        if row + 1 < self.rows.len() {
            self.list.set_follow_mode(FollowMode::Normal);
            self.list.scroll_to_reveal_item(row);
        }
    }

    fn row_of(&self, item: &TimelineItemId) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.items().any(|index| &self.items[index].id == item))
    }

    /// The message an action from the keyboard is about: the selected one,
    /// else the newest one that passes `accept` (`own`, `editable`).
    pub(crate) fn chosen(&self, accept: impl Fn(bool, bool) -> bool) -> Option<MessageChoice> {
        let actionable = self.actionable();
        let item = match &self.selected {
            Some(selected) => actionable
                .iter()
                .find(|(item, own, editable)| item == selected && accept(*own, *editable)),
            None => actionable
                .iter()
                .rev()
                .find(|(_, own, editable)| accept(*own, *editable)),
        }?;
        self.choice(&item.0).map(|(choice, _)| choice)
    }

    /// Opens the quick reaction picker on `item` (the selected or newest
    /// message when `None`).
    pub(crate) fn open_picker(&mut self, item: Option<TimelineItemId>, cx: &mut Context<Self>) {
        let item = item.or_else(|| self.chosen(|_, _| true).map(|choice| choice.target.item));
        if let Some(item) = &item
            && let Some(row) = self.row_of(item)
        {
            self.reveal_row(row);
        }
        self.picker = item;
        cx.notify();
    }

    /// Whether the quick reaction picker is open (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Closes the picker and the selection; `false` when neither was open.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.picker.take().is_some() | self.selected.take().is_some();
        cx.notify();
        open
    }

    fn react(&mut self, item: TimelineItemId, key: &str, cx: &mut Context<Self>) {
        self.picker = None;
        if let Some((choice, _)) = self.choice(&item) {
            cx.emit(TimelineViewEvent::React {
                target: choice.target,
                key: key.to_owned(),
            });
        }
        cx.notify();
    }

    fn message_action(&mut self, item: TimelineItemId, action: RowAction, cx: &mut Context<Self>) {
        self.picker = None;
        if let Some((choice, _)) = self.choice(&item) {
            cx.emit(match action {
                RowAction::Reply => TimelineViewEvent::Reply(choice),
                RowAction::Edit => TimelineViewEvent::Edit(choice),
                RowAction::Delete => TimelineViewEvent::Delete(choice),
            });
        }
        cx.notify();
    }

    /// Applies a batch of updates and measures again only the rows that
    /// changed.
    fn apply(&mut self, batch: Vec<TimelineUpdate>, cx: &mut Context<Self>) {
        let mut items_changed = false;
        for update in batch {
            match update {
                TimelineUpdate::Items(diffs) => {
                    items_changed = true;
                    if apply_all(&mut self.items, diffs).is_err() {
                        // Out of step: draw what there is; the session's
                        // next reset puts it right.
                        self.fingerprints.clear();
                    }
                }
                TimelineUpdate::Pagination(state) => self.pagination = state,
            }
        }
        if items_changed {
            self.rows = rows(&self.items);
            let fingerprints: Vec<u64> = self
                .rows
                .iter()
                .map(|row| row_fingerprint(row, &self.items))
                .collect();
            if !self.loaded {
                self.list.reset(self.rows.len());
                self.list.set_follow_mode(FollowMode::Tail);
            } else if let Some((range, count)) = changed_rows(&self.fingerprints, &fingerprints) {
                self.list.splice(range, count);
            }
            self.fingerprints = fingerprints;
            self.loaded = true;
            self.reveal_pending_event();
            if self
                .selected
                .as_ref()
                .is_some_and(|selected| self.row_of(selected).is_none())
            {
                self.selected = None;
            }
            if self.list.is_following_tail() {
                cx.emit(TimelineViewEvent::ReadToEnd);
            }
        }
        cx.notify();
    }

    /// Asks for older messages, once at a time, until the start.
    pub(crate) fn load_older(&mut self, cx: &mut Context<Self>) {
        if !self.loaded || !matches!(self.pagination, PaginationState::Idle) {
            return;
        }
        if self
            .items
            .first()
            .is_some_and(|item| item.kind == TimelineItemKind::TimelineStart)
        {
            return;
        }
        if let Some(controls) = &self.controls
            && controls.paginate_backwards(PAGE_EVENTS)
        {
            self.pagination = PaginationState::Loading;
            cx.notify();
        }
    }

    /// Asks again after older messages failed to load.
    fn retry_older(&mut self, cx: &mut Context<Self>) {
        self.pagination = PaginationState::Idle;
        self.load_older(cx);
    }

    /// Scrolls to the newest message and follows new ones.
    pub(crate) fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(FollowMode::Tail);
        self.list.scroll_to_end();
        cx.notify();
    }

    /// Scrolls to the read marker. `false` when it is not loaded (there
    /// is nothing unread, or it is older than what is loaded).
    pub(crate) fn jump_to_oldest_unread(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(row) = self.rows.iter().position(|row| {
            row.items()
                .any(|index| self.items[index].kind == TimelineItemKind::ReadMarker)
        }) else {
            return false;
        };
        self.list.set_follow_mode(FollowMode::Normal);
        self.list.scroll_to_reveal_item(row);
        cx.notify();
        true
    }

    /// Shows the event `event` once it is loaded.
    pub(crate) fn reveal_event(&mut self, event: OwnedEventId, cx: &mut Context<Self>) {
        self.pending_event = Some(event);
        self.reveal_pending_event();
        cx.notify();
    }

    fn reveal_pending_event(&mut self) {
        let Some(event) = &self.pending_event else {
            return;
        };
        let found = self.rows.iter().position(|row| {
            row.items().any(|index| {
                matches!(&self.items[index].kind, TimelineItemKind::Event(item)
                    if item.event_id.as_ref() == Some(event))
            })
        });
        if let Some(row) = found {
            self.list.set_follow_mode(FollowMode::Normal);
            self.list.scroll_to_reveal_item(row);
            self.pending_event = None;
        }
    }

    /// Opens every folded run of room changes.
    pub(crate) fn expand_all_changes(&mut self, cx: &mut Context<Self>) {
        self.expand_all = true;
        self.list.remeasure();
        cx.notify();
    }

    fn toggle_group(&mut self, first: TimelineItemId, row: usize, cx: &mut Context<Self>) {
        if self.expand_all {
            // Folding one folds it alone.
            self.expand_all = false;
            for row in &self.rows {
                if let TimelineRow::StateGroup { range } = row {
                    self.expanded.insert(self.items[range.start].id.clone());
                }
            }
            self.list.remeasure();
        }
        if !self.expanded.remove(&first) {
            self.expanded.insert(first);
        }
        self.list.splice(row..row + 1, 1);
        cx.notify();
    }

    fn reveal_spoilers(&mut self, item: TimelineItemId, row: usize, cx: &mut Context<Self>) {
        if self.revealed.insert(item) {
            self.list.splice(row..row + 1, 1);
            cx.notify();
        }
    }

    fn link_clicked(&mut self, rich: &RichText, link: usize, cx: &mut Context<Self>) {
        let Some(target) = rich.link(link) else {
            return;
        };
        let text = rich
            .blocks
            .iter()
            .flat_map(|block| {
                block
                    .spans
                    .iter()
                    .filter(|span| span.style.link == Some(link))
                    .filter_map(|span| block.text.get(span.range.clone()))
            })
            .collect::<Vec<_>>()
            .join(" ");
        cx.emit(TimelineViewEvent::Link {
            href: target.href.clone(),
            text,
        });
    }

    // -- Rendering ----------------------------------------------------------

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        {
            self.rendered_rows += 1;
        }
        let colors = ComponentTheme::get(cx).colors;
        let Some(row) = self.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        let now = now_ms();
        match row {
            TimelineRow::StateGroup { range } => self.render_state_group(index, range, &colors, cx),
            TimelineRow::Item {
                index: item,
                continues,
            } => {
                let item = self.items[item].clone();
                match &item.kind {
                    TimelineItemKind::DateDivider { timestamp_ms } => {
                        date_divider(index, &day_label(*timestamp_ms, now), &colors)
                    }
                    TimelineItemKind::ReadMarker => read_marker(index, &colors),
                    TimelineItemKind::TimelineStart => self.render_start(&colors),
                    TimelineItemKind::Event(event) if event.content.is_state() => {
                        state_line(index, event, &colors)
                    }
                    TimelineItemKind::Event(event) => {
                        self.render_message(index, &item.id, event, continues, &colors, cx)
                    }
                }
            }
        }
    }

    fn render_start(&self, colors: &UiColors) -> AnyElement {
        let has_messages = self.items.iter().any(|item| {
            matches!(&item.kind, TimelineItemKind::Event(event) if !event.content.is_state())
        });
        let name = self.room.name.clone();
        div()
            .id("timeline-start")
            .w_full()
            .accessible(Role::Heading, format!("The start of {name}"))
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .px(px(SPACE_6))
            .pt(px(SPACE_6))
            .pb(px(SPACE_4))
            .child(Avatar::new(name.clone()).size(56.0))
            .child(
                div()
                    .type_style(TYPE_HEADLINE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name.clone()),
            )
            .child(
                div()
                    .type_style(TYPE_BODY)
                    .text_color(rgb(colors.muted))
                    .child(readable(
                        "timeline-start-detail",
                        format!(
                            "This is the start of {name}. {}",
                            self.room.encryption.description()
                        ),
                    )),
            )
            .when(!has_messages, |this| {
                this.child(
                    div()
                        .mt(px(SPACE_2))
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child(readable(
                            "timeline-empty",
                            "No messages yet. What people send here shows up below.",
                        )),
                )
            })
            .into_any_element()
    }

    fn render_state_group(
        &mut self,
        row: usize,
        range: std::ops::Range<usize>,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let first_id = self.items[range.start].id.clone();
        let expanded = self.expand_all || self.expanded.contains(&first_id);
        let lines: Vec<String> = self.items[range.clone()]
            .iter()
            .filter_map(|item| match &item.kind {
                TimelineItemKind::Event(event) => describe(&event.content, &event.sender_label()),
                _ => None,
            })
            .collect();
        let first = lines.first().cloned().unwrap_or_default();
        let summary = summary_label(&first, lines.len());
        let toggle_id = first_id.clone();
        div()
            .id(("state-group", row as u64))
            .w_full()
            .accessible(Role::Group, summary.clone())
            .flex()
            .flex_col()
            .gap(px(SPACE_1))
            .px(px(SPACE_6))
            .py(px(SPACE_1))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(icon("users", ICON_SIZE_SM, colors.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child(summary),
                    )
                    .child(
                        Button::new(
                            ElementId::NamedInteger("state-group-toggle".into(), row as u64),
                            if expanded { "Hide" } else { "Show all" },
                        )
                        .size(ButtonSize::Small)
                        .ghost()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_group(toggle_id.clone(), row, cx);
                        })),
                    ),
            )
            .when(expanded, |this| {
                this.children(lines.into_iter().enumerate().map(|(index, line)| {
                    div()
                        .pl(px(SPACE_6))
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(readable(("state-group-line", index as u64), line))
                }))
            })
            .into_any_element()
    }

    fn render_message(
        &mut self,
        row: usize,
        id: &TimelineItemId,
        event: &TimelineEvent,
        continues: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sender = event.sender_label();
        let time = time_label(event.timestamp_ms);
        let label = accessible_label(&sender, &time, event);
        let name_color = avatar_color(&sender, Some(event.sender.as_str()));
        let body = self.render_body(row, id, event, colors, cx);
        let selected = self.selected.as_ref() == Some(id);
        let picker = self.picker.as_ref() == Some(id);
        let toolbar = actions_for(event).map(|(own, editable)| {
            self.render_toolbar(
                row,
                id,
                &sender,
                own,
                editable,
                selected || picker,
                picker,
                colors,
                cx,
            )
        });
        let reactions_row = (!event.reactions.is_empty())
            .then(|| self.render_reactions(row, id, &event.reactions, colors, cx));
        let group: SharedString = format!("timeline-row-{row}").into();
        div()
            .id(("timeline-row", row as u64))
            .group(group)
            .relative()
            .w_full()
            .accessible(Role::ListItem, label)
            .when(selected, |this| this.bg(rgb(colors.selected_background)))
            .flex()
            .gap(px(SPACE_3))
            .px(px(SPACE_6))
            .pt(px(if continues { SPACE_0_5 } else { SPACE_3 }))
            .pb(px(SPACE_0_5))
            .hover(|this| this.bg(rgb(mix(colors.background, colors.hover_background, 0.6))))
            .child(div().flex_none().w(px(AVATAR)).when(!continues, |this| {
                this.child(
                    Avatar::new(sender.clone())
                        .address(event.sender.to_string())
                        .size(AVATAR),
                )
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(SPACE_1))
                    .when(!continues, |this| {
                        this.child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(px(SPACE_2))
                                .child(
                                    div()
                                        .type_style(TYPE_BODY_STRONG)
                                        .text_color(rgb(readable_name(name_color, colors)))
                                        .child(sender.clone()),
                                )
                                .child(
                                    div()
                                        .type_style(TYPE_CAPTION)
                                        .text_color(rgb(colors.muted))
                                        .child(time.clone()),
                                ),
                        )
                    })
                    .children(
                        event
                            .in_reply_to
                            .as_ref()
                            .map(|reply| self.render_reply(row, reply, colors, cx)),
                    )
                    .child(body)
                    .children(meta_line(event, colors))
                    .children(self.render_send_problem(row, id, event, colors, cx))
                    .children(reactions_row),
            )
            .children(toolbar)
            .into_any_element()
    }

    /// Why the encryption stopped one of the account's messages, with
    /// "Send anyway" when that resolves it (ADR 0009).
    fn render_send_problem(
        &self,
        row: usize,
        id: &TimelineItemId,
        event: &TimelineEvent,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(SendState::Failed {
            problem: Some(problem),
            ..
        }) = event.send_state
        else {
            return None;
        };
        let target = MessageRef {
            item: id.clone(),
            event_id: event.event_id.clone(),
        };
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SPACE_2))
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.danger))
                        .child(readable(
                            ("timeline-send-problem", row as u64),
                            problem.message(),
                        )),
                )
                .when(problem.can_send_anyway(), |this| {
                    this.child(
                        Button::new(("timeline-send-anyway", row as u64), wording::SEND_ANYWAY)
                            .size(ButtonSize::Small)
                            .icon("send")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(TimelineViewEvent::SendAnyway(target.clone()));
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// A message's actions: shown while it is hovered, selected, or its
    /// picker is open.
    #[allow(clippy::too_many_arguments)]
    fn render_toolbar(
        &self,
        row: usize,
        id: &TimelineItemId,
        sender: &str,
        own: bool,
        editable: bool,
        shown: bool,
        picker: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group: SharedString = format!("timeline-row-{row}").into();
        let item = id.clone();
        let whose = if own {
            "your message".to_owned()
        } else {
            format!("{sender}'s message")
        };
        let button = |name: &str, icon_name: &'static str, label: String, action: RowAction| {
            let item = item.clone();
            IconButton::new(
                ElementId::NamedInteger(format!("timeline-{name}").into(), row as u64),
                icon_name,
                label,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.message_action(item.clone(), action, cx);
            }))
        };
        let react_item = item.clone();
        div()
            .id(("timeline-actions", row as u64))
            .accessible(Role::Toolbar, format!("Actions for {whose}"))
            .absolute()
            .top(px(-SPACE_2))
            .right(px(SPACE_6))
            .flex()
            .items_center()
            .gap(px(SPACE_0_5))
            .p(px(SPACE_0_5))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.surface))
            .shadow_sm()
            .when(!shown, |this| {
                this.invisible().group_hover(group, |style| style.visible())
            })
            .child(
                IconButton::new(
                    ("timeline-react", row as u64),
                    "face-slightly-smiling-plus",
                    format!("React to {whose}"),
                )
                .selected(picker)
                .shortcut(crate::signin::primary_key("ReactToMessage"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let open = this.picker.as_ref() != Some(&react_item);
                    this.open_picker(open.then(|| react_item.clone()), cx);
                    if !open {
                        this.picker = None;
                    }
                })),
            )
            .child(
                button(
                    "reply",
                    "reply",
                    format!("Reply to {whose}"),
                    RowAction::Reply,
                )
                .shortcut(crate::signin::primary_key("ReplyToMessage")),
            )
            .when(editable, |this| {
                this.child(
                    button(
                        "edit",
                        "pencil",
                        "Edit your message".to_owned(),
                        RowAction::Edit,
                    )
                    .shortcut(crate::signin::primary_key("EditMessage")),
                )
            })
            .when(own, |this| {
                this.child(
                    button(
                        "delete",
                        "trash-2",
                        "Delete your message".to_owned(),
                        RowAction::Delete,
                    )
                    .shortcut(crate::signin::primary_key("DeleteMessage")),
                )
            })
            .when(picker, |this| {
                this.child(self.render_picker(row, id, colors, cx))
            })
            .into_any_element()
    }

    /// The quick reaction picker, under the message's actions.
    fn render_picker(
        &self,
        row: usize,
        id: &TimelineItemId,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Above the actions: the newest messages are at the bottom.
        div()
            .absolute()
            .bottom_full()
            .right_0()
            .mb(px(SPACE_1))
            .child(
                div()
                    .id(("timeline-picker", row as u64))
                    .accessible(Role::Toolbar, "Quick reactions")
                    .flex()
                    .gap(px(SPACE_0_5))
                    .p(px(SPACE_1))
                    .rounded(px(RADIUS_LG))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .bg(rgb(colors.surface))
                    .shadow_md()
                    .occlude()
                    .children(QUICK_REACTIONS.iter().enumerate().map(|(index, key)| {
                        let item = id.clone();
                        div()
                            .id(ElementId::NamedInteger(
                                "timeline-quick-reaction".into(),
                                (row as u64) << 8 | index as u64,
                            ))
                            .hit_target(move || format!("timeline-quick-reaction-{row}-{index}"))
                            .accessible(Role::Button, format!("React with {key}"))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(32.0))
                            .rounded(px(RADIUS_MD))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(colors.hover_background)))
                            .text_size(px(18.0))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.react(item.clone(), key, cx);
                            }))
                            .child(*key)
                    })),
            )
            .into_any_element()
    }

    /// A message's reactions: each one adds or takes back the account's.
    fn render_reactions(
        &self,
        row: usize,
        id: &TimelineItemId,
        reactions: &[Reaction],
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .gap(px(SPACE_1))
            .children(reactions.iter().enumerate().map(|(index, reaction)| {
                let who = if reaction.senders.is_empty() {
                    format!("{} people", reaction.count)
                } else {
                    reaction.senders.join(", ")
                };
                let label = if reaction.own {
                    format!(
                        "{} {}, {who}. Take back your reaction",
                        reaction.key, reaction.count
                    )
                } else {
                    format!(
                        "{} {}, {who}. React with {}",
                        reaction.key, reaction.count, reaction.key
                    )
                };
                let tip: SharedString = format!("Reacted with {}: {who}", reaction.key).into();
                let (fill, border) = if reaction.own {
                    (mix(colors.background, colors.accent, 0.16), colors.accent)
                } else {
                    (colors.surface, colors.border)
                };
                let item = id.clone();
                let key = reaction.key.clone();
                div()
                    .id(ElementId::NamedInteger(
                        "timeline-reaction".into(),
                        (row as u64) << 8 | index as u64,
                    ))
                    .hit_target(move || format!("timeline-reaction-{row}-{index}"))
                    .accessible(Role::Button, label)
                    .tooltip(Tooltip::text(tip))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_1))
                    .h(px(24.0))
                    .min_w(px(24.0))
                    .px(px(SPACE_2))
                    .rounded(px(RADIUS_FULL))
                    .border_1()
                    .border_color(rgb(border))
                    .bg(rgb(fill))
                    .cursor_pointer()
                    .hover(|this| this.border_color(rgb(colors.accent)))
                    .type_style(TYPE_CAPTION)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.react(item.clone(), &key, cx);
                    }))
                    .child(reaction.key.clone())
                    .child(
                        div()
                            .text_color(rgb(colors.muted))
                            .child(reaction.count.to_string()),
                    )
            }))
            .into_any_element()
    }

    fn render_reply(
        &self,
        row: usize,
        reply: &ReplyPreview,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sender = reply
            .sender
            .clone()
            .unwrap_or_else(|| "A message".to_owned());
        let text = reply
            .text
            .as_ref()
            .map(|text| one_line(text.as_str()))
            .unwrap_or_else(|| "Loading the message…".to_owned());
        let event = reply.event_id.clone();
        div()
            .id(("timeline-reply", row as u64))
            .hit_target(move || format!("timeline-reply-{row}"))
            .accessible(Role::Button, format!("Reply to {sender}: {text}"))
            .flex()
            .items_center()
            .gap(px(SPACE_2))
            .min_h(px(28.0))
            .max_w(px(520.0))
            .px(px(SPACE_2))
            .py(px(SPACE_1))
            .rounded(px(RADIUS_MD))
            .border_l_2()
            .border_color(rgb(colors.accent))
            .bg(rgb(mix(colors.background, colors.surface, 0.8)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.reveal_event(event.clone(), cx)))
            .child(icon("corner-down-right", ICON_SIZE_SM, colors.muted))
            .child(
                div()
                    .flex_none()
                    .type_style(TYPE_CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(sender),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(text),
            )
            .into_any_element()
    }

    fn render_body(
        &mut self,
        row: usize,
        id: &TimelineItemId,
        event: &TimelineEvent,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &event.content {
            TimelineContent::Message {
                kind, rich, media, ..
            } => {
                let tone = match kind {
                    MessageKind::Notice => TextTone::Notice,
                    MessageKind::Emote => TextTone::Emote,
                    _ => TextTone::Normal,
                };
                let revealed = self.revealed.contains(id);
                let text = if *kind == MessageKind::Emote {
                    emote_text(&event.sender_label(), rich)
                } else {
                    rich.clone()
                };
                let this = cx.entity().downgrade();
                let item = id.clone();
                let clicked = text.clone();
                let on_click: super::rich_text::ClickHandler = Rc::new(move |click, _, cx| {
                    let _ = this.update(cx, |this, cx| match click {
                        TextClick::Link(link) => this.link_clicked(&clicked, link, cx),
                        TextClick::Spoiler => this.reveal_spoilers(item.clone(), row, cx),
                    });
                });
                let show_text = media.is_none()
                    || (!text.is_empty()
                        && media
                            .as_ref()
                            .is_some_and(|media| text.plain_text() != media.filename));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .min_w_0()
                    .when(*kind == MessageKind::Notice, |this| {
                        this.child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(SPACE_2))
                                .child(icon("info", ICON_SIZE_SM, colors.muted))
                                .child(div().flex_1().min_w_0().child(render_rich_text(
                                    ("message-text", row as u64),
                                    &text,
                                    tone,
                                    revealed,
                                    colors,
                                    on_click.clone(),
                                ))),
                        )
                    })
                    .when(*kind != MessageKind::Notice && show_text, |this| {
                        this.child(render_rich_text(
                            ("message-text", row as u64),
                            &text,
                            tone,
                            revealed,
                            colors,
                            on_click.clone(),
                        ))
                    })
                    .children(
                        media
                            .as_ref()
                            .map(|media| self.render_media(row, *kind, media, colors, cx)),
                    )
                    .into_any_element()
            }
            TimelineContent::Sticker { body } => div()
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .child(format!("Sticker: {}", body.as_str()))
                .into_any_element(),
            content => {
                let (icon_name, text) = match content {
                    TimelineContent::Redacted => ("trash-2", placeholder(content)),
                    TimelineContent::UnableToDecrypt(reason) => (
                        match reason {
                            UtdReason::Waiting => "hourglass",
                            _ => "lock",
                        },
                        placeholder(content),
                    ),
                    _ => ("message-square-dashed", placeholder(content)),
                };
                div()
                    .flex()
                    .items_start()
                    .gap(px(SPACE_2))
                    .type_style(TYPE_BODY)
                    .italic()
                    .text_color(rgb(colors.muted))
                    .child(
                        div()
                            .pt(px(2.0))
                            .child(icon(icon_name, ICON_SIZE_SM, colors.muted)),
                    )
                    .child(div().flex_1().min_w_0().child(text.unwrap_or_default()))
                    .into_any_element()
            }
        }
    }

    fn render_media(
        &mut self,
        row: usize,
        kind: MessageKind,
        media: &MediaInfo,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let save = {
            let media = media.clone();
            cx.listener(move |_, _, _, cx| cx.emit(TimelineViewEvent::Save(media.clone())))
        };
        if kind != MessageKind::Image {
            let icon_name = match kind {
                MessageKind::Video => "file-image",
                MessageKind::Audio => "file",
                _ if media.filename.to_ascii_lowercase().ends_with(".pdf") => "file-text",
                _ => "file",
            };
            let detail = match (kind, media.size_label()) {
                (MessageKind::Video, Some(size)) => format!("Video, {size}"),
                (MessageKind::Audio, Some(size)) => format!("Audio, {size}"),
                (_, Some(size)) => size,
                (MessageKind::Video, None) => "Video".to_owned(),
                (MessageKind::Audio, None) => "Audio".to_owned(),
                _ => "File".to_owned(),
            };
            return div()
                .id(("timeline-file", row as u64))
                .accessible(Role::Group, format!("{}, {detail}", media.filename))
                .flex()
                .items_center()
                .gap(px(SPACE_3))
                .max_w(px(420.0))
                .p(px(SPACE_3))
                .rounded(px(RADIUS_LG))
                .border_1()
                .border_color(rgb(colors.border))
                .bg(rgb(colors.surface))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(40.0))
                        .rounded(px(RADIUS_MD))
                        .bg(rgb(mix(colors.surface, colors.accent, 0.14)))
                        .child(icon(icon_name, 20.0, colors.accent)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .type_style(TYPE_BODY_STRONG)
                                .child(media.filename.clone()),
                        )
                        .child(
                            div()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .child(detail),
                        ),
                )
                .child(
                    IconButton::new(
                        ElementId::NamedInteger("timeline-save".into(), row as u64),
                        "download",
                        format!("Save {}", media.filename),
                    )
                    .on_click(save),
                )
                .into_any_element();
        }

        let (width, height) = fit(media.declared_size());
        let state = self.images.update(cx, |images, cx| images.get(media, cx));
        let label = format!("Image: {}", media.filename);
        let content = match state {
            ImageState::Ready(image) => img(ImageSource::Render(image))
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            ImageState::Loading => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(spinner("timeline-image-spinner", 20.0, colors.muted))
                .into_any_element(),
            ImageState::Failed(error) => {
                let (icon_name, sentence) = match &error {
                    MediaError::Svg => ("shield-off", error.to_string()),
                    _ => ("image-off", error.to_string()),
                };
                let retry = matches!(error, MediaError::Unavailable).then(|| {
                    let media = media.clone();
                    let images = self.images.clone();
                    Button::new(
                        ElementId::NamedInteger("timeline-image-retry".into(), row as u64),
                        "Try again",
                    )
                    .size(ButtonSize::Small)
                    .on_click(move |_, _, cx| {
                        images.update(cx, |images, cx| images.retry(&media, cx));
                    })
                });
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .text_center()
                    .child(icon(icon_name, 24.0, colors.muted))
                    .child(
                        div()
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child(readable(("timeline-image-problem", row as u64), sentence)),
                    )
                    .children(retry)
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_1))
            .child(
                div()
                    .id(("timeline-image", row as u64))
                    .accessible(Role::Image, label)
                    .relative()
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(RADIUS_LG))
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(colors.border))
                    .bg(rgb(mix(colors.surface, colors.border, 0.4)))
                    .child(content)
                    .child(
                        div().absolute().top(px(SPACE_2)).right(px(SPACE_2)).child(
                            IconButton::new(
                                ElementId::NamedInteger("timeline-save".into(), row as u64),
                                "download",
                                format!("Save {}", media.filename),
                            )
                            .variant(crate::components::ButtonVariant::Subtle)
                            .on_click(save),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(SPACE_2))
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(media.filename.clone())
                    .children(media.size_label()),
            )
            .into_any_element()
    }
}

/// The size to draw an image at: its declared size fitted in the
/// timeline's box, or a placeholder box.
fn fit(declared: Option<(u32, u32)>) -> (f32, f32) {
    let Some((width, height)) = declared else {
        return (320.0, 200.0);
    };
    let (width, height) = (width as f32, height as f32);
    let scale = (IMAGE_MAX_WIDTH / width)
        .min(IMAGE_MAX_HEIGHT / height)
        .min(1.0);
    ((width * scale).max(120.0), (height * scale).max(80.0))
}

/// An emote's text: the sender's name, then what they did.
fn emote_text(sender: &str, rich: &RichText) -> RichText {
    let mut text = rich.clone();
    if let Some(first) = text.blocks.first_mut() {
        let prefix = format!("{sender} ");
        let shift = prefix.len();
        first.text.insert_str(0, &prefix);
        for span in &mut first.spans {
            span.range = span.range.start + shift..span.range.end + shift;
        }
    }
    text
}

/// A name colour that reads on the background.
fn readable_name(color: u32, colors: &UiColors) -> u32 {
    crate::styles::contrast::ensure_contrast(color, &[colors.background], 4.5, colors.foreground)
}

fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let line = line.trim();
    if line.chars().count() > 120 {
        let cut: String = line.chars().take(117).collect();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

/// What a screen reader reads for a message row.
fn accessible_label(sender: &str, time: &str, event: &TimelineEvent) -> String {
    let text = match &event.content {
        TimelineContent::Message {
            kind, body, media, ..
        } => match (kind, media) {
            (MessageKind::Image, Some(media)) => format!("Image {}", media.filename),
            (_, Some(media)) => format!("File {}", media.filename),
            _ => one_line(body.as_str()),
        },
        TimelineContent::Sticker { body } => format!("Sticker {}", body.as_str()),
        content => placeholder(content).unwrap_or_default().to_owned(),
    };
    let mut label = format!("{sender}, {time}: {text}");
    if let TimelineContent::Message { edited: true, .. } = event.content {
        label.push_str(" (edited)");
    }
    label
}

/// "Edited", "Sending…" or "Not sent" under a message.
fn meta_line(event: &TimelineEvent, colors: &UiColors) -> Option<AnyElement> {
    let edited = matches!(event.content, TimelineContent::Message { edited: true, .. });
    let (text, color) = match event.send_state {
        Some(SendState::Sending) => ("Sending…", colors.muted),
        // The encryption's reason shows on its own line instead.
        Some(SendState::Failed {
            problem: Some(_), ..
        }) => return None,
        Some(SendState::Failed { .. }) => ("Not sent", colors.danger),
        _ if edited => ("Edited", colors.muted),
        _ => return None,
    };
    Some(
        div()
            .type_style(TYPE_CAPTION)
            .text_color(rgb(color))
            .child(text)
            .into_any_element(),
    )
}

/// What a message's action button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowAction {
    Reply,
    Edit,
    Delete,
}

fn date_divider(row: usize, label: &str, colors: &UiColors) -> AnyElement {
    div()
        .id(("timeline-day", row as u64))
        .w_full()
        .accessible(Role::Heading, label.to_owned())
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .px(px(SPACE_6))
        .pt(px(SPACE_4))
        .pb(px(SPACE_1))
        .child(div().flex_1().h(px(1.0)).bg(rgb(colors.border)))
        .child(
            div()
                .px(px(SPACE_3))
                .py(px(2.0))
                .rounded(px(RADIUS_FULL))
                .border_1()
                .border_color(rgb(colors.border))
                .bg(rgb(colors.background))
                .type_style(TYPE_CAPTION)
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(colors.muted))
                .child(label.to_owned()),
        )
        .child(div().flex_1().h(px(1.0)).bg(rgb(colors.border)))
        .into_any_element()
}

fn read_marker(row: usize, colors: &UiColors) -> AnyElement {
    div()
        .id(("timeline-read-marker", row as u64))
        .w_full()
        .accessible(Role::Label, "New messages")
        .flex()
        .items_center()
        .gap(px(SPACE_2))
        .px(px(SPACE_6))
        .py(px(SPACE_2))
        .child(div().flex_1().h(px(1.0)).bg(rgb(colors.danger)))
        .child(
            div()
                .type_style(TYPE_CAPTION)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.danger))
                .child("New messages"),
        )
        .into_any_element()
}

fn state_line(row: usize, event: &TimelineEvent, colors: &UiColors) -> AnyElement {
    let text = describe(&event.content, &event.sender_label()).unwrap_or_default();
    let icon_name = match &event.content {
        TimelineContent::State {
            change: familychat_core::timeline::StateChange::Encryption,
            ..
        } => "lock",
        TimelineContent::Membership { .. } => "user",
        TimelineContent::ProfileChange { .. } => "pencil",
        _ => "settings",
    };
    div()
        .id(("timeline-state", row as u64))
        .w_full()
        .accessible(Role::Label, text.clone())
        .flex()
        .items_center()
        .gap(px(SPACE_2))
        .px(px(SPACE_6))
        .py(px(SPACE_1))
        .child(icon(icon_name, ICON_SIZE_SM, colors.muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE_CAPTION)
                .text_color(rgb(colors.muted))
                .child(text),
        )
        .into_any_element()
}

/// The not-encrypted banner's sentence for a room, when it needs one.
pub(crate) fn encryption_notice(room: &RoomSummary) -> Option<&'static str> {
    (room.encryption == Encryption::NotEncrypted).then(|| room.encryption.description())
}

const SPACE_0_5: f32 = crate::styles::SPACE_0_5;

impl Render for TimelineView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        if let Some(error) = &self.error {
            return div()
                .id("timeline")
                .size_full()
                .flex()
                .child(empty_state(
                    "This room couldn't be opened",
                    error,
                    "alert-circle",
                    None,
                    &colors,
                ))
                .into_any_element();
        }
        if !self.loaded {
            return div()
                .id("timeline")
                .accessible(Role::Status, "Loading messages")
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_3))
                .child(spinner("timeline-loading", 24.0, colors.muted))
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child("Loading messages…"),
                )
                .into_any_element();
        }
        let following = self.list.is_following_tail();
        let pagination = self.pagination.clone();
        div()
            .id("timeline")
            .accessible(Role::List, format!("Messages in {}", self.room.name))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, index, _window, cx| this.render_row(index, cx)),
                )
                .flex_1()
                .w_full()
                .pb(px(SPACE_3)),
            )
            .when(pagination == PaginationState::Loading, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(SPACE_3))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("timeline-loading-older")
                                .accessible(Role::Status, "Loading older messages")
                                .flex()
                                .items_center()
                                .gap(px(SPACE_2))
                                .px(px(SPACE_3))
                                .py(px(SPACE_1))
                                .rounded(px(RADIUS_FULL))
                                .border_1()
                                .border_color(rgb(colors.border))
                                .bg(rgb(colors.background))
                                .shadow_sm()
                                .type_style(TYPE_CAPTION)
                                .child(spinner("timeline-older-spinner", 14.0, colors.muted))
                                .child("Loading older messages…"),
                        ),
                )
            })
            .when_some(
                match &pagination {
                    PaginationState::Failed { message } => Some(message.clone()),
                    _ => None,
                },
                |this, message| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(SPACE_3))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(SPACE_2))
                                    .px(px(SPACE_3))
                                    .py(px(SPACE_1))
                                    .rounded(px(RADIUS_FULL))
                                    .bg(rgb(colors.danger_background))
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(colors.danger))
                                    .child(readable("timeline-older-problem", message))
                                    .child(
                                        Button::new("timeline-older-retry", "Try again")
                                            .size(ButtonSize::Small)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.retry_older(cx);
                                            })),
                                    ),
                            ),
                    )
                },
            )
            .when(!following && !self.rows.is_empty(), |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom(px(SPACE_4))
                        .right(px(SPACE_6))
                        .rounded(px(RADIUS_FULL))
                        .shadow_md()
                        .child(
                            IconButton::new(
                                "timeline-jump-latest",
                                "arrow-down-to-line",
                                "Jump to the latest message",
                            )
                            .variant(crate::components::ButtonVariant::Subtle)
                            .shortcut(crate::signin::primary_key("JumpToLatest"))
                            .on_click(cx.listener(|this, _, _, cx| this.jump_to_latest(cx))),
                        ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_fit_the_timeline_box_and_keep_their_shape() {
        assert_eq!(fit(None), (320.0, 200.0));
        let (width, height) = fit(Some((1600, 1067)));
        assert!(width <= IMAGE_MAX_WIDTH && height <= IMAGE_MAX_HEIGHT);
        assert!((width / height - 1600.0 / 1067.0).abs() < 0.01);
        let (width, height) = fit(Some((40_000, 8_000)));
        assert!(width <= IMAGE_MAX_WIDTH && height >= 76.0);
        assert_eq!(fit(Some((100, 50))), (120.0, 80.0));
    }

    #[test]
    fn emotes_start_with_the_sender() {
        let rich = RichText::plain("is buying chocolate chips");
        let emote = emote_text("Dad", &rich);
        assert_eq!(emote.plain_text(), "Dad is buying chocolate chips");
        assert_eq!(one_line("\n\n first \nsecond"), "first");
    }
}
