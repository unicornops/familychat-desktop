//! Opening the profile, as a frontend sees it (PLAN.md, section 5.4; ADR
//! 0003): the window asks [`StartupApi::open`] and draws what comes back.
//!
//! The profile's states are `familychat-store`'s (ready, keychain
//! unavailable, unreadable); opening them, restoring the accounts and
//! building the sessions happen off the UI thread, in the app's wiring.
//! A frontend never sees the store, the keychain or a Matrix type: only
//! the sentences to show and, once the profile is open, the
//! [`SessionApi`] and [`SigninApi`] to draw from.

use std::{fmt, sync::Arc};

use tokio::sync::oneshot;

use crate::{session::SessionApi, signin::SigninApi};

/// The services a frontend draws from once the profile is open.
#[derive(Clone)]
pub struct Opened {
    /// The accounts, their sync and their rooms.
    pub session: Arc<dyn SessionApi>,
    /// New sign-ins.
    pub signin: Arc<dyn SigninApi>,
    /// Something to say once, such as where Start over moved the old data.
    pub notice: Option<String>,
}

impl fmt::Debug for Opened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opened")
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

/// What opening the profile came to.
#[derive(Debug, Clone)]
pub enum OpenState {
    /// Open: the accounts in the registry were restored.
    Ready(Opened),
    /// The keychain is locked or can't be reached: the sentence to show,
    /// with Try again. There is no plaintext fallback.
    KeychainUnavailable(String),
    /// The data's key is gone: the sentence to show, with Start over.
    Unreadable(String),
    /// Something else failed (a full disk, a newer version's data): the
    /// sentence to show, with Try again.
    Failed(String),
}

/// Opens the profile for a frontend. Each call returns at once; the answer
/// arrives on the receiver once the work, done off the UI thread, is
/// finished.
pub trait StartupApi: Send + Sync {
    /// Opens the profile, or tries again after it was not ready.
    fn open(&self) -> oneshot::Receiver<OpenState>;

    /// Moves unreadable data aside and starts a new store (only after
    /// [`OpenState::Unreadable`]; otherwise it answers as [`Self::open`]).
    fn start_over(&self) -> oneshot::Receiver<OpenState>;
}
