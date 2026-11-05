//! What a failed request means for an account: the classification the sync
//! supervisor turns into a [`SyncState`](familychat_core::sync::SyncState).
//!
//! The important cases are the ones a retry cannot fix, or must not be
//! hurried: a suspended account (`M_USER_SUSPENDED`, a Family Chat curfew)
//! is its own state with slow rechecks, not an error and not a reason to
//! restart the sync at once; a revoked token is "signed out"; a server
//! without native sliding sync is "not supported".

use std::time::Duration;

use matrix_sdk::{
    ClientBuildError, HttpError, RefreshTokenError,
    ruma::api::error::{ErrorKind, RetryAfter},
    sliding_sync::VersionBuilderError,
};
use matrix_sdk_ui::{encryption_sync_service, room_list_service, sync_service};

/// What went wrong, as far as retrying is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// `M_USER_SUSPENDED`.
    Suspended,
    /// The token is no longer accepted (`M_UNKNOWN_TOKEN`, or a refresh
    /// that failed for good).
    SignedOut {
        /// The server's `soft_logout`.
        soft_logout: bool,
    },
    /// No native sliding sync.
    NotSupported,
    /// `M_LIMIT_EXCEEDED`, with the server's wait when it gave one.
    RateLimited(Option<Duration>),
    /// The server could not be reached, or answered with a server error.
    Unreachable,
    /// Anything else. The text is the error's own, for the log; the user
    /// sees a fixed sentence.
    Other(String),
}

impl Failure {
    /// The failure behind a stopped [`sync_service::SyncService`].
    pub(crate) fn from_sync(error: &sync_service::Error) -> Self {
        let sdk = match error {
            sync_service::Error::RoomList(room_list_service::Error::SlidingSync(error))
            | sync_service::Error::EncryptionSync(
                encryption_sync_service::Error::SlidingSync(error)
                | encryption_sync_service::Error::ClientError(error)
                | encryption_sync_service::Error::LockError(error),
            ) => error,
            other => return Self::Other(other.to_string()),
        };
        Self::from_sdk(sdk)
    }

    /// The failure behind an SDK error.
    pub(crate) fn from_sdk(error: &matrix_sdk::Error) -> Self {
        match error {
            matrix_sdk::Error::Http(http) => Self::from_http(http),
            other => Self::Other(other.to_string()),
        }
    }

    /// The failure behind an HTTP error.
    pub(crate) fn from_http(error: &HttpError) -> Self {
        match error {
            HttpError::Reqwest(_) => Self::Unreachable,
            HttpError::Cached(inner) => Self::from_http(inner),
            HttpError::RefreshToken(refresh) => Self::from_refresh(refresh),
            other => {
                if let Some(api) = other.as_client_api_error() {
                    if let Some(failure) = api.error_kind().and_then(Self::from_kind) {
                        return failure;
                    }
                    if api.status_code.is_server_error() {
                        return Self::Unreachable;
                    }
                }
                Self::Other(other.to_string())
            }
        }
    }

    fn from_refresh(error: &RefreshTokenError) -> Self {
        match error {
            RefreshTokenError::RefreshTokenRequired => Self::SignedOut { soft_logout: false },
            RefreshTokenError::MatrixAuth(http) => match Self::from_http(http) {
                // A refresh the server refused for good means signing in
                // again; anything else (no network) is retried.
                failure @ (Self::Unreachable | Self::RateLimited(_) | Self::Suspended) => failure,
                Self::SignedOut { soft_logout } => Self::SignedOut { soft_logout },
                _ => Self::SignedOut { soft_logout: false },
            },
            other => Self::Other(other.to_string()),
        }
    }

    fn from_kind(kind: &ErrorKind) -> Option<Self> {
        Some(match kind {
            ErrorKind::UserSuspended => Self::Suspended,
            ErrorKind::UnknownToken(data) => Self::SignedOut {
                soft_logout: data.soft_logout,
            },
            ErrorKind::LimitExceeded(data) => {
                Self::RateLimited(data.retry_after.as_ref().and_then(|after| match after {
                    RetryAfter::Delay(delay) => Some(*delay),
                    RetryAfter::DateTime(_) => None,
                }))
            }
            // The sliding sync endpoint is not there.
            ErrorKind::Unrecognized => Self::NotSupported,
            _ => return None,
        })
    }

    /// The failure behind a client that could not be built.
    pub(crate) fn from_build(error: &ClientBuildError) -> Self {
        match error {
            ClientBuildError::SlidingSyncVersion(
                VersionBuilderError::NativeVersionIsUnset
                | VersionBuilderError::MissingVersionsResponse,
            ) => Self::NotSupported,
            ClientBuildError::Http(http) => Self::from_http(http),
            other => Self::Other(other.to_string()),
        }
    }
}
