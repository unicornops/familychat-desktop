//! The sign-in flows (PLAN.md, sections 6.4 to 6.6), as one state machine
//! a frontend drives through [`SigninStep`]s and [`SigninAnswer`]s.
//!
//! A code link:
//!
//! 1. is parsed ([`LoginLink::parse`]); a code it carries that may not be
//!    sent is dropped there;
//! 2. stops at [`SigninStep::AlreadySignedIn`] when the account it names
//!    is signed in already (the link is not kept);
//! 3. always asks first ([`SigninStep::Confirm`]);
//! 4. on Continue, redeems the code once, at its `hs`, through the
//!    sign-in's own client, with one redemption in flight across every
//!    flow ([`Services::gate`]);
//! 5. binds the new session's user to the hint, or with no hint to
//!    `account_provider`, signing the device out otherwise;
//! 6. falls through to the password form after any failure (a `401` or
//!    `403` says the code was used or expired), signing out a device
//!    whose credentials were issued and then not kept;
//! 7. hands the credentials to matrix-sdk through [`AccountSink::add`],
//!    whose post-login check is [`FamilyChatCheck`].
//!
//! The password form a link leads to is pre-filled from `account_provider`
//! and the hint, never from `hs`: `hs` exists only inside the code, which
//! the password path never sees. Every password sign-in resolves the typed
//! server through discovery and, for Family Chat, the allowlist, before
//! anything but an unauthenticated `GET` is sent, and is sent once: a rate
//! limit is shown with its wait, never retried.

use std::sync::Arc;

use familychat_core::{
    OwnedUserId,
    signin::{
        ConfirmCode, Finished, PasswordForm, PasswordKind, SigninAnswer, SigninProblem,
        SigninStart, SigninStep, Working,
    },
};
use familychat_security::{AccountSession, AuthKind, Secret};
use familychat_session::PostLoginCheck;
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{info, warn};

use crate::{
    allowlist::Allowlist,
    discovery::{DiscoveryError, discover},
    handoff::{AccountSink, AddFailure, FamilyChatCheck, OtherServerCheck},
    host::ServerName,
    http::Transport,
    link::{LoginLink, SigninCode, parse_user_id, same_user, user_server},
    login::{Credentials, LoginFailure, login_password, logout, redeem_token},
    wording,
};

/// What a flow runs with.
pub struct Services {
    /// The sign-in's HTTP client.
    pub transport: Arc<dyn Transport>,
    /// Where new accounts go.
    pub accounts: Arc<dyn AccountSink>,
    /// The Family Chat allowlist ([`Allowlist::for_build`] in the app).
    pub allowlist: Allowlist,
    /// Held while a code is redeemed and its account opened: one
    /// redemption in flight, whichever flow it belongs to.
    pub gate: Arc<Mutex<()>>,
    /// The device display name ([`crate::device_display_name`]).
    pub device_name: String,
}

/// Runs one flow to its end: shows each step on `steps` and reads the
/// person's answers from `answers`. When the answers' sender is dropped
/// the flow ends as cancelled.
pub async fn run_flow(
    services: Arc<Services>,
    start: SigninStart,
    steps: watch::Sender<SigninStep>,
    answers: mpsc::UnboundedReceiver<SigninAnswer>,
) {
    let flow = Flow {
        services,
        steps,
        answers,
    };
    match start {
        SigninStart::Link(text) => {
            let parsed = LoginLink::parse(text.expose(), &flow.services.allowlist);
            drop(text);
            match parsed {
                Ok(link) => flow.code(link).await,
                Err(error) => {
                    info!(?error, "the text is not a sign-in link");
                    flow.show(SigninStep::NotALink(wording::NOT_A_LINK.to_owned()));
                }
            }
        }
        SigninStart::FamilyChatPassword => {
            flow.password(blank_form(PasswordKind::FamilyChat)).await;
        }
        SigninStart::OtherServer => flow.password(blank_form(PasswordKind::OtherServer)).await,
    }
}

fn blank_form(kind: PasswordKind) -> PasswordForm {
    PasswordForm {
        kind,
        server: String::new(),
        server_placeholder: match kind {
            PasswordKind::FamilyChat => wording::FAMILY_SERVER_PLACEHOLDER.to_owned(),
            PasswordKind::OtherServer => "matrix.org".to_owned(),
        },
        username: String::new(),
        notice: None,
        problem: None,
    }
}

/// How a sign-in attempt that did not finish ended.
enum NotFinished {
    /// Back to the form with this problem.
    Problem(SigninProblem),
    /// The account was signed in already (final).
    AlreadySignedIn(String),
}

impl From<SigninProblem> for NotFinished {
    fn from(problem: SigninProblem) -> Self {
        Self::Problem(problem)
    }
}

struct Flow {
    services: Arc<Services>,
    steps: watch::Sender<SigninStep>,
    answers: mpsc::UnboundedReceiver<SigninAnswer>,
}

impl Flow {
    fn show(&self, step: SigninStep) {
        self.steps.send_replace(step);
    }

    /// The code flow, from a parsed link.
    async fn code(mut self, link: LoginLink) {
        let LoginLink {
            account_provider,
            login_hint,
            code,
            dropped_code,
        } = link;
        let mut form = blank_form(PasswordKind::FamilyChat);
        form.server = account_provider.to_string();
        form.username = login_hint
            .as_ref()
            .map(|hint| hint.localpart().to_owned())
            .unwrap_or_default();

        if let Some(hint) = &login_hint
            && self.services.accounts.is_signed_in(hint)
        {
            info!("the link's account is signed in already; its code is not used");
            drop(code);
            self.show(SigninStep::AlreadySignedIn(wording::already_signed_in(
                hint,
            )));
            return;
        }

        let Some(code) = code else {
            if let Some(dropped) = dropped_code {
                info!(?dropped, "the link's code was dropped unsent");
                form.notice = Some(wording::CODE_NOT_USED.to_owned());
            }
            return self.password(form).await;
        };

        self.show(SigninStep::Confirm(ConfirmCode {
            question: match &login_hint {
                Some(hint) => wording::confirm_user(hint),
                None => wording::confirm_server(&form.server),
            },
            detail: wording::CONFIRM_DETAIL.to_owned(),
            user_id: login_hint.clone(),
            server: form.server.clone(),
        }));
        loop {
            match self.answers.recv().await {
                Some(SigninAnswer::Continue) => break,
                Some(SigninAnswer::UsePassword) => {
                    drop(code);
                    return self.password(form).await;
                }
                Some(SigninAnswer::Cancel) | None => {
                    drop(code);
                    self.show(SigninStep::Cancelled);
                    return;
                }
                Some(SigninAnswer::SubmitPassword { .. }) => {}
            }
        }

        match self
            .redeem(code, login_hint.as_ref(), &account_provider)
            .await
        {
            Ok(finished) => self.show(SigninStep::Finished(finished)),
            Err(NotFinished::AlreadySignedIn(message)) => {
                self.show(SigninStep::AlreadySignedIn(message));
            }
            Err(NotFinished::Problem(problem)) => {
                form.problem = Some(problem);
                self.password(form).await;
            }
        }
    }

    /// Redeems `code` and opens its account.
    async fn redeem(
        &self,
        code: SigninCode,
        hint: Option<&OwnedUserId>,
        account_provider: &ServerName,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        let Ok(_gate) = services.gate.clone().try_lock_owned() else {
            info!("another sign-in code is being redeemed; this one is not sent");
            return Err(SigninProblem::new(wording::CODE_BUSY).into());
        };
        let SigninCode { hs, token } = code;
        // `hs` passed the allowlist when the link was parsed; the URL built
        // from it is checked again, so the token goes only to a
        // `CheckedHomeserver`.
        let Some(homeserver) = hs
            .https_url()
            .ok()
            .and_then(|url| services.allowlist.check(&url).ok())
        else {
            return Err(SigninProblem::new(wording::CODE_NOT_USED).into());
        };
        self.show(SigninStep::Working(Working::RedeemingCode));
        let credentials = match redeem_token(
            services.transport.as_ref(),
            &homeserver,
            token,
            &services.device_name,
        )
        .await
        {
            Ok(credentials) => credentials,
            Err(failure) => {
                return Err(match failure {
                    LoginFailure::Rejected { .. } => {
                        SigninProblem::new(wording::CODE_USED_OR_EXPIRED)
                    }
                    LoginFailure::RateLimited(wait) => rate_limited(wait),
                    LoginFailure::Unreachable(_) => {
                        SigninProblem::new(wording::unreachable(&hs.to_string()))
                    }
                    LoginFailure::Redirected(_)
                    | LoginFailure::Refused { .. }
                    | LoginFailure::Unreadable => SigninProblem::new(wording::CODE_FAILED),
                }
                .into());
            }
        };

        let bound = match hint {
            Some(hint) => same_user(&credentials.user_id, hint),
            None => user_server(&credentials.user_id).as_ref() == Some(account_provider),
        };
        if !bound {
            warn!("the code signed in another account than the link names; signing it out");
            logout(services.transport.as_ref(), credentials).await;
            return Err(SigninProblem::new(wording::CODE_FOR_ANOTHER_ACCOUNT).into());
        }
        self.finish(credentials, AuthKind::LoginToken, None).await
    }

    /// The password form, until it signs in or is cancelled.
    async fn password(mut self, mut form: PasswordForm) {
        loop {
            self.show(SigninStep::Password(form.clone()));
            let (server, username, password) = loop {
                match self.answers.recv().await {
                    Some(SigninAnswer::SubmitPassword {
                        server,
                        username,
                        password,
                    }) => break (server, username, password),
                    Some(SigninAnswer::Cancel) | None => {
                        self.show(SigninStep::Cancelled);
                        return;
                    }
                    Some(SigninAnswer::Continue | SigninAnswer::UsePassword) => {}
                }
            };
            form.server = server.trim().to_owned();
            form.username = username.trim().to_owned();
            let password = Secret::new(password.expose().to_owned());
            match self
                .password_sign_in(form.kind, &form.server, &form.username, &password)
                .await
            {
                Ok(finished) => {
                    self.show(SigninStep::Finished(finished));
                    return;
                }
                Err(NotFinished::AlreadySignedIn(message)) => {
                    self.show(SigninStep::AlreadySignedIn(message));
                    return;
                }
                Err(NotFinished::Problem(problem)) => form.problem = Some(problem),
            }
        }
    }

    /// One password sign-in attempt.
    async fn password_sign_in(
        &self,
        kind: PasswordKind,
        server: &str,
        username: &str,
        password: &Secret<String>,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        if username.is_empty() {
            return Err(SigninProblem::new(wording::NO_USERNAME).into());
        }
        if password.expose().is_empty() {
            return Err(SigninProblem::new(wording::NO_PASSWORD).into());
        }
        let (server_name, user) = server_and_user(server, username)?;

        self.show(SigninStep::Working(Working::CheckingServer));
        let allowlist = (kind == PasswordKind::FamilyChat).then_some(&services.allowlist);
        let homeserver = discover(services.transport.as_ref(), &server_name, allowlist)
            .await
            .map_err(|error| {
                info!(?error, "discovery did not find a homeserver to sign in to");
                SigninProblem::new(match error {
                    DiscoveryError::NotFamilyChat => wording::NOT_FAMILY_CHAT.to_owned(),
                    DiscoveryError::NotHttps => wording::NOT_HTTPS.to_owned(),
                    DiscoveryError::Unreachable(_) => {
                        wording::unreachable(&server_name.to_string())
                    }
                    DiscoveryError::BadBaseUrl | DiscoveryError::NotAMatrixServer => {
                        wording::not_a_matrix_server(&server_name.to_string())
                    }
                })
            })?;

        self.show(SigninStep::Working(Working::SigningIn));
        let credentials = login_password(
            services.transport.as_ref(),
            &homeserver,
            &user,
            password,
            &services.device_name,
        )
        .await
        .map_err(|failure| match failure {
            LoginFailure::Rejected { errcode, .. }
                if errcode.as_deref() == Some("M_USER_DEACTIVATED") =>
            {
                SigninProblem::new(wording::DEACTIVATED)
            }
            LoginFailure::Rejected { .. } => SigninProblem::new(wording::WRONG_PASSWORD),
            LoginFailure::RateLimited(wait) => rate_limited(wait),
            LoginFailure::Unreachable(_) => {
                SigninProblem::new(wording::unreachable(&server_name.to_string()))
            }
            LoginFailure::Redirected(_)
            | LoginFailure::Refused { .. }
            | LoginFailure::Unreadable => SigninProblem::new(wording::SIGNIN_FAILED),
        })?;
        // The password goes with the credentials once, for the server's
        // user-interactive auth when the account's cross-signing identity
        // is made (ADR 0009); the session wipes it after.
        let password = Secret::new(password.expose().clone());
        self.finish(credentials, AuthKind::Password, Some(password))
            .await
    }

    /// Opens the account `credentials` signed in, or signs its device out.
    async fn finish(
        &self,
        credentials: Credentials,
        auth_kind: AuthKind,
        password: Option<Secret<String>>,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        if services.accounts.is_signed_in(&credentials.user_id) {
            info!("the account is signed in already; signing the new device out");
            let message = wording::already_signed_in(&credentials.user_id);
            logout(services.transport.as_ref(), credentials).await;
            return Err(NotFinished::AlreadySignedIn(message));
        }
        self.show(SigninStep::Working(Working::OpeningAccount));
        let user_id = credentials.user_id.clone();
        let check: Box<dyn PostLoginCheck> = if credentials.homeserver.is_family_chat() {
            Box::new(FamilyChatCheck::new(
                services.allowlist.clone(),
                user_id.clone(),
            ))
        } else {
            Box::new(OtherServerCheck::new(user_id.clone()))
        };
        let session = AccountSession {
            user_id: user_id.to_string(),
            device_id: credentials.device_id.clone(),
            homeserver_url: credentials.homeserver.as_account_url(),
            access_token: credentials.access_token.clone(),
            refresh_token: credentials.refresh_token.clone(),
            auth_kind,
        };
        match services.accounts.add(session, check, password).await {
            Ok(account) => {
                info!("the sign-in finished");
                Ok(Finished { account, user_id })
            }
            Err(AddFailure::Refused(refusal)) => {
                warn!("the post-login check refused the sign-in");
                Err(SigninProblem::new(refusal).into())
            }
            Err(AddFailure::NotSupported) => {
                logout(services.transport.as_ref(), credentials).await;
                Err(SigninProblem::new(wording::NOT_SUPPORTED).into())
            }
            Err(AddFailure::Failed(error)) => {
                warn!(%error, "the new account could not be opened; signing the device out");
                logout(services.transport.as_ref(), credentials).await;
                Err(SigninProblem::new(wording::COULD_NOT_SAVE).into())
            }
        }
    }
}

fn rate_limited(wait: Option<std::time::Duration>) -> SigninProblem {
    SigninProblem {
        message: wording::rate_limited(wait),
        retry_after: wait,
    }
}

/// The server to discover and the user to sign in as, from the form: a
/// full Matrix ID names its own server (and must agree with a typed one);
/// a username needs the server field. What was typed is trimmed, and one
/// trailing `/` after a typed address is forgiven; the name itself is then
/// parsed strictly.
fn server_and_user(server: &str, username: &str) -> Result<(ServerName, String), SigninProblem> {
    let typed = server.trim();
    let typed = typed.strip_suffix('/').unwrap_or(typed);
    let typed = (!typed.is_empty())
        .then(|| ServerName::parse(typed))
        .transpose()
        .map_err(|_| SigninProblem::new(wording::INVALID_SERVER))?;
    if username.starts_with('@') {
        let user_id =
            parse_user_id(username).ok_or_else(|| SigninProblem::new(wording::INVALID_USERNAME))?;
        let server_name =
            user_server(&user_id).ok_or_else(|| SigninProblem::new(wording::INVALID_USERNAME))?;
        if typed.as_ref().is_some_and(|typed| *typed != server_name) {
            return Err(SigninProblem::new(wording::SERVER_MISMATCH));
        }
        return Ok((server_name, user_id.to_string()));
    }
    if username.contains(':') || username.chars().any(char::is_whitespace) {
        return Err(SigninProblem::new(wording::INVALID_USERNAME));
    }
    let server_name = typed.ok_or_else(|| SigninProblem::new(wording::INVALID_SERVER))?;
    Ok((server_name, username.to_owned()))
}
