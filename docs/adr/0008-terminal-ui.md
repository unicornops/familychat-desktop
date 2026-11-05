# ADR 0008: The Terminal UI

## Status

Proposed with Phase 10 (issue #11); accepted when that pull request
merges. It records PLAN.md sections 5.2, 5.3, 5.5, 5.8, 5.9, 7 and 13.10
as built for reading. Writing, encryption and answering invitations in the
terminal are Phase 11 (issue #12), which adds to this ADR.

## Context

PLAN.md promises a second frontend: a terminal UI in Zmail's style that
shares every non-visual rule with the window. By Phase 10 the shared half
exists: `StartupApi` opens the profile, `SigninApi` signs in, `SessionApi`
streams rooms and timelines, and `familychat-core` holds the commands
table, the timeline's rows, the rich-text tree, the link rules and the
wording. Zmail's ADR 0007 already answered how a terminal UI should look
and behave (Pine and Alpine's frame, Zellij's copy, a terminal sanitiser
with a hostile corpus), and its `zmail-tui` crate is the code that does
it. The open questions were how much of that to copy, how the terminal
reaches the same startup, sign-in and session the window does without
depending on a Matrix crate, and what the terminal must never do that a
GPUI window cannot do at all: write an escape sequence a server sent.

## Decision

### The crate and its boundary

`familychat-tui` depends on ratatui and crossterm (the versions Zmail
uses), `familychat-core`, `familychat-security` (the terminal sanitiser and
safe file names; no GPUI and no Matrix crate) and Tokio's sync primitives
only. It never depends on GPUI, `familychat-ui`, matrix-sdk or
`familychat-signin`; `scripts/check-dependency-boundaries.sh` checks its
graph as before. Its test-only dependency on `familychat-test-support`
(the fixtures) is a dev-dependency, like the window's.

`familychat tui` (the app's `tui` feature, on by default) takes the
profile lock first, as every command does, so a second Family Chat on the
profile, window or terminal, stops with "Family Chat is already open with
this profile; quit it first" before the keychain or the store is touched
(`crates/familychat-app/tests/profile_lock.rs`). It then starts the
background runtime and the window's own `LiveStartup` over the lock, and
hands it to `familychat_tui::run` as an `Arc<dyn StartupApi>`: the keychain
locked or unreachable (Try again, `t`), the data's key lost (Start over,
`s`, asked first), a failure (Try again) and the ready profile are the
same states with the same sentences as in the window. When the terminal
UI ends, the accounts' syncs stop before the runtime does. Flags:
`--bell` (ring the bell for new messages), `--ascii` (ASCII drawing in a
UTF-8 terminal) and `--no-mouse` (leave the mouse to the terminal);
`FAMILYCHAT_TUI_COPY_COMMAND` names a program copies are piped to. The
settings screen of Phase 13 will keep them.

### The frame is Zmail's

Every screen has Zmail's bands: a reversed title bar (the app and version,
the screen's name, the signed-in Matrix ID and "Room n of m"), the body,
a status line (messages centred in reverse video, the sync state's label
on the right, in its tone) and a two-line command bar that pages with `O`
when it is full. `:` opens the palette on every screen whose keys are
commands. The mouse, the selection and the copy are Zmail's, which are
Zellij's: a drag selects inside one pane (the room list, the timeline,
the members) and copies when released, a double click takes a word and a
triple click a line, the wheel scrolls the pane under it, a click on the
bar runs its command, Shift leaves the mouse to the terminal, and a copy
goes out as OSC 52 (or through `pbcopy` on a local Mac, whose Terminal
has no OSC 52, or the copy command). A copied wrapped line comes back
whole, without the indent or quote bar it was drawn with. `NO_COLOR`
turns every colour off (bold, reverse and underline carry the meaning
instead) and a non-UTF-8 locale or `--ascii` draws with ASCII.

These files are copied from Zmail and keep its notice: `text.rs`,
`pane.rs` and `clipboard.rs` from `zmail-tui`, and `terminal.rs` with its
corpus from `zmail-security`; `palette.rs`, `view.rs`, `links.rs` and the
mouse and copy half of `app.rs` are adapted (`about.toml` lists them).

### Commands and keys come from the shared table

Every row of `familychat_core::commands::COMMANDS` is a terminal command
on the table's terminal key, with the table's name and description in the
palette (`commands::SHARED`). The terminal adds only what a window does
with its pointer and focus: Enter, Esc and `<`, the arrows and pages, Tab
and Shift-Tab between links and files, `l` for one by its number, `s` to
save, `y` to copy, `t` to try again, `v` for spoilers, `b` for the bell
(`commands::LOCAL`), and they never reuse a table key. A new table row
fails `every_table_command_has_a_terminal_command` until it is mapped or
listed in `commands::LATER` with the phase that brings it, so the
frontends cannot drift apart. In a text field (the link, the password
form, the save path) letters are text, `Esc` goes back and `^C` quits.

### The sign-in draws steps and decides nothing

The sign-in screens are the window's, in text: the welcome page (a code
from a parent first, then a password, then another Matrix server), the
code page and the flow's steps. Every rule, server and sentence is
`familychat-signin`'s (ADR 0006); the terminal sends `SigninAnswer`s:

- **A pasted link.** The terminal's bracketed paste hands the link over as
  one event, which never runs as keys; on the welcome or code page it
  starts `SigninStart::Link` at once, as the window's Paste does. Typed
  into the code page instead, it is masked and wiped once used. QR images
  are the window's (a terminal can't paste one).
- **The confirmation is always shown** before a code is used, and only `y`
  answers `Continue`, only while the step is `Confirm`: not Enter, not a
  paste, so a key pressed twice or a pasted newline can't redeem a code.
  `p` uses the password instead and `n` or Esc cancels. A test drives the
  fixture sign-in to prove nothing is redeemed before `y`, and the live
  test checks the front end's access log for it.
- **The password forms** (Family Chat's, and another server's) keep the
  fields the step pre-fills (the link's `account_provider`, never `hs`),
  mask the password and wipe it once sent, and hold Enter while a rate
  limit's countdown runs; nothing retries.
- **The browser** (Phase 14, ADR 0010). "Use another Matrix server" asks
  for the server first, then shows what it offers: Ctrl-B "Continue in
  your browser" where the server has OAuth or single sign-on, Ctrl-E to
  change the server. "Continue in your browser?" shows the whole address
  and only `y` answers `Continue`; the browser step that follows opens the
  address once with the system's opener (`o` opens it again, `c` copies
  it, for a browser on another computer), and nothing of the address's
  query is drawn. QR codes are the window's: the form says the server
  offers one, and `LinkDevice` ("Sign in a new device") is in
  `commands::LATER` (a QR code a phone reads from a terminal is later
  work).

### Rooms and timelines

The room list shows the filters (`1` to `4`), each room's name, unread
count, mention badge, the marked-unread dot, invitations with who sent
them, the latest message and its time. From 120 columns the list and the
open room sit side by side and the room follows the list's cursor, as
Zmail's preview pane follows its index; narrower, they take turns. The
room's header names the encryption state, and a room that is not
encrypted says so in a banner (PLAN.md, section 6.8). The members show
beside the timeline where there is room (`m`), and instead of it where
there is not. An invitation is read-only here: answering is Phase 11.

The timeline draws `familychat-core`'s rows (room changes folded, a
sender's messages grouped) with its wording for room changes, days and
times; its rich text as terminal styles (bold, italic, underline,
strike-through, inline code in backticks, headings, quotes with a bar,
lists with their markers, code blocks, rules; spoilers hidden until `v`);
replies, reactions, edits and send states. It is bottom-aligned, asks for
older messages when its top is on screen, and keeps the view where it is
while new messages arrive below. It is laid out once per width and
change, so the 10,000-event fixture costs nothing while it doesn't change.

The sync state is the status line's label and, when it has a sentence, a
banner above the rooms with Try again where a retry helps. The suspended
state (`M_USER_SUSPENDED`) is "Paused" with its own sentence, never an
error, and the terminal never retries it.

### Links, files and notifications

Every link and file in the open room is numbered (`[3]`). Opening one goes
through `links::classify`: a Matrix link opens the room in the app (or
names the person), an `https`, `http` or `mailto` address shows a dialog
with the whole address, never cut, what opening it does and the
look-alike warning, and opens (or copies) only on `y` (or `c`); anything
else is refused. Images and other files are named files (kind, name,
size, dimensions); `s` or Enter asks where to save one, starting in
Downloads with the safe name, asks before replacing a file, and saves it
through `SessionApi::save_file`. Nothing is opened, and no image is drawn.

A room whose notifying unread count goes up, other than the one being
read, puts "New message in ROOM from SENDER" in the status line, and rings
the bell with `--bell` (or `b`). Phase 9's notification stream (the push
rules, the per-room modes, hiding content) replaces this rule when it
lands; the status line and the bell stay.

### Terminal safety

Every string a server sends goes through
`familychat_security::terminal::sanitize` (or `sanitize_line` for one-line
fields) where it is drawn: room names, topics, previews and inviters,
display names and senders, message bodies run by run (so styles stay on
their text), link text and addresses, file names, reactions, the names
inside room changes, the sign-in's sentences (they can name a server or a
Matrix ID from a link), and session and media errors. It removes C0
controls but the newline, DEL, every C1 control, ESC and everything it
introduces (CSI, OSC, DCS, SOS, PM, APC and their 8-bit forms), and the
bidirectional and zero-width characters. The only escape sequences the
terminal writes are ratatui's drawing, the bell, and OSC 52 for a copy the
person made, whose payload is base64. No OSC 8 hyperlinks.

`familychat-security/tests/terminal_corpus.rs` is Zmail's corpus (cursor
moves, titles, OSC 52, OSC 8, DCS and tmux passthrough, kitty graphics,
8-bit introducers, bidi tricks, every code point to U+02FF, seeded
mutations and random text), checked by an independent reader of the
output. `familychat-tui`'s `tests/hostile.rs` draws the hostile fixture
(`FixtureSession::with_hostile_at`: escape sequences in room names,
topics, previews, an inviter, display names, bodies, a formatted
message's text and link, a reply, a file name, a reaction and a room
change) at three sizes, with the link and save questions open, and fails
on any control, escape, C1 or invisible character in any cell or on the
status line, and on any payload the attacks carry.

### Fixtures, snapshots and the live tests

`familychat_tui::fixture_views` names every screen (48 views) as data: how
the profile opens, the sign-in step, a signed-in account's sync and
rooms, and where the UI starts. `familychat launch-tui-fixture VIEW`
(`--list`) opens one in a real terminal, behind the app's `ui-fixture`
feature, which turns on the terminal UI's `fixture-views` feature and is
never in a release (`scripts/check-release-features.sh`).

The snapshot tests draw every view on ratatui's `TestBackend` at 80x24 and
160x48 with a fixed clock in UTC, three times: with colour, with
`NO_COLOR` (the text must be the same and the style map colourless) and
in ASCII (no box drawing or typographic character of ours). Each file in
`src/snapshots` holds the text, a map of the styles a cell at a time, the
`NO_COLOR` map and the ASCII text. `FAMILYCHAT_UPDATE_SNAPSHOTS=1 cargo
test -p familychat-tui` rewrites them. A test in `familychat-app` checks
that every view has both snapshots, and one in `familychat-tui` that
every view has a snapshot test.

`crates/familychat-app/tests/live_tui_signin.rs` drives the terminal UI's
own state, key by key and paste by paste, over the real `LiveSignin` and
`LiveSession` on a throwaway profile with the mock keychain: a code minted
on Palpo, pasted and used only after `y` (no login in the access log
before it), and a typed password. Each new account has an invitation to
a room whose name carries escape sequences, which the terminal shows
filtered. CI's quality job runs it with the other live tests.

## Consequences

- The window and the terminal share the startup, the sign-in, the
  session, the commands, the rows and the wording; what differs is only
  drawing and the terminal's own movement keys.
- Text snapshots review well in a diff, but they don't show what a real
  terminal makes of emoji, wide characters or colours. Each phase that
  changes the terminal records a manual check in a real terminal on each
  OS: macOS Terminal and iTerm2, Windows Terminal, and a Linux terminal
  (bracketed paste, the mouse and Shift-drag, OSC 52 or `pbcopy`, the
  bell, `NO_COLOR`, ASCII, 120 columns and narrower, resizing).
- A terminal without bracketed paste (the legacy Windows console) types a
  paste out as keys. On the code page that only fills the (masked) field;
  on the welcome page its letters would run as commands, so there press
  `c` first. The confirmation still stands between any link and its code.
- Emoji with a variation selector (`❤️`) are one cell wide to
  `unicode-width` and two in many terminals, so a row with one can end a
  cell late. Zmail has the same limit.
- New-message notices follow unread counts until Phase 9 adds the
  notification stream; the terminal UI then shows that instead.
