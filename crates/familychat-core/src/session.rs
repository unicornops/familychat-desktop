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

use ruma::{OwnedUserId, RoomId};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    diff::ListDiff,
    media::{DecodedImage, MediaError, MediaInfo},
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
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionError {}

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

    /// Accepts (`accept`) or declines an invitation to `room`.
    fn answer_invite(&self, account: &AccountKey, room: &RoomId, accept: bool) -> Reply<()>;

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
    }
}
