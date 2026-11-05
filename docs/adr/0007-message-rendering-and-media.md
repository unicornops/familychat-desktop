# ADR 0007: Message Rendering and Media

## Status

Proposed with Phase 6 (issue #7); accepted when that pull request merges.
It records PLAN.md sections 5.8, 5.10, 7, 8 and 13.6 as built.

## Context

Phase 3 streams rooms and timelines to any frontend through `SessionApi`,
and Phase 5 gave the window its shell. Phase 6 draws them: the room list,
the room and its timeline, rich text, images and files, and links. Every
one of those is content from other people, through their homeservers, and
the threat model (PLAN.md section 7) names a malicious message (HTML,
images, file names) and a malicious server. There is no web view (PLAN.md
section 5.8), so the question is how to draw formatted messages and media
without parsing anything hostile on the UI thread, without loading
anything from anywhere but the account's homeserver, and without the
frontends (the window now, the terminal UI in Phase 10) each making their
own rules.

## Decision

### Formatted bodies become a tree, never markup

A message's `formatted_body` (`org.matrix.custom.html`) never reaches a
frontend. `familychat-session`'s mapping hands it to
`familychat-security::matrix_html`, which turns it into
`familychat-core::rich_text::RichText` in three passes, none of which
loads anything (the pre-pass and the element stack are adapted from
Zmail's mail sanitiser):

1. A linear tokenizer pre-pass checks the caps: 64 KiB of markup (a whole
   Matrix event is at most that), 4,096 elements, and nesting (an
   approximate 192 before parsing, an exact 48 after). A body over a cap
   gets no rich text: `message_text` falls back to the plain `body`.
2. ammonia 4.2 (html5ever's tree builder, so deep nesting cannot overflow
   the stack) keeps the Matrix specification's allowlist: `font`, `del`,
   `h1` to `h6`, `blockquote`, `p`, `a`, `ul`, `ol`, `sup`, `sub`, `li`,
   `b`, `i`, `u`, `strong`, `em`, `s`, `code`, `hr`, `br`, `div`,
   `table` and its parts, `caption`, `pre`, `span`, `img`, `details` and
   `summary`. The reply fallback (`mx-reply`), scripts, styles, frames,
   objects, embeds, audio, video, SVG, MathML, forms and templates go with
   their contents. Attributes kept: `a href` with an `https`, `http`,
   `mailto` or `matrix` address only; `img src` only as `mxc://` (with
   `alt` and `title`); `ol start` as a number; `code class` as
   `language-*`; `span data-mx-spoiler`. Relative addresses, comments and
   every colour (`data-mx-color`, `data-mx-bg-color`, `font color`) go: a
   sender cannot make text unreadable against the theme.
3. A tokenizer pass over ammonia's output builds the tree, with its own
   caps (512 blocks, 64 KiB of text, 256 links, quotes and lists drawn at
   most 8 deep).

The HTML sanitised is the sender's own, read from the event's JSON (the
latest edit's `m.new_content` when the message was edited), not the
formatted body matrix-sdk-ui's timeline hands over. The timeline has
already run ruma-html over that copy, and ruma-html removes an element it
does not allow but keeps its contents, so `<script>alert(1)</script>`
would arrive as the text `alert(1)` and its contents could no longer go
with it. The SDK's copy is the fallback only where there is no JSON yet (a
local echo of our own message).

The tree is flat: a list of blocks (paragraph, heading, list item with its
depth and marker, code, rule), each one run of text with ordered,
non-overlapping styled ranges (bold, italic, underline, strikethrough,
code, spoiler, raised or lowered, link) and a quote depth. That is what a
styled-text element draws in one pass, and it cannot grow a deep structure
whatever the sender wrote. Table rows become one block each, cells joined
with " · "; an inline image is its alt text (inline `mxc://` images are not
fetched in this phase). Control characters and the bidirectional
embeddings, overrides and isolates (U+202A to U+202E, U+2066 to U+2069)
are removed, so a link's text cannot be reordered. A plain body becomes
the same tree (`RichText::plain`), with the web and `mailto:` addresses
written in it as links. `RichText`'s `Debug`, like `MessageBody`'s,
prints sizes only.

The window draws the tree with GPUI's `StyledText` and `InteractiveText`
(as Zmail's reader draws its links): links and hidden spoilers are click
targets inside the text, a link's address is its tooltip. The terminal UI
will draw the same tree as spans (Phase 10).

`tests/hostile_html.rs` (adapted from Zmail's hostile corpus) runs the
fixtures under `tests/fixtures/hostile_html` (script and event-handler
vectors, mutation XSS, frames and objects, styles, `javascript:`, `data:`
and `file:` links, SVG and MathML with scripts inside, the reply
fallback, bidirectional tricks, entity and encoding tricks, misnesting),
twenty generated pathological inputs (10,000 nested elements, huge
tables, a 1 MB body) and 2,000 deterministic mutations (20,000 in a
release build) through the sanitiser, and checks every output twice: an
independent tokenizer pass over the sanitised markup (only allowlisted
elements and attributes, only allowed schemes) and the tree's invariants
(ranges in bounds and on character boundaries, ordered and not
overlapping, valid link indices, bounded depths, the caps, no text from a
removed element), each within a time bound.

### Links: Matrix links in the app, everything else asks

`familychat-core::links::classify` decides what a click does, so both
frontends behave the same:

- A `matrix:` URI or an `https://matrix.to/#/` link (ruma's parsers) names
  a user, a room or an event: the window opens the room (and scrolls to
  the event) when it is in the room list, and says who a user is. Nothing
  is requested from anyone.
- An `https`, `http` or `mailto` address shows a dialog (adapted from
  Zmail's reader): the whole address in a monospace box, read out before
  the choice, with Copy Link, Cancel and Open Link. When the link's text
  looks like a web address on another site than the target (the userinfo
  trick included), the dialog says so and Cancel becomes the main button.
  The address goes to the operating system only after Open Link.
- Anything else (`javascript:`, `file:`, `data:`, relative addresses) is
  refused with a toast; the sanitiser already drops them from HTML, and
  this is the second check.

No link is ever previewed or fetched (PLAN.md section 8).

### Media: authenticated media only, decoded within limits, never SVG

A frontend never fetches or decodes media. It holds a `MediaInfo` (the
sender's declared type, size and dimensions, a safe file name, and an
opaque `MediaHandle`: the source's JSON, whose `Debug` hides the
encrypted file's key) and asks the session:

- **Fetching.** Every account's client is built with
  `familychat-session`'s `AuthenticatedMediaFetcher` instead of the SDK's
  default. It uses only the authenticated media endpoints (Matrix 1.11,
  MSC3916: `/_matrix/client/v1/media/download` and `/thumbnail`), with the
  account's access token (refreshed once on a 401), through the account's
  own HTTP client (no redirects followed). A server whose `/versions` lacks
  them gets no media request at all, and the frontends say "Your server
  doesn't support authenticated media". Each download has a five-minute
  timeout and a byte cap enforced while the body streams in (256 MiB, 32
  MiB for images); an encrypted file is decrypted with the SDK's
  attachment decryptor. The SDK still caches results in the account's
  encrypted media store.
- **Images.** `SessionApi::image` refuses an SVG (by type or name) before
  fetching anything, asks the server for an 800 by 600 thumbnail of a
  plain image (falling back to the file), and uses the sender's thumbnail
  or the file in an encrypted room. `familychat-security::images::
  decode_image` then runs on Tokio's blocking pool with
  `TIMELINE_IMAGE_LIMITS`: 32 MiB of file; anything that looks like SVG or
  XML refused whatever it is called; the format sniffed from the bytes
  and only PNG (APNG included), JPEG, GIF and WebP read; the header's
  dimensions checked before any pixel is decoded (8,192 pixels a side, 32
  megapixels a frame, with `image`'s own limits as a second line);
  animations read a frame at a time and refused past 120 frames or 48
  megapixels in all; frames scaled down to 1,600 pixels a side. What
  comes back is RGBA frames: there is no format left for a frontend to
  parse, and nothing it could hand to an SVG renderer. The window turns
  them into GPUI images (BGRA) on its background executor and keeps the 48
  most recently drawn, dropping the others' textures.
  `tests/image_bombs.rs` covers a PNG header claiming 100,000 pixels a
  side, a 6,000 by 6,000 decompression bomb under 2 MB, a GIF with a
  65,535-pixel screen, frame bombs over the frame and pixel budgets, SVG
  in four disguises, truncated and unknown files, and real images of every
  format; each bomb is refused within half a second.
- **Files.** Video, audio and other files are not played or opened (PLAN.md
  section 5.8): the window shows the name and size with a Save button,
  asks where with the platform's save dialog (suggesting the session's
  safe name: `familychat-security::file_names::sanitize_filename`, copied
  from Zmail, so no path, control or bidirectional character, Windows
  device name or trailing dot survives), and `SessionApi::save_file`
  writes it through a temporary file in the chosen folder, renamed when
  complete. Nothing is opened afterwards.

### The room list and the timeline in the window

- **Rooms.** `RoomsView` keeps its own copy of the room list, applying the
  session's `ListDiff` batches (bursts coalesced into one redraw), and draws
  it with GPUI's `uniform_list` (only visible rows are laid out). The
  filters are PLAN.md's: all, unread, people, invitations. Each row shows
  unread and mention badges, the latest message, a not-encrypted mark,
  and for an invitation who sent it with Accept and Decline (also offered
  in a card when the invitation is opened). Invitations are answered
  through `SessionApi::answer_invite` (join or leave); Phase 7's
  `familychat-actions` may take this over.
- **The room.** The header shows the room, its topic or member count, an
  encryption badge (Encrypted, Not encrypted, unknown) with its sentence,
  and the member list button; a room that is not encrypted also gets a
  banner (PLAN.md section 6.8). The member list (`SessionApi::members`)
  shows joined and invited members with their roles.
- **The timeline.** `TimelineView` keeps its own items and draws them with
  GPUI's `list`, bottom-aligned and following new messages while the
  reader is at the end, laying out only rows on screen.
  `familychat-core::timeline::rows` (shared with the terminal UI) folds
  runs of two or more membership and room changes into one summary row
  that opens on request, and groups a sender's messages within five
  minutes. After each batch only the rows whose content changed are
  measured again (`row_fingerprint`, `changed_rows`), so a reaction in a
  room of 10,000 messages lays out one row. Scrolling near the top asks
  for older messages. Rows: text, notices (muted, with an icon), emotes
  ("Dad is making pancakes"), rich text, replies (a quote that scrolls to
  the original), edits ("Edited"), redactions ("Message deleted"),
  reactions (chips naming who reacted), the read marker ("New messages"),
  day dividers ("Today", "Yesterday", the weekday, the date), single room
  changes in words, folded summaries, the start of the room, and
  "Waiting for this message's key" for what this device can't decrypt
  (the reasons are Phase 8's).
- **Layout.** Below 760 pixels the window shows the list or the room,
  with a back button; from 1,180 pixels the member list sits beside the
  timeline, and below that it covers the timeline. The window opens at
  1,180 by 780.
- **Commands.** The commands table gains a Rooms scope: next and previous
  room, next and previous unread room, jump to the oldest unread message,
  jump to the latest, show or hide the members, and the four filters,
  each with a window key and a terminal key, in the palette and a macOS
  Rooms menu.

### The 1,000-room, 10,000-event fixture

`FixtureSession::with_large` loads 1,000 rooms and a 10,000-event room
(day dividers, plain and formatted messages, some long, reactions, a read
marker). `view_tests/perf.rs::a_large_room_scrolls_without_blocking` runs
with every view test: sixty scroll frames up the room lay out only the
rows on screen (asserted from a count of rows laid out), and the room list
scrolls the same way. `performance_budgets` (ignored; run in a release
build, adapted from Zmail's) times the real window with the platform's
text system, CPU time on the main thread per step:

Recorded on 2026-11-05 on Linux x86_64 (AMD Ryzen 5 3600 6-Core Processor, 12 threads), release
build, cosmic-text; the window opened on the fixture in 73.9 ms.

| Measurement | n | Median | p95 | Max | Budget |
|---|---|---|---|---|---|
| Timeline scroll frame, from the end | 120 | 2.14 ms | 2.41 ms | 2.90 ms | 8.33 ms (120 Hz) |
| Timeline scroll frame, middle | 120 | 2.11 ms | 2.38 ms | 2.77 ms | 8.33 ms |
| Timeline jump far away (as a scrollbar drag) | 60 | 2.96 ms | 3.51 ms | 4.11 ms | 8.33 ms |
| Room list scroll frame (1,000 rooms) | 120 | 1.98 ms | 2.30 ms | 2.70 ms | 8.33 ms |
| One event changed (a reaction) | 20 | 5.21 ms | 5.99 ms | 6.30 ms | 100 ms |
| Whole timeline replaced (10,000 events) | 20 | 7.52 ms | 9.65 ms | 9.73 ms | 100 ms |
| Open a room (Next room) | 30 | 4.57 ms | 6.09 ms | 8.20 ms | 100 ms |

Nothing went over its budget. One changed event costs more than a scroll
frame because the rows and their fingerprints are recomputed over all
10,000 items (linear, but cheap); only the changed row is laid out again.
Run it with
`cargo test --release -p familychat-ui --lib performance_budgets -- --ignored --nocapture`
(`FAMILYCHAT_PERF_REPORT=<dir>` writes the tables).

## Consequences

- No markup, remote image format or address ever reaches a frontend:
  changing what is allowed means changing `familychat-security` (and its
  corpora), not a view.
- The terminal UI (Phase 10) draws the same `RichText`, rows, link rules
  and file names, so both frontends say and allow the same things.
- A homeserver without authenticated media shows no media at all in this
  app. The live test (`messages_and_an_image_from_another_user_appear`)
  checks that the Palpo Family Chat runs offers it and that every request
  for the image went to the authenticated endpoints.
- Inline `mxc://` images in formatted messages, custom emoji, avatars,
  video and audio playback and link previews are not drawn; avatars are
  initials. Sending, read receipts, reactions and redactions are Phase 7;
  unable-to-decrypt reasons and verification are Phase 8.
- Links inside message text are pointer targets; reaching them from the
  keyboard is part of the Phase 13 accessibility pass.
