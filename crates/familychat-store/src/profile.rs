//! Opening a profile: the lock, the keychain, the app store, and the two
//! states that are not an open profile (ADR 0003; Zmail's ADR 0005 is the
//! pattern).
//!
//! [`Profile::open`] takes the profile lock first, so nothing else is
//! touched while another process has the profile, and then gives one of:
//!
//! - [`ProfileState::Ready`]: the store is open.
//! - [`ProfileState::KeychainUnavailable`]: the keychain is locked or can't
//!   be reached. Nothing was created or changed; [`KeychainUnavailable::retry`]
//!   tries again. There is no plaintext fallback.
//! - [`ProfileState::Unreadable`]: there is data, and its key is gone (or the
//!   keychain's key doesn't open it, or the file is damaged). Nobody can
//!   read it. [`Unreadable::start_over`] moves it aside to
//!   `unreadable-<time>/` (kept, in case the key comes back) and starts a
//!   new store.
//!
//! Both non-ready states keep holding the profile lock, so the screen that
//! shows them owns the profile until the user retries, starts over or quits.

use std::{fmt, fs, io, path::PathBuf, sync::Arc, time::SystemTime};

use familychat_core::identity::APP_NAME;
use familychat_security::{
    AccountId, Keychain, KeychainEntry, KeychainError, Passphrase,
    keychain::{create_passphrase, delete_account_entries, load_passphrase},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    app_store::{self, AccountSummary, AppStore, NewAccount, Preference, Probe, StoreError},
    lock::{LockError, ProfileLock},
    paths::{DataDir, UNREADABLE_PREFIX},
};

/// Why a profile could not be opened at all.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Another process has the profile open, or the lock failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// The app store failed in a way neither state covers (a full disk, a
    /// store from a newer version).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Moving unreadable data aside failed.
    #[error("failed to move the unreadable data aside: {0}")]
    MoveAside(io::Error),
}

/// The profile once [`Profile::open`] has looked at it.
#[derive(Debug)]
pub enum ProfileState {
    /// Open and ready.
    Ready(Profile),
    /// The keychain is locked or can't be reached: show the reason and
    /// Retry.
    KeychainUnavailable(KeychainUnavailable),
    /// The data can't be read: show the recovery screen with Start Over.
    Unreadable(Unreadable),
}

/// The lock and what opening needs, carried through the states.
struct Held {
    lock: ProfileLock,
    keychain: Arc<dyn Keychain>,
}

impl Held {
    fn dir(&self) -> &DataDir {
        self.lock.dir()
    }
}

/// An open profile: its directory, the held lock, the keychain and the app
/// store.
pub struct Profile {
    held: Held,
    store: AppStore,
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("dir", &self.held.dir().root())
            .finish_non_exhaustive()
    }
}

impl Profile {
    /// Opens the profile in `dir` with secrets from `keychain`.
    pub fn open(dir: &DataDir, keychain: Arc<dyn Keychain>) -> Result<ProfileState, OpenError> {
        Self::open_locked(ProfileLock::acquire(dir)?, keychain)
    }

    /// Opens the profile whose lock the caller already holds: the app takes
    /// the lock first, then starts logging into the profile, then opens it.
    pub fn open_locked(
        lock: ProfileLock,
        keychain: Arc<dyn Keychain>,
    ) -> Result<ProfileState, OpenError> {
        open_locked(Held { lock, keychain })
    }

    /// The profile's data directory.
    pub fn dir(&self) -> &DataDir {
        self.held.dir()
    }

    /// The keychain the profile was opened with, for the account sessions
    /// (`account-<uuid>-session`) the session crate keeps.
    pub fn keychain(&self) -> Arc<dyn Keychain> {
        self.held.keychain.clone()
    }

    /// The accounts registry, in the order the accounts were added.
    pub fn accounts(&self) -> Result<Vec<AccountSummary>, StoreError> {
        self.store.accounts()
    }

    /// Adds an account to the registry.
    pub fn add_account(&self, account: &NewAccount) -> Result<AccountId, StoreError> {
        self.store.add_account(account)
    }

    /// Adds an account under an id made beforehand: a QR code sign-in
    /// makes the account's stores before the server says who signed in
    /// (ADR 0010).
    pub fn add_account_with_id(
        &self,
        id: AccountId,
        account: &NewAccount,
    ) -> Result<(), StoreError> {
        self.store.add_account_with_id(id, account)
    }

    /// Removes an account: its keychain entries, its directory and its
    /// registry row, in that order, so its secrets go first. `false` when
    /// it was not in the registry.
    pub fn remove_account(&self, account: AccountId) -> Result<bool, RemoveAccountError> {
        delete_account_entries(self.held.keychain.as_ref(), account)?;
        match fs::remove_dir_all(self.held.dir().account_dir(account)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(RemoveAccountError::Files(error)),
        }
        Ok(self.store.remove_account(account)?)
    }

    /// The passphrase for the account's SDK stores (`account-<uuid>-store`),
    /// created on first use.
    pub fn account_store_passphrase(
        &self,
        account: AccountId,
    ) -> Result<Passphrase, KeychainError> {
        let entry = KeychainEntry::AccountStore(account);
        let keychain = self.held.keychain.as_ref();
        match load_passphrase(keychain, &entry)? {
            Some(passphrase) => Ok(passphrase),
            None => create_passphrase(keychain, &entry),
        }
    }

    /// A plaintext preference.
    pub fn preference(&self, preference: Preference) -> Result<Option<String>, StoreError> {
        self.store.preference(preference)
    }

    /// Sets a plaintext preference. Never put anything identifying here.
    pub fn set_preference(&self, preference: Preference, value: &str) -> Result<(), StoreError> {
        self.store.set_preference(preference, value)
    }

    /// Encrypted UI state under `key`.
    pub fn ui_state<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StoreError> {
        self.store.ui_state(key)
    }

    /// Saves encrypted UI state under `key`.
    pub fn set_ui_state(&self, key: &str, value: &impl Serialize) -> Result<(), StoreError> {
        self.store.set_ui_state(key, value)
    }

    /// The account's draft for `room` (ADR 0012), decrypted.
    pub fn draft<T: DeserializeOwned>(
        &self,
        account: AccountId,
        room: &str,
    ) -> Result<Option<T>, StoreError> {
        self.store.draft(account, room)
    }

    /// Keeps the account's draft for `room`, encrypted (`None` forgets
    /// it). Removing the account removes its drafts.
    pub fn set_draft(
        &self,
        account: AccountId,
        room: &str,
        draft: Option<&impl Serialize>,
    ) -> Result<(), StoreError> {
        self.store.set_draft(account, room, draft)
    }
}

/// Why an account could not be removed.
#[derive(Debug, thiserror::Error)]
pub enum RemoveAccountError {
    /// Its keychain entries could not be deleted.
    #[error(transparent)]
    Keychain(#[from] KeychainError),
    /// Its directory could not be deleted.
    #[error("failed to delete the account's files: {0}")]
    Files(io::Error),
    /// Its registry row could not be deleted.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The keychain is locked or can't be reached. Nothing was created or
/// changed.
pub struct KeychainUnavailable {
    held: Held,
    error: KeychainError,
}

impl fmt::Debug for KeychainUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeychainUnavailable")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl KeychainUnavailable {
    /// What went wrong.
    pub fn error(&self) -> &KeychainError {
        &self.error
    }

    /// What to tell the user, with what to do next.
    pub fn message(&self) -> String {
        keychain_unavailable_message(&self.error)
    }

    /// Tries again, still holding the profile.
    pub fn retry(self) -> Result<ProfileState, OpenError> {
        open_locked(self.held)
    }
}

/// What [`KeychainUnavailable::message`] says for `error`: what went wrong,
/// what to do next, and that there is no plaintext fallback. Public so a
/// screenshot fixture shows the same sentence without a keychain.
pub fn keychain_unavailable_message(error: &KeychainError) -> String {
    keychain_unavailable_message_with_hint(error, KEYCHAIN_HINT)
}

/// [`keychain_unavailable_message`] with Linux's hint, whatever the build
/// targets: the fixtures draw the Linux "no Secret Service" case, and their
/// snapshots must read the same on every OS.
pub fn linux_keychain_unavailable_message(error: &KeychainError) -> String {
    keychain_unavailable_message_with_hint(error, LINUX_KEYCHAIN_HINT)
}

fn keychain_unavailable_message_with_hint(error: &KeychainError, hint: &str) -> String {
    let what = match error {
        KeychainError::Locked(_) => format!(
            "{APP_NAME} can't open its data because the keychain is locked. Unlock it and try \
             again."
        ),
        _ => format!(
            "{APP_NAME} can't reach the keychain, where it keeps the key to its data.\
             {hint} Try again once it is available."
        ),
    };
    format!("{what} {APP_NAME} never keeps its data without encryption. ({error})")
}

/// What [`Unreadable::message`] says for `reason`: what happened, what
/// Start Over does and what is lost. Public for the screenshot fixture.
pub fn unreadable_message(reason: UnreadableReason) -> String {
    let what = match reason {
        UnreadableReason::KeyMissing => format!(
            "{APP_NAME}'s data on this computer is encrypted, and its key is no longer in the \
             keychain (after a system reinstall, a keychain reset, or copying the data from \
             another computer), so nobody can read it."
        ),
        UnreadableReason::WrongKey => format!(
            "{APP_NAME}'s data on this computer is encrypted, and the key in the keychain \
             doesn't open it, so nobody can read it."
        ),
        UnreadableReason::Damaged => {
            format!("{APP_NAME}'s data on this computer is damaged and can't be read.")
        }
    };
    format!(
        "{what} Start over moves it aside, in case the key turns up, and you sign in again. \
         Your messages are safe on your homeserver; to read encrypted messages again you need \
         your recovery key or another signed-in device."
    )
}

/// Where to look on Linux when the keychain can't be reached.
const LINUX_KEYCHAIN_HINT: &str = " On Linux it needs a Secret Service, such as GNOME Keyring \
                                   or KWallet, running in your session.";
/// Where to look when the keychain can't be reached.
#[cfg(all(unix, not(target_os = "macos")))]
const KEYCHAIN_HINT: &str = LINUX_KEYCHAIN_HINT;
#[cfg(not(all(unix, not(target_os = "macos"))))]
const KEYCHAIN_HINT: &str = "";

/// Why the data can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableReason {
    /// The keychain has no key for it (a reinstalled system, a reset
    /// keychain, a data directory copied from another computer).
    KeyMissing,
    /// The keychain's key does not open it.
    WrongKey,
    /// The store file is damaged.
    Damaged,
}

/// There is data that nobody can read. The recovery screen.
pub struct Unreadable {
    held: Held,
    reason: UnreadableReason,
}

impl fmt::Debug for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unreadable")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

/// What [`Unreadable::start_over`] did.
#[derive(Debug)]
pub struct StartedOver {
    /// Where the old data went.
    pub moved_to: PathBuf,
    /// The profile after starting over: ready, unless the keychain failed
    /// while making the new key.
    pub state: ProfileState,
}

impl Unreadable {
    /// Why it can't be read.
    pub fn reason(&self) -> UnreadableReason {
        self.reason
    }

    /// What to tell the user, with what Start Over does and what is lost.
    pub fn message(&self) -> String {
        unreadable_message(self.reason)
    }

    /// Tries again, still holding the profile (if the keychain was restored
    /// meanwhile).
    pub fn retry(self) -> Result<ProfileState, OpenError> {
        open_locked(self.held)
    }

    /// Moves the unreadable data (the app store and `accounts/`) aside to
    /// `unreadable-<time>/` and opens a new store.
    pub fn start_over(self) -> Result<StartedOver, OpenError> {
        let moved_to = move_aside(self.held.dir()).map_err(OpenError::MoveAside)?;
        tracing::warn!(
            reason = ?self.reason,
            "moved unreadable data aside to {}",
            moved_to.display()
        );
        let state = open_locked(self.held)?;
        Ok(StartedOver { moved_to, state })
    }
}

/// Opens the profile whose lock `held` has.
fn open_locked(held: Held) -> Result<ProfileState, OpenError> {
    let path = held.dir().store_path();
    let probe = match app_store::probe(&path) {
        Ok(probe) => probe,
        Err(StoreError::Damaged(_)) => return Ok(unreadable(held, UnreadableReason::Damaged)),
        Err(error) => return Err(error.into()),
    };
    let keychain = held.keychain.as_ref();
    let passphrase = match (load_passphrase(keychain, &KeychainEntry::AppStore), probe) {
        (Ok(Some(passphrase)), _) => passphrase,
        (Ok(None), Probe::Encrypted) => return Ok(unreadable(held, UnreadableReason::KeyMissing)),
        // A damaged entry is as good as none: with data it can't be the
        // data's key; without, it is replaced.
        (Err(KeychainError::Malformed(_)), Probe::Encrypted) => {
            return Ok(unreadable(held, UnreadableReason::KeyMissing));
        }
        (Ok(None) | Err(KeychainError::Malformed(_)), Probe::Absent | Probe::Empty) => {
            match create_passphrase(keychain, &KeychainEntry::AppStore) {
                Ok(passphrase) => passphrase,
                Err(error) => return Ok(keychain_unavailable(held, error)),
            }
        }
        (Err(error), _) => return Ok(keychain_unavailable(held, error)),
    };
    let store = match probe {
        Probe::Encrypted => match AppStore::open(&path, &passphrase) {
            Ok(store) => store,
            Err(StoreError::WrongKey) => return Ok(unreadable(held, UnreadableReason::WrongKey)),
            Err(StoreError::Damaged(_)) => return Ok(unreadable(held, UnreadableReason::Damaged)),
            Err(error) => return Err(error.into()),
        },
        Probe::Absent | Probe::Empty => AppStore::create(&path, &passphrase)?,
    };
    tracing::info!("opened the app store");
    Ok(ProfileState::Ready(Profile { held, store }))
}

fn unreadable(held: Held, reason: UnreadableReason) -> ProfileState {
    tracing::warn!(?reason, "the app store can't be read");
    ProfileState::Unreadable(Unreadable { held, reason })
}

fn keychain_unavailable(held: Held, error: KeychainError) -> ProfileState {
    tracing::warn!(%error, "the keychain is not available");
    ProfileState::KeychainUnavailable(KeychainUnavailable { held, error })
}

/// Moves the app store (with its journals) and `accounts/` into a new
/// `unreadable-<time>/` in the data directory, and returns it. The logs and
/// the lock stay.
fn move_aside(dir: &DataDir) -> io::Result<PathBuf> {
    let stamp = familychat_security::time::compact(SystemTime::now());
    let mut target = dir.root().join(format!("{UNREADABLE_PREFIX}{stamp}"));
    let mut attempt = 1;
    while target.exists() {
        attempt += 1;
        target = dir
            .root()
            .join(format!("{UNREADABLE_PREFIX}{stamp}-{attempt}"));
    }
    familychat_security::logging::create_private_dir(&target)?;
    let store = dir.store_path();
    let mut moving = vec![dir.accounts_dir()];
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = store.as_os_str().to_owned();
        name.push(suffix);
        moving.push(PathBuf::from(name));
    }
    for from in moving.iter().filter(|path| path.exists()) {
        let Some(name) = from.file_name() else {
            continue;
        };
        fs::rename(from, target.join(name))?;
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use familychat_security::{KeychainFailure, MemoryKeychain};

    use super::*;
    use crate::test_dir::TestDir;

    fn alice() -> NewAccount {
        NewAccount {
            user_id: "@alice:example.org".to_owned(),
            homeserver_url: "https://matrix.example.org".to_owned(),
        }
    }

    /// Opens the profile; see [`crate::test_dir::eventually`] for the retry.
    fn open(dir: &TestDir, keychain: &Arc<MemoryKeychain>) -> ProfileState {
        crate::test_dir::eventually(
            || Profile::open(dir.data_dir(), keychain.clone()),
            |error| matches!(error, OpenError::Lock(LockError::AlreadyOpen { .. })),
        )
        .expect("the profile opens")
    }

    fn ready(state: ProfileState) -> Profile {
        match state {
            ProfileState::Ready(profile) => profile,
            other => panic!("expected a ready profile, got {other:?}"),
        }
    }

    #[test]
    fn a_new_profile_makes_its_key_and_reopens_with_it() {
        let dir = TestDir::new("profile-new");
        let keychain = Arc::new(MemoryKeychain::new());
        let profile = ready(open(&dir, &keychain));
        assert_eq!(keychain.entry_names(), ["app-store"]);
        assert!(profile.accounts().expect("accounts").is_empty());
        let id = profile.add_account(&alice()).expect("added");
        assert_eq!(profile.dir(), dir.data_dir());
        drop(profile);

        let profile = ready(open(&dir, &keychain));
        let accounts = profile.accounts().expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, id);
        assert_eq!(accounts[0].user_id, "@alice:example.org");
        assert_eq!(
            keychain.entry_names(),
            ["app-store"],
            "the same key, not a new one"
        );
    }

    #[test]
    fn a_second_open_of_the_same_profile_is_already_open() {
        let dir = TestDir::new("profile-twice");
        let keychain = Arc::new(MemoryKeychain::new());
        let _first = ready(open(&dir, &keychain));
        match Profile::open(dir.data_dir(), keychain.clone()) {
            Err(OpenError::Lock(LockError::AlreadyOpen { .. })) => {}
            other => panic!("expected already open, got {other:?}"),
        }
    }

    #[test]
    fn a_locked_keychain_creates_nothing_and_retry_recovers() {
        for failure in [
            KeychainFailure::Locked,
            KeychainFailure::Unavailable,
            KeychainFailure::Forgetful,
        ] {
            let dir = TestDir::new("profile-locked-new");
            let keychain = Arc::new(MemoryKeychain::new());
            keychain.fail_with(Some(failure));
            let blocked = match open(&dir, &keychain) {
                ProfileState::KeychainUnavailable(blocked) => blocked,
                other => panic!("{failure:?}: expected the keychain state, got {other:?}"),
            };
            assert!(
                !dir.data_dir().store_path().exists(),
                "{failure:?}: no store without its key"
            );
            assert!(
                blocked
                    .message()
                    .contains("never keeps its data without encryption")
            );
            if failure == KeychainFailure::Locked {
                assert!(
                    blocked.message().contains("keychain is locked"),
                    "{}",
                    blocked.message()
                );
            }
            // Still the owner of the profile while the message shows.
            assert!(matches!(
                Profile::open(dir.data_dir(), keychain.clone()),
                Err(OpenError::Lock(LockError::AlreadyOpen { .. }))
            ));
            keychain.fail_with(None);
            let profile = ready(blocked.retry().expect("retry"));
            assert!(profile.accounts().expect("accounts").is_empty());
        }
    }

    #[test]
    fn a_locked_keychain_leaves_an_existing_store_untouched() {
        let dir = TestDir::new("profile-locked-existing");
        let keychain = Arc::new(MemoryKeychain::new());
        let profile = ready(open(&dir, &keychain));
        profile.add_account(&alice()).expect("added");
        drop(profile);
        let before = dir.snapshot();
        keychain.fail_with(Some(KeychainFailure::Locked));
        let blocked = match open(&dir, &keychain) {
            ProfileState::KeychainUnavailable(blocked) => blocked,
            other => panic!("expected the keychain state, got {other:?}"),
        };
        assert!(matches!(blocked.error(), KeychainError::Locked(_)));
        assert_eq!(dir.snapshot(), before, "nothing was written");
        keychain.fail_with(None);
        let profile = ready(blocked.retry().expect("retry"));
        assert_eq!(profile.accounts().expect("accounts").len(), 1);
    }

    #[test]
    fn a_missing_key_is_unreadable_and_start_over_moves_the_data_aside() {
        let dir = TestDir::new("profile-missing");
        let keychain = Arc::new(MemoryKeychain::new());
        let profile = ready(open(&dir, &keychain));
        let id = profile.add_account(&alice()).expect("added");
        fs::create_dir_all(profile.dir().account_sdk_dir(id)).expect("account dir");
        fs::write(
            profile.dir().account_sdk_dir(id).join("state.sqlite3"),
            b"sdk",
        )
        .expect("sdk");
        drop(profile);
        keychain
            .delete(&KeychainEntry::AppStore)
            .expect("the key is gone");

        let unreadable = match open(&dir, &keychain) {
            ProfileState::Unreadable(unreadable) => unreadable,
            other => panic!("expected unreadable, got {other:?}"),
        };
        assert_eq!(unreadable.reason(), UnreadableReason::KeyMissing);
        assert!(unreadable.message().contains("Start over moves it aside"));
        assert!(
            keychain.entry_names().is_empty(),
            "no new key over the old data"
        );
        // Retrying without the key changes nothing.
        let unreadable = match unreadable.retry().expect("retry") {
            ProfileState::Unreadable(unreadable) => unreadable,
            other => panic!("expected unreadable, got {other:?}"),
        };

        let started = unreadable.start_over().expect("started over");
        let name = started
            .moved_to
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a name");
        assert!(name.starts_with("unreadable-"), "{name}");
        assert_eq!(started.moved_to.parent(), Some(dir.data_dir().root()));
        assert!(started.moved_to.join("familychat.sqlite3").exists());
        assert!(
            started
                .moved_to
                .join("accounts")
                .join(id.to_string())
                .join("sdk")
                .join("state.sqlite3")
                .exists()
        );
        assert!(!dir.data_dir().accounts_dir().exists());
        let profile = ready(started.state);
        assert!(profile.accounts().expect("accounts").is_empty());
        assert_eq!(keychain.entry_names(), ["app-store"]);
        // The moved data is still the old store, untouched.
        assert_eq!(
            app_store::probe(&started.moved_to.join("familychat.sqlite3")).expect("probe"),
            Probe::Encrypted
        );
    }

    #[test]
    fn a_wrong_or_damaged_key_is_unreadable() {
        let dir = TestDir::new("profile-wrong");
        let keychain = Arc::new(MemoryKeychain::new());
        drop(ready(open(&dir, &keychain)));
        create_passphrase(keychain.as_ref(), &KeychainEntry::AppStore).expect("another key");
        match open(&dir, &keychain) {
            ProfileState::Unreadable(unreadable) => {
                assert_eq!(unreadable.reason(), UnreadableReason::WrongKey);
            }
            other => panic!("expected unreadable, got {other:?}"),
        }
        keychain
            .set(&KeychainEntry::AppStore, "damaged")
            .expect("set");
        match open(&dir, &keychain) {
            ProfileState::Unreadable(unreadable) => {
                assert_eq!(unreadable.reason(), UnreadableReason::KeyMissing);
                // Starting over replaces the damaged entry with a new key.
                let started = unreadable.start_over().expect("started over");
                ready(started.state);
                assert!(
                    load_passphrase(keychain.as_ref(), &KeychainEntry::AppStore)
                        .expect("readable")
                        .is_some()
                );
            }
            other => panic!("expected unreadable, got {other:?}"),
        }
    }

    #[test]
    fn a_damaged_store_is_unreadable() {
        let dir = TestDir::new("profile-damaged");
        let keychain = Arc::new(MemoryKeychain::new());
        dir.data_dir().create().expect("dir");
        fs::write(
            dir.data_dir().store_path(),
            b"definitely not an SQLite database, but long enough to look at...",
        )
        .expect("written");
        match open(&dir, &keychain) {
            ProfileState::Unreadable(unreadable) => {
                assert_eq!(unreadable.reason(), UnreadableReason::Damaged);
                assert!(unreadable.message().contains("damaged"));
            }
            other => panic!("expected unreadable, got {other:?}"),
        }
    }

    #[test]
    fn removing_an_account_deletes_its_secrets_files_and_row() {
        let dir = TestDir::new("profile-remove");
        let keychain = Arc::new(MemoryKeychain::new());
        let profile = ready(open(&dir, &keychain));
        let id = profile.add_account(&alice()).expect("added");
        let passphrase = profile.account_store_passphrase(id).expect("created");
        assert_eq!(
            profile.account_store_passphrase(id).expect("loaded"),
            passphrase,
            "created once, then loaded"
        );
        fs::create_dir_all(profile.dir().account_sdk_dir(id)).expect("dir");
        assert!(profile.remove_account(id).expect("removed"));
        assert_eq!(keychain.entry_names(), ["app-store"]);
        assert!(!profile.dir().account_dir(id).exists());
        assert!(profile.accounts().expect("accounts").is_empty());
        assert!(!profile.remove_account(id).expect("already gone"));
    }
}
