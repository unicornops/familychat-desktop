//! Family Chat's local data (PLAN.md, section 5.4; ADR 0003): where a
//! profile lives, the lock that gives it one process, and the app store
//! with the accounts registry, preferences and UI state.
//!
//! - [`paths`]: the data directory per OS, `FAMILYCHAT_DATA_DIR`, and the
//!   layout inside it.
//! - [`lock`]: the exclusive profile lock, `familychat.lock`.
//! - [`profile`]: opening a profile, and its states: ready, keychain
//!   unavailable (retry; never a plaintext fallback), and unreadable (start
//!   over moves the old data aside).
//! - The app store itself (`familychat.sqlite3`): SQLite with migrations,
//!   identifying values encrypted with matrix-sdk's `StoreCipher`.
//!
//! The SDK's own stores (`accounts/<uuid>/sdk/` and `accounts/<uuid>/cache/`)
//! are opened by `familychat-session`, with the passphrase
//! [`Profile::account_store_passphrase`] gives.

mod app_store;
pub mod lock;
pub mod paths;
pub mod profile;
#[cfg(test)]
mod test_dir;

pub use app_store::{AccountSummary, NewAccount, Preference, StoreError};
pub use lock::{LockError, ProfileLock};
pub use paths::{DataDir, DataDirError};
pub use profile::{
    KeychainUnavailable, OpenError, Profile, ProfileState, RemoveAccountError, StartedOver,
    Unreadable, UnreadableReason, keychain_unavailable_message, linux_keychain_unavailable_message,
    unreadable_message,
};
