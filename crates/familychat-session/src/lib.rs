//! Family Chat's Matrix sessions (PLAN.md, sections 5.1, 5.2 and 5.6; ADR
//! 0005): the background runtime, and on it one `matrix_sdk::Client` per
//! account, synced with matrix-sdk-ui's `SyncService`, with its room list
//! and timelines bridged to the frontends as `familychat-core` view models.
//!
//! - [`BackgroundRuntime`]: the dedicated Tokio runtime every SDK future
//!   runs on (copied from Zmail's `zmail-sync`).
//! - [`AccountManager`]: restores the registry's accounts from the keychain
//!   at start, and adds new sign-ins through a [`PostLoginCheck`].
//! - [`Account`]: one client, its stores, its refresh-token persistence and
//!   the supervisor that keeps its sync going and reports its
//!   [`SyncState`](familychat_core::sync::SyncState).
//! - [`LiveSession`]: the [`SessionApi`](familychat_core::session::SessionApi)
//!   both frontends consume, with each account's notifications (the SDK's
//!   notification handler) and unread total.
//! - `encryption`: the client's encryption settings (ADR 0009), the
//!   account's identity, recovery, verification, devices, room key files
//!   and identity warnings.
//! - `qr_login`: sign-in with a QR code (MSC4108, ADR 0010), for this
//!   computer and for a new device of a signed-in account.
//! - [`LiveSettings`]: the [`SettingsApi`](familychat_core::settings::SettingsApi)
//!   over the profile's preferences.
//!
//! The frontends never depend on this crate's Matrix half: they see
//! `SessionApi` and the view models only
//! (`scripts/check-dependency-boundaries.sh`).

// matrix-sdk's futures nest deeply; the default limit is too low for the
// futures that open a room list or a timeline.
#![recursion_limit = "256"]

mod account;
mod backoff;
mod config;
mod encryption;
mod failure;
mod live;
mod manager;
mod mapping;
mod media;
mod messaging;
mod notifications;
mod qr_login;
mod room_list;
mod runtime;
mod settings;
mod timeline;

pub use account::{Account, OpenError, account_key};
pub use config::SessionConfig;
pub use live::LiveSession;
pub use manager::{AccountManager, AddAccountError, AnyHomeserver, PostLoginCheck, Restored};
pub use media::{AuthenticatedMediaFetcher, DOWNLOAD_TIMEOUT, MAX_DOWNLOAD_BYTES};
pub use qr_login::{QrAnswer, QrEvent, QrLoginError, QrLoginRequest, UrlGuard};
pub use runtime::BackgroundRuntime;
pub use settings::LiveSettings;
