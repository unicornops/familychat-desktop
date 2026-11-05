//! The account manager: one [`Account`] per registered account, restored
//! from the keychain at start, and new sign-ins added through the
//! post-login check (PLAN.md, sections 5.1 and 6.6).

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock},
};

use familychat_core::session::{AccountKey, AccountView};
use familychat_security::{
    AccountId, AccountSession, Keychain, Secret,
    keychain::{load_session, save_session},
};
use familychat_store::{DataDir, NewAccount, Profile};
use matrix_sdk::ruma::UserId;
use reqwest::Url;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::{
    SessionConfig,
    account::{Account, OpenError, OpenParams},
    encryption::VerificationFeed,
    notifications::NotificationFeed,
};

/// How many times [`AccountManager::forget`] tries to delete the files.
const FORGET_ATTEMPTS: u32 = 20;
/// The wait between those tries.
const FORGET_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// The check every new sign-in passes before its account syncs (PLAN.md,
/// section 6.6): the client's homeserver and user, after the session is
/// restored. For a Family Chat sign-in, Phase 4's allowlist re-check goes
/// here; a refusal signs the new device out and forgets the account.
pub trait PostLoginCheck: Send + Sync {
    /// `Ok` to keep the sign-in, or the sentence to refuse it with.
    fn check(&self, homeserver: &Url, user_id: &UserId) -> Result<(), String>;
}

/// Accepts every homeserver: for other Matrix servers, and for tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnyHomeserver;

impl PostLoginCheck for AnyHomeserver {
    fn check(&self, _homeserver: &Url, _user_id: &UserId) -> Result<(), String> {
        Ok(())
    }
}

/// Why a new sign-in could not be added.
#[derive(Debug, thiserror::Error)]
pub enum AddAccountError {
    /// The registry could not record it.
    #[error(transparent)]
    Store(#[from] familychat_store::StoreError),
    /// The keychain could not keep it.
    #[error(transparent)]
    Keychain(#[from] familychat_security::KeychainError),
    /// The account could not be opened (refusals included); nothing of it
    /// is kept.
    #[error(transparent)]
    Open(#[from] OpenError),
}

/// How restoring one account went.
#[derive(Debug)]
pub struct Restored {
    /// The account.
    pub id: AccountId,
    /// `Ok` when it is open and syncing.
    pub result: Result<(), OpenError>,
}

/// The accounts of one profile.
pub struct AccountManager {
    profile: Mutex<Profile>,
    keychain: Arc<dyn Keychain>,
    dir: DataDir,
    config: SessionConfig,
    accounts: RwLock<BTreeMap<AccountId, Arc<Account>>>,
    views: watch::Sender<Vec<AccountView>>,
    notifications: NotificationFeed,
    verifications: VerificationFeed,
}

impl std::fmt::Debug for AccountManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountManager")
            .field("dir", &self.dir.root())
            .finish_non_exhaustive()
    }
}

impl AccountManager {
    /// The manager of an open profile (whose lock it now holds).
    pub fn new(profile: Profile, config: SessionConfig) -> Self {
        let keychain = profile.keychain();
        let dir = profile.dir().clone();
        Self {
            profile: Mutex::new(profile),
            keychain,
            dir,
            config,
            accounts: RwLock::new(BTreeMap::new()),
            views: watch::channel(Vec::new()).0,
            notifications: NotificationFeed::default(),
            verifications: VerificationFeed::default(),
        }
    }

    fn profile(&self) -> MutexGuard<'_, Profile> {
        self.profile
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Opens every account in the registry from its keychain session, with
    /// no sign-in. Each account's result is reported; one that fails (its
    /// session is gone from the keychain, its server does not support
    /// native sliding sync) does not stop the others.
    pub async fn restore_all(&self) -> Result<Vec<Restored>, familychat_store::StoreError> {
        let registered = self.profile().accounts()?;
        let mut restored = Vec::with_capacity(registered.len());
        for summary in registered {
            if self.account(summary.id).is_some() {
                continue;
            }
            let result = self.restore(summary.id).await;
            if let Err(error) = &result {
                warn!(account = %summary.id, %error, "the account could not be restored");
            }
            restored.push(Restored {
                id: summary.id,
                result: result.map(|_| ()),
            });
        }
        Ok(restored)
    }

    async fn restore(&self, id: AccountId) -> Result<Arc<Account>, OpenError> {
        let session = load_session(self.keychain.as_ref(), id)?.ok_or(OpenError::NoSession)?;
        let passphrase = self.profile().account_store_passphrase(id)?;
        let account = Account::open(
            OpenParams {
                id,
                session,
                passphrase,
                dir: self.dir.clone(),
                config: self.config.clone(),
                keychain: self.keychain.clone(),
                notifications: self.notifications.clone(),
                verifications: self.verifications.clone(),
            },
            None,
            None,
        )
        .await?;
        info!(account = %id, "restored the account");
        Ok(self.insert(account))
    }

    /// Adds a new sign-in: records it in the registry, keeps its session
    /// in the keychain, opens it, runs `check`, and starts its sync. When
    /// anything fails (the check included) the account is forgotten again:
    /// its keychain entries, its files and its registry row.
    pub async fn add_account(
        &self,
        session: AccountSession,
        check: &dyn PostLoginCheck,
    ) -> Result<AccountId, AddAccountError> {
        self.add_account_with_password(session, check, None).await
    }

    /// [`Self::add_account`] for a password sign-in: `password` answers the
    /// server's user-interactive auth if it asks for it when the account's
    /// cross-signing identity is made (ADR 0009), and is wiped once that is
    /// done. It is never stored.
    pub async fn add_account_with_password(
        &self,
        session: AccountSession,
        check: &dyn PostLoginCheck,
        password: Option<Secret<String>>,
    ) -> Result<AccountId, AddAccountError> {
        let id = self.profile().add_account(&NewAccount {
            user_id: session.user_id.clone(),
            homeserver_url: session.homeserver_url.clone(),
        })?;
        let opened = async {
            save_session(self.keychain.as_ref(), id, &session)?;
            let passphrase = self.profile().account_store_passphrase(id)?;
            let account = Account::open(
                OpenParams {
                    id,
                    session,
                    passphrase,
                    dir: self.dir.clone(),
                    config: self.config.clone(),
                    keychain: self.keychain.clone(),
                    notifications: self.notifications.clone(),
                    verifications: self.verifications.clone(),
                },
                Some(check),
                password,
            )
            .await?;
            Ok::<_, AddAccountError>(account)
        }
        .await;
        match opened {
            Ok(account) => {
                info!(account = %id, "added the account");
                self.insert(account);
                Ok(id)
            }
            Err(error) => {
                self.forget(id).await;
                Err(error)
            }
        }
    }

    /// Forgets an account that never opened: its keychain entries, its
    /// files and its registry row. The SDK's stores close as the client's
    /// last tasks end, and Windows refuses to delete an open file, so the
    /// files are tried a few times.
    async fn forget(&self, id: AccountId) {
        for attempt in 1..=FORGET_ATTEMPTS {
            match self.profile().remove_account(id) {
                Ok(_) => return,
                Err(error) if attempt < FORGET_ATTEMPTS => {
                    info!(account = %id, %error, "the account's files are still open; trying again");
                }
                Err(error) => {
                    warn!(account = %id, %error, "could not forget the account that failed to open");
                    return;
                }
            }
            tokio::time::sleep(FORGET_RETRY_DELAY).await;
        }
    }

    fn insert(&self, account: Account) -> Arc<Account> {
        let account = Arc::new(account);
        let mut accounts = self
            .accounts
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        accounts.insert(account.id(), account.clone());
        self.views.send_replace(
            accounts
                .values()
                .map(|account| account.view().clone())
                .collect(),
        );
        account
    }

    /// The open accounts, as they change.
    pub fn accounts(&self) -> watch::Receiver<Vec<AccountView>> {
        self.views.subscribe()
    }

    /// An open account.
    pub fn account(&self, id: AccountId) -> Option<Arc<Account>> {
        self.accounts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    /// An open account, by the key the frontends use.
    pub fn account_by_key(&self, key: &AccountKey) -> Option<Arc<Account>> {
        let id = key.0.parse().ok()?;
        self.account(id)
    }

    /// Stops every account's sync. The stores close when the accounts are
    /// dropped.
    pub async fn shutdown(&self) {
        let accounts: Vec<_> = self
            .accounts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for account in accounts {
            account.shutdown().await;
        }
    }

    /// Where every account's notifications go.
    pub(crate) fn notification_feed(&self) -> &NotificationFeed {
        &self.notifications
    }

    /// Where every account's incoming verification requests go.
    pub(crate) fn verification_feed(&self) -> &VerificationFeed {
        &self.verifications
    }

    /// Runs `with` on the profile (the registry, preferences, UI state).
    pub fn with_profile<T>(&self, with: impl FnOnce(&Profile) -> T) -> T {
        with(&self.profile())
    }
}
