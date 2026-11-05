//! An account's sync state, as both frontends show it (PLAN.md, section
//! 5.6), with its wording (section 5.3: the sentences live next to the
//! logic, so the window and the terminal UI say the same thing).

use std::time::Duration;

use crate::identity::APP_NAME;

/// Where an account's sync is.
///
/// The session moves an account between these states on its own: it starts
/// in [`SyncState::Idle`], is [`SyncState::Connecting`] until the first
/// sync answer, then [`SyncState::Running`]. A failure becomes one of the
/// other states, each with its own retry rule, so no failure turns into a
/// stream of requests:
///
/// | State | Retries |
/// | --- | --- |
/// | [`SyncState::Offline`] | with backoff (one second, doubling to a minute) |
/// | [`SyncState::Suspended`] | slowly (half a minute, doubling to five minutes) |
/// | [`SyncState::Error`] | with backoff, as offline |
/// | [`SyncState::NotSupported`], [`SyncState::SignedOut`] | never on its own |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncState {
    /// Not started (or stopped).
    Idle,
    /// Started; waiting for the server's first answer.
    Connecting,
    /// Syncing: new messages arrive as they are sent.
    Running,
    /// The server can't be reached (no network, a timeout, a server error).
    Offline {
        /// When the next try is.
        retry_in: Duration,
    },
    /// The homeserver refuses the account for now with `M_USER_SUSPENDED`:
    /// a Family Chat curfew, or a server administrator. Not an error and
    /// not a reason to sign in again; the session checks again slowly.
    ///
    /// The pinned Palpo refuses every request of a suspended account,
    /// reading included (family-chat decision 11 is open); whatever the
    /// server allows while suspended, this is the state to show instead of
    /// an error.
    Suspended {
        /// When the next check is.
        recheck_in: Duration,
    },
    /// The homeserver does not offer native sliding sync (MSC4186), which
    /// this app needs (PLAN.md, Open decision 6).
    NotSupported,
    /// The homeserver no longer accepts this sign-in (the token was revoked
    /// or expired): sign in again.
    SignedOut {
        /// The server asked for a "soft" logout: the local data may be kept
        /// and the same account signed in again.
        soft_logout: bool,
    },
    /// Something else went wrong.
    Error {
        /// What went wrong, for the user; never a token or a message body.
        message: String,
        /// When the next try is.
        retry_in: Duration,
    },
}

impl SyncState {
    /// Whether the session will try again on its own.
    pub fn retries(&self) -> bool {
        matches!(
            self,
            Self::Offline { .. } | Self::Suspended { .. } | Self::Error { .. }
        )
    }

    /// A short name for the state, as a status indicator shows it. The
    /// suspended state has its own ("Paused"): it is not an error.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "Not syncing",
            Self::Connecting => "Connecting…",
            Self::Running => "Up to date",
            Self::Offline { .. } => "Offline",
            Self::Suspended { .. } => "Paused",
            Self::NotSupported => "Not supported",
            Self::SignedOut { .. } => "Signed out",
            Self::Error { .. } => "Sync problem",
        }
    }

    /// Whether this is a state the user should see a sentence for.
    pub fn message(&self) -> Option<String> {
        Some(match self {
            Self::Idle | Self::Connecting | Self::Running => return None,
            Self::Offline { .. } => {
                format!("{APP_NAME} can't reach your server. It will keep trying.")
            }
            Self::Suspended { .. } => {
                "Chat is paused for now. It starts again when the pause ends.".to_owned()
            }
            Self::NotSupported => format!(
                "This server doesn't support the way {APP_NAME} syncs (native sliding sync). Ask \
                 its administrator to turn it on, or use another app."
            ),
            Self::SignedOut { .. } => {
                "You have been signed out of this account. Sign in again to keep chatting."
                    .to_owned()
            }
            Self::Error { message, .. } => message.clone(),
        })
    }
}

/// Something about an account the user should know that is not its sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountProblem {
    /// The homeserver renewed the sign-in, but the keychain could not keep
    /// the new token: the account may need signing in again after a
    /// restart.
    TokenNotSaved,
}

impl AccountProblem {
    /// What to tell the user.
    pub fn message(&self) -> String {
        match self {
            Self::TokenNotSaved => format!(
                "{APP_NAME} couldn't save your sign-in in the keychain. If the keychain stays \
                 unavailable you will need to sign in again after {APP_NAME} restarts."
            ),
        }
    }
}

/// An account's live status: its sync and anything else to tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatus {
    /// The sync state.
    pub sync: SyncState,
    /// A problem to show alongside it.
    pub problem: Option<AccountProblem>,
}

impl Default for AccountStatus {
    fn default() -> Self {
        Self {
            sync: SyncState::Idle,
            problem: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_failures_have_sentences() {
        for quiet in [SyncState::Idle, SyncState::Connecting, SyncState::Running] {
            assert!(quiet.message().is_none(), "{quiet:?}");
            assert!(!quiet.retries());
        }
        let suspended = SyncState::Suspended {
            recheck_in: Duration::from_secs(30),
        };
        assert!(suspended.retries());
        let message = suspended.message().expect("a sentence");
        assert!(message.starts_with("Chat is paused"), "{message}");
        assert!(!message.contains("error"), "{message}");
        assert!(!SyncState::NotSupported.retries());
        assert!(
            SyncState::NotSupported
                .message()
                .expect("a sentence")
                .contains("sliding sync")
        );
        assert!(!SyncState::SignedOut { soft_logout: false }.retries());
    }

    #[test]
    fn suspended_has_its_own_label() {
        let suspended = SyncState::Suspended {
            recheck_in: Duration::from_secs(30),
        };
        assert_eq!(suspended.label(), "Paused");
        let error = SyncState::Error {
            message: "x".to_owned(),
            retry_in: Duration::from_secs(1),
        };
        assert_ne!(suspended.label(), error.label());
        assert_eq!(SyncState::Running.label(), "Up to date");
    }
}
