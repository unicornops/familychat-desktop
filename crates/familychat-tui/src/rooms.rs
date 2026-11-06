//! The rooms in the terminal (ADR 0008; PLAN.md, sections 5.6 and 13.10):
//! the room list, the open room's timeline and members, the sync state,
//! links, files and the status line's new-message notices.
//!
//! The lists are the session's, kept here as `ListDiff` batches arrive
//! ([`familychat_core::diff::apply_all`]); the timeline's rows, rich text
//! and wording are `familychat-core`'s. Below 120 columns the list and the
//! room take turns on the screen; from 120 they sit side by side and the
//! room follows the list's cursor, as Zmail's preview pane follows its
//! index.

use std::{collections::HashMap, path::PathBuf, time::Instant};

use familychat_actions::{ActionEvent, Actions};
use familychat_core::{
    OwnedRoomId,
    diff::{ListDiff, apply_all},
    encryption::IdentityWarning,
    links::{LinkAction, MatrixLink, classify},
    media::{MediaError, MediaInfo},
    rooms::{RoomListFilter, RoomListLoading, RoomMember, RoomSummary, sort_members},
    session::{
        AccountView, MediaReply, Reply, RoomListControls, RoomListSubscription, SessionError,
        TimelineControls, TimelineSubscription,
    },
    sync::{AccountStatus, SyncState},
    timeline::{PaginationState, TimelineItem, TimelineItemId, TimelineUpdate},
};
use familychat_security::{file_names::sanitize_filename, terminal::sanitize_line};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{composer::Composer, encryption::EncryptionState};

use crate::{
    app::{App, CommandEntry, Effect, Prompt, RoomsLaunch, SIDE_BY_SIDE_FROM, Screen},
    commands::Cmd,
    pane::PaneLine,
};

/// Events asked for at a time when the top of the timeline is reached.
const PAGE_EVENTS: u16 = 20;

/// Which side has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// The room list.
    List,
    /// The open room's commands (the timeline): single letters are
    /// commands.
    Room,
    /// The open room's composer: letters are text (PLAN.md decision 10).
    Composer,
}

/// A link or a file in the open room, by the number it shows as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reference {
    /// A link: where it goes and its text.
    Link {
        /// The address, as the sanitiser kept it.
        href: String,
        /// The link's text.
        text: String,
    },
    /// An image or a file, saved where the person says.
    File(MediaInfo),
}

/// The open room's timeline, laid out for one width.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    /// What it was laid out for: the width, the timeline's generation,
    /// the selected reference and whether spoilers show.
    pub(crate) key: (u16, u64, Option<usize>, bool),
    /// The lines.
    pub(crate) lines: Vec<PaneLine>,
    /// The links and files, in the order they are numbered.
    pub(crate) refs: Vec<Reference>,
    /// Each reference's first line.
    pub(crate) ref_lines: Vec<usize>,
    /// The read marker's line.
    pub(crate) read_marker: Option<usize>,
    /// The last item's id when it was laid out.
    pub(crate) last_item: Option<String>,
    /// The selected message's first line.
    pub(crate) selected_line: Option<usize>,
}

/// The open room's members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MembersState {
    /// Not asked for.
    NotAsked,
    /// Asked for.
    Loading,
    /// Here.
    Ready(Vec<RoomMember>),
    /// They couldn't be read.
    Failed(String),
}

/// The open room.
#[derive(Debug)]
pub(crate) struct OpenRoom {
    pub(crate) room: RoomSummary,
    reply: Option<Reply<TimelineSubscription>>,
    updates: Option<mpsc::Receiver<TimelineUpdate>>,
    controls: Option<TimelineControls>,
    pub(crate) items: Vec<TimelineItem>,
    pub(crate) loaded: bool,
    pub(crate) error: Option<String>,
    pub(crate) pagination: PaginationState,
    pub(crate) members: MembersState,
    members_reply: Option<Reply<Vec<RoomMember>>>,
    /// Lines between the bottom of the timeline and the bottom of the
    /// pane: 0 follows the newest message.
    pub(crate) from_bottom: usize,
    pub(crate) selected_ref: Option<usize>,
    pub(crate) spoilers: bool,
    /// Counts the timeline's changes, for the layout cache.
    pub(crate) generation: u64,
    pub(crate) layout: Option<Layout>,
    /// Where the person writes (none for an invitation).
    pub(crate) composer: Option<Composer>,
    /// The message the room's commands are about (`[` and `]`).
    pub(crate) selected_item: Option<TimelineItemId>,
    /// The selected message is to be scrolled into view.
    pub(crate) reveal_selected: bool,
    warnings_reply: Option<Reply<watch::Receiver<Vec<IdentityWarning>>>>,
    warnings_rx: Option<watch::Receiver<Vec<IdentityWarning>>>,
    /// The room's identity warnings.
    pub(crate) warnings: Vec<IdentityWarning>,
    /// The warning being answered.
    pub(crate) acknowledging: Option<(familychat_core::OwnedUserId, Reply<()>)>,
}

/// The signed-in screens' state.
#[derive(Debug)]
pub(crate) struct RoomsState {
    pub(crate) account: AccountView,
    status_rx: Option<watch::Receiver<AccountStatus>>,
    pub(crate) status: AccountStatus,
    list_reply: Option<Reply<RoomListSubscription>>,
    updates: Option<mpsc::Receiver<Vec<ListDiff<RoomSummary>>>>,
    loading_rx: Option<watch::Receiver<RoomListLoading>>,
    controls: Option<RoomListControls>,
    pub(crate) rooms: Vec<RoomSummary>,
    pub(crate) loading: RoomListLoading,
    pub(crate) list_error: Option<String>,
    pub(crate) filter: RoomListFilter,
    pub(crate) cursor: usize,
    pub(crate) open: Option<OpenRoom>,
    pub(crate) focus: Focus,
    pub(crate) members_shown: bool,
    /// Each room's notifying unread count when last seen, for the status
    /// line's notices.
    unread_seen: HashMap<OwnedRoomId, u64>,
    saves: Vec<(String, MediaReply<()>)>,
    /// The timeline pane's height at the last frame.
    pub(crate) timeline_height: usize,
    /// The list pane's height at the last frame.
    pub(crate) list_height: usize,
    /// The account's messaging actions (ADR 0012), and what they report.
    pub(crate) actions: Option<Actions>,
    pub(crate) action_events: Option<mpsc::UnboundedReceiver<ActionEvent>>,
    /// The account's encryption (ADR 0009).
    pub(crate) encryption: EncryptionState,
    /// When the open room was last marked as read.
    pub(crate) last_read: Option<Instant>,
}

impl RoomsState {
    /// Shows `filter`; `true` when the list took it.
    pub(crate) fn controls_set_filter(&mut self, filter: RoomListFilter) -> bool {
        if self
            .controls
            .as_ref()
            .is_some_and(|controls| controls.set_filter(filter))
        {
            self.filter = filter;
            true
        } else {
            false
        }
    }

    /// Whether the room has the keys.
    pub(crate) fn room_focused(&self) -> bool {
        self.focus != Focus::List && self.open.is_some()
    }

    /// Answers asked for that have not arrived.
    pub(crate) fn pending(&self) -> usize {
        let open = self.open.as_ref().map_or(0, |open| {
            usize::from(open.reply.is_some())
                + usize::from(open.members_reply.is_some())
                + usize::from(!open.loaded && open.error.is_none() && !open.room.is_invite())
                + usize::from(open.pagination == PaginationState::Loading)
                + usize::from(open.warnings_reply.is_some())
                + usize::from(open.acknowledging.is_some())
                + open.composer.as_ref().map_or(0, Composer::pending)
        });
        usize::from(self.list_reply.is_some()) + open + self.saves.len() + self.encryption.pending()
    }

    /// The room under the list's cursor.
    pub(crate) fn current(&self) -> Option<&RoomSummary> {
        self.rooms.get(self.cursor)
    }

    /// Whether the sync banner offers Try again.
    pub(crate) fn can_retry(&self) -> bool {
        matches!(
            self.status.sync,
            SyncState::Offline { .. }
                | SyncState::Error { .. }
                | SyncState::NotSupported
                | SyncState::SignedOut { .. }
        )
    }
}

fn poll<T>(reply: &mut Option<oneshot::Receiver<T>>) -> Option<Option<T>> {
    let receiver = reply.as_mut()?;
    match receiver.try_recv() {
        Ok(value) => {
            *reply = None;
            Some(Some(value))
        }
        Err(oneshot::error::TryRecvError::Empty) => None,
        Err(oneshot::error::TryRecvError::Closed) => {
            *reply = None;
            Some(None)
        }
    }
}

/// `~/x` and `~` as the home folder.
fn expand_home(input: &str) -> PathBuf {
    let input = input.trim();
    if let Some(home) = std::env::home_dir() {
        if input == "~" {
            return home;
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

impl App {
    /// The rooms of `account`, opening its room list.
    pub(crate) fn rooms_screen(&self, account: AccountView) -> Screen {
        let session = self.opened().map(|opened| opened.session.clone());
        let status_rx = session
            .as_ref()
            .and_then(|session| session.account_status(&account.key));
        let status = status_rx
            .as_ref()
            .map(|status| status.borrow().clone())
            .unwrap_or_default();
        let list_reply = session
            .as_ref()
            .map(|session| session.room_list(&account.key, RoomListFilter::All));
        let (actions, action_events) = match &session {
            Some(session) => {
                let (actions, events) = Actions::channel(session.clone(), account.key.clone());
                (Some(actions), Some(events))
            }
            None => (None, None),
        };
        let encryption = EncryptionState::new(session.as_deref(), &account.key);
        Screen::Rooms(Box::new(RoomsState {
            account,
            status_rx,
            status,
            list_reply,
            updates: None,
            loading_rx: None,
            controls: None,
            rooms: Vec::new(),
            loading: RoomListLoading::NotLoaded,
            list_error: None,
            filter: RoomListFilter::All,
            cursor: 0,
            open: None,
            focus: Focus::List,
            members_shown: false,
            unread_seen: HashMap::new(),
            saves: Vec::new(),
            timeline_height: 10,
            list_height: 10,
            actions,
            action_events,
            encryption,
            last_read: None,
        }))
    }

    pub(crate) fn rooms_state(&mut self) -> Option<&mut RoomsState> {
        match &mut self.screen {
            Screen::Rooms(rooms) => Some(rooms),
            _ => None,
        }
    }

    pub(crate) fn rooms_ref(&self) -> Option<&RoomsState> {
        match &self.screen {
            Screen::Rooms(rooms) => Some(rooms),
            _ => None,
        }
    }

    /// Whether the room list and the room sit side by side.
    pub(crate) fn side_by_side(&self) -> bool {
        self.size.0 >= SIDE_BY_SIDE_FROM
    }

    /// Polls the room list, the open room and the saves; `true` when
    /// anything arrived.
    pub(crate) fn rooms_tick(&mut self) -> bool {
        let bell = self.settings.bell;
        let Some(rooms) = self.rooms_state() else {
            return false;
        };
        let mut changed = false;
        if let Some(status) = rooms.status_rx.as_mut()
            && status.has_changed().unwrap_or(false)
        {
            rooms.status = status.borrow_and_update().clone();
            changed = true;
        }
        match poll(&mut rooms.list_reply) {
            Some(Some(Ok(subscription))) => {
                let (updates, loading, controls) = subscription.split();
                rooms.loading = *loading.borrow();
                rooms.updates = Some(updates);
                rooms.loading_rx = Some(loading);
                rooms.controls = Some(controls);
                changed = true;
            }
            Some(Some(Err(error))) => {
                rooms.list_error = Some(sanitize_line(&error.to_string()));
                changed = true;
            }
            Some(None) => {
                rooms.list_error = Some(SessionError::ShutDown.to_string());
                changed = true;
            }
            None => {}
        }
        if let Some(loading) = rooms.loading_rx.as_mut()
            && loading.has_changed().unwrap_or(false)
        {
            rooms.loading = *loading.borrow_and_update();
            changed = true;
        }
        let mut batches = Vec::new();
        if let Some(updates) = rooms.updates.as_mut() {
            while let Ok(batch) = updates.try_recv() {
                batches.push(batch);
            }
        }
        let mut notice = None;
        if !batches.is_empty() {
            changed = true;
            let selected = rooms.current().map(|room| room.room_id.clone());
            for batch in batches {
                if apply_all(&mut rooms.rooms, batch).is_err() {
                    tracing::warn!("a room list change was out of range; waiting for the next");
                }
            }
            // The list may have loaded before its loading state said so.
            if rooms.loading == RoomListLoading::NotLoaded && !rooms.rooms.is_empty() {
                rooms.loading = RoomListLoading::Loaded { total: None };
            }
            if let Some(selected) = selected
                && let Some(index) = rooms.rooms.iter().position(|room| room.room_id == selected)
            {
                rooms.cursor = index;
            }
            rooms.cursor = rooms.cursor.min(rooms.rooms.len().saturating_sub(1));
            if let Some(open) = rooms.open.as_mut()
                && let Some(summary) = rooms
                    .rooms
                    .iter()
                    .find(|room| room.room_id == open.room.room_id)
            {
                open.room = summary.clone();
            }
            notice = new_messages(rooms);
        }
        if let Some(open) = rooms.open.as_mut() {
            changed |= open_room_tick(open);
        }
        changed |= self.writing_tick();
        changed |= self.encryption_tick();
        let Some(rooms) = self.rooms_state() else {
            return changed;
        };
        let mut saved = Vec::new();
        rooms
            .saves
            .retain_mut(|(name, reply)| match reply.try_recv() {
                Ok(result) => {
                    saved.push((name.clone(), result));
                    false
                }
                Err(oneshot::error::TryRecvError::Empty) => true,
                Err(oneshot::error::TryRecvError::Closed) => {
                    saved.push((name.clone(), Err(MediaError::Unavailable)));
                    false
                }
            });
        for (name, result) in saved {
            changed = true;
            self.status = Some(match result {
                Ok(()) => format!("Saved {name}."),
                Err(error) => sanitize_line(&error.to_string()),
            });
        }
        if let Some(notice) = notice {
            self.status = Some(notice);
            if bell {
                self.effects.push(Effect::Bell);
            }
        }
        // Side by side, the room under the cursor shows beside the list.
        if self.side_by_side()
            && let Some(rooms) = self.rooms_state()
            && rooms.open.is_none()
            && !rooms.rooms.is_empty()
        {
            let cursor = rooms.cursor;
            self.open_room(cursor, false);
            changed = true;
        }
        changed
    }

    /// The fixture's first rooms view, a step at a time; what is left.
    pub(crate) fn launch_rooms(&mut self, mut launch: RoomsLaunch) -> Option<RoomsLaunch> {
        let Some(rooms) = self.rooms_state() else {
            return Some(launch);
        };
        if rooms.controls.is_none() && rooms.list_error.is_none() {
            return Some(launch);
        }
        if launch.filter != rooms.filter {
            self.rooms_run(match launch.filter {
                RoomListFilter::All => Cmd::ShowAllRooms,
                RoomListFilter::Unread => Cmd::ShowUnreadRooms,
                RoomListFilter::People => Cmd::ShowPeople,
                RoomListFilter::Invites => Cmd::ShowInvites,
            });
            return Some(launch);
        }
        if let Some(room) = launch.room.clone() {
            let Some(index) = rooms
                .rooms
                .iter()
                .position(|summary| summary.room_id.as_str() == room)
            else {
                return Some(launch);
            };
            self.open_room(index, true);
            if launch.members {
                self.rooms_run(Cmd::ToggleMemberList);
            }
            if launch.spoilers {
                self.rooms_run(Cmd::Spoilers);
            }
            launch.room = None;
        }
        if launch.reference.is_some() || launch.save.is_some() {
            let refs = self
                .rooms_ref()
                .and_then(|rooms| rooms.open.as_ref())
                .and_then(|open| open.layout.as_ref())
                .map_or(0, |layout| layout.refs.len());
            if refs == 0 {
                return Some(launch);
            }
            if let Some(number) = launch.reference.take() {
                self.open_reference(number.saturating_sub(1));
            }
            if let Some(number) = launch.save.take() {
                self.open_reference(number.saturating_sub(1));
            }
            return None;
        }
        // The writing and encryption views: one step a tick, each once the
        // answers to the last have arrived.
        if self.pending_work() > 0 {
            return Some(launch);
        }
        if std::mem::take(&mut launch.select_own) {
            let loaded = self
                .rooms_ref()
                .and_then(|rooms| rooms.open.as_ref())
                .is_some_and(|open| open.loaded);
            if !loaded {
                launch.select_own = true;
                return Some(launch);
            }
            if let Some(open) = self.rooms_state().and_then(|rooms| rooms.open.as_mut()) {
                open.selected_item = crate::writing::actionable(open)
                    .into_iter()
                    .rev()
                    .find(|choice| choice.own)
                    .map(|choice| choice.target.item);
                open.reveal_selected = true;
                open.generation += 1;
            }
            if let Some(rooms) = self.rooms_state() {
                rooms.focus = Focus::Room;
            }
            return Some(launch);
        }
        if let Some((name, size)) = launch.file.take() {
            self.add_file(familychat_actions::FileSource::Bytes {
                name,
                data: std::sync::Arc::from(vec![0_u8; size]),
            });
            return Some(launch);
        }
        if !launch.commands.is_empty() {
            let cmd = launch.commands.remove(0);
            self.run(cmd);
            return Some(launch);
        }
        if let Some(typed) = launch.typed.take() {
            for ch in typed.chars() {
                let key = if ch == '\r' {
                    crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Enter,
                        crossterm::event::KeyModifiers::NONE,
                    )
                } else if ch == '\n' {
                    crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Char('j'),
                        crossterm::event::KeyModifiers::CONTROL,
                    )
                } else {
                    crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Char(ch),
                        crossterm::event::KeyModifiers::NONE,
                    )
                };
                self.handle_event(crossterm::event::Event::Key(key));
            }
        }
        None
    }

    /// Opens the room at `index` of the list, and gives it the keys when
    /// `focus`.
    pub(crate) fn open_room(&mut self, index: usize, focus: bool) {
        let session = self.opened().map(|opened| opened.session.clone());
        let tasks = self.tasks.clone();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let Some(room) = rooms.rooms.get(index).cloned() else {
            return;
        };
        rooms.cursor = index;
        // The composer has the keys in a room that can be written in
        // (PLAN.md decision 10); otherwise the room's commands do.
        let writable = familychat_actions::ComposerState::of(&rooms.status, &room)
            == familychat_actions::ComposerState::Writable;
        let same = rooms.open.as_ref().is_some_and(|open| {
            open.room.room_id == room.room_id && open.room.is_invite() == room.is_invite()
        });
        if focus {
            rooms.focus = if writable {
                Focus::Composer
            } else {
                Focus::Room
            };
        }
        if same {
            return;
        }
        // The draft of the room being left is kept now.
        if let Some(composer) = rooms.open.as_mut().and_then(|open| open.composer.as_mut())
            && let Some(draft) = composer.draft_now()
            && let Some(actions) = &rooms.actions
        {
            tasks.run(actions.save_draft(&composer.room, draft));
        }
        if let Some((typing, room_id)) = rooms
            .open
            .as_mut()
            .and_then(|open| open.composer.as_mut())
            .and_then(|composer| Some((composer.typing.stopped()?, composer.room.clone())))
            && let Some(actions) = &rooms.actions
        {
            tasks.run(actions.typing(&room_id, typing));
        }
        let reply = (!room.is_invite())
            .then(|| {
                session
                    .as_ref()
                    .map(|session| session.timeline(&rooms.account.key, &room.room_id))
            })
            .flatten();
        let members_reply = rooms
            .members_shown
            .then(|| {
                session
                    .as_ref()
                    .map(|session| session.members(&rooms.account.key, &room.room_id))
            })
            .flatten();
        let members = if members_reply.is_some() {
            MembersState::Loading
        } else {
            MembersState::NotAsked
        };
        let composer = (!room.is_invite()).then(|| {
            let reply = session
                .as_ref()
                .map(|session| session.members(&rooms.account.key, &room.room_id));
            Composer::new(room.room_id.clone(), reply)
        });
        if composer.is_some()
            && let Some(actions) = &rooms.actions
        {
            tasks.run(actions.load_draft(&room.room_id));
        }
        let warnings_reply = (!room.is_invite())
            .then(|| {
                session
                    .as_ref()
                    .map(|session| session.identity_warnings(&rooms.account.key, &room.room_id))
            })
            .flatten();
        rooms.open = Some(OpenRoom {
            room,
            reply,
            updates: None,
            controls: None,
            items: Vec::new(),
            loaded: false,
            error: None,
            pagination: PaginationState::Idle,
            members,
            members_reply,
            from_bottom: 0,
            selected_ref: None,
            spoilers: false,
            generation: 0,
            layout: None,
            composer,
            selected_item: None,
            reveal_selected: false,
            warnings_reply,
            warnings_rx: None,
            warnings: Vec::new(),
            acknowledging: None,
        });
        self.selection = None;
    }

    /// Moves the list's cursor by `by` rows; in the side-by-side layout
    /// the room follows.
    fn move_cursor(&mut self, by: isize) {
        let side_by_side = self.side_by_side();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        if rooms.rooms.is_empty() {
            return;
        }
        let last = rooms.rooms.len() - 1;
        let cursor = rooms.cursor.saturating_add_signed(by).min(last);
        rooms.cursor = cursor;
        self.reveal.insert(crate::pane::PaneId::Rooms);
        if side_by_side {
            self.open_room(cursor, false);
        }
    }

    /// The next or previous room (with unread messages, when `unread`),
    /// opened.
    fn step_room(&mut self, forward: bool, unread: bool) {
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let count = rooms.rooms.len();
        if count == 0 {
            return;
        }
        let start = rooms.cursor;
        let found = (1..=count)
            .map(|offset| {
                if forward {
                    (start + offset) % count
                } else {
                    (start + count - offset % count) % count
                }
            })
            .filter(|&index| !unread || index != start)
            .find(|&index| !unread || rooms.rooms[index].unread.is_unread());
        let found = if unread {
            found
        } else {
            // Next and previous stop at the ends of the list.
            let next = if forward {
                (start + 1).min(count - 1)
            } else {
                start.saturating_sub(1)
            };
            Some(next)
        };
        match found {
            Some(index) => {
                self.reveal.insert(crate::pane::PaneId::Rooms);
                self.open_room(index, true);
            }
            None => self.status = Some("No more unread rooms.".to_owned()),
        }
    }

    /// Scrolls the open room's timeline `by` lines towards older ones (a
    /// negative `by` goes towards the newest).
    pub(crate) fn scroll_timeline(&mut self, by: isize) {
        let Some(open) = self.rooms_state().and_then(|rooms| rooms.open.as_mut()) else {
            return;
        };
        let total = open.layout.as_ref().map_or(0, |layout| layout.lines.len());
        open.from_bottom = open.from_bottom.saturating_add_signed(by).min(total);
    }

    /// A command in the rooms.
    pub(crate) fn rooms_run(&mut self, cmd: Cmd) {
        if self.writing_run(cmd) || (cmd == Cmd::TryAgain && self.retry_identity()) {
            return;
        }
        let session = self.opened().map(|opened| opened.session.clone());
        let side_by_side = self.side_by_side();
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let focused = rooms.room_focused();
        let page = if focused {
            rooms.timeline_height.saturating_sub(2).max(1)
        } else {
            (rooms.list_height / 2).max(1)
        };
        match cmd {
            Cmd::NextRoom => self.step_room(true, false),
            Cmd::PreviousRoom => self.step_room(false, false),
            Cmd::NextUnreadRoom => self.step_room(true, true),
            Cmd::PreviousUnreadRoom => self.step_room(false, true),
            Cmd::ShowAllRooms | Cmd::ShowUnreadRooms | Cmd::ShowPeople | Cmd::ShowInvites => {
                let filter = match cmd {
                    Cmd::ShowUnreadRooms => RoomListFilter::Unread,
                    Cmd::ShowPeople => RoomListFilter::People,
                    Cmd::ShowInvites => RoomListFilter::Invites,
                    _ => RoomListFilter::All,
                };
                if filter != rooms.filter
                    && rooms
                        .controls
                        .as_ref()
                        .is_some_and(|controls| controls.set_filter(filter))
                {
                    rooms.filter = filter;
                    rooms.cursor = 0;
                    rooms.focus = Focus::List;
                    self.scroll.insert(crate::pane::PaneId::Rooms, 0);
                }
            }
            Cmd::ToggleMemberList => {
                rooms.members_shown = !rooms.members_shown;
                if rooms.members_shown
                    && let Some(open) = rooms.open.as_mut()
                    && open.members == MembersState::NotAsked
                    && let Some(session) = &session
                {
                    open.members_reply =
                        Some(session.members(&rooms.account.key, &open.room.room_id));
                    open.members = MembersState::Loading;
                }
            }
            Cmd::JumpToLatest => {
                if let Some(open) = rooms.open.as_mut() {
                    open.from_bottom = 0;
                }
            }
            Cmd::JumpToOldestUnread => {
                let height = rooms.timeline_height;
                let target = rooms.open.as_mut().and_then(|open| {
                    let layout = open.layout.as_ref()?;
                    let marker = layout.read_marker?;
                    let total = layout.lines.len();
                    Some((open, total, marker))
                });
                match target {
                    Some((open, total, marker)) => {
                        // The marker at the top of the pane.
                        open.from_bottom = total.saturating_sub(marker + height.max(1));
                    }
                    None => self.status = Some("There are no unread messages here.".to_owned()),
                }
            }
            Cmd::Select => match rooms.focus {
                Focus::List => {
                    let cursor = rooms.cursor;
                    self.open_room(cursor, true);
                }
                Focus::Room | Focus::Composer => {
                    if let Some(selected) = rooms.open.as_ref().and_then(|open| open.selected_ref) {
                        self.open_reference(selected);
                    }
                }
            },
            Cmd::Back | Cmd::GoBack => {
                // A selected message is let go first.
                if let Some(open) = rooms.open.as_mut()
                    && open.selected_item.take().is_some()
                {
                    open.generation += 1;
                    return;
                }
                if let Some(open) = rooms.open.as_mut() {
                    open.selected_ref = None;
                }
                rooms.focus = Focus::List;
                self.reveal.insert(crate::pane::PaneId::Rooms);
            }
            Cmd::Up | Cmd::Down | Cmd::PageUp | Cmd::PageDown => {
                let up = matches!(cmd, Cmd::Up | Cmd::PageUp);
                let by = if matches!(cmd, Cmd::Up | Cmd::Down) {
                    1
                } else {
                    page
                } as isize;
                if focused {
                    self.scroll_timeline(if up { by } else { -by });
                } else {
                    self.move_cursor(if up { -by } else { by });
                }
            }
            Cmd::NextRef | Cmd::PrevRef => self.step_reference(cmd == Cmd::NextRef),
            Cmd::OpenRef => {
                let refs = rooms
                    .open
                    .as_ref()
                    .and_then(|open| open.layout.as_ref())
                    .map_or(0, |layout| layout.refs.len());
                if refs == 0 {
                    self.status = Some("There are no links or files here.".to_owned());
                } else {
                    self.prompt = Some(Prompt::RefNumber(String::new()));
                }
            }
            Cmd::SaveFile => {
                let file = rooms.open.as_ref().and_then(|open| {
                    let layout = open.layout.as_ref()?;
                    match layout.refs.get(open.selected_ref?)? {
                        Reference::File(media) => Some(media.clone()),
                        Reference::Link { .. } => None,
                    }
                });
                match file {
                    Some(media) => self.ask_save(media),
                    None => {
                        self.status = Some(
                            "Choose a file first: Tab moves to the next link or file.".to_owned(),
                        );
                    }
                }
            }
            Cmd::TryAgain => {
                if rooms.can_retry()
                    && let Some(session) = &session
                {
                    session.retry_sync(&rooms.account.key);
                    self.status = Some("Trying again…".to_owned());
                }
            }
            Cmd::Spoilers => {
                if let Some(open) = rooms.open.as_mut() {
                    open.spoilers = !open.spoilers;
                }
            }
            _ => {}
        }
        // The side-by-side layout keeps a room open beside the list.
        if side_by_side
            && let Some(rooms) = self.rooms_state()
            && rooms.open.is_none()
            && !rooms.rooms.is_empty()
        {
            let cursor = rooms.cursor;
            self.open_room(cursor, false);
        }
    }

    /// Tab and Shift-Tab: the next or previous link or file.
    fn step_reference(&mut self, forward: bool) {
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let height = rooms.timeline_height.max(1);
        let Some(open) = rooms.open.as_mut() else {
            return;
        };
        let Some(layout) = open.layout.as_ref() else {
            return;
        };
        let count = layout.refs.len();
        if count == 0 {
            self.status = Some("There are no links or files here.".to_owned());
            return;
        }
        rooms.focus = Focus::Room;
        let next = match open.selected_ref {
            // The first Tab picks the newest: the bottom of the room.
            None if forward => count - 1,
            None => count - 1,
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
        };
        open.selected_ref = Some(next);
        // Bring it into view.
        let total = layout.lines.len();
        let line = layout.ref_lines.get(next).copied().unwrap_or(0);
        let top = total.saturating_sub(height + open.from_bottom);
        if line < top || line >= top + height {
            open.from_bottom = total.saturating_sub(line + height / 2 + 1);
        }
        let status = match &layout.refs[next] {
            Reference::Link { href, .. } => {
                format!(
                    "Link [{}]: {}  (Enter opens it)",
                    next + 1,
                    sanitize_line(href)
                )
            }
            Reference::File(media) => format!(
                "File [{}]: {}  (s or Enter saves it)",
                next + 1,
                sanitize_line(&media.filename)
            ),
        };
        self.status = Some(status);
    }

    /// Opens reference `index` of the open room: a link through
    /// `links::classify`, a file through the save question.
    pub(crate) fn open_reference(&mut self, index: usize) {
        let Some(open) = self.rooms_state().and_then(|rooms| rooms.open.as_mut()) else {
            return;
        };
        let Some(reference) = open
            .layout
            .as_ref()
            .and_then(|layout| layout.refs.get(index))
            .cloned()
        else {
            self.status = Some(format!("There is no link or file {}.", index + 1));
            return;
        };
        open.selected_ref = Some(index);
        match reference {
            Reference::File(media) => self.ask_save(media),
            Reference::Link { href, text } => match classify(&href, &text) {
                LinkAction::Navigate(link) => self.navigate(link),
                LinkAction::Confirm(prompt) => self.prompt = Some(Prompt::Link(prompt)),
                LinkAction::Refuse => {
                    self.status = Some("Family Chat doesn't open this kind of link.".to_owned());
                }
            },
        }
    }

    /// A Matrix link: a room opens here, a person is named.
    fn navigate(&mut self, link: MatrixLink) {
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let room_in_list = |id: &str| {
            rooms
                .rooms
                .iter()
                .position(|room| room.room_id.as_str() == id)
        };
        match link {
            MatrixLink::Room(room) | MatrixLink::Event { room, .. } => {
                match room_in_list(room.as_str()) {
                    Some(index) => self.open_room(index, true),
                    None => {
                        self.status = Some(format!(
                            "{} isn't one of your rooms.",
                            sanitize_line(room.as_str())
                        ));
                    }
                }
            }
            MatrixLink::User(user) => {
                let member = rooms.open.as_ref().and_then(|open| match &open.members {
                    MembersState::Ready(members) => members
                        .iter()
                        .find(|member| member.user_id == user)
                        .map(RoomMember::name),
                    _ => None,
                });
                let who = match member {
                    Some(name) => format!("{} ({user})", sanitize_line(&name)),
                    None => user.to_string(),
                };
                self.status = Some(format!("That's {who}."));
            }
        }
    }

    /// "Save NAME to:", starting in the Downloads folder with the safe
    /// name.
    fn ask_save(&mut self, media: MediaInfo) {
        let name = sanitize_filename(&media.filename);
        let input = self.downloads.join(name).display().to_string();
        self.prompt = Some(Prompt::Save { media, input });
    }

    /// The save question's answer: asks before replacing a file.
    pub(crate) fn save_to(&mut self, media: MediaInfo, input: &str) {
        if input.trim().is_empty() {
            return;
        }
        let mut path = expand_home(input);
        if path.is_dir() {
            path = path.join(sanitize_filename(&media.filename));
        }
        if path.exists() {
            self.prompt = Some(Prompt::Replace { media, path });
            return;
        }
        self.start_save(media, path);
    }

    /// Saves the file through the session, where the person chose.
    pub(crate) fn start_save(&mut self, media: MediaInfo, path: PathBuf) {
        let Some(session) = self.opened().map(|opened| opened.session.clone()) else {
            return;
        };
        let Some(rooms) = self.rooms_state() else {
            return;
        };
        let name = sanitize_line(
            &path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let reply = session.save_file(&rooms.account.key, &media, path);
        rooms.saves.push((name.clone(), reply));
        self.status = Some(format!("Saving {name}…"));
    }

    /// A click on row `target` of the room list.
    pub(crate) fn rooms_click(&mut self, target: usize) {
        self.open_room(target, true);
    }

    /// The command bar in the rooms: the composer's keys while it has
    /// them, the room's commands while they do, the list's otherwise.
    pub(crate) fn rooms_commands(&self, rooms: &RoomsState) -> Vec<CommandEntry> {
        let mut entries = Vec::new();
        let retry = rooms.can_retry()
            || matches!(
                rooms.encryption.status.identity,
                familychat_core::encryption::IdentitySetup::Failed(_)
            );
        let writable =
            App::composer_state(rooms) == Some(familychat_actions::ComposerState::Writable);
        let invite = rooms
            .open
            .as_ref()
            .is_some_and(|open| open.room.is_invite());
        if rooms.focus == Focus::Composer && rooms.room_focused() {
            entries.extend([
                CommandEntry::of(Cmd::Send),
                CommandEntry::of(Cmd::NewLine),
                CommandEntry::of(Cmd::ToTimeline),
            ]);
            if self.settings.external_editor {
                entries.push(CommandEntry::of(Cmd::Editor));
            }
            entries.extend([
                CommandEntry::of(Cmd::PageUp),
                CommandEntry::of(Cmd::PageDown),
                CommandEntry::of(Cmd::NextRoom),
                CommandEntry::of(Cmd::PreviousRoom),
                CommandEntry::of(Cmd::SetUpRecovery),
                CommandEntry::with("^C", "Quit", Cmd::Quit),
            ]);
            return entries;
        }
        if rooms.room_focused() {
            entries.push(CommandEntry::of(Cmd::Back));
            if invite {
                entries.extend([
                    CommandEntry::of(Cmd::Accept),
                    CommandEntry::of(Cmd::Decline),
                ]);
            } else {
                if writable {
                    entries.push(CommandEntry::of(Cmd::FocusComposer));
                }
                entries.extend([
                    CommandEntry::of(Cmd::SelectPreviousMessage),
                    CommandEntry::of(Cmd::SelectNextMessage),
                ]);
                if writable {
                    entries.extend([
                        CommandEntry::of(Cmd::ReplyToMessage),
                        CommandEntry::of(Cmd::ReactToMessage),
                    ]);
                    let selected = self.selected_choice();
                    if selected.as_ref().is_none_or(|choice| choice.editable) {
                        entries.push(CommandEntry::of(Cmd::EditMessage));
                    }
                    if selected.as_ref().is_some_and(|choice| choice.own) {
                        entries.push(CommandEntry::of(Cmd::DeleteMessage));
                    }
                    if selected.as_ref().is_some_and(|choice| {
                        choice
                            .problem
                            .is_some_and(familychat_core::timeline::SendProblem::can_send_anyway)
                    }) {
                        entries.push(CommandEntry::of(Cmd::SendAnyway));
                    }
                }
                if rooms
                    .open
                    .as_ref()
                    .is_some_and(|open| !open.warnings.is_empty())
                {
                    entries.push(CommandEntry::of(Cmd::Acknowledge));
                }
                entries.extend([
                    CommandEntry::of(Cmd::PageUp),
                    CommandEntry::of(Cmd::PageDown),
                    CommandEntry::of(Cmd::NextRef),
                    CommandEntry::of(Cmd::OpenRef),
                    CommandEntry::of(Cmd::SaveFile),
                ]);
                if writable {
                    entries.push(CommandEntry::of(Cmd::AttachFile));
                }
                entries.extend([
                    CommandEntry::of(Cmd::JumpToOldestUnread),
                    CommandEntry::of(Cmd::JumpToLatest),
                ]);
            }
            entries.push(CommandEntry::of(Cmd::ToggleMemberList));
        } else {
            entries.push(CommandEntry::with("Enter", "Open", Cmd::Select));
        }
        if retry {
            entries.insert(1, CommandEntry::of(Cmd::TryAgain));
        }
        entries.extend([
            CommandEntry::of(Cmd::NextUnreadRoom),
            CommandEntry::of(Cmd::Palette),
            CommandEntry::of(Cmd::Quit),
            CommandEntry::of(Cmd::NextRoom),
            CommandEntry::of(Cmd::PreviousRoom),
            CommandEntry::of(Cmd::PreviousUnreadRoom),
            CommandEntry::of(Cmd::ShowAllRooms),
            CommandEntry::of(Cmd::ShowUnreadRooms),
            CommandEntry::of(Cmd::ShowPeople),
            CommandEntry::of(Cmd::ShowInvites),
        ]);
        if !rooms.room_focused() {
            entries.push(CommandEntry::of(Cmd::ToggleMemberList));
        }
        if rooms.room_focused() {
            entries.push(CommandEntry::of(Cmd::Spoilers));
            if rooms.open.is_some() {
                entries.push(CommandEntry::of(Cmd::LeaveRoom));
            }
        }
        entries.extend([
            CommandEntry::of(Cmd::ConfirmIdentity),
            CommandEntry::of(Cmd::SetUpRecovery),
            CommandEntry::of(Cmd::Copy),
            CommandEntry::of(Cmd::Bell),
        ]);
        entries
    }
}

/// Polls the open room's timeline and members; `true` when anything
/// arrived.
fn open_room_tick(open: &mut OpenRoom) -> bool {
    let mut changed = false;
    match poll(&mut open.reply) {
        Some(Some(Ok(subscription))) => {
            let (updates, controls) = subscription.split();
            open.updates = Some(updates);
            open.controls = Some(controls);
            changed = true;
        }
        Some(Some(Err(error))) => {
            open.error = Some(sanitize_line(&error.to_string()));
            changed = true;
        }
        Some(None) => {
            open.error = Some(SessionError::ShutDown.to_string());
            changed = true;
        }
        None => {}
    }
    if let Some(updates) = open.updates.as_mut() {
        while let Ok(update) = updates.try_recv() {
            changed = true;
            match update {
                TimelineUpdate::Items(batch) => {
                    if apply_all(&mut open.items, batch).is_err() {
                        tracing::warn!("a timeline change was out of range; waiting for the next");
                    }
                    open.loaded = true;
                    open.generation += 1;
                }
                TimelineUpdate::Pagination(state) => {
                    // The layout shows it ("Loading older messages…").
                    open.pagination = state;
                    open.generation += 1;
                }
            }
        }
    }
    match poll(&mut open.warnings_reply) {
        Some(Some(Ok(warnings))) => {
            open.warnings = warnings.borrow().clone();
            open.warnings_rx = Some(warnings);
            changed = true;
        }
        Some(_) => changed = true,
        None => {}
    }
    if let Some(warnings) = open.warnings_rx.as_mut()
        && warnings.has_changed().unwrap_or(false)
    {
        open.warnings = warnings.borrow_and_update().clone();
        changed = true;
    }
    if let Some(composer) = open.composer.as_mut() {
        changed |= composer.poll();
    }
    match poll(&mut open.members_reply) {
        Some(Some(Ok(mut members))) => {
            sort_members(&mut members);
            open.members = MembersState::Ready(members);
            changed = true;
        }
        Some(Some(Err(error))) => {
            open.members = MembersState::Failed(sanitize_line(&error.to_string()));
            changed = true;
        }
        Some(None) => {
            open.members = MembersState::Failed(SessionError::ShutDown.to_string());
            changed = true;
        }
        None => {}
    }
    changed
}

/// Asks for older messages when the top of the timeline is on screen.
pub(crate) fn paginate_if_at_top(open: &mut OpenRoom, top: usize) {
    if top == 0
        && open.loaded
        && open.pagination == PaginationState::Idle
        && open
            .controls
            .as_ref()
            .is_some_and(|controls| controls.paginate_backwards(PAGE_EVENTS))
    {
        open.pagination = PaginationState::Loading;
    }
}

/// The status line's notice for rooms whose notifying unread count went up
/// since the list last showed them (not the room being read), if any.
fn new_messages(rooms: &mut RoomsState) -> Option<String> {
    let reading = rooms
        .open
        .as_ref()
        .filter(|_| rooms.focus == Focus::Room)
        .map(|open| open.room.room_id.clone());
    let mut news = Vec::new();
    for room in &rooms.rooms {
        let count = room.unread.notifications;
        let before = rooms.unread_seen.insert(room.room_id.clone(), count);
        if let Some(before) = before
            && count > before
            && reading.as_ref() != Some(&room.room_id)
        {
            news.push(room);
        }
    }
    match news.as_slice() {
        [] => None,
        [room] => {
            let from = room
                .preview
                .as_ref()
                .filter(|preview| !preview.is_own)
                .map(|preview| format!(" from {}", sanitize_line(&preview.sender)))
                .unwrap_or_default();
            Some(format!(
                "New message in {}{from}",
                sanitize_line(&room.name)
            ))
        }
        many => Some(format!("New messages in {} rooms", many.len())),
    }
}
