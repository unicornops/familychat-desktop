# ADR 0008: The Terminal UI

## Status

Accepted with Phase 10 (issue #11), which built the terminal UI for
reading (PLAN.md sections 5.2, 5.3, 5.5, 5.8, 5.9, 7 and 13.10). Phase 11
(issue #12) adds writing, invitations and encryption (PLAN.md section
13.11 and decision 10): the sections "Writing", "Encryption" and "What
the terminal leaves to the window" below, accepted when its pull request
merges.

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
UTF-8 terminal), `--no-mouse` (leave the mouse to the terminal) and
`--editor` (the external editor, "Writing" below);
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
save, `y` to copy, `t` to try again, `v` for spoilers, `b` for the bell,
and from Phase 11 the composer's Enter, `^J` and `^_`, `a` and `x` for an
invitation, `S` for Send anyway, `w` for an identity warning, and `d`,
`r` and `s` on "Confirm it's you" (`commands::LOCAL`), and they never
reuse a table key. Decision 10's `O` for a link stays `l`: `O` pages the
command bar, as in Zmail. A new table row
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
there is not. An invitation is answered in the room ("Writing").

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

`familychat_tui::fixture_views` names every screen (78 views) as data: how
the profile opens, the sign-in step, a signed-in account's sync and
rooms, its encryption, a verification, identity warnings, a message the
encryption stopped, the upload limit, and where the UI starts (a room,
then commands run and keys typed, one step a tick once the answers to the
last have arrived). `familychat launch-tui-fixture VIEW`
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
filtered. `crates/familychat-app/tests/live_tui_writing.rs` (Phase 11)
drives it the same way against a test-support device, a second
`LiveSession` on a profile of its own driven through `SessionApi`: the
terminal joins an encrypted room it was invited to with `a`, sends a
message the other member's device reads decrypted, shows the answer,
replies to it (`[`, `R`; the other side sees the reply's target) and
reacts to it (`+`, `1`); and a new device in the terminal confirms
itself by emoji with the account's first device ("Confirm it's you", `d`,
`e`, `y`, the seven emoji and their names checked on screen against the
other device's), then verifies a third device that asks it to. CI's
quality job runs both with the other live tests.

### Writing

The composer is Zmail's compose editor: `ratatui-textarea` with Zmail's
settings (no cursor-line underline, word wrap, only plain keys reach it)
under the timeline, one message long, growing to six lines. It follows
PLAN.md decision 10: opening a room the person can write in gives the
composer the keys, so letters are text; Esc hands them to the room's
commands, where the table's single letters work (`[` and `]` select a
message, `R` reply, `E` edit, `+` react, `D` delete, `A` attach, `L`
leave, `i` back to the composer), and Esc again goes back to the list.
In the composer, Enter sends; `^J`, Alt-Enter and (where the terminal
tells it apart) Shift-Enter start a new line; Up in an empty composer
edits the account's newest message; the table's Ctrl keys still work.
Nothing in the composer decides a rule: every change to a room is
`familychat-actions`' (ADR 0012), whose tasks run to their end on a
short-lived thread each, parked between wake-ups (`tasks.rs`; the
terminal has no async runtime, and `send_file` reads and cleans a file
inside its task), and whose events arrive on its channel on each tick.

- **Markdown and mentions.** What is typed is sent as typed; the actions
  turn it into Markdown with the HTML the sanitiser keeps. `@` lists the
  room's members (the actions' `mentions`), Up and Down move, Tab or
  Enter picks, and the mention goes in `m.mentions`.
- **Replies, edits, reactions and deletions** are about the selected
  message, or with none selected the newest one that fits, as in the
  window: a reply shows "Replying to …" over the field, an edit puts the
  message's text in it, Esc cancels either. `+` opens the quick picker
  (the window's six; `1` to `6`; names instead of emoji in ASCII). `D`
  needs one of the account's own messages selected and asks first, in a
  dialog only `y` answers. A selected message the encryption stopped
  shows why, and `S` sends it anyway (ADR 0009).
- **Files by path.** `A` asks for a path, starting in Downloads, with Tab
  completing as far as the names in that folder agree (and listing them).
  The file is checked against the server's upload limit by its size
  before anything is read (`file_allowed`); one over the limit says so
  and never waits. A file waits under the field until the message goes
  (Backspace in an empty field takes the last one back), and goes through
  `send_file`, which takes a picture's location out.
- **Invitations.** An invitation's room says who sent it; `a` joins and
  `x` declines, through the actions. Once joined, the room opens again
  with its timeline and composer (from All, when the list was showing
  Invites).
- **Paused.** While the account is suspended the composer has no field:
  "Chat is paused" and the actions' sentence, read-only, and the room's
  commands have the keys without the writing ones. The actions refuse
  every write anyway.
- **Drafts, typing and receipts** are the window's rules: the draft is
  read when a room opens and kept a second after typing pauses (and when
  the room is left, never for an edit); typing notices go at most every
  few seconds and stop after five quiet ones or a send; the read receipt
  moves when the newest message is on screen, at most every two seconds.
- **Text from outside the keys** (a paste, a draft, an edited message's
  old text, the external editor's text, a member's name) goes through
  `terminal::sanitize` before it reaches the editor.
- **The external editor** is Zmail's and off unless `familychat tui
  --editor`: `^_` hands the message to `$VISUAL` or `$EDITOR` (split as a
  copy command is, no shell) on a file in a fresh 0700 folder (the file
  0600), which is overwritten with zeros and removed with anything the
  editor left beside it however the editor ends; the text comes back
  filtered. It is opt-in because the message sits unencrypted on disk
  while the editor is open. `editor.rs` is copied from Zmail.

### Encryption

The terminal draws the session's `EncryptionStatus` and
`VerificationFlow`s and sends the answers back, as the window's
`EncryptionView` does (ADR 0009), with the same wording:

- **Confirm it's you** takes the rooms' place while this device isn't
  confirmed: `d` another device (comparing emoji), `r` the recovery key
  (when the account has recovery), `s` (or Esc) skip, after the warning,
  which only `y` accepts. Skipped, a banner over the rooms says the device
  isn't confirmed, and `V` (ConfirmIdentity) brings the screen back. A new
  reason to confirm (the identity reset elsewhere) asks again.
- **The recovery key and the password** the server may ask for before the
  identity is made are typed (or pasted) into masked fields, held as
  `SecretField`s (wiped when taken or dropped) and handed on as
  `SecretText`, used once and never drawn, kept or logged. A key that
  doesn't open recovery says so, and the field starts again.
- **Verification** is a dialog over the body: waiting for the other
  device, a request from one (`y` accepts, `n` declines; Enter answers
  nothing), ready, the seven emoji with their names under or beside them
  (in a row where the dialog is wide enough, one a line where not, and
  names only in ASCII), confirming, done, or why it stopped (`y` they
  match, `n` they don't, Esc cancels). Requests from the account's other
  devices reach the terminal through `verification_requests`, which it
  listens on from the moment the profile opens (so the session no longer
  declines them while it is open); one at a time, a newer one while a
  verification is open is declined, as in the window. No QR code is
  drawn: a terminal's cells don't make a code a phone reliably reads, so
  "ready" offers the emoji, which work with every device.
- **Recovery** (`^R`, SetUpRecovery, and the banner offering it) asks
  first, then shows the new key once, in the dialog: `c` copies it (OSC
  52, the copy wiped from memory once written) and Enter then closes, or
  `w` says it was written down; the key is dropped (wiped) when the
  dialog closes.
- **Banners and the timeline.** The identity couldn't be made (`t` tries
  again), recovery isn't set up (`^R`), and each identity warning of the
  open room (`w` answers the first: OK, or withdraws the old
  verification). Unable-to-decrypt messages say why with `UtdReason`'s
  sentences, and a message the encryption stopped says
  `SendProblem`'s.

### What the terminal leaves to the window

`commands::LATER` lists what the shared table has that the terminal
doesn't offer yet, each with when: the notification settings, the devices
dialog (verifying another device, signing one out with the password the
server may ask for) and room key export and import (a passphrase typed
twice and a file chosen) wait for the terminal's settings screen in Phase
13. A verification QR code is the window's (above).

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
- The composer has the keys in an open room, so a single-letter command
  there is an Esc away; the command bar always shows which keys are live.
- Shift-Enter reaches the terminal UI only where the terminal reports it
  (the kitty keyboard protocol, Windows Terminal); `^J` and Alt-Enter
  start a new line everywhere.
- The emoji comparison shows each emoji's name too, so a terminal whose
  font draws an emoji differently, or not at all, can still compare. A
  phone that only scans a code verifies with the window, or compares
  emoji.
