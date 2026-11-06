//! Sign-in with a QR code (MSC4108; ADR 0010), through the SDK's OAuth
//! API: this computer shows the code and a signed-in device scans it
//! ([`AccountManager::add_account_with_qr_code`]), or a signed-in account
//! shows one for a new device to scan
//! ([`AccountManager::link_device_with_qr_code`]).
//!
//! The rules are `familychat-signin`'s: it checks the homeserver and the
//! OAuth issuer before either starts, and hands in a [`UrlGuard`] that
//! every address the SDK is about to use is checked against while it runs
//! (the rendezvous the QR code names, the homeserver the other device
//! names), and a [`PostLoginCheck`] the new account passes before anything
//! of it is stored. A refusal stops the sign-in, signs the new device out
//! (revoking its tokens) and forgets it.
//!
//! The secure channel, the device authorization grant and the transfer of
//! the account's secrets are the SDK's. Each side's progress arrives as
//! [`QrEvent`]s and the person's answers go back as [`QrAnswer`]s; the
//! check code is the only thing typed. The QR code carries the rendezvous
//! and this side's key, so it travels as [`QrModules`] (whose `Debug` is
//! its size) and its bytes as a [`Secret`], for the tests.

use std::sync::Arc;

use familychat_core::encryption::QrModules;
use familychat_security::{
    AccountId, AccountSession, AuthKind, Secret, keychain::save_session,
    logging::create_private_dir,
};
use familychat_store::{NewAccount, StoreError};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client,
    authentication::oauth::{
        ClientRegistrationData,
        qrcode::{
            GeneratedQrProgress, GrantLoginProgress, LoginFailureReason, LoginProgress,
            QRCodeGrantLoginError, QRCodeLoginError, QrCodeData, QrCodeIntentData,
            SecureChannelError,
        },
    },
    ruma::{OwnedUserId, serde::Raw},
    sliding_sync::VersionBuilder,
};
use matrix_sdk_base::crypto::matrix_sdk_qrcode::qrcode::{Color, QrCode};
use reqwest::Url;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    AccountManager, PostLoginCheck,
    account::{Account, OpenError, OpenParams, client_builder},
    failure::Failure,
};

/// Checks an address the SDK is about to use during a QR sign-in: `true`
/// to go on. `familychat-signin` makes it from its allowlist rules.
pub type UrlGuard = Arc<dyn Fn(&Url) -> bool + Send + Sync>;

/// How a QR sign-in is going.
pub enum QrEvent {
    /// The code to show.
    Ready {
        /// To draw.
        qr: QrModules,
        /// Its bytes (the tests decode the drawing and compare).
        data: Secret<Vec<u8>>,
    },
    /// The other device scanned it: the check code it shows is needed
    /// ([`QrAnswer::CheckCode`]).
    Scanned,
    /// This computer signing in: the other device approves the sign-in
    /// with the server, which may show it this code.
    WaitingForApproval {
        /// The server's user code.
        user_code: String,
    },
    /// Signing a new device in: approve it in the browser at this address,
    /// then [`QrAnswer::Continue`] (or [`QrAnswer::Cancel`]).
    ApproveInBrowser {
        /// The server's verification address (with the device's code).
        verification_uri: Secret<String>,
    },
    /// The keys are being sent or received.
    SyncingSecrets,
}

impl std::fmt::Debug for QrEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready { qr, .. } => write!(f, "Ready({qr:?})"),
            Self::Scanned => f.write_str("Scanned"),
            Self::WaitingForApproval { .. } => f.write_str("WaitingForApproval"),
            Self::ApproveInBrowser { .. } => f.write_str("ApproveInBrowser"),
            Self::SyncingSecrets => f.write_str("SyncingSecrets"),
        }
    }
}

/// What the person answers during a QR sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrAnswer {
    /// The two-digit check code the other device shows.
    CheckCode(u8),
    /// The new device's sign-in is being approved in the browser.
    Continue,
    /// Stop.
    Cancel,
}

/// Why a QR sign-in stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QrLoginError {
    /// The code expired, or the other device went away.
    #[error("the QR code expired")]
    Expired,
    /// The check code typed is not the other device's.
    #[error("the check code did not match")]
    CheckCodeMismatch,
    /// The sign-in was declined, on the other device or in the browser.
    #[error("the sign-in was declined")]
    Declined,
    /// The person cancelled (or the frontend went away).
    #[error("the sign-in was cancelled")]
    Cancelled,
    /// The signed-in account has no keys to hand the new device (its
    /// identity is not set up on this computer).
    #[error("this device does not hold the account's keys")]
    NoSecrets,
    /// The new account is signed in on this profile already (its new
    /// device has been signed out).
    #[error("the account is signed in already")]
    AlreadySignedIn(OwnedUserId),
    /// An address or the new account was refused by the sign-in's rules;
    /// the text is the refusal's wording.
    #[error("{0}")]
    Refused(String),
    /// The server can't do this (no device authorization grant, no
    /// rendezvous, or no native sliding sync).
    #[error("the server does not support this")]
    NotSupported,
    /// Anything else.
    #[error("{0}")]
    Failed(String),
}

/// A QR sign-in of this computer.
pub struct QrLoginRequest {
    /// The homeserver, checked by the sign-in.
    pub homeserver: Url,
    /// The OAuth client metadata to register with (dynamic registration).
    pub client_metadata: serde_json::Value,
    /// Checks the rendezvous and any homeserver the other device names.
    pub guard: UrlGuard,
}

impl std::fmt::Debug for QrLoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrLoginRequest").finish_non_exhaustive()
    }
}

/// The QR code `data` makes, as modules.
pub(crate) fn qr_modules(data: &QrCodeData) -> Result<QrModules, QrLoginError> {
    let code = QrCode::new(data.to_bytes())
        .map_err(|error| QrLoginError::Failed(format!("could not draw the QR code: {error}")))?;
    Ok(QrModules {
        width: code.width(),
        dark: code
            .to_colors()
            .into_iter()
            .map(|color| color == Color::Dark)
            .collect(),
    })
}

/// The rendezvous `data` names.
fn rendezvous(data: &QrCodeData) -> Option<&Url> {
    match data.intent_data() {
        QrCodeIntentData::Msc4108 { rendezvous_url, .. } => Some(rendezvous_url),
        QrCodeIntentData::Msc4388 { base_url, .. } => Some(base_url),
    }
}

/// The QR code is ready: checks its rendezvous and shows it.
async fn ready(
    data: &QrCodeData,
    guard: &UrlGuard,
    events: &mpsc::Sender<QrEvent>,
) -> Result<(), QrLoginError> {
    let allowed = rendezvous(data).is_some_and(|url| guard(url));
    if !allowed {
        warn!("the QR code's rendezvous is not a server the sign-in may use");
        return Err(QrLoginError::Refused(
            "The server's QR code sign-in isn't on a server Family Chat may use.".to_owned(),
        ));
    }
    let qr = qr_modules(data)?;
    let _ = events
        .send(QrEvent::Ready {
            qr,
            data: Secret::new(data.to_bytes()),
        })
        .await;
    Ok(())
}

fn login_error(error: QRCodeLoginError) -> QrLoginError {
    match error {
        QRCodeLoginError::NotFound => QrLoginError::Expired,
        QRCodeLoginError::SecureChannel(SecureChannelError::InvalidCheckCode) => {
            QrLoginError::CheckCodeMismatch
        }
        QRCodeLoginError::LoginFailure { reason, .. } => failure_reason(&reason),
        QRCodeLoginError::OAuth(error) => {
            use matrix_sdk::authentication::oauth::qrcode::{
                DeviceAuthorizationOAuthError, DeviceCodeErrorResponseType,
            };
            match error.as_request_token_error() {
                Some(DeviceCodeErrorResponseType::AccessDenied) => QrLoginError::Declined,
                Some(DeviceCodeErrorResponseType::ExpiredToken) => QrLoginError::Expired,
                _ if matches!(
                    error,
                    DeviceAuthorizationOAuthError::NoDeviceAuthorizationEndpoint
                ) =>
                {
                    QrLoginError::NotSupported
                }
                _ => QrLoginError::Failed(error.to_string()),
            }
        }
        error => QrLoginError::Failed(error.to_string()),
    }
}

fn grant_error(error: QRCodeGrantLoginError) -> QrLoginError {
    match error {
        QRCodeGrantLoginError::NotFound => QrLoginError::Expired,
        QRCodeGrantLoginError::InvalidCheckCode
        | QRCodeGrantLoginError::SecureChannel(SecureChannelError::InvalidCheckCode) => {
            QrLoginError::CheckCodeMismatch
        }
        QRCodeGrantLoginError::MissingSecretsBackup(_) => QrLoginError::NoSecrets,
        QRCodeGrantLoginError::LoginFailure { reason } => failure_reason(&reason),
        QRCodeGrantLoginError::UnsupportedProtocol(_) => QrLoginError::NotSupported,
        error => QrLoginError::Failed(error.to_string()),
    }
}

fn failure_reason(reason: &LoginFailureReason) -> QrLoginError {
    match reason {
        LoginFailureReason::UserCancelled => QrLoginError::Declined,
        LoginFailureReason::AuthorizationExpired => QrLoginError::Expired,
        LoginFailureReason::UnsupportedProtocol => QrLoginError::NotSupported,
        reason => QrLoginError::Failed(format!("the other device stopped the sign-in: {reason}")),
    }
}

impl AccountManager {
    /// Signs this computer in with a QR code a signed-in device scans:
    /// makes the account's stores, shows the code, takes the check code,
    /// lets the other device approve the sign-in, receives the account's
    /// secrets, runs `check` on the new account before anything of it is
    /// kept, then adds it and starts its sync. Anything that fails (a
    /// refusal included) signs the new device out and forgets it.
    pub async fn add_account_with_qr_code(
        &self,
        request: QrLoginRequest,
        check: &dyn PostLoginCheck,
        events: mpsc::Sender<QrEvent>,
        mut answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> Result<AccountId, QrLoginError> {
        let id = AccountId::generate()
            .map_err(|error| QrLoginError::Failed(format!("no random bytes: {error}")))?;
        let mut signed_in = None;
        let result = self
            .qr_sign_in(id, &request, check, &events, &mut answers, &mut signed_in)
            .await;
        match result {
            Ok(id) => Ok(id),
            Err(error) => {
                if let Some(client) = signed_in {
                    info!(account = %id, "signing out the QR sign-in that is not kept");
                    if let Err(error) = client.logout().await {
                        warn!(account = %id, %error, "could not sign the QR sign-in out");
                    }
                }
                self.forget(id).await;
                Err(error)
            }
        }
    }

    async fn qr_sign_in(
        &self,
        id: AccountId,
        request: &QrLoginRequest,
        check: &dyn PostLoginCheck,
        events: &mpsc::Sender<QrEvent>,
        answers: &mut mpsc::UnboundedReceiver<QrAnswer>,
        signed_in: &mut Option<Client>,
    ) -> Result<AccountId, QrLoginError> {
        let failed = |error: String| QrLoginError::Failed(error);
        let passphrase = self
            .profile()
            .account_store_passphrase(id)
            .map_err(|error| failed(error.to_string()))?;
        let sdk_dir = self.dir.account_sdk_dir(id);
        let cache_dir = self.dir.account_cache_dir(id);
        for dir in [&sdk_dir, &cache_dir] {
            create_private_dir(dir).map_err(|error| failed(error.to_string()))?;
        }
        let client = client_builder(
            request.homeserver.as_str(),
            &sdk_dir,
            &cache_dir,
            &passphrase.to_hex(),
            &self.config,
        )
        .sliding_sync_version_builder(VersionBuilder::DiscoverNative)
        .build()
        .await
        .map_err(|error| match Failure::from_build(&error) {
            Failure::NotSupported => QrLoginError::NotSupported,
            _ => failed(error.to_string()),
        })?;
        let metadata = serde_json::value::to_raw_value(&request.client_metadata)
            .map_err(|error| failed(error.to_string()))?;
        let registration = ClientRegistrationData::new(Raw::from_json(metadata));

        let oauth = client.oauth();
        let login = oauth.login_with_qr_code(Some(&registration)).generate();
        let mut progress = login.subscribe_to_progress();
        let mut login = std::pin::pin!(login.into_future());
        let mut check_code = None;
        loop {
            tokio::select! {
                result = &mut login => {
                    result.map_err(login_error)?;
                    break;
                }
                Some(step) = progress.next() => match step {
                    LoginProgress::EstablishingSecureChannel(GeneratedQrProgress::QrReady(data)) => {
                        ready(&data, &request.guard, events).await?;
                    }
                    LoginProgress::EstablishingSecureChannel(GeneratedQrProgress::QrScanned(sender)) => {
                        check_code = Some(sender);
                        let _ = events.send(QrEvent::Scanned).await;
                    }
                    LoginProgress::WaitingForToken { user_code } => {
                        // The other device may name another homeserver
                        // (MSC4108's `m.login.protocols`); the SDK has
                        // switched to it by now, before any token is
                        // issued.
                        if !(request.guard)(&client.homeserver()) {
                            warn!("the other device named a homeserver the sign-in may not use");
                            return Err(QrLoginError::Refused(
                                "The other device is signed in to a server Family Chat may not use."
                                    .to_owned(),
                            ));
                        }
                        let _ = events.send(QrEvent::WaitingForApproval { user_code }).await;
                    }
                    LoginProgress::SyncingSecrets => {
                        let _ = events.send(QrEvent::SyncingSecrets).await;
                    }
                    LoginProgress::Starting | LoginProgress::Done => {}
                },
                answer = answers.recv() => match answer {
                    Some(QrAnswer::CheckCode(code)) => {
                        if let Some(sender) = check_code.take() {
                            let _ = sender.send(code).await;
                        }
                    }
                    Some(QrAnswer::Continue) => {}
                    Some(QrAnswer::Cancel) | None => return Err(QrLoginError::Cancelled),
                },
            }
        }
        *signed_in = Some(client.clone());

        let (Some(meta), Some(tokens), Some(client_id)) = (
            client.session_meta().cloned(),
            client.session_tokens(),
            client.oauth().client_id().cloned(),
        ) else {
            return Err(failed("the QR sign-in left no session".to_owned()));
        };
        let homeserver = client.homeserver();
        check
            .check(&homeserver, &meta.user_id)
            .map_err(QrLoginError::Refused)?;

        let session = AccountSession {
            user_id: meta.user_id.to_string(),
            device_id: meta.device_id.to_string(),
            homeserver_url: homeserver.as_str().trim_end_matches('/').to_owned(),
            access_token: Secret::new(tokens.access_token),
            refresh_token: tokens.refresh_token.map(Secret::new),
            auth_kind: AuthKind::QrCode,
            oauth_client_id: Some(client_id.as_str().to_owned()),
        };
        match self.profile().add_account_with_id(
            id,
            &NewAccount {
                user_id: session.user_id.clone(),
                homeserver_url: session.homeserver_url.clone(),
            },
        ) {
            Ok(()) => {}
            Err(StoreError::DuplicateAccount) => {
                return Err(QrLoginError::AlreadySignedIn(meta.user_id));
            }
            Err(error) => return Err(failed(error.to_string())),
        }
        save_session(self.keychain.as_ref(), id, &session)
            .map_err(|error| failed(error.to_string()))?;
        let account = Account::open(
            OpenParams {
                id,
                session,
                passphrase,
                dir: self.dir.clone(),
                config: self.config.clone(),
                keychain: self.keychain.clone(),
                notifications: self.notification_feed().clone(),
                verifications: self.verification_feed().clone(),
            },
            None,
            None,
            Some(client),
        )
        .await
        .map_err(|error| match error {
            OpenError::NotSupported => QrLoginError::NotSupported,
            OpenError::Refused(refusal) => QrLoginError::Refused(refusal),
            error => failed(error.to_string()),
        })?;
        info!(account = %id, "added the account a QR code signed in");
        self.insert(account);
        Ok(id)
    }

    /// Signs a new device in to the open account `id` with a QR code it
    /// scans (MSC4108): shows the code, takes the check code the new device
    /// shows, hands the sign-in's address to approve in the browser
    /// ([`QrEvent::ApproveInBrowser`], answered with
    /// [`QrAnswer::Continue`]), then sends the new device the account's
    /// secrets. `guard` checks the rendezvous the code names.
    pub async fn link_device_with_qr_code(
        &self,
        id: AccountId,
        guard: UrlGuard,
        events: mpsc::Sender<QrEvent>,
        mut answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> Result<(), QrLoginError> {
        let client = self
            .account(id)
            .and_then(|account| account.client().cloned())
            .ok_or_else(|| QrLoginError::Failed("the account is not open".to_owned()))?;
        let oauth = client.oauth();
        let grant = oauth.grant_login_with_qr_code().generate();
        let mut progress = grant.subscribe_to_progress();
        let mut grant = std::pin::pin!(grant.into_future());
        let mut check_code = None;
        let mut continuation = None;
        loop {
            tokio::select! {
                result = &mut grant => {
                    result.map_err(grant_error)?;
                    info!(account = %id, "a new device signed in with this account's QR code");
                    return Ok(());
                }
                Some(step) = progress.next() => match step {
                    GrantLoginProgress::EstablishingSecureChannel(GeneratedQrProgress::QrReady(data)) => {
                        ready(&data, &guard, &events).await?;
                    }
                    GrantLoginProgress::EstablishingSecureChannel(GeneratedQrProgress::QrScanned(sender)) => {
                        check_code = Some(sender);
                        let _ = events.send(QrEvent::Scanned).await;
                    }
                    GrantLoginProgress::WaitingForAuth { verification_uri, continuation_sender } => {
                        continuation = Some(continuation_sender);
                        let _ = events
                            .send(QrEvent::ApproveInBrowser {
                                verification_uri: Secret::new(verification_uri.into()),
                            })
                            .await;
                    }
                    GrantLoginProgress::SyncingSecrets => {
                        let _ = events.send(QrEvent::SyncingSecrets).await;
                    }
                    GrantLoginProgress::Starting | GrantLoginProgress::Done => {}
                },
                answer = answers.recv() => match answer {
                    Some(QrAnswer::CheckCode(code)) => {
                        if let Some(sender) = check_code.take() {
                            let _ = sender.send(code).await;
                        }
                    }
                    Some(QrAnswer::Continue) => {
                        if let Some(sender) = continuation.take() {
                            let _ = sender.confirm().await;
                        }
                    }
                    Some(QrAnswer::Cancel) | None => {
                        if let Some(sender) = continuation.take() {
                            // Tells the new device, so it stops too.
                            let _ = sender.cancel().await;
                            let _ = (&mut grant).await;
                        }
                        return Err(QrLoginError::Cancelled);
                    }
                },
            }
        }
    }
}
