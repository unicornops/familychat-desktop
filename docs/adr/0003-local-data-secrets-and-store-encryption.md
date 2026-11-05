# ADR 0003: Local Data, Secrets and Store Encryption

## Status

Proposed with Phase 1 (issue #2); accepted when that pull request
merges. It records PLAN.md, sections 5.4 and 5.5, as built. Zmail's ADR
0005 (unicornops/zmail, "Encrypting the Local Store at Rest") is the
pattern for the key handling and the recovery behaviour; the storage
choice differs, for the reason in section 4.

## Context

Family Chat Desktop keeps, per profile:

- an **app store**: the accounts registry (each account's Matrix ID and
  homeserver URL), preferences and UI state (such as the last room open);
- from Phase 3, each account's **matrix-sdk stores** (state, crypto, event
  cache, media), which hold messages, room keys and the device's identity;
- **secrets**: each account's access and refresh tokens, and the
  passphrases that open the stores;
- **logs** and a **crash log**.

The threat model is Zmail's: **a stolen, lost or imaged disk, or a copied
data directory** (a backup, a synced folder, a disposed laptop), read by
someone who does not hold the user's OS login; and **another local user**.
It is not a compromised running session (section 9).

Two constraints shape the design:

- matrix-sdk's crypto store must have exactly one writer, so two Family
  Chat processes must never open the same profile at once.
- `libsqlite3-sys` links SQLite once per build. matrix-sdk-sqlite 0.19.1
  requires `rusqlite ^0.40.2` (libsqlite3-sys 0.38) with plain SQLite, so
  the app store uses that line too, and whatever SQLite the app store gets,
  the SDK's stores get.

## Decision

### 1. The data directory

One directory per profile: `~/Library/Application Support/Family Chat`
on macOS, `$XDG_DATA_HOME/familychat` (else `~/.local/share/familychat`)
on Linux, `%APPDATA%\Family Chat` on Windows, or `FAMILYCHAT_DATA_DIR`
when set (`familychat_store::paths`, with Zmail's resolution rules):

```text
familychat.lock                  profile lock (exclusive)
familychat.sqlite3               app store: accounts registry, preferences, UI state
accounts/<account-uuid>/sdk/     matrix-sdk state, crypto and event cache stores (Phase 3)
logs/                            familychat.log, crash.log (redacted)
unreadable-<time>/               data moved aside by start over
```

On Unix the directory, `logs/` and the log files are created owner-only
(0700 and 0600). An account's directory is named by a random UUID (its
`AccountId`), never by its Matrix ID.

### 2. One process per profile

`familychat.lock` is locked exclusively (`std::fs::File::try_lock`:
`flock` on Unix, `LockFileEx` on Windows, as Zmail's `lock.rs`) before
anything else, by the window, the terminal UI and `familychat accounts`
alike. A second process is refused at once with "Family Chat is already
open with this profile", before it touches the keychain, the store or
the logs. The OS drops the lock when the process ends, so a crash never
leaves a profile locked. Forwarding a second window's links to the first
is Phase 9.

### 3. Secrets: only in the OS keychain

Every secret lives in the OS keychain (`keyring` 3.6.3: the macOS login
keychain, Windows Credential Manager, the Secret Service on Linux), under
one service id, the app id `family.safechat.desktop`, in three kinds of
entry (`familychat_security::keychain::KeychainEntry`):

| Entry | Holds |
| --- | --- |
| `app-store` | the app store's passphrase |
| `account-<uuid>-session` | the account's session, as JSON (`format`: 1): user id, device id, homeserver URL, access token, refresh token, auth kind; later the OAuth client registration |
| `account-<uuid>-store` | the passphrase of that account's SDK stores |

- Every passphrase is 32 bytes from the OS CSPRNG (`getrandom`), stored as
  lower-case hexadecimal. A new passphrase is written and **read back**
  before anything is encrypted with it, so a keychain that accepts a
  write and loses it (a broken Secret Service can) fails before data is
  stranded, as Zmail's `stash_new_key` does.
- In memory, secrets are `Secret<T>`: wiped (`zeroize`) when dropped, no
  `Display` or `Serialize`, and a `Debug` that prints `[redacted]` and
  nothing else, not even the length. `Passphrase` and `AccountSession`
  print the same. Errors from the keychain never quote a stored value.
- A keychain that is locked, refuses access or can't be reached is an
  error (section 6). **There is no plaintext fallback**, and no second
  place a secret may go. A passphrase-wrapped key for headless Linux is
  later work (PLAN.md, section 14).
- The `Keychain` trait is the seam: `OsKeychain` in the app,
  `MemoryKeychain` (with injectable locked, unreachable and forgetful
  failures) in every test, so no test touches a developer's keychain.

### 4. The app store: SQLite with the SDK's `StoreCipher`

`familychat.sqlite3` is plain SQLite (rusqlite 0.40, `bundled`, so every
OS builds the same SQLite from source, as matrix-sdk's `bundled-sqlite`
will). Identifying values are encrypted with matrix-sdk's own
`StoreCipher` (`matrix-sdk-store-encryption` =0.19.1), the construction
matrix-sdk-sqlite uses for every value in the SDK's stores:

- The cipher's keys (a ChaCha20-Poly1305 key and a MAC key seed) are
  generated once and kept in the `store_cipher` table, encrypted with the
  `app-store` passphrase through `export_with_key` (HKDF-SHA256). The
  passphrase is random, so it is used as a key and not stretched with
  PBKDF2 as a password would be.
- **Encrypted** (`encrypt_value`, XChaCha20-Poly1305 with a random nonce):
  each account's record (Matrix ID, homeserver URL), and every UI state
  value (room IDs and the like). Where a lookup needs a value, the store
  keeps its keyed hash (`hash_key`, BLAKE3 keyed per table) beside it: the
  registry's one-row-per-Matrix-ID check, the UI state's keys.
- **Plaintext**: only non-identifying preferences (theme, text size),
  from a closed list (`Preference`), and the account UUIDs and their
  order.
- Migrations follow Zmail: named SQL batches recorded in
  `schema_migrations`, each in its own transaction. A store with a
  migration this build does not know was written by a newer Family Chat
  and is refused without being changed.
- The key is checked (the cipher is imported) before any migration runs,
  and a store whose creation was cut short (no cipher row yet) is
  finished, not treated as unreadable.

**Why not SQLCipher**, as Zmail uses:

- One `libsqlite3-sys` per build: building it with SQLCipher replaces
  SQLite for matrix-sdk-sqlite too, so every SDK database would run on
  SQLCipher without being keyed by it, and the build would take on
  Zmail's crypto backend (vendored OpenSSL on Linux and Windows, NASM in
  Windows CI, CommonCrypto on macOS) for one small database.
- The SDK's stores, where the messages and keys are, already encrypt
  every value with `StoreCipher`; the same construction for the app store
  gives one encryption story, one audited implementation and no C crypto.
- What SQLCipher would add is hiding the structure below. For a registry
  of a handful of accounts and a few preferences, that is not worth the
  cost above.

### 5. What remains visible on disk

To someone holding the data directory but not the keychain:

- **Structure**: the schema (table and column names), which says the
  file is Family Chat's app store.
- **Counts**: the number of rows in each table (how many accounts, how
  many UI state values), and the number of `accounts/<uuid>/` directories.
  The keychain's entry names (`account-<uuid>-…`) say the same to someone
  who can list it.
- **Sizes**: each ciphertext is its plaintext's length plus a fixed
  overhead, so the length of a Matrix ID and homeserver URL can be
  estimated (not their contents).
- **The plaintext preferences**, the account UUIDs and their order, and
  when the store was created (`schema_migrations.applied_at`) and last
  written (file times).
- **Logs**: redacted (section 7), but they show when the app ran and what
  kinds of things happened.
- **Moved-aside data** in `unreadable-<time>/`, which is still encrypted,
  until the user deletes it.

A test (`crates/familychat-store/tests/no_identifying_values_on_disk.rs`)
writes a known Matrix ID, homeserver URL and room ID and checks no file in
the data directory contains them, open (database and write-ahead log)
and closed, while a plaintext preference is found (so the scan reads
what SQLite wrote).

### 6. When the keychain fails or the key is gone

`Profile::open` (after the lock) gives one of three states, each with the
words both frontends will show:

- **Ready**.
- **Keychain unavailable**: locked, access refused, or no Secret Service.
  Nothing has been created or changed; the user is told why and retries
  (`KeychainUnavailable::retry`, which keeps the lock). A new profile with
  an unavailable keychain gets no store file at all.
- **Unreadable**: there is an app store and the keychain has no key for
  it (a reinstalled system, a reset keychain, a copied data directory),
  the keychain's key does not open it, or the file is damaged. Nobody can
  read it, which is the point. As in Zmail's ADR 0005, the recovery
  screen says what happened and what is lost (local state only; messages
  are on the homeserver, and encrypted history needs the recovery key or
  another device), and **start over** moves the app store, its journals
  and `accounts/` into `unreadable-<UTC time>/` instead of deleting them,
  so a restored keychain can still open them; then it opens a new store.
  Start over does not touch the keychain: a damaged `app-store` entry is
  replaced only when there is no data it could belong to.

`familychat accounts` reports the non-ready states and exits non-zero;
`familychat start-over` is the CLI's start over (the recovery screens
are Phase 5 and 10). It refuses when the data opens, and when the keychain
is merely unavailable.

### 7. Logs and the crash log

`tracing` writes to `logs/familychat.log` only: no stderr, no remote
logging. Family Chat's crates log at `info`, matrix-sdk and everything
else at `warn` (`FAMILYCHAT_LOG` takes `target=level` directives). Every
line is scrubbed before it is written: URL query strings, fragments and
user info, `familychat://` links in full, `Bearer` credentials, the values
of token-, password-, secret- and key-named fields, and Matrix tokens by
their prefixes (`syt_`, `syr_`, `mat_`, `mar_`, `mct_`). Message bodies
and other private values are wrapped in `Redacted` at the call site,
which prints `[redacted]`. A panic appends one scrubbed line to
`logs/crash.log` (Zmail's hook). Each file moves to `.old` at start past
its size limit. Logging starts only after the lock is held. A test
captures the log lines of events carrying a passphrase, tokens, a
password, a login token in a URL, a message body and a `familychat://`
link, and checks none of them appears.

### 8. Testing on every OS

The store and security tests are unit and integration tests in
`cargo test --workspace`, which CI runs on Linux (quality job) and on
macOS and Windows (build job). Two kinds of test reach real processes and
real keychains:

- the profile lock with two processes: the test binary re-runs itself
  as the second process (Zmail's pattern), and `familychat accounts`,
  `start-over`, `tui` and the window are each refused by the real binary
  while a test holds the lock;
- the real keychain: `real_keychain_round_trip` is `#[ignore]`d in
  `cargo test` and run by CI's build job on each OS, under a test-only
  service id, on Linux against a throwaway Secret Service
  (`scripts/keychain-session.sh`); the build job also runs
  `familychat accounts` twice through the platform keychain.

### 9. What this does not protect

As in Zmail's ADR 0005: anything running as the same user while Family
Chat can unlock its data (the Secret Service gives any process of the
session the secret; Windows Credential Manager gives it to any process of
the user; macOS asks unless the request comes from Family Chat itself);
the process's memory; and someone who has the user's OS login, and so
the keychain.

## Consequences

- Secrets are never written anywhere but the keychain, and nothing that
  identifies a person is written to disk unencrypted.
- A machine without a working keychain (a headless Linux box without a
  Secret Service) can't use Family Chat until one runs; that is the
  price of no plaintext fallback.
- A lost keychain loses the local data for good; moving it aside keeps
  the chance that the key comes back.
- Frontends get each state with its wording from `familychat-store`, so
  the window and the terminal UI say the same thing.
- `rusqlite` and `matrix-sdk-store-encryption` move only with the Matrix
  crates (Dependabot groups them).

## Decisions

Taken in Phase 1 from PLAN.md's recommendations; Rob may revisit them:

1. `StoreCipher` for identifying values, no SQLCipher (PLAN.md, section
   5.4).
2. The window and the terminal UI take the profile lock at start from
   Phase 1, before either opens the store, so "one process per profile"
   holds from now on.
3. Logs live under the data directory (`logs/`, as PLAN.md lays out),
   not in a separate state directory as Zmail's crash log does.
4. `familychat start-over` gives the CLI the recovery action, as Zmail's
   `reset-store` does.
