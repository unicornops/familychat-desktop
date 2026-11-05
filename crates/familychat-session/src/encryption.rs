//! End-to-end encryption (PLAN.md, section 5.7; ADR 0009): the client's
//! encryption settings, the account's [`EncryptionStatus`], making its
//! cross-signing identity (without user-interactive auth first, MSC3967,
//! then with the password), recovery, verification flows bridged to
//! [`VerificationFlow`]s, the devices, room key files and the identity
//! warnings of a room.
//!
//! All crypto is the SDK's. Secrets cross this module only as
//! [`SecretText`] or [`Secret`]: a password answering the server's
//! user-interactive auth is used for one request and then dropped (wiped),
//! a recovery key goes straight to the frontend that shows it once, and a
//! key file's passphrase to the SDK's export or import. None is logged.
//! ruma's `uiaa::Password`, which the SDK sends, holds its own copy of a
//! password while the request is built and sent; it is not ours to wipe,
//! and it is dropped with the request.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use familychat_core::{
    UserId,
    encryption::{
        AuthOutcome, DeviceTrust, DeviceView, EncryptionStatus, IdentitySetup, IdentityWarning,
        IdentityWarningKind, KeyImport, QrModules, RecoveryState as CoreRecovery, SasEmoji,
        SecretText, VerificationAnswer, VerificationFlow, VerificationStep, VerifyingWith, wording,
    },
    session::{AccountKey, IncomingVerification, SUBSCRIPTION_BUFFER, SessionError},
};
use familychat_security::Secret;
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client, Room,
    encryption::{
        BackupDownloadStrategy, CrossSigningResetAuthType, EncryptionSettings, VerificationState,
        recovery::{RecoveryError, RecoveryState},
        verification::{
            QrVerification, QrVerificationState, SasState, SasVerification, VerificationRequest,
            VerificationRequestState,
        },
    },
    ruma::{
        OwnedDeviceId,
        api::client::{
            keys::get_keys,
            uiaa::{AuthData, MatrixUserIdentifier, Password, UserIdentifier},
        },
        events::key::verification::request::ToDeviceKeyVerificationRequestEvent,
    },
};
use matrix_sdk_base::crypto::{
    CollectStrategy, DecryptionSettings, IdentityState, TrustRequirement,
    matrix_sdk_qrcode::qrcode::Color,
};
use matrix_sdk_ui::unable_to_decrypt_hook::{
    UnableToDecryptHook, UnableToDecryptInfo, UtdHookManager,
};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::backoff::Backoff;

// ---------------------------------------------------------------------------
// Settings (ADR 0009)
// ---------------------------------------------------------------------------

/// The client's encryption settings (ADR 0009): Element X's at the pinned
/// SDK, except that cross-signing is made by [`ensure_identity`] rather
/// than by the SDK's login, which never sees our sign-ins (ADR 0006).
/// Backups are made when the account has none, and a message this device
/// can't read asks the backup for its key: the per-session endpoint, which
/// the pinned Palpo answers correctly (its whole-backup download is
/// malformed, so `OneShot` reads nothing there).
pub(crate) fn encryption_settings() -> EncryptionSettings {
    EncryptionSettings {
        auto_enable_cross_signing: false,
        backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure,
        auto_enable_backups: true,
    }
}

/// Who gets a room's keys: Element X's default (its "exclude insecure
/// devices" mode is off by default). Every device but a blacklisted one,
/// except that a verified person's unconfirmed device, or a verified
/// person's new identity, stops the message until the person says "Send
/// anyway" ([`familychat_core::timeline::SendProblem`]).
pub(crate) fn room_key_recipients() -> CollectStrategy {
    CollectStrategy::ErrorOnVerifiedUserProblem
}

/// Which senders' messages are decrypted: Element X's default, every
/// sender's; the timeline marks those from unconfirmed devices.
pub(crate) fn decryption_settings() -> DecryptionSettings {
    DecryptionSettings {
        sender_device_trust_requirement: TrustRequirement::Untrusted,
    }
}

/// Room history shared with people invited to a room (MSC4268), as Element
/// X does.
pub(crate) const SHARE_HISTORY_ON_INVITE: bool = true;

// ---------------------------------------------------------------------------
// Unable to decrypt
// ---------------------------------------------------------------------------

/// matrix-sdk-ui's unable-to-decrypt hook, reporting to the local log only
/// (PLAN.md, section 8: nothing leaves the computer): the reason, and how
/// long a message took to decrypt when its key came late. No event id,
/// room or sender is logged. The timeline's items carry the same cause,
/// which `mapping::utd_reason` turns into the sentence a frontend shows.
#[derive(Debug)]
struct LocalUtdLog;

impl UnableToDecryptHook for LocalUtdLog {
    fn on_utd(&self, info: UnableToDecryptInfo) {
        let reason = crate::mapping::utd_cause(info.cause);
        match info.time_to_decrypt {
            Some(late) => info!(
                ?reason,
                late_ms = late.as_millis() as u64,
                "a message's key came late"
            ),
            None => info!(?reason, "a message couldn't be decrypted"),
        }
    }
}

/// How long the hook waits for a late key before it counts a message as
/// unreadable.
const UTD_GRACE: Duration = Duration::from_secs(60);

/// The account's unable-to-decrypt hook, for every timeline it opens.
pub(crate) async fn utd_hook(client: &Client) -> Arc<UtdHookManager> {
    let mut hook =
        UtdHookManager::new(Arc::new(LocalUtdLog), client.clone()).with_max_delay(UTD_GRACE);
    if let Err(error) = hook.reload_from_store().await {
        info!(%error, "the unable-to-decrypt hook's record couldn't be read");
    }
    Arc::new(hook)
}

// ---------------------------------------------------------------------------
// The account's status
// ---------------------------------------------------------------------------

/// The account's encryption status and the lock that keeps one identity
/// set-up at a time.
#[derive(Debug)]
pub(crate) struct EncryptionShared {
    pub(crate) status: watch::Sender<EncryptionStatus>,
    setting_up: tokio::sync::Mutex<()>,
}

impl Default for EncryptionShared {
    fn default() -> Self {
        Self {
            status: watch::channel(EncryptionStatus::default()).0,
            setting_up: tokio::sync::Mutex::new(()),
        }
    }
}

impl EncryptionShared {
    fn update(&self, change: impl FnOnce(&mut EncryptionStatus)) {
        self.status.send_if_modified(|status| {
            let before = status.clone();
            change(status);
            *status != before
        });
    }
}

fn device_trust(state: VerificationState) -> DeviceTrust {
    match state {
        VerificationState::Unknown => DeviceTrust::Unknown,
        VerificationState::Verified => DeviceTrust::Verified,
        VerificationState::Unverified => DeviceTrust::Unverified,
    }
}

fn recovery_state(state: RecoveryState) -> CoreRecovery {
    match state {
        RecoveryState::Unknown => CoreRecovery::Unknown,
        RecoveryState::Enabled => CoreRecovery::Enabled,
        RecoveryState::Disabled => CoreRecovery::Disabled,
        RecoveryState::Incomplete => CoreRecovery::Incomplete,
    }
}

/// How long [`follow`] waits before trying the identity again after a
/// failure that wasn't the server asking for a password: ten seconds,
/// doubling to ten minutes.
const IDENTITY_RETRY: (Duration, Duration) = (Duration::from_secs(10), Duration::from_secs(600));

/// Keeps the account's [`EncryptionStatus`] up to date for as long as it
/// runs: makes the identity when the account has none (with `password` for
/// a password sign-in, then wiped), and follows this device's trust,
/// recovery, the account's other devices and its identity.
pub(crate) async fn follow(
    client: Client,
    shared: Arc<EncryptionShared>,
    password: Option<Secret<String>>,
) {
    let encryption = client.encryption();
    let mut verification = encryption.verification_state();
    let mut recovery = encryption.recovery().state_stream();
    shared.update(|status| {
        status.device = device_trust(verification.get());
        status.recovery = recovery_state(encryption.recovery().state());
    });

    let mut password = password;
    let mut retry = Backoff::new(IDENTITY_RETRY.0, IDENTITY_RETRY.1);
    while ensure_identity(&client, &shared, password.take())
        .await
        .is_err()
    {
        let wait = retry.next_delay();
        let mut status = shared.status.subscribe();
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = status.wait_for(|status| status.identity == IdentitySetup::Ready) => break,
        }
    }
    update_other_devices(&client, &shared).await;

    let Ok(devices) = encryption.devices_stream().await else {
        return;
    };
    let Ok(identities) = encryption.user_identities_stream().await else {
        return;
    };
    let mut devices = std::pin::pin!(devices);
    let mut identities = std::pin::pin!(identities);
    loop {
        tokio::select! {
            state = verification.next() => match state {
                Some(state) => {
                    shared.update(|status| status.device = device_trust(state));
                    update_other_devices(&client, &shared).await;
                }
                None => return,
            },
            state = recovery.next() => match state {
                Some(state) => shared.update(|status| status.recovery = recovery_state(state)),
                None => return,
            },
            update = devices.next() => match update {
                Some(_) => update_other_devices(&client, &shared).await,
                None => return,
            },
            update = identities.next() => match update {
                Some(update) => {
                    let own = client.user_id().map(ToOwned::to_owned);
                    if own.is_some_and(|own| {
                        update.new.contains_key(&own) || update.changed.contains_key(&own)
                    }) {
                        shared.update(|status| status.identity = IdentitySetup::Ready);
                    }
                }
                None => return,
            },
        }
    }
}

/// Whether the account has another device cross-signed by its identity,
/// which could confirm this one.
async fn update_other_devices(client: &Client, shared: &EncryptionShared) {
    let (Some(user_id), Some(device_id)) = (client.user_id(), client.device_id()) else {
        return;
    };
    let Ok(devices) = client.encryption().get_user_devices(user_id).await else {
        return;
    };
    let others = devices
        .devices()
        .any(|device| device.device_id() != device_id && device.is_cross_signed_by_owner());
    shared.update(|status| status.other_devices = others);
}

/// Makes the account's identity when it has none, as
/// [`set_up_identity`]: `Ok` once it is ready, or the server asks for a
/// password this device doesn't have (shown as
/// [`IdentitySetup::NeedsPassword`]); `Err` after another failure, shown
/// as [`IdentitySetup::Failed`], which [`follow`] tries again later.
async fn ensure_identity(
    client: &Client,
    shared: &EncryptionShared,
    password: Option<Secret<String>>,
) -> Result<AuthOutcome, ()> {
    match set_up_identity(client, shared, password).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            info!(%error, "the account's identity couldn't be checked or made yet");
            Err(())
        }
    }
}

/// Makes the account's cross-signing identity if it has none: first with
/// no user-interactive auth (MSC3967 lets the first upload through), then,
/// when the server asks, with `password`. The password is dropped (and so
/// wiped) when this returns.
pub(crate) async fn set_up_identity(
    client: &Client,
    shared: &EncryptionShared,
    password: Option<Secret<String>>,
) -> Result<AuthOutcome, SessionError> {
    let _one_at_a_time = shared.setting_up.lock().await;
    let encryption = client.encryption();
    let user_id = client.user_id().ok_or(SessionError::NotReady)?.to_owned();
    let failed = |shared: &EncryptionShared| {
        shared.update(|status| {
            status.identity = IdentitySetup::Failed(wording::SETUP_FAILED.to_owned());
        });
        SessionError::Failed(wording::SETUP_FAILED.to_owned())
    };
    // The server is asked, not the local store: an upload the server
    // refused leaves the new identity in the store as if it were the
    // account's.
    match server_has_identity(client, &user_id).await {
        Ok(true) => {
            // So this device's store knows the server's identity too.
            if let Err(error) = encryption.request_user_identity(&user_id).await {
                info!(%error, "the account's identity couldn't be fetched");
            }
            shared.update(|status| status.identity = IdentitySetup::Ready);
            return Ok(AuthOutcome::Done);
        }
        Ok(false) => {}
        Err(error) => {
            info!(%error, "the server couldn't be asked about the account's identity");
            return Err(failed(shared));
        }
    }
    shared.update(|status| {
        if status.identity != IdentitySetup::NeedsPassword {
            status.identity = IdentitySetup::SettingUp;
        }
    });
    // First with no user-interactive auth: MSC3967 lets the first upload
    // through. A retry re-uploads the identity made the first time.
    let uiaa = match encryption.bootstrap_cross_signing(None).await {
        Ok(()) => {
            info!("made the account's cross-signing identity");
            shared.update(|status| status.identity = IdentitySetup::Ready);
            return Ok(AuthOutcome::Done);
        }
        Err(error) => match error.as_uiaa_response() {
            Some(uiaa) => uiaa.clone(),
            None => {
                warn!(%error, "making the cross-signing identity failed");
                return Err(failed(shared));
            }
        },
    };
    info!("the server asks for user-interactive auth before the first cross-signing upload");
    let Some(password) = password else {
        shared.update(|status| status.identity = IdentitySetup::NeedsPassword);
        return Ok(AuthOutcome::PasswordNeeded);
    };
    let auth = password_auth(&user_id, &password, uiaa.session.clone());
    drop(password);
    match encryption.bootstrap_cross_signing(Some(auth)).await {
        Ok(()) => {
            info!("made the cross-signing identity with the password");
            shared.update(|status| status.identity = IdentitySetup::Ready);
            Ok(AuthOutcome::Done)
        }
        Err(error) if error.as_uiaa_response().is_some() => {
            info!("the server refused the password for the cross-signing upload");
            shared.update(|status| status.identity = IdentitySetup::NeedsPassword);
            Ok(AuthOutcome::WrongPassword)
        }
        Err(error) => {
            warn!(%error, "making the cross-signing identity with the password failed");
            Err(failed(shared))
        }
    }
}

/// Whether the server publishes a master cross-signing key for `user_id`
/// (one `/keys/query`).
async fn server_has_identity(client: &Client, user_id: &UserId) -> matrix_sdk::HttpResult<bool> {
    let mut request = get_keys::v3::Request::new();
    request.device_keys.insert(user_id.to_owned(), Vec::new());
    let response = client.send(request).await?;
    Ok(response.master_keys.contains_key(user_id))
}

/// The `m.login.password` stage for `user_id`.
fn password_auth(user_id: &UserId, password: &Secret<String>, session: Option<String>) -> AuthData {
    let mut auth = Password::new(
        UserIdentifier::Matrix(MatrixUserIdentifier::new(user_id.to_string())),
        password.expose().clone(),
    );
    auth.session = session;
    AuthData::Password(auth)
}

/// A password from a frontend, for one request.
pub(crate) fn password_secret(password: Option<SecretText>) -> Option<Secret<String>> {
    password
        .filter(|password| !password.expose().is_empty())
        .map(|password| Secret::new(password.expose().to_owned()))
}

// ---------------------------------------------------------------------------
// Recovery
// ---------------------------------------------------------------------------

/// Sets recovery up and answers with the new recovery key.
pub(crate) async fn enable_recovery(client: &Client) -> Result<SecretText, SessionError> {
    match client
        .encryption()
        .recovery()
        .enable()
        .wait_for_backups_to_upload()
        .await
    {
        Ok(key) => {
            info!("recovery is set up");
            Ok(SecretText::new(key))
        }
        Err(RecoveryError::BackupExistsOnServer) => Err(SessionError::Failed(
            "Your account already has a key backup that this device can't open. Confirm it's \
             you first, then set up recovery."
                .to_owned(),
        )),
        Err(error) => {
            warn!(%error, "setting up recovery failed");
            Err(SessionError::Failed(
                "Recovery couldn't be set up. Check your connection and try again.".to_owned(),
            ))
        }
    }
}

/// Makes a new recovery key in place of the old one (which stops working)
/// and answers with it; recovery is set up first when it isn't yet.
pub(crate) async fn reset_recovery_key(client: &Client) -> Result<SecretText, SessionError> {
    let recovery = client.encryption().recovery();
    if recovery.state() != RecoveryState::Enabled {
        return enable_recovery(client).await;
    }
    match recovery.reset_key().await {
        Ok(key) => {
            info!("made a new recovery key");
            Ok(SecretText::new(key))
        }
        Err(error) => {
            warn!(%error, "making a new recovery key failed");
            Err(SessionError::Failed(
                "A new recovery key couldn't be made. Check your connection and try again."
                    .to_owned(),
            ))
        }
    }
}

/// Resets the account's cross-signing identity and its key backup (for an
/// account with no confirmed device and no recovery key), answering the
/// server's user-interactive auth with `password` when it asks. The
/// password is dropped (wiped) when this returns. OAuth approval in a
/// browser is Phase 14's.
pub(crate) async fn reset_identity(
    client: &Client,
    password: Option<Secret<String>>,
) -> Result<AuthOutcome, SessionError> {
    let user_id = client.user_id().ok_or(SessionError::NotReady)?.to_owned();
    let failed = || {
        SessionError::Failed(
            "Your identity couldn't be reset. Check your connection and try again.".to_owned(),
        )
    };
    let handle = match client.encryption().recovery().reset_identity().await {
        Ok(None) => {
            info!("reset the account's identity with no further auth");
            return Ok(AuthOutcome::Done);
        }
        Ok(Some(handle)) => handle,
        Err(error) => {
            warn!(%error, "resetting the identity failed");
            return Err(failed());
        }
    };
    let session = match handle.auth_type() {
        CrossSigningResetAuthType::Uiaa(uiaa) => uiaa.session.clone(),
        CrossSigningResetAuthType::OAuth(_) => {
            return Err(SessionError::Failed(
                wording::RESET_NEEDS_BROWSER.to_owned(),
            ));
        }
    };
    let Some(password) = password else {
        return Ok(AuthOutcome::PasswordNeeded);
    };
    let auth = password_auth(&user_id, &password, session);
    drop(password);
    match handle.reset(Some(auth)).await {
        Ok(()) => {
            info!("reset the account's identity with the password");
            Ok(AuthOutcome::Done)
        }
        Err(RecoveryError::Sdk(error)) if error.as_uiaa_response().is_some() => {
            Ok(AuthOutcome::WrongPassword)
        }
        Err(error) => {
            warn!(%error, "resetting the identity with the password failed");
            Err(failed())
        }
    }
}

/// Confirms this device with the recovery key.
pub(crate) async fn recover(client: &Client, key: SecretText) -> Result<(), SessionError> {
    let typed = key.expose().trim();
    if typed.is_empty() {
        return Err(SessionError::Failed(wording::NO_RECOVERY_KEY.to_owned()));
    }
    let result = client.encryption().recovery().recover(typed).await;
    drop(key);
    match result {
        Ok(()) => {
            info!("recovered the account's secrets with the recovery key");
            Ok(())
        }
        Err(RecoveryError::SecretStorage(error)) => {
            info!(%error, "the recovery key didn't open secret storage");
            Err(SessionError::Failed(wording::WRONG_RECOVERY_KEY.to_owned()))
        }
        Err(error) => {
            warn!(%error, "recovering with the recovery key failed");
            Err(SessionError::Failed(
                "Your recovery key couldn't be used just now. Check your connection and try again."
                    .to_owned(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// The account's devices, this one first, then the most recently seen.
pub(crate) async fn devices(client: &Client) -> Result<Vec<DeviceView>, SessionError> {
    let user_id = client.user_id().ok_or(SessionError::NotReady)?.to_owned();
    let own_device = client.device_id().map(ToOwned::to_owned);
    let listed = client.devices().await.map_err(|error| {
        info!(%error, "listing the devices failed");
        SessionError::Failed("Your devices couldn't be listed. Try again.".to_owned())
    })?;
    let crypto = client.encryption().get_user_devices(&user_id).await.ok();
    let mut views: Vec<DeviceView> = listed
        .devices
        .iter()
        .map(|device| {
            let verified = crypto
                .as_ref()
                .and_then(|devices| devices.get(&device.device_id))
                .is_some_and(|device| device.is_cross_signed_by_owner());
            DeviceView {
                device_id: device.device_id.to_string(),
                display_name: device.display_name.clone(),
                is_this_device: own_device.as_ref() == Some(&device.device_id),
                verified,
                last_seen_ms: device.last_seen_ts.map(|ts| u64::from(ts.get())),
            }
        })
        .collect();
    views.sort_by(|a, b| {
        b.is_this_device
            .cmp(&a.is_this_device)
            .then(b.last_seen_ms.cmp(&a.last_seen_ms))
            .then(a.device_id.cmp(&b.device_id))
    });
    Ok(views)
}

/// Signs another device out, answering user-interactive auth with
/// `password` when the server asks for it.
pub(crate) async fn sign_out_device(
    client: &Client,
    device_id: &str,
    password: Option<Secret<String>>,
) -> Result<AuthOutcome, SessionError> {
    let user_id = client.user_id().ok_or(SessionError::NotReady)?.to_owned();
    if client
        .device_id()
        .is_some_and(|own| own.as_str() == device_id)
    {
        return Err(SessionError::Failed(
            "This device can't sign itself out from here.".to_owned(),
        ));
    }
    let devices = [OwnedDeviceId::from(device_id)];
    let failed =
        || SessionError::Failed("The device couldn't be signed out. Try again.".to_owned());
    let uiaa = match client.delete_devices(&devices, None).await {
        Ok(_) => return Ok(AuthOutcome::Done),
        Err(error) => match error.as_uiaa_response() {
            Some(uiaa) => uiaa.clone(),
            None => {
                info!(%error, "signing a device out failed");
                return Err(failed());
            }
        },
    };
    let Some(password) = password else {
        return Ok(AuthOutcome::PasswordNeeded);
    };
    let auth = password_auth(&user_id, &password, uiaa.session.clone());
    drop(password);
    match client.delete_devices(&devices, Some(auth)).await {
        Ok(_) => {
            info!("signed another device out");
            Ok(AuthOutcome::Done)
        }
        Err(error) if error.as_uiaa_response().is_some() => Ok(AuthOutcome::WrongPassword),
        Err(error) => {
            info!(%error, "signing a device out with the password failed");
            Err(failed())
        }
    }
}

// ---------------------------------------------------------------------------
// Room key files
// ---------------------------------------------------------------------------

/// The shortest passphrase a key file is written with.
pub(crate) const MIN_PASSPHRASE: usize = 8;

/// Writes every room key to `path`, encrypted with `passphrase`.
pub(crate) async fn export_room_keys(
    client: &Client,
    path: PathBuf,
    passphrase: SecretText,
) -> Result<(), SessionError> {
    if passphrase.expose().chars().count() < MIN_PASSPHRASE {
        return Err(SessionError::Failed(
            wording::PASSPHRASE_TOO_SHORT.to_owned(),
        ));
    }
    let result = client
        .encryption()
        .export_room_keys(path, passphrase.expose(), |_| true)
        .await;
    drop(passphrase);
    result.map_err(|error| {
        warn!(%error, "exporting the room keys failed");
        SessionError::Failed(wording::EXPORT_FAILED.to_owned())
    })?;
    info!("exported the room keys");
    Ok(())
}

/// Reads room keys from `path` with `passphrase`.
pub(crate) async fn import_room_keys(
    client: &Client,
    path: PathBuf,
    passphrase: SecretText,
) -> Result<KeyImport, SessionError> {
    if passphrase.expose().is_empty() {
        return Err(SessionError::Failed(wording::NO_PASSPHRASE.to_owned()));
    }
    let result = client
        .encryption()
        .import_room_keys(path, passphrase.expose())
        .await;
    drop(passphrase);
    let result = result.map_err(|error| {
        info!(%error, "importing room keys failed");
        SessionError::Failed(wording::IMPORT_FAILED.to_owned())
    })?;
    info!(
        imported = result.imported_count,
        total = result.total_count,
        "imported room keys"
    );
    Ok(KeyImport {
        imported: result.imported_count as u64,
        total: result.total_count as u64,
    })
}

// ---------------------------------------------------------------------------
// Identity warnings
// ---------------------------------------------------------------------------

/// Follows a room's identity warnings until the receiver is dropped.
pub(crate) async fn identity_warnings(
    room: Room,
) -> Result<watch::Receiver<Vec<IdentityWarning>>, SessionError> {
    let changes = room
        .subscribe_to_identity_status_changes()
        .await
        .map_err(|error| {
            info!(%error, "the room's identity changes couldn't be followed");
            SessionError::Failed("The room's identity warnings couldn't be loaded.".to_owned())
        })?;
    let (sender, receiver) = watch::channel(Vec::new());
    tokio::spawn(async move {
        let mut changes = std::pin::pin!(changes);
        let mut warnings: Vec<IdentityWarning> = Vec::new();
        loop {
            tokio::select! {
                () = sender.closed() => return,
                batch = changes.next() => {
                    let Some(batch) = batch else { return };
                    for change in batch {
                        warnings.retain(|warning| warning.user_id != change.user_id);
                        let kind = match change.changed_to {
                            IdentityState::PinViolation => IdentityWarningKind::Changed,
                            IdentityState::VerificationViolation => {
                                IdentityWarningKind::VerificationLost
                            }
                            IdentityState::Pinned | IdentityState::Verified => continue,
                        };
                        let display_name = room
                            .get_member_no_sync(&change.user_id)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|member| member.display_name().map(str::to_owned));
                        warnings.push(IdentityWarning {
                            user_id: change.user_id,
                            display_name,
                            kind,
                        });
                    }
                    if sender.send(warnings.clone()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    Ok(receiver)
}

/// Acknowledges `user`'s new identity, or withdraws the verification of
/// their old one.
pub(crate) async fn acknowledge_identity(
    client: &Client,
    user: &UserId,
) -> Result<(), SessionError> {
    let identity = client
        .encryption()
        .get_user_identity(user)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| SessionError::Failed("That person's identity isn't known.".to_owned()))?;
    let result = if identity.has_verification_violation() {
        identity.withdraw_verification().await
    } else {
        identity.pin().await
    };
    result.map_err(|error| {
        warn!(%error, "acknowledging an identity change failed");
        SessionError::Failed("That couldn't be saved. Try again.".to_owned())
    })
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Where every account's incoming verification requests go: one frontend
/// listens.
#[derive(Debug, Clone, Default)]
pub(crate) struct VerificationFeed {
    sender: Arc<Mutex<Option<mpsc::Sender<IncomingVerification>>>>,
}

impl VerificationFeed {
    /// A new receiver, which replaces the previous one.
    pub(crate) fn subscribe(&self) -> mpsc::Receiver<IncomingVerification> {
        let (sender, receiver) = mpsc::channel(SUBSCRIPTION_BUFFER);
        *self.sender.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        receiver
    }

    /// Sends `flow` if a frontend is listening; `false` otherwise.
    fn send(&self, account: AccountKey, flow: VerificationFlow) -> bool {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        sender.is_some_and(|sender| sender.try_send((account, flow)).is_ok())
    }
}

/// Hands every verification request one of the account's own devices
/// sends to the feed. Requests from other people are left alone: Family
/// Chat verifies the account's own devices (ADR 0009).
pub(crate) fn register_incoming(client: &Client, account: AccountKey, feed: VerificationFeed) {
    client.add_event_handler(
        move |event: ToDeviceKeyVerificationRequestEvent, client: Client| {
            let account = account.clone();
            let feed = feed.clone();
            async move {
                if client.user_id() != Some(&event.sender) {
                    info!("ignored a verification request from someone else");
                    return;
                }
                let Some(request) = client
                    .encryption()
                    .get_verification_request(&event.sender, &event.content.transaction_id)
                    .await
                else {
                    return;
                };
                let with = VerifyingWith {
                    user_id: event.sender.clone(),
                    device_id: Some(event.content.from_device.to_string()),
                    device_name: client
                        .encryption()
                        .get_device(&event.sender, &event.content.from_device)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|device| device.display_name().map(str::to_owned)),
                    own: true,
                };
                let flow = drive(request.clone(), with);
                if !feed.send(account, flow) {
                    info!("no window is listening for the verification request; declining it");
                    let _ = request.cancel().await;
                }
            }
        },
    );
}

/// Asks the account's other devices to verify this one.
pub(crate) async fn verify_this_device(client: &Client) -> Result<VerificationFlow, SessionError> {
    let user_id = client.user_id().ok_or(SessionError::NotReady)?.to_owned();
    let identity = client
        .encryption()
        .get_user_identity(&user_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            SessionError::Failed(
                "Your account has no identity to confirm this device with yet.".to_owned(),
            )
        })?;
    let request = identity.request_verification().await.map_err(|error| {
        info!(%error, "asking the other devices to verify failed");
        SessionError::Failed("The request couldn't be sent. Try again.".to_owned())
    })?;
    Ok(drive(
        request,
        VerifyingWith {
            user_id,
            device_id: None,
            device_name: None,
            own: true,
        },
    ))
}

/// Asks another of the account's devices to verify itself.
pub(crate) async fn verify_device(
    client: &Client,
    device_id: &str,
) -> Result<VerificationFlow, SessionError> {
    let user_id = client.user_id().ok_or(SessionError::NotReady)?.to_owned();
    let device_id = OwnedDeviceId::from(device_id);
    let device = client
        .encryption()
        .get_device(&user_id, &device_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| SessionError::Failed("That device isn't known.".to_owned()))?;
    let name = device.display_name().map(str::to_owned);
    let request = device.request_verification().await.map_err(|error| {
        info!(%error, "asking a device to verify failed");
        SessionError::Failed("The request couldn't be sent. Try again.".to_owned())
    })?;
    Ok(drive(
        request,
        VerifyingWith {
            user_id,
            device_id: Some(device_id.to_string()),
            device_name: name,
            own: true,
        },
    ))
}

/// The verification under way after the request.
enum Under {
    Request,
    Sas(SasVerification),
    Qr(QrVerification),
}

/// Bridges `request` to a [`VerificationFlow`]: its states become steps,
/// and the person's answers act on it, until it ends.
fn drive(request: VerificationRequest, with: VerifyingWith) -> VerificationFlow {
    let (steps, steps_rx) = watch::channel(if request.we_started() {
        VerificationStep::Waiting
    } else {
        VerificationStep::Incoming
    });
    let (answers_tx, answers) = mpsc::unbounded_channel();
    tokio::spawn(run_verification(request, steps, answers));
    VerificationFlow::new(with, steps_rx, answers_tx)
}

fn cancelled(info: &matrix_sdk::encryption::verification::CancelInfo) -> VerificationStep {
    VerificationStep::Cancelled(wording::cancelled(
        info.cancel_code().as_str(),
        info.cancelled_by_us(),
    ))
}

/// The QR code `qr` shows, as modules.
fn qr_modules(qr: &QrVerification) -> Option<QrModules> {
    let code = qr.to_qr_code().ok()?;
    Some(QrModules {
        width: code.width(),
        dark: code
            .to_colors()
            .into_iter()
            .map(|color| color == Color::Dark)
            .collect(),
    })
}

fn emoji(sas: &SasVerification) -> Option<VerificationStep> {
    let emojis = sas.emoji()?;
    Some(VerificationStep::CompareEmoji(
        emojis
            .iter()
            .map(|emoji| SasEmoji {
                symbol: emoji.symbol.to_owned(),
                description: emoji.description.to_owned(),
            })
            .collect(),
    ))
}

async fn run_verification(
    request: VerificationRequest,
    steps: watch::Sender<VerificationStep>,
    mut answers: mpsc::UnboundedReceiver<VerificationAnswer>,
) {
    let show = |step: VerificationStep| {
        steps.send_if_modified(|current| {
            let changed = *current != step;
            *current = step;
            changed
        });
    };
    let mut request_changes = std::pin::pin!(request.changes());
    let mut under = Under::Request;
    let mut sas_changes: Option<
        std::pin::Pin<Box<dyn futures_util::Stream<Item = SasState> + Send>>,
    > = None;
    let mut qr_changes: Option<
        std::pin::Pin<Box<dyn futures_util::Stream<Item = QrVerificationState> + Send>>,
    > = None;
    let mut answers_open = true;

    loop {
        if steps.borrow().is_final() {
            return;
        }
        tokio::select! {
            state = request_changes.next() => {
                let Some(state) = state else { return };
                match state {
                    VerificationRequestState::Created { .. } => show(VerificationStep::Waiting),
                    VerificationRequestState::Requested { .. } => show(VerificationStep::Incoming),
                    VerificationRequestState::Ready { .. } => {
                        // A QR code for the other device to scan, if it can.
                        let qr = match request.generate_qr_code().await {
                            Ok(Some(qr)) => {
                                let modules = qr_modules(&qr);
                                qr_changes = Some(Box::pin(qr.changes()));
                                under = Under::Qr(qr);
                                modules
                            }
                            _ => None,
                        };
                        show(VerificationStep::Ready { qr });
                    }
                    VerificationRequestState::Transitioned { verification, .. } => {
                        match verification {
                            matrix_sdk::encryption::verification::Verification::SasV1(sas) => {
                                if !sas.we_started() {
                                    // The other device chose emoji.
                                    if let Err(error) = sas.accept().await {
                                        info!(%error, "accepting the emoji comparison failed");
                                    }
                                }
                                // Subscribed before looking, so no change
                                // falls between the two.
                                sas_changes = Some(Box::pin(sas.changes()));
                                show(emoji(&sas).unwrap_or(VerificationStep::Starting));
                                qr_changes = None;
                                under = Under::Sas(sas);
                            }
                            matrix_sdk::encryption::verification::Verification::QrV1(qr) => {
                                qr_changes = Some(Box::pin(qr.changes()));
                                under = Under::Qr(qr);
                            }
                            _ => {}
                        }
                    }
                    VerificationRequestState::Done => show(VerificationStep::Done),
                    VerificationRequestState::Cancelled(info) => show(cancelled(&info)),
                }
            }
            state = async { match sas_changes.as_mut() { Some(changes) => changes.next().await, None => std::future::pending().await } } => {
                let Some(state) = state else { sas_changes = None; continue };
                match state {
                    SasState::Created { .. } | SasState::Started { .. } | SasState::Accepted { .. } => {
                        show(VerificationStep::Starting);
                    }
                    SasState::KeysExchanged { .. } => {
                        if let Under::Sas(sas) = &under
                            && let Some(step) = emoji(sas)
                        {
                            show(step);
                        }
                    }
                    SasState::Confirmed => show(VerificationStep::Confirming),
                    SasState::Done { .. } => show(VerificationStep::Done),
                    SasState::Cancelled(info) => show(cancelled(&info)),
                }
            }
            state = async { match qr_changes.as_mut() { Some(changes) => changes.next().await, None => std::future::pending().await } } => {
                let Some(state) = state else { qr_changes = None; continue };
                match state {
                    QrVerificationState::Started => {}
                    QrVerificationState::Scanned => show(VerificationStep::Scanned),
                    QrVerificationState::Confirmed | QrVerificationState::Reciprocated => {
                        show(VerificationStep::Confirming);
                    }
                    QrVerificationState::Done { .. } => show(VerificationStep::Done),
                    QrVerificationState::Cancelled(info) => show(cancelled(&info)),
                }
            }
            answer = answers.recv(), if answers_open => {
                let Some(answer) = answer else {
                    // The frontend went away; the verification goes on
                    // until it ends or times out.
                    answers_open = false;
                    continue;
                };
                let result = match (answer, &under) {
                    (VerificationAnswer::Accept, _) => request.accept().await,
                    (VerificationAnswer::CompareEmoji, _) => match request.start_sas().await {
                        Ok(Some(sas)) => {
                            sas_changes = Some(Box::pin(sas.changes()));
                            show(emoji(&sas).unwrap_or(VerificationStep::Starting));
                            qr_changes = None;
                            under = Under::Sas(sas);
                            Ok(())
                        }
                        Ok(None) => Ok(()),
                        Err(error) => Err(error),
                    },
                    (VerificationAnswer::Match, Under::Sas(sas)) => sas.confirm().await,
                    (VerificationAnswer::NoMatch, Under::Sas(sas)) => sas.mismatch().await,
                    (VerificationAnswer::ConfirmScanned, Under::Qr(qr)) => qr.confirm().await,
                    (VerificationAnswer::Cancel, Under::Sas(sas)) => sas.cancel().await,
                    (VerificationAnswer::Cancel, Under::Qr(qr)) if !matches!(qr.state(), QrVerificationState::Started) => qr.cancel().await,
                    (VerificationAnswer::Cancel, _) => request.cancel().await,
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    info!(%error, "a verification answer couldn't be sent");
                    show(VerificationStep::Cancelled(
                        "The verification couldn't continue. Check your connection and start it again."
                            .to_owned(),
                    ));
                }
            }
        }
    }
}
