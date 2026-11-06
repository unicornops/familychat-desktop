# Hardening checklist

The security and privacy controls of Family Chat Desktop (PLAN.md sections
7 and 8), where each is enforced and what proves it. Threats, in order: a
crafted link or QR code that phishes a password or signs a child into
someone else's account; a malicious homeserver or message (HTML, images,
escape sequences, file names); a stolen or copied disk; another local user;
the supply chain.

Each item is either **checked** (a test or a CI job fails when it breaks),
**by design** (the code has one way to do the thing, reviewed, with no
automated check), or **manual** (a check by hand per release or phase).

## Sign-in

| Control | Where | Proof |
|---|---|---|
| Every Family Chat sign-in rule (allowlist before anything is sent, confirm before a code is redeemed, the account bound to the link, no redirects, no retries, no password to `hs`, the post-login re-check) | `familychat-signin` (ADR 0006) | checked: `tests/flows.rs`, `tests/properties.rs`, `tests/mocked_signin.rs`, live `tests/live_signin.rs` (from the access log); ADR 0006's table maps each rule of family-chat's `docs/client-login-links.md` to its test |
| The window never redeems a code without "Continue" on the confirmation | `familychat-ui` `signin` | checked: `view_tests/signin.rs` |
| The terminal answers the confirmation only with `y` | `familychat-tui` `signin` | checked: `src/tests/behaviour.rs` |
| A QR image is decoded locally, bounded, and its text never shown or kept | `familychat-ui` `qr` | checked: `qr::tests`; by design: the text goes straight to `SigninStart::Link` |
| The link parser and the server-name parser survive any input | `familychat-signin` `link`, `host` | checked: proptest in `tests/properties.rs`; fuzz smoke (`fuzz/fuzz_targets/signin_link.rs`, CI's `fuzz` job, 60 s per run) |
| A forwarded link is a sign-in like any other (the confirmation first), never logged | `familychat-instance`, `familychat-ui` `shell` | checked: `familychat-instance/tests/logs.rs`, `view_tests/forwarded_links.rs` |

## Transport

| Control | Where | Proof |
|---|---|---|
| rustls with the system roots, HTTPS only, no insecure option | `familychat-session` `config`, `familychat-signin` `http` | checked: `config::tests::the_app_client_refuses_plain_http` |
| Test CAs and the harness hostnames only behind `test-harness`, never in a release | `familychat-test-support` | checked: `scripts/check-release-features.sh` (CI and the release workflow) |
| No traffic to anything but the account's homeserver | everywhere | checked: the egress test (ADR 0004, "Egress"): the live tests run through a recording proxy and `scripts/check-egress.sh` fails the build on any other host; `--self-test` proves it fails |

Known: the app's own HTTP clients (`SessionConfig::new`,
`ReqwestTransport::new`) honour `HTTPS_PROXY`, `HTTP_PROXY` and
`ALL_PROXY` as reqwest does by default, so a person (or their company)
can route the app through a proxy. The proxy sees the hostnames; TLS stays
end to end and is checked against the system roots. Only the harness
clients refuse proxies they weren't given.

## Local data

| Control | Where | Proof |
|---|---|---|
| Secrets only in the OS keychain; no plaintext fallback when it is locked or missing | `familychat-security` `keychain`, `familychat-store` `profile` (ADR 0003) | checked: `profile` tests with `MemoryKeychain` (locked, unreachable, forgetful); `real_keychain` tests in CI's build job on each OS |
| The SDK's stores encrypted with a keychain passphrase; identifying values in the app store encrypted with the `StoreCipher` | `familychat-session` `account`, `familychat-store` `app_store` | checked: `familychat-store/tests/no_identifying_values_on_disk.rs`, the session's `assert_not_in_files` checks |
| One process per profile (the lock before the keychain or the store) | `familychat-store` `lock`, `familychat-app` | checked: `familychat-app/tests/profile_lock.rs` |
| Signing out removes the keychain entries, the registry row and drafts, and the SDK's stores and caches, secrets first | `familychat-session` `AccountManager::sign_out`, `familychat-store` `Profile::remove_account` (ADR 0015) | checked: `familychat-session/tests/sign_out.rs` (server told, refusing, unreachable; the sweep of leftovers at the next start), `profile` tests |
| A password, recovery key or passphrase is used for one request and wiped, never stored or logged | `familychat-core` `SecretText`, `familychat-security` `Secret` | checked: `Debug` tests, `familychat-signin/tests/logs.rs`, the view tests' "the password is wiped" and "no password is recorded" checks |
| Notifications show "New message" when the person hides the text, and nothing at all when they are off | `familychat-notify`, `familychat-ui` `notifications` | checked: `familychat-notify` tests, `view_tests/notifications.rs`, `view_tests/settings.rs` |
| Logs are local, redacted and never hold tokens, passwords, keys, bodies or query strings | `familychat-security` `redact`, `logging` | checked: the scrubber's tests, `familychat-signin/tests/logs.rs`, `familychat-instance/tests/logs.rs` |

Known: what stays visible on disk is the table structure, row counts and
value sizes (ADR 0003). A passphrase-wrapped store key for a headless
Linux without a Secret Service is later work.

## Content

| Control | Where | Proof |
|---|---|---|
| Matrix HTML sanitised (ammonia with the Matrix allowlist, caps on depth, blocks, links and text) and drawn as a tree, never markup, never the sender's colours | `familychat-security` `matrix_html` (ADR 0007) | checked: `tests/hostile_html.rs`; fuzz smoke (`fuzz/fuzz_targets/matrix_html.rs`) |
| No web view, no link previews, no remote content beyond the homeserver; media through authenticated media only | `familychat-session` `media` | checked: `mocked_session.rs` (`media_is_never_fetched_without_authenticated_media`), the live image test's access-log check, the egress test |
| Images decoded within limits, SVG never | `familychat-security` `images` | checked: `tests/image_bombs.rs` |
| A link out of the app shows its whole address and asks first, and look-alikes are flagged; the Family Chat panel too | `familychat-core` `links`, the rooms' and Settings' link dialog | checked: `links` tests, `view_tests/rooms.rs`, `view_tests/settings.rs` |
| Safe file names; files saved only where the person chose, never opened | `familychat-security` `file_names`, `familychat-session` `media` | checked: `file_names` tests, `view_tests/rooms.rs` |
| A photo's location is taken out before it is sent | `familychat-security` `image_metadata`, `familychat-actions` | checked: `image_metadata` tests, live `live_actions.rs` |

## Terminal

| Control | Where | Proof |
|---|---|---|
| Every server string through `terminal::sanitize` (controls, ESC and every sequence it starts, C1, bidi, zero-width) | `familychat-security` `terminal`, `familychat-tui` | checked: `tests/terminal_corpus.rs`, `familychat-tui/src/tests/hostile.rs` |
| The only escape sequences written are ratatui's, the bell and OSC 52 for the person's own copy | `familychat-tui` | by design (ADR 0008) |

## Local IPC

| Control | Where | Proof |
|---|---|---|
| A user-only socket in a 0700 directory, or a pipe with a user-only DACL and a random name; strict, bounded parsing | `familychat-instance` (ADR 0013) | checked: its tests on every OS |
| `unsafe` only in `familychat-instance/src/windows_security.rs`, each block justified | workspace lints | checked: `unsafe_code = "forbid"` everywhere else |

## Code and supply chain

| Control | Where | Proof |
|---|---|---|
| Licences, advisories, bans and sources | `deny.toml` | checked: `cargo deny` in CI's quality job; no ignore without a reason recorded next to it |
| Advisories | | checked: `cargo audit` in CI |
| Third-party notices shipped | `about.toml` | checked: `cargo about generate --fail` in CI and the release |
| The frontends never depend on matrix-sdk, and sign-in only through `SigninApi` | ADR 0001 | checked: `scripts/check-dependency-boundaries.sh` |
| Upstream pinned exactly and moved deliberately (GPUI tag, the Matrix crates, the harness images) | `Cargo.toml`, `docs/upstream/` | by design; Dependabot and the weekly Zed workflow open the pull requests |
| GitHub Actions pinned by commit SHA | `.github/workflows/` | by design |
| Fuzzing the link parser and the sanitiser | `fuzz/` | checked: CI's `fuzz` job (cargo-fuzz on a pinned nightly, 60 s per target); longer runs by hand: `cd fuzz && cargo +nightly-2026-09-19 fuzz run signin_link` |

## Privacy

| Control | Where | Proof |
|---|---|---|
| No analytics, crash reporting, rageshake or remote logging | everywhere | by design; Settings → Diagnostics says nothing is uploaded and copies only the version, the GPUI pin, the OS and the architecture (`view_tests/settings.rs` checks the copy names nobody) |
| Read receipts and typing notices can be turned off; with receipts off the read position is private (`m.read.private`) | `familychat-actions` (ADR 0015) | checked: `familychat-actions` tests, `view_tests/settings.rs` |
| Notifications only to the local notification centre | `familychat-ui` `notifications` | by design; manual check per OS (ADR 0013) |

## Manual checks per release

- The screen-reader script in `docs/accessibility.md` on each OS.
- A real sign-in with a code against Family Chat staging (`familychat.dev`, a `staging` build) and one against another Matrix server.
- Notifications posted and clicked on each OS (ADR 0013).
- The terminal UI in a real terminal on each OS (ADR 0008).
