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
//!
//! Phase 14 (ADR 0010): "Use another Matrix server" asks for the server
//! first, finds it (any homeserver under the allowlist is a Family Chat
//! one, with its rules) and shows only what it offers ([`probe`]): the
//! password, "Continue in your browser" (OAuth 2.0 or single sign-on, after
//! a confirmation that shows the full address) and "Sign in with a QR
//! code". A Family Chat password sign-in the server refuses as an unknown
//! login type offers the same. [`SigninStart::LinkDevice`] shows a signed-in
//! account's QR code for a new device and has its sign-in approved in the
//! browser. Each of them shares the one-at-a-time gate with the code.

use std::{pin::pin, sync::Arc, time::Duration};

use familychat_core::{
    OwnedUserId,
    session::AccountKey,
    signin::{
        BrowserKind, BrowserPurpose, BrowserStep, BrowserUrl, CheckCodeForm, ConfirmBrowser,
        ConfirmCode, Finished, PasswordForm, PasswordKind, QrCodeStep, QrPurpose, ServerForm,
        SigninAnswer, SigninProblem, SigninStart, SigninStep, Working,
    },
};
use familychat_security::{AccountSession, AuthKind, Secret};
use familychat_session::{QrAnswer, QrEvent, QrLoginError, QrLoginRequest, UrlGuard};
use reqwest::Url;
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc, watch};
use tracing::{info, warn};

use crate::{
    allowlist::{Allowlist, CheckedHomeserver},
    discovery::{DiscoveryError, discover},
    handoff::{AccountSink, AddFailure, check_for},
    host::ServerName,
    http::Transport,
    link::{LoginLink, SigninCode, parse_user_id, same_user, user_server},
    login::{Credentials, LoginFailure, login_password, logout, redeem_token},
    loopback::{Callback, Loopback},
    methods::{ServerOffer, probe},
    oauth::{Authorization, client_metadata, display_address, register_client},
    wording,
};

/// How long a browser sign-in may take before the flow stops waiting.
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
        SigninStart::OtherServer => flow.other_server().await,
        SigninStart::LinkDevice(account) => flow.link_device(account).await,
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
        methods: None,
    }
}

/// How a sign-in attempt that did not finish ended.
enum NotFinished {
    /// Back to the form with this problem.
    Problem(SigninProblem),
    /// The account was signed in already (final).
    AlreadySignedIn(String),
    /// Back to the form as it was (a browser or QR sign-in the person
    /// cancelled).
    Back,
    /// The frontend went away (final, as cancelled).
    Gone,
    /// The server took no password but offers these (ADR 0010).
    OfferMethods(Box<(CheckedHomeserver, ServerOffer, SigninProblem)>),
}

/// Where the methods form went.
enum MethodsOutcome {
    /// The flow ended (its final step is showing).
    Done,
    /// Back to the server step.
    ChangeServer,
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
                Some(_) => {}
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
            Err(NotFinished::OfferMethods(offered)) => {
                form.problem = Some(offered.2);
                self.password(form).await;
            }
            Err(NotFinished::Back) => self.password(form).await,
            Err(NotFinished::Gone) => self.show(SigninStep::Cancelled),
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
                    Some(_) => {}
                }
            };
            form.server = server.trim().to_owned();
            form.username = username.trim().to_owned();
            let password = Secret::new(password.expose().to_owned());
            match self
                .password_sign_in(form.kind, &form.server, &form.username, &password, None)
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
                Err(NotFinished::OfferMethods(offered)) => {
                    let (homeserver, offer, problem) = *offered;
                    form.methods = Some(offer.methods.clone());
                    form.problem = Some(problem);
                    self.methods(form, homeserver, offer).await;
                    return;
                }
                Err(NotFinished::Back) => form.problem = None,
                Err(NotFinished::Gone) => {
                    self.show(SigninStep::Cancelled);
                    return;
                }
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
        known: Option<&CheckedHomeserver>,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        if username.is_empty() {
            return Err(SigninProblem::new(wording::NO_USERNAME).into());
        }
        if password.expose().is_empty() {
            return Err(SigninProblem::new(wording::NO_PASSWORD).into());
        }
        let (server_name, user) = server_and_user(server, username)?;

        let homeserver = match known {
            Some(homeserver) => homeserver.clone(),
            None => {
                self.show(SigninStep::Working(Working::CheckingServer));
                self.find_homeserver(kind, &server_name).await?
            }
        };

        self.show(SigninStep::Working(Working::SigningIn));
        let credentials = login_password(
            services.transport.as_ref(),
            &homeserver,
            &user,
            password,
            &services.device_name,
        )
        .await;
        let credentials = match credentials {
            Ok(credentials) => Ok(credentials),
            // A server that does not know the login type (an OAuth-only
            // server, MSC3861) may offer the browser or a QR code instead.
            Err(LoginFailure::Refused {
                status: 400 | 404,
                errcode,
            }) if known.is_none()
                && matches!(errcode.as_deref(), Some("M_UNRECOGNIZED" | "M_UNKNOWN")) =>
            {
                let offer = probe(
                    services.transport.as_ref(),
                    &homeserver,
                    &services.allowlist,
                )
                .await;
                if offer.methods.browser.is_some() || offer.methods.qr_code {
                    info!("the server took no password but offers another way to sign in");
                    return Err(NotFinished::OfferMethods(Box::new((
                        homeserver,
                        offer,
                        SigninProblem::new(wording::USE_BROWSER_INSTEAD),
                    ))));
                }
                Err(LoginFailure::Refused {
                    status: 400,
                    errcode,
                })
            }
            Err(failure) => Err(failure),
        };
        let credentials = credentials.map_err(|failure| match failure {
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
        let check = check_for(
            &credentials.homeserver,
            &services.allowlist,
            Some(user_id.clone()),
        );
        let session = AccountSession {
            user_id: user_id.to_string(),
            device_id: credentials.device_id.clone(),
            homeserver_url: credentials.homeserver.as_account_url(),
            access_token: credentials.access_token.clone(),
            refresh_token: credentials.refresh_token.clone(),
            auth_kind,
            oauth_client_id: credentials
                .oauth
                .as_ref()
                .map(|grant| grant.client_id.clone()),
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

// ---------------------------------------------------------------------------
// Phase 14: the server first, the browser and QR codes (ADR 0010)
// ---------------------------------------------------------------------------

impl Flow {
    /// Resolves `name` for a sign-in of `kind`: a Family Chat password
    /// sign-in only under the allowlist; another server's under the
    /// allowlist's rules when it is a Family Chat homeserver, `https://`
    /// otherwise.
    async fn find_homeserver(
        &self,
        kind: PasswordKind,
        name: &ServerName,
    ) -> Result<CheckedHomeserver, NotFinished> {
        let services = self.services.clone();
        let allowlist = (kind == PasswordKind::FamilyChat).then_some(&services.allowlist);
        let homeserver = discover(services.transport.as_ref(), name, allowlist)
            .await
            .map_err(|error| {
                info!(?error, "discovery did not find a homeserver to sign in to");
                SigninProblem::new(match error {
                    DiscoveryError::NotFamilyChat => wording::NOT_FAMILY_CHAT.to_owned(),
                    DiscoveryError::NotHttps => wording::NOT_HTTPS.to_owned(),
                    DiscoveryError::Unreachable(_) => wording::unreachable(&name.to_string()),
                    DiscoveryError::BadBaseUrl | DiscoveryError::NotAMatrixServer => {
                        wording::not_a_matrix_server(&name.to_string())
                    }
                })
            })?;
        // Any homeserver under the allowlist is a Family Chat one, whichever
        // way in: its sign-ins get the Family Chat rules.
        Ok(services
            .allowlist
            .check(homeserver.url())
            .unwrap_or(homeserver))
    }

    /// "Use another Matrix server": the server step, then what it offers.
    async fn other_server(mut self) {
        let mut form = ServerForm {
            server: String::new(),
            server_placeholder: wording::OTHER_SERVER_PLACEHOLDER.to_owned(),
            problem: None,
        };
        loop {
            self.show(SigninStep::Server(form.clone()));
            let server = loop {
                match self.answers.recv().await {
                    Some(SigninAnswer::ChooseServer { server }) => break server,
                    Some(SigninAnswer::Cancel) | None => {
                        self.show(SigninStep::Cancelled);
                        return;
                    }
                    Some(_) => {}
                }
            };
            form.server = server.trim().to_owned();
            let typed = form.server.strip_suffix('/').unwrap_or(&form.server);
            let Ok(name) = ServerName::parse(typed) else {
                form.problem = Some(SigninProblem::new(wording::INVALID_SERVER));
                continue;
            };
            self.show(SigninStep::Working(Working::CheckingServer));
            let homeserver = match self.find_homeserver(PasswordKind::OtherServer, &name).await {
                Ok(homeserver) => homeserver,
                Err(NotFinished::Problem(problem)) => {
                    form.problem = Some(problem);
                    continue;
                }
                Err(_) => {
                    form.problem = Some(SigninProblem::new(wording::SIGNIN_FAILED));
                    continue;
                }
            };
            let services = self.services.clone();
            let offer = probe(
                services.transport.as_ref(),
                &homeserver,
                &services.allowlist,
            )
            .await;
            if !offer.methods.any() {
                form.problem = Some(SigninProblem::new(wording::NO_METHODS));
                continue;
            }
            let methods_form = PasswordForm {
                methods: Some(offer.methods.clone()),
                server: form.server.clone(),
                ..blank_form(PasswordKind::OtherServer)
            };
            match self.methods(methods_form, homeserver, offer).await {
                MethodsOutcome::Done => return,
                MethodsOutcome::ChangeServer => form.problem = None,
            }
        }
    }

    /// The form with the server's methods, until it signs in, is
    /// cancelled, or (another server's) goes back to the server step.
    async fn methods(
        &mut self,
        mut form: PasswordForm,
        homeserver: CheckedHomeserver,
        offer: ServerOffer,
    ) -> MethodsOutcome {
        loop {
            self.show(SigninStep::Password(form.clone()));
            let attempt = match self.answers.recv().await {
                Some(SigninAnswer::SubmitPassword {
                    server,
                    username,
                    password,
                }) if offer.methods.password => {
                    if !form.server_is_fixed() {
                        form.server = server.trim().to_owned();
                    }
                    form.username = username.trim().to_owned();
                    let password = Secret::new(password.expose().to_owned());
                    let known = form.server_is_fixed().then_some(&homeserver);
                    self.password_sign_in(form.kind, &form.server, &form.username, &password, known)
                        .await
                }
                Some(SigninAnswer::UseBrowser) if offer.methods.browser.is_some() => {
                    self.browser_sign_in(&homeserver, &offer).await
                }
                Some(SigninAnswer::UseQrCode) if offer.methods.qr_code => {
                    self.qr_sign_in(&homeserver, &form.server).await
                }
                Some(SigninAnswer::ChangeServer) if form.kind == PasswordKind::OtherServer => {
                    return MethodsOutcome::ChangeServer;
                }
                Some(SigninAnswer::Cancel) | None => Err(NotFinished::Gone),
                Some(_) => continue,
            };
            match attempt {
                Ok(finished) => {
                    self.show(SigninStep::Finished(finished));
                    return MethodsOutcome::Done;
                }
                Err(NotFinished::AlreadySignedIn(message)) => {
                    self.show(SigninStep::AlreadySignedIn(message));
                    return MethodsOutcome::Done;
                }
                Err(NotFinished::Problem(problem)) => form.problem = Some(problem),
                Err(NotFinished::OfferMethods(offered)) => form.problem = Some(offered.2),
                Err(NotFinished::Back) => form.problem = None,
                Err(NotFinished::Gone) => {
                    self.show(SigninStep::Cancelled);
                    return MethodsOutcome::Done;
                }
            }
        }
    }

    /// The one-at-a-time gate every sign-in that sends a credential takes.
    fn gate(&self) -> Result<OwnedMutexGuard<()>, NotFinished> {
        self.services.gate.clone().try_lock_owned().map_err(|_| {
            info!("another sign-in is running; this one waits");
            NotFinished::Problem(SigninProblem::new(wording::SIGNIN_BUSY))
        })
    }

    /// Asks before a browser opens; `Ok` on Continue.
    async fn confirm_browser(
        &mut self,
        address: &str,
        purpose: BrowserPurpose,
    ) -> Result<(), NotFinished> {
        let (question, detail) = match purpose {
            BrowserPurpose::SignIn => (wording::CONFIRM_BROWSER, wording::CONFIRM_BROWSER_DETAIL),
            BrowserPurpose::ApproveDevice => {
                (wording::CONFIRM_APPROVE, wording::CONFIRM_APPROVE_DETAIL)
            }
        };
        self.show(SigninStep::ConfirmBrowser(ConfirmBrowser {
            question: question.to_owned(),
            address: address.to_owned(),
            detail: detail.to_owned(),
            purpose,
        }));
        loop {
            match self.answers.recv().await {
                Some(SigninAnswer::Continue) => return Ok(()),
                Some(SigninAnswer::Cancel) => return Err(NotFinished::Back),
                None => return Err(NotFinished::Gone),
                Some(_) => {}
            }
        }
    }

    /// Waits until the person cancels (or the frontend goes away).
    async fn until_cancelled(answers: &mut mpsc::UnboundedReceiver<SigninAnswer>) -> NotFinished {
        loop {
            match answers.recv().await {
                Some(SigninAnswer::Cancel) => return NotFinished::Back,
                None => return NotFinished::Gone,
                Some(_) => {}
            }
        }
    }

    /// Shows the browser step and waits for the browser to come back to
    /// `loopback` with `state`, for the person to cancel, or for the
    /// timeout.
    async fn wait_for_browser(
        &mut self,
        loopback: &Loopback,
        state: &Secret<String>,
        address: &str,
        url: &Url,
    ) -> Result<Callback, NotFinished> {
        self.show(SigninStep::Browser(BrowserStep {
            address: address.to_owned(),
            url: BrowserUrl::new(url.to_string()),
            message: wording::BROWSER_WAITING.to_owned(),
            purpose: BrowserPurpose::SignIn,
        }));
        tokio::select! {
            callback = loopback.wait(state) => callback.map_err(|error| {
                warn!(%error, "the loopback redirect stopped");
                SigninProblem::new(wording::BROWSER_FAILED).into()
            }),
            stopped = Self::until_cancelled(&mut self.answers) => Err(stopped),
            () = tokio::time::sleep(BROWSER_TIMEOUT) => {
                info!("the browser sign-in timed out");
                Err(SigninProblem::new(wording::BROWSER_TIMED_OUT).into())
            }
        }
    }

    /// "Continue in your browser": OAuth 2.0 when the server has an
    /// authorization server, single sign-on otherwise.
    async fn browser_sign_in(
        &mut self,
        homeserver: &CheckedHomeserver,
        offer: &ServerOffer,
    ) -> Result<Finished, NotFinished> {
        let Some(browser) = offer.methods.browser.clone() else {
            return Err(NotFinished::Back);
        };
        self.confirm_browser(&browser.address, BrowserPurpose::SignIn)
            .await?;
        let _gate = self.gate()?;
        self.show(SigninStep::Working(Working::PreparingBrowser));
        let loopback = Loopback::bind().await.map_err(|error| {
            warn!(%error, "could not listen for the browser's redirect");
            NotFinished::from(SigninProblem::new(wording::BROWSER_FAILED))
        })?;
        match (browser.kind, &offer.auth) {
            (BrowserKind::OAuth, Some(auth)) => {
                self.oauth_sign_in(homeserver, auth, &loopback, &browser.address)
                    .await
            }
            (BrowserKind::Sso, _) => {
                self.sso_sign_in(homeserver, &loopback, &browser.address)
                    .await
            }
            (BrowserKind::OAuth, None) => Err(NotFinished::Back),
        }
    }

    async fn oauth_sign_in(
        &mut self,
        homeserver: &CheckedHomeserver,
        auth: &crate::oauth::AuthServer,
        loopback: &Loopback,
        address: &str,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        let failed = |_| NotFinished::from(SigninProblem::new(wording::BROWSER_FAILED));
        let redirect_uri = loopback.redirect_uri();
        let client_id = register_client(
            services.transport.as_ref(),
            auth,
            &client_metadata(Some(&redirect_uri)),
        )
        .await
        .map_err(failed)?;
        let authorization = Authorization::new(auth, client_id, redirect_uri).map_err(failed)?;
        let callback = self
            .wait_for_browser(loopback, &authorization.state, address, &authorization.url)
            .await?;
        if let Some(error) = callback.get("error") {
            info!(error = %error.expose(), "the authorization server answered with an error");
            return Err(SigninProblem::new(if error.expose() == "access_denied" {
                wording::BROWSER_DECLINED
            } else {
                wording::BROWSER_FAILED
            })
            .into());
        }
        let Some(code) = callback.get("code").cloned() else {
            return Err(SigninProblem::new(wording::BROWSER_FAILED).into());
        };
        drop(callback);
        self.show(SigninStep::Working(Working::SigningIn));
        let credentials = authorization
            .exchange(services.transport.as_ref(), auth, homeserver, code)
            .await
            .map_err(|failure| {
                info!(?failure, "the browser sign-in's code was not exchanged");
                NotFinished::from(SigninProblem::new(wording::BROWSER_FAILED))
            })?;
        self.finish(credentials, AuthKind::OAuth, None).await
    }

    async fn sso_sign_in(
        &mut self,
        homeserver: &CheckedHomeserver,
        loopback: &Loopback,
        address: &str,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        // The server adds `loginToken` to the redirect and keeps what is
        // there, so the redirect carries this sign-in's state too.
        let state = crate::oauth::new_state()
            .map_err(|_| NotFinished::from(SigninProblem::new(wording::BROWSER_FAILED)))?;
        let mut redirect = loopback.redirect_uri();
        redirect
            .query_pairs_mut()
            .append_pair("state", state.expose());
        let mut url = homeserver.endpoint("_matrix/client/v3/login/sso/redirect");
        url.query_pairs_mut()
            .append_pair("redirectUrl", redirect.as_str());
        let callback = self
            .wait_for_browser(loopback, &state, address, &url)
            .await?;
        let Some(token) = callback.get("loginToken").cloned() else {
            return Err(SigninProblem::new(wording::BROWSER_FAILED).into());
        };
        drop(callback);
        self.show(SigninStep::Working(Working::SigningIn));
        let credentials = redeem_token(
            services.transport.as_ref(),
            homeserver,
            token,
            &services.device_name,
        )
        .await
        .map_err(|failure| match failure {
            LoginFailure::RateLimited(wait) => rate_limited(wait).into(),
            failure => {
                info!(?failure, "the single sign-on token was not accepted");
                NotFinished::from(SigninProblem::new(wording::BROWSER_FAILED))
            }
        })?;
        self.finish(credentials, AuthKind::Sso, None).await
    }

    /// What every address a QR sign-in uses is checked against: the
    /// homeserver's own rule (ADR 0010).
    fn guard(&self, homeserver: &CheckedHomeserver) -> UrlGuard {
        let homeserver = homeserver.clone();
        let allowlist = self.services.allowlist.clone();
        Arc::new(move |url: &Url| homeserver.check_related(url, &allowlist).is_ok())
    }

    /// "Sign in with a QR code": this computer shows the code, a signed-in
    /// device scans it.
    async fn qr_sign_in(
        &mut self,
        homeserver: &CheckedHomeserver,
        server: &str,
    ) -> Result<Finished, NotFinished> {
        let services = self.services.clone();
        let _gate = self.gate()?;
        self.show(SigninStep::Working(Working::PreparingQrCode));
        let (events_sender, mut events) = mpsc::channel(8);
        let (qr_answers, qr_answers_receiver) = mpsc::unbounded_channel();
        let request = QrLoginRequest {
            homeserver: homeserver.url().clone(),
            client_metadata: client_metadata(None),
            guard: self.guard(homeserver),
        };
        let check = check_for(homeserver, &services.allowlist, None);
        let mut login = pin!(services.accounts.add_with_qr_code(
            request,
            check,
            events_sender,
            qr_answers_receiver
        ));
        let purpose = QrPurpose::SignIn;
        let mut gone = false;
        let result = loop {
            tokio::select! {
                result = &mut login => break result,
                Some(event) = events.recv() => match event {
                    QrEvent::Ready { qr, .. } => self.show(SigninStep::QrCode(QrCodeStep {
                        qr,
                        purpose,
                        server: server.to_owned(),
                    })),
                    QrEvent::Scanned => self.show(SigninStep::CheckCode(CheckCodeForm {
                        purpose,
                        problem: None,
                    })),
                    QrEvent::WaitingForApproval { .. } => {
                        self.show(SigninStep::Working(Working::WaitingForOtherDevice));
                    }
                    QrEvent::SyncingSecrets => {
                        self.show(SigninStep::Working(Working::OpeningAccount));
                    }
                    QrEvent::ApproveInBrowser { .. } => {}
                },
                answer = self.answers.recv() => match answer {
                    Some(SigninAnswer::SubmitCheckCode(text)) => match check_code(&text) {
                        Some(code) => {
                            let _ = qr_answers.send(QrAnswer::CheckCode(code));
                            self.show(SigninStep::Working(Working::WaitingForOtherDevice));
                        }
                        None => self.show(SigninStep::CheckCode(CheckCodeForm {
                            purpose,
                            problem: Some(wording::CHECK_CODE_INVALID.to_owned()),
                        })),
                    },
                    Some(SigninAnswer::Cancel) => {
                        let _ = qr_answers.send(QrAnswer::Cancel);
                    }
                    None => {
                        gone = true;
                        let _ = qr_answers.send(QrAnswer::Cancel);
                    }
                    Some(_) => {}
                },
            }
        };
        match result {
            Ok((account, user_id)) => {
                info!("the QR code sign-in finished");
                Ok(Finished { account, user_id })
            }
            Err(_) if gone => Err(NotFinished::Gone),
            Err(QrLoginError::Cancelled) => Err(NotFinished::Back),
            Err(QrLoginError::AlreadySignedIn(user_id)) => Err(NotFinished::AlreadySignedIn(
                wording::already_signed_in(&user_id),
            )),
            Err(error) => {
                info!(%error, "the QR code sign-in stopped");
                Err(SigninProblem::new(match error {
                    QrLoginError::Expired => wording::QR_EXPIRED.to_owned(),
                    QrLoginError::CheckCodeMismatch => wording::CHECK_CODE_MISMATCH.to_owned(),
                    QrLoginError::Declined => wording::QR_DECLINED.to_owned(),
                    QrLoginError::NotSupported => wording::QR_NOT_SUPPORTED.to_owned(),
                    QrLoginError::Refused(refusal) => refusal,
                    _ => wording::QR_FAILED.to_owned(),
                })
                .into())
            }
        }
    }

    /// "Sign in a new device": `account`'s QR code for a new device to
    /// scan, its check code, its sign-in approved in the browser at the
    /// account's authorization server, then the keys sent to it.
    async fn link_device(mut self, account: AccountKey) {
        let step = match self.link(&account).await {
            Ok(()) => SigninStep::DeviceLinked(wording::LINKED.to_owned()),
            Err(LinkStop::Cancelled) => SigninStep::Cancelled,
            Err(LinkStop::Stopped(message)) => SigninStep::Stopped(message),
        };
        self.show(step);
    }

    async fn link(&mut self, account: &AccountKey) -> Result<(), LinkStop> {
        let services = self.services.clone();
        let stopped = |message: &str| LinkStop::Stopped(message.to_owned());
        let Some(url) = services.accounts.homeserver_of(account) else {
            return Err(stopped(wording::LINK_FAILED));
        };
        let homeserver = services
            .allowlist
            .check(&url)
            .or_else(|_| CheckedHomeserver::other_server(&url))
            .map_err(|_| stopped(wording::LINK_NOT_SUPPORTED))?;
        let _gate = self.gate().map_err(|_| stopped(wording::SIGNIN_BUSY))?;
        self.show(SigninStep::Working(Working::PreparingQrCode));
        let offer = probe(
            services.transport.as_ref(),
            &homeserver,
            &services.allowlist,
        )
        .await;
        let Some(auth) = offer.auth.filter(|_| offer.methods.qr_code) else {
            return Err(stopped(wording::LINK_NOT_SUPPORTED));
        };
        let (events_sender, mut events) = mpsc::channel(8);
        let (qr_answers, qr_answers_receiver) = mpsc::unbounded_channel();
        let mut grant = pin!(services.accounts.link_device(
            account,
            self.guard(&homeserver),
            events_sender,
            qr_answers_receiver
        ));
        let purpose = QrPurpose::LinkDevice;
        let server = display_address(homeserver.url());
        // The approval address, once the new device sent it and it passed
        // the check: opened only after the person's Continue.
        let mut approving: Option<Url> = None;
        let mut cancelled = false;
        let mut wrong_address = false;
        let result = loop {
            tokio::select! {
                result = &mut grant => break result,
                Some(event) = events.recv() => match event {
                    QrEvent::Ready { qr, .. } => self.show(SigninStep::QrCode(QrCodeStep {
                        qr,
                        purpose,
                        server: server.clone(),
                    })),
                    QrEvent::Scanned => self.show(SigninStep::CheckCode(CheckCodeForm {
                        purpose,
                        problem: None,
                    })),
                    QrEvent::ApproveInBrowser { verification_uri } => {
                        // The new device sends this address; it must be the
                        // account's own authorization server, or the browser
                        // could open wherever a stranger's device says.
                        match approval_address(verification_uri.expose(), &auth, &homeserver, &services.allowlist) {
                            Some(url) => {
                                approving = Some(url);
                                self.show(SigninStep::ConfirmBrowser(ConfirmBrowser {
                                    question: wording::CONFIRM_APPROVE.to_owned(),
                                    address: auth.address(),
                                    detail: wording::CONFIRM_APPROVE_DETAIL.to_owned(),
                                    purpose: BrowserPurpose::ApproveDevice,
                                }));
                            }
                            None => {
                                warn!("the new device's approval address is not the account's authorization server");
                                wrong_address = true;
                                let _ = qr_answers.send(QrAnswer::Cancel);
                            }
                        }
                    }
                    QrEvent::SyncingSecrets => self.show(SigninStep::Working(Working::SharingKeys)),
                    QrEvent::WaitingForApproval { .. } => {}
                },
                answer = self.answers.recv() => match answer {
                    Some(SigninAnswer::SubmitCheckCode(text)) => match check_code(&text) {
                        Some(code) => {
                            let _ = qr_answers.send(QrAnswer::CheckCode(code));
                            self.show(SigninStep::Working(Working::WaitingForOtherDevice));
                        }
                        None => self.show(SigninStep::CheckCode(CheckCodeForm {
                            purpose,
                            problem: Some(wording::CHECK_CODE_INVALID.to_owned()),
                        })),
                    },
                    Some(SigninAnswer::Continue) => {
                        if let Some(url) = approving.take() {
                            self.show(SigninStep::Browser(BrowserStep {
                                address: auth.address(),
                                url: BrowserUrl::new(url.to_string()),
                                message: wording::APPROVE_WAITING.to_owned(),
                                purpose: BrowserPurpose::ApproveDevice,
                            }));
                            let _ = qr_answers.send(QrAnswer::Continue);
                        }
                    }
                    Some(SigninAnswer::Cancel) | None => {
                        cancelled = true;
                        let _ = qr_answers.send(QrAnswer::Cancel);
                    }
                    Some(_) => {}
                },
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(_) if wrong_address => Err(stopped(wording::LINK_WRONG_ADDRESS)),
            Err(_) if cancelled => Err(LinkStop::Cancelled),
            Err(error) => {
                info!(%error, "signing a new device in stopped");
                Err(stopped(match error {
                    QrLoginError::Expired => wording::QR_EXPIRED,
                    QrLoginError::CheckCodeMismatch => wording::CHECK_CODE_MISMATCH,
                    QrLoginError::Declined | QrLoginError::Cancelled => wording::LINK_DECLINED,
                    QrLoginError::NoSecrets => wording::LINK_NO_KEYS,
                    QrLoginError::NotSupported => wording::LINK_NOT_SUPPORTED,
                    QrLoginError::Refused(_) => wording::LINK_NOT_SUPPORTED,
                    _ => wording::LINK_FAILED,
                }))
            }
        }
    }
}

/// How signing a new device in ended, when it did not.
enum LinkStop {
    Cancelled,
    Stopped(String),
}

/// A check code as typed: two digits (spaces around them forgiven).
fn check_code(text: &str) -> Option<u8> {
    let text = text.trim();
    (text.len() == 2 && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The approval address a new device sent, if it is on the account's
/// authorization server (the issuer's or the device endpoint's origin) and
/// passes the homeserver's rule.
fn approval_address(
    text: &str,
    auth: &crate::oauth::AuthServer,
    homeserver: &CheckedHomeserver,
    allowlist: &Allowlist,
) -> Option<Url> {
    let url = Url::parse(text).ok()?;
    let url = homeserver.check_related(&url, allowlist).ok()?;
    let origins = [
        Some(&auth.issuer),
        auth.device_authorization_endpoint.as_ref(),
    ];
    origins
        .into_iter()
        .flatten()
        .any(|origin| origin.origin() == url.origin())
        .then_some(url)
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
