//! Test support for Family Chat Desktop.
//!
//! With the `fixtures` feature, [`fixtures`] has the fixture
//! implementations of `familychat-core`'s `SessionApi`, `SigninApi` and
//! `StartupApi` that the window's view tests and
//! `familychat launch-ui-fixture` (the screenshot matrix) draw from.
//!
//! [`HarnessEnv`] reads the settings `scripts/homeserver-harness.sh up`
//! writes for the local homeserver harness (ADR 0004,
//! `docker/homeserver-harness`). With the `test-harness` feature, which only
//! test builds turn on, the crate adds the harness driver,
//! `HomeserverHarness`, and its HTTP client, which trusts the harness's
//! throwaway CA and resolves the harness hostnames to its front end and
//! nothing else.

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!(
    "the `test-harness` feature trusts the homeserver harness's throwaway CA and is for debug test builds only (ADR 0004)"
);

mod env;
#[cfg(feature = "fixtures")]
pub mod fixtures;
pub mod photo;

pub use env::{DEFAULT_STATE_DIR, ENV_FILE_VAR, HarnessEnv, STATE_DIR_VAR, parse_env_file};

#[cfg(feature = "test-harness")]
mod client;
#[cfg(feature = "test-harness")]
mod harness;

#[cfg(feature = "test-harness")]
pub use client::{HarnessResolver, admin_client, edge_client, edge_client_builder};
#[cfg(feature = "test-harness")]
pub use harness::{AccessLogLine, HomeserverHarness, LoginToken, Reply, Session, TestUser};
