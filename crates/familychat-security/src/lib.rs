//! Family Chat's secrets and security defaults (PLAN.md, sections 5.4 and
//! 7; ADR 0003).
//!
//! - [`keychain`]: the OS keychain under one service id, its typed entries
//!   (`app-store`, `account-<uuid>-session`, `account-<uuid>-store`) and
//!   the mock every test uses.
//! - [`secret`]: [`Secret`], whose `Debug` prints nothing of the value and
//!   which is wiped when dropped, and [`Passphrase`], 32 bytes from the OS
//!   CSPRNG.
//! - [`redact`]: [`Redacted`] and the scrubber every log line passes
//!   through.
//! - [`logging`]: the local log file and the crash log.
//!
//! Later phases add the Matrix HTML and terminal sanitisers, safe download
//! names and image limits here (PLAN.md, section 5.1).

pub mod keychain;
pub mod logging;
pub mod redact;
pub mod secret;
pub mod time;

pub use keychain::{
    AccountId, AccountSession, AuthKind, Keychain, KeychainEntry, KeychainError, KeychainFailure,
    MemoryKeychain, OsKeychain,
};
pub use redact::Redacted;
pub use secret::{PASSPHRASE_LEN, Passphrase, REDACTED, Secret};
