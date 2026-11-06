//! Family Chat's messaging actions: the one place a frontend changes
//! anything in a room (PLAN.md, sections 5.3 and 13.7; ADR 0012). The
//! Zmail equivalent is `zmail-actions`, whose shape this follows: a
//! service that knows nothing of a UI toolkit and reports what happened to
//! a plain callback.
//!
//! [`Actions`] works over `familychat-core`'s
//! [`SessionApi`](familychat_core::session::SessionApi), so it has no
//! matrix-sdk, GPUI or ratatui dependency and both frontends use it
//! directly (`scripts/check-dependency-boundaries.sh`). Each method returns
//! an [`ActionTask`], a future that needs no particular runtime (it only
//! awaits the session's `tokio::sync` answers): the window runs it on
//! GPUI's background executor, a test awaits it. As it runs it reports
//! [`ActionEvent`]s to the callback the service was made with, which the
//! window turns into its own events (the callback is `Send`, so it hands
//! them over a channel; [`Actions::channel`] makes one).
//!
//! The rules that are not drawing live here, so the window and the
//! terminal UI keep the same ones:
//!
//! - [`markdown`]: what the person wrote becomes Markdown through ruma's
//!   conversion, mentions become `matrix.to` links and `m.mentions`, and
//!   the HTML is what Family Chat's own sanitiser keeps.
//! - [`mentions`]: the `@` autocomplete over the room's members.
//! - [`files`]: a file is checked against the server's upload limit
//!   before it is read, and a picture loses its location
//!   (`familychat_security::image_metadata`) before it is sent.
//! - New rooms are encrypted unless the server's well-known says
//!   `io.element.e2ee.default: false` ([`encrypt_new_room`]).
//! - While the account is paused (`M_USER_SUSPENDED`, a Family Chat
//!   curfew) nothing is sent: every write is refused with
//!   [`Refusal::Paused`] before it reaches the session, and the composer
//!   is read-only ([`ComposerState`]).
//! - Typing notices are sent at most every few seconds
//!   ([`TypingNotices`]).
//!
//! Sends go through the SDK's send queue in the session: the message shows
//! at once as a local echo, and one that couldn't go (offline) goes when
//! the connection comes back.

pub mod files;
pub mod markdown;
pub mod mentions;

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use familychat_core::{
    OwnedEventId, OwnedRoomId, OwnedUserId, RoomId,
    messaging::{Draft, MessageRef, NewRoom, RoomDone, RoomRequest, ServerDefaults},
    rooms::{Membership, RoomSummary},
    session::{AccountKey, SessionApi, SessionError},
    settings::PrivacySettings,
    sync::{AccountStatus, SyncState},
};
use familychat_security::image_metadata::MetadataError;
use tokio::sync::{mpsc, watch};

pub use files::{FileOptions, FileSource};

/// A running action: a future to drive on any executor. It reports what
/// happens to the service's callback, so its output is nothing.
pub type ActionTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What the person did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Sent a message.
    Send,
    /// Sent a reply.
    Reply,
    /// Edited a message.
    Edit,
    /// Added or took back a reaction.
    React,
    /// Deleted a message.
    Redact,
    /// Read the room up to the newest message.
    MarkRead,
    /// Started or stopped typing.
    Typing,
    /// Sent a file.
    SendFile,
    /// Invited someone.
    Invite,
    /// Accepted an invitation.
    AcceptInvite,
    /// Declined an invitation.
    DeclineInvite,
    /// Left a room.
    Leave,
    /// Made a room.
    CreateRoom,
    /// Started a direct chat.
    CreateDirectChat,
    /// Kept a draft.
    SaveDraft,
    /// Opened a draft.
    LoadDraft,
    /// Sent a message the encryption stopped, anyway (ADR 0009).
    SendAnyway,
}

impl Action {
    /// The sentence for when it failed.
    pub fn failed_sentence(self) -> &'static str {
        match self {
            Self::Send | Self::Reply => "The message couldn't be sent. Try again.",
            Self::Edit => "The message couldn't be changed. Try again.",
            Self::React => "The reaction couldn't be sent. Try again.",
            Self::Redact => "The message couldn't be deleted. Try again.",
            Self::MarkRead => "The room couldn't be marked as read.",
            Self::Typing => "Family Chat couldn't tell the room you're typing.",
            Self::SendFile => "The file couldn't be sent. Try again.",
            Self::Invite => "The invitation couldn't be sent. Try again.",
            Self::AcceptInvite => "The invitation couldn't be accepted. Try again.",
            Self::DeclineInvite => "The invitation couldn't be declined. Try again.",
            Self::Leave => "Family Chat couldn't leave the room. Try again.",
            Self::CreateRoom => "The room couldn't be made. Try again.",
            Self::CreateDirectChat => "The chat couldn't be started. Try again.",
            Self::SaveDraft => "Your draft couldn't be kept.",
            Self::LoadDraft => "Your draft couldn't be opened.",
            Self::SendAnyway => "The message couldn't be sent anyway. Try again.",
        }
    }

    /// Whether it writes to the server (and so is refused while paused).
    fn writes(self) -> bool {
        !matches!(self, Self::SaveDraft | Self::LoadDraft)
    }
}

/// Why an action was refused before anything was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The account is paused (a Family Chat curfew).
    Paused,
    /// There is nothing to send.
    Empty,
    /// The file is larger than the server takes.
    TooLarge {
        /// Its size, in bytes.
        size: u64,
        /// The server's limit, in bytes.
        limit: u64,
    },
    /// The server's upload limit couldn't be found out, so a file can't be
    /// checked.
    LimitUnknown,
    /// The picture's location can't be taken out.
    Location(MetadataError),
    /// The file couldn't be read.
    Unreadable,
    /// The account isn't in the room.
    NotJoined,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Paused => f.write_str(
                "Chat is paused for now, so nothing can be sent. It starts again when the pause ends.",
            ),
            Self::Empty => f.write_str("There's nothing to send."),
            Self::TooLarge { size, limit } => write!(
                f,
                "That file is {}, and this server takes files up to {}. Nothing was sent.",
                files::size_label(*size),
                files::size_label(*limit)
            ),
            Self::LimitUnknown => f.write_str(
                "Family Chat couldn't check how large a file this server takes. Try again when you're online.",
            ),
            Self::Location(error) => write!(f, "{error}"),
            Self::Unreadable => f.write_str("That file couldn't be read."),
            Self::NotJoined => f.write_str("You can only write in rooms you've joined."),
        }
    }
}

/// What happened, as the callback hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionEvent {
    /// Done, or queued to send (a message shows at once as a local echo).
    Done {
        /// The room, when the action has one.
        room: Option<OwnedRoomId>,
        /// What was done.
        action: Action,
    },
    /// Refused before anything was sent.
    Refused {
        /// The room, when the action has one.
        room: Option<OwnedRoomId>,
        /// What was refused.
        action: Action,
        /// Why.
        refusal: Refusal,
    },
    /// The session couldn't do it.
    Failed {
        /// The room, when the action has one.
        room: Option<OwnedRoomId>,
        /// What failed.
        action: Action,
        /// Why, in the session's words.
        error: SessionError,
    },
    /// A room or direct chat was made.
    RoomCreated {
        /// The new room.
        room: OwnedRoomId,
        /// Whether it is a direct chat.
        direct: bool,
        /// Whether it was made encrypted (its `m.room.encryption` state may
        /// reach the room list a moment after the room does).
        encrypted: bool,
    },
    /// A room's draft was read.
    DraftLoaded {
        /// The room.
        room: OwnedRoomId,
        /// Its draft, if one was kept.
        draft: Option<Draft>,
    },
}

impl ActionEvent {
    /// The sentence to show the person, when there is one: why it was
    /// refused or failed.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Refused { refusal, .. } => Some(refusal.to_string()),
            Self::Failed { action, error, .. } => Some(match error {
                SessionError::Failed(_) => action.failed_sentence().to_owned(),
                other => other.to_string(),
            }),
            _ => None,
        }
    }
}

/// The callback every [`ActionEvent`] is reported to.
pub type EventSink = Arc<dyn Fn(ActionEvent) + Send + Sync>;

/// Whether the composer can write in a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerState {
    /// It can (offline included: sends wait in the queue).
    Writable,
    /// The account is paused: "Chat is paused", read-only.
    Paused,
    /// The account isn't in the room (an invitation, or it left).
    NotJoined,
}

impl ComposerState {
    /// The composer's state for `room` while the account is in `status`.
    pub fn of(status: &AccountStatus, room: &RoomSummary) -> Self {
        if matches!(status.sync, SyncState::Suspended { .. }) {
            Self::Paused
        } else if room.membership != Membership::Joined {
            Self::NotJoined
        } else {
            Self::Writable
        }
    }

    /// The banner's title while paused.
    pub const PAUSED_TITLE: &'static str = "Chat is paused";

    /// What the composer says instead of its placeholder.
    pub fn placeholder(self, room_name: &str) -> String {
        match self {
            Self::Writable => format!("Message {room_name}"),
            Self::Paused => {
                "Chat is paused. Messages can't be sent until the pause ends.".to_owned()
            }
            Self::NotJoined => "Join the room to write in it.".to_owned(),
        }
    }
}

/// Whether a new room is encrypted: always, unless the server's
/// well-known says `io.element.e2ee.default: false` (PLAN.md, section 5.7).
pub fn encrypt_new_room(defaults: &ServerDefaults) -> bool {
    defaults.encryption_default != Some(false)
}

/// How often a "typing" notice is renewed while the person keeps typing.
pub const TYPING_RENEW: Duration = Duration::from_secs(3);
/// How long the composer waits after the last key before it says the
/// person stopped typing.
pub const TYPING_IDLE: Duration = Duration::from_secs(5);
/// How long typing must pause before a draft is kept.
pub const DRAFT_SAVE_DELAY: Duration = Duration::from_secs(1);

/// When to send typing notices: once when typing starts, again every
/// [`TYPING_RENEW`] while it goes on, and "stopped" when the text is
/// emptied, sent, or idle for [`TYPING_IDLE`].
#[derive(Debug, Clone, Default)]
pub struct TypingNotices {
    sent_at: Option<Instant>,
}

impl TypingNotices {
    /// The text changed at `now`: the notice to send, if any.
    pub fn changed(&mut self, now: Instant, text_is_empty: bool) -> Option<bool> {
        if text_is_empty {
            return self.stopped();
        }
        match self.sent_at {
            Some(at) if now.duration_since(at) < TYPING_RENEW => None,
            _ => {
                self.sent_at = Some(now);
                Some(true)
            }
        }
    }

    /// Typing stopped (sent, emptied, idle, room left): `Some(false)` when
    /// the room was told the person was typing.
    pub fn stopped(&mut self) -> Option<bool> {
        self.sent_at.take().map(|_| false)
    }

    /// Whether the room was told the person is typing.
    pub fn is_typing(&self) -> bool {
        self.sent_at.is_some()
    }
}

/// What a send is: a new message, a reply or an edit, from a [`Draft`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTarget {
    /// A new message.
    New,
    /// A reply to this message.
    Reply(OwnedEventId),
    /// The new text of this message.
    Edit(MessageRef),
}

/// The messaging actions of one account, reporting to one callback.
#[derive(Clone)]
pub struct Actions {
    session: Arc<dyn SessionApi>,
    account: AccountKey,
    events: EventSink,
    files: FileOptions,
    /// Whether read receipts and typing notices go out (ADR 0015); sent
    /// when nothing says otherwise.
    privacy: Option<watch::Receiver<PrivacySettings>>,
}

impl fmt::Debug for Actions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Actions")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl Actions {
    /// The actions of `account`, reporting to `events`.
    pub fn new(
        session: Arc<dyn SessionApi>,
        account: AccountKey,
        events: impl Fn(ActionEvent) + Send + Sync + 'static,
    ) -> Self {
        Self {
            session,
            account,
            events: Arc::new(events),
            files: FileOptions::default(),
            privacy: None,
        }
    }

    /// Follows the profile's privacy settings: with read receipts off,
    /// [`Self::mark_read`] sends a private receipt (the account's devices
    /// and unread counts follow, nobody else sees it); with typing notices
    /// off, [`Self::typing`] sends nothing.
    pub fn with_privacy(mut self, privacy: watch::Receiver<PrivacySettings>) -> Self {
        self.privacy = Some(privacy);
        self
    }

    /// The privacy settings in force now.
    fn privacy(&self) -> PrivacySettings {
        self.privacy
            .as_ref()
            .map(|privacy| *privacy.borrow())
            .unwrap_or_default()
    }

    /// The actions of `account`, with their events on a channel.
    pub fn channel(
        session: Arc<dyn SessionApi>,
        account: AccountKey,
    ) -> (Self, mpsc::UnboundedReceiver<ActionEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let actions = Self::new(session, account, move |event| {
            let _ = sender.send(event);
        });
        (actions, receiver)
    }

    /// Changes how files are prepared.
    pub fn with_file_options(mut self, files: FileOptions) -> Self {
        self.files = files;
        self
    }

    /// The account.
    pub fn account(&self) -> &AccountKey {
        &self.account
    }

    /// Whether the account is paused now.
    pub fn is_paused(&self) -> bool {
        self.session
            .account_status(&self.account)
            .is_some_and(|status| matches!(status.borrow().sync, SyncState::Suspended { .. }))
    }

    fn emit(&self, event: ActionEvent) {
        (self.events)(event);
    }

    /// Refuses a write while paused; `true` when it may go ahead.
    fn may_write(&self, room: Option<&RoomId>, action: Action) -> bool {
        if action.writes() && self.is_paused() {
            self.emit(ActionEvent::Refused {
                room: room.map(ToOwned::to_owned),
                action,
                refusal: Refusal::Paused,
            });
            return false;
        }
        true
    }

    /// Runs one room request and reports how it went.
    fn room_task(&self, room: &RoomId, action: Action, request: RoomRequest) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            if !this.may_write(Some(&room), action) {
                return;
            }
            let reply = this
                .session
                .room_action(&this.account, &room, request)
                .await;
            this.report(
                Some(room),
                action,
                reply.unwrap_or(Err(SessionError::ShutDown)),
            );
        })
    }

    fn report(
        &self,
        room: Option<OwnedRoomId>,
        action: Action,
        result: Result<RoomDone, SessionError>,
    ) {
        self.emit(match result {
            Ok(_) => ActionEvent::Done { room, action },
            Err(error) => ActionEvent::Failed {
                room,
                action,
                error,
            },
        });
    }

    /// Sends what the person wrote: a new message, a reply or an edit
    /// (`target`), with the `mentions` the composer inserted.
    pub fn send(&self, room: &RoomId, draft: &Draft, target: SendTarget) -> ActionTask {
        let message = markdown::compose(draft.text.as_str(), &draft.mentions);
        let action = match target {
            SendTarget::New => Action::Send,
            SendTarget::Reply(_) => Action::Reply,
            SendTarget::Edit(_) => Action::Edit,
        };
        if message.body.as_str().trim().is_empty() {
            let this = self.clone();
            let room = room.to_owned();
            return Box::pin(async move {
                this.emit(ActionEvent::Refused {
                    room: Some(room),
                    action,
                    refusal: Refusal::Empty,
                });
            });
        }
        let request = match target {
            SendTarget::New => RoomRequest::Send {
                message,
                reply_to: None,
            },
            SendTarget::Reply(event) => RoomRequest::Send {
                message,
                reply_to: Some(event),
            },
            SendTarget::Edit(target) => RoomRequest::Edit { target, message },
        };
        self.room_task(room, action, request)
    }

    /// Adds the account's `key` reaction to a message, or takes it back.
    pub fn toggle_reaction(&self, room: &RoomId, target: MessageRef, key: &str) -> ActionTask {
        self.room_task(
            room,
            Action::React,
            RoomRequest::ToggleReaction {
                target,
                key: key.to_owned(),
            },
        )
    }

    /// Deletes one of the account's messages (the frontend has asked
    /// first).
    pub fn redact(&self, room: &RoomId, target: MessageRef, reason: Option<String>) -> ActionTask {
        self.room_task(room, Action::Redact, RoomRequest::Redact { target, reason })
    }

    /// Sends a message the encryption stopped
    /// (`familychat_core::timeline::SendProblem`), as Element X's "Send
    /// anyway" does: an unconfirmed device of a verified person gets the
    /// key too, or a changed identity's old verification is withdrawn
    /// (ADR 0009). Refused while paused, like every write.
    pub fn send_anyway(&self, room: &RoomId, target: MessageRef) -> ActionTask {
        self.room_task(room, Action::SendAnyway, RoomRequest::SendAnyway(target))
    }

    /// Moves the read receipt and the read marker to the newest message.
    /// Nothing is sent while paused, and nothing is reported when it went
    /// well (it happens every time a room is read).
    pub fn mark_read(&self, room: &RoomId) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            if this.is_paused() {
                return;
            }
            let request = if this.privacy().read_receipts {
                RoomRequest::MarkRead
            } else {
                RoomRequest::MarkReadPrivately
            };
            let reply = this
                .session
                .room_action(&this.account, &room, request)
                .await;
            if let Ok(Err(error)) = reply {
                this.emit(ActionEvent::Failed {
                    room: Some(room),
                    action: Action::MarkRead,
                    error,
                });
            }
        })
    }

    /// Tells the room the person is typing, or has stopped. Quiet: a
    /// typing notice that fails is not worth telling anyone about, and
    /// nothing is sent while paused or with typing notices turned off.
    pub fn typing(&self, room: &RoomId, typing: bool) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            if this.is_paused() || !this.privacy().typing_notices {
                return;
            }
            let _ = this
                .session
                .room_action(&this.account, &room, RoomRequest::Typing(typing))
                .await;
        })
    }

    /// Checks a file against the server's upload limit, without reading
    /// it: what the composer does when a file is picked, so an over-limit
    /// file is refused before it is even pending.
    pub fn check_file(&self, room: &RoomId, source: FileSource) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            if let Err(refusal) = this.size_allowed(&source).await {
                this.emit(ActionEvent::Refused {
                    room: Some(room),
                    action: Action::SendFile,
                    refusal,
                });
            }
        })
    }

    /// The same check as [`Self::check_file`], answering instead of
    /// reporting: the file's size when the server takes it, or why not.
    /// A frontend shows a file as pending only once this says yes.
    pub fn file_allowed(
        &self,
        source: FileSource,
    ) -> Pin<Box<dyn Future<Output = Result<u64, Refusal>> + Send + 'static>> {
        let this = self.clone();
        Box::pin(async move { this.size_allowed(&source).await })
    }

    async fn size_allowed(&self, source: &FileSource) -> Result<u64, Refusal> {
        let size = source.size().map_err(|_| Refusal::Unreadable)?;
        let limits = self.limits().await?;
        files::check_size(size, &limits).map(|()| size)
    }

    async fn limits(&self) -> Result<familychat_core::messaging::MediaLimits, Refusal> {
        match self.session.media_limits(&self.account).await {
            Ok(Ok(limits)) => Ok(limits),
            _ => Err(Refusal::LimitUnknown),
        }
    }

    /// Sends a file: its size is checked against the server's limit before
    /// it is read or uploaded, and a picture's location is taken out.
    pub fn send_file(
        &self,
        room: &RoomId,
        source: FileSource,
        reply_to: Option<OwnedEventId>,
    ) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            if !this.may_write(Some(&room), Action::SendFile) {
                return;
            }
            let prepared = match this.limits().await {
                Ok(limits) => files::prepare(&source, &limits, this.files),
                Err(refusal) => Err(refusal),
            };
            let file = match prepared {
                Ok(file) => file,
                Err(refusal) => {
                    this.emit(ActionEvent::Refused {
                        room: Some(room),
                        action: Action::SendFile,
                        refusal,
                    });
                    return;
                }
            };
            let reply = this
                .session
                .room_action(
                    &this.account,
                    &room,
                    RoomRequest::SendFile { file, reply_to },
                )
                .await;
            this.report(
                Some(room),
                Action::SendFile,
                reply.unwrap_or(Err(SessionError::ShutDown)),
            );
        })
    }

    /// Invites `user` to the room.
    pub fn invite(&self, room: &RoomId, user: OwnedUserId) -> ActionTask {
        self.room_task(room, Action::Invite, RoomRequest::Invite(user))
    }

    /// Accepts or declines an invitation.
    pub fn answer_invite(&self, room: &RoomId, accept: bool) -> ActionTask {
        let action = if accept {
            Action::AcceptInvite
        } else {
            Action::DeclineInvite
        };
        self.room_task(room, action, RoomRequest::AnswerInvite(accept))
    }

    /// Leaves the room (the frontend has asked first).
    pub fn leave(&self, room: &RoomId) -> ActionTask {
        self.room_task(room, Action::Leave, RoomRequest::Leave)
    }

    /// Makes a room called `name` and invites `invite`; encrypted unless
    /// the server says new rooms aren't.
    pub fn create_room(&self, name: Option<String>, invite: Vec<OwnedUserId>) -> ActionTask {
        self.create(name, invite, false)
    }

    /// Starts a direct chat with `user`; encrypted unless the server says
    /// new rooms aren't.
    pub fn create_direct_chat(&self, user: OwnedUserId) -> ActionTask {
        self.create(None, vec![user], true)
    }

    fn create(&self, name: Option<String>, invite: Vec<OwnedUserId>, direct: bool) -> ActionTask {
        let this = self.clone();
        let action = if direct {
            Action::CreateDirectChat
        } else {
            Action::CreateRoom
        };
        Box::pin(async move {
            if !this.may_write(None, action) {
                return;
            }
            // A server that can't be asked keeps the default: encrypted.
            let defaults = match this.session.server_defaults(&this.account).await {
                Ok(Ok(defaults)) => defaults,
                _ => ServerDefaults::default(),
            };
            let encrypted = encrypt_new_room(&defaults);
            let request = NewRoom {
                name: name
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty()),
                invite,
                direct,
                encrypted,
            };
            match this.session.create_room(&this.account, request).await {
                Ok(Ok(room)) => this.emit(ActionEvent::RoomCreated {
                    room,
                    direct,
                    encrypted,
                }),
                Ok(Err(error)) => this.emit(ActionEvent::Failed {
                    room: None,
                    action,
                    error,
                }),
                Err(_) => this.emit(ActionEvent::Failed {
                    room: None,
                    action,
                    error: SessionError::ShutDown,
                }),
            }
        })
    }

    /// Reads the room's draft; it arrives as [`ActionEvent::DraftLoaded`].
    pub fn load_draft(&self, room: &RoomId) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            match this.session.draft(&this.account, &room).await {
                Ok(Ok(draft)) => this.emit(ActionEvent::DraftLoaded { room, draft }),
                Ok(Err(error)) => this.emit(ActionEvent::Failed {
                    room: Some(room),
                    action: Action::LoadDraft,
                    error,
                }),
                Err(_) => {}
            }
        })
    }

    /// Keeps the room's draft, or forgets it when it is empty. Quiet when
    /// it works.
    pub fn save_draft(&self, room: &RoomId, draft: Draft) -> ActionTask {
        let this = self.clone();
        let room = room.to_owned();
        Box::pin(async move {
            let draft = (!draft.is_empty()).then_some(draft);
            if let Ok(Err(error)) = this.session.save_draft(&this.account, &room, draft).await {
                this.emit(ActionEvent::Failed {
                    room: Some(room),
                    action: Action::SaveDraft,
                    error,
                });
            }
        })
    }
}

#[cfg(test)]
mod tests;
