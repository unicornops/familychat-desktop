# ADR 0015: Settings, Accessibility and Hardening

## Status

Proposed with Phase 13 (issue #14); accepted when that pull request
merges. It records PLAN.md sections 5.4 (sign-out), 5.10, 7, 8, 9 and
13.13 as built. ADR 0010 is reserved for Phase 14 and 0014 is Phase 12's,
so this is 0015.

## Context

Phases 5 to 10 left a window that signs in, reads, writes and encrypts,
with one settings dialog (whether notifications show the text) and the
encryption dialogs reachable only from the command palette. What was
missing for a finished app: one place for the settings, a way to sign out
that leaves nothing behind, the accessibility checks Zmail does, and the
checks that the app keeps its promises (budgets, no egress, hostile input).

## Decision

### The Settings window is Zmail's

`familychat-ui`'s `settings` module is adapted from Zmail's
`src/settings.rs` (the header says so; `about.toml` lists it): a window of
its own, titled "Settings" (the screenshot script finds it by that title),
one per app, opened with Secondary+, (a context-less binding handled
app-wide, so it works from the Settings window and the macOS menu bar),
the title bar's settings button, the command palette, or `OpenSettings`
in the macOS app menu. "Notification settings" (Secondary+Alt+N) now opens
it at Notifications; Phase 9's dialog is gone. A section list on the
leading side (a vertical tab list: Up, Down, Home and End move while it has
focus) and the open section beside it, Zmail's setting rows, group
headings and scrolling. Escape or Secondary+W closes it when no dialog is
open.

The main window installs a `SettingsHost` global once the profile opens
(the `Opened` session and settings, the bootstrap, the main window's
handle and the account it shows); the Settings window draws from the same
services:

| Section | What it holds |
|---------|---------------|
| Account | the account (name, user id, homeserver, this device's name and id). A Family Chat account (`SessionApi::account_details().family_chat`, which the app's wiring decides with the sign-in's own allowlist, `Allowlist::is_family_chat`, so the rule stays in `familychat-signin`) says that a parent manages it in the panel (resets a password, gives a new sign-in code, deactivates it) and offers **Open the Family Chat panel…**, which goes through the same link question as every link out of the app (`links::classify` and the rooms' dialog, now `rooms::link_prompt_dialog`); another server's account says to use that server's tools. Nothing in the app changes a password or deactivates an account. **Sign out…** |
| Sessions | the account's devices: the devices view of ADR 0009 drawn in the page (`EncryptionView::render_device_list`), with Verify and Sign out (the password when the server asks), refreshed when the page opens |
| Security & privacy | recovery (set it up, or **Change recovery key…**), room key export and import, **Reset identity…**, and the privacy switches: read receipts, typing notices and notification text |
| Appearance | theme (match system, light, dark), text size (100% to 200% in 25% steps), motion (match system, reduce, full) and contrast (match system, increased) |
| Notifications | whether notifications show at all, and whether they show the text |
| Diagnostics | the version, the GPUI pin (Zed tag and commit), where the logs and the crash log are, **Open logs folder** (the OS opens the folder; nothing is read), **Copy version details** (version, GPUI, OS and architecture: nothing that names a person) and the sentence that nothing is uploaded |

The Settings window holds an `EncryptionView` of its own over the account,
built `inline()`: its dialogs (verification, recovery, the key shown once,
signing a device out, export, import and the two resets) open in the
Settings window, and a device's sign-out returns to the page rather than
to a devices dialog. It follows the account's encryption status like the
main window's; verification requests from other devices still open in the
main window, which listens for them.

### Settings are three watches

`familychat-core`'s `settings` has three groups, each a `watch` with a
save: `NotificationSettings` (`enabled`, new, and `show_content`),
`PrivacySettings` (`read_receipts`, `typing_notices`) and
`AppearanceSettings` (`ThemeChoice`, `text_size`, `MotionChoice`,
`ContrastChoice`). They are the app store's plaintext preferences
(`notifications`, `read-receipts`, `typing-notices`, `theme`, `text-size`,
`motion`, `contrast`, as `on`/`off` or the choice's name; ADR 0003: they
identify nobody), read when the profile opens and written on the blocking
pool by `LiveSettings`. A value that doesn't parse is the default; a text
size snaps to the nearest step. `MemorySettings` has them for fixtures,
with `refuse_saves` for the "couldn't save" path.

The Settings window shows a change at once and saves it; when the profile
refuses, the change is taken back and the sentence is a toast. The main
window follows the same watches and applies them to every window: the
theme (a choice overrides the OS appearance; "match system" is the OS's,
or the fixture's pinned one), `TextScale` (the rem size `window_frame`
sets, so text, rows, buttons and dialog widths grow), `motion::apply`
(kept, so a later read of the system setting applies the choice too) and
`ContrastSetting::prefer_increased` (macOS Increase Contrast's palette on
every platform; a Windows contrast theme still wins). With notifications
off, the notifier is not asked at all; the title still counts.

### Read receipts and typing are honoured by the actions

`familychat-actions`' `Actions::with_privacy(watch)` follows the privacy
settings: with read receipts off, `mark_read` asks for
`RoomRequest::MarkReadPrivately` (the read marker and an `m.read.private`
receipt, so the account's other devices and the unread counts stay right
and nobody else sees it; Element's behaviour); with typing notices off,
`typing` sends nothing at all. Both frontends' rooms use `Actions`, so the
terminal UI gets the same rule when it writes (Phase 11) by passing the
same watch.

### Signing out removes everything

`SessionApi::sign_out` (`AccountManager::sign_out`):

1. the account leaves the open accounts and `accounts()` at once (the
   window goes back to the sign-in, or to the next account);
2. its sync stops;
3. the server is told (`Client::logout`, within 10 seconds): best effort,
   because an account must be removable from a computer whose server is
   gone or refuses. The answer says which (`SignedOut::Everywhere` or
   `OnlyHere`, with a sentence that says to sign the device out from
   another device's sessions);
4. its keychain entries (`account-<uuid>-session`, `-store`), then its
   registry row and its drafts, then `accounts/<uuid>/` (the SDK's state
   and crypto stores, the event and media caches). `Profile::remove_account`
   now deletes in that order, so the secrets go first and anything left
   behind is ciphertext whose key is gone. Files another process still has
   open (Windows) are retried for five seconds, then left to
   `Profile::remove_orphaned_account_dirs`, which `restore_all` runs at
   every start.

The Settings window asks first ("Sign out of …?"), says what goes, warns
when nothing else could bring the encrypted history back (no recovery and
no other confirmed device), then closes; the main window says how it went.
`tests/sign_out.rs` checks it against a mocked server with `MemoryKeychain`
and a temporary profile: a server that signs the device out, one that
refuses, one that can't be reached, and the sweep at the next start.

### The two Phase 8 follow-ups

- **Changing the recovery key** (`SessionApi::reset_recovery_key`,
  `Recovery::reset_key`; recovery is set up instead when there is none):
  the new key is shown once in the same dialog as the first, with Copy,
  Save and Done once kept.
- **Resetting the identity** (`SessionApi::reset_identity`,
  `Recovery::reset_identity`): for an account with no confirmed device and
  no recovery key. A warning first; the server's user-interactive auth is
  answered with the password when it asks (`AuthOutcome::PasswordNeeded`
  shows the field), the password used once and wiped. A server that wants
  approval in a browser (OAuth) gets a sentence saying the app can't do
  that yet (Phase 14).
- **Send anyway is reviewed**: the timeline's Send anyway opens a dialog
  that lists what the message would go to (`SessionApi::stopped_message`:
  the unconfirmed devices, the account's own first, with their owners' and
  devices' names, or the people whose identity changed), with Cancel and
  Send anyway; only that button sends, through `Actions::send_anyway`.

### Accessibility to Zmail's baseline

`docs/accessibility.md` is the checklist (what ships, what doesn't, and
the manual screen-reader script per OS). The automated part: every
Settings section and dialog is in the screenshot matrix in dark and light
and in `view_tests/settings.rs`, which checks every control's accessible
name and every pointer target at 24 px (at 100% and 200% text), as
`view_tests/a11y.rs` and `hit_targets.rs` do for the other screens.

### Performance budgets in CI

`view_tests/perf.rs::performance_budgets` (ADR 0007's benchmark) runs in
CI's `performance` job in a release build on Ubuntu, with the tables in
the step summary and the `performance` artefact. Shared runners are noisy,
so the job fails only when a median is over `FAMILYCHAT_PERF_GATE` (3)
times its budget: 25 ms for a scroll frame, 300 ms for an action. Locally
the medians are 2 to 10 ms.

### The egress test

ADR 0004's "Egress" section: the live tests run through a recording proxy
(`scripts/egress-proxy.py`) that the harness client and any client
honouring the proxy variables go through, the harness resolver records the
names it refuses, and `scripts/check-egress.sh` fails the quality job on
any host in the proxy log, the resolver's refusals or the front end's
access log that is not a harness homeserver, or on a proxy log with
nothing in it. `--self-test` is the negative test, run first: an
unexpected host must fail the check.

### Fuzz smoke: cargo-fuzz on nightly

`fuzz/` (outside the workspace, with its own lock file) has two libFuzzer
targets: `signin_link` (`LoginLink::parse` and `ServerName::parse`, with
the invariants the sign-in relies on: a code is kept only for an
allowlisted host, a name parses back to itself) and `matrix_html` (the
sanitiser, the rich-text tree and `message_text`, with the hostile corpus's
markup and tree checks and the caps). CI's `fuzz` job builds them with
nightly (pinned to the one checked, `nightly-2026-09-19`) and runs each for
60 seconds from the seeds in `fuzz/corpus/`. Nightly is needed for
coverage-guided fuzzing with AddressSanitizer (`-Zsanitizer`); a bounded
proptest run on stable would find far less in a minute, and the proptest
properties the sign-in already has keep running in the workspace tests.
The cost is the build: `familychat-signin` pulls matrix-sdk in through
`familychat-session`, so the first build is long (13 minutes locally),
and the job caches its target directory.

### `docs/hardening.md`

The security checklist: each control of PLAN.md section 7 and 8 with
where it is enforced and which test proves it, and the known gaps.

## Consequences

- A new setting is a field in one of the three groups, a preference key,
  a row in a section, a fixture view and the section's view test.
- The terminal UI has no settings page yet (`OpenSettings` and
  `NotificationSettings` are in its `LATER` list); it reads the same
  settings once it has one.
- Signing out can leave the device in the account's session list when the
  server can't be reached; the app says so, and the device can be signed
  out from another one.
- Links inside message text are still pointer targets only (ADR 0007);
  reaching them from the keyboard is a follow-up, as it is in Zmail.
- Screen-reader support is checked by hand per OS (`docs/accessibility.md`);
  GPUI's test platform never builds the accessibility tree.
- The fuzz job's nightly is moved by hand; a new nightly that can't build
  the targets fails that job alone.
