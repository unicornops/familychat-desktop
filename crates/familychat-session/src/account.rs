//! One account: its `matrix_sdk::Client`, its `SyncService`, and the
//! supervisor that keeps the sync going and reports its state.
//!
//! The client is built the way ADR 0005 records (PLAN.md, sections 5.4 and
//! 6.6): SQLite stores in `accounts/<uuid>/sdk/` (state, crypto) and
//! `accounts/<uuid>/cache/` (event cache, media), every value encrypted
//! with the passphrase in the keychain's `account-<uuid>-store`; Family
//! Chat's own HTTP client; `respect_login_well_known(false)`, so a login
//! response can't move the account to another server; refresh tokens
//! handled by the SDK, with every rotated token written to the keychain's
//! `account-<uuid>-session` before the request that needed it is retried;
//! native sliding sync, found through `/versions`; and media fetched only
//! through authenticated media, capped and timed (`media`).

use std::{
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};

use familychat_core::{
    encryption::EncryptionStatus,
    session::{AccountKey, AccountView},
    sync::{AccountProblem, AccountStatus, SyncState},
};
use familychat_security::{
    AccountId, AccountSession, Keychain, Passphrase, Secret,
    keychain::{load_session, save_session},
    logging::create_private_dir,
};
use familychat_store::DataDir;
use matrix_sdk::{
    Client, SessionChange, SessionMeta, SessionTokens,
    authentication::matrix::MatrixSession,
    ruma::{OwnedDeviceId, OwnedUserId},
    sliding_sync::VersionBuilder,
};
use matrix_sdk_ui::{
    room_list_service, sync_service::SyncService, unable_to_decrypt_hook::UtdHookManager,
};
use tokio::{
    sync::{Notify, broadcast, watch},
    task::JoinHandle,
};
use tracing::{info, warn};

use crate::{
    PostLoginCheck, SessionConfig,
    backoff::Backoff,
    encryption::{self, EncryptionShared, VerificationFeed},
    failure::Failure,
    media::AuthenticatedMediaFetcher,
    messaging::OpenTimelines,
    notifications::{self, NotificationFeed},
};

/// Offline and server errors: one second, doubling to a minute.
const OFFLINE_BACKOFF: (Duration, Duration) = (Duration::from_secs(1), Duration::from_secs(60));
/// A suspended account: half a minute, doubling to five minutes. Every
/// request a suspended account makes is refused, so it checks rarely.
const SUSPENDED_BACKOFF: (Duration, Duration) = (Duration::from_secs(30), Duration::from_secs(300));
/// Other failures: five seconds, doubling to five minutes.
const ERROR_BACKOFF: (Duration, Duration) = (Duration::from_secs(5), Duration::from_secs(300));

/// What the user sees for a failure the session can't name.
const SYNC_FAILED: &str = "Family Chat couldn't sync this account. It will try again.";

/// Why an account could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The keychain has no session for it.
    #[error("the keychain has no sign-in for this account")]
    NoSession,
    /// The stored session is not a valid Matrix session.
    #[error("the stored sign-in is damaged: {0}")]
    BadSession(String),
    /// The keychain could not be used.
    #[error(transparent)]
    Keychain(#[from] familychat_security::KeychainError),
    /// The account's directories could not be created.
    #[error("the account's files could not be created: {0}")]
    Files(std::io::Error),
    /// The homeserver has no native sliding sync.
    #[error("the homeserver does not support native sliding sync")]
    NotSupported,
    /// The client could not be built or restored.
    #[error("the account could not be opened: {0}")]
    Client(String),
    /// The post-login check refused the new sign-in (PLAN.md, section 6.6);
    /// its device has been signed out. The text is the refusal's wording.
    #[error("{0}")]
    Refused(String),
}

/// The parts of an open account the room list, timelines and actions use.
#[derive(Clone)]
pub(crate) struct Ready {
    pub(crate) client: Client,
    pub(crate) sync: Arc<SyncService>,
    pub(crate) timelines: OpenTimelines,
    /// Every timeline's unable-to-decrypt hook (ADR 0009).
    pub(crate) utd_hook: Arc<UtdHookManager>,
}

/// An account and its supervisor. Dropping it stops the supervisor.
pub struct Account {
    id: AccountId,
    view: AccountView,
    shared: Arc<Shared>,
    supervisor: JoinHandle<()>,
}

/// What the account and its supervisor share.
struct Shared {
    ready: OnceLock<Ready>,
    status: watch::Sender<AccountStatus>,
    /// The unread total (`notifications::unread_total`).
    unread: watch::Sender<u64>,
    /// The account's encryption (ADR 0009).
    encryption: Arc<EncryptionShared>,
    retry: Notify,
}

impl Shared {
    fn set_sync(&self, sync: SyncState) {
        self.status.send_if_modified(|status| {
            let changed = status.sync != sync;
            status.sync = sync;
            changed
        });
    }

    fn set_unread(&self, unread: u64) {
        self.unread.send_if_modified(|current| {
            let changed = *current != unread;
            *current = unread;
            changed
        });
    }

    fn set_problem(&self, problem: Option<AccountProblem>) {
        self.status.send_if_modified(|status| {
            let changed = status.problem != problem;
            status.problem = problem;
            changed
        });
    }
}

/// The key the frontends name an account by.
pub fn account_key(id: AccountId) -> AccountKey {
    AccountKey(id.to_string())
}

/// Everything needed to open an account.
pub(crate) struct OpenParams {
    pub(crate) id: AccountId,
    pub(crate) session: AccountSession,
    pub(crate) passphrase: Passphrase,
    pub(crate) dir: DataDir,
    pub(crate) config: SessionConfig,
    pub(crate) keychain: Arc<dyn Keychain>,
    pub(crate) notifications: NotificationFeed,
    pub(crate) verifications: VerificationFeed,
}

impl Account {
    /// Builds and restores the account's client, then starts its
    /// supervisor on the current Tokio runtime.
    ///
    /// `check` is for a new sign-in: it runs once the client is restored,
    /// before anything syncs, and a server without native sliding sync is
    /// an error. A restored account whose server has no native sliding
    /// sync opens in [`SyncState::NotSupported`] instead, and opens its
    /// client when asked to retry.
    ///
    /// `password` is a password sign-in's, for the server's user-interactive
    /// auth when the account's cross-signing identity is made (ADR 0009);
    /// it is wiped once that is done.
    pub(crate) async fn open(
        params: OpenParams,
        check: Option<&dyn PostLoginCheck>,
        password: Option<Secret<String>>,
    ) -> Result<Self, OpenError> {
        let id = params.id;
        let user_id = OwnedUserId::try_from(params.session.user_id.clone())
            .map_err(|error| OpenError::BadSession(error.to_string()))?;
        let view = AccountView {
            key: account_key(id),
            user_id,
            homeserver_url: params.session.homeserver_url.clone(),
        };
        let (status, _) = watch::channel(AccountStatus {
            sync: SyncState::Connecting,
            problem: None,
        });
        let shared = Arc::new(Shared {
            ready: OnceLock::new(),
            status,
            unread: watch::channel(0).0,
            encryption: Arc::new(EncryptionShared::default()),
            retry: Notify::new(),
        });
        let supervisor = match open_client(&params, &shared, check).await {
            Ok((ready, offline)) => {
                if offline {
                    shared.set_sync(SyncState::Offline {
                        retry_in: OFFLINE_BACKOFF.0,
                    });
                }
                let _ = shared.ready.set(ready.clone());
                tokio::spawn(supervise(id, ready, shared.clone(), password))
            }
            Err(OpenError::NotSupported) if check.is_none() => {
                info!(account = %id, "the homeserver has no native sliding sync");
                shared.set_sync(SyncState::NotSupported);
                tokio::spawn(reopen(params, shared.clone()))
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            id,
            view,
            shared,
            supervisor,
        })
    }

    /// The account's local id.
    pub fn id(&self) -> AccountId {
        self.id
    }

    /// What the frontends show for it.
    pub fn view(&self) -> &AccountView {
        &self.view
    }

    /// Its status, as it changes.
    pub fn status(&self) -> watch::Receiver<AccountStatus> {
        self.shared.status.subscribe()
    }

    /// Its unread total, as it changes.
    pub fn unread(&self) -> watch::Receiver<u64> {
        self.shared.unread.subscribe()
    }

    /// Its encryption, as it changes.
    pub fn encryption(&self) -> watch::Receiver<EncryptionStatus> {
        self.shared.encryption.status.subscribe()
    }

    pub(crate) fn encryption_shared(&self) -> Arc<EncryptionShared> {
        self.shared.encryption.clone()
    }

    /// The account's Matrix client.
    pub fn client(&self) -> Option<&Client> {
        self.shared.ready.get().map(|ready| &ready.client)
    }

    pub(crate) fn ready(&self) -> Option<Ready> {
        self.shared.ready.get().cloned()
    }

    /// Asks a waiting or stopped sync to try again now.
    pub fn retry_sync(&self) {
        self.shared.retry.notify_waiters();
    }

    /// Stops the supervisor and the sync.
    pub async fn shutdown(&self) {
        self.supervisor.abort();
        if let Some(ready) = self.shared.ready.get() {
            ready.sync.stop().await;
        }
        self.shared.set_sync(SyncState::Idle);
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        self.supervisor.abort();
    }
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Builds the client, restores the session and builds the sync service.
/// `true` when the homeserver could not be reached, so the client was built
/// from what is stored and will find out about the server once it syncs.
async fn open_client(
    params: &OpenParams,
    shared: &Arc<Shared>,
    check: Option<&dyn PostLoginCheck>,
) -> Result<(Ready, bool), OpenError> {
    let sdk_dir = params.dir.account_sdk_dir(params.id);
    let cache_dir = params.dir.account_cache_dir(params.id);
    for dir in [&sdk_dir, &cache_dir] {
        create_private_dir(dir).map_err(OpenError::Files)?;
    }
    let passphrase = params.passphrase.to_hex();

    let (client, offline) = match build_client(
        params,
        &sdk_dir,
        &cache_dir,
        &passphrase,
        VersionBuilder::DiscoverNative,
    )
    .await
    {
        Ok(client) => (client, false),
        Err(error) => match Failure::from_build(&error) {
            Failure::NotSupported => return Err(OpenError::NotSupported),
            // No network: open what is stored. The account was syncing
            // before (it could not have been signed in otherwise), so
            // native sliding sync is assumed until the server says
            // otherwise.
            Failure::Unreachable | Failure::RateLimited(_) => {
                info!(account = %params.id, "the homeserver can't be reached; opening the account offline");
                let client = build_client(
                    params,
                    &sdk_dir,
                    &cache_dir,
                    &passphrase,
                    VersionBuilder::Native,
                )
                .await
                .map_err(|error| OpenError::Client(error.to_string()))?;
                (client, true)
            }
            _ => return Err(OpenError::Client(error.to_string())),
        },
    };

    persist_rotated_tokens(&client, params, shared.clone())
        .map_err(|error| OpenError::Client(error.to_string()))?;

    let session = &params.session;
    let user_id = OwnedUserId::try_from(session.user_id.clone())
        .map_err(|error| OpenError::BadSession(error.to_string()))?;
    let device_id: OwnedDeviceId = session.device_id.clone().into();
    client
        .restore_session(MatrixSession {
            meta: SessionMeta { user_id, device_id },
            tokens: SessionTokens {
                access_token: session.access_token.expose().clone(),
                refresh_token: session
                    .refresh_token
                    .as_ref()
                    .map(|token| token.expose().clone()),
            },
        })
        .await
        .map_err(|error| OpenError::Client(error.to_string()))?;

    if let Some(check) = check {
        let homeserver = client.homeserver();
        let user_id = client.user_id().map(ToOwned::to_owned);
        let verdict = match &user_id {
            Some(user_id) => check.check(&homeserver, user_id),
            None => Err("The sign-in has no user.".to_owned()),
        };
        if let Err(refusal) = verdict {
            warn!(account = %params.id, "the post-login check refused the sign-in; signing the device out");
            if let Err(error) = client.logout().await {
                warn!(account = %params.id, %error, "signing the refused device out failed");
            }
            return Err(OpenError::Refused(refusal));
        }
    }

    // Before the first sync, so nothing it reports is missed.
    encryption::register_incoming(
        &client,
        account_key(params.id),
        params.verifications.clone(),
    );
    notifications::register(
        &client,
        account_key(params.id),
        params.notifications.clone(),
    )
    .await;

    let sync = SyncService::builder(client.clone())
        .build()
        .await
        .map_err(|error| OpenError::Client(error.to_string()))?;
    Ok((
        Ready {
            utd_hook: encryption::utd_hook(&client).await,
            client,
            sync: Arc::new(sync),
            timelines: OpenTimelines::default(),
        },
        offline,
    ))
}

async fn build_client(
    params: &OpenParams,
    sdk_dir: &Path,
    cache_dir: &Path,
    passphrase: &str,
    version: VersionBuilder,
) -> Result<Client, matrix_sdk::ClientBuildError> {
    Client::builder()
        .homeserver_url(&params.session.homeserver_url)
        .sqlite_store_with_cache_path(sdk_dir, cache_dir, Some(passphrase))
        .http_client(params.config.http.clone())
        .media_fetcher(Arc::new(AuthenticatedMediaFetcher::new(
            params.config.http.clone(),
        )))
        .respect_login_well_known(false)
        // ADR 0009: Element X's encryption defaults at the pinned SDK.
        .with_encryption_settings(encryption::encryption_settings())
        .with_room_key_recipient_strategy(encryption::room_key_recipients())
        .with_decryption_settings(encryption::decryption_settings())
        .with_enable_share_history_on_invite(encryption::SHARE_HISTORY_ON_INVITE)
        .handle_refresh_tokens()
        .sliding_sync_version_builder(version)
        .build()
        .await
}

/// Writes every token the SDK rotates to the keychain, synchronously in
/// the SDK's refresh path: the new tokens are in the keychain before the
/// request that needed the refresh is retried, so a crash right after a
/// refresh never leaves the keychain with a refresh token the server has
/// already replaced.
fn persist_rotated_tokens(
    client: &Client,
    params: &OpenParams,
    shared: Arc<Shared>,
) -> matrix_sdk::Result<()> {
    let id = params.id;
    let template = params.session.clone();
    let keychain = params.keychain.clone();
    let reload_keychain = params.keychain.clone();
    client.set_session_callbacks(
        Box::new(move |_client| {
            let session = load_session(reload_keychain.as_ref(), id)?
                .ok_or("the keychain has no sign-in for this account")?;
            Ok(SessionTokens {
                access_token: session.access_token.expose().clone(),
                refresh_token: session
                    .refresh_token
                    .as_ref()
                    .map(|token| token.expose().clone()),
            })
        }),
        Box::new(move |client| {
            let tokens = client
                .session_tokens()
                .ok_or("the client has no tokens to save")?;
            let mut session = template.clone();
            session.access_token = Secret::new(tokens.access_token);
            session.refresh_token = tokens.refresh_token.map(Secret::new);
            match save_session(keychain.as_ref(), id, &session) {
                Ok(()) => {
                    info!(account = %id, "saved the renewed sign-in");
                    shared.set_problem(None);
                    Ok(())
                }
                Err(error) => {
                    warn!(account = %id, error = %error, "could not save the renewed sign-in");
                    shared.set_problem(Some(AccountProblem::TokenNotSaved));
                    Err(error.into())
                }
            }
        }),
    )
}

/// For a restored account whose server had no native sliding sync: waits
/// for a retry, then opens the client again, and supervises it once it
/// opens.
async fn reopen(params: OpenParams, shared: Arc<Shared>) {
    let mut errors = Backoff::new(ERROR_BACKOFF.0, ERROR_BACKOFF.1);
    let mut wait = None;
    loop {
        match wait {
            Some(wait) => {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = shared.retry.notified() => {}
                }
            }
            None => shared.retry.notified().await,
        }
        shared.set_sync(SyncState::Connecting);
        match open_client(&params, &shared, None).await {
            Ok((ready, _offline)) => {
                let _ = shared.ready.set(ready.clone());
                return supervise(params.id, ready, shared, None).await;
            }
            Err(OpenError::NotSupported) => {
                shared.set_sync(SyncState::NotSupported);
                wait = None;
            }
            Err(error) => {
                warn!(account = %params.id, %error, "the account could not be opened");
                let retry_in = errors.next_delay();
                shared.set_sync(SyncState::Error {
                    message: SYNC_FAILED.to_owned(),
                    retry_in,
                });
                wait = Some(retry_in);
            }
        }
    }
}

/// What the supervisor does after a failure.
fn after_failure(
    failure: &Failure,
    offline: &mut Backoff,
    suspended: &mut Backoff,
    errors: &mut Backoff,
) -> (SyncState, Option<Duration>) {
    match failure {
        Failure::Suspended => {
            let wait = suspended.next_delay();
            (SyncState::Suspended { recheck_in: wait }, Some(wait))
        }
        Failure::SignedOut { soft_logout } => (
            SyncState::SignedOut {
                soft_logout: *soft_logout,
            },
            None,
        ),
        Failure::NotSupported => (SyncState::NotSupported, None),
        Failure::RateLimited(after) => {
            let backoff = offline.next_delay();
            let wait = after.map_or(backoff, |after| after.max(backoff));
            (SyncState::Offline { retry_in: wait }, Some(wait))
        }
        Failure::Unreachable => {
            let wait = offline.next_delay();
            (SyncState::Offline { retry_in: wait }, Some(wait))
        }
        Failure::Other(_) => {
            let wait = errors.next_delay();
            (
                SyncState::Error {
                    message: SYNC_FAILED.to_owned(),
                    retry_in: wait,
                },
                Some(wait),
            )
        }
    }
}

/// Turns the send queue back on, and reads the server's upload limit
/// once, when the sync reaches the server.
fn on_running(client: &Client) {
    let client = client.clone();
    tokio::spawn(async move {
        if !client.send_queue().is_enabled() {
            info!("the connection is back; sending what was queued");
        }
        // Rooms whose own queue stopped after a failed send start again
        // too: this wakes every room's queue.
        client.send_queue().set_enabled(true).await;
        if let Err(error) = client.load_or_fetch_max_upload_size().await {
            info!(%error, "the server's upload limit couldn't be read yet");
        }
    });
}

/// After a send that failed but can be retried (the server couldn't be
/// reached), turns the queue back on after `wait`, so the message goes
/// once the server answers even if the sync never stopped.
fn retry_sending_after(client: &Client, wait: Duration) {
    let client = client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(wait).await;
        client.send_queue().set_enabled(true).await;
    });
}

/// Keeps the account's sync going: starts the `SyncService`, reports its
/// state, and after a failure waits as the failure says before starting it
/// again (PLAN.md, section 5.6). The SDK's own offline mode is not used: it
/// restarts the sync as soon as `/versions` answers, which for a suspended
/// account (whose `/versions` works and whose every other request is
/// refused) is a request loop.
///
/// It also keeps the send queue going (ADR 0012): the SDK stops a room's
/// queue after a send fails, so the supervisor turns it back on when the
/// sync reaches the server again, and after a backoff when a send fails
/// while the sync keeps running. A send refused with `M_USER_SUSPENDED`
/// is the account's pause showing on a send (a server that lets a paused
/// account read): the account goes to `Suspended` like a refused sync.
///
/// It also runs the account's encryption follower (`encryption::follow`)
/// for as long as it runs, handing it a password sign-in's password.
async fn supervise(
    id: AccountId,
    ready: Ready,
    shared: Arc<Shared>,
    password: Option<Secret<String>>,
) {
    let _encryption = AbortOnDrop(tokio::spawn(encryption::follow(
        ready.client.clone(),
        shared.encryption.clone(),
        password,
    )));
    let mut offline = Backoff::new(OFFLINE_BACKOFF.0, OFFLINE_BACKOFF.1);
    let mut suspended = Backoff::new(SUSPENDED_BACKOFF.0, SUSPENDED_BACKOFF.1);
    let mut errors = Backoff::new(ERROR_BACKOFF.0, ERROR_BACKOFF.1);
    let mut sending = Backoff::new(OFFLINE_BACKOFF.0, OFFLINE_BACKOFF.1);
    let mut sync_state = ready.sync.state();
    let mut list_state = ready.sync.room_list_service().state();
    let mut session_changes = ready.client.subscribe_to_session_changes();
    let mut room_updates = ready.client.subscribe_to_all_room_updates();
    shared.set_unread(notifications::unread_total(&ready.client));
    let mut send_errors = ready.client.send_queue().subscribe_errors();

    loop {
        // A start after a failure begins a new attempt, so the account is
        // connecting again (an offline account keeps its offline state
        // until the server answers).
        if !matches!(shared.status.borrow().sync, SyncState::Offline { .. }) {
            shared.set_sync(SyncState::Connecting);
        }
        ready.sync.start().await;

        let failure = loop {
            tokio::select! {
                state = sync_state.next() => match state {
                    Some(matrix_sdk_ui::sync_service::State::Error(error)) => {
                        break Failure::from_sync(&error);
                    }
                    Some(_) => {}
                    None => return,
                },
                state = list_state.next() => match state {
                    Some(
                        room_list_service::State::SettingUp
                        | room_list_service::State::Recovering
                        | room_list_service::State::Running,
                    ) => {
                        offline.reset();
                        suspended.reset();
                        errors.reset();
                        if shared.status.borrow().sync != SyncState::Running {
                            sending.reset();
                            on_running(&ready.client);
                        }
                        shared.set_sync(SyncState::Running);
                    }
                    Some(_) => {}
                    None => return,
                },
                update = room_updates.recv() => match update {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        shared.set_unread(notifications::unread_total(&ready.client));
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                error = send_errors.recv() => match error {
                    Ok(error) => match Failure::from_sdk(&error.error) {
                        Failure::Suspended => break Failure::Suspended,
                        _ if error.is_recoverable => {
                            let wait = sending.next_delay();
                            info!(account = %id, ?wait, "a send failed; trying again later");
                            retry_sending_after(&ready.client, wait);
                        }
                        other => info!(account = %id, failure = ?other, "a send was refused"),
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                change = session_changes.recv() => match change {
                    Ok(SessionChange::UnknownToken(data)) => {
                        break Failure::SignedOut { soft_logout: data.soft_logout };
                    }
                    Ok(SessionChange::TokensRefreshed) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
        };

        ready.sync.stop().await;
        let (state, wait) = after_failure(&failure, &mut offline, &mut suspended, &mut errors);
        match &failure {
            Failure::Other(error) => {
                warn!(account = %id, %error, "the sync stopped");
            }
            other => info!(account = %id, failure = ?other, "the sync stopped"),
        }
        shared.set_sync(state);
        match wait {
            Some(wait) => {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = shared.retry.notified() => {}
                }
            }
            None => shared.retry.notified().await,
        }
    }
}

/// Aborts a task when dropped: the supervisor's own tasks end with it.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backoffs() -> (Backoff, Backoff, Backoff) {
        (
            Backoff::new(OFFLINE_BACKOFF.0, OFFLINE_BACKOFF.1),
            Backoff::new(SUSPENDED_BACKOFF.0, SUSPENDED_BACKOFF.1),
            Backoff::new(ERROR_BACKOFF.0, ERROR_BACKOFF.1),
        )
    }

    #[test]
    fn a_suspension_is_its_own_state_and_rechecks_slowly() {
        let (mut offline, mut suspended, mut errors) = backoffs();
        let waits: Vec<_> = (0..6)
            .map(|_| {
                let (state, wait) = after_failure(
                    &Failure::Suspended,
                    &mut offline,
                    &mut suspended,
                    &mut errors,
                );
                assert!(matches!(state, SyncState::Suspended { .. }), "{state:?}");
                wait.expect("it rechecks")
            })
            .collect();
        assert_eq!(waits, [30, 60, 120, 240, 300, 300].map(Duration::from_secs));
    }

    #[test]
    fn signed_out_and_not_supported_wait_for_the_user() {
        let (mut offline, mut suspended, mut errors) = backoffs();
        for failure in [
            Failure::SignedOut { soft_logout: true },
            Failure::NotSupported,
        ] {
            let (_, wait) = after_failure(&failure, &mut offline, &mut suspended, &mut errors);
            assert_eq!(wait, None, "{failure:?}");
        }
    }

    #[test]
    fn a_rate_limit_waits_at_least_as_long_as_the_server_asks() {
        let (mut offline, mut suspended, mut errors) = backoffs();
        let (state, wait) = after_failure(
            &Failure::RateLimited(Some(Duration::from_secs(42))),
            &mut offline,
            &mut suspended,
            &mut errors,
        );
        assert_eq!(wait, Some(Duration::from_secs(42)));
        assert_eq!(
            state,
            SyncState::Offline {
                retry_in: Duration::from_secs(42)
            }
        );
    }

    #[test]
    fn other_errors_show_a_fixed_sentence() {
        let (mut offline, mut suspended, mut errors) = backoffs();
        let (state, _) = after_failure(
            &Failure::Other("syt_secret in an error".to_owned()),
            &mut offline,
            &mut suspended,
            &mut errors,
        );
        let message = state.message().expect("a sentence");
        assert!(!message.contains("syt_"), "{message}");
    }
}
