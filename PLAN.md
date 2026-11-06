# Family Chat Desktop Plan

Status: accepted as the working plan, November 4, 2026; Phase 0 (the repository scaffold, issue #1) is in place. Each phase is a GitHub issue (phase N is issue #N+1), tracked in issue #16.

Family Chat Desktop (`familychat-desktop`) is a native desktop chat client for macOS, Linux and Windows, written in Rust, with a graphical UI (GPUI) and a terminal UI (ratatui). It is a standards-compliant Matrix client that works with any homeserver that supports what modern Matrix clients need, but its first job is to be the best way to use **Family Chat**, the family-safe hosted Matrix service at `safechat.family`, including its easy sign-in with a parent's code.

It is built in the same style as [zmail](https://github.com/unicornops/zmail): the same workspace shape, toolchain, licence, lint and dependency policy, CI and release flow, ADR habit and agent instructions. Section 11 lists exactly what is copied.

The app is alpha software until stated otherwise: every phase ships in normal releases from `main`, with no preview builds.

## 1. Goals

- A fast, native, accessible chat client for macOS, Linux and Windows, with a GPUI desktop UI and a terminal UI that share all non-visual logic.
- Matrix done properly through the matrix-rust-sdk: end-to-end encryption by default, cross-signing, device verification, key backup and recovery, authenticated media, read receipts, typing, reactions, replies, edits and redactions.
- Family Chat sign-in that is easy and safe:
  - a sign-in code from a parent (the control panel's link or QR), redeemed with `m.login.token`;
  - password sign-in, with the server found from the family's address, including families on their own domain;
  - the rules every Family Chat client follows (docs/client-login-links.md in unicornops/family-chat), so the desktop is no weaker than the iOS, Android and web apps.
- Any other Matrix server through the standard flows: password, and later SSO, OAuth 2.0 (MSC3861) and QR login (MSC4108).
- Family Chat's hard privacy rule: user message content is never sent to a third-party hosted service. The app talks only to the account's homeserver.
- Secure by default: secrets only in the OS keychain, every local store encrypted at rest, strict TLS, no remote content beyond the homeserver, sanitised rendering, terminal escape filtering.

## 2. Non-goals

- Voice and video calls (Element Call). Undecided on the platform side (family-chat #258).
- A web view or embedded browser of any kind. Message HTML is sanitised and drawn natively.
- Push gateways. The desktop app syncs while it runs and raises local notifications.
- Analytics, crash reporting services, rageshake bug reports and remote logging.
- Client-side link previews, third-party GIF, sticker, map tile or emoji services.
- Running the GUI and the TUI on the same profile at the same time (section 5.5).
- Supporting homeservers without native sliding sync (MSC4186), as Element X does (Open decision 6).
- A Mac App Store, Microsoft Store, Flatpak or Snap build, and an auto-updater, during alpha.
- Languages other than English (British) at first. Wording lives in one place so it can be translated later.
- Copying code from the Element forks (AGPL-3.0). Behaviour is reimplemented from the Matrix spec and Family Chat's contract documents.

## 3. Product principles

- Never block the UI thread on network, disk, crypto or decoding.
- The UI reads view models built from local state; it never talks to a server itself.
- Both frontends get the same behaviour from the same code: anything that is not drawing lives in a GPUI-free, ratatui-free crate.
- No silent trust decisions: a sign-in code is always confirmed, an external link always shows its real address and asks, a server outside Family Chat's allowlist is refused before any credential is sent.
- Pin upstream inputs tightly (GPUI to a Zed stable tag, matrix-sdk to an exact minor) and move them deliberately.
- Write it down: each significant decision is an ADR in `docs/adr/`.

## 4. Decisions carried over and version choices

Taken from zmail unless Rob decides otherwise (section 15):

| Topic | Choice |
| --- | --- |
| Licence | `GPL-3.0-only` (LICENSE is the GPL 3 text); every dependency must be compatible |
| Rust | edition 2024, `resolver = "3"`, toolchain pinned to the one Zed pins for the GPUI tag (`1.98.1` today) |
| Lints | `missing_docs = "warn"`, `unsafe_code = "forbid"`, clippy `dbg_macro`, `todo`, `unwrap_used` denied |
| GPUI | git dependency on `zed-industries/zed` at a **stable release tag** for `gpui` and `gpui_platform`, never crates.io `gpui 0.2.2`; mirrored `[patch.crates-io]`; weekly bump workflow |
| Policy | `cargo deny` (licences, advisories, bans, sources), `cargo audit`, `cargo about` |
| Commits | Conventional Commits only |
| Releases | release-please (cargo-workspace plugin), tags `vX.Y.Z`, GitHub Releases |
| Packaging | archives plus native installers: `.deb`, DMG, MSI |
| Accessibility | GPUI started with `gpui_platform::application()` (AccessKit on) |

Version decisions to confirm in Phase 0 (checked November 4, 2026):

| Input | Version | Note |
| --- | --- | --- |
| Zed (GPUI) | `v1.27.0` (stable, released November 4, 2026) | toolchain `1.98.1`. Its `[patch.crates-io]` now takes `async-task` from `zed-industries/async-task` (zmail's tag used smol-rs) and adds `khronos-egl`; mirror the ones GPUI's runtime uses. zmail's deviation to Zed PR #62379 was for its web renderer and does **not** apply here. |
| matrix-sdk, matrix-sdk-ui | `0.19.1` (September 2026), MSRV 1.96 | breaking changes most releases: pin `=0.19.x`, move all matrix crates together |
| reqwest | `0.13` | what matrix-sdk 0.19 uses (zmail is on 0.12) |
| rusqlite | `0.40` line, only if used directly | `libsqlite3-sys` links SQLite once per build, so any direct use must match `matrix-sdk-sqlite` (zmail's 0.41 and SQLCipher do not fit) |
| TLS | rustls with aws-lc-rs (matrix-sdk's `rustls-aws-lc-rs`) | same provider family zmail already accepts in `deny.toml` |
| ratatui, crossterm, ratatui-textarea | `0.30.2`, `0.29`, `0.9.4` | as zmail |
| keyring | `3.6.3` | as zmail; `4.2` exists, evaluate for both apps together |
| ammonia | `4.2.4` | Matrix HTML sanitiser |
| rqrr | `0.11` | decoding a pasted or dropped QR image, pure Rust |
| interprocess | `2.4` | local socket for single-instance hand-off |

## 5. Architecture

### 5.1 Workspace layout

| Crate | Responsibility | GPUI | ratatui | matrix-sdk |
| --- | --- | --- | --- | --- |
| `crates/familychat-app` | Binary `familychat`. CLI (`familychat`, `familychat tui`, `familychat open-link`, `launch-ui-fixture`, `launch-tui-fixture`, account and diagnostics commands), wiring, single-instance hand-off, crash log, `build.rs` (Zed pin, Windows version resource), packaging metadata (`.deb`, WiX, Info.plist, `.desktop`). | yes | yes (feature `tui`, on by default) | via session |
| `crates/familychat-ui` | GPUI frontend: window, components (from zmail-ui), sign-in screens, room list, timeline, composer, verification and recovery, settings, keymaps, command palette, notifications posting. | yes | no | **no** |
| `crates/familychat-tui` | Terminal frontend (ADR 0008). | no | yes | **no** |
| `crates/familychat-core` | Domain types and view models both frontends draw: account, room summary, timeline item, member, sync state, errors and their wording; the rich-text AST; the commands table (ids, names, default keys for GUI and TUI); fuzzy matcher; the `SessionApi` trait the frontends consume, with live and fixture implementations. Depends on `ruma` identifiers only, no network. | no | no | no |
| `crates/familychat-session` | The zmail-sync equivalent. `BackgroundRuntime` (a dedicated tokio runtime thread, copied from zmail). Account manager: builds one `matrix_sdk::Client` per account (SQLite stores with a passphrase, our own reqwest client, `respect_login_well_known(false)`, refresh tokens persisted), restores sessions, runs `matrix_sdk_ui::sync_service::SyncService`, exposes the room list, timelines, members, media, encryption flows and notifications as runtime-agnostic channels of `familychat-core` types. Implements `SessionApi`. | no | no | yes |
| `crates/familychat-signin` | The zmail-setup equivalent: every sign-in flow as a state machine both frontends drive. Family Chat link parsing, strict server-name parsing, discovery, the allowlist, code redemption, password, other servers, post-login checks; OAuth, SSO and QR later. | no | no | yes |
| `crates/familychat-actions` | The zmail-actions equivalent: user actions (send, reply, edit, react, redact, read markers, typing, attachments, create DM or room, invite, join, leave), each reporting events to a plain callback; slash commands. | no | no | via session |
| `crates/familychat-security` | Keychain access and typed secrets (`Secret<T>` with an empty `Debug`, zeroised); log redaction; the Matrix HTML sanitiser and its hostile corpus; the terminal sanitiser and corpus (copied from zmail); safe file names for downloads (from zmail's `attachment_safety`); image decoding limits; link display and confirmation wording. | no | no | no |
| `crates/familychat-store` | App-level local data: data directory layout, the profile lock, the accounts registry, preferences and UI state in SQLite, with identifying values encrypted (ADR 0003). | no | no | no (`matrix-sdk-store-encryption` only) |
| `crates/familychat-notify` | Turns SDK notifications into OS notification payloads: sender, room, body or "New message", flood cap, per-room coalescing, the room a click opens. Same shape as zmail-notify. | no | no | no |
| `crates/familychat-theme` | Zed theme compatibility layer, copied from zmail-theme, plus the bundled Family Chat light and dark themes. | no | no | no |
| `crates/familychat-assets` | Embedded Lucide icons, Inter (Linux), app icon. Copied from zmail-assets. | no | no | no |
| `crates/familychat-test-support` | Fixture data and `FixtureSession` for screenshots, view tests and TUI snapshots; the live homeserver harness driver (`HomeserverHarness`); test-only HTTP clients trusting the harness CA. | no | no | yes |

Rules enforced in CI (a small script over `cargo metadata`):

- `familychat-ui` and `familychat-tui` never list `matrix-sdk`, `matrix-sdk-ui` or `gpui`/`ratatui` of the other frontend in their manifests.
- `familychat-core`'s public API exposes no matrix-sdk types.
- `unsafe_code = "forbid"` everywhere. If the Windows named-pipe security descriptor needs it, it lives in one module with each block justified, as zmail's renderer FFI does.

### 5.2 Async runtime and GPUI/tokio interop

GPUI has its own executors and no tokio reactor; matrix-sdk (reqwest, hyper, its stores) needs tokio. zmail already solved this, and the same pattern is reused (ADR 0005):

- `BackgroundRuntime::start(n)` starts a multi-thread tokio runtime on a dedicated thread and probes it, exactly as `crates/zmail-sync/src/lib.rs` does.
- Every SDK future runs on that runtime (`handle.spawn`). The UI never `.await`s an SDK future on a GPUI executor.
- Results come back through runtime-agnostic channels (`tokio::sync::oneshot`/`mpsc` receivers or `futures` channels), which GPUI tasks await in `cx.spawn` and then `update` their entity, as zmail's views do.
- matrix-sdk-ui's `eyeball` streams (room list entries, timeline items) are polled inside tokio tasks; their `VectorDiff`s are mapped to `familychat-core` items and sent over a channel. The UI applies diffs to its own model and coalesces bursts to one notify per frame.
- The TUI's main thread runs the draw and input loop (crossterm), receives the same channels with `try_recv` on each tick, and never blocks on the network.

### 5.3 How the GUI and TUI share logic

- **Session and view models:** both frontends consume `SessionApi` from `familychat-core`. The live implementation is `familychat-session`; the fixture implementation in `familychat-test-support` serves canned rooms and timelines for screenshots, view tests and TUI snapshots.
- **Flows:** sign-in, verification, recovery and room actions are state machines in `familychat-signin`, `familychat-session` and `familychat-actions`. Frontends only draw the state and send the user's answer, so wording and rules can't diverge.
- **Commands and keys:** `familychat-core::commands` is the single table of commands (id, name, description, default GUI keys, TUI key). The GUI defines a GPUI action per command and builds its command palette and cheat sheet from the table. The TUI builds its command bar and `:` palette from it. A test fails when a command lacks either binding. GUI keymaps are JSON (`keymaps/default.json`, `macos.json`) as in zmail.
- **Wording:** user-facing sentences for errors, refusals and confirmations live next to the logic that produces them, so both frontends say the same thing ("This server isn't a Family Chat server").

### 5.4 Data, store and secrets

Layout under the data directory (macOS `~/Library/Application Support/Family Chat`, Linux `$XDG_DATA_HOME/familychat`, Windows `%APPDATA%\Family Chat`; `FAMILYCHAT_DATA_DIR` overrides it, as `ZMAIL_DATA_DIR` does):

```
familychat.lock                  profile lock (exclusive)
familychat.sqlite3               app store: accounts registry, preferences, UI state
accounts/<account-uuid>/sdk/     matrix-sdk state and crypto stores
accounts/<account-uuid>/cache/   matrix-sdk event cache and media stores (ADR 0005)
logs/                            redacted logs, crash.log
```

- **Secrets** live only in the OS keychain (macOS login keychain, Windows Credential Manager, Secret Service on Linux) under one service id (Open decision 2): `app-store` (the app store's passphrase), `account-<uuid>-session` (user id, device id, access token, refresh token, homeserver URL, auth kind and, later, the OAuth client registration), `account-<uuid>-store` (the passphrase for that account's SDK stores). Every passphrase is 32 random bytes from the OS CSPRNG.
- **SDK stores** are opened with `sqlite_store_with_cache_path(.., Some(passphrase))`: matrix-sdk-sqlite encrypts every value with its `StoreCipher` (ChaCha20-Poly1305, keys hashed with HMAC), so messages, keys and media never sit on disk in plaintext.
- **The app store** uses the same `StoreCipher` construction for identifying values (user ids, homeserver URLs, room ids). Only non-identifying preferences (theme, text size) are plaintext.
- **What remains visible** on disk: table structure, row counts and value sizes. ADR 0003 records this residual leak and why SQLCipher is not used (one `libsqlite3-sys` per build means the SDK's databases and ours would share one SQLite library; keying only ours buys little and complicates the build).
- **No keychain, or a locked one:** the app shows the reason with Retry, never falls back to plaintext. A missing key (reinstalled OS, copied data directory) leads to a recovery screen that moves the old data aside and signs in again (zmail ADR 0005 behaviour). A passphrase-wrapped key for headless Linux is later work.
- **Refresh tokens:** `handle_refresh_tokens()` on; the client's save-session callback writes rotated tokens to the keychain inside the SDK's refresh, before the request that needed them is retried, and `subscribe_to_session_changes()` reports a token the server no longer accepts (ADR 0005).
- **Logs:** `tracing` to a local file only, rotated; `matrix_sdk` at `warn` by default; no message bodies, tokens, query strings or keys ever (a `Redacted` wrapper and a test that scans captured logs during sign-in and sync).
- **Sign-out:** server logout, then keychain entries deleted, then the account's directories removed.

### 5.5 One process per profile, single instance and links

matrix-sdk's crypto store must not be written by two unsynchronised processes, so only one process opens a profile at a time (unlike zmail, whose GUI and TUI share a store and coordinate with a sync lock).

- `familychat.lock` is taken exclusively at start. A second **GUI** launch forwards its arguments (a `familychat://` or `https://safechat.family/app/login` link, `--open-room`) to the running instance over a local socket and exits. A second **TUI** says Family Chat is already open and exits. Separate profiles (`--profile`) are allowed.
- The socket is user-only: a Unix socket inside a 0700 per-user directory (`$XDG_RUNTIME_DIR`, the macOS per-user temporary directory), a Windows named pipe whose DACL grants only the current user. Messages are length-prefixed JSON; the receiver runs the same strict parser as any other link, and nothing is logged.
- On macOS a link reaches the running app as an Apple Event, through GPUI's `on_open_urls`.
- `familychat open-link` reads a link from standard input, so a token never has to sit in shell history. OS URL handlers on Linux and Windows pass the link in `argv`; the process forwards it at once and exits (section 12, risk).

### 5.6 Sync

- `matrix_sdk_ui::sync_service::SyncService` with native sliding sync (MSC4186), discovered through `sliding_sync_version_builder`. Palpo implements it (`crates/server/src/routing/client/sync_msc4186.rs` in palpo), and the mobile forks already use it (`slidingSyncVersion: NATIVE`).
- A server without it gets a clear message instead of a broken client (Open decision 6).
- Room list: `RoomListService` with dynamic paging and filters (all, unread, people, invites). Timelines: `matrix_sdk_ui::Timeline` per open room, with back-pagination and the SDK send queue (local echo, offline retry).
- Sync state (running, offline, error) is shown in the window and the TUI status line; reconnects back off.

### 5.7 End-to-end encryption

All crypto is the matrix-rust-sdk's (vodozemac). ADR 0009 records the defaults:

- New private rooms and DMs are encrypted, unless the server's `.well-known/matrix/client` sets `io.element.e2ee.default: false` (how a server can say "not here").
- After sign-in, "Confirm it's you": verify with another device (emoji SAS; the desktop can show a verification QR for a phone to scan) or enter the recovery key; skipping is allowed with a warning.
- Cross-signing is bootstrapped when the account has none. A password sign-in answers user-interactive auth with the password. A code sign-in has no password and relies on the server accepting the first cross-signing upload without UIA (MSC3967); this is family-chat #257 and is tested on the harness (Phase 8).
- Recovery: key backup and secret storage through `encryption().recovery()`; the recovery key is shown once with copy and save.
- Device list with verify and sign out; incoming verification requests; identity change and pinning-violation banners; unable-to-decrypt reasons from matrix-sdk-ui's hook.
- Room key export and import (a passphrase-protected file), which Family Chat's custom-domain migration needs to keep encrypted history (family-chat BYOD design).
- Room key sharing and decryption trust settings follow Element X's defaults at the pinned SDK version.

### 5.8 Message rendering and media

No web view. ADR 0007 covers:

- **Formatted bodies:** `formatted_body` HTML goes through `familychat-security::matrix_html` (ammonia with the Matrix spec's tag and attribute allowlist, `mx-reply` stripped, depth and size caps, link schemes limited to `https`, `http`, `mailto`, `matrix`, images only as `mxc://`). It then becomes a `familychat-core` rich-text AST that the GUI draws as styled text and the TUI as spans.
- **Links:** `matrix:` and `matrix.to` links open inside the app. Any other link shows its real address and asks before handing it to the OS (as zmail does). A link whose text looks like a different address is flagged.
- **Media:** fetched only through the account's homeserver with authenticated media (Matrix v1.11), cached in the encrypted SDK media store. Images are decoded off the UI thread with caps on bytes, dimensions and frames. Remote SVG is never rendered. Video and audio are files (save, or open after confirmation through a temporary copy that is cleaned up, as zmail does).
- **Uploads:** checked against `/_matrix/client/v1/media/config` before sending (Family Chat's Palpo allows 11 MiB); location and camera metadata stripped from images by default.
- **Composer:** Markdown through the SDK's `markdown` feature; mentions autocomplete from the member list.

### 5.9 Notifications

- `client.register_notification_handler` delivers push-rule-evaluated, decrypted notifications during sync. `familychat-notify` composes a payload (sender and room, body truncated, or "New message" when the user hides content), coalesces per room, caps the rate (zmail's flood cap), and names the room a click opens.
- The GUI posts it with GPUI's system notifications (`cx.show_system_notification`, `on_system_notification_response`), as `crates/zmail-ui/src/notifications.rs` does: UNUserNotificationCenter on macOS, toasts on Windows, the freedesktop service on Linux. The TUI shows it in the status line, with an optional bell.
- No notification for the focused room; room notification modes and keywords come from the account's push rules (`client.notification_settings()`).
- No push gateway and no pusher: notifications arrive while the app runs. Running in the background with a tray icon is later work.

### 5.10 Theming, assets and accessibility

- Zed theme JSON compatibility, copied from zmail-theme; bundled Family Chat light and dark themes derived from the website's brand; the theme follows the OS appearance until the user picks one.
- zmail's accessibility baseline (docs/phase5-hardening.md in zmail): AccessKit names and roles for every control, keyboard reachability, focus rings, 24 px minimum targets, text size 100% to 200%, reduced motion, Windows contrast themes, macOS Increase Contrast.
- Lucide icons and Inter (Linux) from zmail's assets, with their licences shipped.

## 6. Family Chat integration

### 6.1 What the platform looks like

From unicornops/family-chat at `b0001ac` and the palpo fork:

- Each family has its own homeserver. New families run **Palpo** (`HOMESERVER_DEFAULT_TYPE=palpo`, image `ghcr.io/unicornops/palpo:sha-b188d47` in `infra/kubernetes/platform/configmap.yaml`); the last Continuwuity family moved to Palpo (#237) and Continuwuity is being retired. The type registry is `control-panel/apps/provisioning/homeserver_types.py`.
- **Server name versus serving host.** A family's Matrix `server_name` is either `<name>.safechat.family` or the family's own domain (bring your own domain). Traffic is always served from a generated `<name>.safechat.family` host (`serving_hostname`), and a custom domain only publishes `.well-known/matrix/client` and `/server` delegating to it (`control-panel/apps/provisioning/domain_verification.py`).
- The edge proxy in each family pod (`render_nginx_edge_config` in `control-panel/apps/provisioning/homeserver_config.py`) exposes only `/.well-known/matrix/*`, `/_matrix/client/`, `/_matrix/media/` (11 MiB body limit), `/_matrix/key/` and allowlisted `/_matrix/federation/`. No `/_synapse/client/` paths are exposed for Palpo.
- Staging uses `*.familychat.dev`; the control panel is `https://panel.safechat.family`; the website is `https://safechat.family`.

### 6.2 Sign-in methods the desktop offers

The welcome screen leads with Family Chat and keeps other servers one step away:

1. **"Sign in with a code from your parent"**: paste the sign-in link, paste or drop the QR image (decoded locally, never uploaded), or arrive through a `familychat://` link. Redeemed with `m.login.token`.
2. **"Sign in with your password"**: the family's address (`smith.safechat.family`, `smith.ie`) or a full Matrix ID, then the password.
3. **"Use another Matrix server"**: any homeserver: the server first, then what it offers (a password, OAuth or SSO in the browser, a QR code; Phase 14, ADR 0010).

Device display name: `Family Chat (macOS)`, `Family Chat (Windows)` or `Family Chat (Linux)`, after the contract's `Family Chat (iPhone)`.

### 6.3 The sign-in link contract

Source of truth: `docs/client-login-links.md` (family-chat), built by `control-panel/apps/accounts/qr_login.py` (`build_app_login_url`), minted by `control-panel/apps/accounts/login_tokens.py` (`mint_login_token`).

```
https://safechat.family/app/login?account_provider=<server_name>&login_hint=mxid:<mxid>[&hs=<serving_hostname>&token=<login_token>]
```

| Parameter | Meaning |
| --- | --- |
| `account_provider` | always; the family's Matrix server name, resolved through `.well-known/matrix/client` |
| `login_hint` | always; `mxid:` plus the full Matrix ID; parse as a query string, never split on `:` |
| `hs` | only with `token`; the bare `hostname[:port]` that answers the client API (differs from `account_provider` for custom-domain families) |
| `token` | Palpo families only, parent-minted, single use, about 5 minutes (`LOGIN_TOKEN_TTL_MS`); minted with `POST /_synapse/admin/v1/users/{mxid}/login_token` (palpo-im/palpo#444) |

The desktop also accepts the same query on `familychat://safechat.family/app/login?...` (mirroring the iOS app's `family.safechat.app://safechat.family/app/login?...` form, so one parser serves both), and the same text pasted with surrounding whitespace. The web form (`https://app.safechat.family/?loginToken=..&hs=..`) is not needed.

### 6.4 Redeeming a code (every rule from the contract)

1. Parse `account_provider`, `login_hint`, `hs`, `token`. Keep `hs` and `token` only together, and only when `hs` is a bare `hostname[:port]` under the allowlist; otherwise drop the token unsent and degrade to the hint form. Drop a `login_hint` whose server differs from `account_provider`.
2. If that account is already signed in, say so and do not redeem; never keep the link to replay later.
3. Always ask first: **"Sign in as `<mxid>`?"** (or name the server when there is no hint), Continue or Cancel.
4. On Continue, `POST https://<hs>/_matrix/client/v3/login` with `{"type": "m.login.token", "token": ..., "initial_device_display_name": ...}` through a dedicated reqwest client with **redirects off and no retries**, one redemption in flight. The SDK is not used for this request, so its general HTTP behaviour can't follow a redirect with the token.
5. Check the returned `user_id` equals the hint, or with no hint that its server equals `account_provider`. Otherwise log the new device out and refuse.
6. A 401 or 403 means "That code has been used or has expired. Ask your parent for a new one."; then fall through to password sign-in. Any failure after credentials were issued logs that device out (best effort).
7. Hand the credentials to matrix-sdk with `restore_session`, then run the post-login check (6.6).
8. The token is never logged, persisted, put in `Debug` output or panics, or kept after use.

### 6.5 Password sign-in, discovery and the allowlist

- A server name, typed or from a Matrix ID or `account_provider`, is parsed strictly as a bare host: refuse `\`, `@`, `%`, `/`, `?`, `#`, whitespace and any scheme other than an optional leading `https://`. Regression cases from the fork reviews are part of the tests (the iOS backslash bypass, the Android apex fallback and redirect findings).
- Resolve it through `.well-known/matrix/client`. For a Family Chat account, accept the result only if the homeserver URL is `https://` and its host is under `*.safechat.family` (subdomains only, never the apex). This is what lets custom-domain families in. Only unauthenticated discovery requests (`well-known`, `/versions`) go out before this check passes.
- Password sign-in for a code link pre-fills the server from **`account_provider`**, never `hs`, even when discovery fails: a crafted link must not collect a password.
- Palpo throttles logins per TCP peer, and behind the edge proxy that is one bucket per family (`[rc_login] per_second = 0.2, burst = 20`). The client never retries a login on its own and shows `M_LIMIT_EXCEEDED` with its wait time.
- Refusal wording: "This server isn't a Family Chat server".
- Environments: production allowlist `*.safechat.family`; a `staging` Cargo feature (never in releases) adds `*.familychat.dev`; the `test-harness` feature adds `*.familychat.test`.

### 6.6 After every sign-in

matrix-sdk can re-point a client from a login response's `well_known`; the desktop builds clients with `respect_login_well_known(false)` and still checks the client's homeserver against the allowlist after every Family Chat sign-in (password, code, OAuth or QR). On a mismatch it logs the new device out and refuses.

### 6.7 How a desktop user gets a code

The panel shows a token-bearing link only as a QR image (never the raw URL), and the website's fallback page (`website/themes/familychat/layouts/page/app-login.html`) offers app buttons only to Android and iOS. A desktop usually has no camera. The plan:

- In this repo: paste the link, or paste or drop the QR image (an image copied from the panel page or a screenshot), decoded locally with `rqrr`. Webcam scanning is later work.
- `familychat://` handling (Phase 9) with OS registration (Phase 12).
- Cross-repo (family-chat issues to file, Open decision 4): the fallback page offers "Open in Family Chat for desktop" with the `familychat://` link on desktop browsers; optionally the panel offers "Sign in on this computer".
- The Electron desktop build in familychat-web already registers `familychat://` (`apps/desktop/familychat/build.json`); the two must not both be installed (Open decision 3).

### 6.8 Supervision, curfews and the parental gate on desktop

- **Supervision** is server-side and transparent (`docs/monitoring-bot-architecture.md`): a visible "Family Safety Monitor" bot joins rooms at levels above `off`; the content levels (`alerts`, `full`) are meant to turn E2EE off, but that enforcement is still a placeholder. The client therefore honours a server's `io.element.e2ee.default: false`, shows a clear banner in unencrypted rooms, and never hides members. A dedicated supervision indicator needs a platform contract first (later work).
- **Curfews** suspend the account (`docs/monitoring-time-restrictions.md`): the server rejects sends with `M_USER_SUSPENDED`. The client shows "Chat is paused" with a read-only composer while reads keep working, and checks again on the next sync.
- **Parental gate:** decision 10 of family-chat #232 applies it to iOS and Android only (store rules for a child audience). The desktop has no maths gate (Open decision 7) but every external link asks first with the full address, and there are no other exits: no browser views, no "open in browser" from files, no store links.

### 6.9 Other touch points

- **Account management:** for Family Chat accounts, "Forgot password" and password changes explain that a parent resets passwords in the control panel (`https://panel.safechat.family`) or can give a new sign-in code; account deactivation is not offered in-app.
- **GIFs:** later, through the platform's proxy (family-chat #238) once it exists; the client never contacts the GIF provider.
- **Push:** not needed on desktop; no Sygnal registration.

### 6.10 Homeserver capability gaps that shape the design

| Capability | Palpo (Family Chat pin) | Effect |
| --- | --- | --- |
| Native sliding sync (MSC4186) | yes | SyncService works |
| Spec versions | up to v1.12, authenticated media | fine |
| `m.login.token` | accepted and advertised (`session.rs`) | code sign-in works |
| OAuth 2.0 / MAS (MSC3861) | not configured (family-chat #239 plans MAS per family) | no OAuth sign-in for Family Chat yet; the desktop offers it where a server has it, with the issuer under the allowlist for Family Chat (ADR 0010) |
| QR login rendezvous (MSC4108 / MSC4388) | discovery answers `false` (`unstable.rs`) | QR login only on other servers until #239 |
| First cross-signing upload without UIA (MSC3967) | yes, verified by the Phase 2 harness (family-chat #257) | code-only accounts get cross-signing; replacing the keys asks for a password |
| Login rate limit | one bucket per family behind the proxy | never retry logins |
| Upload size | 11 MiB | check media config first |
| Suspension | `M_USER_SUSPENDED` | read-only state |

### 6.11 Source references

- family-chat: `docs/client-login-links.md`, `docs/app-links.md`, `docs/monitoring-bot-architecture.md`, `docs/monitoring-time-restrictions.md`, `docs/staging-environment.md`, `control-panel/apps/accounts/qr_login.py`, `control-panel/apps/accounts/login_tokens.py`, `control-panel/apps/provisioning/homeserver_config.py`, `control-panel/apps/provisioning/homeserver_types.py`, `control-panel/apps/provisioning/domain_verification.py`, `website/themes/familychat/layouts/page/app-login.html`, issues #232, #235, #236, #237, #238, #239, #254, #256, #257.
- familychat-android: `libraries/matrix/impl/src/main/kotlin/io/element/android/libraries/matrix/impl/auth/LoginTokenExchanger.kt` (redirects and retries off, redacted `toString`), `libraries/parentalgate/`.
- familychat-web: `apps/web/src/utils/LoginLink.ts`, `apps/desktop/src/protocol.ts` (query redaction in logs), `apps/desktop/familychat/build.json`.
- familychat-ios: `ElementX/Sources/Application/Settings/AppSettings.swift` (`accountProviders = ["*.safechat.family"]`, Family Chat URLs).
- palpo (unicornops fork): `crates/server/src/routing/client/session.rs`, `unstable.rs`, `sync_msc4186.rs`, `crates/server/src/routing/client.rs` (versions).

## 7. Security model

Threats, in order: a crafted link or QR that phishes a password or signs a child into someone else's account; a malicious homeserver or message (HTML, images, escape sequences, file names); a stolen or copied disk; another local user; supply chain.

- **Sign-in:** the rules of 6.4 to 6.6; nothing sent before the allowlist check; confirm before redeem; user id binding; no redirects; no password to `hs`; post-login re-check.
- **Transport:** rustls, system roots, no insecure option in release builds; test CAs only behind the `test-harness` feature, checked in CI.
- **Local data:** section 5.4; secrets only in the keychain; every store encrypted; no plaintext fallback.
- **Content:** sanitised HTML, no web view, no remote fetches except the homeserver, image decode limits, safe file names, no auto-open of files.
- **Terminal:** every server-originated string through `terminal::sanitize` (controls, ESC, C1, bidi, zero-width), OSC 52 only for the user's own copy (zmail ADR 0007).
- **Local IPC:** user-only socket or pipe, strict parsing of what arrives.
- **Logs and crashes:** redacted, local only.
- **Code:** `unsafe_code = "forbid"`; `cargo deny`, `cargo audit`, Dependabot; fuzz targets for the link parser and the sanitiser.

## 8. Privacy: no third-party content processing

User message content never leaves the device except to the account's own homeserver.

- No analytics, crash reporting, rageshake or remote logging.
- No client-side link previews; server-side previews off by default for Family Chat accounts and in encrypted rooms.
- No identity server, map tiles, GIF or sticker providers, emoji CDNs, Gravatar or font services.
- Media and avatars only through the homeserver (authenticated media proxies remote media).
- Notifications go to the local OS notification centre only, with an option to hide content.
- An **egress test** in CI runs the live harness tests through a recording proxy and fails on any host other than the harness homeservers (zmail's renderer canary does the same for HTML mail).

## 9. Testing strategy

- **Unit tests** in every crate: `cargo test --workspace --locked`.
- **Mocked Matrix:** matrix-sdk's `testing` feature (`MatrixMockServer`, wiremock) for session and sign-in flows without Docker.
- **Live harness** (`docker/homeserver-harness`, `scripts/homeserver-harness.sh up|down|env|mint-token`, ADR 0004):
  - Palpo pinned to the image Family Chat runs, with its Postgres sidecar (`postgres:18-alpine`), configured like `render_palpo_config`; admin bootstrap as `PalpoType.bootstrap_admin` does (token-gated registration, then `UPDATE users SET is_admin` in Postgres, then login), so tests mint real login tokens through the admin API and can suspend users.
  - A TLS front end with a throwaway CA made on every `up`, serving `hs1.familychat.test` (allowlisted), `smith.test` (a custom-domain well-known delegating to `hs1`), and `evil.familychat.example` (not allowlisted).
  - Synapse, pinned, for "any homeserver" coverage; Synapse with MAS added in Phase 14.
  - Test clients resolve the harness hostnames and trust its CA only through `familychat-test-support`'s `test-harness` feature.
  - Docker Hub credentials optional as in zmail (`DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`).
  - Live tests are `#[ignore]` and run in CI's quality job with `-- --ignored`.
- **Hostile corpora:** Matrix HTML (scripts, iframes, styles, `data:` URIs, deep nesting, huge tables, mismatched link text), terminal escapes (copied from zmail), image bombs, file names.
- **Property tests and fuzzing:** the link parser and server-name parser (proptest), plus a short fuzz smoke run in CI.
- **GUI view tests:** `#[gpui::test]` with `TestAppContext`/`VisualTestContext` over `FixtureSession`, headless; accessible-name and hit-target tests as in zmail.
- **Screenshot fixtures:** `familychat launch-ui-fixture VIEW [dark|light]` opens the window on a fixture state with no network; `scripts/capture-ui-screenshots.sh --matrix artifacts/ui-screenshots` (from zmail's `capture-release-screenshot.sh`) captures each state under Xvfb with openbox, xdotool, ImageMagick and lavapipe; CI uploads the `ui-screenshots` artefact on PRs that touch the UI.
- **TUI snapshots:** ratatui `TestBackend` at 80x24 and 160x48 in `src/snapshots`, rewritten with `FAMILYCHAT_UPDATE_SNAPSHOTS=1`; `familychat launch-tui-fixture [VIEW]`.
- **Per-OS:** store, security, signin, notify and TUI tests also run on macOS and Windows in the build job.
- **Manual checks** written down per phase: a real terminal on each OS, screen readers, a real code sign-in against Family Chat staging (`familychat.dev`).

## 10. CI, release and distribution

- **`ci.yml`** (zmail's shape):
  - `changes` (path filters);
  - `quality` on Linux: fmt, clippy `-D warnings`, `cargo deny`, `cargo audit`, workspace tests, live harness tests, the egress test, the dependency-boundary script;
  - `build` on ubuntu, macOS and Windows with per-OS tests;
  - `ui_screenshots` on UI changes.
- **`release-please.yml`:** release PRs from Conventional Commits; on release, archives (`familychat-linux-x86_64.tar.gz`, `familychat-macos.tar.gz`, `familychat-windows-x86_64.zip`) with LICENSE, `licenses/` and `THIRD_PARTY.html` (cargo-about); `.deb` (cargo-deb), MSI (cargo-wix), DMG (`scripts/create-dmg.sh`); a release screenshot; `SHA256SUMS` and build provenance attestations. Also `release-backfill.yml` and `scripts/render-release-pr.sh` from zmail.
- **Signing:**
  - macOS: Developer ID and notarisation with the org-wide `APPLE_*` secrets when present, otherwise ad hoc (as zmail).
  - Windows: Azure Artifact Signing when its secrets are present (family-chat #232 decision 6), otherwise unsigned with a release note.
  - Linux: unsigned during alpha. Nobody describes Windows or `.deb` signing as done until it is.
- **URL scheme registration:** Info.plist `CFBundleURLTypes` (macOS), `HKCU\Software\Classes\familychat` from the MSI (Windows), `MimeType=x-scheme-handler/familychat` in the `.desktop` file (Linux).
- **`zed-upstream.yml`:** weekly GPUI bump PR (`scripts/bump-zed-gpui.sh`).
- **`dependabot.yml`:** Cargo `fix(deps)` grouped minor and patch, Actions `ci(deps)`; GPUI ignored; matrix-sdk, matrix-sdk-ui, matrix-sdk-base and ruma grouped so they move together.
- **Distribution:** GitHub Releases during alpha (Open decision 9).

## 11. What to copy from zmail

| zmail file | Action |
| --- | --- |
| `Cargo.toml` | copy `[workspace.package]`, `[workspace.lints]`, the GPUI lines (stable tag) and the `[patch.crates-io]` comment and mechanism |
| `rust-toolchain.toml`, `.cargo/config.toml`, `.gitignore`, `CHANGELOG.md` header | copy |
| `LICENSE` | copy (GPL 3) |
| `deny.toml` | copy; drop the Proton, pgp, proton-srp, SQLCipher and `huacnlee/zed` entries; keep only the advisory ignores the GPUI graph still triggers; `allow-git` for exactly the git repositories the GPUI graph uses (Zed and its forks, plus the `gfx-rs/wgpu` revision `gpui_wgpu` pins) |
| `about.toml` | copy the accepted list and the habit of recording ported files and bundled assets |
| `AGENTS.md`, `CLAUDE.md` (symlink), `.github/copilot-instructions.md` | copy the structure and rules; rewrite the content |
| `.github/workflows/ci.yml` | copy `changes`, `quality`, `build`, UI screenshots; drop web renderer, Proton, NASM and OpenSSL steps |
| `.github/workflows/release-please.yml`, `release-backfill.yml`, `scripts/render-release-pr.sh`, `.github/release-please-config.json` | copy and rename; Azure signing instead of PFX |
| `.github/workflows/zed-upstream.yml`, `scripts/bump-zed-gpui.sh`, `docs/upstream/zed.md` | copy; the PR-pin logic can go |
| `.github/dependabot.yml` | copy; matrix group |
| `docs/adr/0003-gpui-zed-stable-pin.md` | becomes ADR 0002 |
| `docs/adr/0005-store-encryption.md`, `0007-terminal-ui.md` | patterns for ADR 0003 and ADR 0008 |
| `crates/zmail-sync/src/lib.rs` `BackgroundRuntime` | copy into `familychat-session` |
| `crates/zmail-theme`, `crates/zmail-assets`, `assets/fonts/inter`, `assets/icons` | copy with licences |
| `crates/zmail-ui/src/components/*`, `crates/zmail-ui/LICENSE-APACHE`, `keymaps/` | copy and adapt; keep file headers and `about.toml` notices |
| `crates/zmail-ui/src/notifications.rs`, `crates/zmail-notify` | pattern |
| `crates/zmail-security/src/terminal.rs`, `tests/terminal_corpus.rs`, `attachment_safety.rs`, keyring wrapper | copy |
| `crates/zmail-tui` (`clipboard.rs`, `terminal.rs`, `palette.rs`, `editor.rs`, snapshot tests) | copy or pattern |
| `crates/zmail-app/src/ui_fixture.rs`, `scripts/capture-release-screenshot.sh` | pattern for `FixtureSession` and `capture-ui-screenshots.sh` |
| `crates/zmail-app/build.rs`, `wix/main.wxs`, `packaging/debian`, `*.desktop`, `scripts/create-dmg.sh`, `scripts/generate-app-icon.sh` | copy and adapt |
| `scripts/mail-harness.sh` | pattern for `homeserver-harness.sh` (pre-pulls, retries, Docker Hub login) |
| `docs/phase5-hardening.md` | pattern for `docs/hardening.md` |

## 12. Risks

- **matrix-sdk churn:** breaking changes in most releases. Pin exactly, group updates, keep SDK types out of `familychat-core`, record the pin in `docs/upstream/matrix-sdk.md`.
- **GPUI churn:** pin to stable tags, weekly bump PR, fix breakage in the bump PR.
- **Palpo maturity:** sliding sync and crypto endpoints on a young server. The harness runs the exact Family Chat image; failures become palpo issues upstream.
- **Code-only accounts and cross-signing** (MSC3967, family-chat #257): if the server demands UIA, the UX must explain it, and the platform may need a change.
- **Login throttling** per family: a retry loop on one device could lock out the household.
- **Headless Linux without Secret Service:** the TUI over SSH can't unlock; a passphrase-wrapped key is later work.
- **One process per profile:** GUI and TUI can't run together on one profile.
- **Tokens in `argv`** from OS URL handlers on Linux and Windows are briefly visible to the same user's processes; forward and exit immediately, never log.
- **Binary size and build time** (GPUI with wgpu plus matrix-sdk): measure in Phase 0 and track.
- **AGPL code nearby:** the Element forks must not be copied into a GPL-3.0-only codebase.

## 13. Phases of work

Each phase is one GitHub issue and one or more PRs, merges on its own, and ships in a normal release.

### 13.0 Phase 0: Repository scaffold

Priority P0. Depends on nothing.

Goal: the workspace, toolchain, policy, CI and release flow in zmail's style, with an empty GPUI window and a placeholder TUI.

Scope:
- Workspace as section 4; crates `familychat-app` (binary `familychat`), `familychat-ui`, `familychat-tui`, `familychat-core`, `familychat-session` (only `BackgroundRuntime`), `familychat-theme`, `familychat-assets`.
- GPUI at the latest Zed stable tag with mirrored patches; app started with `gpui_platform::application()`.
- LICENSE, README (alpha warning, build and platform packages), AGENTS.md with CLAUDE.md symlink and copilot instructions, this PLAN.md, CHANGELOG.md, `.gitignore`, `.cargo/config.toml`.
- `deny.toml`, `about.toml`, Dependabot, `ci.yml`, `release-please.yml` (archives only), `release-backfill.yml`, `zed-upstream.yml` with the bump script.
- `docs/adr/0001-architecture.md`, `docs/adr/0002-gpui-zed-stable-pin.md`, `docs/upstream/zed.md`.

Acceptance criteria:
- fmt, clippy `-D warnings` and tests pass in CI; build job green on Linux, macOS and Windows.
- `familychat` opens an empty Family Chat window on each OS; `familychat tui` draws a placeholder and quits on `q`; `familychat --version` works.
- `cargo deny check licenses advisories bans sources` and `cargo audit` pass in CI.
- A test awaits, from a GPUI task, a future that ran on the tokio runtime.
- Merging to `main` opens a release PR; a release attaches the three archives with LICENSE and notices.
- The Zed bump workflow runs on dispatch and reports the pin is current.
- ADR 0001 and 0002 merged; AGENTS.md lists crates, commands and rules; release binary size recorded.

### 13.1 Phase 1: Secrets and the local store

Priority P0. Depends on Phase 0.

Goal: where everything lives, encrypted, with secrets only in the keychain and one process per profile.

Scope:
- `familychat-security`: keychain wrapper and typed entries, `Secret<T>`, redaction, log and crash-log setup.
- `familychat-store`: data directory and `FAMILYCHAT_DATA_DIR`, profile lock, app SQLite with migrations, identifying values encrypted with `StoreCipher`, accounts registry, preferences; keychain-locked, missing-key and start-over states.
- CLI `familychat accounts`.
- ADR 0003.

Acceptance criteria:
- A second process on the same profile gets "already open" (two-process test).
- A known Matrix ID written to the registry is not found in the database file's bytes.
- An unreachable or locked keychain gives an error state and never a plaintext store; a missing key gives the recovery state, and start over moves the old files aside.
- `Debug` of every secret type prints nothing; captured logs contain no secret.
- Store and security tests run on all three OS in CI.

### 13.2 Phase 2: Local homeserver harness

Priority P0. Depends on Phase 0.

Goal: real homeservers on a developer machine and in CI, matching what Family Chat runs.

Scope:
- `docker/homeserver-harness` and `scripts/homeserver-harness.sh` as section 9 (Palpo with Postgres, TLS front end with a throwaway CA, custom-domain well-known, a non-allowlisted host, Synapse).
- `familychat-test-support::HomeserverHarness`: users, rooms, sending as another user, minting login tokens, suspending users.
- Test-only HTTP client and CA trust behind `test-harness`.
- ADR 0004 and `docs/upstream/homeservers.md` (pins, and how to follow family-chat's `PALPO_IMAGE`).

Acceptance criteria:
- `up`, `env` and `down` work locally and in CI.
- Live test: a minted token redeems once with `m.login.token` (200) and the replay gets 403 `M_FORBIDDEN`.
- Live test: `smith.test` well-known resolves to `hs1.familychat.test`; Synapse answers `/versions`.
- Live test: suspending a user makes a send fail with `M_USER_SUSPENDED`.
- CI checks that a release build of `familychat-app` does not enable `test-harness`.

### 13.3 Phase 3: Matrix session core

Priority P0. Depends on Phases 1 and 2.

Goal: accounts that restore, sync and stream rooms and timelines to any frontend.

Scope:
- `familychat-session`: account manager, client construction (store passphrase, our reqwest client, `respect_login_well_known(false)`, refresh tokens persisted), SyncService, room list with filters, timelines with back-pagination, sync state, offline handling.
- `SessionApi` and `familychat-core` view models.
- The dependency-boundary script in CI.
- ADR 0005.

Acceptance criteria:
- Live: an account signed in by test support restores after a restart without signing in again; sync reaches running; seeded rooms appear; a message from another harness user reaches the timeline stream within 10 seconds; back-pagination loads older messages.
- Live (Synapse with refresh tokens): rotated tokens are persisted and survive a restart.
- Mocked: a server without native sliding sync gives the "not supported" state.
- SDK stores are passphrase-encrypted; no message text is found in their files.
- The boundary script passes: no matrix-sdk in the frontends' manifests or in `familychat-core`'s API.

### 13.4 Phase 4: Sign-in core

Priority P0. Depends on Phase 3.

Goal: every Family Chat and generic password sign-in rule, as GPUI-free state machines with tests.

Scope:
- `familychat-signin`: link parsing (https, `familychat://`, pasted text); strict host parsing; environments and allowlist; discovery; the code flow of 6.4; password for Family Chat and other servers; post-login check; device names; error wording.
- ADR 0006 with a table mapping each rule of the contract to its test.

Acceptance criteria:
- Every rule in "What each client must implement" has a passing test.
- Live: code sign-in works; the replay shows the used-or-expired message and offers password; a non-allowlisted `hs` means the token is never sent (checked in the harness log); a mismatched `user_id` is logged out and refused; `account_provider=smith.test` signs in.
- Password for a code link never goes to `hs`; a 307 on `/login` is not followed.
- Other-server password sign-in works on Synapse.
- Tokens never appear in logs, `Debug` or panic messages.

### 13.5 Phase 5: GUI shell, sign-in screens and screenshot fixtures

Priority P0. Depends on Phase 4.

Goal: a real window to sign in from, and the screenshot and view-test machinery for everything after it.

Scope:
- Components from zmail-ui; Family Chat themes; keymaps; command palette from the commands table.
- Welcome, code (paste link, paste or drop QR image), confirm, expired code, password, other server, keychain and recovery screens; sync state.
- `FixtureSession`, `launch-ui-fixture`, `scripts/capture-ui-screenshots.sh`, the CI UI Screenshots job; view tests.
- ADR 0011 (0007 to 0010 are reserved for the phases that name them).

Acceptance criteria:
- Sign-in to the harness with a pasted link, a pasted QR image and a password (manual on each OS and view tests).
- The confirm dialog always appears before a code is redeemed.
- The screenshot matrix covers every sign-in state in dark and light and is uploaded on UI PRs.
- Every control has an accessible name and a 24 px target (view tests).

### 13.6 Phase 6: Rooms and timeline in the GUI

Priority P0. Depends on Phases 3 and 5.

Goal: read everything: room list, timeline, rich text and media, safely.

Scope:
- Room list (filters, unread and mention badges, invites, DMs).
- Room header with encryption state; member list.
- Timeline: virtualised and bottom-aligned; text, notices, emotes, sanitised HTML, replies, edits, redactions, reactions, read marker, day separators, state summaries, unable-to-decrypt placeholders.
- Images through authenticated media with decode limits; files saved with safe names.
- In-app `matrix:` links and confirmation for other links.
- Fixture states; ADR 0007.

Acceptance criteria:
- The HTML hostile corpus and image-bomb tests pass; remote SVG is never drawn.
- A 1,000-room and 10,000-event fixture scrolls without blocking (benchmark recorded).
- Live: messages and images from another user appear.
- Every external link shows its full address and asks.
- Screenshot states: room list, timeline variants, empty states, breakpoints, dark and light.

### 13.7 Phase 7: Messaging actions and the composer

Priority P0. Depends on Phase 6.

Goal: take part in conversations from the GUI, through shared actions.

Scope:
- `familychat-actions`: send with Markdown, reply, edit, react, redact, read receipts, typing, attachments (size check, location metadata stripped), create DM or room (encryption default per 5.7), invite, accept or decline invites, leave; send queue; drafts.
- The "Chat is paused" state.
- GUI composer with mentions.

Acceptance criteria:
- Live: send, reply, edit, react and redact round-trip between two harness users.
- An over-limit file is refused before upload with a clear message.
- Offline sends go out on reconnect (harness stopped and started).
- A suspended user sees the paused banner and a read-only composer; unsuspending restores it.
- An image's location metadata is gone after upload.
- `familychat-actions` has no GPUI dependency; screenshot states added.

### 13.8 Phase 8: End-to-end encryption

Priority P0. Depends on Phases 6 and 7.

Goal: encryption people can live with: confirm identity, recover, verify, understand failures.

Scope:
- Confirm-it's-you after sign-in; cross-signing bootstrap including the code-login path; recovery setup and use.
- Emoji verification and verification QR display; incoming requests; device list.
- Identity-change banners; unable-to-decrypt reasons; key export and import.
- ADR 0009.

Acceptance criteria:
- Live: two devices verify by emoji and history decrypts.
- Live: recovery set up on one device lets a new device read history.
- Live: a code-only account bootstraps cross-signing on the harness Palpo, or the test records the UIA demand and the UX handles it; outcome reported on family-chat #257.
- Key export and import round-trips.
- Screenshot states added.

### 13.9 Phase 9: Notifications, single instance and sign-in links

Priority P0. Depends on Phases 4 and 6.

Goal: know when someone writes, and open a sign-in link straight into the running app.

Scope:
- `familychat-notify` and GPUI system notifications; click opens the room; hide-content setting; unread count in the title.
- Profile-lock hand-off over the user-only local socket or pipe; macOS `on_open_urls`; `familychat open-link` from standard input.
- File the family-chat issues for the fallback page's desktop button (and the panel option if Rob chooses it).

Acceptance criteria:
- Notifications on each OS for background rooms only; a click opens the room (manual list).
- Hidden content shows "New message"; flood cap tested.
- A second launch with a link shows the confirm dialog in the running window; the socket is user-only (permission test); no token in logs.
- The family-chat issue is filed and linked.

### 13.10 Phase 10: Terminal UI: sign-in and reading

Priority P1. Depends on Phases 4 and 6.

Goal: a terminal frontend that signs in and reads, in zmail's TUI style.

Scope:
- `familychat-tui`: title bar, body, status line and command bar; room list and room screens, side by side from 120 columns; `:` palette; mouse; Zellij-style copy; terminal sanitiser on every server string; numbered links with confirmation; images as named files with save; status-line notifications.
- Sign-in by pasted link (bracketed paste) or password.
- `launch-tui-fixture`; ADR 0008.

Acceptance criteria:
- Live: TUI sign-in by link and by password on the harness.
- Snapshots of every screen at 80x24 and 160x48, `NO_COLOR` and ASCII.
- The terminal hostile corpus passes, and a test draws hostile room names, display names and bodies.
- Manual check in a real terminal on each OS recorded.

### 13.11 Phase 11: Terminal UI: writing and encryption

Priority P1. Depends on Phases 7, 8 and 10.

Goal: full participation from the terminal.

Scope:
- Composer on ratatui-textarea; reply, edit, react and redact through `familychat-actions`; attachments by path with Tab completion; invites.
- Emoji verification shown as emoji and names; recovery key entry.
- The paused state; opt-in external editor (zmail's approach).

Acceptance criteria:
- Live: send and receive, reply and react from the TUI.
- Live: emoji verification against a test-support device.
- Snapshots added.

### 13.12 Phase 12: Packaging, signing and distribution

Priority P1. Depends on Phase 9.

Goal: installable, signed where credentials exist, with the `familychat://` scheme registered.

Scope:
- DMG with Info.plist and URL type, notarisation; MSI with scheme registration and Azure Artifact Signing; `.deb` and `.desktop`.
- `THIRD_PARTY.html`, `SHA256SUMS`, provenance attestations, release screenshot.
- `docs/release.md`; README install section.

Acceptance criteria:
- A release attaches DMG, MSI, `.deb` and archives with licences.
- A `familychat://` link in a browser opens the installed app on each OS (manual list).
- Notarisation and Windows signing are verified in CI when secrets exist; otherwise the release notes say unsigned.
- Checksums and attestations are attached.

### 13.13 Phase 13: Settings, accessibility and hardening

Priority P1. Depends on Phases 7, 8 and 9.

Goal: the rest of a finished app, checked.

Scope:
- Settings: account and profile; Family Chat account management guidance (panel link); sessions; security and privacy (recovery, key export, notification content, receipts and typing); appearance; notifications; diagnostics.
- Sign out and remove account.
- Accessibility pass to zmail's baseline; performance budgets job; egress test in CI.
- `docs/hardening.md` security checklist; fuzz smoke in CI.

Acceptance criteria:
- Every settings section has screenshots and view tests.
- Sign out removes keychain entries and data.
- The accessibility checklist is done, with screen-reader checks recorded per OS.
- The performance job passes.
- The egress test fails the build on any host other than the harness homeservers.

### 13.14 Phase 14: Next-generation auth and QR login for other servers

Priority P2. Depends on Phases 4, 5 and 8.

Goal: OAuth 2.0 (MSC3861), SSO and QR login (MSC4108/MSC4388) where the server supports them, ready for Family Chat when family-chat #239 lands.

Scope:
- matrix-sdk OAuth with the system browser and a loopback redirect (`local-server`); `m.login.sso`.
- QR login with the desktop showing the QR (`login_with_qr_code().generate()`), and granting a phone sign-in from the desktop.
- Capability detection; harness with Synapse and MAS; ADR 0010.

Acceptance criteria:
- Live: OAuth sign-in and token refresh on the MAS harness.
- Live: QR login between two SDK devices on the MAS harness.
- Hidden on Palpo (capability test).
- Family Chat allowlist rules apply to OAuth and QR sign-ins.

## 14. Later work (not yet phased)

GIF picker through family-chat #238; a supervision indicator once the platform defines one; a managed "Family Chat servers only" policy; tray icon, background running and start at login; threads; spaces; local message search (`matrix-sdk-search`); calls (family-chat #258); webcam QR scanning; arm64 Linux and Windows builds; Homebrew, winget and Flatpak; an updater and `packages.safechat.family`; translations; a passphrase-wrapped store key for headless Linux.

## 15. Open decisions for Rob

1. **Licence.** Recommendation: `GPL-3.0-only`, as zmail. All of matrix-rust-sdk, vodozemac, ruma and GPUI are Apache-2.0 or MIT and compatible. The Element forks are AGPL-3.0, so nothing is copied from them.
2. **Identifiers.** Recommendation: app and bundle id `family.safechat.desktop` (the id the Electron desktop build already uses, since this app replaces it); binary and Debian package `familychat`; keychain service `family.safechat.desktop`; data directory "Family Chat"; URL scheme `familychat`. The alternative is zmail's style (`ie.unicornops.familychat`), which would split Family Chat's ids across two namespaces.
3. **The Electron desktop build.** familychat-web's `apps/desktop` also claims `familychat://` and the `familychat` package name, and has no signed release yet. Recommendation: this app becomes the Family Chat desktop client; familychat-web stays the web app, and its desktop items in family-chat #235 (signing, packages host, auto-update) are closed or moved here once Phase 12 ships.
4. **How a desktop user receives a code.** Recommendation:
   - (a) In this repo, pasting the link or the QR image (planned).
   - (b) In family-chat, the website fallback page offers "Open in Family Chat for desktop" with the `familychat://` link on desktop browsers.
   - (c) Optionally, the panel offers "Sign in on this computer" when a parent mints a code on the child's computer. This opens the link without displaying it, but it relaxes the contract's "QR only" stance, so it is your call.
5. **Other Matrix servers in the Family Chat app.** Recommendation: allowed, behind "Use another Matrix server", because the app should be a full Matrix client and a determined child can install another client anyway. A managed policy that limits a device to Family Chat servers is later work. The alternative is a Family Chat-only build.
6. **Servers without native sliding sync.** Recommendation: unsupported with a clear message, as Element X does. Palpo and current Synapse have it, and a classic `/sync` fallback would double the sync and room-list code.
7. **Parental gate on desktop.** Recommendation: no maths gate (the store rule that motivated it is mobile only, decision 10 in family-chat #232), but every external link confirms with its full address, and there are no other exits.
8. **Signing.**
   - macOS: reuse the org-wide `APPLE_*` secrets, as zmail and the other UnicornOps macOS apps do.
   - Windows: Azure Artifact Signing, as decided for Family Chat in family-chat #232. You need to confirm the signing account and certificate profile exist, and where the secrets are managed (a protected `release` environment through gitops-environments, as proposed for the forks).
   - Linux: `.deb` signing deferred.
9. **Distribution and updates.** Recommendation: GitHub Releases only during alpha, no auto-updater; revisit `packages.safechat.family` and an updater at stable.
10. **Terminal UI style.** You chose Pine/Alpine for zmail's mail TUI. Recommendation for chat: zmail's frame (title bar, status line, always-visible command bar, `:` palette, Zellij-style copy), with a chat model where the composer has focus and Esc moves to the timeline for single-letter commands (`R` reply, `E` edit, `+` react, `D` delete, `O` open link).
