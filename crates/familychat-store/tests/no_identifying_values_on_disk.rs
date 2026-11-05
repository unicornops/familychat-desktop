//! A Matrix ID written to the registry is not found in the bytes of any
//! file in the data directory (ADR 0003; Zmail's `no_plaintext_on_disk.rs`
//! is the pattern). Plaintext preferences are, which shows the scan reads
//! what SQLite wrote.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use familychat_security::{KeychainEntry, MemoryKeychain, keychain::load_passphrase};
use familychat_store::{DataDir, NewAccount, Preference, Profile, ProfileState};

const USER_ID: &str = "@grandma-marker-7f3a:hs1.familychat-marker.test";
const LOCALPART: &str = "grandma-marker-7f3a";
const SERVER: &str = "hs1.familychat-marker.test";
const HOMESERVER_URL: &str = "https://matrix.familychat-marker.test:8448";
const ROOM_ID: &str = "!kitchen-marker-91c2:hs1.familychat-marker.test";
const THEME: &str = "plaintext-theme-marker";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    TempDir(std::env::temp_dir().join(format!(
        "familychat-store-scan-{}-{nanos}",
        std::process::id()
    )))
}

/// Every file under `root`.
fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// The bytes of `path`, or `None` for the two files Windows will not let
/// another handle read while the profile is open: the held lock file
/// (`LockFileEx`), which must be empty, and SQLite's wal-index (`-shm`,
/// byte-range locked), which holds page numbers and hashes, no row data.
/// Every other file must be readable.
fn read(path: &Path) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if cfg!(windows) => {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name == "familychat.lock" {
                assert_eq!(fs::metadata(path).expect("metadata").len(), 0);
                None
            } else if name.ends_with("-shm") {
                None
            } else {
                panic!("{} is unreadable: {error}", path.display())
            }
        }
        Err(error) => panic!("{} is unreadable: {error}", path.display()),
    }
}

/// The files under `root` whose bytes contain `needle`.
fn files_containing(root: &Path, needle: &[u8]) -> Vec<PathBuf> {
    files(root)
        .into_iter()
        .filter(|path| {
            read(path)
                .is_some_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
        })
        .collect()
}

fn assert_nothing_identifying(root: &Path, when: &str, passphrase_hex: &str) {
    let scanned = files(root);
    assert!(
        scanned
            .iter()
            .any(|path| path.ends_with("familychat.sqlite3")),
        "{when}: the store is among {scanned:?}"
    );
    for needle in [
        USER_ID,
        LOCALPART,
        SERVER,
        HOMESERVER_URL,
        ROOM_ID,
        passphrase_hex,
    ] {
        let found = files_containing(root, needle.as_bytes());
        assert!(found.is_empty(), "{when}: {needle:?} found in {found:?}");
    }
    let theme = files_containing(root, THEME.as_bytes());
    assert!(
        !theme.is_empty(),
        "{when}: the plaintext preference should be readable in {scanned:?}"
    );
}

#[test]
fn a_matrix_id_in_the_registry_is_not_in_the_files() {
    let temp = temp_dir();
    let keychain = Arc::new(MemoryKeychain::new());
    let ProfileState::Ready(profile) =
        Profile::open(&DataDir::at(&temp.0), keychain.clone()).expect("opens")
    else {
        panic!("a new profile is ready");
    };
    let passphrase = load_passphrase(keychain.as_ref(), &KeychainEntry::AppStore)
        .expect("readable")
        .expect("created");
    let passphrase_hex = passphrase.to_hex();

    let id = profile
        .add_account(&NewAccount {
            user_id: USER_ID.to_owned(),
            homeserver_url: HOMESERVER_URL.to_owned(),
        })
        .expect("added");
    profile
        .set_ui_state(&format!("last-room/{id}"), &ROOM_ID)
        .expect("saved");
    profile
        .set_preference(Preference::Theme, THEME)
        .expect("saved");
    assert_eq!(profile.accounts().expect("read")[0].user_id, USER_ID);

    // While open: the main file and the write-ahead log.
    assert_nothing_identifying(&temp.0, "open", &passphrase_hex);
    drop(profile);
    // Closed: everything checkpointed into the main file.
    assert_nothing_identifying(&temp.0, "closed", &passphrase_hex);

    // And it all reads back with the key.
    let ProfileState::Ready(profile) =
        Profile::open(&DataDir::at(&temp.0), keychain).expect("opens")
    else {
        panic!("the profile reopens");
    };
    assert_eq!(profile.accounts().expect("read")[0].user_id, USER_ID);
    assert_eq!(
        profile
            .ui_state::<String>(&format!("last-room/{id}"))
            .expect("read")
            .as_deref(),
        Some(ROOM_ID)
    );
}
