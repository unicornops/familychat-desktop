# ADR 0012: Messaging Actions and the Composer

## Status

Proposed with Phase 7 (issue #8); accepted when that pull request merges.
It records PLAN.md sections 5.3, 5.6, 5.7 (the encryption default for new
rooms), 5.8 (uploads and the composer), 6.8 (curfews), 6.10 and 13.7 as
built. ADRs 0008 to 0010 are reserved by PLAN.md and 0013 by Phase 9, so
this is 0012.

## Context

Phases 3 to 6 read: rooms, timelines, media. Phase 7 writes, from the
window now and from the terminal UI in Phase 11, so the rules of writing
(what Markdown becomes, which files may go, whether a new room is
encrypted, what a paused account may do) must live outside both
frontends. The frontends may not depend on matrix-sdk (ADR 0001), so
whatever holds those rules must not either. Family Chat adds three
constraints of its own: its Palpo takes uploads up to 11 MiB, a child's
photo must not carry where it was taken, and a curfew suspends the account
(`M_USER_SUSPENDED`), which must read as "Chat is paused", not as a
failure.

## Decision

### `familychat-actions`, over `SessionApi`

A new crate, `familychat-actions`, is the one place a frontend changes
anything in a room. It has no GPUI, ratatui or matrix-sdk dependency: it
works through `familychat-core`'s `SessionApi`, so both frontends depend on
it directly (Zmail's `zmail-actions` has the same place over its store).
`scripts/check-dependency-boundaries.sh` fails if its graph reaches a
matrix-sdk crate or either frontend's toolkit, and self-tests that the
window's graph has it.

`Actions` (one per account) returns an `ActionTask` for each action: a
future that only awaits the session's `tokio::sync` answers, so it runs on
any executor (the window uses GPUI's background executor; the tests await
it). As it runs it reports `ActionEvent`s (done, refused before anything
was sent, failed, a room made, a draft read) to the plain callback the
service was made with; `Actions::channel` puts them on a channel, which is
how the window receives them. `ActionEvent::message` is the sentence a
frontend shows.

`SessionApi` gains the session's primitives, all of them requests already
prepared: `room_action` (a `RoomRequest`: send, edit, toggle a reaction,
redact, mark read, typing, send a file, invite, answer an invitation,
leave), `create_room`, `media_limits`, `server_defaults`, `draft` and
`save_draft`. Phase 6's `answer_invite` became `RoomRequest::AnswerInvite`,
so invitations go through the actions too. `familychat-core::messaging`
holds the types; the ones that carry text or bytes (`OutgoingMessage`,
`OutgoingFile`, `Mention`, `NewRoom`) print sizes only in `Debug`.

### What is sent

- **Markdown** is ruma's conversion (`FormattedBody::markdown`, the one
  matrix-sdk's `text_markdown` uses, on pulldown-cmark), so it matches
  every other matrix-rust-sdk client. Plain text sends no HTML.
- The **HTML we send is what our own sanitiser keeps**: the converted
  markup goes through `familychat_security::matrix_html::sanitize` (the
  allowlist every received message goes through, ADR 0007), and a test
  checks that what is sent is a fixed point of it. Raw HTML typed into
  the composer is kept only as far as the allowlist goes (`<b>` stays,
  `<script>`, remote images and `javascript:` links go).
- **Mentions** are picked from the room's members (the `@` list,
  `mentions`): the name goes in the text, and on sending each one still in
  the text becomes a `https://matrix.to/#/@user:server` link and joins
  `m.mentions`. A mention deleted from the text is not sent.
- Messages, edits, reactions and deletions go through matrix-sdk-ui's
  `Timeline` (the room's open one, so the local echo shows at once, or one
  made for the action) and so through the SDK's **send queue**; files go
  through the send queue's `send_attachment`. A request answers as soon as
  it is queued.

### The send queue and offline sends

The SDK stops a room's send queue after a failed send. The account's
supervisor (ADR 0005) now keeps it going: when the sync reaches the server
again it turns the queue back on (which wakes every room's) and reads the
upload limit once; when a send fails but can be retried while the sync
keeps running, it turns the queue on again after a backoff (one second,
doubling to a minute). So a message written with no connection shows as
"Sending…" and goes when the server is back. A live test stops the
harness's front end, sends, starts it again and sees the message arrive.

### Files

- The size is checked against the server's limit
  (`/_matrix/client/v1/media/config`, through the SDK's
  `load_or_fetch_max_upload_size`) **before the file is read or
  uploaded**, and refused with the sizes in the sentence ("That file is
  14 MB, and this server takes files up to 11 MB. Nothing was sent.").
  The window checks when a file is picked, pasted or dropped, so an
  over-limit file never even waits as a card, and again when it is sent.
  If the limit can't be found out, no file is sent. A live test shows the
  media config read in the front end's access log and no upload.
- **A picture loses its location** by default
  (`familychat_security::image_metadata::remove_location`), losslessly: the
  file's own structure is walked and the metadata blocks left out, so the
  pixels are the sender's bytes. JPEG: EXIF (rebuilt with only the
  orientation, so photos stay upright), XMP, Photoshop and IPTC, FlashPix
  and multi-picture segments, comments, and anything after the image's end
  (a multi-picture file's extra images); PNG: `eXIf`, the text chunks
  (where XMP lives) and `tIME`; WebP: the `EXIF` and `XMP ` chunks and
  their `VP8X` flags; GIF: XMP application extensions and comments. A file
  that says it is one of these but can't be walked is decoded within the
  image limits and encoded again. HEIC, AVIF and TIFF can't be rewritten
  here, so they are refused with a sentence saying so rather than sent with
  their location. The tests inspect the bytes: unit tests per format, an
  actions test of what reaches the session, and a live test of the bytes
  another member downloads. A setting to keep the location is Phase 13's.

### New rooms

New rooms and direct chats are encrypted unless the server's
`.well-known/matrix/client` says `io.element.e2ee.default: false`
(PLAN.md, section 5.7). The session reads the well-known from the user's
server name (the homeserver's own origin when it is the same host) with
the account's HTTP client; one it can't read says nothing, and the room is
encrypted. The rule itself (`encrypt_new_room`) is the actions'. A live
test makes a direct chat on Palpo and sees it encrypted.

### Paused accounts

`ComposerState` (writable, paused, not joined) is decided in the actions
from the account's status and the room. While the account is
`SyncState::Suspended`, every write is refused with `Refusal::Paused`
before it reaches the session (no request is made), typing notices and
read receipts are not sent, and the window's composer is read-only with
"Chat is paused. Messages can't be sent until the pause ends." The sync
banner's title for that state is "Chat is paused". When the pause ends the
composer is writable again. This holds whatever family-chat decision 11
says about reads: if a server lets a paused account sync, the pause shows
on the first send instead: the supervisor treats a send refused with
`M_USER_SUSPENDED` like a refused sync (the account goes to `Suspended`
and rechecks slowly), and the refusal reads as "Chat is paused".

### Drafts

What was being written in a room is kept between visits in the app store's
new `drafts` table (migration `0002_drafts`): the account's UUID, the keyed
hash of the room id and the draft encrypted with the store's
`StoreCipher`, like the rest of the identifying data (ADR 0003). The
window keeps it a second after typing stops and when the room is left,
and forgets it when the message is sent. Removing an account removes its
drafts. An edit is never kept as a draft.

### Typing and read receipts

Typing notices are sent when typing starts, renewed every three seconds
while it goes on (the SDK throttles too), and stopped when the text is
emptied, sent, or idle for five seconds. The read receipt and the read
marker move together (`send_multiple_receipts`, which the SDK skips when
they would not move) when the newest message is on screen and the window
is in front, at most every two seconds. Settings for both are Phase 13's.

### The composer and the message actions in the window

- `ComposerView` replaces Phase 6's read-only footer: Zmail's multi-line
  `TextInput` with a new `submit_on_enter` mode (Enter submits,
  Shift+Enter is the new `Newline` action), an attach button, the send
  button, a reply or edit preview with Cancel (Escape), cards for the files
  waiting, an error line, and the mention list (Up, Down, Enter, Tab and
  Escape work it while it shows, under the `Composer && showing_mentions`
  context, as Zmail's recipient field does). Up in an empty composer edits
  the account's newest message. Files come from the picker, the clipboard
  (a pasted picture or copied files) and drops.
- Each message shows its actions while hovered or selected from the
  keyboard: React (a quick picker of six reactions), Reply, and for the
  account's own messages Edit and Delete. A reaction under a message adds
  or takes back the account's own. Deleting a message and leaving a room
  ask first.
- New commands, each in the commands table with window and terminal keys:
  `FocusComposer`, `AttachFile`, `SelectPreviousMessage`,
  `SelectNextMessage`, `ReplyToMessage`, `EditMessage`, `ReactToMessage`,
  `DeleteMessage`, `LeaveRoom`. Their terminal keys follow PLAN.md's Open
  decision 10 (`R`, `E`, `+`, `D`).
- Screenshot states (dark and light, with the narrow and wide
  breakpoints): `composer-text`, `composer-reply`, `composer-edit`,
  `composer-mentions`, `composer-attachment`, `composer-too-large`,
  `room-paused`, `message-actions` and `message-delete`; the empty
  composer is in every room screenshot.

## Consequences

- Both frontends share every writing rule and sentence; Phase 11's
  terminal composer uses the same `Actions`, mention matching and file
  preparation.
- A frontend can't send markup our own sanitiser would strip, a file over
  the server's limit, a picture with its location, or anything while
  paused.
- The app store has a second migration; a store a newer Family Chat
  changed still refuses to open (ADR 0003).
- Not done here: the window has no dialog yet to start a direct chat or
  make a room, or to invite someone (the actions, their tests and the
  session can); slash commands; retrying a message the server refused
  for good (it can be deleted, which cancels it); private read receipts and turning typing
  notices off (Phase 13's settings); verification and the rest of
  encryption (Phase 8).
