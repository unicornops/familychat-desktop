//! `RoomsView`: the signed-in window's body (PLAN.md, section 13.6): the
//! room list with its filters, unread and mention badges and invitations;
//! the open room's header with its encryption state, its timeline and its
//! member list; and the dialog every link outside Matrix opens.
//!
//! Everything arrives through `SessionApi`: the room list as `ListDiff`
//! batches applied to the view's own list (bursts coalesced into one
//! redraw), each room's timeline in its [`TimelineView`], members and
//! invitation answers as one-off replies. Nothing here talks to a server.
//!
//! Links: a `matrix:` or `matrix.to` link opens in the app (the room, the
//! event, or who the person is); any other address shows the whole
//! address and asks before the operating system opens it, saying so when
//! the link's text shows another site (`links::classify`; the dialog is
//! adapted from Zmail's `zmail-ui`, `src/reader/links.rs`). Files are
//! saved where the person chooses, under the safe name the session made.
//!
//! The layout follows the window: below [`NARROW_BELOW`] it shows the list
//! or the room, with a back button; from [`MEMBERS_BESIDE_FROM`] the member
//! list sits beside the timeline, and below it covers the timeline.
//!
//! Taking part (Phase 7, ADR 0012): the open room has a [`ComposerView`]
//! under its timeline, and everything the person does (send, reply, edit,
//! react, delete, files, read receipts, typing, invitations, leaving) goes
//! through one `familychat-actions` [`Actions`], whose events come back
//! over a channel and are drawn here: a refused or failed send in the
//! composer, anything else as a toast. Deleting a message and leaving a
//! room ask first. While the account is paused the composer is read-only.

mod composer;
mod media;
mod members;
pub(crate) mod rich_text;
mod timeline;

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use familychat_actions::{Action, ActionEvent, Actions, ComposerState, FileSource, Refusal};
use familychat_core::{
    OwnedRoomId,
    encryption::{IdentityWarning, IdentityWarningKind},
    links::{LinkAction, LinkPrompt, MatrixLink, classify},
    media::MediaInfo,
    rooms::{Encryption, RoomListFilter, RoomListLoading, RoomSummary},
    session::{AccountView, RoomListControls, SessionApi, SessionError},
    sync::AccountStatus,
    timeline::time_label,
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, IntoElement, Render, Role, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

pub(crate) use self::composer::{ComposerEvent, ComposerView, Target};
pub(crate) use self::media::ImageCache;
use self::members::{MEMBERS_WIDTH, MembersState, render_members};
use self::timeline::MessageChoice;
pub(crate) use self::timeline::TimelineView;
pub(crate) use self::timeline::TimelineViewEvent;
use self::timeline::encryption_notice;
use crate::a11y::{A11yFlags, AccessibleExt, readable};
use crate::actions::{
    AttachFile, DeleteMessage, EditMessage, FocusComposer, JumpToLatest, JumpToOldestUnread,
    LeaveRoom, NextRoom, NextUnreadRoom, PreviousRoom, PreviousUnreadRoom, ReactToMessage,
    ReplyToMessage, SelectNextMessage, SelectPreviousMessage, ShowAllRooms, ShowInvites,
    ShowPeople, ShowUnreadRooms, ToggleMemberList,
};
use crate::components::actions::Cancel;
use crate::components::tooltip::Tooltip;
use crate::components::{
    Avatar, Badge, BadgeTone, Banner, BannerTone, Button, ButtonSize, ComponentTheme, HitTargetExt,
    IconButton, Modal, ModalState, Scrollbar, Toast, ToastHost, ToastKind,
};
use crate::signin::{primary_key, spinner};
use crate::styles::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE, TypeStyleExt, UiColors,
};
use crate::widgets::{empty_state, icon};

/// Key context of the rooms view.
pub(crate) const KEY_CONTEXT: &str = "Rooms";
/// Below this width the window shows the list or the room, not both.
pub(crate) const NARROW_BELOW: f32 = 760.0;
/// From this width the member list sits beside the timeline.
pub(crate) const MEMBERS_BESIDE_FROM: f32 = 1180.0;
/// The room list's width beside a room.
pub(crate) const ROOM_LIST_WIDTH: f32 = 300.0;
/// A room list row's height.
const ROOM_ROW_HEIGHT: f32 = 64.0;

/// Where the window opens the rooms, for `familychat launch-ui-fixture` and
/// the view tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomsLaunch {
    /// The filter to show.
    pub filter: RoomListFilter,
    /// The room to open, by id.
    pub room: Option<String>,
    /// Open the member list.
    pub members: bool,
    /// Open the link dialog for this address and link text.
    pub link: Option<(String, String)>,
    /// Open every folded run of room changes.
    pub expand_changes: bool,
    /// The composer's state to show (the screenshot matrix).
    pub composer: Option<ComposerLaunch>,
    /// Select the newest message and open its quick reaction picker.
    pub message_actions: bool,
    /// Ask about deleting the account's newest message.
    pub delete_prompt: bool,
    /// An encryption screen or dialog to open (ADR 0009).
    pub encryption: Option<crate::EncryptionLaunch>,
}

/// A composer state to open on (`familychat launch-ui-fixture`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerLaunch {
    /// This text typed.
    Text(String),
    /// Replying to the newest message from someone else.
    Reply,
    /// Editing the account's newest message.
    Edit,
    /// This text typed, ending in an `@` query: the mention list shows.
    Mention(String),
    /// A file waiting to be sent.
    Attachment,
    /// A file over the server's limit was refused.
    TooLarge,
}

/// A room came in front of the person (the shell marks its
/// notifications seen when the window has focus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoomOpened(pub(crate) OwnedRoomId);

/// The open room.
struct OpenRoom {
    timeline: Entity<TimelineView>,
    composer: Entity<ComposerView>,
    _events: Subscription,
    _composer_events: Subscription,
}

/// The signed-in window's body.
pub(crate) struct RoomsView {
    session: Arc<dyn SessionApi>,
    account: AccountView,
    toasts: Entity<ToastHost>,
    focus_handle: FocusHandle,
    filter: RoomListFilter,
    rooms: Vec<RoomSummary>,
    loading: RoomListLoading,
    loaded: bool,
    list_error: Option<String>,
    controls: Option<RoomListControls>,
    list_scroll: UniformListScrollHandle,
    selected: Option<OwnedRoomId>,
    open: Option<OpenRoom>,
    /// On a narrow window: the room shows instead of the list.
    room_in_front: bool,
    members_open: bool,
    members: Option<(OwnedRoomId, MembersState)>,
    images: Entity<ImageCache>,
    link_prompt: Option<LinkPrompt>,
    link_modal: ModalState,
    launch: Option<RoomsLaunch>,
    /// A room a notification opened that the list doesn't hold.
    notified: Option<RoomSummary>,
    answering: Vec<OwnedRoomId>,
    actions: Actions,
    status: AccountStatus,
    /// What the launch still has to do once the room's timeline loads.
    pending_launch: Option<RoomsLaunch>,
    delete_prompt: Option<(OwnedRoomId, MessageChoice)>,
    delete_modal: ModalState,
    leave_prompt: Option<RoomSummary>,
    leave_modal: ModalState,
    /// When the open room was last marked read.
    last_read: Option<std::time::Instant>,
    /// The open room's identity warnings (ADR 0009).
    identity_warnings: Vec<IdentityWarning>,
    warnings_task: Task<()>,
    /// Rooms this window made encrypted whose `m.room.encryption` state
    /// hasn't reached the list yet: no "Not encrypted" banner meanwhile.
    made_encrypted: HashSet<OwnedRoomId>,
    _status_task: Task<()>,
    _list_task: Task<()>,
    _action_events: Task<()>,
    _loading_task: Task<()>,
    members_task: Task<()>,
    action_task: Task<()>,
}

impl EventEmitter<RoomOpened> for RoomsView {}

impl RoomsView {
    /// The rooms of `account`.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountView,
        toasts: Entity<ToastHost>,
        launch: Option<RoomsLaunch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = launch
            .as_ref()
            .map(|launch| launch.filter)
            .unwrap_or_default();
        let images = cx.new(|_| ImageCache::new(session.clone(), account.key.clone()));
        let (actions, mut action_events) = Actions::channel(session.clone(), account.key.clone());
        let action_events = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = action_events.recv().await {
                let mut events = vec![first];
                while let Ok(more) = action_events.try_recv() {
                    events.push(more);
                }
                if this
                    .update_in(cx, |this, window, cx| {
                        this.action_events(events, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let status_receiver = session.account_status(&account.key);
        let status = status_receiver
            .as_ref()
            .map(|receiver| receiver.borrow().clone())
            .unwrap_or_default();
        let status_task = match status_receiver {
            Some(mut receiver) => cx.spawn_in(window, async move |this, cx| {
                while receiver.changed().await.is_ok() {
                    let status = receiver.borrow_and_update().clone();
                    if this
                        .update_in(cx, |this, window, cx| this.set_status(status, window, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }),
            None => Task::ready(()),
        };
        let reply = session.room_list(&account.key, filter);
        let list_task = cx.spawn_in(window, async move |this, cx| {
            let subscription = match reply.await {
                Ok(Ok(subscription)) => subscription,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.list_error = Some(error.to_string());
                        cx.notify();
                    });
                    return;
                }
                Err(_) => return,
            };
            let (mut updates, mut loading, controls) = subscription.split();
            let first_loading = *loading.borrow_and_update();
            if this
                .update(cx, |this, cx| {
                    this.controls = Some(controls);
                    this.loading = first_loading;
                    this._loading_task = cx.spawn(async move |this, cx| {
                        while loading.changed().await.is_ok() {
                            let state = *loading.borrow_and_update();
                            if this
                                .update(cx, |this, cx| {
                                    this.loading = state;
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                })
                .is_err()
            {
                return;
            }
            while let Some(first) = updates.recv().await {
                let mut diffs = first;
                while let Ok(more) = updates.try_recv() {
                    diffs.extend(more);
                }
                if this
                    .update_in(cx, |this, window, cx| this.apply(diffs, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            session,
            account,
            toasts,
            focus_handle: cx.focus_handle(),
            filter,
            rooms: Vec::new(),
            loading: RoomListLoading::NotLoaded,
            loaded: false,
            list_error: None,
            controls: None,
            list_scroll: UniformListScrollHandle::new(),
            selected: None,
            open: None,
            room_in_front: false,
            members_open: false,
            members: None,
            images,
            link_prompt: None,
            link_modal: ModalState::new(cx),
            launch,
            notified: None,
            answering: Vec::new(),
            actions,
            status,
            pending_launch: None,
            delete_prompt: None,
            delete_modal: ModalState::new(cx),
            leave_prompt: None,
            leave_modal: ModalState::new(cx),
            last_read: None,
            identity_warnings: Vec::new(),
            warnings_task: Task::ready(()),
            made_encrypted: HashSet::new(),
            _status_task: status_task,
            _list_task: list_task,
            _action_events: action_events,
            _loading_task: Task::ready(()),
            members_task: Task::ready(()),
            action_task: Task::ready(()),
        }
    }

    /// The rooms the list shows.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rooms(&self) -> &[RoomSummary] {
        &self.rooms
    }

    /// The filter.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn filter(&self) -> RoomListFilter {
        self.filter
    }

    /// The open room's id.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn selected(&self) -> Option<&OwnedRoomId> {
        self.selected.as_ref()
    }

    /// The room the person can see: the open room, unless a narrow window
    /// shows the list instead.
    pub(crate) fn room_in_view(&self, window: &Window) -> Option<&OwnedRoomId> {
        let narrow = f32::from(window.viewport_size().width) < NARROW_BELOW;
        self.selected
            .as_ref()
            .filter(|_| !narrow || self.room_in_front)
    }

    /// The account the rooms are of.
    pub(crate) fn account(&self) -> &AccountView {
        &self.account
    }

    /// Opens the room a notification named: from the list when it is
    /// there, otherwise from what the notification said (a room past the
    /// list's first page, or outside its filter).
    pub(crate) fn open_notified_room(
        &mut self,
        room: OwnedRoomId,
        name: String,
        is_direct: bool,
        is_invite: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.rooms.iter().any(|summary| summary.room_id == room) {
            let mut summary = RoomSummary::new(room.clone(), name);
            summary.is_direct = is_direct;
            if is_invite {
                summary.membership = familychat_core::rooms::Membership::Invited;
            }
            self.notified = Some(summary);
        }
        self.open_room(room, window, cx);
    }

    /// The open room's timeline.
    pub(crate) fn composer(&self) -> Option<&Entity<ComposerView>> {
        self.open.as_ref().map(|open| &open.composer)
    }

    /// The question about deleting a message, while it is open.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn delete_prompt(&self) -> Option<&MessageChoice> {
        self.delete_prompt.as_ref().map(|(_, choice)| choice)
    }

    /// The open room's timeline.
    pub(crate) fn timeline(&self) -> Option<&Entity<TimelineView>> {
        self.open.as_ref().map(|open| &open.timeline)
    }

    /// Whether the member list shows.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn members_open(&self) -> bool {
        self.members_open
    }

    /// The open link dialog's link.
    pub(crate) fn link_prompt(&self) -> Option<&LinkPrompt> {
        self.link_prompt
            .as_ref()
            .filter(|_| self.link_modal.is_open())
    }

    /// The account's image cache.
    #[cfg(test)]
    pub(crate) fn images(&self) -> &Entity<ImageCache> {
        &self.images
    }

    fn toast(&self, toast: Toast, cx: &mut Context<Self>) {
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(toast, cx);
        });
    }

    // -- The room list ------------------------------------------------------

    fn apply(
        &mut self,
        diffs: Vec<familychat_core::diff::ListDiff<RoomSummary>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if familychat_core::diff::apply_all(&mut self.rooms, diffs).is_err() {
            // Out of step: ask for the list again through the filter.
            if let Some(controls) = &self.controls {
                controls.set_filter(self.filter);
            }
        }
        self.loaded = true;
        if let Some(open) = &self.open
            && let Some(room) = self
                .rooms
                .iter()
                .find(|room| Some(&room.room_id) == self.selected.as_ref())
                .cloned()
        {
            let state = ComposerState::of(&self.status, &room);
            open.composer.update(cx, |composer, cx| {
                composer.set_room(room.clone(), cx);
                composer.set_state(state, window, cx);
            });
            open.timeline
                .update(cx, |timeline, cx| timeline.set_room(room, cx));
        }
        if let Some(launch) = self.launch.take() {
            self.apply_launch(launch, window, cx);
        }
        cx.notify();
    }

    fn apply_launch(&mut self, launch: RoomsLaunch, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(room) = launch
            .room
            .as_deref()
            .and_then(|room| OwnedRoomId::try_from(room).ok())
        {
            self.open_room(room, window, cx);
        }
        if launch.members {
            self.set_members_open(true, cx);
        }
        if launch.expand_changes
            && let Some(timeline) = self.timeline().cloned()
        {
            timeline.update(cx, |timeline, cx| timeline.expand_all_changes(cx));
        }
        if let Some((href, text)) = &launch.link {
            self.open_link(href, text, window, cx);
        }
        if let Some(open) = &self.open {
            match &launch.composer {
                Some(ComposerLaunch::Text(text) | ComposerLaunch::Mention(text)) => {
                    let text = text.clone();
                    open.composer
                        .update(cx, |composer, cx| composer.set_text(&text, window, cx));
                }
                Some(ComposerLaunch::Attachment) => {
                    open.composer.update(cx, |composer, cx| {
                        composer.set_text("The photos from Saturday", window, cx);
                        composer.add_file(
                            FileSource::Bytes {
                                name: "Grandad's 80th.pdf".to_owned(),
                                data: Arc::from(vec![0u8; 2_516_582]),
                            },
                            cx,
                        );
                    });
                }
                Some(ComposerLaunch::TooLarge) => {
                    open.composer.update(cx, |composer, cx| {
                        composer.add_file(
                            FileSource::Bytes {
                                name: "Holiday video.mov".to_owned(),
                                data: Arc::from(vec![0u8; 14_890_000]),
                            },
                            cx,
                        );
                    });
                }
                _ => {}
            }
        }
        // The rest needs the timeline's messages.
        if matches!(
            launch.composer,
            Some(ComposerLaunch::Reply | ComposerLaunch::Edit)
        ) || launch.message_actions
            || launch.delete_prompt
        {
            self.pending_launch = Some(launch);
        }
    }

    /// The launch steps that need the open room's messages.
    fn apply_pending_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(launch) = self.pending_launch.take() else {
            return;
        };
        let Some(timeline) = self.timeline().cloned() else {
            return;
        };
        match launch.composer {
            Some(ComposerLaunch::Reply) => {
                if let Some(choice) = timeline.read(cx).chosen(|own, _| !own) {
                    self.reply_to(choice, window, cx);
                }
            }
            Some(ComposerLaunch::Edit) => {
                if let Some(choice) = timeline.read(cx).chosen(|_, editable| editable) {
                    self.edit(choice, window, cx);
                }
            }
            _ => {}
        }
        if launch.message_actions {
            timeline.update(cx, |timeline, cx| {
                timeline.select_step(-1, cx);
                timeline.open_picker(None, cx);
            });
        }
        if launch.delete_prompt
            && let Some(choice) = timeline.read(cx).chosen(|own, _| own)
        {
            self.ask_to_delete(choice, window, cx);
        }
    }

    fn set_status(&mut self, status: AccountStatus, window: &mut Window, cx: &mut Context<Self>) {
        self.status = status;
        if let Some(open) = &self.open {
            let room = open.timeline.read(cx).room().clone();
            let state = ComposerState::of(&self.status, &room);
            open.composer
                .update(cx, |composer, cx| composer.set_state(state, window, cx));
        }
        cx.notify();
    }

    /// Shows another set of rooms.
    pub(crate) fn set_filter(&mut self, filter: RoomListFilter, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        if let Some(controls) = &self.controls {
            controls.set_filter(filter);
        }
        self.list_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// Opens `room` (an invitation shows its answer instead of a timeline).
    pub(crate) fn open_room(
        &mut self,
        room: OwnedRoomId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.room_in_front = true;
        if self.selected.as_ref() == Some(&room) && self.open.is_some() {
            cx.notify();
            return;
        }
        let summary = self
            .rooms
            .iter()
            .find(|summary| summary.room_id == room)
            .cloned()
            .or_else(|| {
                self.notified
                    .take()
                    .filter(|summary| summary.room_id == room)
            });
        self.notified = None;
        cx.emit(RoomOpened(room.clone()));
        if let Some(open) = &self.open {
            open.composer
                .update(cx, |composer, cx| composer.save_draft_now(cx));
        }
        self.selected = Some(room.clone());
        self.open = None;
        self.last_read = None;
        self.follow_identity_warnings(room.clone(), cx);
        if let Some(index) = self
            .rooms
            .iter()
            .position(|summary| summary.room_id == room)
        {
            self.list_scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
        }
        match summary {
            Some(summary) if !summary.is_invite() => {
                let session = self.session.clone();
                let account = self.account.key.clone();
                let images = self.images.clone();
                let timeline = {
                    let summary = summary.clone();
                    cx.new(|cx| TimelineView::new(session, account, summary, images, window, cx))
                };
                let events = cx.subscribe_in(&timeline, window, |this, _, event, window, cx| {
                    this.timeline_event(event.clone(), window, cx);
                });
                let state = ComposerState::of(&self.status, &summary);
                let composer = {
                    let session = self.session.clone();
                    let account = self.account.key.clone();
                    let actions = self.actions.clone();
                    cx.new(|cx| {
                        ComposerView::new(session, account, summary, actions, state, window, cx)
                    })
                };
                let composer_events =
                    cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
                        this.composer_event(event.clone(), window, cx);
                    });
                self.open = Some(OpenRoom {
                    timeline,
                    composer,
                    _events: events,
                    _composer_events: composer_events,
                });
                if self.members_open {
                    self.load_members(cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn select_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(room) = self.rooms.get(index).map(|room| room.room_id.clone()) {
            self.open_room(room, window, cx);
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rooms.iter().position(|room| &room.room_id == selected)
    }

    /// Opens the room `step` places along the list (wrapping is not done).
    fn step(
        &mut self,
        forward: bool,
        unread_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.rooms.len();
        if count == 0 {
            return;
        }
        let start = self.selected_index();
        let candidates: Vec<usize> = if forward {
            (start.map_or(0, |index| index + 1)..count).collect()
        } else {
            (0..start.unwrap_or(count)).rev().collect()
        };
        let next = candidates
            .into_iter()
            .find(|index| !unread_only || self.rooms[*index].unread.is_unread());
        match next {
            Some(index) => self.select_index(index, window, cx),
            None if unread_only => self.toast(Toast::new("No more unread rooms."), cx),
            None => {}
        }
    }

    fn answer_invite(&mut self, room: OwnedRoomId, accept: bool, cx: &mut Context<Self>) {
        if self.answering.contains(&room) {
            return;
        }
        self.answering.push(room.clone());
        cx.background_spawn(self.actions.answer_invite(&room, accept))
            .detach();
        cx.notify();
    }

    /// What the actions reported, a burst at a time.
    fn action_events(
        &mut self,
        events: Vec<ActionEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for event in events {
            self.action_event(event, window, cx);
        }
        cx.notify();
    }

    fn room_name(&self, room: &OwnedRoomId) -> String {
        self.rooms
            .iter()
            .find(|summary| &summary.room_id == room)
            .map(|summary| summary.name.clone())
            .unwrap_or_default()
    }

    fn action_event(&mut self, event: ActionEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            event,
            ActionEvent::Refused {
                refusal: Refusal::Empty,
                ..
            }
        ) {
            return;
        }
        let answered = match &event {
            ActionEvent::Done {
                room: Some(room),
                action,
            }
            | ActionEvent::Failed {
                room: Some(room),
                action,
                ..
            }
            | ActionEvent::Refused {
                room: Some(room),
                action,
                ..
            } if matches!(action, Action::AcceptInvite | Action::DeclineInvite) => {
                Some(room.clone())
            }
            _ => None,
        };
        if let Some(room) = &answered {
            self.answering.retain(|answering| answering != room);
        }
        let open_room = self.selected.clone();
        let composer = self.composer().cloned();
        match &event {
            ActionEvent::DraftLoaded { room, draft } => {
                if open_room.as_ref() == Some(room)
                    && let Some(composer) = composer
                {
                    let target = draft
                        .as_ref()
                        .and_then(|draft| draft.reply_to.clone())
                        .map(|event_id| {
                            let found = self
                                .timeline()
                                .and_then(|timeline| timeline.read(cx).choice_for_event(&event_id));
                            Target::Reply {
                                event_id,
                                sender: found
                                    .as_ref()
                                    .map(|choice| choice.sender.clone())
                                    .unwrap_or_else(|| "a message".to_owned()),
                                text: found
                                    .map(|choice| composer::one_line(&choice.text))
                                    .unwrap_or_default(),
                            }
                        })
                        .unwrap_or(Target::New);
                    let draft = draft.clone();
                    composer.update(cx, |composer, cx| {
                        composer.apply_draft(draft, target, _window, cx);
                    });
                }
            }
            ActionEvent::Failed {
                room: Some(room),
                action: Action::LoadDraft,
                ..
            } => {
                // No draft to show, but what is typed now can be kept.
                if open_room.as_ref() == Some(room)
                    && let Some(composer) = composer
                {
                    composer.update(cx, |composer, cx| {
                        composer.apply_draft(None, Target::New, _window, cx);
                    });
                }
            }
            ActionEvent::Refused {
                room: Some(room),
                action: Action::Send | Action::Reply | Action::Edit | Action::SendFile,
                refusal,
            } if open_room.as_ref() == Some(room) && *refusal != Refusal::Empty => {
                if let Some(composer) = composer {
                    let message = refusal.to_string();
                    composer.update(cx, |composer, cx| composer.show_error(message, cx));
                }
            }
            ActionEvent::Failed {
                room: Some(room),
                action: action @ (Action::Send | Action::Reply | Action::Edit | Action::SendFile),
                ..
            } if open_room.as_ref() == Some(room) => {
                if let (Some(composer), Some(message)) = (composer, event.message()) {
                    let _ = action;
                    composer.update(cx, |composer, cx| composer.show_error(message, cx));
                }
            }
            ActionEvent::Done {
                room: Some(room),
                action: Action::Leave,
            } => {
                let name = self.room_name(room);
                if self.selected.as_ref() == Some(room) {
                    self.selected = None;
                    self.open = None;
                }
                self.toast(Toast::new(format!("You left {name}.")), cx);
            }
            ActionEvent::RoomCreated {
                room, encrypted, ..
            } => {
                let room = room.clone();
                if *encrypted {
                    self.made_encrypted.insert(room.clone());
                }
                self.open_room(room, _window, cx);
            }
            ActionEvent::Done {
                room: Some(room),
                action: Action::AcceptInvite,
            } => {
                let name = self.room_name(room);
                self.toast(
                    Toast::new(format!("You joined {name}.")).kind(ToastKind::Success),
                    cx,
                );
            }
            ActionEvent::Done {
                room: Some(room),
                action: Action::DeclineInvite,
            } => {
                let name = self.room_name(room);
                self.toast(
                    Toast::new(format!("You declined the invitation to {name}.")),
                    cx,
                );
            }
            other => {
                if let Some(message) = other.message() {
                    self.toast(Toast::new(message).kind(ToastKind::Error), cx);
                }
            }
        }
    }

    // -- Members ------------------------------------------------------------

    /// Shows or hides the member list.
    pub(crate) fn set_members_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.members_open = open;
        if open {
            self.load_members(cx);
        }
        cx.notify();
    }

    fn load_members(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.selected.clone() else {
            return;
        };
        if self
            .members
            .as_ref()
            .is_some_and(|(loaded, state)| loaded == &room && *state != MembersState::Loading)
        {
            return;
        }
        self.members = Some((room.clone(), MembersState::Loading));
        let reply = self.session.members(&self.account.key, &room);
        self.members_task = cx.spawn(async move |this, cx| {
            let state = match reply.await {
                Ok(Ok(members)) => MembersState::Ready(members),
                Ok(Err(error)) => MembersState::Failed(error.to_string()),
                Err(_) => MembersState::Failed("Family Chat is closing.".to_owned()),
            };
            let _ = this.update(cx, |this, cx| {
                if this.selected.as_ref() == Some(&room) {
                    this.members = Some((room, state));
                    cx.notify();
                }
            });
        });
    }

    // -- Links and files ------------------------------------------------------

    fn timeline_event(
        &mut self,
        event: TimelineViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TimelineViewEvent::Link { href, text } => self.open_link(&href, &text, window, cx),
            TimelineViewEvent::Save(media) => self.save(media, window, cx),
            TimelineViewEvent::Reply(choice) => self.reply_to(choice, window, cx),
            TimelineViewEvent::Edit(choice) => self.edit(choice, window, cx),
            TimelineViewEvent::React { target, key } => {
                if let Some(room) = self.selected.clone() {
                    cx.background_spawn(self.actions.toggle_reaction(&room, target, &key))
                        .detach();
                }
            }
            TimelineViewEvent::Delete(choice) => self.ask_to_delete(choice, window, cx),
            TimelineViewEvent::ReadToEnd => {
                self.mark_read(window, cx);
                self.apply_pending_launch(window, cx);
            }
            TimelineViewEvent::SendAnyway(target) => {
                if let Some(room) = self.selected.clone() {
                    cx.background_spawn(self.actions.send_anyway(&room, target))
                        .detach();
                }
            }
        }
    }

    /// Follows the identity warnings of `room` while it is open.
    fn follow_identity_warnings(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        self.identity_warnings.clear();
        let reply = self.session.identity_warnings(&self.account.key, &room);
        self.warnings_task = cx.spawn(async move |this, cx| {
            let Ok(Ok(mut warnings)) = reply.await else {
                return;
            };
            loop {
                let current = warnings.borrow_and_update().clone();
                let showing = this
                    .update(cx, |this, cx| {
                        if this.selected.as_ref() != Some(&room) {
                            return false;
                        }
                        this.identity_warnings = current;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !showing || warnings.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    /// The open room's identity warnings.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn identity_warnings(&self) -> &[IdentityWarning] {
        &self.identity_warnings
    }

    /// Answers an identity warning (acknowledges the new identity, or
    /// withdraws the old verification).
    fn acknowledge_identity(&mut self, warning: IdentityWarning, cx: &mut Context<Self>) {
        let reply = self
            .session
            .acknowledge_identity(&self.account.key, &warning.user_id);
        self.action_task = cx.spawn(async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.identity_warnings
                        .retain(|shown| shown.user_id != warning.user_id);
                    cx.notify();
                }
                Err(error) => {
                    this.toast(Toast::new(error.to_string()).kind(ToastKind::Error), cx);
                }
            });
        });
    }

    fn render_identity_warnings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.identity_warnings.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .px(px(SPACE_4))
                .pt(px(SPACE_3))
                .children(
                    self.identity_warnings
                        .iter()
                        .enumerate()
                        .map(|(index, warning)| {
                            let tone = match warning.kind {
                                IdentityWarningKind::Changed => BannerTone::Caution,
                                IdentityWarningKind::VerificationLost => BannerTone::Danger,
                            };
                            let answered = warning.clone();
                            Banner::new(("room-identity-warning", index as u64), warning.message())
                                .tone(tone)
                                .title("Identity changed")
                                .action(
                                    Button::new(
                                        ("room-identity-acknowledge", index as u64),
                                        warning.action_label(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.acknowledge_identity(answered.clone(), cx);
                                        },
                                    )),
                                )
                        }),
                )
                .into_any_element(),
        )
    }

    /// The open room is read to its newest message: the read receipt and
    /// marker move there (at most every two seconds, and only while the
    /// window is in front).
    fn mark_read(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if !window.is_window_active() {
            return;
        }
        let Some(room) = self.selected.clone() else {
            return;
        };
        let now = std::time::Instant::now();
        if self
            .last_read
            .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_secs(2))
        {
            return;
        }
        self.last_read = Some(now);
        _cx.background_spawn(self.actions.mark_read(&room)).detach();
    }

    fn reply_to(&mut self, choice: MessageChoice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event_id) = choice.target.event_id.clone() else {
            self.toast(Toast::new("Wait until that message has been sent."), cx);
            return;
        };
        if let Some(open) = &self.open {
            let text = composer::one_line(&choice.text);
            open.composer.update(cx, |composer, cx| {
                composer.reply_to(event_id, choice.sender.clone(), text, window, cx);
            });
        }
        self.dismiss_selection(cx);
    }

    fn edit(&mut self, choice: MessageChoice, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = &self.open {
            open.composer.update(cx, |composer, cx| {
                composer.edit(choice.target.clone(), choice.text.clone(), window, cx);
            });
        }
        self.dismiss_selection(cx);
    }

    fn dismiss_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(timeline) = self.timeline().cloned() {
            timeline.update(cx, |timeline, cx| {
                timeline.dismiss(cx);
            });
        }
    }

    fn ask_to_delete(
        &mut self,
        choice: MessageChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(room) = self.selected.clone() else {
            return;
        };
        self.delete_prompt = Some((room, choice));
        self.delete_modal.open(window, cx);
        cx.notify();
    }

    fn close_delete_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_prompt = None;
        self.delete_modal.close(window, cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Deletes the message the dialog asked about.
    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((room, choice)) = self.delete_prompt.clone() {
            cx.background_spawn(self.actions.redact(&room, choice.target, None))
                .detach();
        }
        self.close_delete_prompt(window, cx);
    }

    fn close_leave_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_prompt = None;
        self.leave_modal.close(window, cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn confirm_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(room) = self.leave_prompt.clone() {
            cx.background_spawn(self.actions.leave(&room.room_id))
                .detach();
        }
        self.close_leave_prompt(window, cx);
    }

    fn composer_event(
        &mut self,
        event: ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::Sent => {
                if let Some(timeline) = self.timeline().cloned() {
                    timeline.update(cx, |timeline, cx| timeline.jump_to_latest(cx));
                }
            }
            ComposerEvent::EditLatest => {
                let choice = self
                    .timeline()
                    .and_then(|timeline| timeline.read(cx).chosen(|_, editable| editable));
                if let Some(choice) = choice {
                    self.edit(choice, window, cx);
                }
            }
        }
    }

    /// Opens a link from a message: in the app, or after asking.
    pub(crate) fn open_link(
        &mut self,
        href: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match classify(href, text) {
            LinkAction::Navigate(link) => self.navigate(link, window, cx),
            LinkAction::Confirm(prompt) => {
                self.link_prompt = Some(prompt);
                self.link_modal.open(window, cx);
                cx.notify();
            }
            LinkAction::Refuse => {
                self.toast(
                    Toast::new("Family Chat doesn't open this kind of link.")
                        .kind(ToastKind::Error),
                    cx,
                );
            }
        }
    }

    fn navigate(&mut self, link: MatrixLink, window: &mut Window, cx: &mut Context<Self>) {
        let room_in_list = |id: &str| {
            self.rooms
                .iter()
                .find(|room| room.room_id.as_str() == id)
                .map(|room| room.room_id.clone())
        };
        match link {
            MatrixLink::Room(room) => match room_in_list(room.as_str()) {
                Some(room) => self.open_room(room, window, cx),
                None => self.toast(Toast::new(format!("{room} isn't one of your rooms.")), cx),
            },
            MatrixLink::Event { room, event } => match room_in_list(room.as_str()) {
                Some(room) => {
                    self.open_room(room, window, cx);
                    if let Some(timeline) = self.timeline().cloned() {
                        timeline.update(cx, |timeline, cx| timeline.reveal_event(event, cx));
                    }
                }
                None => self.toast(Toast::new(format!("{room} isn't one of your rooms.")), cx),
            },
            MatrixLink::User(user) => {
                let member = self.members.as_ref().and_then(|(_, state)| match state {
                    MembersState::Ready(members) => members
                        .iter()
                        .find(|member| member.user_id == user)
                        .map(|member| member.name()),
                    _ => None,
                });
                let who = match member {
                    Some(name) => format!("{name} ({user})"),
                    None => user.to_string(),
                };
                self.toast(Toast::new(format!("That's {who}.")), cx);
            }
        }
    }

    fn close_link_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.link_prompt = None;
        self.link_modal.close(window, cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Hands the dialog's address to the operating system, and closes it.
    fn open_prompted_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prompt) = self.link_prompt.as_ref() {
            cx.open_url(&prompt.target);
        }
        self.close_link_prompt(window, cx);
    }

    fn save(&mut self, media: MediaInfo, _window: &mut Window, cx: &mut Context<Self>) {
        let directory = downloads_dir();
        let chosen = cx.prompt_for_new_path(&directory, Some(&media.filename));
        let session = self.session.clone();
        let account = self.account.key.clone();
        self.action_task = cx.spawn(async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.toast(
                            Toast::new(format!("The file picker couldn't open: {error}"))
                                .kind(ToastKind::Error),
                            cx,
                        );
                    });
                    return;
                }
            };
            let result = session
                .save_file(&account, &media, path.clone())
                .await
                .unwrap_or(Err(familychat_core::media::MediaError::Unavailable));
            let _ = this.update(cx, |this, cx| {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let toast = match result {
                    Ok(()) => Toast::new(format!("Saved {name}.")).kind(ToastKind::Success),
                    Err(error) => Toast::new(error.to_string()).kind(ToastKind::Error),
                };
                this.toast(toast, cx);
            });
        });
    }

    // -- Commands -----------------------------------------------------------

    fn next_room(&mut self, _: &NextRoom, window: &mut Window, cx: &mut Context<Self>) {
        self.step(true, false, window, cx);
    }

    fn previous_room(&mut self, _: &PreviousRoom, window: &mut Window, cx: &mut Context<Self>) {
        self.step(false, false, window, cx);
    }

    fn next_unread(&mut self, _: &NextUnreadRoom, window: &mut Window, cx: &mut Context<Self>) {
        self.step(true, true, window, cx);
    }

    fn previous_unread(
        &mut self,
        _: &PreviousUnreadRoom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step(false, true, window, cx);
    }

    fn jump_to_oldest_unread(
        &mut self,
        _: &JumpToOldestUnread,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(timeline) = self.timeline().cloned() else {
            return;
        };
        if !timeline.update(cx, |timeline, cx| timeline.jump_to_oldest_unread(cx)) {
            self.toast(Toast::new("There are no unread messages here."), cx);
        }
    }

    fn jump_to_latest(&mut self, _: &JumpToLatest, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(timeline) = self.timeline().cloned() {
            timeline.update(cx, |timeline, cx| timeline.jump_to_latest(cx));
        }
    }

    fn toggle_members(&mut self, _: &ToggleMemberList, _: &mut Window, cx: &mut Context<Self>) {
        let open = !self.members_open;
        self.set_members_open(open, cx);
    }

    fn focus_composer(&mut self, _: &FocusComposer, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(composer) = self.composer().cloned() {
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        }
    }

    fn attach_file(&mut self, _: &AttachFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(composer) = self.composer().cloned() {
            composer.update(cx, |composer, cx| composer.pick_files(window, cx));
        }
    }

    fn select_message(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(timeline) = self.timeline().cloned() {
            timeline.update(cx, |timeline, cx| timeline.select_step(step, cx));
        }
    }

    /// The message a keyboard command is about, or a toast saying why
    /// there is none.
    fn chosen(
        &mut self,
        accept: impl Fn(bool, bool) -> bool,
        none: &str,
        cx: &mut Context<Self>,
    ) -> Option<MessageChoice> {
        let choice = self
            .timeline()
            .and_then(|timeline| timeline.read(cx).chosen(accept));
        if choice.is_none() {
            self.toast(Toast::new(none.to_owned()), cx);
        }
        choice
    }

    fn reply_command(&mut self, _: &ReplyToMessage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(choice) = self.chosen(|_, _| true, "There's no message to reply to.", cx) {
            self.reply_to(choice, window, cx);
        }
    }

    fn edit_command(&mut self, _: &EditMessage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(choice) = self.chosen(
            |_, editable| editable,
            "Pick one of your messages to edit.",
            cx,
        ) {
            self.edit(choice, window, cx);
        }
    }

    fn react_command(&mut self, _: &ReactToMessage, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(timeline) = self.timeline().cloned() {
            timeline.update(cx, |timeline, cx| timeline.open_picker(None, cx));
        }
    }

    fn delete_command(&mut self, _: &DeleteMessage, window: &mut Window, cx: &mut Context<Self>) {
        let selected_own = self.timeline().is_some_and(|timeline| {
            let timeline = timeline.read(cx);
            timeline.selected().is_some() && timeline.chosen(|own, _| own).is_some()
        });
        if !selected_own {
            self.toast(Toast::new("Select one of your messages to delete it."), cx);
            return;
        }
        if let Some(choice) = self.chosen(|own, _| own, "", cx) {
            self.ask_to_delete(choice, window, cx);
        }
    }

    fn leave_command(&mut self, _: &LeaveRoom, window: &mut Window, cx: &mut Context<Self>) {
        let room = self
            .selected
            .as_ref()
            .and_then(|selected| self.rooms.iter().find(|room| &room.room_id == selected))
            .cloned();
        if let Some(room) = room {
            self.leave_prompt = Some(room);
            self.leave_modal.open(window, cx);
            cx.notify();
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        let dismissed = self
            .timeline()
            .cloned()
            .is_some_and(|timeline| timeline.update(cx, |timeline, cx| timeline.dismiss(cx)));
        if !dismissed {
            cx.propagate();
        }
    }

    // -- Rendering ----------------------------------------------------------

    fn render_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("room-filters")
            .accessible(Role::RadioGroup, "Show")
            .flex()
            .flex_wrap()
            .gap(px(SPACE_1))
            .px(px(SPACE_3))
            .pb(px(SPACE_2))
            .children(RoomListFilter::ALL.into_iter().map(|filter| {
                let selected = self.filter == filter;
                Button::new(
                    SharedString::from(format!("rooms-filter-{}", filter.label().to_lowercase())),
                    filter.label(),
                )
                .role(Role::RadioButton)
                .size(ButtonSize::Small)
                .selected(selected)
                .when(!selected, |button| button.ghost())
                .shortcut(primary_key(match filter {
                    RoomListFilter::All => "ShowAllRooms",
                    RoomListFilter::Unread => "ShowUnreadRooms",
                    RoomListFilter::People => "ShowPeople",
                    RoomListFilter::Invites => "ShowInvites",
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
            }))
    }

    fn render_room_rows(
        &mut self,
        range: std::ops::Range<usize>,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Stateful<gpui::Div>> {
        let now = now_ms();
        range
            .filter_map(|index| {
                let room = self.rooms.get(index)?.clone();
                Some(self.render_room_row(index, &room, now, colors, cx))
            })
            .collect()
    }

    fn render_room_row(
        &self,
        index: usize,
        room: &RoomSummary,
        now: u64,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = self.selected.as_ref() == Some(&room.room_id);
        let unread = room.unread.is_unread();
        let room_id = room.room_id.clone();
        let when = room
            .last_activity_ms
            .map(|at| activity_label(at, now))
            .unwrap_or_default();
        let second_line: SharedString = if room.is_invite() {
            match &room.invited_by {
                Some(inviter) => format!("{inviter} invited you").into(),
                None => "You're invited".into(),
            }
        } else {
            match &room.preview {
                Some(preview) if room.is_direct && !preview.is_own => {
                    preview.text.as_str().to_owned().into()
                }
                Some(preview) => {
                    let sender = if preview.is_own {
                        "You"
                    } else {
                        &preview.sender
                    };
                    format!("{sender}: {}", preview.text.as_str()).into()
                }
                None => "No messages yet".into(),
            }
        };
        let fill = if selected {
            colors.selected_background
        } else {
            colors.surface
        };
        let busy = self.answering.contains(&room.room_id);
        div()
            .id(("room-row", index as u64))
            .hit_target(move || format!("room-row-{index}"))
            .accessible(Role::ListBoxOption, room.accessible_label())
            .aria_selected(selected)
            .w_full()
            .h(px(ROOM_ROW_HEIGHT))
            .px(px(SPACE_2))
            .flex()
            .items_center()
            .gap(px(SPACE_3))
            .rounded(px(RADIUS_LG))
            .bg(rgb(fill))
            .when(!selected, |this| {
                this.hover(|this| this.bg(rgb(colors.hover_background)))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_room(room_id.clone(), window, cx);
            }))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(Avatar::new(room.name.clone()).size(40.0)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_1))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .type_style(TYPE_BODY)
                                    .font_weight(if unread || room.is_invite() {
                                        FontWeight::BOLD
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .child(room.name.clone()),
                            )
                            .when(room.encryption == Encryption::NotEncrypted, |this| {
                                this.child(icon("lock-open", 13.0, colors.warning))
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(if unread {
                                        colors.accent
                                    } else {
                                        colors.muted
                                    }))
                                    .child(when),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_1))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(if unread {
                                        colors.foreground
                                    } else {
                                        colors.muted
                                    }))
                                    .child(second_line),
                            )
                            .when(room.unread.mentions > 0, |this| {
                                this.child(Badge::label("@").tone(BadgeTone::Danger))
                            })
                            .when(room.unread.notifications > 0, |this| {
                                this.child(Badge::count(
                                    usize::try_from(room.unread.notifications)
                                        .unwrap_or(usize::MAX),
                                ))
                            })
                            .when(
                                room.unread.notifications == 0 && room.unread.marked_unread,
                                |this| this.child(Badge::dot()),
                            ),
                    ),
            )
            .when(room.is_invite(), |this| {
                let accept_room = room.room_id.clone();
                let decline_room = room.room_id.clone();
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(SPACE_1))
                        .child(
                            IconButton::new(
                                SharedString::from(format!("invite-decline-{index}")),
                                "x",
                                format!("Decline the invitation to {}", room.name),
                            )
                            .disabled(busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.answer_invite(decline_room.clone(), false, cx);
                                },
                            )),
                        )
                        .child(
                            IconButton::new(
                                SharedString::from(format!("invite-accept-{index}")),
                                "check",
                                format!("Accept the invitation to {}", room.name),
                            )
                            .variant(crate::components::ButtonVariant::Primary)
                            .disabled(busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.answer_invite(accept_room.clone(), true, cx);
                                },
                            )),
                        ),
                )
            })
    }

    fn render_list_body(&mut self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        if let Some(error) = &self.list_error {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(empty_state(
                    "Your rooms couldn't be loaded",
                    error,
                    "alert-circle",
                    None,
                    colors,
                ))
                .into_any_element();
        }
        if !self.loaded || self.loading == RoomListLoading::NotLoaded && self.rooms.is_empty() {
            return div()
                .id("rooms-loading")
                .accessible(Role::Status, "Loading your rooms")
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_3))
                .child(spinner("rooms-loading-spinner", 24.0, colors.muted))
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child("Loading your rooms…"),
                )
                .into_any_element();
        }
        if self.rooms.is_empty() {
            let (title, detail, icon_name) = match self.filter {
                RoomListFilter::All => (
                    "No rooms yet",
                    "When someone in your family adds you to a room, it shows up here.",
                    "messages-square",
                ),
                RoomListFilter::Unread => (
                    "You're all caught up",
                    "Rooms with new messages show up here.",
                    "circle-check",
                ),
                RoomListFilter::People => (
                    "No direct chats",
                    "Chats with one other person show up here.",
                    "user",
                ),
                RoomListFilter::Invites => (
                    "No invitations",
                    "When someone invites you to a room, it shows up here.",
                    "mail-plus",
                ),
            };
            let action = (self.filter != RoomListFilter::All).then(|| {
                Button::new("rooms-show-all", "Show all rooms")
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_filter(RoomListFilter::All, cx)),
                    )
                    .into_any_element()
            });
            // A column, so the card takes the pane's width and its text
            // wraps instead of running past the edge.
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(empty_state(title, detail, icon_name, action, colors))
                .into_any_element();
        }
        let colors = *colors;
        div()
            .id("room-list")
            .accessible(Role::ListBox, format!("Rooms, {}", self.filter.label()))
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                uniform_list(
                    "room-rows",
                    self.rooms.len(),
                    cx.processor(move |this, range, _window, cx| {
                        this.render_room_rows(range, &colors, cx)
                    }),
                )
                .track_scroll(&self.list_scroll)
                .size_full()
                .px(px(SPACE_2))
                .pb(px(SPACE_2)),
            )
            .child(Scrollbar::for_uniform_list(
                "room-list-scrollbar",
                &self.list_scroll,
            ))
            .into_any_element()
    }

    fn render_list_pane(
        &mut self,
        width: Option<f32>,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let user = self.account.user_id.clone();
        let body = self.render_list_body(colors, cx);
        div()
            .id("rooms-pane")
            .accessible(Role::Navigation, "Rooms")
            .flex()
            .flex_col()
            .h_full()
            .when_some(width, |this, width| this.w(px(width)).flex_none())
            .when(width.is_none(), |this| this.flex_1())
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                div().px(px(SPACE_4)).pt(px(SPACE_4)).pb(px(SPACE_2)).child(
                    div()
                        .id("rooms-heading")
                        .accessible(Role::Heading, "Rooms")
                        .aria_level(1)
                        .type_style(TYPE_HEADLINE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Rooms"),
                ),
            )
            .child(self.render_filters(cx))
            .child(body)
            .child(
                div()
                    .id("rooms-account")
                    .accessible(Role::Group, format!("Signed in as {user}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_3))
                    .px(px(SPACE_4))
                    .py(px(SPACE_3))
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .child(
                        Avatar::new(user.localpart().to_owned())
                            .address(user.to_string())
                            .size(32.0),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .type_style(TYPE_BODY_STRONG)
                                    .child(user.localpart().to_owned()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(colors.muted))
                                    .child(user.server_name().to_string()),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_header(
        &self,
        room: &RoomSummary,
        narrow: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = room
            .topic
            .clone()
            .unwrap_or_else(|| match room.joined_members {
                0 => String::new(),
                1 => "1 member".to_owned(),
                count => format!("{count} members"),
            });
        let (lock_icon, lock_color, lock_fill) = match room.encryption {
            Encryption::Encrypted => ("lock", colors.success, colors.success_background),
            Encryption::NotEncrypted => ("lock-open", colors.warning, colors.warning_background),
            Encryption::Unknown => ("shield", colors.muted, colors.button_background),
        };
        let encryption = room.encryption;
        div()
            .id("room-header")
            .accessible(Role::Banner, room.name.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_3))
            .h(px(60.0))
            .px(px(SPACE_4))
            .border_b_1()
            .border_color(rgb(colors.border))
            .when(narrow, |this| {
                this.child(
                    IconButton::new("room-back", "chevron-left", "Back to rooms").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.room_in_front = false;
                            cx.notify();
                        }),
                    ),
                )
            })
            .child(Avatar::new(room.name.clone()).size(36.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id("room-name")
                            .accessible(Role::Heading, room.name.clone())
                            .aria_level(2)
                            .truncate()
                            .type_style(TYPE_BODY_STRONG)
                            .child(room.name.clone()),
                    )
                    .when(!detail.is_empty(), |this| {
                        this.child(
                            div()
                                .truncate()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .id("room-encryption")
                    .accessible(
                        Role::Label,
                        format!("{}. {}", encryption.label(), encryption.description()),
                    )
                    .tooltip(Tooltip::text(encryption.description()))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(SPACE_1))
                    .h(px(26.0))
                    .px(px(SPACE_2))
                    .rounded(px(RADIUS_FULL))
                    .bg(rgb(lock_fill))
                    .text_color(rgb(lock_color))
                    .type_style(TYPE_CAPTION)
                    .font_weight(FontWeight::MEDIUM)
                    .child(icon(lock_icon, 14.0, lock_color))
                    .when(!narrow, |this| this.child(encryption.label())),
            )
            .child(
                IconButton::new(
                    "room-members",
                    "users",
                    if self.members_open {
                        "Hide the members"
                    } else {
                        "Show the members"
                    },
                )
                .selected(self.members_open)
                .shortcut(primary_key("ToggleMemberList"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_members(&ToggleMemberList, window, cx);
                })),
            )
            .child(
                IconButton::new("room-leave", "door-open", "Leave the room")
                    .shortcut(primary_key("LeaveRoom"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.leave_command(&LeaveRoom, window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_delete_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, choice) = self.delete_prompt.as_ref()?;
        let preview = composer::one_line(&choice.text);
        Some(
            Modal::new("delete-prompt", self.delete_modal.focus_handle())
                .title("Delete this message?")
                .description("It goes for everyone in the room, and it can't be brought back.")
                .width(px(440.0))
                .child(readable("delete-preview", preview))
                .footer(Button::new("delete-cancel", "Cancel").on_click(
                    cx.listener(|this, _, window, cx| this.close_delete_prompt(window, cx)),
                ))
                .footer(
                    Button::new("delete-confirm", "Delete")
                        .icon("trash-2")
                        .danger()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_delete(window, cx)),
                        ),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.close_delete_prompt(window, cx)))
                .into_any_element(),
        )
    }

    fn render_leave_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let room = self.leave_prompt.as_ref()?;
        Some(
            Modal::new("leave-prompt", self.leave_modal.focus_handle())
                .title(format!("Leave {}?", room.name))
                .description(
                    "You won't see new messages here. Someone in the room can invite you back.",
                )
                .width(px(440.0))
                .footer(Button::new("leave-cancel", "Cancel").on_click(
                    cx.listener(|this, _, window, cx| this.close_leave_prompt(window, cx)),
                ))
                .footer(
                    Button::new("leave-confirm", "Leave")
                        .icon("door-open")
                        .danger()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_leave(window, cx)),
                        ),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.close_leave_prompt(window, cx)))
                .into_any_element(),
        )
    }

    fn render_invite(
        &self,
        room: &RoomSummary,
        narrow: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.answering.contains(&room.room_id);
        let inviter = room
            .invited_by
            .clone()
            .unwrap_or_else(|| "Someone".to_owned());
        let accept_room = room.room_id.clone();
        let decline_room = room.room_id.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .when(narrow, |this| {
                this.child(div().p(px(SPACE_2)).child(
                    IconButton::new("room-back", "chevron-left", "Back to rooms").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.room_in_front = false;
                            cx.notify();
                        }),
                    ),
                ))
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(SPACE_6))
                    .child(
                        div()
                            .id("invite-card")
                            .accessible(Role::Group, format!("Invitation to {}", room.name))
                            .w_full()
                            .max_w(px(420.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(SPACE_4))
                            .p(px(SPACE_6))
                            .rounded(px(12.0))
                            .border_1()
                            .border_color(rgb(colors.border))
                            .bg(rgb(colors.background))
                            .shadow_md()
                            .text_center()
                            .child(Avatar::new(room.name.clone()).size(64.0))
                            .child(
                                div()
                                    .id("invite-heading")
                                    .accessible(Role::Heading, format!("Join {}?", room.name))
                                    .type_style(TYPE_HEADLINE)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("Join {}?", room.name)),
                            )
                            .child(
                                div()
                                    .type_style(TYPE_BODY)
                                    .text_color(rgb(colors.muted))
                                    .child(readable(
                                        "invite-detail",
                                        format!(
                                            "{inviter} invited you. {}",
                                            room.encryption.description()
                                        ),
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(SPACE_2))
                                    .child(
                                        Button::new("invite-decline", "Decline")
                                            .disabled(busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.answer_invite(decline_room.clone(), false, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("invite-accept", "Join the room")
                                            .primary()
                                            .icon("check")
                                            .disabled(busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.answer_invite(accept_room.clone(), true, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_members_pane(
        &self,
        overlay: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self
            .members
            .as_ref()
            .filter(|(room, _)| Some(room) == self.selected.as_ref())
            .map_or(MembersState::Loading, |(_, state)| state.clone());
        let count = match &state {
            MembersState::Ready(members) => Some(members.len()),
            _ => None,
        };
        div()
            .id("members-pane")
            .accessible(Role::Complementary, "Members")
            .flex()
            .flex_col()
            .h_full()
            .when(!overlay, |this| {
                this.w(px(MEMBERS_WIDTH)).flex_none().border_l_1()
            })
            .when(overlay, |this| this.flex_1())
            .border_color(rgb(colors.border))
            .bg(rgb(colors.surface))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(SPACE_2))
                    .h(px(60.0))
                    .px(px(SPACE_4))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        div()
                            .id("members-heading")
                            .accessible(Role::Heading, "Members")
                            .flex_1()
                            .type_style(TYPE_BODY_STRONG)
                            .child(match count {
                                Some(count) => format!("Members · {count}"),
                                None => "Members".to_owned(),
                            }),
                    )
                    .child(
                        IconButton::new("members-close", "x", "Hide the members").on_click(
                            cx.listener(|this, _, _, cx| this.set_members_open(false, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .id("members-list")
                    .accessible(Role::List, "Members")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py(px(SPACE_1))
                    .child(render_members(&state, colors)),
            )
            .into_any_element()
    }

    fn render_room_pane(
        &mut self,
        narrow: bool,
        members_beside: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let room = self
            .selected
            .as_ref()
            .and_then(|selected| self.rooms.iter().find(|room| &room.room_id == selected))
            .cloned()
            .or_else(|| {
                self.timeline()
                    .map(|timeline| timeline.read(cx).room().clone())
            });
        let Some(room) = room else {
            return div()
                .id("room-pane")
                .flex_1()
                .flex()
                .bg(rgb(colors.background))
                .child(empty_state(
                    "Pick a room",
                    "Choose a room on the left to read what's new.",
                    "message-circle-heart",
                    None,
                    colors,
                ))
                .into_any_element();
        };
        if room.is_invite() {
            return div()
                .id("room-pane")
                .flex_1()
                .bg(rgb(colors.background))
                .child(self.render_invite(&room, narrow, colors, cx))
                .into_any_element();
        }
        let timeline = self.timeline().cloned();
        let header = self.render_header(&room, narrow, colors, cx);
        let members_overlay = self.members_open && !members_beside;
        div()
            .id("room-pane")
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(colors.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(header)
                    .children(self.render_identity_warnings(cx))
                    .when_some(
                        encryption_notice(&room)
                            .filter(|_| !self.made_encrypted.contains(&room.room_id)),
                        |this, notice| {
                            this.child(
                                div().px(px(SPACE_4)).pt(px(SPACE_3)).child(
                                    Banner::new("room-not-encrypted", notice)
                                        .tone(BannerTone::Caution)
                                        .title("Not encrypted"),
                                ),
                            )
                        },
                    )
                    .child(if members_overlay {
                        self.render_members_pane(true, colors, cx)
                    } else {
                        div()
                            .flex_1()
                            .min_h_0()
                            .children(timeline.map(|timeline| timeline.into_any_element()))
                            .into_any_element()
                    })
                    .children(
                        self.composer()
                            .cloned()
                            .filter(|_| !members_overlay)
                            .map(|composer| composer.into_any_element()),
                    ),
            )
            .when(self.members_open && members_beside, |this| {
                this.child(self.render_members_pane(false, colors, cx))
            })
            .into_any_element()
    }

    fn render_link_prompt(&self, colors: &UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.link_prompt()?;
        let target = prompt.target.clone();
        let warning = prompt.warning().map(|(title, sentence)| {
            div()
                .id("link-warning")
                .accessible(Role::Alert, format!("{title}. {sentence}"))
                .flex()
                .items_start()
                .gap(px(SPACE_2))
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(rgb(colors.warning))
                .bg(rgb(colors.warning_background))
                .child(icon("shield-alert", ICON_SIZE_SM, colors.warning))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_1))
                        .type_style(TYPE_CAPTION)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(sentence),
                )
        });
        let open = Button::new("link-open", prompt.open_label())
            .on_click(cx.listener(|this, _, window, cx| this.open_prompted_link(window, cx)));
        // A mismatched link makes Cancel the main button.
        let (open, cancel) = if prompt.is_mismatched() {
            (
                open.danger(),
                Button::new("link-cancel", "Cancel").primary(),
            )
        } else {
            (open.primary(), Button::new("link-cancel", "Cancel"))
        };
        Some(
            Modal::new("link-prompt", self.link_modal.focus_handle())
                .title(prompt.title())
                .description(prompt.description())
                .width(px(520.0))
                .children(warning)
                .child(
                    div()
                        .id("link-target-box")
                        .p(px(SPACE_3))
                        .rounded(px(RADIUS_MD))
                        .bg(rgb(colors.surface))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .font_family(rich_text::MONO_FONT)
                        .text_sm()
                        // The whole address, read out before the choice.
                        .child(readable("link-target", prompt.target.clone())),
                )
                .footer(Button::new("link-copy", "Copy Link").icon("copy").on_click(
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(target.clone()));
                    },
                ))
                .footer(cancel.on_click(
                    cx.listener(|this, _, window, cx| this.close_link_prompt(window, cx)),
                ))
                .footer(open)
                .on_dismiss(cx.listener(|this, _, window, cx| this.close_link_prompt(window, cx)))
                .into_any_element(),
        )
    }
}

/// The time the list shows for a room's latest activity: the time today,
/// "Yesterday", the weekday this week, or the date.
fn activity_label(at: u64, now: u64) -> String {
    match familychat_core::timeline::day_label(at, now).as_str() {
        "Today" => time_label(at),
        other => {
            // "3 November" stays; "Monday" stays; dates from other years
            // drop to the short form.
            other.to_owned()
        }
    }
}

/// The current time, in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Where the save dialog starts: the Downloads folder, else the home
/// folder.
fn downloads_dir() -> PathBuf {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

impl Focusable for RoomsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RoomsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let width = f32::from(window.viewport_size().width);
        let narrow = width < NARROW_BELOW;
        let members_beside = width >= MEMBERS_BESIDE_FROM;
        let room_shown = self.selected.is_some() && (!narrow || self.room_in_front);
        let body = if narrow {
            if room_shown {
                self.render_room_pane(true, false, &colors, cx)
            } else {
                self.render_list_pane(None, &colors, cx)
            }
        } else {
            let list = self.render_list_pane(Some(ROOM_LIST_WIDTH), &colors, cx);
            let room = self.render_room_pane(false, members_beside, &colors, cx);
            div()
                .flex()
                .size_full()
                .child(list)
                .child(room)
                .into_any_element()
        };
        div()
            .id("rooms")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::next_room))
            .on_action(cx.listener(Self::previous_room))
            .on_action(cx.listener(Self::next_unread))
            .on_action(cx.listener(Self::previous_unread))
            .on_action(cx.listener(Self::jump_to_oldest_unread))
            .on_action(cx.listener(Self::jump_to_latest))
            .on_action(cx.listener(Self::toggle_members))
            .on_action(cx.listener(Self::focus_composer))
            .on_action(cx.listener(Self::attach_file))
            .on_action(cx.listener(|this, _: &SelectPreviousMessage, _, cx| {
                this.select_message(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextMessage, _, cx| {
                this.select_message(1, cx);
            }))
            .on_action(cx.listener(Self::reply_command))
            .on_action(cx.listener(Self::edit_command))
            .on_action(cx.listener(Self::react_command))
            .on_action(cx.listener(Self::delete_command))
            .on_action(cx.listener(Self::leave_command))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|this, _: &ShowAllRooms, _, cx| {
                this.set_filter(RoomListFilter::All, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowUnreadRooms, _, cx| {
                this.set_filter(RoomListFilter::Unread, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowPeople, _, cx| {
                this.set_filter(RoomListFilter::People, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowInvites, _, cx| {
                this.set_filter(RoomListFilter::Invites, cx);
            }))
            .a11y_flags(A11yFlags::default())
            .size_full()
            .flex()
            .child(body)
            .children(self.render_link_prompt(&colors, cx))
            .children(self.render_delete_prompt(cx))
            .children(self.render_leave_prompt(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_shows_the_time_today_and_the_day_otherwise() {
        let now = 1_762_340_400_000;
        assert_eq!(activity_label(now, now).len(), 5);
        assert_eq!(activity_label(now - 24 * 60 * 60 * 1000, now), "Yesterday");
    }
}
