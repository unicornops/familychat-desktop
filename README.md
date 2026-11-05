# Family Chat Desktop

> [!WARNING]
> **Family Chat Desktop is alpha software and is in no way ready for real use.** Right now the window signs you in and lets you read and write in your rooms (without device verification or key backup yet), and the terminal UI signs you in and lets you read them. Expect bugs, missing features and breaking changes between releases. Use the Family Chat apps for iOS, Android or the web in the meantime.

Family Chat Desktop (`familychat`) is a native desktop chat client for macOS, Linux and Windows, written in Rust, with a graphical UI built on GPUI and a terminal UI built on ratatui. It is a standards-compliant Matrix client that works with any homeserver supporting what modern Matrix clients need, and its first job is to be the best way to use [Family Chat](https://safechat.family), the family-safe hosted Matrix service, including easy sign-in with a code from a parent.

It is built in the same style as [Zmail](https://github.com/unicornops/zmail). [`PLAN.md`](./PLAN.md) has the goals, architecture, security model and phases of work; each phase is a GitHub issue, tracked in [#16](https://github.com/unicornops/familychat-desktop/issues/16).

## Current Status

Phase 0, the repository scaffold ([#1](https://github.com/unicornops/familychat-desktop/issues/1)):

- a Rust workspace in Zmail's style: edition 2024, the Rust toolchain Zed pins, GPL-3.0-only, strict lints, `cargo deny`, `cargo audit` and `cargo about` ([ADR 0001](./docs/adr/0001-architecture.md))
- GPUI from Zed's `v1.27.0` stable release ([ADR 0002](./docs/adr/0002-gpui-zed-stable-pin.md), [`docs/upstream/zed.md`](./docs/upstream/zed.md)), bumped by a weekly workflow
- `familychat` opens an empty Family Chat window in the light or dark theme that matches the system
- `familychat tui` draws a placeholder in the terminal; `q` quits (signing in and reading came in Phase 10)
- `familychat --version` prints the version and the GPUI it was built with
- the dedicated Tokio runtime every Matrix call will run on, with a test that a GPUI task awaits work done on it
- CI on Linux, macOS and Windows (lint, tests, a release build and a window smoke test with a screenshot on each), and releases from Conventional Commits with release-please

Phase 1, secrets and the local store ([#2](https://github.com/unicornops/familychat-desktop/issues/2), [ADR 0003](./docs/adr/0003-local-data-secrets-and-store-encryption.md)):

- secrets only in the OS keychain (macOS login keychain, Windows Credential Manager, the Secret Service on Linux), never with a plaintext fallback
- an app store (accounts registry, preferences, UI state) in SQLite, with every Matrix ID, homeserver and room ID encrypted with matrix-sdk's store cipher
- one Family Chat per profile: a second one says it is already open
- a recovery path when the key to the local data is gone: `familychat start-over` moves the old data aside
- `familychat accounts` lists the accounts
- local, redacted logs and a crash log in the profile's `logs/`; nothing is uploaded

Phase 3, the Matrix session core ([#4](https://github.com/unicornops/familychat-desktop/issues/4), [ADR 0005](./docs/adr/0005-matrix-session-runtime-and-bridging.md)):

- matrix-sdk and matrix-sdk-ui 0.19.1, with end-to-end encryption, encrypted SQLite stores and rustls; only `familychat-session` depends on them, and CI checks that the window, the terminal UI and `familychat-core` never do
- each account restores from the keychain at start without signing in again, and syncs with native sliding sync; a server without it gets a clear "not supported" state
- sync states both frontends will show with the same wording, including a suspended account (a Family Chat curfew) as its own state that rechecks slowly instead of an error
- refresh tokens rotate, and each new token is in the keychain before it is used
- the room list (with all, unread, people and invites filters) and room timelines (with back-pagination) streamed as view models, ready for the GUI (Phase 5 and 6) and the terminal UI (Phase 10) to draw

Phase 4, the sign-in core ([#5](https://github.com/unicornops/familychat-desktop/issues/5), [ADR 0006](./docs/adr/0006-sign-in-rules-and-flows.md)):

- every rule of Family Chat's sign-in link contract, as one state machine both frontends will drive: sign-in links (`https://safechat.family/app/login?...`, `familychat://...` or pasted), a confirmation before any code is used, one redemption at a time through a client that never follows a redirect or retries, the account bound to the link, and a used or expired code falling through to the password
- a strict server-name parser and the `*.safechat.family` allowlist, applied to where a family's address really leads (so custom-domain families work), with nothing but discovery sent to a server outside it, and a re-check after every sign-in
- password sign-in for Family Chat and for any other Matrix server over HTTPS; a rate limit is shown with its wait, never retried
- tokens and passwords never in logs, debug output, panic messages or on disk; proven against the local homeserver harness from its access log

Phase 5, the window's shell and sign-in screens ([#6](https://github.com/unicornops/familychat-desktop/issues/6), [ADR 0011](./docs/adr/0011-gui-shell-sign-in-screens-and-fixtures.md)):

- Zmail's component set and window frame, so the window draws its own title bar everywhere (GNOME on Wayland included), in the Family Chat light and dark themes, with keymaps and a command palette (Ctrl+K, or Cmd+K on macOS) built from one commands table
- sign in with a code from a parent: paste the sign-in link, or paste or drop the QR code image (read on your computer, never uploaded); "Sign in as …?" is always asked before a code is used, and a used or expired code falls back to your password
- sign in with your password, to your family's server or any other Matrix server, with the wait counted down when the server asks to slow down
- the keychain and lost-key screens (Try again, Start over) when the local data can't be opened
- once signed in, the account and its sync state (including "Paused" for a Family Chat curfew)
- `familychat launch-ui-fixture` (a build feature) and a screenshot of every sign-in screen in dark and light on each pull request that touches the UI

Phase 6, rooms and the timeline in the window ([#7](https://github.com/unicornops/familychat-desktop/issues/7), [ADR 0007](./docs/adr/0007-message-rendering-and-media.md)):

- the room list with all, unread, people and invitation filters, unread and mention badges, and invitations you accept or decline from the list
- the open room's header with its encryption state (a banner in rooms that are not encrypted), and its member list with roles
- the timeline, scrolling smoothly through tens of thousands of messages: text, notices, emotes, formatted messages (sanitised HTML drawn as styled text, never as a web page), replies, edits, deleted messages, reactions, the "New messages" marker, day dividers, folded room changes and a placeholder for messages this device can't read yet
- images from your server through authenticated media, decoded on your computer within strict limits (SVG is never drawn); files saved where you choose, under a safe name
- `matrix:` and `matrix.to` links open in the app; every other link shows its whole address and asks before your browser opens it, and warns when its text shows a different site
- keyboard commands to move between rooms and unread rooms, jump to the oldest unread or the latest message, show the members and switch filters

Phase 7, writing ([#8](https://github.com/unicornops/familychat-desktop/issues/8), [ADR 0012](./docs/adr/0012-messaging-actions-and-the-composer.md)):

- a composer under each room: Enter sends, Shift+Enter starts a new line, and Markdown (**bold**, _italic_, `code`, lists, quotes) is sent as the formatted text Family Chat itself would show; `@` suggests the room's members
- reply, edit and delete your messages, and react with a quick picker or by clicking a reaction; deleting a message and leaving a room ask first; each action is a keyboard command too
- files from the attach button, the clipboard or a drop: one larger than your server takes (11 MB on Family Chat) is refused before anything is uploaded, and a photo's location is taken out before it is sent
- a message written while offline waits and goes when the connection is back; what you were writing in a room is kept (encrypted) until you come back to it
- during a Family Chat curfew the window says "Chat is paused" and the composer is read-only until the pause ends
- the room hears when you are typing and how far you have read; new rooms and direct chats are encrypted unless your server says otherwise
- all of it through `familychat-actions`, which the terminal UI will use too

Phase 8, end-to-end encryption ([#9](https://github.com/unicornops/familychat-desktop/issues/9), [ADR 0009](./docs/adr/0009-end-to-end-encryption.md)):

- "Confirm it's you" after signing in on a new device: compare emoji with a phone or computer that is already signed in (or let the phone scan a code the window shows), or enter your recovery key; you can skip for now, after a warning, and a banner reminds you
- your account's cross-signing is set up for you: a sign-in with a code from a parent needs no password on Family Chat, and if another server asks for one, the window explains why and asks for it
- recovery: a recovery key, shown once with Copy and Save, and an encrypted backup of your message keys on your server, so a new device can read your history
- your devices, with which ones are verified; verify or sign out the others
- verification requests from your other devices open in the window; a banner in a room when someone's identity changes, and a reason under every message this device can't read
- a message that stops because someone has a device they haven't confirmed says so, with Send anyway
- room keys exported to a file protected by a passphrase, and imported from one (what moving your family to its own address needs)
- Element X's encryption defaults (ADR 0009 lists them and the one place Family Chat differs)

Phase 9, notifications, single instance and sign-in links ([#10](https://github.com/unicornops/familychat-desktop/issues/10), [ADR 0013](./docs/adr/0013-notifications-single-instance-and-sign-in-links.md)):

- your computer's own notifications for new messages while Family Chat is open, for the rooms you aren't looking at: who wrote, where and what (or just "New message" if you turn the text off in Settings → Notifications, Ctrl+Alt+N or Cmd+Option+N), one per room however many arrive, and never more than six a minute; clicking one opens the room
- the number of unread messages in the window's title
- one Family Chat per profile, now with a hand-off: opening a sign-in link while Family Chat is open (`familychat open-link`, which reads the link so it never sits in your shell history, or `familychat LINK`) brings the open window forward with "Sign in as …?", over your rooms if you're signed in, and nothing is used until you say Continue
- the link travels only over a socket or pipe your user alone can open, and never reaches a log

Phase 10, the terminal UI: sign-in and reading ([#11](https://github.com/unicornops/familychat-desktop/issues/11), [ADR 0008](./docs/adr/0008-terminal-ui.md)):

- `familychat tui` in Zmail's terminal style: a title bar, the screen, a status line and a command bar that always shows the keys that work, built from the same commands table as the window; `:` opens a palette of them
- the same start as the window, with the keychain and lost-key screens; it refuses to start while the window (or another terminal) has the profile open
- sign in with a code from a parent by pasting the link (always asked "Sign in as …?" first, answered only with `y`), or with your password, to your family's server or any other
- the room list and a room side by side from 120 columns (or one after the other), with filters, unread and mention badges and invitations; timelines with formatted text as terminal styles, replies, reactions, folded room changes and the sync state, "Paused" included
- every link and file numbered: links show their whole address and ask before your browser opens them, files are saved where you type
- the mouse (click, scroll, drag to select and copy, as in Zellij), new messages in the status line with an optional bell (`--bell`), `NO_COLOR` and ASCII drawing (`--ascii`)
- every name, message and file name from a server is filtered for escape sequences before it reaches your terminal

Phase 13, settings, accessibility and hardening ([#14](https://github.com/unicornops/familychat-desktop/issues/14), [ADR 0015](./docs/adr/0015-settings-accessibility-and-hardening.md)):

- a Settings window (Ctrl+, or Cmd+,): your account and this device; for a Family Chat account, where a parent resets your password, gives you a new sign-in code or deactivates the account (the Family Chat panel, opened only after the usual "Open this link?"); your sessions; recovery (and a new recovery key), room keys, and resetting your identity when nothing else is left; read receipts and typing notices you can turn off; theme, text size up to 200%, reduced motion and increased contrast; notifications on or off, with or without the text; and diagnostics: the version, where the logs are and a button to open their folder (nothing is ever uploaded)
- sign out: your server is told when it can be, then the account's keys, stored messages and pictures, drafts and settings are removed from this computer
- Send anyway shows which devices or people a stopped message would go to before it is sent
- the accessibility checklist ([`docs/accessibility.md`](./docs/accessibility.md)): names and roles for every control, the keyboard, announcements, 24 px targets, contrast, text size and motion, with the screen-reader check to run on each OS
- the hardening checklist ([`docs/hardening.md`](./docs/hardening.md)), and in CI: the performance budgets in a release build, an egress test that fails the build if the live tests reach any host but the test homeservers, and a fuzz run of the sign-in link parser and the message sanitiser

Writing and encryption from the terminal (Phase 11) and the rest follow in the later phases; opening `familychat://` links straight from your browser needs the installers of Phase 12.

## Building

Install the Rust toolchain with [rustup](https://rustup.rs); `rust-toolchain.toml` picks the right version.

On Linux (Debian and Ubuntu package names):

```bash
sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype6-dev
```

GPUI renders through Vulkan (`libvulkan1`) or EGL/GL (`libegl1`) at runtime. On macOS GPUI may need the Metal toolchain: `xcodebuild -downloadComponent MetalToolchain`.

```bash
cargo build -p familychat-app --release --locked
target/release/familychat             # the window
target/release/familychat tui         # the terminal UI (--bell, --ascii, --no-mouse); q quits
target/release/familychat accounts    # the accounts registry
target/release/familychat open-link   # paste a sign-in link; it goes to the open window, or opens one
target/release/familychat --version
```

Family Chat keeps its data in `~/Library/Application Support/Family Chat` (macOS), `~/.local/share/familychat` (Linux) or `%APPDATA%\Family Chat` (Windows); `FAMILYCHAT_DATA_DIR` picks another directory. On Linux it needs a Secret Service (GNOME Keyring or KWallet) running in your session.

Checks before a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check licenses advisories bans sources
cargo audit
```

The window's screens can be opened on fixtures, with no network, keychain or profile, and captured in dark and light under Xvfb (CI uploads them as the `ui-screenshots` artefact on pull requests that touch the UI):

```bash
cargo build -p familychat-app --release --locked --features ui-fixture
target/release/familychat launch-ui-fixture --list
target/release/familychat launch-ui-fixture confirm dark
xvfb-run -a --server-args="-screen 0 1920x1080x24" scripts/capture-ui-screenshots.sh --matrix artifacts/ui-screenshots
```

The terminal UI opens on the same fixtures (its snapshots are in `crates/familychat-tui/src/snapshots`):

```bash
cargo run -p familychat-app --features ui-fixture -- launch-tui-fixture --list
cargo run -p familychat-app --features ui-fixture -- launch-tui-fixture room
```

See [`AGENTS.md`](./AGENTS.md) for the crates, commands and rules.

## Platform Packages

Each release on the [releases page](https://github.com/unicornops/familychat-desktop/releases) attaches:

- `familychat-linux-x86_64.tar.gz` (Linux, x86-64)
- `familychat-macos.tar.gz` (macOS, Apple silicon)
- `familychat-windows-x86_64.zip` (Windows, x86-64)

Each holds the `familychat` binary, `LICENSE`, `THIRD_PARTY.html` (the licences of the Rust crates it is built from) and `licenses/` (the bundled Inter font and Lucide icons), with `SHA256SUMS-<OS>` checksums next to them. The binaries are not signed yet: macOS will ask before opening one from the internet. Installers (`.deb`, DMG, MSI), signing and the `familychat://` link handler come in Phase 12.

## Privacy

User message content never leaves the device except to the account's own homeserver: no analytics, crash reporting, remote logging, link previews or third-party media services (PLAN.md, section 8).

## Licence

GPL-3.0-only; see [`LICENSE`](./LICENSE). Bundled third-party assets keep their own licences: Inter (SIL Open Font License 1.1, `assets/fonts/inter/LICENSE.txt`) and Lucide icons (ISC, `assets/icons/LICENSE.txt`).
