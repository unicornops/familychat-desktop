# Family Chat Desktop

> [!WARNING]
> **Family Chat Desktop is alpha software and is in no way ready for real use.** Right now the window signs you in and shows your account's sync state, and the terminal UI is a placeholder: you cannot read or send messages yet. Expect bugs, missing features and breaking changes between releases. Use the Family Chat apps for iOS, Android or the web in the meantime.

Family Chat Desktop (`familychat`) is a native desktop chat client for macOS, Linux and Windows, written in Rust, with a graphical UI built on GPUI and a terminal UI built on ratatui. It is a standards-compliant Matrix client that works with any homeserver supporting what modern Matrix clients need, and its first job is to be the best way to use [Family Chat](https://safechat.family), the family-safe hosted Matrix service, including easy sign-in with a code from a parent.

It is built in the same style as [Zmail](https://github.com/unicornops/zmail). [`PLAN.md`](./PLAN.md) has the goals, architecture, security model and phases of work; each phase is a GitHub issue, tracked in [#16](https://github.com/unicornops/familychat-desktop/issues/16).

## Current Status

Phase 0, the repository scaffold ([#1](https://github.com/unicornops/familychat-desktop/issues/1)):

- a Rust workspace in Zmail's style: edition 2024, the Rust toolchain Zed pins, GPL-3.0-only, strict lints, `cargo deny`, `cargo audit` and `cargo about` ([ADR 0001](./docs/adr/0001-architecture.md))
- GPUI from Zed's `v1.27.0` stable release ([ADR 0002](./docs/adr/0002-gpui-zed-stable-pin.md), [`docs/upstream/zed.md`](./docs/upstream/zed.md)), bumped by a weekly workflow
- `familychat` opens an empty Family Chat window in the light or dark theme that matches the system
- `familychat tui` draws a placeholder in the terminal; `q` quits
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
- once signed in, the account and its sync state (including "Paused" for a Family Chat curfew); rooms and messages are Phase 6
- `familychat launch-ui-fixture` (a build feature) and a screenshot of every sign-in screen in dark and light on each pull request that touches the UI

The terminal UI signs in from Phase 10. Drawing rooms and the timeline, sending, encryption, notifications and the rest follow in the later phases.

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
target/release/familychat tui         # the terminal UI
target/release/familychat accounts    # the accounts registry
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
