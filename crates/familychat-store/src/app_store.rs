//! The app store, `familychat.sqlite3` (ADR 0003): the accounts registry,
//! preferences and UI state, in plain SQLite with every identifying value
//! encrypted by matrix-sdk's `StoreCipher`.
//!
//! - The cipher's keys are generated once and kept in the database
//!   (`store_cipher`), encrypted with the app store passphrase from the
//!   keychain (`StoreCipher::export_with_key`: the passphrase is 32 random
//!   bytes, so it is used as a key, through HKDF, rather than stretched).
//! - Identifying values (Matrix IDs, homeserver URLs, room IDs) are stored
//!   only as `StoreCipher::encrypt_value` ciphertext (XChaCha20-Poly1305).
//!   Where a lookup needs one, the store keeps its keyed hash
//!   (`StoreCipher::hash_key`, BLAKE3 keyed per table) beside it.
//! - Only non-identifying preferences (theme, text size) are plaintext.
//!
//! Migrations follow Zmail's `zmail-store`: named SQL batches recorded in
//! `schema_migrations`, each applied once in its own transaction. A store
//! with a migration this build does not know was written by a newer
//! Family Chat and is refused rather than changed.

use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use familychat_security::{AccountId, Passphrase};
use matrix_sdk_store_encryption::StoreCipher;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_app_store", SCHEMA_0001),
    ("0002_drafts", SCHEMA_0002),
];

const SCHEMA_0001: &str = r#"
-- The StoreCipher's keys, encrypted with the app store passphrase.
CREATE TABLE store_cipher (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    export BLOB NOT NULL
);

-- The accounts registry. `id` is a random UUID, `user_id_hash` the keyed
-- hash of the Matrix ID (one row per account), `data` the encrypted
-- AccountRecord.
CREATE TABLE accounts (
    id TEXT PRIMARY KEY NOT NULL,
    position INTEGER NOT NULL,
    user_id_hash BLOB NOT NULL UNIQUE,
    data BLOB NOT NULL
);

-- Non-identifying preferences, in plaintext.
CREATE TABLE preferences (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

-- UI state that may identify someone (the last room open): keyed hashes of
-- the keys, encrypted values.
CREATE TABLE ui_state (
    key BLOB PRIMARY KEY NOT NULL,
    value BLOB NOT NULL
);
"#;

const SCHEMA_0002: &str = r#"
-- What was being written in each room, kept between visits (ADR 0012).
-- `account_id` is the account's random UUID (it identifies nobody),
-- `room_hash` the keyed hash of the room id, `value` the encrypted draft.
-- Removing an account removes its drafts.
CREATE TABLE drafts (
    account_id TEXT NOT NULL,
    room_hash BLOB NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (account_id, room_hash)
);
"#;

/// `StoreCipher::hash_key` table names, one per use.
const ACCOUNTS_USER_ID: &str = "accounts.user_id";
const UI_STATE_KEY: &str = "ui_state.key";
const DRAFTS_ROOM: &str = "drafts.room";

/// Why the app store could not be used.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed (a full disk, a permission problem).
    #[error("the app store failed to {action}: {source}")]
    Sqlite {
        /// What it was doing.
        action: &'static str,
        /// SQLite's error.
        source: rusqlite::Error,
    },
    /// The keychain's passphrase does not open this store.
    #[error("the app store's key does not open it")]
    WrongKey,
    /// The file is not a Family Chat app store, or its contents are
    /// damaged.
    #[error("the app store is damaged: {0}")]
    Damaged(String),
    /// A newer Family Chat changed the store; this one can't read it.
    #[error(
        "the app store was changed by a newer version of Family Chat ({0}); update Family Chat to open it"
    )]
    Newer(String),
    /// The Matrix ID is not `@user:server`.
    #[error("{0} is not a Matrix user ID (@user:server)")]
    InvalidUserId(String),
    /// The account is in the registry already.
    #[error("that account is already signed in")]
    DuplicateAccount,
    /// Encrypting a value failed.
    #[error("failed to encrypt a value for the app store: {0}")]
    Encryption(String),
}

fn sqlite(action: &'static str) -> impl FnOnce(rusqlite::Error) -> StoreError {
    move |source| StoreError::Sqlite { action, source }
}

/// What is in the store's file, read without its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    /// No file.
    Absent,
    /// A file whose creation never finished: no cipher, so no data.
    Empty,
    /// An app store with data encrypted under some passphrase.
    Encrypted,
}

/// Looks at the store at `path` without decrypting anything.
pub(crate) fn probe(path: &Path) -> Result<Probe, StoreError> {
    if !path.exists() {
        return Ok(Probe::Absent);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(classify("open the app store"))?;
    let has_cipher_table: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'store_cipher')",
            [],
            |row| row.get(0),
        )
        .map_err(classify("read the app store"))?;
    if !has_cipher_table {
        return Ok(Probe::Empty);
    }
    let ciphers: i64 = conn
        .query_row("SELECT COUNT(*) FROM store_cipher", [], |row| row.get(0))
        .map_err(classify("read the app store"))?;
    Ok(if ciphers == 0 {
        Probe::Empty
    } else {
        Probe::Encrypted
    })
}

/// SQLite's "not a database" and "corrupt" as [`StoreError::Damaged`].
fn classify(action: &'static str) -> impl Fn(rusqlite::Error) -> StoreError {
    move |source| match source.sqlite_error_code() {
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => {
            StoreError::Damaged(source.to_string())
        }
        _ => StoreError::Sqlite { action, source },
    }
}

/// A Matrix account as the registry keeps it (encrypted, whole).
#[derive(Serialize, Deserialize)]
struct AccountRecord {
    user_id: String,
    homeserver_url: String,
}

/// An account to add to the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    /// The Matrix user ID, `@user:server`.
    pub user_id: String,
    /// The homeserver's client-server API base URL.
    pub homeserver_url: String,
}

/// An account in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    /// The account's local id (its keychain entries and directory).
    pub id: AccountId,
    /// The Matrix user ID.
    pub user_id: String,
    /// The homeserver's client-server API base URL.
    pub homeserver_url: String,
}

/// The preferences kept in plaintext. Only what identifies nobody belongs
/// here; anything that could (a room, a person, a server) goes in the
/// encrypted UI state instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    /// The theme: `system`, `light` or `dark`.
    Theme,
    /// The text size, in percent (`100` to `200`).
    TextSize,
    /// Whether notifications show the message's text: `show` or `hide`.
    NotificationContent,
    /// Whether system notifications are posted at all: `on` or `off`.
    Notifications,
    /// Whether public read receipts are sent: `on` or `off`.
    ReadReceipts,
    /// Whether typing notices are sent: `on` or `off`.
    TypingNotices,
    /// Reduced motion: `system`, `reduce` or `full`.
    Motion,
    /// Contrast: `system` or `increased`.
    Contrast,
}

impl Preference {
    fn key(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::TextSize => "text-size",
            Self::NotificationContent => "notification-content",
            Self::Notifications => "notifications",
            Self::ReadReceipts => "read-receipts",
            Self::TypingNotices => "typing-notices",
            Self::Motion => "motion",
            Self::Contrast => "contrast",
        }
    }
}

/// The open app store.
pub(crate) struct AppStore {
    conn: Mutex<Connection>,
    cipher: StoreCipher,
}

impl AppStore {
    /// Creates the store at `path` (or finishes one whose creation was cut
    /// short), with a new cipher encrypted under `passphrase`.
    pub(crate) fn create(path: &Path, passphrase: &Passphrase) -> Result<Self, StoreError> {
        let mut conn = Connection::open(path).map_err(classify("create the app store"))?;
        configure(&conn)?;
        migrate(&mut conn)?;
        let cipher =
            StoreCipher::new().map_err(|error| StoreError::Encryption(error.to_string()))?;
        let export = cipher
            .export_with_key(passphrase.bytes())
            .map_err(|error| StoreError::Encryption(error.to_string()))?;
        conn.execute(
            "INSERT INTO store_cipher (id, export) VALUES (1, ?1)",
            [export],
        )
        .map_err(sqlite("save the store cipher"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            cipher,
        })
    }

    /// Opens the store at `path`, which [`probe`] found encrypted, with
    /// `passphrase`. The key is checked before anything is changed.
    pub(crate) fn open(path: &Path, passphrase: &Passphrase) -> Result<Self, StoreError> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(classify("open the app store"))?;
        let export: Vec<u8> = conn
            .query_row("SELECT export FROM store_cipher WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(classify("read the store cipher"))?;
        // A wrong key fails the AEAD check; anything else (a version or
        // KDF this build does not know, bytes that do not decode) means the
        // row is damaged.
        let cipher = StoreCipher::import_with_key(passphrase.bytes(), &export).map_err(
            |error| match error {
                matrix_sdk_store_encryption::Error::Encryption(_) => StoreError::WrongKey,
                other => StoreError::Damaged(other.to_string()),
            },
        )?;
        configure(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            cipher,
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn encrypt(&self, value: &impl Serialize) -> Result<Vec<u8>, StoreError> {
        self.cipher
            .encrypt_value(value)
            .map_err(|error| StoreError::Encryption(error.to_string()))
    }

    fn decrypt<T: DeserializeOwned>(&self, value: &[u8]) -> Result<T, StoreError> {
        self.cipher
            .decrypt_value(value)
            .map_err(|error| StoreError::Damaged(format!("a value does not decrypt: {error}")))
    }

    /// The registry, in the order the accounts were added.
    pub(crate) fn accounts(&self) -> Result<Vec<AccountSummary>, StoreError> {
        let conn = self.conn();
        let mut statement = conn
            .prepare("SELECT id, data FROM accounts ORDER BY position, id")
            .map_err(sqlite("read the accounts"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(sqlite("read the accounts"))?;
        let mut accounts = Vec::new();
        for row in rows {
            let (id, data) = row.map_err(sqlite("read the accounts"))?;
            let id = id
                .parse()
                .map_err(|_| StoreError::Damaged("an account id is malformed".to_owned()))?;
            let record: AccountRecord = self.decrypt(&data)?;
            accounts.push(AccountSummary {
                id,
                user_id: record.user_id,
                homeserver_url: record.homeserver_url,
            });
        }
        Ok(accounts)
    }

    /// Adds `account` to the registry under a new random id.
    pub(crate) fn add_account(&self, account: &NewAccount) -> Result<AccountId, StoreError> {
        let id = AccountId::generate()
            .map_err(|error| StoreError::Encryption(format!("no random bytes: {error}")))?;
        self.add_account_with_id(id, account)?;
        Ok(id)
    }

    pub(crate) fn add_account_with_id(
        &self,
        id: AccountId,
        account: &NewAccount,
    ) -> Result<(), StoreError> {
        if !is_matrix_user_id(&account.user_id) {
            return Err(StoreError::InvalidUserId(account.user_id.clone()));
        }
        let hash = self
            .cipher
            .hash_key(ACCOUNTS_USER_ID, account.user_id.as_bytes());
        let data = self.encrypt(&AccountRecord {
            user_id: account.user_id.clone(),
            homeserver_url: account.homeserver_url.clone(),
        })?;
        let mut conn = self.conn();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite("add the account"))?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE user_id_hash = ?1)",
                [hash.as_slice()],
                |row| row.get(0),
            )
            .map_err(sqlite("add the account"))?;
        if exists {
            return Err(StoreError::DuplicateAccount);
        }
        transaction
            .execute(
                "INSERT INTO accounts (id, position, user_id_hash, data)
                 VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM accounts), ?2, ?3)",
                params![id.to_string(), hash.as_slice(), data],
            )
            .map_err(sqlite("add the account"))?;
        transaction.commit().map_err(sqlite("add the account"))?;
        Ok(())
    }

    /// Removes the account from the registry; `false` when it was not there.
    pub(crate) fn remove_account(&self, id: AccountId) -> Result<bool, StoreError> {
        let conn = self.conn();
        conn.execute("DELETE FROM drafts WHERE account_id = ?1", [id.to_string()])
            .map_err(sqlite("remove the account's drafts"))?;
        let removed = conn
            .execute("DELETE FROM accounts WHERE id = ?1", [id.to_string()])
            .map_err(sqlite("remove the account"))?;
        Ok(removed > 0)
    }

    /// The account's draft for `room`, decrypted.
    pub(crate) fn draft<T: DeserializeOwned>(
        &self,
        account: AccountId,
        room: &str,
    ) -> Result<Option<T>, StoreError> {
        let hash = self.cipher.hash_key(DRAFTS_ROOM, room.as_bytes());
        let value: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT value FROM drafts WHERE account_id = ?1 AND room_hash = ?2",
                params![account.to_string(), hash.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite("read a draft"))?;
        value.map(|value| self.decrypt(&value)).transpose()
    }

    /// Keeps the account's draft for `room`, encrypted, or forgets it
    /// (`None`).
    pub(crate) fn set_draft(
        &self,
        account: AccountId,
        room: &str,
        draft: Option<&impl Serialize>,
    ) -> Result<(), StoreError> {
        let hash = self.cipher.hash_key(DRAFTS_ROOM, room.as_bytes());
        let conn = self.conn();
        match draft {
            Some(draft) => {
                let value = self.encrypt(draft)?;
                conn.execute(
                    "INSERT INTO drafts (account_id, room_hash, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT (account_id, room_hash) DO UPDATE SET value = excluded.value",
                    params![account.to_string(), hash.as_slice(), value],
                )
                .map(drop)
                .map_err(sqlite("save a draft"))
            }
            None => conn
                .execute(
                    "DELETE FROM drafts WHERE account_id = ?1 AND room_hash = ?2",
                    params![account.to_string(), hash.as_slice()],
                )
                .map(drop)
                .map_err(sqlite("forget a draft")),
        }
    }

    /// A preference's value, if set.
    pub(crate) fn preference(&self, preference: Preference) -> Result<Option<String>, StoreError> {
        self.conn()
            .query_row(
                "SELECT value FROM preferences WHERE key = ?1",
                [preference.key()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite("read a preference"))
    }

    /// Sets a preference.
    pub(crate) fn set_preference(
        &self,
        preference: Preference,
        value: &str,
    ) -> Result<(), StoreError> {
        self.conn()
            .execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [preference.key(), value],
            )
            .map(drop)
            .map_err(sqlite("save a preference"))
    }

    /// The UI state under `key`, if any.
    pub(crate) fn ui_state<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StoreError> {
        let hash = self.cipher.hash_key(UI_STATE_KEY, key.as_bytes());
        let value: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT value FROM ui_state WHERE key = ?1",
                [hash.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite("read the UI state"))?;
        value.map(|value| self.decrypt(&value)).transpose()
    }

    /// Saves UI state under `key`, both encrypted.
    pub(crate) fn set_ui_state(&self, key: &str, value: &impl Serialize) -> Result<(), StoreError> {
        let hash = self.cipher.hash_key(UI_STATE_KEY, key.as_bytes());
        let value = self.encrypt(value)?;
        self.conn()
            .execute(
                "INSERT INTO ui_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![hash.as_slice(), value],
            )
            .map(drop)
            .map_err(sqlite("save the UI state"))
    }
}

/// Whether `text` looks like a Matrix user ID: `@`, a localpart, `:` and a
/// server name. The full grammar is checked by ruma from Phase 3.
fn is_matrix_user_id(text: &str) -> bool {
    text.strip_prefix('@')
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(localpart, server)| {
            !localpart.is_empty() && !server.is_empty() && !text.chars().any(char::is_whitespace)
        })
}

fn configure(conn: &Connection) -> Result<(), StoreError> {
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(sqlite("configure SQLite"))?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        "#,
    )
    .map_err(classify("configure SQLite"))
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .map_err(sqlite("prepare the migrations"))?;
    let applied: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT version FROM schema_migrations")
            .map_err(sqlite("read the migrations"))?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(sqlite("read the migrations"))?
            .collect::<Result<_, _>>()
            .map_err(sqlite("read the migrations"))?
    };
    if let Some(unknown) = applied
        .iter()
        .find(|version| !MIGRATIONS.iter().any(|(known, _)| known == version))
    {
        return Err(StoreError::Newer(unknown.clone()));
    }
    for (version, sql) in MIGRATIONS {
        if applied.iter().any(|done| done == version) {
            continue;
        }
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite("begin a migration"))?;
        transaction
            .execute_batch(sql)
            .map_err(sqlite("apply a migration"))?;
        transaction
            .execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                [version],
            )
            .map_err(sqlite("record a migration"))?;
        transaction.commit().map_err(sqlite("commit a migration"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    fn passphrase() -> Passphrase {
        Passphrase::generate().expect("random")
    }

    #[test]
    fn probe_tells_absent_empty_and_encrypted_apart() {
        let dir = TestDir::new("probe");
        let path = dir.data_dir().store_path();
        dir.data_dir().create().expect("dir");
        assert_eq!(probe(&path).expect("probe"), Probe::Absent);
        // A file whose creation was cut short before the cipher was saved.
        {
            let mut conn = Connection::open(&path).expect("open");
            migrate(&mut conn).expect("migrate");
        }
        assert_eq!(probe(&path).expect("probe"), Probe::Empty);
        drop(AppStore::create(&path, &passphrase()).expect("created"));
        assert_eq!(probe(&path).expect("probe"), Probe::Encrypted);
        std::fs::write(
            &path,
            b"this is not SQLite, and it is long enough to be checked.....",
        )
        .expect("written");
        assert!(matches!(probe(&path), Err(StoreError::Damaged(_))));
    }

    #[test]
    fn the_store_opens_only_with_its_passphrase() {
        let dir = TestDir::new("passphrase");
        dir.data_dir().create().expect("dir");
        let path = dir.data_dir().store_path();
        let key = passphrase();
        let store = AppStore::create(&path, &key).expect("created");
        store
            .add_account(&NewAccount {
                user_id: "@alice:example.org".to_owned(),
                homeserver_url: "https://matrix.example.org".to_owned(),
            })
            .expect("added");
        drop(store);
        assert!(matches!(
            AppStore::open(&path, &passphrase()),
            Err(StoreError::WrongKey)
        ));
        let store = AppStore::open(&path, &key).expect("opens with its key");
        assert_eq!(store.accounts().expect("accounts").len(), 1);
    }

    #[test]
    fn the_registry_keeps_order_and_refuses_duplicates() {
        let dir = TestDir::new("registry");
        dir.data_dir().create().expect("dir");
        let store = AppStore::create(&dir.data_dir().store_path(), &passphrase()).expect("created");
        let alice = NewAccount {
            user_id: "@alice:example.org".to_owned(),
            homeserver_url: "https://matrix.example.org".to_owned(),
        };
        let bob = NewAccount {
            user_id: "@bob:safechat.family".to_owned(),
            homeserver_url: "https://hs1.safechat.family".to_owned(),
        };
        let alice_id = store.add_account(&alice).expect("added");
        let bob_id = store.add_account(&bob).expect("added");
        assert!(matches!(
            store.add_account(&alice),
            Err(StoreError::DuplicateAccount)
        ));
        let accounts = store.accounts().expect("accounts");
        assert_eq!(
            accounts,
            [
                AccountSummary {
                    id: alice_id,
                    user_id: alice.user_id.clone(),
                    homeserver_url: alice.homeserver_url.clone(),
                },
                AccountSummary {
                    id: bob_id,
                    user_id: bob.user_id.clone(),
                    homeserver_url: bob.homeserver_url.clone(),
                },
            ]
        );
        assert!(store.remove_account(alice_id).expect("removed"));
        assert!(!store.remove_account(alice_id).expect("already gone"));
        assert_eq!(store.accounts().expect("accounts").len(), 1);
        // Removed, it can be added again.
        store.add_account(&alice).expect("added again");
    }

    #[test]
    fn only_matrix_user_ids_are_accepted() {
        assert!(is_matrix_user_id("@alice:example.org"));
        assert!(is_matrix_user_id("@a:[::1]:8448"));
        for bad in [
            "alice:example.org",
            "@alice",
            "@:example.org",
            "@alice:",
            "@al ice:x",
            "",
        ] {
            assert!(!is_matrix_user_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn drafts_are_encrypted_per_account_and_go_with_it() {
        let dir = TestDir::new("drafts");
        dir.data_dir().create().expect("dir");
        let key = passphrase();
        let path = dir.data_dir().store_path();
        let store = AppStore::create(&path, &key).expect("created");
        let account = store
            .add_account(&NewAccount {
                user_id: "@ana:smith.safechat.family".to_owned(),
                homeserver_url: "https://smith.safechat.family/".to_owned(),
            })
            .expect("added");
        let room = "!family:smith.safechat.family";
        assert_eq!(store.draft::<String>(account, room).expect("read"), None);
        store
            .set_draft(
                account,
                room,
                Some(&"a secret plan for Saturday".to_owned()),
            )
            .expect("kept");
        assert_eq!(
            store
                .draft::<String>(account, room)
                .expect("read")
                .as_deref(),
            Some("a secret plan for Saturday")
        );
        drop(store);
        let bytes = std::fs::read(&path).expect("the file");
        for needle in ["secret plan", "!family"] {
            assert!(
                !bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "{needle} is on disk in plaintext"
            );
        }
        let store = AppStore::open(&path, &key).expect("opens");
        store
            .set_draft(account, room, None::<&String>)
            .expect("forgotten");
        assert_eq!(store.draft::<String>(account, room).expect("read"), None);
        store
            .set_draft(account, room, Some(&"again".to_owned()))
            .expect("kept");
        assert!(store.remove_account(account).expect("removed"));
        assert_eq!(store.draft::<String>(account, room).expect("read"), None);
    }

    #[test]
    fn preferences_and_ui_state_round_trip() {
        let dir = TestDir::new("prefs");
        dir.data_dir().create().expect("dir");
        let key = passphrase();
        let path = dir.data_dir().store_path();
        let store = AppStore::create(&path, &key).expect("created");
        assert_eq!(store.preference(Preference::Theme).expect("read"), None);
        store
            .set_preference(Preference::Theme, "dark")
            .expect("set");
        store
            .set_preference(Preference::Theme, "light")
            .expect("replaced");
        store
            .set_preference(Preference::TextSize, "15")
            .expect("set");
        store
            .set_ui_state("last-room", &"!room:example.org")
            .expect("set");
        drop(store);
        let store = AppStore::open(&path, &key).expect("opens");
        assert_eq!(
            store
                .preference(Preference::Theme)
                .expect("read")
                .as_deref(),
            Some("light")
        );
        assert_eq!(
            store
                .preference(Preference::TextSize)
                .expect("read")
                .as_deref(),
            Some("15")
        );
        assert_eq!(
            store
                .ui_state::<String>("last-room")
                .expect("read")
                .as_deref(),
            Some("!room:example.org")
        );
        assert_eq!(store.ui_state::<String>("other").expect("read"), None);
    }

    #[test]
    fn a_store_from_a_newer_version_is_refused_unchanged() {
        let dir = TestDir::new("newer");
        dir.data_dir().create().expect("dir");
        let key = passphrase();
        let path = dir.data_dir().store_path();
        drop(AppStore::create(&path, &key).expect("created"));
        Connection::open(&path)
            .expect("open")
            .execute(
                "INSERT INTO schema_migrations (version) VALUES ('9999_from_the_future')",
                [],
            )
            .expect("inserted");
        match AppStore::open(&path, &key) {
            Err(StoreError::Newer(version)) => assert_eq!(version, "9999_from_the_future"),
            Err(other) => panic!("expected a newer store, got {other:?}"),
            Ok(_) => panic!("expected a newer store, but it opened"),
        }
    }

    #[test]
    fn migrations_are_recorded_once() {
        let dir = TestDir::new("migrations");
        dir.data_dir().create().expect("dir");
        let mut conn = Connection::open(dir.data_dir().store_path()).expect("open");
        migrate(&mut conn).expect("first");
        migrate(&mut conn).expect("again is a no-op");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, i64::try_from(MIGRATIONS.len()).expect("small"));
    }
}
