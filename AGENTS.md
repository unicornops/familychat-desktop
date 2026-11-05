# AGENTS.md

Shared instructions for human and AI contributors working in this repository.

## What This Repo Is

Family Chat Desktop is a native Rust Matrix client for macOS, Linux and Windows, focused on:

- being the best way to use Family Chat (`safechat.family`), including sign-in with a code from a parent
- standards-compliant Matrix through the matrix-rust-sdk, with end-to-end encryption by default, for any homeserver
- a GPUI desktop window and a ratatui terminal UI that share all non-visual logic
- security and privacy first: secrets only in the OS keychain, encrypted local stores, no third-party services

It is built in the same style as Zmail (unicornops/zmail): the same workspace shape, toolchain, lint and dependency policy, CI and release flow, ADR habit and agent instructions.

It is alpha software. Phase 0 (the scaffold), Phase 1 (secrets and the local store) and Phase 2 (the local homeserver harness) are in place: an empty Family Chat window, a placeholder terminal UI, the keychain and the encrypted app store behind `familychat accounts`, and real homeservers to test against. Each later phase is a GitHub issue (phase N is issue #N+1, tracked in #16).

## Important Files

- `README.md`: current status, commands and platform notes
- `PLAN.md`: product goals, architecture, Family Chat integration, security model and the phases of work
- `docs/adr/0001-architecture.md`: the workspace, crates and their rules, the GPUI and Tokio pattern, the identifiers
- `docs/adr/0002-gpui-zed-stable-pin.md`: pinning GPUI to Zed stable release tags, with the build costs
- `docs/adr/0003-local-data-secrets-and-store-encryption.md`: the data directory, the profile lock, the keychain entries, the app store's `StoreCipher` encryption and what stays visible, the keychain and missing-key states, the logs
- `docs/upstream/zed.md`: the current GPUI pin, the mirrored patches and how to bump
- `docs/adr/0004-homeserver-harness.md`: the local homeserver harness, its test-only CA trust and how releases are kept free of it
- `docs/upstream/homeservers.md`: the harness's image pins (Palpo follows family-chat's `PALPO_IMAGE`) and how to move them

## Workspace Overview

- `crates/familychat-app`: the `familychat` binary: CLI (`familychat` opens the window, `familychat tui`, `familychat accounts`, `familychat start-over`, `familychat --version`, `familychat help`), wiring, and `build.rs` (the Zed pin read from `Cargo.lock`, the Windows version resource). Every command that uses the profile takes its lock first, then starts logging, then opens the keychain and the store. The terminal UI is its `tui` feature, on by default
- `crates/familychat-ui`: the GPUI frontend. Phase 0: one empty window (`ShellView`) in the bundled theme that matches the OS appearance, the Linux UI font, and the view tests, including the one that awaits from a GPUI task a future that ran on Tokio. Never depends on matrix-sdk or ratatui
- `crates/familychat-tui`: the terminal frontend on ratatui and crossterm: Zmail's frame (title bar, body, status line, command bar) with a placeholder that quits on `q`; the terminal guard copied from Zmail. `ratatui` `TestBackend` snapshots at 80x24 and 160x48 live in `src/snapshots` (`FAMILYCHAT_UPDATE_SNAPSHOTS=1 cargo test -p familychat-tui` rewrites them; review the diff). Never depends on GPUI or matrix-sdk
- `crates/familychat-core`: domain types and view models both frontends draw; `identity` holds the app's names and identifiers (app id `family.safechat.desktop`, binary `familychat`, URL scheme `familychat`, the data directory names and `FAMILYCHAT_DATA_DIR`). No GPUI, ratatui or network, and no matrix-sdk types in its API
- `crates/familychat-security`: the OS keychain (`keyring`, one service id `family.safechat.desktop`, typed entries `app-store`, `account-<uuid>-session`, `account-<uuid>-store`) behind the `Keychain` trait, with `OsKeychain` for the app and `MemoryKeychain` (injectable locked, unreachable and forgetful failures) for tests; `Secret<T>` and `Passphrase` (32 bytes from the OS CSPRNG), whose `Debug` prints `[redacted]` and which are wiped on drop; `AccountId` (random UUIDs); `redact` (the `Redacted` wrapper and the scrubber every log line passes through); `logging` (the local log file and the crash log). No GPUI, ratatui or matrix-sdk
- `crates/familychat-store`: the data directory per OS and `FAMILYCHAT_DATA_DIR` (`paths`), the exclusive profile lock `familychat.lock` (`lock`), and `Profile::open` with its states (`Ready`, `KeychainUnavailable` with retry, `Unreadable` with start over, which moves the data to `unreadable-<time>/`); the app store `familychat.sqlite3` (rusqlite with migrations, identifying values encrypted with matrix-sdk's `StoreCipher`): the accounts registry, plaintext preferences, encrypted UI state. Depends on `matrix-sdk-store-encryption` only, not matrix-sdk
- `crates/familychat-session`: `BackgroundRuntime`, the dedicated Tokio runtime copied from Zmail's `zmail-sync`; the Matrix sessions arrive in Phase 3
- `crates/familychat-theme`: the Zed theme compatibility layer (copied from Zmail's `zmail-theme`) and the bundled Family Chat light and dark themes (`themes/`)
- `crates/familychat-assets`: embedded Lucide icons (`assets/icons`) and the Inter faces Linux builds use (`assets/fonts/inter`), copied from Zmail's `zmail-assets`
- `crates/familychat-test-support`: test support, never a dependency of the app. `HarnessEnv` reads the homeserver harness's settings; behind the `test-harness` feature (test builds only), `HomeserverHarness` drives the harness (members, rooms, sending as any member, login tokens, suspension) through a client that trusts only the harness CA and resolves only the harness hosts. Its live tests are `tests/live_homeserver_harness.rs`

## Repo Expectations For Agents

- Keep the UI off the network: the frontends draw view models from local state and never talk to a server themselves. Never block the UI thread on network, disk, crypto or decoding.
- Preserve the crate boundaries in ADR 0001: `familychat-ui` and `familychat-tui` never depend on matrix-sdk, matrix-sdk-ui or each other's toolkit, and `familychat-core` exposes no matrix-sdk types.
- Every Matrix future runs on `BackgroundRuntime`; results come back through runtime-agnostic channels that GPUI tasks await (`cx.spawn`) or the terminal UI polls.
- Secrets live only in the OS keychain, through `familychat-security`'s `Keychain`: never in a file, the store, an environment variable or a log, and there is no plaintext fallback when the keychain is locked or missing (ADR 0003). Hold them in `Secret<T>` (or `Passphrase`), and give any new secret-bearing type a `Debug` that prints `[redacted]`.
- Never log tokens, passwords, keys, message bodies or query strings. Wrap private values in `Redacted`; the log scrubber is a second line of defence, not the first.
- In the app store, encrypt anything that identifies a person, a server or a room with the `StoreCipher` (keyed hashes for lookups); only non-identifying preferences are plaintext. `rusqlite` stays on the line `matrix-sdk-sqlite` uses (`libsqlite3-sys` links SQLite once per build) and moves only with the Matrix crates; no SQLCipher.
- Take the profile lock before touching the keychain or the store: one process per profile.
- Tests never touch the developer's real keychain: use `MemoryKeychain`. Real-keychain tests are `#[ignore]`d, use a test-only service id, and run in CI's build job on each OS.
- Keep security and privacy defaults strict. User message content is never sent to a third-party hosted service: the app talks only to the account's homeserver. No analytics, crash reporting or remote logging, no link previews, no remote content beyond the homeserver.
- Nothing may be copied from the Element forks (AGPL-3.0) into this GPL-3.0-only codebase. Code copied from Zmail keeps a note saying so; record ported files and bundled assets in `about.toml`.
- Pin upstream inputs tightly and move them deliberately. Record each significant decision as an ADR in `docs/adr/`.
- Release packaging ships platform archives (Linux, macOS, Windows) with `LICENSE`, `THIRD_PARTY.html` and the bundled assets' licences. Native installers (`.deb`, DMG, MSI), signing and the `familychat://` scheme registration are Phase 12: do not describe any of them as done until they are.

## Commands

Core verification:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Store and security tests run in the workspace tests on every OS. The real-keychain tests are ignored there; run them on a desktop session with `cargo test -p familychat-security -- --ignored real_keychain`, or on headless Linux against a throwaway Secret Service with `scripts/keychain-session.sh cargo test -p familychat-security -- --ignored real_keychain` (needs `dbus` and `gnome-keyring`; your own keyrings are never seen). CI's build job runs them on each OS, and `familychat accounts` twice through the platform keychain.

UI view tests use GPUI's test platform (`#[gpui::test]` with `TestAppContext`, from the `test-support` feature in `familychat-ui`'s dev-dependencies). They run headless with no display server; on Linux they only need the build packages below. A test that waits on another thread (the Tokio runtime) calls `cx.executor().allow_parking()` first.

Live homeserver harness (ADR 0004; needs Docker with Compose, `curl`, `jq` and `openssl`):

```bash
scripts/homeserver-harness.sh up       # Palpo (Family Chat's image) + Postgres, Synapse, the TLS front end; bootstraps the admin bot
cargo test -p familychat-test-support --test live_homeserver_harness -- --ignored
scripts/homeserver-harness.sh env      # the FAMILYCHAT_HARNESS_* settings the tests read
scripts/homeserver-harness.sh mint-token alice   # a single-use m.login.token for @alice:hs1.familychat.test
scripts/homeserver-harness.sh down
```

The front end serves `hs1.familychat.test` (a Family Chat family's edge proxy, Palpo), `smith.test` (a custom domain whose well-known delegates to hs1), `evil.familychat.example` (a homeserver outside the allowlist) and `synapse.test` on `127.0.0.1:18443`, with a CA made on every `up` (`target/homeserver-harness/ca.pem`); Palpo's admin API is on `127.0.0.1:18008`. Nothing on the machine trusts the CA: only `familychat-test-support`'s client does. Live tests are `#[ignore]` and need the harness up first; CI's quality job runs them with `-- --ignored`, with Docker Hub logins from the optional `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN` secrets. Create members through the admin API (`HomeserverHarness::new_user`), not registration: Palpo allows a burst of only three registrations per peer, and the whole harness is one peer. `scripts/check-release-features.sh` (CI and the release workflow) fails if a release build of `familychat-app` could include `familychat-test-support` or any `test-harness` feature; never make the app depend on either.

Release build, and the app itself:

```bash
cargo build -p familychat-app --release --locked
target/release/familychat             # the window
target/release/familychat tui         # the terminal UI; q quits
target/release/familychat accounts    # the accounts registry, through the keychain and the store
target/release/familychat start-over  # move data whose key is gone aside and start a new store
target/release/familychat --version   # the version and the GPUI pin
```

`FAMILYCHAT_DATA_DIR` picks another profile directory (use one under a temporary directory to try things without touching your real profile). `FAMILYCHAT_LOG` sets the log directives (`familychat=debug,matrix_sdk=info`); logs go to `logs/familychat.log` in the profile, and panics to `logs/crash.log`.

Window smoke test (what CI's build job runs on each platform; Linux needs Xvfb, xdotool, ImageMagick and Mesa's lavapipe Vulkan driver, `mesa-vulkan-drivers`):

```bash
xvfb-run -a --server-args="-screen 0 1280x800x24" scripts/window-smoke.sh target/release/familychat artifacts/window-smoke
```

On macOS run `scripts/window-smoke.sh` without `xvfb-run`; on Windows, `./scripts/window-smoke.ps1 -App target/release/familychat.exe -OutputDir artifacts/window-smoke`. Each opens the window, checks its title, takes a screenshot and quits; CI uploads them as the `window-smoke-<OS>` artefacts.

Dependency policy:

```bash
cargo deny check licenses advisories bans sources
cargo audit
cargo about generate --locked --fail about.hbs -o THIRD_PARTY.html
```

`deny.toml` ignores only the "unmaintained" advisories of crates the pinned GPUI pulls in, each with its reason; re-check them when the pin moves, and do not add an ignore without a strong reason. `about.toml` accepts the same licences as `deny.toml` (install cargo-about with `cargo install cargo-about --locked --features cli`).

Dependabot (`.github/dependabot.yml`) opens weekly update pull requests for Cargo (`fix(deps)`, minor and patch grouped, the Matrix crates in a group of their own) and GitHub Actions (`ci(deps)`), plus security updates for Dependabot alerts. It skips the GPUI pin; the `zed-upstream` workflow moves that.

Linux build packages:

```bash
sudo apt-get update && sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype6-dev
```

At runtime GPUI renders through wgpu on Vulkan (`libvulkan1`) with an EGL/GL fallback (`libegl1`), and the keychain needs a Secret Service (GNOME Keyring, KWallet) in the session; without one Family Chat reports the keychain as unavailable.

macOS note:

- GPUI may need the Metal toolchain installed with `xcodebuild -downloadComponent MetalToolchain`.

GPUI pin:

- GPUI is a git dependency pinned to a Zed stable release tag (`tag = "vX.Y.Z"` on the `gpui` and `gpui_platform` lines of `Cargo.toml`), not the crates.io `gpui` crate. Keep both lines on the same tag, and `rust-toolchain.toml` on the toolchain Zed pins at that tag.
- The weekly `zed-upstream` workflow (`scripts/bump-zed-gpui.sh`) opens a `chore(deps): bump Zed GPUI to vX.Y.Z` pull request when Zed ships a newer stable release; see `docs/upstream/zed.md` for manual bumps and the mirrored `[patch.crates-io]` entries.
- Start the app with `gpui_platform::application()`. Do not use `Application::new_inaccessible`, which disables AccessKit screen-reader support.

Releases:

- release-please (`.github/workflows/release-please.yml`) keeps a release pull request open from the Conventional Commits on `main`; `scripts/render-release-pr.sh` adds the commits outside `crates/familychat-app` that release-please does not count. Merging the release pull request tags `vX.Y.Z` and attaches `familychat-linux-x86_64.tar.gz`, `familychat-macos.tar.gz` and `familychat-windows-x86_64.zip` with checksums. Pull requests that change the release workflow or the notices build the archives as a dry run (workflow artefacts only); `release-backfill.yml` rebuilds a release's Windows archive.
- Release pull requests and Zed bump pull requests are opened with `RELEASE_PLEASE_TOKEN` when that secret exists, otherwise with `GITHUB_TOKEN`, whose pull requests do not start CI on their own.

## Commit Policy

Conventional Commits must always be used.

Use formats like:

- `feat: add the sign-in code confirmation`
- `fix: keep the timeline at the bottom after a resize`
- `docs: clarify packaging status`
- `ci: add Linux font dependencies`

Do not create non-conventional commit messages in this repository. The repository squash-merges pull requests, so the pull request title is the commit release-please reads.

## Related Agent Files

- [`CLAUDE.md`](./CLAUDE.md)
- [`.github/copilot-instructions.md`](./.github/copilot-instructions.md)

Those files should stay aligned with this document and point back here instead of drifting into separate instruction sets.
