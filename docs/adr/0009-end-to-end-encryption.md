# ADR 0009: End-to-End Encryption

## Status

Proposed with Phase 8 (issue #9); accepted when that pull request merges.
It records PLAN.md sections 5.7, 6.6, 6.10 (MSC3967) and 13.8 as built.
PLAN.md reserved this number for the encryption defaults.

## Context

Every Family Chat room is end-to-end encrypted (ADR 0012 makes new rooms
so), and all crypto is the matrix-rust-sdk's, in `familychat-session`. Up
to Phase 7 the window could only say "Waiting for this message's key":
nothing confirmed a new device, set recovery up, verified anything or
explained a message it couldn't read, so a second device, or a lost one,
meant lost history. Phase 8 makes encryption something a family can live
with, in the window; the terminal UI gets the same flows in Phase 11.

Three things shape it:

- Family Chat's sign-ins run outside the SDK (ADR 0006): `familychat-signin`
  logs in with its own transport and hands the session to the SDK through
  `restore_session`. The SDK's login never sees a password, so it cannot
  answer the user-interactive auth (UIA) the first cross-signing upload may
  need.
- Children sign in with a code from a parent (`m.login.token`) and often
  have no password at all. Their cross-signing depends on the server
  letting the first upload through without UIA (MSC3967, family-chat
  #257). The Phase 2 harness found that the pinned Palpo does; replacing
  the keys still asks for a password.
- Family Chat moves families to custom domains (the BYOD design), which
  needs the history's keys carried across: room key export and import.

## Decision

### The encryption defaults: Element X's at the pinned SDK

The client is built (`account::build_client`) with the settings Element X
Android builds its client with on matrix-sdk 0.19 (read from its public
`RustMatrixClientFactory` and the SDK's FFI `ClientBuilder` defaults;
nothing copied):

| Setting | Element X | Family Chat | Why |
| --- | --- | --- | --- |
| Room key recipients (`CollectStrategy`) | `ErrorOnVerifiedUserProblem` ("exclude insecure devices" is a labs flag, off by default; on, it is `IdentityBasedStrategy`) | `ErrorOnVerifiedUserProblem` | the same |
| Decryption trust (`TrustRequirement`) | `Untrusted` (`CrossSignedOrLegacy` with the labs flag) | `Untrusted` | the same; the timeline marks messages from unconfirmed devices |
| Share history on invite (MSC4268) | on | on | the same; a child added to the family room can read what came before |
| Make backups when the account has none | on | on | the same |
| Cross-signing made automatically | on (at the SDK's login, with the password) | off: `encryption::set_up_identity` makes it | the SDK's login never sees our sign-ins; see below |
| Backup download | `AfterDecryptionFailure` (the FFI default) | `AfterDecryptionFailure` | the same; see below for why not `OneShot` |

One difference, for a reason:

- **Cross-signing is made by the session, not by the SDK's login.** After
  every sign-in and restore, `encryption::follow` asks the server (one
  `/keys/query`, not the local store: an upload the server refused leaves
  the new identity in the store as if it were the account's) whether the
  account has a master key. If it has none, `set_up_identity` uploads one
  with no UIA first (MSC3967); if the server answers with UIA, it answers
  with the password a password sign-in handed on, or, with none (a code
  sign-in on a server without MSC3967), shows "Your server needs your
  password": the page explains that a code-only account needs a parent to
  set a password in the Family Chat panel, takes the password and tries
  again, or "Not now" (with the skip warning). Element X gets the same
  result from the SDK's login with the password.

The backup is read one key at a time, as Element X reads it: each message
that fails to decrypt asks the backup for its key
(`GET /room_keys/keys/{room}/{session}`), and when the backup's key
arrives (after a verification, or from secret storage with the recovery
key) the SDK's redecryptor tries the messages it holds again, which
fetches theirs. Downloading the whole backup at once (`OneShot`) was
tried first and fails on the pinned Palpo: its `GET /room_keys/keys` (and
`/room_keys/keys/{room}`) answer each stored key's encrypted payload as if
it were a room's `{"sessions": …}` map, so the SDK finds no sessions and
the history stays unreadable; the per-session endpoint answers correctly.
That Palpo bug is a follow-up for the fork, and Element X's choice works
around it.

The SDK's `automatic-room-key-forwarding` feature stays off (ADR 0005):
keys reach a new device through verification and the backup. Secret
requests (the cross-signing keys and the backup key, after a
verification) are not part of that feature and work.

What `ErrorOnVerifiedUserProblem` means for a family: when a verified
person (most often the account itself, once confirmed) has a device they
haven't confirmed, or a verified person's identity changes, the message
stops instead of going to that device. The timeline says why
(`SendProblem`: "Not sent: someone in this room has a device they haven't
confirmed") with **Send anyway**, which goes through `familychat-actions`
(`Actions::send_anyway`, `RoomRequest::SendAnyway`): as Element X does, the
unconfirmed devices' local trust is set to "ignored", or the changed
identity's old verification is withdrawn, and the message is put back in
the send queue (`unwedge`). Refused while paused, like every write.

### The password: handed on once, then wiped

A password sign-in's password goes from `familychat-signin`'s flow to
`AccountSink::add` (a new `Option<Secret<String>>`) and from there to
`AccountManager::add_account_with_password`, `Account::open` and the
account's supervisor, which hands it to `encryption::follow`. It is used
for the cross-signing upload's UIA only if the server asks, and dropped
(so wiped, `Secret` zeroizes) as soon as that is done. It is never
stored, never in the app store or the keychain, never logged.

The same holds for the passwords typed into the window later (the password
page, signing another device out), the recovery key typed or shown, and a
key file's passphrase: each is taken out of its field with
`TextInput::take_secret`, carried as `familychat-core`'s `SecretText`
(`Debug` prints `[redacted]`, wiped on drop), and turned into a `Secret`
for one request. One copy is not ours to wipe: ruma's `uiaa::Password`,
which the SDK sends, holds the password in a `String` while the request
is built and sent, and drops it with the request (its `Debug` redacts it).
The mocked tests (`tests/mocked_encryption.rs`) check the password goes
only to `/keys/device_signing/upload`, only with the answers to UIA, and
never into the log; the live tests check the log for the password, the
recovery key, the passphrase and the login token.

### `SessionApi` and the view models

`familychat-core`'s new `encryption` module holds the view models and
their wording (both frontends say the same thing): `EncryptionStatus`
(this device's `DeviceTrust`, the account's `IdentitySetup`, its
`RecoveryState`, whether it has another confirmed device),
`VerificationFlow` (steps on a `watch`, answers on an `mpsc`, as a sign-in
flow), `VerificationStep` and `VerificationAnswer`, `SasEmoji`,
`QrModules` (a QR code as modules; `Debug` shows its size only, since it
carries the flow's one-time secret), `DeviceView`, `KeyImport`,
`IdentityWarning` and `UtdReason`. `TimelineContent::UnableToDecrypt` now
carries a `UtdReason`, and `SendState::Failed` a `SendProblem`.

`SessionApi` gains the encryption calls: `encryption`, `set_up_identity`,
`verify_this_device`, `verify_device`, `verification_requests` (one
listener, like `notifications`), `enable_recovery`, `recover`, `devices`,
`sign_out_device`, `export_room_keys`, `import_room_keys`,
`identity_warnings` and `acknowledge_identity`. No matrix-sdk type crosses
it (`scripts/check-dependency-boundaries.sh`). The live implementation is
`familychat-session`'s `encryption` module; `familychat-test-support`'s
`FixtureSession` scripts every one of them for the view tests and
screenshots, recording what was asked (`EncryptionAsk`) and never a
secret.

### "Confirm it's you"

While the account has an identity and this device isn't confirmed
(`EncryptionStatus::needs_confirmation`), the window shows "Confirm it's
you" in place of the rooms, after every sign-in and every start:

- **Use another device**: asks the account's other devices to verify this
  one (the identity's `request_verification`). Both sides can then compare
  emoji (SAS); when the other device can scan (a phone), the desktop shows
  a QR code for it (`m.qr_code.show.v1` and `m.reciprocate.v1`; it never
  scans, having no camera), dark on white in every theme with its quiet
  zone, and asks "Does the other device say the code was scanned?" before
  confirming.
- **Enter your recovery key**: shown when the account has recovery.
- **Skip for now**: asks first ("Until you confirm this device, it can't
  read messages sent before it signed in…"), then the rooms show with a
  "Not confirmed" banner and a Confirm it's you button; the command
  palette's Confirm it's you brings the screen back. A new reason to
  confirm (another device reset the identity) asks again.

Only the account's own devices are verified: a request from someone else
is left alone (Family Chat has no person-to-person verification yet), and
a request that arrives with no window listening is declined.

A request to confirm this device can be lost on either side at the pinned
SDK, and nothing reports it:
- **Sending:** matrix-sdk 0.19.1 sends it only to the account's devices
  this one knows as signed by the identity. So the session asks the
  server for the account's keys first (`request_user_identity`, a
  `/keys/query`), falling back to the stored ones.
- **Receiving:** a device drops a request from a device it has no keys
  for yet ("Could not retrieve the device data for the incoming
  verification request, ignoring it"). It learns a new device's keys only
  when its sync reports the device-list change.

If the other device shows nothing, Cancel and Use another device again
sends a new request. The live test does exactly that, every 15 seconds.
Offering "Send again" in the waiting dialog itself is a follow-up.

### Recovery

When this device is confirmed and the account has no recovery, a banner
offers it. **Set up recovery** (`recovery().enable()`, waiting for the
backup's upload) makes secret storage and a recovery key, shown once in a
dialog that can't be dismissed by clicking outside it, in a monospaced
box, with **Copy** and **Save to a file…** (a file the person chooses, in
Documents by default, written readable by its owner only on Unix); Done is
enabled only once the key was copied or saved. Closing the dialog drops
the key. A wrong key on another device says "That recovery key doesn't
match"; the right one fetches the account's secrets, which also sign this
device (`import_secrets`), and the backup's keys.

### Devices, identity warnings, unable to decrypt

- **Your devices** lists the account's devices (`/devices` with the
  crypto store's trust): this device first, then the most recently seen,
  each with Verified or Not verified. Another device can be verified
  (from a confirmed device) or signed out; signing out asks the server's
  UIA with the password when it asks ("If you signed in with a code, ask
  your parent to sign the device out from the Family Chat panel").
- **Identity warnings**: the open room follows
  `subscribe_to_identity_status_changes`. A member whose identity was
  reset gets a caution banner with OK (pin the new identity); one the
  account had verified gets a danger banner with Withdraw verification
  (nothing is sent to them until then).
- **Unable to decrypt**: matrix-sdk-ui's cause (`UtdCause`) on each
  timeline item becomes a `UtdReason` with its sentence (sent before you
  joined, the sender's identity changed, an unconfirmed or unknown sending
  device, history from before this device with no backup or before it was
  confirmed, withheld from this device or by the sender). The SDK's
  unable-to-decrypt hook (`UtdHookManager`, with a 60-second grace for
  late keys) is installed on every timeline and reports to the local log
  only: the reason and how late a key came, never an event, room or
  sender (PLAN.md section 8).

### Room key export and import

**Export room keys** asks for a passphrase twice (at least eight
characters, the same both times) before asking where to save, then writes
every room key with `export_room_keys` (a standard Megolm key export,
PBKDF2 with 500,000 rounds, readable by any Matrix client). **Import room
keys** takes a file the person chooses and its passphrase and says how
many keys were new ("Imported 12 of 14 keys."); a wrong passphrase says
so. This is what a family's move to its own domain uses to keep its
encrypted history.

### A new room's "Not encrypted" banner

ADR 0012 found the room list can show a room this window just made before
its `m.room.encryption` state arrives. `ActionEvent::RoomCreated` now says
whether the room was made encrypted, and the window shows no "Not
encrypted" banner for such a room until the list catches up.

### A room that looks not encrypted

The room key export test once exported no keys at all from a device that
had just made an encrypted room and sent into it: the message had gone
out in plaintext. matrix-sdk 0.19.1 encrypts only when the room's info
holds an `m.room.encryption` event, and it marks the encryption state as
known whenever a sliding sync answer asked for `m.room.encryption`,
whether the answer carried it or not (`RoomInfo::handle_encryption_state`;
`Room::latest_encryption_state` asks the server only while the state is
unknown). Both Palpo and Synapse can answer for a new room before its
initial state is all there (Palpo appends the creator's join, then each
initial event in turn, and the join wakes the creator's sync), so an
answer shows the room without its `m.room.encryption`; the SDK then takes the
room as not encrypted until a later answer brings the event, and a
message the send queue sends in between goes out in plaintext.
`Client::create_room` doesn't record the encryption its own request
asked for. The live test below saw it before the fix: every one of eight
new rooms on Synapse and one of eight on Palpo looked not encrypted for a
moment, and four messages (three on Synapse, one on Palpo) were stored
as `m.room.message` with their text. Palpo also remembers per connection
which state events it has sent, whether or not the client got the answer,
so a lost answer can keep an `m.room.encryption` from ever being sent
again on that connection. The decision, fail-closed, in
`familychat-session`'s `messaging`:

- A room made encrypted has the `m.room.encryption` content its
  `createRoom` asked for written into the SDK's room info (saved to the
  store, or kept in memory if the store refuses) before `create_room`
  hands the room back. A sync answer without the event never takes one
  away, so nothing sent into the room can go out in plaintext.
- Before a message, an edit, a file or Send anyway is queued in a room
  the SDK takes as not encrypted, the guess is thrown away and the server
  is asked (`GET /rooms/{id}/state/m.room.encryption/`). An event the
  server has sticks in the room's info and the queue encrypts; a 404 is a
  room that really isn't encrypted. When the server can't be asked
  (offline), the state stays unknown and the send queue asks the server
  itself before it sends. This covers any room whose sync left it looking
  not encrypted, not only new ones; rooms known to be encrypted cost no
  request, a room that isn't costs one per send. Reactions and
  redactions are never encrypted by the SDK, as in every Matrix client.

What is left: a send queued offline into a room that wrongly looks not
encrypted, where the first sync after the connection returns again
leaves the room looking not encrypted before the queue sends. That needs
a room the sync wrongly left looking not encrypted and an outage at
once; new rooms are never in it. Upstream, `Client::create_room` could
record the encryption it asked for, and Palpo could tie the state it
remembers as sent to the client's `pos` (an issue for the unicornops/palpo
fork). The live test
`messages_sent_at_once_into_a_new_encrypted_room_are_encrypted` makes
eight encrypted rooms on Palpo and on Synapse, sends into each at once and
reads what the server stored: `m.room.encrypted`, without the text. The
window's held-back "Not encrypted" banner (above) stays, for the room
list's first sight of a room before `create_room` answers.

### Commands

Confirm it's you (Secondary+Alt+V), Set up recovery (Secondary+Alt+R),
Your devices (Secondary+Shift+D), Export room keys (Secondary+Alt+E) and
Import room keys (Secondary+Alt+I) are rows of the commands table, bound
in the window's `Shell` context and handled by the signed-in view. The
terminal UI lists them in `LATER` for Phase 11.

## Consequences

- A second device, or a new one after losing the old, reads the history:
  by verification with another device, or with the recovery key. The live
  tests show both on the harness Palpo, and a key file carrying the
  history to a device that has neither.
- A code-only account on Palpo makes its cross-signing identity with no
  password (MSC3967); on a server that asks for one, the window says so
  and offers the way forward. The live test prints which happened, for
  family-chat #257.
- With Element X's recipient strategy, a family member with an unconfirmed
  device stops messages until someone presses Send anyway (or the device
  is confirmed). This is Element X's behaviour and the price of MSC4153's
  direction; turning on "exclude insecure devices" later is a change of
  two settings here.
- The pinned Palpo's whole-backup download (`GET /room_keys/keys`) answers
  in the wrong shape; until the fork fixes it, the backup must be read key
  by key (as here), and a client using `OneShot` reads nothing from it.
- One copy of a UIA password lives in ruma's request while it is sent.
- Not yet: verifying other people, resetting the identity or the recovery
  key, dehydrated devices, and the terminal UI's encryption (Phase 11);
  the settings pages that gather these (Phase 13); QR login (Phase 14).
