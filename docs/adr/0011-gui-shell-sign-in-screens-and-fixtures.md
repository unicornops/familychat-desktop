# ADR 0011: The Window's Shell, Sign-in Screens and Screenshot Fixtures

## Status

Proposed with Phase 5 (issue #6); accepted when that pull request merges.
It records PLAN.md sections 5.3, 5.10, 6.2, 6.7, 9 and 13.5 as built.
PLAN.md reserves ADRs 0007 (message rendering), 0008 (the terminal UI),
0009 (encryption) and 0010 (next-generation auth), so this is 0011.

## Context

Phases 3 and 4 put the Matrix sessions and every sign-in rule behind
`SessionApi` and `SigninApi`, with no frontend drawing them. Phase 5 gives
the window its shell and the sign-in screens, and the fixture and
view-test machinery every later GUI phase uses. Zmail has all of the
machinery (components, title bar, keymaps, palette, `launch-ui-fixture`,
the screenshot matrix, accessibility and hit-target tests), so the
question is mostly how to adapt it without letting sign-in rules or the
profile's states leak into the frontend.

## Decision

### The shell is Zmail's

`familychat-ui` copies Zmail's component set (`Button`, `IconButton`,
`Banner`, `Modal`, `Menu`, `TextInput`, `Toast`, `Tooltip`, `Kbd`,
`Avatar`, `Badge`, `Scrollbar`, `Popover`), its foundations (`styles` with
`UiColors`, the type and spacing scale, `PlatformStyle`, WCAG contrast and
the Windows and macOS high-contrast settings; `a11y`; `motion`;
`widgets`; `keymap`) and its window chrome (`TitleBar` and
`window_frame`, Zmail's ports of Zed's `platform_title_bar` and
client-side decorations). Each file keeps its header and says it was
copied; `about.toml` lists them and the Zed licences behind three of them.
Mail-only pieces (the gallery, split handles, select, skeleton rows) are
left out.

The window asks for client-side decorations everywhere but KDE (as Zmail
does), so GNOME on Wayland, which has no server-side title bar, gets
Family Chat's own; `FAMILYCHAT_WINDOW_DECORATIONS=server|client`
overrides it. macOS keeps its traffic lights over a transparent title bar;
Windows gets Zmail's caption buttons.

The bundled themes now pass the contrast test Zmail runs on its own: the
light theme's accent is teal-700 (`#0F766E`, 5.5:1 on white) instead of
teal-600, its success and error colours are a step darker, and the dark
theme's button and selection fills are darker teal with a lighter error
red, so every text and icon colour reaches 4.5:1 on every fill it is drawn
on.

### Commands: one table, one keymap, one palette

`familychat_core::commands::COMMANDS` is the commands table (PLAN.md
section 5.3): id, name, description, the window's default keys (GPUI
syntax, `secondary-` for Cmd or Ctrl), the terminal UI's key, a scope
(global, or sign-in) and an icon. The window defines one GPUI action per
command (`familychat::<id>`), binds it in `keymaps/default.json` (and
`macos.json` for Hide, Hide Others and Minimize, which have no terminal
equivalent and are listed as platform actions), builds the command palette
(Secondary+K, Zmail's palette over the table with the shared fuzzy
matcher) and the macOS menu bar from it. Tests fail when a command lacks a
key in either frontend, when the keymap binds a command to other keys than
the table's, or when an action is in neither.

### Opening the profile: `StartupApi`

`familychat-ui` cannot depend on `familychat-store` (its encryption crate
is a `matrix-sdk-*` crate, which the dependency-boundary check refuses),
so `familychat-core` gains `startup::StartupApi`: `open()` and
`start_over()` answer on a oneshot with an `OpenState` (ready, with the
`SessionApi` and `SigninApi` to draw from; keychain unavailable; unreadable;
failed), each carrying the store's own sentence.

The app's `LiveStartup` does the work on the background runtime: the
keychain and the store in a blocking task (`Profile::open_locked`, or
`retry`, or `start_over`), then `AccountManager::new` and `restore_all`,
then `LiveSession` and `LiveSignin::new(manager, ReqwestTransport,
handle)`. A profile that is not ready stays in `LiveStartup` with its lock,
so Try again and Start over carry on from it and no second process can
take the profile meanwhile. When the window closes the app stops every
account's sync before the runtime goes. The UI thread never touches the
keychain, the disk or the network.

The screens are Zmail's `store_unlock` shapes: Opening; "Family Chat can't
reach the keychain" with Try again (never a plaintext fallback);
"Family Chat can't open your data" with Start over, which says where the
old data went in a toast.

### The sign-in screens draw steps; they decide nothing

`SigninView` has three pages. Welcome offers the three ways in PLAN.md
section 6.2, the code first. The code page takes the link in a masked
field (the link holds the code, so it is not shown; `TextInput::take_secret`
moves the text out and wipes the field's undo history), or a QR code image
pasted (the Paste button, Secondary+Shift+V, or Secondary+V in the link
field when the clipboard holds an image or an image file) or dropped on
the window. Everything else is the current `SigninStep`:

- `Confirm` opens a modal dialog with the step's question ("Sign in as
  @ana:smith.safechat.family?"), its detail, the account and Continue,
  Cancel and "Use my password instead". Continue is the only place the view
  sends `SigninAnswer::Continue`, and it does nothing unless the step is
  `Confirm`; while the dialog is open the other sign-in commands and drops
  are ignored. Escape and Cancel cancel the flow.
- `Working` shows the step's sentence with a spinner.
- `Password` fills the form from the step (the server comes from
  `account_provider` for a link, never `hs`, because the sign-in decides
  that), shows its notice and problem (the used-or-expired sentence,
  a refusal), and counts a rate limit down from
  `SigninProblem::retry_after`: the button says "Try again in N s" and
  nothing is sent until the wait is over. The sign-in itself never
  retries.
- `NotALink` returns to the code page with its sentence; `Cancelled` to
  the welcome page; `AlreadySignedIn` says so; `Finished` tells the shell,
  which shows the account.

Problems are read out once by a live region; the countdown is visible but
not announced every second.

### QR images are read here, bounded, off the UI thread

`qr::decode_signin_qr` decodes the image with `image` (PNG, JPEG, GIF, BMP,
TIFF, WebP: formats already in the graph through GPUI) and finds the code
with `rqrr` (PLAN.md section 4), on GPUI's background executor. The file
is refused above 40 MiB, the image above 8192 pixels a side or 512 MiB of
pixels; screenshots larger than 2000 pixels are scaled down before the
scan. The bytes and the grey pixels are wiped after the scan, and the text
goes straight to `SigninStart::Link`, where every rule of the contract
applies; it is never put in the field, logged or kept. Webcam scanning is
later work.

### Signed in

Until the room list (Phase 6), the window shows the account, its sync
state in words (`SyncState::label` and `message`, in `familychat-core` so
the terminal UI says the same) and Try again for the states that wait for
one. The title bar has a sync indicator; Suspended is "Paused" with a
pause icon in the info colour, never an error or a retry.

### Fixtures, `launch-ui-fixture` and the screenshot matrix

`familychat-test-support`'s `fixtures` feature has `FixtureSession`,
`FixtureSignin` and `FixtureStartup`. They speak the real sentences (the
sign-in's `wording` and the store's `keychain_unavailable_message` and
`unreadable_message`), never touch the network, the keychain or the disk,
and the sign-in fixture follows the flow's shape (it redeems a code only
after `Continue` on a confirmation) and records every start and answer
without their secrets.

`familychat launch-ui-fixture VIEW [dark|light]` opens the window on a
fixture view (`--list` names them). It is behind the app's `ui-fixture`
feature, off by default, because it pulls `familychat-test-support` in:
`scripts/check-release-features.sh` fails if the release graph reaches that
crate or if the feature becomes a default, and self-tests that it sees the
feature. `scripts/capture-ui-screenshots.sh --matrix` (adapted from
Zmail's `capture-release-screenshot.sh`) captures every view in dark and
light, and three at the minimum width, under Xvfb with openbox, xdotool,
ImageMagick and lavapipe; a test in the app checks the matrix against the
views. CI's UI Screenshots job builds the release binary with the feature
and uploads the `ui-screenshots` artefact on pull requests that touch the
UI.

### View tests

`#[gpui::test]` tests open the real `ShellView` over the fixtures: every
screen's controls have accessible names (Zmail's `audit`), every pointer
target is at least 24 by 24 pixels (Zmail's `hit_target` walk), the
confirmation always comes before a code is redeemed (Enter, Space, the
sign-in keys and a paste on the dialog redeem nothing; Cancel, Escape and
"Use my password instead" redeem nothing; Continue redeems once, after the
confirmation was shown), a pasted QR image and a dropped image file decode
to the fixture link, text that is not a link returns to the code page, a
used code falls back to the password form, a rate limit holds the form,
Try again and Start over reach the sign-in, the palette runs the sign-in
commands, and a suspended account reads "Paused".

## Consequences

- Every later GUI phase adds its screens to the fixtures, the screenshot
  matrix (in dark and light) and the accessibility and hit-target tests.
- The window is the first frontend to sign in. The terminal UI (Phase 10)
  draws the same steps and the same commands table.
- Release builds cannot reach the homeserver harness (its CA and its
  `*.familychat.test` names are test-only, ADR 0004), so the manual
  sign-in checks run against Family Chat itself (or a `staging` build
  against `familychat.dev`) and another Matrix server.
- Pasting an image needs the platform's clipboard image support: GPUI reads
  images from the clipboard on macOS, Windows and Linux (X11 and Wayland);
  a copied image file is read from its path.
