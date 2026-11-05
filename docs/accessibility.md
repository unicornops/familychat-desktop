# Accessibility

The accessibility baseline Family Chat Desktop keeps (PLAN.md section 5.10),
Zmail's (`docs/phase5-hardening.md` in unicornops/zmail), checked across
every screen of the window in Phase 13 (ADR 0015). It says what ships, so
nobody relies on support that does not exist. None of the screen-reader
support has been confirmed with a real screen reader yet:
[the manual check](#manual-screen-reader-check) is the script for doing
that on each OS, and its results are recorded in the pull request or
release that ran it.

## Checklist

Each item is **checked** (a view test fails when it breaks), **built**
(in the code, checked by hand), or **not yet**.

### Names, roles and structure

- [x] **checked** Every control the window draws has a role and a
  non-empty accessible name (`AccessibleExt::accessible`):
  `view_tests/a11y.rs` (the sign-in, profile, rooms, composer and
  palette screens), `view_tests/encryption.rs` (every encryption screen
  and dialog) and `view_tests/settings.rs` (every Settings section and
  dialog) fail on an empty one. Icon-only buttons are named by their
  tooltip; shortcuts are spelled out ("Ctrl+Shift+K", "Shift+Command+K").
- [x] **built** The window is an application named "Family Chat"; the
  Settings window is "Settings". Dialogs are modal and named by their
  title. The room list is a list box whose rows read the room, its unread
  and mention counts and invitations; the filters are a radio group; the
  timeline's messages are list items read with their sender, time and text; the member list is a list.
- [x] **built** Settings is a vertical tab list of sections and a tab
  panel, each section with a level-1 heading and level-2 group headings;
  on-off settings are check boxes and choices are radio groups, so a
  screen reader says whether each is on.
- [x] **built** Text a screen reader needs (empty states, explanations, a
  link's whole address, the recovery key, a setting's description) is
  readable text, not decoration.
- [ ] **not yet** Links inside a message's text are pointer targets, not
  separate links a screen reader or the keyboard can reach (as in Zmail).
  The terminal UI numbers them; the window needs the same (follow-up).
- [ ] **not yet** Text fields report their text but not the caret or the
  selection (GPUI).

### Keyboard

- [x] **checked** Every command is a GPUI action with a key from the
  commands table, in the command palette (Secondary+K) and, on macOS, the
  menu bar; tests fail when a command has no key or the keymap disagrees.
  Settings is Secondary+, from any window.
- [x] **built** Buttons, check boxes, radio buttons, the section list and
  dialogs are Tab stops with a visible focus ring; Enter and Space
  activate; Escape closes the topmost dialog, and the Settings window when
  none is open.
- [x] **checked** In Settings, Up, Down, Home and End move through the
  sections while the list keeps focus (`view_tests/settings.rs`).
- [x] **built** The rooms: Alt+Up/Down move through rooms (with Shift, the
  unread ones), Secondary+L moves to the composer, Secondary+Alt+Up/Down
  select messages, and each message action has a key; the composer sends
  with Enter and starts a line with Shift+Enter.
- [x] **built** Pointer-only things have keyboard equivalents: a message's
  hover actions are keys on the selected message; a QR image can be pasted.

### Announcements

- [x] **built** Toasts are announced politely, errors assertively (the
  toast host's live regions); sign-in and encryption problems are read
  once by a live region; the command palette announces the highlighted
  command ("Settings, 2 of 9").
- [x] **built** The sync state is a polite status ("Sync: Up to date",
  "Paused"); a rate-limit countdown is visible but not announced every
  second.
- [x] **built** A setting the profile couldn't save is taken back and
  announced as an error toast (`view_tests/settings.rs`).

### Pointer targets

- [x] **checked** Every pointer target is at least 24×24 px (WCAG 2.2,
  2.5.8): `view_tests/hit_targets.rs`, `view_tests/encryption.rs` and
  `view_tests/settings.rs` walk what each screen draws; Settings is
  checked at 100% and 200% text. The overlay scrollbar's strip is the
  documented exception (`components/hit_target.rs`): the wheel, the
  trackpad and Page Up/Down scroll.

### Contrast, text size and motion

- [x] **checked** Both bundled themes reach WCAG AA (4.5:1 text, 3:1
  boundaries) on every fill they are drawn on (`styles::contrast`).
- [x] **built** Windows contrast themes: the system colours replace the
  theme's (read at start and when the window is activated).
- [x] **built** macOS Increase Contrast: secondary text reaches 7:1 and
  borders 4.5:1.
- [x] **checked** Settings → Appearance → Contrast → Increased gives that
  palette on every platform, Linux included; a Windows contrast theme
  still wins (`view_tests/settings.rs`).
- [x] **checked** Settings → Appearance → Text size, 100% to 200% in 25%
  steps, scales every window's text, rows, buttons and dialog widths
  (GPUI's rem size); targets are checked at 200%, room list rows grow
  with their two lines (`the_rooms_grow_with_the_text_size`), and the screenshot
  matrix has Settings and a room at 200% (`settings-appearance-large`,
  `room-text-200`). GPUI exposes no OS text-scale setting, so the
  system's text size isn't followed; use the setting.
- [x] **checked** Settings → Appearance → Motion: Match system (macOS
  Reduce motion, GNOME `enable-animations`, Windows animation effects),
  Reduce, Full. With motion reduced the spinners stand still and animated
  images show their first frame; nothing in the window slides
  (`motion.rs` lists every animation).
- [x] **built** Theme: match system, light or dark.

## Manual screen-reader check

Run this on each OS before saying screen-reader support works, and write
down what is silent or wrong. Use an account with a few rooms, one of them
unread.

1. **Start the screen reader.** macOS: VoiceOver (Cmd+F5). Windows: NVDA
   (Ctrl+Alt+N) or Narrator (Win+Ctrl+Enter). Linux: Orca (Super+Alt+S) on
   GNOME. Open Family Chat: it should be announced as "Family Chat".
2. **Sign-in.** Signed out, Tab through the welcome screen: each way in
   is read with its description. Choose "Sign in with a code", paste a
   link: "Sign in as …?" is read as a dialog with its buttons.
3. **Rooms.** Tab to the room list: Up and Down read each room with its
   unread count ("Smith family, 3 unread, 1 mention"). Alt+Down opens the
   next room; the room's name is read as a heading.
4. **Timeline.** Secondary+Alt+Up selects messages: each is read with its
   sender, time and text. A message this device can't read says why.
5. **Composer.** Secondary+L: the field is read as "Message <room>".
   Type and press Enter: the message appears; the typing notice is not
   read.
6. **Palette.** Secondary+K, type "set": each Down reads the highlighted
   command ("Settings, 1 of 3"). Escape closes it.
7. **Settings.** Secondary+, opens "Settings": the section list is read as
   tabs; Up and Down read each section. Tab moves into the section: each
   check box is read with its state ("Send read receipts, checked"), each
   choice as a radio button in its group ("Theme, Match system, selected,
   1 of 3").
8. **Errors.** In Settings → Account → Sign out…, the dialog's title and
   warning are read. Cancel.
9. **Toasts.** Copy version details in Diagnostics: "Copied the version
   details." is read without moving focus.
10. **Text size.** Settings → Appearance → Text size → 200%: every window
    grows; nothing in the rooms is cut off at 1280×800.
11. **Contrast.** Windows: turn a contrast theme on and switch back; the
    window redraws in its colours. macOS: Increase contrast, switch back;
    secondary text and borders strengthen. Any OS: Contrast → Increased.
12. **Reduced motion.** Turn the system's reduce-motion setting on (or
    Motion → Reduce) and open a room while syncing: the sync indicator
    stands still.

### Results

| OS | Screen reader | Date | Result |
|---|---|---|---|
| macOS | VoiceOver | | not run yet |
| Windows | NVDA, Narrator | | not run yet |
| Linux (GNOME) | Orca | | not run yet |
