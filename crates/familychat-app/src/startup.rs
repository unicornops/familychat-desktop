//! [`LiveStartup`]: the `StartupApi` the window opens the profile through
//! (ADR 0003, ADR 0011).
//!
//! The window's process already holds the profile lock (taken before
//! logging starts). Opening runs on the background runtime: the keychain
//! and the app store are read in a blocking task, then, once the profile
//! is ready, [`AccountManager`] restores the registry's accounts and the
//! window gets a [`LiveSession`] and a [`LiveSignin`] over it. A profile
//! that is not ready keeps its state here (and with it the lock), so Try
//! again and Start over carry on from it; nothing is ever opened without
//! encryption.

use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use familychat_core::{
    identity::APP_NAME,
    startup::{OpenState, Opened, StartupApi},
};
use familychat_security::Keychain;
use familychat_session::{AccountManager, LiveSession, SessionConfig};
use familychat_signin::{LiveSignin, ReqwestTransport};
use familychat_store::{
    DataDir, KeychainUnavailable, Profile, ProfileLock, ProfileState, Unreadable,
};
use tokio::{runtime::Handle, sync::oneshot};

/// Where the profile is between calls.
enum Held {
    /// Locked, not opened yet.
    Locked(ProfileLock),
    /// The keychain was unavailable.
    Keychain(KeychainUnavailable),
    /// The data's key is gone.
    Unreadable(Unreadable),
    /// Open.
    Open {
        manager: Arc<AccountManager>,
        opened: Opened,
    },
    /// Opening failed and dropped the lock: the next try takes it again.
    Released,
}

/// What to do with the held profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Open,
    StartOver,
}

/// Opens the window's profile with the OS keychain on the background
/// runtime.
pub(crate) struct LiveStartup {
    handle: Handle,
    dir: DataDir,
    keychain: Arc<dyn Keychain>,
    held: Arc<Mutex<Option<Held>>>,
}

impl LiveStartup {
    /// The startup of the profile whose `lock` the process holds.
    pub(crate) fn new(lock: ProfileLock, keychain: Arc<dyn Keychain>, handle: Handle) -> Self {
        Self {
            handle,
            dir: lock.dir().clone(),
            keychain,
            held: Arc::new(Mutex::new(Some(Held::Locked(lock)))),
        }
    }

    /// The account manager, once the profile is open (to stop the syncs
    /// when the window closes).
    pub(crate) fn manager(&self) -> Option<Arc<AccountManager>> {
        match &*self.held.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(Held::Open { manager, .. }) => Some(manager.clone()),
            _ => None,
        }
    }

    fn run(&self, step: Step) -> oneshot::Receiver<OpenState> {
        let (answer, receiver) = oneshot::channel();
        let held = self.held.clone();
        let dir = self.dir.clone();
        let keychain = self.keychain.clone();
        let handle = self.handle.clone();
        self.handle.spawn(async move {
            let taken = held.lock().unwrap_or_else(PoisonError::into_inner).take();
            let blocking =
                tokio::task::spawn_blocking(move || advance(taken, step, &dir, keychain));
            let (next, state) = match blocking.await {
                Ok(Advanced::Ready { profile, notice }) => {
                    match open_sessions(profile, notice, &handle).await {
                        Ok((manager, opened)) => (
                            Held::Open {
                                manager,
                                opened: opened.clone(),
                            },
                            OpenState::Ready(opened),
                        ),
                        Err(error) => {
                            tracing::warn!("the sessions could not start: {error:#}");
                            (Held::Released, OpenState::Failed(format!("{error:#}")))
                        }
                    }
                }
                Ok(Advanced::Held(next, state)) => (next, state),
                Err(error) => (
                    Held::Released,
                    OpenState::Failed(format!(
                        "{APP_NAME} stopped while opening its data: {error}"
                    )),
                ),
            };
            *held.lock().unwrap_or_else(PoisonError::into_inner) = Some(next);
            let _ = answer.send(state);
        });
        receiver
    }
}

impl StartupApi for LiveStartup {
    fn open(&self) -> oneshot::Receiver<OpenState> {
        self.run(Step::Open)
    }

    fn start_over(&self) -> oneshot::Receiver<OpenState> {
        self.run(Step::StartOver)
    }
}

/// Where a blocking step left the profile.
enum Advanced {
    /// Ready to open the sessions.
    Ready {
        profile: Profile,
        notice: Option<String>,
    },
    /// Not ready (or already open), and the state to show.
    Held(Held, OpenState),
}

/// Moves the held profile one step on: opens it, tries again, or starts
/// over. Blocks on the keychain and the store.
fn advance(held: Option<Held>, step: Step, dir: &DataDir, keychain: Arc<dyn Keychain>) -> Advanced {
    let mut notice = None;
    let opened = match held {
        Some(Held::Open { manager, opened }) => {
            return Advanced::Held(
                Held::Open {
                    manager,
                    opened: opened.clone(),
                },
                OpenState::Ready(opened),
            );
        }
        Some(Held::Locked(lock)) => Profile::open_locked(lock, keychain),
        Some(Held::Keychain(blocked)) => blocked.retry(),
        Some(Held::Unreadable(unreadable)) if step == Step::StartOver => {
            unreadable.start_over().map(|started| {
                notice = Some(format!(
                    "Moved the data that can't be read to {}. Delete it once you are sure its \
                     key won't turn up.",
                    started.moved_to.display()
                ));
                started.state
            })
        }
        Some(Held::Unreadable(unreadable)) => unreadable.retry(),
        Some(Held::Released) | None => ProfileLock::acquire(dir)
            .map_err(Into::into)
            .and_then(|lock| Profile::open_locked(lock, keychain)),
    };
    match opened {
        Ok(ProfileState::Ready(profile)) => Advanced::Ready { profile, notice },
        Ok(ProfileState::KeychainUnavailable(blocked)) => {
            let message = blocked.message();
            tracing::warn!(error = %blocked.error(), "the keychain is unavailable");
            Advanced::Held(
                Held::Keychain(blocked),
                OpenState::KeychainUnavailable(message),
            )
        }
        Ok(ProfileState::Unreadable(unreadable)) => {
            let message = unreadable.message();
            tracing::warn!(reason = ?unreadable.reason(), "the profile's data can't be read");
            Advanced::Held(Held::Unreadable(unreadable), OpenState::Unreadable(message))
        }
        Err(error) => {
            tracing::warn!("the profile could not be opened: {error}");
            Advanced::Held(Held::Released, OpenState::Failed(error.to_string()))
        }
    }
}

/// The account manager over an open profile, its accounts restored, and
/// the session and sign-in the window draws from.
async fn open_sessions(
    profile: Profile,
    notice: Option<String>,
    handle: &Handle,
) -> Result<(Arc<AccountManager>, Opened)> {
    let config = SessionConfig::new().context("failed to build the session's HTTP client")?;
    let manager = Arc::new(AccountManager::new(profile, config));
    let restored = manager
        .restore_all()
        .await
        .context("failed to read the accounts registry")?;
    let failed = restored
        .iter()
        .filter(|account| account.result.is_err())
        .count();
    tracing::info!(
        restored = restored.len() - failed,
        failed,
        "restored the accounts"
    );
    let transport = ReqwestTransport::new().context("failed to build the sign-in's HTTP client")?;
    let signin = LiveSignin::new(manager.clone(), Arc::new(transport), handle.clone());
    let session = LiveSession::new(manager.clone(), handle.clone());
    Ok((
        manager,
        Opened {
            session: Arc::new(session),
            signin: Arc::new(signin),
            notice,
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use familychat_security::{KeychainEntry, KeychainFailure, MemoryKeychain};
    use familychat_session::BackgroundRuntime;

    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "familychat-startup-{name}-{}-{nanos}",
                std::process::id()
            )))
        }

        fn dir(&self) -> DataDir {
            DataDir::at(&self.0)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait(receiver: oneshot::Receiver<OpenState>) -> OpenState {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut receiver = receiver;
        loop {
            match receiver.try_recv() {
                Ok(state) => return state,
                Err(oneshot::error::TryRecvError::Empty) => {
                    assert!(std::time::Instant::now() < deadline, "no answer");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("the startup went away: {error}"),
            }
        }
    }

    #[test]
    fn a_locked_keychain_is_retried_until_it_opens() {
        let runtime = BackgroundRuntime::start(1).expect("the runtime starts");
        let test = TestDir::new("keychain");
        let keychain = Arc::new(MemoryKeychain::new());
        keychain.fail_with(Some(KeychainFailure::Locked));
        let lock = ProfileLock::acquire(&test.dir()).expect("locked");
        let startup = LiveStartup::new(lock, keychain.clone(), runtime.handle().clone());

        let state = wait(startup.open());
        assert!(
            matches!(&state, OpenState::KeychainUnavailable(message) if message.contains("locked")),
            "{state:?}"
        );
        assert!(startup.manager().is_none());
        // The profile is still held: another process could not take it.
        assert!(ProfileLock::acquire(&test.dir()).is_err());

        keychain.fail_with(None);
        let state = wait(startup.open());
        let OpenState::Ready(opened) = state else {
            panic!("ready: {state:?}");
        };
        assert!(opened.session.accounts().borrow().is_empty());
        assert!(startup.manager().is_some());
        // Asking again gives the same open profile.
        assert!(matches!(wait(startup.open()), OpenState::Ready(_)));
    }

    #[test]
    fn a_lost_key_starts_over_with_a_notice() {
        let runtime = BackgroundRuntime::start(1).expect("the runtime starts");
        let test = TestDir::new("lost-key");
        let keychain = Arc::new(MemoryKeychain::new());
        // A first open makes the store and its key; then the key goes.
        drop(Profile::open(&test.dir(), keychain.clone()).expect("opens"));
        keychain
            .delete(&KeychainEntry::AppStore)
            .expect("the key is gone");

        let lock = ProfileLock::acquire(&test.dir()).expect("locked");
        let startup = LiveStartup::new(lock, keychain, runtime.handle().clone());
        let state = wait(startup.open());
        assert!(matches!(state, OpenState::Unreadable(_)), "{state:?}");
        let state = wait(startup.open());
        assert!(
            matches!(state, OpenState::Unreadable(_)),
            "Try again does not start over"
        );
        let OpenState::Ready(opened) = wait(startup.start_over()) else {
            panic!("ready after starting over");
        };
        let notice = opened.notice.expect("a notice");
        assert!(notice.contains("unreadable-"), "{notice}");
    }
}
