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
//! is wiped when dropped. No step ever carries a token or a password.

use std::{fmt, time::Duration};

use ruma::OwnedUserId;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroize;

use crate::session::AccountKey;

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
    /// "Use another Matrix server", with a password.
    OtherServer,
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
}

impl Working {
    /// What a frontend says meanwhile.
    pub fn message(self) -> &'static str {
        match self {
            Self::RedeemingCode => "Signing in with your code…",
            Self::CheckingServer => "Finding your server…",
            Self::SigningIn => "Signing in…",
            Self::OpeningAccount => "Opening your account…",
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
    /// The password form: [`SigninAnswer::SubmitPassword`] or
    /// [`SigninAnswer::Cancel`].
    Password(PasswordForm),
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
            Self::Finished(_) | Self::AlreadySignedIn(_) | Self::NotALink(_) | Self::Cancelled
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
    fn final_steps() {
        assert!(SigninStep::Cancelled.is_final());
        assert!(SigninStep::NotALink(String::new()).is_final());
        assert!(!SigninStep::Starting.is_final());
        assert!(!SigninStep::Working(Working::SigningIn).is_final());
    }
}
