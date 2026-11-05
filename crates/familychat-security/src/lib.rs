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
//! - [`matrix_html`]: the Matrix HTML sanitiser, which turns a message's
//!   `formatted_body` into `familychat-core`'s rich-text tree (ADR 0007).
//! - [`images`]: decoding message images within byte, size and frame
//!   limits, never SVG.
//! - [`file_names`]: safe names for downloaded files.
//! - [`terminal`]: the terminal sanitiser every server string goes through
//!   before the terminal UI draws it (ADR 0008).

pub mod file_names;
pub mod images;
pub mod keychain;
pub mod logging;
pub mod matrix_html;
pub mod redact;
pub mod secret;
pub mod terminal;
pub mod time;

pub use keychain::{
    AccountId, AccountSession, AuthKind, Keychain, KeychainEntry, KeychainError, KeychainFailure,
    MemoryKeychain, OsKeychain,
};
pub use redact::Redacted;
pub use secret::{PASSPHRASE_LEN, Passphrase, REDACTED, Secret};
