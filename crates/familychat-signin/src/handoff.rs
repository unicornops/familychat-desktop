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
use familychat_security::AccountSession;
use familychat_session::{AccountManager, AddAccountError, OpenError, PostLoginCheck, account_key};
use futures_util::future::BoxFuture;
use reqwest::Url;
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
    /// has it, and starts its sync.
    fn add(
        &self,
        session: AccountSession,
        check: Box<dyn PostLoginCheck>,
    ) -> BoxFuture<'_, Result<AccountKey, AddFailure>>;
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
    ) -> BoxFuture<'_, Result<AccountKey, AddFailure>> {
        Box::pin(async move {
            match self.add_account(session, check.as_ref()).await {
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
}

/// The post-login re-check for a Family Chat sign-in.
#[derive(Debug, Clone)]
pub struct FamilyChatCheck {
    allowlist: Allowlist,
    user_id: OwnedUserId,
}

impl FamilyChatCheck {
    /// The check for a sign-in of `user_id` under `allowlist`.
    pub fn new(allowlist: Allowlist, user_id: OwnedUserId) -> Self {
        Self { allowlist, user_id }
    }
}

impl PostLoginCheck for FamilyChatCheck {
    fn check(&self, homeserver: &Url, user_id: &UserId) -> Result<(), String> {
        if self.allowlist.check(homeserver).is_err() {
            warn!("the client's homeserver is outside the allowlist after the sign-in");
            return Err(wording::NOT_FAMILY_CHAT.to_owned());
        }
        if !same_user(&self.user_id, user_id) {
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
    user_id: OwnedUserId,
}

impl OtherServerCheck {
    /// The check for a sign-in of `user_id`.
    pub fn new(user_id: OwnedUserId) -> Self {
        Self { user_id }
    }
}

impl PostLoginCheck for OtherServerCheck {
    fn check(&self, homeserver: &Url, user_id: &UserId) -> Result<(), String> {
        if CheckedHomeserver::other_server(homeserver).is_err() {
            return Err(wording::NOT_HTTPS.to_owned());
        }
        if !same_user(&self.user_id, user_id) {
            return Err(wording::SIGNIN_FAILED.to_owned());
        }
        Ok(())
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
