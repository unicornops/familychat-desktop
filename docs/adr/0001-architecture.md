# ADR 0001: Architecture

## Status

Accepted on November 5, 2026, with Phase 0 (issue #1). The plan it
records is [`PLAN.md`](../../PLAN.md), sections 4 and 5; Rob's open
decisions are in its section 15, and the ones Phase 0 depends on are
listed under "Decisions" below.

## Context

Family Chat Desktop is a native Matrix client for macOS, Linux and
Windows, built first for Family Chat (`safechat.family`) and usable with
any homeserver. It has two frontends, a GPUI window and a ratatui terminal
UI, which must behave the same way: the same sign-in rules, the same
wording, the same security checks. It talks to Matrix through the
matrix-rust-sdk, which runs on Tokio, while GPUI has executors of its own
and no Tokio reactor.

Zmail (unicornops/zmail) already solved the same shape of problem for
mail: a Rust workspace with a GPUI shell and a terminal UI over shared,
UI-free crates, a dedicated Tokio runtime, a strict dependency policy and
a release flow from Conventional Commits. Family Chat Desktop copies that
style instead of inventing its own (PLAN.md, section 11).

## Decision

### A Rust workspace in Zmail's style

- Edition 2024, `resolver = "3"`, `publish = false`, licence
  `GPL-3.0-only`, with Zmail's workspace lints: `missing_docs` warns,
  `unsafe_code` is forbidden, and clippy's `dbg_macro`, `todo` and
  `unwrap_used` are denied.
- The toolchain is the one Zed pins for the GPUI tag (ADR 0002), in
  `rust-toolchain.toml` and `rust-version`.
- GPUI is a git dependency on a Zed stable release tag (ADR 0002), never
  the crates.io `gpui` crate; the app starts with
  `gpui_platform::application()`, which keeps AccessKit on.
- `cargo deny` (licences, advisories, bans, sources), `cargo audit` and
  `cargo about` run in CI; Dependabot proposes updates weekly.
- Conventional Commits only; release-please makes the release pull
  request and the `vX.Y.Z` tags, and the release workflow attaches the
  platform archives.

### Crates

Each crate has one job, and only the frontends know about a UI toolkit
(PLAN.md, section 5.1):

| Crate | Job | GPUI | ratatui | matrix-sdk |
| --- | --- | --- | --- | --- |
| `familychat-app` | the `familychat` binary: CLI, wiring, `build.rs` | yes | yes (feature `tui`) | through the session |
| `familychat-ui` | the GPUI frontend | yes | no | no |
| `familychat-tui` | the terminal frontend | no | yes | no |
| `familychat-core` | identifiers, domain types and view models both frontends draw | no | no | no |
| `familychat-session` | the background runtime and the Matrix sessions (ADR 0005) | no | no | yes |
| `familychat-theme` | Zed theme compatibility and the bundled themes | no | no | no |
| `familychat-assets` | embedded icons and the Linux UI font | GPUI's `AssetSource` | no | no |

Later phases add `familychat-signin`, `-actions`, `-security`, `-store`,
`-notify` and `-test-support` (PLAN.md, section 5.1). Phase 0 has only the
seven above.

Rules, enforced in CI by `scripts/check-dependency-boundaries.sh` from
Phase 3 (ADR 0005):

- `familychat-ui` and `familychat-tui` never depend on matrix-sdk, on
  matrix-sdk-ui, or on the other frontend's toolkit.
- `familychat-core`'s public API exposes no matrix-sdk types.
- `unsafe_code = "forbid"` everywhere. Code that cannot avoid `unsafe`
  lives in one module with every block justified, as Zmail's renderer
  FFI does.

### GPUI and Tokio

`familychat_session::BackgroundRuntime`, copied from Zmail's `zmail-sync`,
starts a multi-thread Tokio runtime on a dedicated thread and probes it
before returning. Every SDK future runs on that runtime; results come back
through runtime-agnostic channels (`tokio::sync::oneshot` and `mpsc`
receivers) that GPUI tasks await in `cx.spawn` and the terminal UI polls
on each tick. The UI thread never awaits an SDK future on a GPUI executor
and never blocks on the network. `familychat-ui`'s view tests include
`a_gpui_task_awaits_a_future_that_ran_on_tokio`, which runs a future that
needs Tokio's timer on the runtime and awaits its result from a GPUI task.

Zed ships a `gpui_tokio` crate that runs a Tokio runtime inside a GPUI
app. It is not used: Zmail's pattern keeps the frontends independent of
how the session crate runs its futures, and the terminal UI, which has no
GPUI, needs the same runtime.

### Identifiers

From PLAN.md's Open decision 2, all in `familychat_core::identity` so
they change in one place:

| | |
| --- | --- |
| App and bundle id, keychain service | `family.safechat.desktop` |
| Binary and Debian package | `familychat` |
| Data directory | "Family Chat" (macOS, Windows), `familychat` (`$XDG_DATA_HOME`), overridden by `FAMILYCHAT_DATA_DIR` |
| URL scheme | `familychat` (registered with the OS in Phase 12) |

### No code from the Element forks

The Element forks Family Chat's mobile and web apps are built on
(Element X on iOS and Android, Element Web) are AGPL-3.0. Nothing is copied from them into this GPL-3.0-only codebase;
behaviour is reimplemented from the Matrix specification and Family
Chat's own contract documents (PLAN.md, section 2).

## Consequences

- Both frontends share every rule and sentence that is not drawing, so a
  sign-in or security rule cannot differ between them.
- The window and the terminal UI cannot open the same profile at once:
  matrix-sdk's crypto store must have one writer (PLAN.md, section 5.5).
- Upstream inputs are pinned tightly and moved deliberately: GPUI by the
  weekly bump workflow (ADR 0002), matrix-sdk to an exact minor from
  Phase 3.
- The Phase 0 window has system decorations only. Zmail's client-side
  title bar and window frame (for compositors without server-side
  decorations, such as GNOME on Wayland) arrive with the GUI shell in
  Phase 5.

## Decisions

Taken from PLAN.md's recommendations for Phase 0; Rob may revisit them:

1. Licence `GPL-3.0-only` (Open decision 1).
2. The identifiers above (Open decision 2).
