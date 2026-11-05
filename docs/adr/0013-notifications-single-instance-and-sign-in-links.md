# ADR 0013: Notifications, Single Instance and Sign-in Links

## Status

Proposed with Phase 9 (issue #10); accepted when that pull request
merges. It records PLAN.md sections 5.5, 5.9, 6.7 and 13.9 as built.
PLAN.md reserves ADRs 0008 to 0010, and Phase 7 uses 0012, so this is
0013.

## Context

Phases 4 and 6 left a window that signs in and reads rooms. Two things a
chat app needs were missing:

- telling people when someone writes, without a push gateway (PLAN.md
  section 6.9: notifications arrive while the app runs), without sending
  message text anywhere but the OS's own notification centre, and without
  burying the desktop in notifications when a busy room catches up;
- opening a sign-in link (`familychat://safechat.family/app/login?...`, or
  the universal link) in the window that is already running. The profile
  lock (ADR 0003) refuses a second process outright, which is right for
  the crypto store but wrong for a link: the OS starts a second process
  with the link, and that process must hand it over and get out of the
  way without the token sitting in a log, a file or the process list for
  longer than it has to.

The link carries a single-use bearer credential for someone else's
account, usually a child's (ADR 0006). Anything that moves it between
processes is part of the sign-in's attack surface: another local user
must not be able to receive it, inject one, or read it from a log.

## Decision

### Notifications come from the session, decisions from `familychat-notify`

**The producer** is `familychat-session`'s `notifications` module: on every
account's client, before its first sync, it registers matrix-sdk's
notification handler, which the SDK calls during sync for each event its
push rules say to notify about (`Action::should_notify`), after decrypting
it where the account has the keys. The room's notification mode and the
account's keywords are therefore the account's push rules, as every Matrix
client sees them. The handler leaves out:

- the account's own messages (from any of its devices);
- events sent more than a minute before the session started (the grace is
  for clocks that disagree): a sync that catches up after a restart shows
  its unread badges, not a notification storm;
- an event already reported (the last 512 event ids, and one per
  invitation): sliding sync can send a room's latest events again when the
  room is subscribed.

What is left becomes `familychat-core`'s `IncomingNotification`: the
account, the room, the room's name, the sender's display name, whether it
is a direct chat, and `NotificationContent` (the plain text of a text,
notice or emote, with an edit's new text and without a reply's quoted
fallback; or the kind: picture, video, audio, file, location, sticker,
invitation; or unknown, which includes a message the account could not
decrypt). Its `Debug` prints only the kind. `SessionApi::notifications()`
returns a bounded receiver (64); one frontend listens, a new call replaces
the receiver, and the session drops a notification rather than queue it
when the frontend is behind. `SessionApi::unread_total(account)` is a watch
of the account's notifying unread messages over its joined rooms (the
room list's badges, read-receipt based) plus one per invitation, spaces
left out, recomputed on every sync's room updates.

**`familychat-notify`** (no GPUI, ratatui or matrix-sdk, checked by
`scripts/check-dependency-boundaries.sh`; adapted from Zmail's
`zmail-notify`) decides what the OS shows. `Notifier::notify(batch,
focused, now)` takes the notifications that arrived together and the room
the person is looking at:

- **The focused room**, the open room of the window while the window has
  focus (on a narrow window only when the room, not the list, is in
  front), gets nothing, and counts as seen. With the window in the
  background every room notifies, the open one included.
- **Per-room coalescing.** One notification per room, with an opaque tag
  (`familychat-room-<n>`) that the next one for the room reuses, so the OS
  replaces it; it counts what arrived since the room was last seen ("3 new
  messages", then the latest line). Opening the room, clicking its
  notification, or the window gaining focus with the room in front resets
  the count and takes the notification down (`room_seen`).
- **Wording.** A group room's notification is titled with the room and
  says "Ana: See you at six"; a direct chat's is titled with the sender.
  Media say "Sent a picture" and the like, an invitation "Ben invited you".
  Text is made safe to show: control characters, bidi overrides and
  isolates and zero-width spaces dropped (the zero-width joiner kept for
  emoji), whitespace folded, cut to 140 characters (names to 64).
- **Hidden content.** With "Show message text in notifications" off, the
  text is "New message" ("New invitation"); the sender and the room still
  show (PLAN.md section 5.9).
- **Zmail's flood cap**: at most six notifications in a rolling minute.
  Zmail posts every batch over the cap with its detail collapsed; Family
  Chat posts one summary instead ("Messages are arriving quickly. Open
  Family Chat to read them.", tag `familychat-flood`, opening the latest
  room) and nothing more until the minute frees up, so a flood can't
  stack notifications at all. The counts keep running meanwhile.
- **Clicks.** The tag maps back to the room in the notifier (the last 64
  rooms); no Matrix id reaches the OS's notification store.

`window_title(unread)` is the window's title: "Family Chat", or "Family
Chat (3)".

**The window** (`familychat-ui`'s `notifications`, adapted from Zmail's
`zmail-ui`) posts through GPUI's system notifications
(`UNUserNotificationCenter` on macOS, toast notifications on Windows, the
freedesktop notification service on Linux), under the app id GPUI is
given at start. The shell waits 150 ms after the first notification of a
burst so the rest of one sync arrives too, then hands the batch over with
the focused room. A click (`on_system_notification_response`) brings the
window forward and opens the room through `RoomsView::open_notified_room`
(from the list, or from what the notification said for a room past the
list's first page); a click on a notification the notifier doesn't
remember only brings the window forward. The title follows
`unread_total`. System notifications are the window's only; the terminal
UI (Phase 10) has its own status line and bell for new messages, and can
move onto the same notifier later.

### The setting is a plaintext preference

Whether notifications show the text is the app store's plaintext
preference `notification-content` (`show` or `hide`; ADR 0003: it
identifies nobody), default show. `familychat-core` gains `settings`
(`NotificationSettings`, the `SettingsApi` trait with a watch and a save,
and `MemorySettings` for fixtures), `Opened` carries a `SettingsApi`, and
`familychat-session`'s `LiveSettings` reads the preference when the profile
opens and saves on the blocking pool. Until Phase 13's settings screens,
the window has one dialog for it: **Notification settings**, a command in
the commands table (Secondary+Alt+N, in the palette and the macOS app menu;
the terminal UI keeps `ctrl-o` for it and lists it under `LATER` until
Phase 13) with a checkbox and what each choice
means. (Phase 13 replaced the dialog with Settings → Notifications, which
also turns notifications off; ADR 0015.)

### Single instance: the lock, plus a user-only socket or pipe

The lock stays the one rule (one process per profile). The window that
holds it also listens on the profile's endpoint (`familychat-instance`, a
new crate with no GPUI or matrix-sdk):

- **Unix**: a Unix socket in a directory only the user can enter:
  `$XDG_RUNTIME_DIR/familychat/` on Linux (the runtime directory is the
  user's own, 0700, by the XDG specification), `$TMPDIR/familychat/` on
  macOS (the per-user directory under `/var/folders`), or the profile
  directory itself (0700, ADR 0003) when there is no runtime directory. It
  is named after the profile's canonical path (FNV-1a, stable across
  builds), so profiles don't collide. The window makes the directory 0700
  (refusing one it can't narrow), replaces a socket left by a crash (it
  holds the lock, so nothing else listens), binds, makes the socket 0600,
  and checks the socket and the directory have the same owner. A path
  over 100 bytes is refused (Unix socket paths are short).
- **Windows**: a named pipe whose security descriptor is owned by the
  current user and has a protected DACL with one entry, the current
  user's SID with full access (`O:<sid>D:P(A;;GA;;;<sid>)`): no inherited
  entries, no Everyone, Administrators or SYSTEM. It is created as the
  name's first instance and refuses remote clients. Pipe names are one
  namespace for the whole machine, so the name is random
  (`family.safechat.desktop-<128 bits>`) and written in the profile
  directory (`familychat.pipe`, through a temporary file), which only the
  user can read: another user can neither open the pipe nor take its name
  first and wait for a link. The SID lookup and the descriptor need Win32
  calls with no safe wrapper in the graph, so `src/windows_security.rs` is
  the workspace's only `unsafe` code (PLAN.md section 5.1): the crate's
  lints are the workspace's with `unsafe_code = "deny"`, that module
  allows it, each block says why it is sound, and every allocation is
  freed by a guard. `windows-sys` 0.61 was already in the graph.

**The wire** is PLAN.md's length-prefixed JSON: a big-endian `u32` length
and `{"v":1,"link":"..."}` (or `{"v":1}`, come forward), answered with
`{"v":1,"accepted":true|false}`. The window refuses, delivering nothing:
a frame over 8 KiB (from its length alone), anything but exactly that
object (unknown fields, an array, another version), a link over 4 KiB or
not shaped like a sign-in link (`familychat:` or
`https://safechat.family/`, no control characters), and a request that
doesn't arrive within 2 seconds. It serves four second launches at a time.
The app then runs the sign-in's own parser (`LoginLink::parse` with the
build's allowlist) and declines what it refuses; the window starts a
sign-in with the rest, where every rule of the contract applies again.
Errors carry none of the message, the buffers a link passed through are
wiped, and the listener logs only what came of a connection.

**A second launch:**

- `familychat LINK`, how an OS URL handler starts it (registered in Phase
  12): the link is checked with the sign-in's parser first (refused
  without being echoed), then forwarded; the process prints that the
  sign-in continues in the window and exits 0. PLAN.md section 12's risk
  stands: on Linux and Windows the link is in `argv`, briefly visible to
  the same user's processes, so the process forwards it at once and logs
  nothing (it never takes the lock, so it never opens the profile's log).
- `familychat open-link` reads one line (at most 4 KiB) from standard
  input, asking first on a terminal, so a token never has to be in the
  shell's history or the process list.
- `familychat` with nothing asks the window to come forward.
- When nothing listens (the terminal UI has the profile, an older window,
  or the window is still starting after two seconds of trying), it says
  "Family Chat is already open with this profile", as before. The
  terminal UI doesn't listen, and a second terminal UI is still refused.
- Text that looks like a link but isn't one is never echoed in an error.

When the profile is free, `familychat LINK` and `open-link` become the
window and start with the link.

**The window** takes requests from one channel
(`familychat_core::requests`): the listener's, the first launch's own
link, and on macOS the links the OS hands a running app as Apple Events
(GPUI's `on_open_urls`; macOS never starts a second process for a URL).
Each brings the window forward. A link:

- before the profile is open (the keychain screen, say), waits, newest
  only, until it is;
- on the sign-in screen, starts a sign-in with it;
- with an account signed in, opens the sign-in over the rooms (a
  `SigninView` that goes back to them on Cancel, Back or "Already signed
  in": "Back to your rooms"); a finished sign-in shows the new account;
- never interrupts a sign-in that is redeeming a code or signing in with
  a password ("Finish signing in first, then open the link again.").

In every case the sign-in shows "Sign in as …?" first and the window
sends `Continue` only from that dialog (ADR 0011): a forwarded link never
redeems anything by itself.

Registering `familychat://` with the OS (Info.plist, the MSI, the
`.desktop` file) is Phase 12, as is the macOS bundle without which
`on_open_urls` receives nothing; until then links reach the app through
`familychat LINK`, `open-link`, or pasting.

### Testing

- `familychat-notify`: coalescing, the focused room, hidden content, every
  kind's wording, text made safe, the flood cap (a hundred in a minute
  post six and one summary; the rolling window frees one slot at a time),
  forgotten rooms, `Debug` without text, the title.
- `familychat-session`: each event type's content, edits and reply
  fallbacks, an undecryptable event, each event reported once, the feed's
  single receiver and its cap; live (CI's quality job, against the
  harness): another user's message is a notification with their name, the
  room and the text, the account's own isn't, the unread total counts it,
  and no message text reaches the log.
- `familychat-instance` (every OS in CI's build job): a second launch
  forwards a link and a bare one asks to come forward; a window that
  declines is refused; nobody listening is "not listening" after the wait;
  malformed, wrong-version, non-link, over-long and over-the-frame-limit
  messages are refused and nothing is delivered, and the window serves
  the next one; a silent client is dropped after the timeout without
  holding others up; the permission tests (Unix: the directory is 0700,
  the socket the owner's only and the directory's owner's, a loose
  directory is narrowed, and a second launch refuses a loose directory or
  socket; Windows: the pipe's owner and protected one-entry DACL are the
  current user's, read back from the pipe itself); and `tests/logs.rs`, a
  hand-off's log lines at TRACE with no redaction hold no link, token or
  server name.
- `familychat-app` (`tests/handoff.rs`, every OS): the real binary
  forwards `open-link`'s standard input, a `familychat://` or `https://`
  argument, and a bare launch's "come forward" to a listening profile and
  exits 0; refuses non-links before sending, never echoing a token; the
  terminal UI is still refused; the second launch writes no logs and no
  file holds the token; with nobody listening it says the profile is
  open. The unit tests check the CLI never echoes a link and the window is
  handed only links the sign-in's parser accepts.
- `familychat-ui` view tests: a background room's message is posted
  (title, body, an opaque tag, no actions) and a click opens the room and
  takes the notification down; nothing for the room in front, everything
  once the window is in the background; hidden content says "New
  message" and the dialog changes the setting in the profile; the title
  carries the unread total; a room outside the list opens from its
  notification; an unknown click only brings the window forward; a flood
  is capped; a forwarded link shows the confirmation on the sign-in screen
  and over the rooms, redeems only after Continue, goes back to the rooms
  on Cancel, waits for the profile to open, and doesn't interrupt a code
  in use; the new dialogs' names and targets.
- Fixture views and the screenshot matrix: `notification-settings`,
  `notification-settings-hidden` and `forwarded-link`, in dark and light.

### Manual checks

GPUI's test platform records notifications and simulates clicks; the
operating systems' own delivery, and a real second process on a desktop,
are checked by hand on each OS (a release build, a Family Chat or other
account with a second account to send from):

1. **Notifications, macOS.** Start `familychat` from a release archive.
   Allow notifications when macOS asks (System Settings, Notifications,
   Family Chat, if it didn't). With the window in the background, a
   message from the other account in a group room shows "Room" and
   "Name: text"; a direct message shows the sender as the title. Clicking
   it brings the window forward with the room open. With the window in
   front and that room open, a message there shows nothing, and one in
   another room does. Three quick messages in one room replace each other
   ("3 new messages"). (An unbundled binary may get no notifications on
   macOS: `UNUserNotificationCenter` needs a bundle id. Record what
   happens; Phase 12's DMG is the real check.)
2. **Notifications, Windows.** The same, through toast notifications
   (Settings, System, Notifications, Family Chat must be on), and the
   click opens the room. The taskbar shows "Family Chat (N)" while there
   are unread messages.
3. **Notifications, Linux.** The same on GNOME (Wayland) and KDE or
   another freedesktop notification server; the click opens the room
   (GNOME needs the click action; note if the server ignores it).
4. **Hidden content, each OS.** Notification settings (Secondary+Alt+N, or
   the palette): turn "Show message text in notifications" off; a new
   message says "Name: New message" (a direct chat: "New message"), never
   the text. Quit and start again: the setting stayed.
5. **Flood, any OS.** Send twenty messages to several rooms within a
   minute: at most six notifications and one "Messages are arriving
   quickly" summary.
6. **Second launch, each OS.** With the window open and signed out, run
   `familychat open-link`, paste a sign-in link from the panel at the
   prompt and press Enter: the window comes forward with "Sign in as …?",
   the command prints that the sign-in continues in the window and exits;
   Continue signs in. Again while signed in (a code for another member):
   the confirmation shows over the rooms; Cancel goes back to them.
   `familychat '<link>'` does the same (the link is in that command's
   history: use a code you then let expire); a bare `familychat` brings
   the window forward; `familychat tui` while the window is open says it
   is already open.
7. **No token anywhere, each OS.** After step 6, the profile's
   `logs/familychat.log` holds no `token=` value, and neither does
   anything the second launch printed (the terminal shows what was pasted
   at the prompt, which is the terminal's echo, not the app's).
8. **macOS links.** Not until Phase 12 registers the scheme: note that
   `open 'familychat://…'` does nothing yet.

## Consequences

- Notifications arrive only while the window runs; there is no tray icon,
  no background process and no pusher (PLAN.md sections 5.9 and 6.9).
- Room notification modes and keywords are the account's push rules,
  which the SDK applies; choosing them in the app (`notification_settings()`)
  is Phase 13's settings work.
- Only the first signed-in account is shown (Phase 6); a notification for
  another account only brings the window forward.
- The terminal UI (Phase 10, ADR 0008) doesn't listen for second
  launches: a link forwarded while it has the profile is refused with
  "already open" (it could listen on the same endpoint later), and its
  status line and bell announce new messages its own way rather than
  through `familychat-notify`.
- Linux without `$XDG_RUNTIME_DIR` puts the socket in the profile
  directory; a profile path too long for a Unix socket has no hand-off
  (the window warns once in its log; second launches say "already open").
- On Windows an elevated process and a normal one are the same user: the
  DACL lets both in, which is right for one person's profile.
- The window's title shows the count; macOS's Dock badge and Windows'
  taskbar overlay need platform calls GPUI doesn't offer at this pin.

## Follow-ups

- Phase 12: register `familychat://` in the macOS bundle (`CFBundleURLTypes`,
  which also gives `on_open_urls` its links and notifications their bundle
  id), the MSI and the `.desktop` file (`MimeType=x-scheme-handler/familychat`,
  `Exec=familychat %u`), and check a browser's link opens the running
  window on each OS; the Electron build's claim on the scheme (Open
  decision 3).
- Phase 13: the settings screens take over the notification dialog; room
  notification modes and keywords through `notification_settings()`; the
  accessibility pass over the new dialog.
- family-chat: the fallback page's "Open in Family Chat for desktop"
  button on desktop browsers (filed by the coordinator).
