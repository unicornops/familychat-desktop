//! Handing new credentials to matrix-sdk (PLAN.md, sections 6.4 rule 7 and
//! 6.6): [`AccountSink`], which `familychat-session`'s `AccountManager`
//! implements by restoring the session (`restore_session`) and running a
//! [`PostLoginCheck`] before the account syncs, and the checks a sign-in
//! passes there.
//!
//! [`FamilyChatCheck`] is the post-login re-check every Family Chat sign-in
//! gets (code or password): the client's homeserver, as the SDK holds it
//! after the session is restored, must still be under the allowlist, and
//! the client's user must be the one the sign-in was bound to. A refusal
//! signs the new device out and forgets the account (the session crate
//! does both).

use familychat_core::{OwnedUserId, UserId, session::AccountKey};
use familychat_security::{AccountSession, Secret};
use familychat_session::{
    AccountManager, AddAccountError, OpenError, PostLoginCheck, QrAnswer, QrEvent, QrLoginError,
    QrLoginRequest, UrlGuard, account_key,
};
use futures_util::future::BoxFuture;
use reqwest::Url;
use tokio::sync::mpsc;
use tracing::warn;

use crate::{
    allowlist::{Allowlist, CheckedHomeserver},
    link::{parse_user_id, same_user},
    wording,
};

/// Why a sign-in's credentials could not become an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddFailure {
    /// The post-login check refused it; the device is already signed out.
    /// The text is the refusal's wording.
    Refused(String),
    /// The server has no native sliding sync.
    NotSupported,
    /// Anything else (the keychain, the store); the device is still signed
    /// in, so the sign-in signs it out.
    Failed(String),
}

/// Where the sign-in's accounts go: the account manager, or a fake in
/// tests.
pub trait AccountSink: Send + Sync {
    /// Whether an account with this Matrix ID is signed in on this
    /// profile already.
    fn is_signed_in(&self, user_id: &UserId) -> bool;

    /// Restores `session` as a new account, runs `check` once the client
    /// has it, and starts its sync. `password` is a password sign-in's:
    /// the session uses it once, if the server asks for it when the
    /// account's cross-signing identity is made (ADR 0009), and wipes it.
    /// It is never stored.
    fn add(
        &self,
        session: AccountSession,
        check: Box<dyn PostLoginCheck>,
        password: Option<Secret<String>>,
    ) -> BoxFuture<'_, Result<AccountKey, AddFailure>>;

    /// Signs this computer in with a QR code a signed-in device scans
    /// (ADR 0010): progress on `events`, the person's answers from
    /// `answers`, `check` on the new account before anything of it is
    /// kept. The new account and who signed in.
    fn add_with_qr_code(
        &self,
        request: QrLoginRequest,
        check: Box<dyn PostLoginCheck>,
        events: mpsc::Sender<QrEvent>,
        answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> BoxFuture<'_, Result<(AccountKey, OwnedUserId), QrLoginError>> {
        let _ = (request, check, events, answers);
        Box::pin(async { Err(QrLoginError::NotSupported) })
    }

    /// Signs a new device in to `account` with a QR code it scans.
    fn link_device(
        &self,
        account: &AccountKey,
        guard: UrlGuard,
        events: mpsc::Sender<QrEvent>,
        answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> BoxFuture<'_, Result<(), QrLoginError>> {
        let _ = (account, guard, events, answers);
        Box::pin(async { Err(QrLoginError::NotSupported) })
    }

    /// An open account's homeserver.
    fn homeserver_of(&self, account: &AccountKey) -> Option<Url> {
        let _ = account;
        None
    }
}

impl AccountSink for AccountManager {
    fn is_signed_in(&self, user_id: &UserId) -> bool {
        match self.with_profile(|profile| profile.accounts()) {
            Ok(accounts) => accounts.iter().any(|account| {
                parse_user_id(&account.user_id).is_some_and(|known| same_user(&known, user_id))
            }),
            Err(error) => {
                warn!(%error, "could not read the accounts to look for an existing sign-in");
                false
            }
        }
    }

    fn add(
        &self,
        session: AccountSession,
        check: Box<dyn PostLoginCheck>,
        password: Option<Secret<String>>,
    ) -> BoxFuture<'_, Result<AccountKey, AddFailure>> {
        Box::pin(async move {
            match self
                .add_account_with_password(session, check.as_ref(), password)
                .await
            {
                Ok(id) => Ok(account_key(id)),
                Err(AddAccountError::Open(OpenError::Refused(refusal))) => {
                    Err(AddFailure::Refused(refusal))
                }
                Err(AddAccountError::Open(OpenError::NotSupported)) => {
                    Err(AddFailure::NotSupported)
                }
                Err(error) => Err(AddFailure::Failed(error.to_string())),
            }
        })
    }

    fn add_with_qr_code(
        &self,
        request: QrLoginRequest,
        check: Box<dyn PostLoginCheck>,
        events: mpsc::Sender<QrEvent>,
        answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> BoxFuture<'_, Result<(AccountKey, OwnedUserId), QrLoginError>> {
        Box::pin(async move {
            let id = self
                .add_account_with_qr_code(request, check.as_ref(), events, answers)
                .await?;
            let user_id = self
                .account(id)
                .map(|account| account.view().user_id.clone())
                .ok_or_else(|| QrLoginError::Failed("the new account is not open".to_owned()))?;
            Ok((account_key(id), user_id))
        })
    }

    fn link_device(
        &self,
        account: &AccountKey,
        guard: UrlGuard,
        events: mpsc::Sender<QrEvent>,
        answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> BoxFuture<'_, Result<(), QrLoginError>> {
        let account = self.account_by_key(account).map(|account| account.id());
        Box::pin(async move {
            let id = account
                .ok_or_else(|| QrLoginError::Failed("the account is not open".to_owned()))?;
            self.link_device_with_qr_code(id, guard, events, answers)
                .await
        })
    }

    fn homeserver_of(&self, account: &AccountKey) -> Option<Url> {
        let account = self.account_by_key(account)?;
        Url::parse(&account.view().homeserver_url).ok()
    }
}

/// The post-login re-check for a Family Chat sign-in.
#[derive(Debug, Clone)]
pub struct FamilyChatCheck {
    allowlist: Allowlist,
    user_id: Option<OwnedUserId>,
}

impl FamilyChatCheck {
    /// The check for a sign-in of `user_id` under `allowlist`.
    pub fn new(allowlist: Allowlist, user_id: OwnedUserId) -> Self {
        Self {
            allowlist,
            user_id: Some(user_id),
        }
    }

    /// The check for a sign-in that learns who signed in only from the
    /// server (a QR code's, ADR 0010): the homeserver alone.
    pub fn any_user(allowlist: Allowlist) -> Self {
        Self {
            allowlist,
            user_id: None,
        }
    }
}

impl PostLoginCheck for FamilyChatCheck {
    fn check(&self, homeserver: &Url, user_id: &UserId) -> Result<(), String> {
        if self.allowlist.check(homeserver).is_err() {
            warn!("the client's homeserver is outside the allowlist after the sign-in");
            return Err(wording::NOT_FAMILY_CHAT.to_owned());
        }
        if self
            .user_id
            .as_ref()
            .is_some_and(|expected| !same_user(expected, user_id))
        {
            warn!("the client's user is not the one the sign-in issued");
            return Err(wording::SIGNIN_FAILED.to_owned());
        }
        Ok(())
    }
}

/// The post-login check for another Matrix server: HTTPS, and the user the
/// sign-in issued.
#[derive(Debug, Clone)]
pub struct OtherServerCheck {
    user_id: Option<OwnedUserId>,
}

impl OtherServerCheck {
    /// The check for a sign-in of `user_id`.
    pub fn new(user_id: OwnedUserId) -> Self {
        Self {
            user_id: Some(user_id),
        }
    }

    /// The check for a QR code's sign-in: HTTPS alone.
    pub fn any_user() -> Self {
        Self { user_id: None }
    }
}

impl PostLoginCheck for OtherServerCheck {
    fn check(&self, homeserver: &Url, user_id: &UserId) -> Result<(), String> {
        if CheckedHomeserver::other_server(homeserver).is_err() {
            return Err(wording::NOT_HTTPS.to_owned());
        }
        if self
            .user_id
            .as_ref()
            .is_some_and(|expected| !same_user(expected, user_id))
        {
            return Err(wording::SIGNIN_FAILED.to_owned());
        }
        Ok(())
    }
}

/// The post-login check for a sign-in at `homeserver`: Family Chat's
/// when it passed the allowlist, another server's otherwise.
pub fn check_for(
    homeserver: &CheckedHomeserver,
    allowlist: &Allowlist,
    user_id: Option<OwnedUserId>,
) -> Box<dyn PostLoginCheck> {
    match (homeserver.is_family_chat(), user_id) {
        (true, Some(user)) => Box::new(FamilyChatCheck::new(allowlist.clone(), user)),
        (true, None) => Box::new(FamilyChatCheck::any_user(allowlist.clone())),
        (false, Some(user)) => Box::new(OtherServerCheck::new(user)),
        (false, None) => Box::new(OtherServerCheck::any_user()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> OwnedUserId {
        parse_user_id(text).expect("valid")
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("url")
    }

    #[test]
    fn the_family_chat_check_refuses_another_server_or_user() {
        let check = FamilyChatCheck::new(Allowlist::production(), user("@ana:smith.ie"));
        assert_eq!(
            check.check(
                &url("https://smith.safechat.family/"),
                &user("@ana:smith.ie")
            ),
            Ok(())
        );
        assert_eq!(
            check.check(&url("https://evil.example/"), &user("@ana:smith.ie")),
            Err(wording::NOT_FAMILY_CHAT.to_owned())
        );
        assert_eq!(
            check.check(&url("https://safechat.family/"), &user("@ana:smith.ie")),
            Err(wording::NOT_FAMILY_CHAT.to_owned())
        );
        assert_eq!(
            check.check(
                &url("http://smith.safechat.family/"),
                &user("@ana:smith.ie")
            ),
            Err(wording::NOT_FAMILY_CHAT.to_owned())
        );
        assert!(
            check
                .check(
                    &url("https://smith.safechat.family/"),
                    &user("@bob:smith.ie")
                )
                .is_err()
        );
    }

    #[test]
    fn the_other_server_check_needs_https_and_the_user() {
        let check = OtherServerCheck::new(user("@a:matrix.org"));
        assert_eq!(
            check.check(
                &url("https://matrix-client.matrix.org/"),
                &user("@a:matrix.org")
            ),
            Ok(())
        );
        assert!(
            check
                .check(&url("http://matrix.org/"), &user("@a:matrix.org"))
                .is_err()
        );
        assert!(
            check
                .check(&url("https://matrix.org/"), &user("@b:matrix.org"))
                .is_err()
        );
    }
}
