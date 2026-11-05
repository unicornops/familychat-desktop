//! Family Chat's sign-in (PLAN.md, sections 6.2 to 6.6; ADR 0006): every
//! rule of family-chat's `docs/client-login-links.md` ("What each client
//! must implement") and password sign-in for Family Chat and other
//! servers, as one GPUI-free state machine both frontends drive through
//! `familychat-core`'s [`SigninApi`](familychat_core::signin::SigninApi).
//!
//! | Module | What it does |
//! | --- | --- |
//! | [`host`] | the strict bare `hostname[:port]` parser every server name goes through |
//! | [`allowlist`] | the environments' suffixes, the check on resolved homeserver URLs, [`CheckedHomeserver`] |
//! | [`link`] | sign-in links (`https://safechat.family/app/login?...`, `familychat://...`, pasted text) |
//! | [`discovery`] | `.well-known/matrix/client`, the allowlist, `/versions` |
//! | [`http`] | the sign-in's own HTTP client: no redirects, no retries |
//! | [`login`] | `m.login.token` and `m.login.password`, and signing out a device not kept |
//! | [`handoff`] | handing credentials to matrix-sdk and the post-login re-check |
//! | [`flow`] | the state machine |
//! | [`live`] | [`LiveSignin`], the `SigninApi` the app wires |
//! | [`wording`] | every sentence the sign-in shows |
//! | [`device`] | `Family Chat (macOS|Windows|Linux)` |
//!
//! Tokens and passwords are [`Secret`](familychat_security::Secret)s from
//! the moment they are parsed: never logged, never in `Debug` or a panic
//! message, never stored, and a code is wiped as soon as its request is
//! built.

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!(
    "the `test-harness` feature allows the homeserver harness's *.familychat.test and is for debug test builds only (ADR 0004, ADR 0006)"
);

pub mod allowlist;
pub mod device;
pub mod discovery;
pub mod flow;
pub mod handoff;
pub mod host;
pub mod http;
pub mod link;
pub mod live;
pub mod login;
pub mod wording;

pub use allowlist::{Allowlist, CheckedHomeserver};
pub use device::device_display_name;
pub use flow::{Services, run_flow};
pub use handoff::{AccountSink, AddFailure, FamilyChatCheck, OtherServerCheck};
pub use host::ServerName;
pub use http::{HttpRequest, HttpResponse, ReqwestTransport, Transport, TransportError};
pub use link::LoginLink;
pub use live::LiveSignin;
