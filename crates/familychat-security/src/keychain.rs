//! The OS keychain, the only place Family Chat keeps secrets (PLAN.md,
//! section 5.4; ADR 0003).
//!
//! Every entry lives under one service id, the app id
//! (`family.safechat.desktop`), with one of three names
//! ([`KeychainEntry`]):
//!
//! - `app-store`: the app store's passphrase;
//! - `account-<uuid>-session`: an account's session ([`AccountSession`]);
//! - `account-<uuid>-store`: the passphrase of that account's SDK stores.
//!
//! The [`Keychain`] trait is the seam: [`OsKeychain`] is the macOS login
//! keychain, Windows Credential Manager or the Secret Service through
//! `keyring`, and [`MemoryKeychain`] is the mock every test uses, so no test
//! touches a developer's real keychain. The real-keychain tests are
//! `#[ignore]`d.
//!
//! A keychain that can't be reached or is locked is an error
//! ([`KeychainError`]), never a reason to keep a secret anywhere else.
//!
//! Adapted from Zmail's `zmail-security` (`KeyringSecretStore`, the
//! `StoreKeychain` trait and its in-memory store, and `stash_new_key`'s
//! read-back check), GPL-3.0-only like this project.

use std::{
    collections::BTreeMap,
    fmt,
    str::FromStr,
    sync::{Mutex, MutexGuard},
};

use familychat_core::identity::APP_ID;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secret::{Passphrase, REDACTED, Secret};

/// The keychain service every entry is stored under: the app id.
pub const SERVICE: &str = APP_ID;

/// An account's local id: a random UUID (version 4) that names its keychain
/// entries and its directory, `accounts/<uuid>/`. It is not derived from
/// the Matrix ID, so it identifies nobody.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(uuid::Uuid);

impl AccountId {
    /// A new random id from the OS CSPRNG.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(uuid::Builder::from_random_bytes(bytes).into_uuid()))
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Hyphenated lower case: `0b5c…-…`.
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountId({self})")
    }
}

impl FromStr for AccountId {
    type Err = InvalidAccountId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let id = uuid::Uuid::try_parse(text).map_err(|_| InvalidAccountId)?;
        // Only the canonical form, so one account has one name on disk and
        // in the keychain.
        if id.hyphenated().to_string() != text {
            return Err(InvalidAccountId);
        }
        Ok(Self(id))
    }
}

/// The text is not an account id in its canonical form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an account id")]
pub struct InvalidAccountId;

/// A named keychain entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeychainEntry {
    /// `app-store`: the app store's passphrase.
    AppStore,
    /// `account-<uuid>-session`: the account's [`AccountSession`].
    AccountSession(AccountId),
    /// `account-<uuid>-store`: the passphrase of the account's SDK stores.
    AccountStore(AccountId),
}

impl KeychainEntry {
    /// The entry's name (the keychain "account" or "user" field).
    pub fn name(&self) -> String {
        match self {
            Self::AppStore => "app-store".to_owned(),
            Self::AccountSession(id) => format!("account-{id}-session"),
            Self::AccountStore(id) => format!("account-{id}-store"),
        }
    }
}

impl fmt::Display for KeychainEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// Why the keychain could not be used. None of these carries a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeychainError {
    /// The keychain is there but locked, or refused access. Unlocking it and
    /// trying again may work.
    #[error("the keychain is locked or refused access: {0}")]
    Locked(String),
    /// The keychain could not be reached: no Secret Service on this Linux
    /// session, or the platform store failed. Trying again may work.
    #[error("the keychain can't be reached: {0}")]
    Unavailable(String),
    /// The entry exists but is not what Family Chat wrote (not text, or not
    /// a passphrase or session it can read).
    #[error("the keychain entry {0} is damaged")]
    Malformed(String),
}

/// Where secrets are kept: the OS keychain in the app ([`OsKeychain`]),
/// memory in tests ([`MemoryKeychain`]).
pub trait Keychain: Send + Sync {
    /// The entry's value, or `None` when there is no such entry.
    fn get(&self, entry: &KeychainEntry) -> Result<Option<Secret<String>>, KeychainError>;
    /// Stores `value` in the entry, replacing what was there.
    fn set(&self, entry: &KeychainEntry, value: &str) -> Result<(), KeychainError>;
    /// Deletes the entry; deleting a missing entry is not an error.
    fn delete(&self, entry: &KeychainEntry) -> Result<(), KeychainError>;
}

/// The OS keychain through `keyring`: the macOS login keychain, Windows
/// Credential Manager, or the Secret Service (GNOME Keyring, KWallet) on
/// Linux.
#[derive(Debug, Clone)]
pub struct OsKeychain {
    service: String,
}

impl OsKeychain {
    /// The keychain under the app's service id, [`SERVICE`].
    pub fn new() -> Self {
        Self {
            service: SERVICE.to_owned(),
        }
    }

    /// The keychain under another service id, so the ignored real-keychain
    /// tests never touch the app's own entries.
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, entry: &KeychainEntry) -> Result<keyring::Entry, KeychainError> {
        keyring::Entry::new(&self.service, &entry.name()).map_err(|error| map_error(error, entry))
    }
}

impl Default for OsKeychain {
    fn default() -> Self {
        Self::new()
    }
}

impl Keychain for OsKeychain {
    fn get(&self, entry: &KeychainEntry) -> Result<Option<Secret<String>>, KeychainError> {
        match self.entry(entry)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_error(error, entry)),
        }
    }

    fn set(&self, entry: &KeychainEntry, value: &str) -> Result<(), KeychainError> {
        self.entry(entry)?
            .set_password(value)
            .map_err(|error| map_error(error, entry))
    }

    fn delete(&self, entry: &KeychainEntry) -> Result<(), KeychainError> {
        match self.entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_error(error, entry)),
        }
    }
}

/// `keyring`'s error as a [`KeychainError`]. The platform's own message is
/// kept for the two failures it explains; the variants that carry the
/// stored bytes or credentials (`BadEncoding`, `Ambiguous`) are reduced to
/// words, so no secret reaches an error message.
fn map_error(error: keyring::Error, entry: &KeychainEntry) -> KeychainError {
    match error {
        keyring::Error::NoStorageAccess(cause) => KeychainError::Locked(cause.to_string()),
        // The Secret Service reports a locked collection as a platform
        // failure ("Cannot create an item in a locked collection").
        keyring::Error::PlatformFailure(cause) if mentions_locked(&cause.to_string()) => {
            KeychainError::Locked(cause.to_string())
        }
        keyring::Error::PlatformFailure(cause) => KeychainError::Unavailable(cause.to_string()),
        keyring::Error::BadEncoding(_) => KeychainError::Malformed(entry.name()),
        keyring::Error::Ambiguous(_) => {
            KeychainError::Unavailable(format!("more than one keychain entry is named {entry}"))
        }
        keyring::Error::TooLong(field, limit) => KeychainError::Unavailable(format!(
            "the keychain refused {entry}: its {field} is longer than {limit}"
        )),
        keyring::Error::Invalid(field, reason) => KeychainError::Unavailable(format!(
            "the keychain refused {entry}: its {field} is invalid ({reason})"
        )),
        keyring::Error::NoEntry => KeychainError::Unavailable(format!("{entry} has no entry")),
        other => KeychainError::Unavailable(format!("unexpected keychain failure: {other}")),
    }
}

/// Whether `message` has the word "locked" (not "blocked").
fn mentions_locked(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.match_indices("locked").any(|(index, _)| {
        !lower[..index]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphabetic())
    })
}

/// How a [`MemoryKeychain`] fails, to test the error states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainFailure {
    /// Every call fails as a locked keychain does.
    Locked,
    /// Every call fails as an unreachable keychain does.
    Unavailable,
    /// Writes succeed and are then forgotten, as a broken Secret Service
    /// can do.
    Forgetful,
}

/// The mock keychain: entries in memory, wiped when dropped, with failures
/// on demand. Used by every test, so none touches the real keychain.
#[derive(Default)]
pub struct MemoryKeychain {
    entries: Mutex<BTreeMap<String, Zeroizing<String>>>,
    failure: Mutex<Option<KeychainFailure>>,
}

impl MemoryKeychain {
    /// An empty keychain that works.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later call fail as `failure` says, or work again with
    /// `None`.
    pub fn fail_with(&self, failure: Option<KeychainFailure>) {
        *lock(&self.failure) = failure;
    }

    /// The names of the entries it holds, sorted.
    pub fn entry_names(&self) -> Vec<String> {
        lock(&self.entries).keys().cloned().collect()
    }

    fn check(&self) -> Result<Option<KeychainFailure>, KeychainError> {
        match *lock(&self.failure) {
            Some(KeychainFailure::Locked) => Err(KeychainError::Locked(
                "the test keychain is locked".to_owned(),
            )),
            Some(KeychainFailure::Unavailable) => Err(KeychainError::Unavailable(
                "the test keychain is unreachable".to_owned(),
            )),
            other => Ok(other),
        }
    }
}

impl fmt::Debug for MemoryKeychain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryKeychain")
            .field("entries", &self.entry_names())
            .field("failure", &*lock(&self.failure))
            .finish()
    }
}

impl Keychain for MemoryKeychain {
    fn get(&self, entry: &KeychainEntry) -> Result<Option<Secret<String>>, KeychainError> {
        self.check()?;
        Ok(lock(&self.entries)
            .get(&entry.name())
            .map(|value| Secret::new(value.to_string())))
    }

    fn set(&self, entry: &KeychainEntry, value: &str) -> Result<(), KeychainError> {
        if self.check()? == Some(KeychainFailure::Forgetful) {
            return Ok(());
        }
        lock(&self.entries).insert(entry.name(), Zeroizing::new(value.to_owned()));
        Ok(())
    }

    fn delete(&self, entry: &KeychainEntry) -> Result<(), KeychainError> {
        self.check()?;
        lock(&self.entries).remove(&entry.name());
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Loads the passphrase in `entry`; `None` when there is no entry.
pub fn load_passphrase(
    keychain: &dyn Keychain,
    entry: &KeychainEntry,
) -> Result<Option<Passphrase>, KeychainError> {
    let Some(text) = keychain.get(entry)? else {
        return Ok(None);
    };
    Passphrase::from_hex(text.expose())
        .map(Some)
        .ok_or_else(|| KeychainError::Malformed(entry.name()))
}

/// Creates a new passphrase in `entry`, replacing what was there, and reads
/// it back before returning it: a keychain that accepts a write and then
/// returns something else (or nothing) would lose the data later, so that
/// is found out before anything is encrypted with it.
pub fn create_passphrase(
    keychain: &dyn Keychain,
    entry: &KeychainEntry,
) -> Result<Passphrase, KeychainError> {
    let passphrase = Passphrase::generate()
        .map_err(|error| KeychainError::Unavailable(format!("no random bytes: {error}")))?;
    keychain.set(entry, &passphrase.to_hex())?;
    match load_passphrase(keychain, entry) {
        Ok(Some(stored)) if stored == passphrase => Ok(passphrase),
        Ok(_) | Err(KeychainError::Malformed(_)) => {
            let _ = keychain.delete(entry);
            Err(KeychainError::Unavailable(format!(
                "the keychain did not return the {entry} passphrase it just stored"
            )))
        }
        Err(error) => Err(error),
    }
}

/// How an account signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// User id and password (`m.login.password`).
    Password,
    /// A one-time login token, such as a Family Chat sign-in code
    /// (`m.login.token`).
    LoginToken,
}

/// What `account-<uuid>-session` holds: everything needed to restore an
/// account's Matrix session. Kept only in the keychain; `Debug` prints
/// [`REDACTED`] for the whole value, since even the ids identify someone.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountSession {
    /// The Matrix user id, `@user:server`.
    pub user_id: String,
    /// The device id the homeserver gave this sign-in.
    pub device_id: String,
    /// The homeserver's client-server API base URL.
    pub homeserver_url: String,
    /// The access token.
    pub access_token: Secret<String>,
    /// The refresh token, when the homeserver issues them.
    pub refresh_token: Option<Secret<String>>,
    /// How the account signed in.
    pub auth_kind: AuthKind,
}

impl fmt::Debug for AccountSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// The format of the session entry this build writes and reads.
const SESSION_FORMAT: u32 = 1;

/// The session entry's JSON, borrowing from an [`AccountSession`] so no
/// secret is copied before it is serialised into wiped memory.
#[derive(Serialize)]
struct SessionRecordOut<'a> {
    format: u32,
    user_id: &'a str,
    device_id: &'a str,
    homeserver_url: &'a str,
    access_token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<&'a str>,
    auth_kind: AuthKind,
}

#[derive(Deserialize)]
struct SessionRecordIn {
    format: u32,
    user_id: String,
    device_id: String,
    homeserver_url: String,
    access_token: String,
    refresh_token: Option<String>,
    auth_kind: AuthKind,
}

impl AccountSession {
    fn to_json(&self) -> Result<Zeroizing<String>, KeychainError> {
        let record = SessionRecordOut {
            format: SESSION_FORMAT,
            user_id: &self.user_id,
            device_id: &self.device_id,
            homeserver_url: &self.homeserver_url,
            access_token: self.access_token.expose(),
            refresh_token: self
                .refresh_token
                .as_ref()
                .map(|token| token.expose().as_str()),
            auth_kind: self.auth_kind,
        };
        serde_json::to_string(&record)
            .map(Zeroizing::new)
            .map_err(|_| KeychainError::Unavailable("the session could not be encoded".to_owned()))
    }

    fn from_json(text: &str, entry: &KeychainEntry) -> Result<Self, KeychainError> {
        // serde_json's error would quote the input; only its position is
        // safe to keep, and even that is not needed.
        let record: SessionRecordIn =
            serde_json::from_str(text).map_err(|_| KeychainError::Malformed(entry.name()))?;
        if record.format != SESSION_FORMAT {
            return Err(KeychainError::Malformed(entry.name()));
        }
        Ok(Self {
            user_id: record.user_id,
            device_id: record.device_id,
            homeserver_url: record.homeserver_url,
            access_token: Secret::new(record.access_token),
            refresh_token: record.refresh_token.map(Secret::new),
            auth_kind: record.auth_kind,
        })
    }
}

/// Saves the account's session in `account-<uuid>-session`.
pub fn save_session(
    keychain: &dyn Keychain,
    account: AccountId,
    session: &AccountSession,
) -> Result<(), KeychainError> {
    keychain.set(&KeychainEntry::AccountSession(account), &session.to_json()?)
}

/// Loads the account's session; `None` when there is none.
pub fn load_session(
    keychain: &dyn Keychain,
    account: AccountId,
) -> Result<Option<AccountSession>, KeychainError> {
    let entry = KeychainEntry::AccountSession(account);
    let Some(text) = keychain.get(&entry)? else {
        return Ok(None);
    };
    AccountSession::from_json(text.expose(), &entry).map(Some)
}

/// Deletes both of the account's entries.
pub fn delete_account_entries(
    keychain: &dyn Keychain,
    account: AccountId,
) -> Result<(), KeychainError> {
    keychain.delete(&KeychainEntry::AccountSession(account))?;
    keychain.delete(&KeychainEntry::AccountStore(account))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> AccountSession {
        AccountSession {
            user_id: "@alice:example.org".to_owned(),
            device_id: "ABCDEFGHIJ".to_owned(),
            homeserver_url: "https://matrix.example.org".to_owned(),
            access_token: Secret::new("syt_YWxpY2U_access_secret".to_owned()),
            refresh_token: Some(Secret::new("syr_YWxpY2U_refresh_secret".to_owned())),
            auth_kind: AuthKind::LoginToken,
        }
    }

    #[test]
    fn entries_have_the_planned_names_under_one_service() {
        let id: AccountId = "0b5c2a1e-7f3d-4c2b-9a8e-1d2c3b4a5f60".parse().expect("id");
        assert_eq!(SERVICE, "family.safechat.desktop");
        assert_eq!(KeychainEntry::AppStore.name(), "app-store");
        assert_eq!(
            KeychainEntry::AccountSession(id).name(),
            "account-0b5c2a1e-7f3d-4c2b-9a8e-1d2c3b4a5f60-session"
        );
        assert_eq!(
            KeychainEntry::AccountStore(id).name(),
            "account-0b5c2a1e-7f3d-4c2b-9a8e-1d2c3b4a5f60-store"
        );
    }

    #[test]
    fn account_ids_are_random_v4_uuids_in_canonical_form() {
        let id = AccountId::generate().expect("random");
        assert_ne!(id, AccountId::generate().expect("random"));
        let text = id.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(text.as_bytes()[14], b'4', "version 4: {text}");
        assert_eq!(text.parse::<AccountId>().expect("parses"), id);
        assert!(text.to_uppercase().parse::<AccountId>().is_err());
        assert!(text.replace('-', "").parse::<AccountId>().is_err());
        assert!("../../etc".parse::<AccountId>().is_err());
    }

    #[test]
    fn a_created_passphrase_loads_back() {
        let keychain = MemoryKeychain::new();
        assert!(
            load_passphrase(&keychain, &KeychainEntry::AppStore)
                .expect("works")
                .is_none()
        );
        let created = create_passphrase(&keychain, &KeychainEntry::AppStore).expect("created");
        let loaded = load_passphrase(&keychain, &KeychainEntry::AppStore)
            .expect("works")
            .expect("present");
        assert_eq!(created, loaded);
        assert_eq!(keychain.entry_names(), ["app-store"]);
    }

    #[test]
    fn a_keychain_that_forgets_fails_the_creation() {
        let keychain = MemoryKeychain::new();
        keychain.fail_with(Some(KeychainFailure::Forgetful));
        assert!(matches!(
            create_passphrase(&keychain, &KeychainEntry::AppStore),
            Err(KeychainError::Unavailable(_))
        ));
    }

    #[test]
    fn a_locked_or_unreachable_keychain_is_an_error() {
        let keychain = MemoryKeychain::new();
        keychain.fail_with(Some(KeychainFailure::Locked));
        assert!(matches!(
            load_passphrase(&keychain, &KeychainEntry::AppStore),
            Err(KeychainError::Locked(_))
        ));
        keychain.fail_with(Some(KeychainFailure::Unavailable));
        assert!(matches!(
            create_passphrase(&keychain, &KeychainEntry::AppStore),
            Err(KeychainError::Unavailable(_))
        ));
        keychain.fail_with(None);
        assert!(keychain.entry_names().is_empty());
    }

    #[test]
    fn keyring_errors_map_to_states_without_secrets() {
        let entry = KeychainEntry::AppStore;
        let locked = keyring::Error::PlatformFailure(
            "DBus error: Cannot create an item in a locked collection".into(),
        );
        assert!(matches!(
            map_error(locked, &entry),
            KeychainError::Locked(_)
        ));
        let denied = keyring::Error::NoStorageAccess("User canceled the operation.".into());
        assert!(matches!(
            map_error(denied, &entry),
            KeychainError::Locked(_)
        ));
        let gone = keyring::Error::PlatformFailure("Unable to autolaunch a dbus-daemon".into());
        assert!(matches!(
            map_error(gone, &entry),
            KeychainError::Unavailable(_)
        ));
        let silent = keyring::Error::PlatformFailure(
            "DBus error: Did not receive a reply. Possible causes include: the remote \
             application did not send a reply, the message bus security policy blocked the reply"
                .into(),
        );
        assert!(matches!(
            map_error(silent, &entry),
            KeychainError::Unavailable(_)
        ));
        let bytes = keyring::Error::BadEncoding(b"secret-bytes".to_vec());
        let mapped = map_error(bytes, &entry);
        assert_eq!(mapped, KeychainError::Malformed("app-store".to_owned()));
        assert!(!mapped.to_string().contains("secret-bytes"));
    }

    #[test]
    fn a_damaged_passphrase_entry_is_malformed() {
        let keychain = MemoryKeychain::new();
        keychain
            .set(&KeychainEntry::AppStore, "not a passphrase")
            .expect("set");
        assert_eq!(
            load_passphrase(&keychain, &KeychainEntry::AppStore).expect_err("damaged"),
            KeychainError::Malformed("app-store".to_owned())
        );
    }

    #[test]
    fn sessions_round_trip_and_never_print() {
        let keychain = MemoryKeychain::new();
        let id = AccountId::generate().expect("random");
        assert!(load_session(&keychain, id).expect("works").is_none());
        let session = session();
        save_session(&keychain, id, &session).expect("saved");
        let loaded = load_session(&keychain, id)
            .expect("works")
            .expect("present");
        assert_eq!(loaded, session);
        assert_eq!(format!("{loaded:?}"), REDACTED);
        assert_eq!(format!("{:?}", loaded.access_token), REDACTED);

        // No refresh token is a valid session too.
        let mut without = session.clone();
        without.refresh_token = None;
        save_session(&keychain, id, &without).expect("saved");
        assert_eq!(
            load_session(&keychain, id)
                .expect("works")
                .expect("present"),
            without
        );

        create_passphrase(&keychain, &KeychainEntry::AccountStore(id)).expect("store");
        assert_eq!(keychain.entry_names().len(), 2);
        delete_account_entries(&keychain, id).expect("deleted");
        assert!(keychain.entry_names().is_empty());
        delete_account_entries(&keychain, id).expect("deleting nothing is fine");
    }

    #[test]
    fn a_damaged_session_is_malformed_and_its_error_quotes_nothing() {
        let keychain = MemoryKeychain::new();
        let id = AccountId::generate().expect("random");
        let entry = KeychainEntry::AccountSession(id);
        keychain
            .set(&entry, r#"{"format":1,"access_token":"syt_leaked"}"#)
            .expect("set");
        let error = load_session(&keychain, id).expect_err("damaged");
        assert!(matches!(error, KeychainError::Malformed(_)));
        assert!(!error.to_string().contains("syt_leaked"), "{error}");
        keychain
            .set(
                &entry,
                &session()
                    .to_json()
                    .expect("json")
                    .replace("\"format\":1", "\"format\":9"),
            )
            .expect("set");
        assert!(matches!(
            load_session(&keychain, id),
            Err(KeychainError::Malformed(_))
        ));
    }

    #[test]
    fn the_memory_keychain_debug_shows_names_not_values() {
        let keychain = MemoryKeychain::new();
        create_passphrase(&keychain, &KeychainEntry::AppStore).expect("created");
        let passphrase = load_passphrase(&keychain, &KeychainEntry::AppStore)
            .expect("works")
            .expect("present");
        let debug = format!("{keychain:?}");
        assert!(debug.contains("app-store"));
        assert!(!debug.contains(passphrase.to_hex().as_str()));
    }

    /// The real OS keychain, under a test-only service id. Ignored: run it
    /// by hand on a desktop session with
    /// `cargo test -p familychat-security -- --ignored real_keychain`.
    #[test]
    #[ignore = "touches the real OS keychain"]
    fn real_keychain_round_trip() {
        let keychain = OsKeychain::with_service(format!("{SERVICE}.test"));
        let id = AccountId::generate().expect("random");
        let entry = KeychainEntry::AccountStore(id);
        let created = create_passphrase(&keychain, &entry).expect("created");
        let loaded = load_passphrase(&keychain, &entry)
            .expect("works")
            .expect("present");
        assert_eq!(created, loaded);
        save_session(&keychain, id, &session()).expect("saved");
        assert_eq!(
            load_session(&keychain, id)
                .expect("works")
                .expect("present"),
            session()
        );
        delete_account_entries(&keychain, id).expect("deleted");
        assert!(load_passphrase(&keychain, &entry).expect("works").is_none());
    }
}
