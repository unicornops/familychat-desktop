//! Fixtures for screenshots and view tests (PLAN.md, sections 5.3 and 9):
//! [`FixtureSession`], [`FixtureSignin`] and [`FixtureStartup`], the
//! fixture implementations of `familychat-core`'s `SessionApi`,
//! `SigninApi` and `StartupApi`.
//!
//! None of them touches the network, the keychain or the disk. They say
//! what the app says: the sign-in's sentences are `familychat-signin`'s
//! wording and the keychain and lost-key sentences `familychat-store`'s,
//! so a screenshot shows the real copy. The sign-in fixture follows the
//! flow's shape (a code is always confirmed before it is "redeemed"), not
//! its rules: it signs in whoever it is told to, and records every start
//! and answer, without their secrets, for the view tests.
//!
//! Each flow runs on a thread of its own and talks over the same
//! `tokio::sync` channels as the live sign-in, so the window awaits it
//! exactly as it awaits `LiveSignin`; there is no Tokio runtime.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread,
    time::Duration,
};

use familychat_core::{
    OwnedRoomId, OwnedUserId, RoomId, UserId,
    diff::ListDiff,
    encryption::{
        AuthOutcome, DeviceView, EncryptionStatus, IdentityWarning, KeyImport, SecretText,
        StoppedMessage, StoppedPerson, VerificationFlow, VerificationStep,
    },
    media::{DecodedImage, MediaError, MediaInfo},
    messaging::{
        Draft, MediaLimits, MessageRef, NewRoom, OutgoingMessage, RoomDone, RoomRequest,
        ServerDefaults,
    },
    notifications::IncomingNotification,
    rich_text::RichText,
    rooms::{
        Membership, RoomListCommand, RoomListFilter, RoomListLoading, RoomMember, RoomSummary,
    },
    session::{
        AccountDetails, AccountKey, AccountView, MediaReply, Reply, RoomListSubscription,
        SUBSCRIPTION_BUFFER, SessionApi, SessionError, SignedOut, TimelineSubscription,
    },
    settings::{MemorySettings, NotificationSettings},
    signin::{
        ConfirmCode, Finished, PasswordForm, PasswordKind, SigninAnswer, SigninApi, SigninFlow,
        SigninProblem, SigninStart, SigninStep, Working,
    },
    startup::{OpenState, Opened, StartupApi},
    sync::{AccountStatus, SyncState},
    timeline::{
        MessageBody, MessageKind, PaginationState, Reaction, ReplyPreview, SendProblem, SendState,
        TimelineCommand, TimelineContent, TimelineEvent, TimelineItem, TimelineItemId,
        TimelineItemKind, TimelineUpdate,
    },
};
use familychat_security::{KeychainError, matrix_html::message_text};
use familychat_signin::wording;
use familychat_store::{
    UnreadableReason, keychain_unavailable_message, linux_keychain_unavailable_message,
    unreadable_message,
};
use tokio::sync::{mpsc, oneshot, watch};

pub mod encryption;
pub mod rooms;

/// The fixture family's server name.
pub const FIXTURE_SERVER: &str = "smith.safechat.family";

/// The account the fixture's codes and passwords sign in.
pub const FIXTURE_USER: &str = "@ana:smith.safechat.family";

/// A sign-in link with a code, as a parent's QR code carries it.
pub const FIXTURE_LINK: &str = "https://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:%40ana%3Asmith.safechat.family&hs=smith.safechat.family&token=fixture-login-token";

/// The fixture account's homeserver URL.
pub const FIXTURE_HOMESERVER_URL: &str = "https://smith.safechat.family/";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fixture account's user id.
pub fn fixture_user() -> OwnedUserId {
    OwnedUserId::try_from(FIXTURE_USER).unwrap_or_else(|_| unreachable!("a valid user id"))
}

// ---------------------------------------------------------------------------
// FixtureSession
// ---------------------------------------------------------------------------

/// How long the fixture's list and timeline threads wait between looks at
/// their commands.
const POLL: Duration = Duration::from_millis(5);

/// What the fixture's media answers for a handle.
type FixtureImage = Result<Arc<DecodedImage>, MediaError>;

/// The fixture `SessionApi`: accounts and their sync states, rooms,
/// timelines, members and pictures, set by the test or the screenshot
/// fixture ([`FixtureSession::with_family`] loads the Smiths, and
/// [`FixtureSession::with_large`] the 1,000-room fixture). Nothing touches
/// the network, the keychain or the disk; every open list and timeline is
/// served by a thread of its own over the same channels as the live
/// session, and follows changes made here.
#[derive(Default)]
pub struct FixtureSession {
    accounts: watch::Sender<Vec<AccountView>>,
    statuses: Mutex<BTreeMap<AccountKey, watch::Sender<AccountStatus>>>,
    retried: Mutex<Vec<AccountKey>>,
    rooms: watch::Sender<Vec<RoomSummary>>,
    rooms_loading: Mutex<bool>,
    timelines: Mutex<BTreeMap<OwnedRoomId, watch::Sender<Vec<TimelineItem>>>>,
    members: Mutex<BTreeMap<OwnedRoomId, Vec<RoomMember>>>,
    images: Mutex<BTreeMap<String, FixtureImage>>,
    hold_pagination: Arc<Mutex<bool>>,
    record: Arc<Mutex<SessionRecord>>,
    notifications: Mutex<Option<mpsc::Sender<IncomingNotification>>>,
    unread: Mutex<BTreeMap<AccountKey, watch::Sender<u64>>>,
    media_limits: Mutex<Option<MediaLimits>>,
    server_defaults: Mutex<ServerDefaults>,
    drafts: Mutex<BTreeMap<(AccountKey, OwnedRoomId), Draft>>,
    fail_requests: Mutex<Option<SessionError>>,
    sent: Mutex<usize>,
    encryption: encryption::FixtureEncryption,
}

/// Family Chat's Palpo upload limit, which the fixture serves unless told
/// otherwise.
pub const FIXTURE_UPLOAD_LIMIT: u64 = 11 * 1024 * 1024;

/// What the window asked of the fixture session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecord {
    /// Each filter a room list was opened or set to, in order.
    pub filters: Vec<RoomListFilter>,
    /// Each timeline opened, in order.
    pub timelines: Vec<OwnedRoomId>,
    /// How many times older messages were asked for.
    pub paginations: usize,
    /// Each invitation answered: the room, and whether it was accepted.
    pub invites: Vec<(OwnedRoomId, bool)>,
    /// The handles of the images asked for.
    pub images: Vec<String>,
    /// Each file saved: its suggested name and where it went.
    pub saved: Vec<(String, PathBuf)>,
    /// Each room request, in order (messages, reactions, receipts, typing,
    /// files, invitations): what `familychat-actions` asked for.
    pub requests: Vec<(OwnedRoomId, RoomRequest)>,
    /// Each new room asked for.
    pub created: Vec<NewRoom>,
    /// Each draft kept (`None`: forgotten), by room.
    pub drafts: Vec<(OwnedRoomId, Option<Draft>)>,
    /// How many times the upload limit was asked for.
    pub media_limit_asks: usize,
    /// Each account signed out, in order.
    pub signed_out: Vec<AccountKey>,
}

impl SessionRecord {
    /// The room requests other than receipts and typing notices.
    pub fn writes(&self) -> Vec<&RoomRequest> {
        self.requests
            .iter()
            .map(|(_, request)| request)
            .filter(|request| {
                !matches!(
                    request,
                    RoomRequest::MarkRead | RoomRequest::MarkReadPrivately | RoomRequest::Typing(_)
                )
            })
            .collect()
    }
}

impl std::fmt::Debug for FixtureSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixtureSession")
            .field("accounts", &self.accounts.borrow().len())
            .field("rooms", &self.rooms.borrow().len())
            .finish_non_exhaustive()
    }
}

/// The rooms `filter` shows, as the live list's filters choose them.
pub fn filter_rooms(rooms: &[RoomSummary], filter: RoomListFilter) -> Vec<RoomSummary> {
    rooms
        .iter()
        .filter(|room| match filter {
            RoomListFilter::All => room.membership != Membership::Left,
            RoomListFilter::Unread => {
                room.membership == Membership::Joined && room.unread.is_unread()
            }
            RoomListFilter::People => room.is_direct && room.membership == Membership::Joined,
            RoomListFilter::Invites => room.membership == Membership::Invited,
        })
        .cloned()
        .collect()
}

impl FixtureSession {
    /// A session with no accounts, and Family Chat's upload limit.
    pub fn new() -> Arc<Self> {
        let session = Self::default();
        *lock(&session.media_limits) = Some(MediaLimits {
            max_upload_bytes: Some(FIXTURE_UPLOAD_LIMIT),
        });
        Arc::new(session)
    }

    /// The upload limit `media_limits` answers with (`None`: it fails, as
    /// an unreachable server's does).
    pub fn set_media_limits(&self, limits: Option<MediaLimits>) {
        *lock(&self.media_limits) = limits;
    }

    /// What the server's well-known says about new rooms.
    pub fn set_server_defaults(&self, defaults: ServerDefaults) {
        *lock(&self.server_defaults) = defaults;
    }

    /// Makes every room request and new room fail with `error` (`None`
    /// puts it back).
    pub fn fail_requests(&self, error: Option<SessionError>) {
        *lock(&self.fail_requests) = error;
    }

    /// Keeps a draft for a room, as one kept on an earlier visit.
    pub fn set_draft(&self, account: &AccountKey, room: &str, draft: Draft) {
        if let Ok(room) = OwnedRoomId::try_from(room) {
            lock(&self.drafts).insert((account.clone(), room), draft);
        }
    }

    /// The account's user id.
    fn own_user(&self, account: &AccountKey) -> Option<OwnedUserId> {
        self.accounts
            .borrow()
            .iter()
            .find(|view| &view.key == account)
            .map(|view| view.user_id.clone())
    }

    /// Carries out a room request on the fixture's rooms and timelines.
    fn carry_out(
        &self,
        account: &AccountKey,
        room: &familychat_core::RoomId,
        request: &RoomRequest,
    ) -> Result<RoomDone, SessionError> {
        let own = self.own_user(account).ok_or(SessionError::UnknownAccount)?;
        match request {
            RoomRequest::AnswerInvite(accept) => {
                lock(&self.record).invites.push((room.to_owned(), *accept));
                self.rooms.send_modify(|rooms| {
                    if *accept {
                        for summary in rooms.iter_mut().filter(|summary| summary.room_id == room) {
                            summary.membership = Membership::Joined;
                            summary.invited_by = None;
                        }
                    } else {
                        rooms.retain(|summary| summary.room_id != room);
                    }
                });
                return Ok(RoomDone::Done);
            }
            RoomRequest::Leave => {
                self.rooms
                    .send_modify(|rooms| rooms.retain(|summary| summary.room_id != room));
                return Ok(RoomDone::Done);
            }
            RoomRequest::MarkRead | RoomRequest::Typing(_) | RoomRequest::Invite(_) => {
                return Ok(RoomDone::Done);
            }
            _ => {}
        }
        let number = {
            let mut sent = lock(&self.sent);
            *sent += 1;
            *sent
        };
        let mut done = RoomDone::Done;
        let timelines = lock(&self.timelines);
        let timeline = timelines.get(room).ok_or(SessionError::UnknownRoom)?;
        timeline.send_modify(|items| {
            done = apply_request(items, request, &own, number);
        });
        Ok(done)
    }
}

/// A message the account sends, as the fixture's timeline shows it.
fn own_message(own: &OwnedUserId, number: usize, content: TimelineContent) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId(format!("fixture-sent-{number}")),
        kind: TimelineItemKind::Event(TimelineEvent {
            event_id: familychat_core::OwnedEventId::try_from(format!("$fixture-sent-{number}"))
                .ok(),
            sender: own.clone(),
            sender_name: Some("Ana".to_owned()),
            timestamp_ms: rooms::now_ms(),
            is_own: true,
            content,
            send_state: Some(SendState::Sent),
            in_reply_to: None,
            reactions: Vec::new(),
        }),
    }
}

fn message_content(message: &OutgoingMessage) -> TimelineContent {
    TimelineContent::Message {
        kind: MessageKind::Text,
        body: message.body.clone(),
        rich: message_text(
            message.body.as_str(),
            message.html.as_ref().map(MessageBody::as_str),
        ),
        edited: false,
        media: None,
    }
}

/// The event a request is about.
fn target_event<'a>(
    items: &'a mut [TimelineItem],
    target: &MessageRef,
) -> Option<&'a mut TimelineEvent> {
    items.iter_mut().find_map(|item| {
        let matches = item.id == target.item;
        match &mut item.kind {
            TimelineItemKind::Event(event)
                if matches || (target.event_id.is_some() && event.event_id == target.event_id) =>
            {
                Some(event)
            }
            _ => None,
        }
    })
}

/// Applies a message request to a fixture timeline, as the server and the
/// timeline would.
fn apply_request(
    items: &mut Vec<TimelineItem>,
    request: &RoomRequest,
    own: &OwnedUserId,
    number: usize,
) -> RoomDone {
    match request {
        RoomRequest::Send { message, reply_to } => {
            let in_reply_to = reply_to.as_ref().map(|event_id| {
                let answered = items.iter().find_map(|item| match &item.kind {
                    TimelineItemKind::Event(event) if event.event_id.as_ref() == Some(event_id) => {
                        Some(event.clone())
                    }
                    _ => None,
                });
                ReplyPreview {
                    event_id: event_id.clone(),
                    sender: answered
                        .as_ref()
                        .and_then(|event| event.sender_name.clone()),
                    text: answered.and_then(|event| match event.content {
                        TimelineContent::Message { body, .. } => Some(body),
                        _ => None,
                    }),
                }
            });
            let mut item = own_message(own, number, message_content(message));
            if let TimelineItemKind::Event(event) = &mut item.kind {
                event.in_reply_to = in_reply_to;
            }
            items.push(item);
        }
        RoomRequest::SendFile { file, .. } => {
            let kind = if file.mime_type.starts_with("image/") {
                MessageKind::Image
            } else {
                MessageKind::File
            };
            items.push(own_message(
                own,
                number,
                TimelineContent::Message {
                    kind,
                    body: MessageBody::new(file.file_name.clone()),
                    rich: RichText::plain(&file.file_name),
                    edited: false,
                    media: None,
                },
            ));
        }
        RoomRequest::Edit { target, message } => {
            if let Some(event) = target_event(items, target) {
                let mut content = message_content(message);
                if let TimelineContent::Message { edited, .. } = &mut content {
                    *edited = true;
                }
                event.content = content;
            }
        }
        RoomRequest::Redact { target, .. } => {
            if let Some(event) = target_event(items, target) {
                event.content = TimelineContent::Redacted;
                event.reactions.clear();
            }
        }
        RoomRequest::ToggleReaction { target, key } => {
            if let Some(event) = target_event(items, target) {
                let added = toggle(&mut event.reactions, key);
                return RoomDone::Reaction(added);
            }
        }
        RoomRequest::SendAnyway(target) => {
            if let Some(event) = target_event(items, target) {
                event.send_state = Some(SendState::Sent);
            }
        }
        RoomRequest::MarkRead
        | RoomRequest::MarkReadPrivately
        | RoomRequest::Typing(_)
        | RoomRequest::Invite(_)
        | RoomRequest::AnswerInvite(_)
        | RoomRequest::Leave => {}
    }
    RoomDone::Done
}

/// Adds or takes back the account's reaction `key`; `true` when added.
fn toggle(reactions: &mut Vec<Reaction>, key: &str) -> bool {
    match reactions.iter().position(|reaction| reaction.key == key) {
        Some(index) if reactions[index].own => {
            let reaction = &mut reactions[index];
            reaction.own = false;
            reaction.count = reaction.count.saturating_sub(1);
            reaction.senders.retain(|sender| sender != "Ana");
            if reaction.count == 0 {
                reactions.remove(index);
            }
            false
        }
        Some(index) => {
            let reaction = &mut reactions[index];
            reaction.own = true;
            reaction.count += 1;
            reaction.senders.push("Ana".to_owned());
            true
        }
        None => {
            reactions.push(Reaction {
                key: key.to_owned(),
                count: 1,
                own: true,
                senders: vec!["Ana".to_owned()],
            });
            true
        }
    }
}

impl FixtureSession {
    /// Adds a signed-in account in `sync`.
    pub fn add_account(&self, user_id: &UserId, sync: SyncState) -> AccountKey {
        let mut statuses = lock(&self.statuses);
        let key = AccountKey(format!("fixture-account-{}", statuses.len() + 1));
        statuses.insert(
            key.clone(),
            watch::channel(AccountStatus {
                sync,
                problem: None,
            })
            .0,
        );
        drop(statuses);
        self.encryption.add_account(&key);
        let server = user_id.server_name().to_string();
        self.accounts.send_modify(|accounts| {
            accounts.push(AccountView {
                key: key.clone(),
                user_id: user_id.to_owned(),
                homeserver_url: format!("https://{server}/"),
            });
        });
        key
    }

    /// Sets an account's encryption (every account starts settled:
    /// confirmed, with an identity and recovery).
    pub fn set_encryption(&self, account: &AccountKey, status: EncryptionStatus) {
        self.encryption.set_status(account, status);
    }

    /// The step the next verification starts at (the screenshot matrix).
    pub fn start_verification_at(&self, step: VerificationStep) {
        self.encryption.set_first_step(Some(step));
    }

    /// Another of the account's devices asks to verify, as a to-device
    /// request would arrive; `false` when no frontend listens.
    pub fn request_verification(&self, account: &AccountKey) -> bool {
        let Some(user) = self.own_user(account) else {
            return false;
        };
        self.encryption.push_request(account, user)
    }

    /// The identity warnings `room` shows.
    pub fn set_identity_warnings(&self, room: &str, warnings: Vec<IdentityWarning>) {
        if let Ok(room) = OwnedRoomId::try_from(room) {
            self.encryption.set_warnings(room, warnings);
        }
    }

    /// Makes every encryption request fail with `error` (`None` puts it
    /// back).
    pub fn fail_encryption(&self, error: Option<SessionError>) {
        self.encryption.fail(error);
    }

    /// What the window asked of the encryption, in order (never a secret).
    pub fn encryption_asks(&self) -> Vec<encryption::EncryptionAsk> {
        self.encryption.asks()
    }

    /// Marks the account's newest message in `room` as stopped by the
    /// encryption with `problem`, as a send queue that refused it would.
    pub fn stop_own_message(&self, room: &str, problem: SendProblem) {
        let Ok(room) = OwnedRoomId::try_from(room) else {
            return;
        };
        if let Some(timeline) = lock(&self.timelines).get(&room) {
            timeline.send_modify(|items| {
                if let Some(event) = items
                    .iter_mut()
                    .rev()
                    .find_map(|item| match &mut item.kind {
                        TimelineItemKind::Event(event) if event.is_own => Some(event),
                        _ => None,
                    })
                {
                    event.send_state = Some(SendState::Failed {
                        recoverable: false,
                        problem: Some(problem),
                    });
                }
            });
        }
    }

    /// Moves an account's sync to `sync`.
    pub fn set_sync(&self, account: &AccountKey, sync: SyncState) {
        if let Some(status) = lock(&self.statuses).get(account) {
            status.send_modify(|status| status.sync = sync);
        }
    }

    /// Reports `incoming` as an account's sync would; `false` when no
    /// frontend is listening.
    pub fn notify(&self, incoming: IncomingNotification) -> bool {
        lock(&self.notifications)
            .as_ref()
            .is_some_and(|sender| sender.try_send(incoming).is_ok())
    }

    /// Sets an account's unread total.
    pub fn set_unread(&self, account: &AccountKey, unread: u64) {
        lock(&self.unread)
            .entry(account.clone())
            .or_insert_with(|| watch::channel(0).0)
            .send_replace(unread);
    }

    /// The accounts `retry_sync` was asked for, in order.
    pub fn retried(&self) -> Vec<AccountKey> {
        lock(&self.retried).clone()
    }

    /// Loads the Smith family's rooms, timelines, members and pictures.
    pub fn with_family(&self) {
        self.with_family_at(rooms::now_ms());
    }

    /// [`FixtureSession::with_family`], its times counted back from `now`
    /// (milliseconds since the Unix epoch), so a snapshot test draws the
    /// same times on every run.
    pub fn with_family_at(&self, now: u64) {
        let family = rooms::family_rooms(now);
        for room in &family {
            let id = room.room_id.as_str();
            self.set_timeline(id, rooms::family_timeline_for(id, now));
            lock(&self.members).insert(room.room_id.clone(), rooms::family_members(id));
        }
        self.set_rooms(family);
        let mut images = lock(&self.images);
        images.insert(rooms::SVG_HANDLE.to_owned(), Err(MediaError::Svg));
        images.insert(
            rooms::HUGE_HANDLE.to_owned(),
            Err(MediaError::TooManyPixels),
        );
    }

    /// Loads `rooms` rooms, the first with `events` events (the 1,000-room,
    /// 10,000-event fixture of PLAN.md section 13.6).
    pub fn with_large(&self, rooms: usize, events: usize) {
        self.with_large_at(rooms, events, rooms::now_ms());
    }

    /// [`FixtureSession::with_large`], its times counted back from `now`.
    pub fn with_large_at(&self, rooms: usize, events: usize, now: u64) {
        let list = rooms::large_rooms(rooms, now);
        if let Some(first) = list.first() {
            self.set_timeline(first.room_id.as_str(), rooms::large_timeline(events, now));
            lock(&self.members).insert(
                first.room_id.clone(),
                rooms::family_members(rooms::FAMILY_ROOM),
            );
        }
        // The next rooms have a short timeline each, so moving through the
        // list opens real rooms.
        let short = rooms::family_timeline_for(rooms::FAMILY_ROOM, now);
        for room in list.iter().skip(1).take(64) {
            self.set_timeline(room.room_id.as_str(), short.clone());
        }
        self.set_rooms(list);
    }

    /// Loads rooms whose names, topics, members, messages, links, file
    /// names and reactions are full of terminal escape sequences, bidi
    /// overrides and other controls (`rooms::hostile_rooms`), for the
    /// terminal UI's hostile-text test.
    pub fn with_hostile_at(&self, now: u64) {
        let hostile = rooms::hostile_rooms(now);
        for room in &hostile {
            let id = room.room_id.as_str();
            self.set_timeline(id, rooms::hostile_timeline(id, now));
            lock(&self.members).insert(room.room_id.clone(), rooms::hostile_members());
        }
        self.set_rooms(hostile);
    }

    /// Replaces the rooms; open lists follow.
    pub fn set_rooms(&self, rooms: Vec<RoomSummary>) {
        self.rooms.send_replace(rooms);
    }

    /// The rooms.
    pub fn rooms(&self) -> Vec<RoomSummary> {
        self.rooms.borrow().clone()
    }

    /// Whether room lists open as still loading (the first sync has not
    /// answered); they load when this is set back.
    pub fn set_rooms_loading(&self, loading: bool) {
        *lock(&self.rooms_loading) = loading;
    }

    /// Replaces a room's timeline; an open timeline follows.
    pub fn set_timeline(&self, room: &str, items: Vec<TimelineItem>) {
        let Ok(room) = OwnedRoomId::try_from(room) else {
            return;
        };
        let mut timelines = lock(&self.timelines);
        match timelines.get(&room) {
            Some(timeline) => {
                timeline.send_replace(items);
            }
            None => {
                timelines.insert(room, watch::channel(items).0);
            }
        }
    }

    /// Changes a room's timeline in place; an open timeline follows.
    pub fn update_timeline(&self, room: &str, change: impl FnOnce(&mut Vec<TimelineItem>)) {
        let Ok(room) = OwnedRoomId::try_from(room) else {
            return;
        };
        if let Some(timeline) = lock(&self.timelines).get(&room) {
            timeline.send_modify(change);
        }
    }

    /// The picture (or refusal) a media handle answers with.
    pub fn set_image(&self, handle: &str, image: Result<DecodedImage, MediaError>) {
        lock(&self.images).insert(handle.to_owned(), image.map(Arc::new));
    }

    /// Keeps older-message loads at "loading" (the screenshot of a
    /// timeline loading its history).
    pub fn hold_pagination(&self, hold: bool) {
        *lock(&self.hold_pagination) = hold;
    }

    /// What the window asked for so far.
    pub fn record(&self) -> SessionRecord {
        lock(&self.record).clone()
    }

    fn image_for(&self, media: &MediaInfo) -> FixtureImage {
        if let Some(image) = lock(&self.images).get(media.source.encoded()) {
            return image.clone();
        }
        if media.is_svg() {
            return Err(MediaError::Svg);
        }
        let (width, height) = media.declared_size().unwrap_or((640, 420));
        Ok(Arc::new(rooms::picture(
            media.source.encoded(),
            width,
            height,
        )))
    }
}

/// Serves an open room list: the filtered rooms, again after each filter
/// change or room change, until the window drops it.
fn serve_room_list(
    rooms: watch::Receiver<Vec<RoomSummary>>,
    mut filter: RoomListFilter,
    updates: mpsc::Sender<Vec<ListDiff<RoomSummary>>>,
    mut commands: mpsc::UnboundedReceiver<RoomListCommand>,
    record: Arc<Mutex<SessionRecord>>,
) {
    let mut rooms = rooms;
    loop {
        if updates.is_closed() {
            return;
        }
        let mut changed = rooms.has_changed().unwrap_or(false);
        while let Ok(command) = commands.try_recv() {
            if let RoomListCommand::SetFilter(new) = command {
                filter = new;
                changed = true;
                lock(&record).filters.push(new);
            }
        }
        if changed {
            let values = filter_rooms(&rooms.borrow_and_update(), filter);
            if updates
                .blocking_send(vec![ListDiff::Reset { values }])
                .is_err()
            {
                return;
            }
        }
        thread::sleep(POLL);
    }
}

/// Serves an open timeline: its items, again after each change, and
/// "nothing older" (or "loading", held) for each request for history.
fn serve_timeline(
    items: watch::Receiver<Vec<TimelineItem>>,
    updates: mpsc::Sender<TimelineUpdate>,
    mut commands: mpsc::UnboundedReceiver<TimelineCommand>,
    record: Arc<Mutex<SessionRecord>>,
    hold: Arc<Mutex<bool>>,
) {
    let mut items = items;
    loop {
        if updates.is_closed() {
            return;
        }
        while let Ok(TimelineCommand::PaginateBackwards { .. }) = commands.try_recv() {
            lock(&record).paginations += 1;
            let state = if *lock(&hold) {
                PaginationState::Loading
            } else {
                PaginationState::ReachedStart
            };
            if updates
                .blocking_send(TimelineUpdate::Pagination(state))
                .is_err()
            {
                return;
            }
        }
        if items.has_changed().unwrap_or(false) {
            let values = items.borrow_and_update().clone();
            if updates
                .blocking_send(TimelineUpdate::Items(vec![ListDiff::Reset { values }]))
                .is_err()
            {
                return;
            }
        }
        thread::sleep(POLL);
    }
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .unwrap_or_else(|error| panic!("the fixture thread starts: {error}"));
}

/// An answer on a fresh oneshot.
fn answered<T>(value: T) -> oneshot::Receiver<T> {
    let (answer, reply) = oneshot::channel();
    let _ = answer.send(value);
    reply
}

impl SessionApi for FixtureSession {
    fn accounts(&self) -> watch::Receiver<Vec<AccountView>> {
        self.accounts.subscribe()
    }

    fn account_status(&self, account: &AccountKey) -> Option<watch::Receiver<AccountStatus>> {
        lock(&self.statuses)
            .get(account)
            .map(watch::Sender::subscribe)
    }

    fn room_list(
        &self,
        account: &AccountKey,
        filter: RoomListFilter,
    ) -> Reply<RoomListSubscription> {
        if !lock(&self.statuses).contains_key(account) {
            return answered(Err(SessionError::UnknownAccount));
        }
        lock(&self.record).filters.push(filter);
        let (updates_tx, updates) = mpsc::channel(SUBSCRIPTION_BUFFER);
        let loading_now = *lock(&self.rooms_loading);
        let (loading_tx, loading) = watch::channel(if loading_now {
            RoomListLoading::NotLoaded
        } else {
            RoomListLoading::Loaded {
                total: u32::try_from(self.rooms.borrow().len()).ok(),
            }
        });
        let (commands_tx, commands) = mpsc::unbounded_channel();
        let mut rooms = self.rooms.subscribe();
        if !loading_now {
            // The first batch is there at once, as a synced list's is.
            let values = filter_rooms(&rooms.borrow_and_update(), filter);
            let _ = updates_tx.try_send(vec![ListDiff::Reset { values }]);
        }
        let record = self.record.clone();
        spawn("familychat-fixture-rooms", move || {
            // Keeps the loading state's sender alive with the list.
            let _loading = loading_tx;
            serve_room_list(rooms, filter, updates_tx, commands, record);
        });
        answered(Ok(RoomListSubscription::new(updates, loading, commands_tx)))
    }

    fn timeline(
        &self,
        account: &AccountKey,
        room: &familychat_core::RoomId,
    ) -> Reply<TimelineSubscription> {
        if !lock(&self.statuses).contains_key(account) {
            return answered(Err(SessionError::UnknownAccount));
        }
        let Some(mut items) = lock(&self.timelines)
            .get(room)
            .map(watch::Sender::subscribe)
        else {
            return answered(Err(SessionError::UnknownRoom));
        };
        lock(&self.record).timelines.push(room.to_owned());
        let (updates_tx, updates) = mpsc::channel(SUBSCRIPTION_BUFFER);
        let values = items.borrow_and_update().clone();
        let _ = updates_tx.try_send(TimelineUpdate::Items(vec![ListDiff::Reset { values }]));
        let (commands_tx, commands) = mpsc::unbounded_channel();
        let (record, hold) = (self.record.clone(), self.hold_pagination.clone());
        spawn("familychat-fixture-timeline", move || {
            serve_timeline(items, updates_tx, commands, record, hold);
        });
        answered(Ok(TimelineSubscription::new(updates, commands_tx)))
    }

    fn members(
        &self,
        _account: &AccountKey,
        room: &familychat_core::RoomId,
    ) -> Reply<Vec<RoomMember>> {
        answered(
            lock(&self.members)
                .get(room)
                .cloned()
                .ok_or(SessionError::UnknownRoom),
        )
    }

    fn room_action(
        &self,
        account: &AccountKey,
        room: &familychat_core::RoomId,
        request: RoomRequest,
    ) -> Reply<RoomDone> {
        if !lock(&self.statuses).contains_key(account) {
            return answered(Err(SessionError::UnknownAccount));
        }
        lock(&self.record)
            .requests
            .push((room.to_owned(), request.clone()));
        if let Some(error) = lock(&self.fail_requests).clone() {
            return answered(Err(error));
        }
        answered(self.carry_out(account, room, &request))
    }

    fn create_room(&self, account: &AccountKey, new_room: NewRoom) -> Reply<OwnedRoomId> {
        if !lock(&self.statuses).contains_key(account) {
            return answered(Err(SessionError::UnknownAccount));
        }
        lock(&self.record).created.push(new_room.clone());
        if let Some(error) = lock(&self.fail_requests).clone() {
            return answered(Err(error));
        }
        let number = {
            let mut sent = lock(&self.sent);
            *sent += 1;
            *sent
        };
        let Ok(room) = OwnedRoomId::try_from(format!("!fixture-new-{number}:{FIXTURE_SERVER}"))
        else {
            return answered(Err(SessionError::Failed("no room id".to_owned())));
        };
        let mut summary = RoomSummary::new(
            room.clone(),
            new_room
                .name
                .clone()
                .unwrap_or_else(|| "New chat".to_owned()),
        );
        summary.is_direct = new_room.direct;
        summary.encryption = if new_room.encrypted {
            familychat_core::rooms::Encryption::Encrypted
        } else {
            familychat_core::rooms::Encryption::NotEncrypted
        };
        summary.joined_members = 1;
        self.set_timeline(room.as_str(), Vec::new());
        self.rooms.send_modify(|rooms| rooms.insert(0, summary));
        answered(Ok(room))
    }

    fn media_limits(&self, _account: &AccountKey) -> Reply<MediaLimits> {
        lock(&self.record).media_limit_asks += 1;
        answered(
            lock(&self.media_limits)
                .ok_or_else(|| SessionError::Failed("The server can't be reached.".to_owned())),
        )
    }

    fn server_defaults(&self, _account: &AccountKey) -> Reply<ServerDefaults> {
        answered(Ok(*lock(&self.server_defaults)))
    }

    fn draft(&self, account: &AccountKey, room: &familychat_core::RoomId) -> Reply<Option<Draft>> {
        answered(Ok(lock(&self.drafts)
            .get(&(account.clone(), room.to_owned()))
            .cloned()))
    }

    fn save_draft(
        &self,
        account: &AccountKey,
        room: &familychat_core::RoomId,
        draft: Option<Draft>,
    ) -> Reply<()> {
        lock(&self.record)
            .drafts
            .push((room.to_owned(), draft.clone()));
        let key = (account.clone(), room.to_owned());
        let mut drafts = lock(&self.drafts);
        match draft {
            Some(draft) => {
                drafts.insert(key, draft);
            }
            None => {
                drafts.remove(&key);
            }
        }
        answered(Ok(()))
    }

    fn image(&self, _account: &AccountKey, media: &MediaInfo) -> MediaReply<Arc<DecodedImage>> {
        lock(&self.record)
            .images
            .push(media.source.encoded().to_owned());
        answered(self.image_for(media))
    }

    fn save_file(&self, _account: &AccountKey, media: &MediaInfo, path: PathBuf) -> MediaReply<()> {
        lock(&self.record)
            .saved
            .push((media.filename.clone(), path));
        answered(Ok(()))
    }

    fn retry_sync(&self, account: &AccountKey) {
        lock(&self.retried).push(account.clone());
    }

    fn notifications(&self) -> mpsc::Receiver<IncomingNotification> {
        let (sender, receiver) = mpsc::channel(SUBSCRIPTION_BUFFER);
        *lock(&self.notifications) = Some(sender);
        receiver
    }

    fn unread_total(&self, account: &AccountKey) -> Option<watch::Receiver<u64>> {
        if !lock(&self.statuses).contains_key(account) {
            return None;
        }
        Some(
            lock(&self.unread)
                .entry(account.clone())
                .or_insert_with(|| watch::channel(0).0)
                .subscribe(),
        )
    }

    fn encryption(&self, account: &AccountKey) -> Option<watch::Receiver<EncryptionStatus>> {
        self.encryption.status(account)
    }

    fn set_up_identity(
        &self,
        account: &AccountKey,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome> {
        answered(self.encryption.set_up_identity(account, password))
    }

    fn verify_this_device(&self, account: &AccountKey) -> Reply<VerificationFlow> {
        let Some(user) = self.own_user(account) else {
            return answered(Err(SessionError::UnknownAccount));
        };
        answered(self.encryption.verify_this_device(account, user))
    }

    fn verify_device(&self, account: &AccountKey, device_id: &str) -> Reply<VerificationFlow> {
        let Some(user) = self.own_user(account) else {
            return answered(Err(SessionError::UnknownAccount));
        };
        answered(self.encryption.verify_device(account, user, device_id))
    }

    fn verification_requests(
        &self,
    ) -> mpsc::Receiver<familychat_core::session::IncomingVerification> {
        self.encryption.subscribe_requests()
    }

    fn enable_recovery(&self, account: &AccountKey) -> Reply<SecretText> {
        answered(self.encryption.enable_recovery(account))
    }

    fn recover(&self, account: &AccountKey, recovery_key: SecretText) -> Reply<()> {
        answered(self.encryption.recover(account, recovery_key))
    }

    fn devices(&self, account: &AccountKey) -> Reply<Vec<DeviceView>> {
        answered(self.encryption.devices(account))
    }

    fn sign_out_device(
        &self,
        account: &AccountKey,
        device_id: &str,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome> {
        answered(
            self.encryption
                .sign_out_device(account, device_id, password),
        )
    }

    fn export_room_keys(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Reply<()> {
        answered(self.encryption.export(account, path, passphrase))
    }

    fn import_room_keys(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Reply<KeyImport> {
        answered(self.encryption.import(account, path, passphrase))
    }

    fn identity_warnings(
        &self,
        account: &AccountKey,
        room: &familychat_core::RoomId,
    ) -> Reply<watch::Receiver<Vec<IdentityWarning>>> {
        if !lock(&self.statuses).contains_key(account) {
            return answered(Err(SessionError::UnknownAccount));
        }
        answered(Ok(self.encryption.warnings(&room.to_owned())))
    }

    fn acknowledge_identity(&self, account: &AccountKey, user: &UserId) -> Reply<()> {
        if !lock(&self.statuses).contains_key(account) {
            return answered(Err(SessionError::UnknownAccount));
        }
        answered(self.encryption.acknowledge(user))
    }

    fn stopped_message(
        &self,
        account: &AccountKey,
        room: &RoomId,
        message: MessageRef,
    ) -> Reply<StoppedMessage> {
        if let Err(error) = self.encryption.stopped(account) {
            return answered(Err(error));
        }
        let not_there =
            || SessionError::Failed("That message isn't waiting to be sent any more.".to_owned());
        let problem = lock(&self.timelines).get(room).and_then(|timeline| {
            let mut items = timeline.borrow().clone();
            target_event(&mut items, &message).and_then(|event| match &event.send_state {
                Some(SendState::Failed { problem, .. }) => *problem,
                _ => None,
            })
        });
        let Some(problem) = problem else {
            return answered(Err(not_there()));
        };
        let (devices, people) = match problem {
            SendProblem::UnconfirmedDevices => (encryption::fixture_stopped_devices(), Vec::new()),
            SendProblem::IdentityChanged => (
                Vec::new(),
                vec![StoppedPerson {
                    user_id: OwnedUserId::try_from("@dad:smith.safechat.family").expect("an id"),
                    name: Some("Dad".to_owned()),
                }],
            ),
            SendProblem::ConfirmThisDevice => (Vec::new(), Vec::new()),
        };
        answered(Ok(StoppedMessage {
            problem,
            devices,
            people,
        }))
    }

    fn reset_recovery_key(&self, account: &AccountKey) -> Reply<SecretText> {
        answered(self.encryption.reset_recovery_key(account))
    }

    fn reset_identity(
        &self,
        account: &AccountKey,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome> {
        answered(self.encryption.reset_identity(account, password))
    }

    fn account_details(&self, account: &AccountKey) -> Reply<AccountDetails> {
        let Some(view) = self
            .accounts
            .borrow()
            .iter()
            .find(|view| &view.key == account)
            .cloned()
        else {
            return answered(Err(SessionError::UnknownAccount));
        };
        let server = view.user_id.server_name().to_string();
        let family_chat = server == "safechat.family" || server.ends_with(".safechat.family");
        let display_name = (view.user_id.localpart() == "ana").then(|| "Ana Smith".to_owned());
        answered(Ok(AccountDetails {
            user_id: view.user_id,
            display_name,
            homeserver_url: view.homeserver_url.trim_end_matches('/').to_owned(),
            device_id: "FAMLAPTOP".to_owned(),
            device_name: Some("Family Chat (Linux)".to_owned()),
            family_chat,
        }))
    }

    fn sign_out(&self, account: &AccountKey) -> Reply<SignedOut> {
        if lock(&self.statuses).remove(account).is_none() {
            return answered(Err(SessionError::UnknownAccount));
        }
        lock(&self.record).signed_out.push(account.clone());
        self.accounts
            .send_modify(|accounts| accounts.retain(|view| &view.key != account));
        answered(Ok(SignedOut::Everywhere))
    }
}

// ---------------------------------------------------------------------------
// FixtureSignin
// ---------------------------------------------------------------------------

/// How a flow started, without the link's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureStart {
    /// A pasted or decoded link; whether it had a code in it.
    Link {
        /// The text had a `token=` parameter.
        with_code: bool,
    },
    /// "Sign in with your password".
    FamilyChatPassword,
    /// "Use another Matrix server".
    OtherServer,
}

/// An answer the fixture received, without the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureAnswer {
    /// Use the code: the fixture "redeems" it.
    Continue,
    /// Don't use the code.
    UsePassword,
    /// A password form was submitted.
    SubmitPassword {
        /// The server field.
        server: String,
        /// The username field.
        username: String,
        /// Whether the password was not empty.
        with_password: bool,
    },
    /// Stop.
    Cancel,
}

/// What the fixture recorded, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureEvent {
    /// A flow started.
    Started(FixtureStart),
    /// The step the flow showed.
    Showed(SigninStep),
    /// The person answered.
    Answered(FixtureAnswer),
    /// The fixture redeemed a code (it only does after `Continue` on a
    /// confirmation).
    Redeemed,
}

/// What a redeemed code or a submitted password leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureOutcome {
    /// The account signs in.
    SignIn,
    /// Back to the password form with this problem.
    Problem(SigninProblem),
}

#[derive(Debug)]
struct SigninScript {
    first_step: Option<SigninStep>,
    code: FixtureOutcome,
    password: FixtureOutcome,
}

/// The fixture `SigninApi`.
pub struct FixtureSignin {
    session: Arc<FixtureSession>,
    script: Mutex<SigninScript>,
    events: Arc<Mutex<Vec<FixtureEvent>>>,
}

impl std::fmt::Debug for FixtureSignin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixtureSignin").finish_non_exhaustive()
    }
}

impl FixtureSignin {
    /// A sign-in whose codes and passwords sign [`FIXTURE_USER`] in to
    /// `session`.
    pub fn new(session: Arc<FixtureSession>) -> Arc<Self> {
        Arc::new(Self {
            session,
            script: Mutex::new(SigninScript {
                first_step: None,
                code: FixtureOutcome::SignIn,
                password: FixtureOutcome::SignIn,
            }),
            events: Arc::default(),
        })
    }

    /// The next flow shows `step` first, whatever it starts with (the
    /// screenshot fixture uses it to show one step and stay there).
    pub fn start_at(&self, step: SigninStep) {
        lock(&self.script).first_step = Some(step);
    }

    /// What a redeemed code leads to.
    pub fn set_code_outcome(&self, outcome: FixtureOutcome) {
        lock(&self.script).code = outcome;
    }

    /// What a submitted password leads to.
    pub fn set_password_outcome(&self, outcome: FixtureOutcome) {
        lock(&self.script).password = outcome;
    }

    /// Everything recorded so far.
    pub fn events(&self) -> Vec<FixtureEvent> {
        lock(&self.events).clone()
    }

    /// The answers recorded so far.
    pub fn answers(&self) -> Vec<FixtureAnswer> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                FixtureEvent::Answered(answer) => Some(answer),
                _ => None,
            })
            .collect()
    }

    /// How many codes were redeemed.
    pub fn redeemed(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| **event == FixtureEvent::Redeemed)
            .count()
    }
}

impl SigninApi for FixtureSignin {
    fn start(&self, start: SigninStart) -> SigninFlow {
        let (kind, first) = match &start {
            SigninStart::Link(text) => {
                let text = text.expose().trim();
                let with_code = text.contains("token=");
                let is_link = (text.starts_with("https://") || text.starts_with("familychat://"))
                    && text.contains("/app/login");
                let first = if !is_link {
                    SigninStep::NotALink(wording::NOT_A_LINK.to_owned())
                } else if with_code {
                    confirm_step()
                } else {
                    SigninStep::Password(PasswordForm {
                        notice: None,
                        ..link_form()
                    })
                };
                (FixtureStart::Link { with_code }, first)
            }
            SigninStart::FamilyChatPassword => (
                FixtureStart::FamilyChatPassword,
                SigninStep::Password(blank_form(PasswordKind::FamilyChat)),
            ),
            SigninStart::OtherServer => (
                FixtureStart::OtherServer,
                SigninStep::Password(blank_form(PasswordKind::OtherServer)),
            ),
        };
        drop(start);
        let (script, events) = {
            let mut script = lock(&self.script);
            let first = script.first_step.take().unwrap_or(first);
            (
                (first, script.code.clone(), script.password.clone()),
                self.events.clone(),
            )
        };
        lock(&events).push(FixtureEvent::Started(kind));
        let (steps, receiver) = watch::channel(SigninStep::Starting);
        let (answers, answers_receiver) = mpsc::unbounded_channel();
        let flow = FixtureFlow {
            session: self.session.clone(),
            events,
            steps,
            answers: answers_receiver,
            code: script.1,
            password: script.2,
        };
        let first = script.0;
        thread::Builder::new()
            .name("familychat-fixture-signin".to_owned())
            .spawn(move || flow.run(first))
            .unwrap_or_else(|error| panic!("the fixture sign-in thread starts: {error}"));
        SigninFlow::new(receiver, answers)
    }
}

fn blank_form(kind: PasswordKind) -> PasswordForm {
    PasswordForm {
        kind,
        server: String::new(),
        server_placeholder: match kind {
            PasswordKind::FamilyChat => wording::FAMILY_SERVER_PLACEHOLDER.to_owned(),
            PasswordKind::OtherServer => "matrix.org".to_owned(),
        },
        username: String::new(),
        notice: None,
        problem: None,
    }
}

/// The password form a fixture link leads to: its server and hint.
fn link_form() -> PasswordForm {
    PasswordForm {
        server: FIXTURE_SERVER.to_owned(),
        username: "ana".to_owned(),
        ..blank_form(PasswordKind::FamilyChat)
    }
}

/// The confirmation for the fixture link.
pub fn confirm_step() -> SigninStep {
    let user = fixture_user();
    SigninStep::Confirm(ConfirmCode {
        question: wording::confirm_user(&user),
        detail: wording::CONFIRM_DETAIL.to_owned(),
        user_id: Some(user),
        server: FIXTURE_SERVER.to_owned(),
    })
}

/// The steps the screenshot fixture shows (each in dark and light).
pub mod steps {
    use super::*;

    /// The password form after a used or expired code (the contract's
    /// password fallback).
    pub fn code_used_or_expired() -> SigninStep {
        SigninStep::Password(PasswordForm {
            problem: Some(SigninProblem::new(wording::CODE_USED_OR_EXPIRED)),
            ..link_form()
        })
    }

    /// The password form after a link whose code could not be used.
    pub fn code_not_used() -> SigninStep {
        SigninStep::Password(PasswordForm {
            notice: Some(wording::CODE_NOT_USED.to_owned()),
            ..link_form()
        })
    }

    /// The Family Chat password form, filled in, after a wrong password.
    pub fn wrong_password() -> SigninStep {
        SigninStep::Password(PasswordForm {
            problem: Some(SigninProblem::new(wording::WRONG_PASSWORD)),
            ..link_form()
        })
    }

    /// The Family Chat password form after `M_LIMIT_EXCEEDED`.
    pub fn rate_limited(wait: Duration) -> SigninStep {
        SigninStep::Password(PasswordForm {
            problem: Some(SigninProblem {
                message: wording::rate_limited(Some(wait)),
                retry_after: Some(wait),
            }),
            ..link_form()
        })
    }

    /// Another server's form after a server outside the allowlist was
    /// refused (shown on the Family Chat form).
    pub fn not_family_chat() -> SigninStep {
        SigninStep::Password(PasswordForm {
            server: "matrix.example.org".to_owned(),
            username: "ana".to_owned(),
            problem: Some(SigninProblem::new(wording::NOT_FAMILY_CHAT)),
            ..blank_form(PasswordKind::FamilyChat)
        })
    }

    /// The account is signed in already.
    pub fn already_signed_in() -> SigninStep {
        SigninStep::AlreadySignedIn(wording::already_signed_in(&fixture_user()))
    }
}

struct FixtureFlow {
    session: Arc<FixtureSession>,
    events: Arc<Mutex<Vec<FixtureEvent>>>,
    steps: watch::Sender<SigninStep>,
    answers: mpsc::UnboundedReceiver<SigninAnswer>,
    code: FixtureOutcome,
    password: FixtureOutcome,
}

impl FixtureFlow {
    fn record(&self, event: FixtureEvent) {
        lock(&self.events).push(event);
    }

    fn show(&self, step: SigninStep) {
        self.record(FixtureEvent::Showed(step.clone()));
        self.steps.send_replace(step);
    }

    fn answer(&mut self) -> Option<SigninAnswer> {
        let answer = self.answers.blocking_recv();
        let recorded = match &answer {
            Some(SigninAnswer::Continue) => FixtureAnswer::Continue,
            Some(SigninAnswer::UsePassword) => FixtureAnswer::UsePassword,
            Some(SigninAnswer::SubmitPassword {
                server,
                username,
                password,
            }) => FixtureAnswer::SubmitPassword {
                server: server.clone(),
                username: username.clone(),
                with_password: !password.expose().is_empty(),
            },
            Some(SigninAnswer::Cancel) | None => FixtureAnswer::Cancel,
        };
        self.record(FixtureEvent::Answered(recorded));
        answer
    }

    fn finish(&self) -> SigninStep {
        let user_id = fixture_user();
        let account = self.session.add_account(&user_id, SyncState::Running);
        SigninStep::Finished(Finished { account, user_id })
    }

    fn run(mut self, first: SigninStep) {
        let mut step = first;
        loop {
            self.show(step.clone());
            step = match step.clone() {
                SigninStep::Confirm(_) => match self.answer() {
                    Some(SigninAnswer::Continue) => {
                        self.show(SigninStep::Working(Working::RedeemingCode));
                        self.record(FixtureEvent::Redeemed);
                        match self.code.clone() {
                            FixtureOutcome::SignIn => self.finish(),
                            FixtureOutcome::Problem(problem) => {
                                SigninStep::Password(PasswordForm {
                                    problem: Some(problem),
                                    ..link_form()
                                })
                            }
                        }
                    }
                    Some(SigninAnswer::UsePassword) => SigninStep::Password(link_form()),
                    Some(SigninAnswer::Cancel) | None => SigninStep::Cancelled,
                    // The live flow ignores a password here, and so does
                    // the fixture.
                    Some(SigninAnswer::SubmitPassword { .. }) => continue,
                },
                SigninStep::Password(mut form) => match self.answer() {
                    Some(SigninAnswer::SubmitPassword {
                        server, username, ..
                    }) => {
                        self.show(SigninStep::Working(Working::SigningIn));
                        match self.password.clone() {
                            FixtureOutcome::SignIn => self.finish(),
                            FixtureOutcome::Problem(problem) => {
                                form.server = server;
                                form.username = username;
                                form.problem = Some(problem);
                                SigninStep::Password(form)
                            }
                        }
                    }
                    Some(SigninAnswer::Cancel) | None => SigninStep::Cancelled,
                    Some(SigninAnswer::Continue | SigninAnswer::UsePassword) => {
                        continue;
                    }
                },
                // Shown and kept (the screenshot fixture's working steps).
                SigninStep::Starting | SigninStep::Working(_) => loop {
                    match self.answer() {
                        Some(SigninAnswer::Cancel) | None => break SigninStep::Cancelled,
                        Some(_) => {}
                    }
                },
                SigninStep::Finished(_)
                | SigninStep::AlreadySignedIn(_)
                | SigninStep::NotALink(_)
                | SigninStep::Cancelled => return,
            };
        }
    }
}

// ---------------------------------------------------------------------------
// FixtureStartup
// ---------------------------------------------------------------------------

/// What [`FixtureStartup`] opens as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureProfile {
    /// Open, with the fixture session and sign-in.
    Ready,
    /// The keychain is locked.
    KeychainLocked,
    /// The keychain can't be reached (no Secret Service on Linux).
    KeychainUnreachable,
    /// The data's key is gone.
    LostKey,
}

/// The fixture `StartupApi`: answers each `open` with the next state of a
/// sequence (staying on the last), and Start over with the ready profile.
pub struct FixtureStartup {
    session: Arc<FixtureSession>,
    signin: Arc<FixtureSignin>,
    settings: Arc<MemorySettings>,
    sequence: Mutex<Vec<FixtureProfile>>,
    opens: Mutex<usize>,
    started_over: Mutex<bool>,
}

impl std::fmt::Debug for FixtureStartup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixtureStartup")
            .field("sequence", &*lock(&self.sequence))
            .finish_non_exhaustive()
    }
}

/// Where the fixture's Start over says the old data went.
pub const FIXTURE_MOVED_TO: &str = "unreadable-20261105T093000Z";

impl FixtureStartup {
    /// Opens as each of `sequence` in turn, over a new fixture session and
    /// sign-in.
    pub fn new(sequence: &[FixtureProfile]) -> Arc<Self> {
        let session = FixtureSession::new();
        let signin = FixtureSignin::new(session.clone());
        Self::with(session, signin, sequence)
    }

    /// Opens as each of `sequence` in turn, over `session` and `signin`.
    pub fn with(
        session: Arc<FixtureSession>,
        signin: Arc<FixtureSignin>,
        sequence: &[FixtureProfile],
    ) -> Arc<Self> {
        Arc::new(Self {
            session,
            signin,
            settings: Arc::new(MemorySettings::default()),
            sequence: Mutex::new(sequence.to_vec()),
            opens: Mutex::new(0),
            started_over: Mutex::new(false),
        })
    }

    /// The fixture session.
    pub fn session(&self) -> &Arc<FixtureSession> {
        &self.session
    }

    /// The fixture sign-in.
    pub fn signin(&self) -> &Arc<FixtureSignin> {
        &self.signin
    }

    /// The fixture settings (in memory).
    pub fn settings(&self) -> &Arc<MemorySettings> {
        &self.settings
    }

    /// Starts with the notifications' content hidden or shown.
    pub fn set_notification_settings(&self, settings: NotificationSettings) {
        use familychat_core::settings::SettingsApi as _;
        drop(self.settings.set_notification_settings(settings));
    }

    /// How many times the window asked to open (Try again included).
    pub fn opens(&self) -> usize {
        *lock(&self.opens)
    }

    /// Whether the window asked to start over.
    pub fn started_over(&self) -> bool {
        *lock(&self.started_over)
    }

    fn state(&self, profile: FixtureProfile, notice: Option<String>) -> OpenState {
        match profile {
            FixtureProfile::Ready => OpenState::Ready(Opened {
                session: self.session.clone(),
                signin: self.signin.clone(),
                settings: self.settings.clone(),
                notice,
            }),
            FixtureProfile::KeychainLocked => {
                OpenState::KeychainUnavailable(keychain_unavailable_message(
                    &KeychainError::Locked("the login keychain is locked".to_owned()),
                ))
            }
            FixtureProfile::KeychainUnreachable => OpenState::KeychainUnavailable(
                linux_keychain_unavailable_message(&KeychainError::Unavailable(
                    "no Secret Service is running in this session".to_owned(),
                )),
            ),
            FixtureProfile::LostKey => {
                OpenState::Unreadable(unreadable_message(UnreadableReason::KeyMissing))
            }
        }
    }
}

impl StartupApi for FixtureStartup {
    fn open(&self) -> oneshot::Receiver<OpenState> {
        let (answer, receiver) = oneshot::channel();
        let index = {
            let mut opens = lock(&self.opens);
            *opens += 1;
            *opens - 1
        };
        let profile = {
            let sequence = lock(&self.sequence);
            sequence
                .get(index)
                .or(sequence.last())
                .copied()
                .unwrap_or(FixtureProfile::Ready)
        };
        let _ = answer.send(self.state(profile, None));
        receiver
    }

    fn start_over(&self) -> oneshot::Receiver<OpenState> {
        let (answer, receiver) = oneshot::channel();
        *lock(&self.started_over) = true;
        let notice = format!(
            "Moved the data that can't be read to {FIXTURE_MOVED_TO}. Delete it once you are \
             sure its key won't turn up."
        );
        let _ = answer.send(self.state(FixtureProfile::Ready, Some(notice)));
        receiver
    }
}

// ---------------------------------------------------------------------------
// FixtureSetup
// ---------------------------------------------------------------------------

/// The rooms a fixture account has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureRooms {
    /// The Smith family's.
    Family,
    /// The family's, without the invitation.
    FamilyNoInvites,
    /// None at all.
    None,
    /// Still loading.
    Loading,
    /// The 1,000-room, 10,000-event fixture.
    Large,
    /// Rooms full of terminal escape sequences
    /// ([`FixtureSession::with_hostile_at`]).
    Hostile,
}

/// A whole fixture state: how the profile opens, the first step the next
/// sign-in shows, a signed-in account and its rooms. The terminal UI's
/// snapshot tests and `familychat launch-tui-fixture` build their views
/// from it.
#[derive(Debug, Clone)]
pub struct FixtureSetup {
    /// How the profile opens.
    pub profile: FixtureProfile,
    /// The first step the next sign-in shows, whatever it starts with.
    pub step: Option<SigninStep>,
    /// A signed-in account ([`FIXTURE_USER`]) in this sync state.
    pub account: Option<SyncState>,
    /// Its rooms.
    pub rooms: FixtureRooms,
    /// The moment the rooms' times count back from, in milliseconds since
    /// the Unix epoch.
    pub now: u64,
}

impl FixtureSetup {
    /// A ready profile with nobody signed in.
    pub fn new(now: u64) -> Self {
        Self {
            profile: FixtureProfile::Ready,
            step: None,
            account: None,
            rooms: FixtureRooms::Family,
            now,
        }
    }

    /// The startup over a new fixture session and sign-in in this state.
    pub fn startup(&self) -> Arc<FixtureStartup> {
        let session = FixtureSession::new();
        if let Some(sync) = &self.account {
            session.add_account(&fixture_user(), sync.clone());
            match self.rooms {
                FixtureRooms::Family => session.with_family_at(self.now),
                FixtureRooms::FamilyNoInvites => {
                    session.with_family_at(self.now);
                    session.set_rooms(
                        session
                            .rooms()
                            .into_iter()
                            .filter(|room| !room.is_invite())
                            .collect(),
                    );
                }
                FixtureRooms::None => {}
                FixtureRooms::Loading => session.set_rooms_loading(true),
                FixtureRooms::Large => {
                    session.with_large_at(rooms::LARGE_ROOMS, rooms::LARGE_EVENTS, self.now)
                }
                FixtureRooms::Hostile => session.with_hostile_at(self.now),
            }
        }
        let signin = FixtureSignin::new(session.clone());
        if let Some(step) = &self.step {
            signin.start_at(step.clone());
        }
        FixtureStartup::with(session, signin, &[self.profile])
    }
}

#[cfg(test)]
mod tests {
    use familychat_core::signin::SecretText;

    use super::*;

    fn next_step(steps: &mut watch::Receiver<SigninStep>, until: impl Fn(&SigninStep) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !until(&steps.borrow()) {
            assert!(std::time::Instant::now() < deadline, "{:?}", steps.borrow());
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_code_is_redeemed_only_after_continue() {
        let session = FixtureSession::new();
        let signin = FixtureSignin::new(session.clone());
        let flow = signin.start(SigninStart::Link(SecretText::from(FIXTURE_LINK)));
        let mut steps = flow.steps();
        next_step(&mut steps, |step| matches!(step, SigninStep::Confirm(_)));
        assert_eq!(signin.redeemed(), 0);
        assert!(flow.answer(SigninAnswer::Continue));
        next_step(&mut steps, |step| matches!(step, SigninStep::Finished(_)));
        assert_eq!(signin.redeemed(), 1);
        assert_eq!(signin.answers(), [FixtureAnswer::Continue]);
        assert_eq!(session.accounts().borrow().len(), 1);
        assert!(
            !format!("{:?}", signin.events()).contains("fixture-login-token"),
            "the link is never recorded"
        );
    }

    #[test]
    fn other_text_is_not_a_link_and_passwords_follow_the_script() {
        let signin = FixtureSignin::new(FixtureSession::new());
        let flow = signin.start(SigninStart::Link(SecretText::from("hello")));
        let mut steps = flow.steps();
        next_step(&mut steps, |step| matches!(step, SigninStep::NotALink(_)));

        signin.set_password_outcome(FixtureOutcome::Problem(SigninProblem::new(
            wording::WRONG_PASSWORD,
        )));
        let flow = signin.start(SigninStart::FamilyChatPassword);
        let mut steps = flow.steps();
        next_step(&mut steps, |step| matches!(step, SigninStep::Password(_)));
        flow.answer(SigninAnswer::SubmitPassword {
            server: FIXTURE_SERVER.to_owned(),
            username: "ana".to_owned(),
            password: SecretText::from("secret"),
        });
        next_step(
            &mut steps,
            |step| matches!(step, SigninStep::Password(form) if form.problem.is_some()),
        );
        drop(flow);
        assert!(!format!("{:?}", signin.events()).contains("secret"));
    }

    #[test]
    fn startup_follows_its_sequence_and_starts_over_ready() {
        let startup = FixtureStartup::new(&[FixtureProfile::KeychainLocked, FixtureProfile::Ready]);
        let first = startup.open().blocking_recv().expect("an answer");
        assert!(
            matches!(&first, OpenState::KeychainUnavailable(message) if message.contains("locked"))
        );
        assert!(matches!(
            startup.open().blocking_recv(),
            Ok(OpenState::Ready(_))
        ));
        assert!(matches!(
            startup.open().blocking_recv(),
            Ok(OpenState::Ready(_))
        ));
        let lost = FixtureStartup::new(&[FixtureProfile::LostKey]);
        assert!(matches!(
            lost.open().blocking_recv(),
            Ok(OpenState::Unreadable(_))
        ));
        let Ok(OpenState::Ready(opened)) = lost.start_over().blocking_recv() else {
            panic!("ready after starting over");
        };
        assert!(
            opened
                .notice
                .is_some_and(|notice| notice.contains(FIXTURE_MOVED_TO))
        );
    }
}
