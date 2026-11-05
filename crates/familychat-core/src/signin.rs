//! Sign-in as both frontends see it (PLAN.md, sections 5.3 and 6.2): the
//! step to draw, the answers a person can give, and [`SigninApi`], which
//! starts a flow.
//!
//! The rules and the wording live in `familychat-signin`, which drives each
//! flow on the background runtime and implements [`SigninApi`]; a
//! frontend only draws the latest [`SigninStep`] and sends
//! [`SigninAnswer`]s, so the window and the terminal UI cannot sign in
//! differently. Steps arrive on a [`watch`] receiver (the latest step and
//! a signal when it changes), answers go back on an unbounded [`mpsc`]
//! sender; neither needs a Tokio runtime on the frontend's side.
//!
//! Secrets in transit (the link, which may carry a sign-in code, and the
//! password) are [`SecretText`]: `Debug` prints `[redacted]` and the text
//! is wiped when dropped. No step ever carries a token or a password; the
//! address a browser sign-in opens is a [`BrowserUrl`], whose `Debug` hides
//! it too.
//!
//! Phase 14 (ADR 0010) adds the ways a server may offer beyond a password:
//! a sign-in in the browser (OAuth 2.0, MSC3861, or `m.login.sso`) and a
//! QR code a signed-in phone scans (MSC4108), both shown only where the
//! server has them ([`ServerMethods`]), and [`SigninStart::LinkDevice`],
//! which shows a QR code from a signed-in account for a new phone.

use std::{fmt, sync::Arc, time::Duration};

use ruma::OwnedUserId;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroize;

use crate::{encryption::QrModules, session::AccountKey};

/// Text that is a secret: a sign-in link or a password. `Debug` prints
/// `[redacted]`; the text is wiped when dropped.
pub struct SecretText(String);

impl SecretText {
    /// Wraps `text`.
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The text. Keep the borrow short and never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretText {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for SecretText {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

impl Drop for SecretText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// How a sign-in starts (PLAN.md, section 6.2).
#[derive(Debug)]
pub enum SigninStart {
    /// "Sign in with a code from your parent": a pasted sign-in link (or
    /// text around one), or a `familychat://` link.
    Link(SecretText),
    /// "Sign in with your password", to a Family Chat server.
    FamilyChatPassword,
    /// "Use another Matrix server": the server first, then the ways it
    /// offers (a password, the browser, a QR code).
    OtherServer,
    /// "Sign in a new device" from a signed-in account: a QR code for a
    /// phone to scan, then its sign-in approved in the browser (MSC4108).
    LinkDevice(AccountKey),
}

/// Which password form is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordKind {
    /// A Family Chat family: the server must resolve under the allowlist.
    FamilyChat,
    /// Any Matrix server over HTTPS.
    OtherServer,
}

/// A problem to show on a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigninProblem {
    /// The sentence.
    pub message: String,
    /// For a rate limit, how long the server asked to wait. The sign-in
    /// never retries on its own; a frontend may count this down.
    pub retry_after: Option<Duration>,
}

impl SigninProblem {
    /// A problem with no wait.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
        }
    }
}

/// The confirmation a code always asks first (the contract's rule 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmCode {
    /// "Sign in as `<mxid>`?", or "Sign in to `<server>`?" without a hint.
    pub question: String,
    /// The sentence under it.
    pub detail: String,
    /// The account the link names, if it names one.
    pub user_id: Option<OwnedUserId>,
    /// The family's server name (`account_provider`).
    pub server: String,
}

/// The password form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordForm {
    /// Family Chat or another server.
    pub kind: PasswordKind,
    /// The server field: the family's address, pre-filled from a link's
    /// `account_provider` (never its `hs`), or what was typed.
    pub server: String,
    /// What the empty server field shows.
    pub server_placeholder: String,
    /// The username field, pre-filled from a link's hint.
    pub username: String,
    /// Why the form is showing, such as a code that was used.
    pub notice: Option<String>,
    /// What went wrong with the last attempt.
    pub problem: Option<SigninProblem>,
    /// What the server offers, once it is known (after "Use another
    /// Matrix server"'s server step, or once a Family Chat address has
    /// been checked). `None` is a password form with an editable server.
    pub methods: Option<ServerMethods>,
}

impl PasswordForm {
    /// Whether the username and password fields show: always until the
    /// server is known, then only if it takes a password.
    pub fn shows_password(&self) -> bool {
        self.methods.as_ref().is_none_or(|methods| methods.password)
    }

    /// Whether the server is fixed (it was chosen on the server step and
    /// its methods depend on it): it shows as text with "Change server".
    pub fn server_is_fixed(&self) -> bool {
        self.kind == PasswordKind::OtherServer && self.methods.is_some()
    }
}

/// The server step of "Use another Matrix server": which server, before
/// anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerForm {
    /// What was typed (empty at first).
    pub server: String,
    /// What the empty field shows.
    pub server_placeholder: String,
    /// What went wrong finding it.
    pub problem: Option<SigninProblem>,
}

/// How a browser sign-in works on this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserKind {
    /// OAuth 2.0 through the server's account provider (MSC3861, MAS).
    OAuth,
    /// The server's single sign-on (`m.login.sso`).
    Sso,
}

/// A sign-in in the browser the server offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserMethod {
    /// OAuth or single sign-on.
    pub kind: BrowserKind,
    /// The full address the sign-in happens at (the OAuth issuer, or the
    /// homeserver for single sign-on), shown before the browser opens.
    pub address: String,
}

/// What a server offers, found before anything but discovery is sent; each
/// way shows only where it works (ADR 0010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerMethods {
    /// `m.login.password`.
    pub password: bool,
    /// "Continue in your browser".
    pub browser: Option<BrowserMethod>,
    /// "Sign in with a QR code": OAuth with the device authorization grant
    /// and the MSC4108 rendezvous.
    pub qr_code: bool,
}

impl ServerMethods {
    /// Whether the server offers anything the app can use.
    pub fn any(&self) -> bool {
        self.password || self.browser.is_some() || self.qr_code
    }
}

/// What a browser step is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPurpose {
    /// Signing this computer in.
    SignIn,
    /// Approving a new device's sign-in (after its QR code).
    ApproveDevice,
}

/// The address a browser sign-in opens. It carries one-time values of the
/// sign-in (its state, a device's code), so `Debug` hides it and it is
/// wiped when the last copy goes; a frontend opens it and never shows,
/// logs or keeps it.
#[derive(Clone)]
pub struct BrowserUrl(Arc<SecretText>);

impl BrowserUrl {
    /// Wraps `url`.
    pub fn new(url: String) -> Self {
        Self(Arc::new(SecretText::new(url)))
    }

    /// The address, to hand to the system's browser.
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl PartialEq for BrowserUrl {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for BrowserUrl {}

impl fmt::Debug for BrowserUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrowserUrl([redacted])")
    }
}

/// "Continue in your browser?": always asked, with the full address,
/// before a browser opens. [`SigninAnswer::Continue`] or
/// [`SigninAnswer::Cancel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmBrowser {
    /// The question.
    pub question: String,
    /// The full address the browser will open at (scheme, host and port).
    pub address: String,
    /// The sentence under it.
    pub detail: String,
    /// Signing in, or approving a new device.
    pub purpose: BrowserPurpose,
}

/// Waiting for the browser: the frontend opens [`BrowserStep::url`] once
/// when this step arrives (and again if the person asks), and shows the
/// address. [`SigninAnswer::Cancel`] stops waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserStep {
    /// The address it opened at, as the confirmation showed it.
    pub address: String,
    /// What to open.
    pub url: BrowserUrl,
    /// The sentence to show while waiting.
    pub message: String,
    /// Signing in, or approving a new device.
    pub purpose: BrowserPurpose,
}

/// What a QR code is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrPurpose {
    /// This computer signs in: a signed-in phone scans it.
    SignIn,
    /// A new phone signs in: it scans this computer's code.
    LinkDevice,
}

/// A QR code to scan (MSC4108). It carries the rendezvous and this
/// side's key, so [`QrModules`]' `Debug` shows its size only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrCodeStep {
    /// The code.
    pub qr: QrModules,
    /// What it is for.
    pub purpose: QrPurpose,
    /// The server, as the person chose it.
    pub server: String,
}

/// The two-digit check code the other device shows, to prove the two are
/// talking to each other: [`SigninAnswer::SubmitCheckCode`] or
/// [`SigninAnswer::Cancel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckCodeForm {
    /// What it is for.
    pub purpose: QrPurpose,
    /// What went wrong with the last code typed.
    pub problem: Option<String>,
}

/// Work in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Working {
    /// Redeeming a code.
    RedeemingCode,
    /// Finding the server and checking it.
    CheckingServer,
    /// Signing in with a password.
    SigningIn,
    /// Opening the new account.
    OpeningAccount,
    /// Getting the browser sign-in ready.
    PreparingBrowser,
    /// Making a QR code.
    PreparingQrCode,
    /// The other device is approving the sign-in.
    WaitingForOtherDevice,
    /// Sending the new device its keys.
    SharingKeys,
}

impl Working {
    /// What a frontend says meanwhile.
    pub fn message(self) -> &'static str {
        match self {
            Self::RedeemingCode => "Signing in with your code…",
            Self::CheckingServer => "Finding your server…",
            Self::SigningIn => "Signing in…",
            Self::OpeningAccount => "Opening your account…",
            Self::PreparingBrowser => "Getting your browser sign-in ready…",
            Self::PreparingQrCode => "Making a QR code…",
            Self::WaitingForOtherDevice => "Approve the sign-in on your other device…",
            Self::SharingKeys => "Sending your keys to the new device…",
        }
    }
}

/// The sign-in finished: the account is open and syncing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// The new account.
    pub account: AccountKey,
    /// Who signed in.
    pub user_id: OwnedUserId,
}

/// What a frontend draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigninStep {
    /// Starting.
    Starting,
    /// "Sign in as ...?": [`SigninAnswer::Continue`],
    /// [`SigninAnswer::UsePassword`] or [`SigninAnswer::Cancel`].
    Confirm(ConfirmCode),
    /// Busy; answers wait until the work is done.
    Working(Working),
    /// "Use another Matrix server"'s first step:
    /// [`SigninAnswer::ChooseServer`] or [`SigninAnswer::Cancel`].
    Server(ServerForm),
    /// The password form: [`SigninAnswer::SubmitPassword`] or
    /// [`SigninAnswer::Cancel`]; with the server's [`ServerMethods`], also
    /// [`SigninAnswer::UseBrowser`], [`SigninAnswer::UseQrCode`] and
    /// [`SigninAnswer::ChangeServer`].
    Password(PasswordForm),
    /// "Continue in your browser?" (always before a browser opens).
    ConfirmBrowser(ConfirmBrowser),
    /// Waiting for the sign-in in the browser.
    Browser(BrowserStep),
    /// A QR code to scan.
    QrCode(QrCodeStep),
    /// The other device's check code.
    CheckCode(CheckCodeForm),
    /// The new device signed in (final).
    DeviceLinked(String),
    /// A new device's sign-in stopped, and why (final).
    Stopped(String),
    /// Done (final).
    Finished(Finished),
    /// The account is already signed in; nothing was redeemed (final).
    AlreadySignedIn(String),
    /// The text was not a sign-in link (final).
    NotALink(String),
    /// The person cancelled, or the frontend went away (final).
    Cancelled,
}

impl SigninStep {
    /// Whether the flow has ended.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Finished(_)
                | Self::AlreadySignedIn(_)
                | Self::NotALink(_)
                | Self::Cancelled
                | Self::DeviceLinked(_)
                | Self::Stopped(_)
        )
    }
}

/// What a person answers.
#[derive(Debug)]
pub enum SigninAnswer {
    /// Use the code ("Continue").
    Continue,
    /// Don't use the code; sign in with a password instead.
    UsePassword,
    /// Sign in with these.
    SubmitPassword {
        /// The server field.
        server: String,
        /// The username field: a username or a full Matrix ID.
        username: String,
        /// The password.
        password: SecretText,
    },
    /// "Use another Matrix server": this server.
    ChooseServer {
        /// The server field.
        server: String,
    },
    /// Back to the server step, to choose another server.
    ChangeServer,
    /// "Continue in your browser".
    UseBrowser,
    /// "Sign in with a QR code".
    UseQrCode,
    /// The check code the other device shows.
    SubmitCheckCode(String),
    /// Stop.
    Cancel,
}

/// One sign-in in progress. Dropping it cancels the flow (a code not yet
/// redeemed is wiped unsent).
#[derive(Debug)]
pub struct SigninFlow {
    steps: watch::Receiver<SigninStep>,
    answers: mpsc::UnboundedSender<SigninAnswer>,
}

impl SigninFlow {
    /// A flow over its channels (for implementations of [`SigninApi`]).
    pub fn new(
        steps: watch::Receiver<SigninStep>,
        answers: mpsc::UnboundedSender<SigninAnswer>,
    ) -> Self {
        Self { steps, answers }
    }

    /// The steps, as they change.
    pub fn steps(&self) -> watch::Receiver<SigninStep> {
        self.steps.clone()
    }

    /// The current step.
    pub fn step(&self) -> SigninStep {
        self.steps.borrow().clone()
    }

    /// Sends an answer; `false` when the flow has ended.
    pub fn answer(&self, answer: SigninAnswer) -> bool {
        self.answers.send(answer).is_ok()
    }
}

/// Starts sign-ins. `familychat-signin`'s `LiveSignin` is the live one; a
/// fixture for screenshots and view tests is `familychat-test-support`'s
/// `FixtureSignin`.
pub trait SigninApi: Send + Sync {
    /// Starts a flow.
    fn start(&self, start: SigninStart) -> SigninFlow;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let link = SecretText::from("https://safechat.family/app/login?token=secret-token");
        let answer = SigninAnswer::SubmitPassword {
            server: "smith.safechat.family".to_owned(),
            username: "ana".to_owned(),
            password: SecretText::from("secret-password"),
        };
        for debug in [
            format!("{:?}", SigninStart::Link(link)),
            format!("{answer:?}"),
            format!("{answer:#?}"),
        ] {
            assert!(!debug.contains("secret-"), "{debug}");
        }
    }

    #[test]
    fn browser_addresses_never_print() {
        let url = BrowserUrl::new("https://auth.example.org/authorize?state=secret-state".into());
        let step = SigninStep::Browser(BrowserStep {
            address: "https://auth.example.org".to_owned(),
            url: url.clone(),
            message: String::new(),
            purpose: BrowserPurpose::SignIn,
        });
        assert!(!format!("{step:?}").contains("secret-"));
        assert_eq!(url, BrowserUrl::new(url.expose().to_owned()));
        assert!(SigninStep::DeviceLinked(String::new()).is_final());
        assert!(SigninStep::Stopped(String::new()).is_final());
    }

    #[test]
    fn the_password_fields_show_until_the_server_says_otherwise() {
        let mut form = PasswordForm {
            kind: PasswordKind::OtherServer,
            server: "example.org".to_owned(),
            server_placeholder: String::new(),
            username: String::new(),
            notice: None,
            problem: None,
            methods: None,
        };
        assert!(form.shows_password());
        assert!(!form.server_is_fixed());
        form.methods = Some(ServerMethods {
            password: false,
            browser: None,
            qr_code: true,
        });
        assert!(!form.shows_password());
        assert!(form.server_is_fixed());
        assert!(form.methods.as_ref().is_some_and(ServerMethods::any));
    }

    #[test]
    fn final_steps() {
        assert!(SigninStep::Cancelled.is_final());
        assert!(SigninStep::NotALink(String::new()).is_final());
        assert!(!SigninStep::Starting.is_final());
        assert!(!SigninStep::Working(Working::SigningIn).is_final());
    }
}
