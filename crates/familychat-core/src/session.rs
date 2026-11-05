//! [`SessionApi`]: what both frontends ask of the Matrix sessions, and how
//! the answers come back (PLAN.md, sections 5.2 and 5.3; ADR 0005).
//!
//! Every answer arrives over a runtime-agnostic channel from
//! `tokio::sync`, which needs no Tokio runtime on the receiving side: a
//! GPUI task awaits it in `cx.spawn`, and the terminal UI's loop polls it
//! with `try_recv` on each tick. The session does its work on its own
//! runtime (`familychat-session`'s `BackgroundRuntime`), so nothing here
//! blocks the UI thread.
//!
//! - Values that change (an account's status, the list of accounts) are
//!   [`watch`] receivers: the latest value, and a signal when it changes.
//! - Streams (a room list, a timeline) are bounded [`mpsc`] receivers of
//!   batches of [`ListDiff`]s, so a frontend that falls behind slows the
//!   session down instead of growing a queue. Dropping the subscription
//!   stops the work behind it.
//! - One-off answers are [`oneshot`] receivers ([`Reply`]).
//!
//! The live implementation is `familychat-session`'s `LiveSession`; a
//! fixture implementation for screenshots and view tests is
//! `familychat-test-support`'s `FixtureSession`.

use std::{fmt, path::PathBuf, sync::Arc};

use ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    diff::ListDiff,
    encryption::{
        AuthOutcome, DeviceView, EncryptionStatus, IdentityWarning, KeyImport, SecretText,
        VerificationFlow,
    },
    media::{DecodedImage, MediaError, MediaInfo},
    messaging::{Draft, MediaLimits, NewRoom, RoomDone, RoomRequest, ServerDefaults},
    notifications::IncomingNotification,
    rooms::{RoomListCommand, RoomListFilter, RoomListLoading, RoomMember, RoomSummary},
    sync::AccountStatus,
    timeline::{TimelineCommand, TimelineUpdate},
};

/// An account's local id, as text: the random UUID that names its keychain
/// entries and its directory (`familychat-security`'s `AccountId`). It
/// identifies nobody.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountKey(pub String);

impl fmt::Display for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A signed-in account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    /// The account's local id.
    pub key: AccountKey,
    /// The Matrix user id.
    pub user_id: OwnedUserId,
    /// The homeserver's client-server API base URL.
    pub homeserver_url: String,
}

/// Why the session could not do what a frontend asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// No signed-in account has this key.
    UnknownAccount,
    /// The account has no such room (yet).
    UnknownRoom,
    /// The account is not ready: it is still being restored, or it could
    /// not be.
    NotReady,
    /// The session has shut down.
    ShutDown,
    /// The server refused because the account is paused
    /// (`M_USER_SUSPENDED`: a Family Chat curfew).
    Paused,
    /// The server refused a file as too large (`M_TOO_LARGE`).
    TooLarge,
    /// Something else failed.
    Failed(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownAccount => f.write_str("That account is not signed in."),
            Self::UnknownRoom => f.write_str("That room isn't available."),
            Self::NotReady => f.write_str("That account isn't ready yet."),
            Self::ShutDown => f.write_str("Family Chat is closing."),
            Self::Paused => f.write_str(
                "Chat is paused for now, so nothing can be sent. It starts again when the pause ends.",
            ),
            Self::TooLarge => f.write_str("The server says that file is too large to send."),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionError {}

/// A verification request another of an account's devices sent, with the
/// account it is for.
pub type IncomingVerification = (AccountKey, VerificationFlow);

/// A one-off answer.
pub type Reply<T> = oneshot::Receiver<Result<T, SessionError>>;

/// A one-off answer about media.
pub type MediaReply<T> = oneshot::Receiver<Result<T, MediaError>>;

/// How many batches a subscription buffers before the session waits for
/// the frontend.
pub const SUBSCRIPTION_BUFFER: usize = 64;

/// An open room list: its changes, its loading state and its controls.
/// Dropping it stops the list.
#[derive(Debug)]
pub struct RoomListSubscription {
    /// Batches of changes; the first is a [`ListDiff::Reset`], and each
    /// filter change starts with another.
    pub updates: mpsc::Receiver<Vec<ListDiff<RoomSummary>>>,
    /// Whether the list has loaded.
    pub loading: watch::Receiver<RoomListLoading>,
    commands: mpsc::UnboundedSender<RoomListCommand>,
}

impl RoomListSubscription {
    /// A subscription over these channels (for implementations of
    /// [`SessionApi`]).
    pub fn new(
        updates: mpsc::Receiver<Vec<ListDiff<RoomSummary>>>,
        loading: watch::Receiver<RoomListLoading>,
        commands: mpsc::UnboundedSender<RoomListCommand>,
    ) -> Self {
        Self {
            updates,
            loading,
            commands,
        }
    }

    /// Shows another set of rooms. `false` when the list has stopped.
    pub fn set_filter(&self, filter: RoomListFilter) -> bool {
        self.commands
            .send(RoomListCommand::SetFilter(filter))
            .is_ok()
    }

    /// Shows one more page of rooms. `false` when the list has stopped.
    pub fn load_more(&self) -> bool {
        self.commands.send(RoomListCommand::LoadMore).is_ok()
    }

    /// Goes back to the first page. `false` when the list has stopped.
    pub fn first_page_only(&self) -> bool {
        self.commands.send(RoomListCommand::FirstPageOnly).is_ok()
    }

    /// The updates, the loading state and the controls apart, so a task
    /// can wait on the updates while the view keeps the controls.
    pub fn split(
        self,
    ) -> (
        mpsc::Receiver<Vec<ListDiff<RoomSummary>>>,
        watch::Receiver<RoomListLoading>,
        RoomListControls,
    ) {
        (
            self.updates,
            self.loading,
            RoomListControls {
                commands: self.commands,
            },
        )
    }
}

/// An open room list's controls, apart from its updates
/// ([`RoomListSubscription::split`]). The list stops when both are dropped.
#[derive(Debug, Clone)]
pub struct RoomListControls {
    commands: mpsc::UnboundedSender<RoomListCommand>,
}

impl RoomListControls {
    /// Shows another set of rooms. `false` when the list has stopped.
    pub fn set_filter(&self, filter: RoomListFilter) -> bool {
        self.commands
            .send(RoomListCommand::SetFilter(filter))
            .is_ok()
    }

    /// Shows one more page of rooms. `false` when the list has stopped.
    pub fn load_more(&self) -> bool {
        self.commands.send(RoomListCommand::LoadMore).is_ok()
    }
}

/// An open timeline: its updates and its controls. Dropping it closes the
/// timeline.
#[derive(Debug)]
pub struct TimelineSubscription {
    /// Updates; the first is the items the timeline starts with.
    pub updates: mpsc::Receiver<TimelineUpdate>,
    commands: mpsc::UnboundedSender<TimelineCommand>,
}

impl TimelineSubscription {
    /// A subscription over these channels (for implementations of
    /// [`SessionApi`]).
    pub fn new(
        updates: mpsc::Receiver<TimelineUpdate>,
        commands: mpsc::UnboundedSender<TimelineCommand>,
    ) -> Self {
        Self { updates, commands }
    }

    /// Loads up to `events` older events; progress arrives as
    /// [`TimelineUpdate::Pagination`]. `false` when the timeline has
    /// closed.
    pub fn paginate_backwards(&self, events: u16) -> bool {
        self.commands
            .send(TimelineCommand::PaginateBackwards { events })
            .is_ok()
    }

    /// The updates and the controls apart, so a task can wait on the
    /// updates while the view keeps the controls.
    pub fn split(self) -> (mpsc::Receiver<TimelineUpdate>, TimelineControls) {
        (
            self.updates,
            TimelineControls {
                commands: self.commands,
            },
        )
    }
}

/// An open timeline's controls, apart from its updates
/// ([`TimelineSubscription::split`]).
#[derive(Debug, Clone)]
pub struct TimelineControls {
    commands: mpsc::UnboundedSender<TimelineCommand>,
}

impl TimelineControls {
    /// Loads up to `events` older events. `false` when the timeline has
    /// closed.
    pub fn paginate_backwards(&self, events: u16) -> bool {
        self.commands
            .send(TimelineCommand::PaginateBackwards { events })
            .is_ok()
    }
}

/// What the frontends ask of the Matrix sessions. Nothing here blocks:
/// each call returns at once with a channel the answer arrives on.
pub trait SessionApi: Send + Sync {
    /// The signed-in accounts, as they change.
    fn accounts(&self) -> watch::Receiver<Vec<AccountView>>;

    /// An account's status (its sync state and any problem), as it
    /// changes; `None` for an unknown account.
    fn account_status(&self, account: &AccountKey) -> Option<watch::Receiver<AccountStatus>>;

    /// Opens the account's room list with `filter`.
    fn room_list(
        &self,
        account: &AccountKey,
        filter: RoomListFilter,
    ) -> Reply<RoomListSubscription>;

    /// Opens a room's timeline.
    fn timeline(&self, account: &AccountKey, room: &RoomId) -> Reply<TimelineSubscription>;

    /// A room's joined and invited members.
    fn members(&self, account: &AccountKey, room: &RoomId) -> Reply<Vec<RoomMember>>;

    /// Does `request` in `room`: sends through the SDK's send queue
    /// (local echo at once, retried when the connection comes back),
    /// edits, reactions, deletions, receipts, typing, invitations and
    /// leaving. `familychat-actions` makes the requests; a frontend goes
    /// through it.
    fn room_action(
        &self,
        account: &AccountKey,
        room: &RoomId,
        request: RoomRequest,
    ) -> Reply<RoomDone>;

    /// Makes a new room or direct chat, and answers with its id.
    fn create_room(&self, account: &AccountKey, room: NewRoom) -> Reply<OwnedRoomId>;

    /// What the account's server accepts as uploads, fetched once and
    /// kept.
    fn media_limits(&self, account: &AccountKey) -> Reply<MediaLimits>;

    /// What the account's server says about new rooms (its well-known).
    fn server_defaults(&self, account: &AccountKey) -> Reply<ServerDefaults>;

    /// The draft kept for `room`, if any.
    fn draft(&self, account: &AccountKey, room: &RoomId) -> Reply<Option<Draft>>;

    /// Keeps `draft` for `room` (encrypted in the app store), or forgets
    /// it (`None`).
    fn save_draft(&self, account: &AccountKey, room: &RoomId, draft: Option<Draft>) -> Reply<()>;

    /// An image to draw: fetched from the account's homeserver through
    /// authenticated media (a thumbnail where the server or the sender
    /// offers one), decrypted, and decoded off the UI thread within
    /// `familychat-security`'s limits. SVG is refused.
    fn image(&self, account: &AccountKey, media: &MediaInfo) -> MediaReply<Arc<DecodedImage>>;

    /// Downloads a file the same way and writes it to `path`, which the
    /// person chose (the frontend suggests a safe name). Nothing is opened.
    fn save_file(&self, account: &AccountKey, media: &MediaInfo, path: PathBuf) -> MediaReply<()>;

    /// Asks a stopped or waiting sync to try again now (after "not
    /// supported", a sign-in problem, or to skip a backoff).
    fn retry_sync(&self, account: &AccountKey);

    /// The notifications every account's sync reports from now on (push
    /// rules applied, the account's own messages and ones from before the
    /// session started left out). One frontend listens: a new call
    /// replaces the previous receiver. The session drops notifications
    /// while the receiver is full rather than queue them.
    fn notifications(&self) -> mpsc::Receiver<IncomingNotification>;

    /// How many unread messages call for the account's attention, as it
    /// changes: the notifying unread messages of its joined rooms (the
    /// room list's badges) plus one for each invitation. `None` for an
    /// unknown account.
    fn unread_total(&self, account: &AccountKey) -> Option<watch::Receiver<u64>>;

    // -- End-to-end encryption (ADR 0009) ------------------------------------

    /// The account's encryption (this device, its identity, recovery), as
    /// it changes. `None` for an unknown account.
    fn encryption(&self, account: &AccountKey) -> Option<watch::Receiver<EncryptionStatus>>;

    /// Makes the account's cross-signing identity when it has none: first
    /// without the server's user-interactive auth (MSC3967), then with
    /// `password` when the server asks for it. The password is used for
    /// this request only and wiped.
    fn set_up_identity(
        &self,
        account: &AccountKey,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome>;

    /// Asks the account's other devices to verify this one.
    fn verify_this_device(&self, account: &AccountKey) -> Reply<VerificationFlow>;

    /// Asks another of the account's devices to verify itself with this one.
    fn verify_device(&self, account: &AccountKey, device_id: &str) -> Reply<VerificationFlow>;

    /// The verification requests other devices send from now on, with the
    /// account each is for. One frontend listens: a new call replaces the
    /// previous receiver.
    fn verification_requests(&self) -> mpsc::Receiver<IncomingVerification>;

    /// Sets recovery up (the key backup and secret storage) and answers
    /// with the new recovery key, which is shown once and kept nowhere.
    fn enable_recovery(&self, account: &AccountKey) -> Reply<SecretText>;

    /// Confirms this device with the recovery key: fetches the account's
    /// secrets from the server and the backup's keys with them.
    fn recover(&self, account: &AccountKey, recovery_key: SecretText) -> Reply<()>;

    /// The account's devices, this one first.
    fn devices(&self, account: &AccountKey) -> Reply<Vec<DeviceView>>;

    /// Signs another of the account's devices out, answering the server's
    /// user-interactive auth with `password` when it asks.
    fn sign_out_device(
        &self,
        account: &AccountKey,
        device_id: &str,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome>;

    /// Writes every room key this device has to `path` (which the person
    /// chose), encrypted with `passphrase`.
    fn export_room_keys(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Reply<()>;

    /// Reads room keys from a file `export_room_keys` (or another Matrix
    /// client) wrote, with its passphrase.
    fn import_room_keys(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Reply<KeyImport>;

    /// The identity warnings a room shows (members whose identity changed),
    /// as they change.
    fn identity_warnings(
        &self,
        account: &AccountKey,
        room: &RoomId,
    ) -> Reply<watch::Receiver<Vec<IdentityWarning>>>;

    /// Answers a warning about `user`: acknowledges the new identity, or
    /// withdraws the verification of the old one.
    fn acknowledge_identity(&self, account: &AccountKey, user: &UserId) -> Reply<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_report_when_their_session_side_has_gone() {
        let (_updates_tx, updates) = mpsc::channel(1);
        let (_loading_tx, loading) = watch::channel(RoomListLoading::NotLoaded);
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let list = RoomListSubscription::new(updates, loading, commands_tx);
        assert!(list.set_filter(RoomListFilter::People));
        assert!(list.load_more());
        assert_eq!(
            commands.try_recv(),
            Ok(RoomListCommand::SetFilter(RoomListFilter::People))
        );
        assert_eq!(commands.try_recv(), Ok(RoomListCommand::LoadMore));
        drop(commands);
        assert!(!list.first_page_only());

        let (_updates_tx, updates) = mpsc::channel(1);
        let (commands_tx, commands) = mpsc::unbounded_channel();
        let timeline = TimelineSubscription::new(updates, commands_tx);
        drop(commands);
        assert!(!timeline.paginate_backwards(20));
    }

    #[test]
    fn errors_have_sentences() {
        assert_eq!(
            SessionError::UnknownAccount.to_string(),
            "That account is not signed in."
        );
        assert_eq!(SessionError::Failed("x".to_owned()).to_string(), "x");
        assert!(
            SessionError::Paused
                .to_string()
                .starts_with("Chat is paused")
        );
    }
}
