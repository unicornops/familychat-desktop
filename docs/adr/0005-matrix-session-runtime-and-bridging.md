# ADR 0005: The Matrix Session: Runtime, Bridging and One Process per Profile

## Status

Proposed with Phase 3 (issue #4); accepted when that pull request
merges. It records PLAN.md, sections 5.1 to 5.6 and 6.6, as built.

## Context

Family Chat Desktop talks Matrix through the matrix-rust-sdk. The SDK
needs Tokio (reqwest, hyper, its SQLite stores); GPUI has its own
executors and no Tokio reactor, and the terminal UI is a plain draw and
input loop on the main thread. Both frontends must show the same rooms,
timelines and states with the same wording, and neither may block its UI
thread on the network, the disk or crypto.

The SDK's types change in most releases, so they must stay behind one
crate, and the frontends must not be able to reach them. And the SDK's
crypto store must have one writer: two processes on one profile could
corrupt the device's keys.

## Decision

### Versions and features

`matrix-sdk` and `matrix-sdk-ui` are pinned to `=0.19.1` (with
`matrix-sdk-store-encryption =0.19.1` and `ruma =0.17.2`, the ruma they
use), and move together (Dependabot's `matrix` group). They use the
reqwest 0.13 and rusqlite 0.40 lines already in the lock. Features, with
the defaults off:

| Crate | Features | Why |
| --- | --- | --- |
| `matrix-sdk` | `e2e-encryption`, `sqlite`, `bundled-sqlite`, `rustls-aws-lc-rs` | encryption, the SQLite stores (SQLite built from source, as rusqlite's `bundled` already does), rustls with aws-lc-rs and no native TLS |
| `matrix-sdk-ui` | `rustls-aws-lc-rs` | `SyncService`, `RoomListService` and `Timeline` need nothing more |

`automatic-room-key-forwarding`, a default, is off: answering other
devices' key requests automatically is left to verification and key
backup (Phase 8). `ruma` is used without features, for its identifiers
only.

Two policy changes come with the SDK: `BSL-1.0` (xxhash-rust, through
growable-bloom-filter) joins the accepted licences in `deny.toml` and
`about.toml`, and `deny.toml` ignores RUSTSEC-2026-0319 (`anymap2` is
unmaintained; matrix-sdk 0.19.1 uses it and has no release without it).
Both are re-checked when the Matrix crates move.

### Crates and the boundary

- `familychat-core` holds the `SessionApi` trait and the view models both
  frontends draw: `AccountView` and `AccountStatus`, `SyncState` with its
  wording, `RoomSummary` and `RoomListFilter`, `TimelineItem` (message text
  in `MessageBody`, whose `Debug` prints only its length), and
  `ListDiff<T>`, the SDK's `VectorDiff` without the SDK. It depends on
  ruma's identifiers and on Tokio's `sync` primitives, nothing else.
- `familychat-session` is the only crate that depends on matrix-sdk and
  matrix-sdk-ui. It holds `BackgroundRuntime`, the `AccountManager`, the
  `Account` and its supervisor, the bridges for the room list and the
  timelines, and `LiveSession`, the `SessionApi` implementation.
- `scripts/check-dependency-boundaries.sh`, in CI's quality job, fails if
  `familychat-ui` or `familychat-tui` depends on matrix-sdk or any
  `matrix-sdk-*` crate (directly or through another crate) or lists the
  other frontend's toolkit, or if `familychat-core` depends on any
  matrix-sdk crate (so its API cannot expose an SDK type). It self-tests
  that it sees matrix-sdk in the session's graph and GPUI in the window's.
  A frontend's dev-dependencies may reach the session crate (the window's
  runtime test does); they never ship.

### The runtime and the bridging

Every SDK future runs on `BackgroundRuntime` (copied from Zmail's
`zmail-sync`): a multi-thread Tokio runtime on its own thread, started and
probed before the frontend runs. `SessionApi`'s methods never block: each
spawns its work on the runtime and returns a channel from `tokio::sync`,
which needs no runtime to receive from. A GPUI task awaits it in
`cx.spawn` and updates its entity; the terminal UI polls it with
`try_recv` on each tick.

- Values that change are `watch` receivers: the accounts, each account's
  status.
- Streams are bounded `mpsc` receivers (64 batches) of `Vec<ListDiff<T>>`:
  the room list (`RoomListSubscription`, with its loading state and its
  filter and paging controls) and each timeline (`TimelineSubscription`,
  with back-pagination). The SDK's eyeball streams are polled in a Tokio
  task per subscription, each batch mapped to core types as it arrives; a
  frontend that falls behind makes that task wait, so nothing queues
  without bound. The first batch is a `Reset` with the whole list.
  Dropping the subscription ends the task.
- One-off answers are `oneshot` receivers (`Reply<T>`).

Coalescing is the frontend's: it drains what is waiting before it redraws,
so a burst of batches costs one frame.

### The client

One `matrix_sdk::Client` per account, built with:

- SQLite stores in `accounts/<uuid>/sdk/` (state, crypto) and
  `accounts/<uuid>/cache/` (event cache, media), through
  `sqlite_store_with_cache_path` with the account's passphrase from the
  keychain's `account-<uuid>-store` (32 random bytes, as hexadecimal), so
  every value is encrypted with the SDK's `StoreCipher`. The cache lives in
  the data directory, not the OS cache directory PLAN.md first named for
  media: start over and account removal then cover it, and the SDK's
  cache path holds the event cache (messages) as well as media. A live
  test scans every file of an account's stores for message text and fails
  if it finds any.
- Family Chat's own reqwest client (`SessionConfig::http_client`): HTTPS
  only, TLS 1.2 or later, the system's roots through rustls with aws-lc-rs,
  no redirects followed, a connect timeout. It is injectable: the live
  tests pass the harness's client, which trusts only the harness CA
  (ADR 0004), and the mocked tests a plain one for their loopback server;
  nothing in the session crate trusts a test CA.
- `respect_login_well_known(false)`: a login response cannot move the
  account to another homeserver (PLAN.md, section 6.6).
- `handle_refresh_tokens()`, with the client's save-session callback
  writing each rotated access and refresh token to the keychain's
  `account-<uuid>-session`. The SDK calls it inside its refresh, before
  the request that needed the refresh is retried, so the keychain holds
  the new tokens before they are used (a mocked test checks it at the
  moment the server first sees the new token; a live Synapse test checks
  that they survive a restart). A keychain that refuses the write shows as
  `AccountProblem::TokenNotSaved`. `subscribe_to_session_changes()`
  reports a token the server no longer accepts.
- Native sliding sync found through `/versions`
  (`VersionBuilder::DiscoverNative`). A server without it is "not
  supported" (Open decision 6): a new sign-in fails with
  `OpenError::NotSupported`, and a restored account opens in
  `SyncState::NotSupported` and checks again only when asked. When the
  server can't be reached at start, the client is built from what is
  stored with native sliding sync assumed (the account synced before), so
  an account opens offline.

The account manager restores every account in the registry from its
keychain session at start, with no sign-in. A new sign-in
(`AccountManager::add_account`) goes into the registry and the keychain,
opens, and passes a `PostLoginCheck` with the client's homeserver and user
before anything syncs; a refusal signs the new device out and forgets the
account (keychain entries, files, registry row). Phase 4's Family Chat
allowlist re-check is a `PostLoginCheck`.

### Sync and its states

Each account's supervisor runs matrix-sdk-ui's `SyncService` and maps it
to `SyncState`: `Connecting` until the room list's first answer,
`Running` after it. The SDK's own offline mode is not used: after any
failure it restarts the sync as soon as `/versions` answers, which for a
suspended account (whose `/versions` works and whose every other request
is refused) is a request loop. The supervisor classifies each failure
instead:

| Failure | State | Next try |
| --- | --- | --- |
| no network, a timeout, a 5xx | `Offline` | 1 s, doubling to 60 s |
| `M_LIMIT_EXCEEDED` | `Offline` | the server's wait, at least the backoff |
| `M_USER_SUSPENDED` | `Suspended` | 30 s, doubling to 5 minutes |
| `M_UNKNOWN_TOKEN`, a refresh refused | `SignedOut` | only when asked |
| `M_UNRECOGNIZED` from the sync endpoint | `NotSupported` | only when asked |
| anything else | `Error` (a fixed sentence) | 5 s, doubling to 5 minutes |

A sync that reaches the server again resets the backoffs. Before the
first start the supervisor lets the SDK's encryption set-up after the
restore finish (at most 15 seconds): run alongside the sync's first key
upload it can stall both inside the pinned SDK (ADR 0010). `retry_sync`
skips a wait.

`Suspended` is its own state, never an error: a Family Chat curfew
suspends the account, and on the pinned Palpo every authenticated request
of a suspended account, `/sync` included, gets 403 `M_USER_SUSPENDED`
(found by the Phase 2 harness; family-chat decision 11 is open). Whatever
Palpo ends up allowing while suspended, this state holds: if reads are
allowed, the sync never meets the error and the suspension shows on
sends (Phase 7); if not, the account shows "Chat is paused" and rechecks
slowly. A mocked test pins down that a suspended account makes no further
sync requests until its recheck, and a live test that a curfew on Palpo
shows as `Suspended` and a retry after it is lifted brings the sync back.

### One process per profile

The profile lock (ADR 0003) is held for the life of the process, and the
`AccountManager` owns the open profile, so one process writes an
account's crypto store. The SDK's cross-process lock stays at its
default; the window and the terminal UI never open one profile at once.
Forwarding a second launch's link to the running instance is ADR 0013
(Phase 9).

### Logs

The session logs through `tracing` like the rest of the app: Family
Chat's crates at `info`, matrix-sdk at `warn` (ADR 0003). It logs account
ids and states, never message text, room names, tokens or passwords. A
live test captures every log line of a sign-in, a restore and a sync
through the app's own logging pipeline and fails on a message body, a
token or a password.

## Consequences

- The frontends see the SDK only as view models and channels; an SDK
  upgrade changes `familychat-session` and nothing else.
- Every list a frontend shows is its own `Vec`, kept in step by diffs, so
  drawing never waits for the session.
- A suspended account and a dead server look different to the user, and
  neither floods the homeserver.
- An account opens offline from its stores, and syncs once the server
  answers.
- Phase 4 adds sign-in on top of `AccountManager::add_account` and its
  `PostLoginCheck`; Phase 5 and Phase 10 draw `SessionApi`; Phase 7's
  send queue and Phase 8's encryption flows join the same bridges.
