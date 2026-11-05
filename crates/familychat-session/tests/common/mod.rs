//! What the session's integration tests share: a throwaway profile with the
//! mock keychain, sessions for it, waiting on an account's status, and log
//! capture through the app's own logging pipeline.

#![allow(dead_code)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use familychat_core::sync::{AccountStatus, SyncState};
use familychat_security::{AccountSession, AuthKind, Keychain, MemoryKeychain, Secret, logging};
use familychat_session::{AccountManager, SessionConfig};
use familychat_store::{DataDir, Profile, ProfileState};
use tokio::sync::watch;
use tracing_subscriber::fmt::MakeWriter;

/// A profile in a fresh temporary directory, with the mock keychain. The
/// directory is removed when it is dropped.
pub struct TestProfile {
    pub root: PathBuf,
    pub keychain: Arc<MemoryKeychain>,
}

impl TestProfile {
    pub fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            root: std::env::temp_dir().join(format!(
                "familychat-session-{name}-{}-{nanos}",
                std::process::id()
            )),
            keychain: Arc::new(MemoryKeychain::new()),
        }
    }

    pub fn dir(&self) -> DataDir {
        DataDir::at(&self.root)
    }

    pub fn open(&self) -> Result<Profile> {
        let keychain: Arc<dyn Keychain> = self.keychain.clone();
        match Profile::open(&self.dir(), keychain)? {
            ProfileState::Ready(profile) => Ok(profile),
            other => bail!("the test profile did not open: {other:?}"),
        }
    }

    /// An account manager over the profile (which it then holds, lock
    /// included, until it is dropped).
    pub fn manager(&self, http: reqwest::Client) -> Result<AccountManager> {
        Ok(AccountManager::new(
            self.open()?,
            SessionConfig::with_http_client(http),
        ))
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// An account session for the keychain.
pub fn account_session(
    user_id: &str,
    device_id: &str,
    homeserver_url: &str,
    access_token: &str,
    refresh_token: Option<&str>,
) -> AccountSession {
    AccountSession {
        user_id: user_id.to_owned(),
        device_id: device_id.to_owned(),
        homeserver_url: homeserver_url.to_owned(),
        access_token: Secret::new(access_token.to_owned()),
        refresh_token: refresh_token.map(|token| Secret::new(token.to_owned())),
        auth_kind: AuthKind::Password,
    }
}

/// Waits until the account's sync state passes `accept`, failing after
/// `timeout` with the last state seen.
pub async fn wait_for_sync(
    status: &mut watch::Receiver<AccountStatus>,
    timeout: Duration,
    what: &str,
    accept: impl Fn(&SyncState) -> bool,
) -> Result<SyncState> {
    let waited = tokio::time::timeout(timeout, async {
        loop {
            let current = status.borrow_and_update().sync.clone();
            if accept(&current) {
                return Ok(current);
            }
            if status.changed().await.is_err() {
                bail!("the account went away while waiting for {what}");
            }
        }
    })
    .await;
    match waited {
        Ok(result) => result,
        Err(_) => bail!(
            "no {what} within {timeout:?}; the account is {:?}",
            status.borrow().sync
        ),
    }
}

/// Every log line the test binary writes, through the app's own logging
/// pipeline (the default directives with Family Chat's crates at debug,
/// and the redacting writer), from the first call on.
pub fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
    static CAPTURED: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    CAPTURED
        .get_or_init(|| {
            let buffer = Arc::new(Mutex::new(Vec::new()));
            let (filter, _) = logging::filter_from(Some(&format!(
                "{},familychat=debug",
                logging::DEFAULT_DIRECTIVES
            )));
            let subscriber = logging::subscriber(Captured(buffer.clone()), filter);
            tracing::subscriber::set_global_default(subscriber)
                .expect("the test binary's only log subscriber");
            buffer
        })
        .clone()
}

/// The captured log, as text.
pub fn log_text() -> String {
    let buffer = captured_logs();
    let bytes = buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    String::from_utf8_lossy(&bytes).into_owned()
}

#[derive(Clone)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Every file under `dir`, recursively.
pub fn files_under(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in
            std::fs::read_dir(&next).with_context(|| format!("reading {}", next.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    Ok(files)
}

/// Fails if any file under `dir` contains `needle`; returns how many bytes
/// it read, so a caller can check the scan was not vacuous.
pub fn assert_not_in_files(dir: &Path, needle: &str) -> Result<u64> {
    let mut scanned = 0;
    for file in files_under(dir)? {
        let bytes = std::fs::read(&file)?;
        scanned += bytes.len() as u64;
        if bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
        {
            bail!("{} contains {needle:?} in plaintext", file.display());
        }
    }
    Ok(scanned)
}
