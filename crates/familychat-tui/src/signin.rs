//! The sign-in in the terminal (ADR 0008; PLAN.md, sections 6.2 to 6.5).
//!
//! It draws `familychat-core`'s [`SigninStep`]s and sends
//! [`SigninAnswer`]s, exactly as the window's `SigninView` does, and
//! decides nothing: which server, whether a code is used, what a link
//! means and every refusal are `familychat-signin`'s (ADR 0006).
//!
//! - **Welcome**: a code from a parent first, then a password, then
//!   another Matrix server.
//! - **Code**: a pasted sign-in link (the terminal's bracketed paste),
//!   kept masked and wiped once used. A paste on the welcome page or the
//!   code page starts the sign-in at once, as the window's Paste does; QR
//!   images are the window's.
//! - **Flow**: the confirmation, always shown before a code is redeemed
//!   and answered only with `y` ([`SigninAnswer::Continue`] is sent from
//!   nowhere else, and only while the step is `Confirm`); the password
//!   form with the rate-limit countdown; the work in progress; the final
//!   steps.
//! - Phase 14 (ADR 0010): "Use another Matrix server"'s server step, the
//!   server's methods on the form (Ctrl-B "Continue in your browser",
//!   Ctrl-E to change the server), the "Continue in your browser?"
//!   question with the full address, answered `Continue` only by `y`, and
//!   the wait while the browser is open: the address is opened once with
//!   the system's opener when that step arrives (`o` again, `c` copies it
//!   for a browser elsewhere) and never printed. QR codes are the
//!   window's: the form says so.

use std::{fmt, time::Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use familychat_core::signin::{SecretText, SigninAnswer, SigninFlow, SigninStart, SigninStep};
use familychat_security::terminal::sanitize_line;
use tokio::sync::watch;
use zeroize::Zeroize;

use crate::{
    app::{App, CommandEntry, Effect, InitialView, Screen},
    commands::Cmd,
};

/// The three ways in, in the welcome page's order.
pub(crate) const OPTIONS: [Cmd; 3] = [
    Cmd::SignInWithCode,
    Cmd::SignInWithPassword,
    Cmd::UseAnotherServer,
];

/// Which page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    /// The three ways in.
    Welcome,
    /// Paste the sign-in link.
    Code,
    /// A flow's steps.
    Flow,
}

/// A password form's fields, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    /// The family's address, or the homeserver.
    Server,
    /// The username or Matrix ID.
    Username,
    /// The password.
    Password,
}

impl Field {
    fn next(self) -> Self {
        match self {
            Self::Server => Self::Username,
            Self::Username => Self::Password,
            Self::Password => Self::Server,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Server => Self::Password,
            Self::Username => Self::Server,
            Self::Password => Self::Username,
        }
    }
}

/// Text that is a secret while it is typed (the link, the password):
/// `Debug` prints `[redacted]`, and it is wiped when taken or dropped.
#[derive(Default)]
pub(crate) struct SecretField(String);

impl SecretField {
    pub(crate) fn len(&self) -> usize {
        self.0.chars().count()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn push_str(&mut self, text: &str) {
        self.0.push_str(text);
    }

    pub(crate) fn pop(&mut self) {
        self.0.pop();
    }

    pub(crate) fn clear(&mut self) {
        self.0.zeroize();
    }

    /// The text, leaving the field empty.
    pub(crate) fn take(&mut self) -> SecretText {
        SecretText::new(std::mem::take(&mut self.0))
    }
}

impl Drop for SecretField {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// The sign-in screens' state.
#[derive(Debug)]
pub(crate) struct SigninState {
    pub(crate) page: Page,
    /// The welcome page's option under the cursor.
    pub(crate) cursor: usize,
    /// The masked link field.
    pub(crate) link: SecretField,
    /// Why the last link didn't work.
    pub(crate) code_problem: Option<String>,
    pub(crate) flow: Option<SigninFlow>,
    steps: Option<watch::Receiver<SigninStep>>,
    pub(crate) step: SigninStep,
    pub(crate) server: String,
    pub(crate) username: String,
    pub(crate) password: SecretField,
    pub(crate) focus: Field,
    /// When a rate limit ends.
    pub(crate) retry_at: Option<Instant>,
    /// The person answered `y` to "Continue in your browser?": the
    /// browser step that follows opens its address, once.
    browser_confirmed: bool,
}

impl SigninState {
    fn new() -> Self {
        Self {
            page: Page::Welcome,
            cursor: 0,
            link: SecretField::default(),
            code_problem: None,
            flow: None,
            steps: None,
            step: SigninStep::Starting,
            server: String::new(),
            username: String::new(),
            password: SecretField::default(),
            focus: Field::Server,
            retry_at: None,
            browser_confirmed: false,
        }
    }

    /// Whether the form's server is fixed (chosen on the server step), so
    /// Tab moves between the username and the password only.
    fn server_is_fixed(&self) -> bool {
        matches!(&self.step, SigninStep::Password(form) if form.server_is_fixed())
    }

    fn next_field(&mut self) {
        self.focus = self.focus.next();
        if self.focus == Field::Server && self.server_is_fixed() {
            self.focus = self.focus.next();
        }
    }

    fn previous_field(&mut self) {
        self.focus = self.focus.previous();
        if self.focus == Field::Server && self.server_is_fixed() {
            self.focus = self.focus.previous();
        }
    }

    /// Whole seconds left of a rate limit, while one is running.
    pub(crate) fn seconds_to_wait(&self) -> Option<u64> {
        let left = self.retry_at?.saturating_duration_since(Instant::now());
        (!left.is_zero()).then(|| left.as_secs_f64().ceil() as u64)
    }

    /// Ends the flow: dropping it cancels it, and a code not yet redeemed
    /// is wiped unsent.
    fn end_flow(&mut self) {
        self.flow = None;
        self.steps = None;
        self.step = SigninStep::Starting;
        self.retry_at = None;
        self.browser_confirmed = false;
        self.password.clear();
    }

    fn show_welcome(&mut self) {
        self.end_flow();
        self.link.clear();
        self.code_problem = None;
        self.page = Page::Welcome;
    }

    fn show_code(&mut self, problem: Option<String>) {
        self.end_flow();
        self.page = Page::Code;
        self.code_problem = problem;
    }

    /// The field being typed into.
    fn field_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            Field::Server => Some(&mut self.server),
            Field::Username => Some(&mut self.username),
            Field::Password => None,
        }
    }

    /// Shows `step`; `true` when the screen changed.
    fn show_step(&mut self, step: SigninStep) {
        match &step {
            SigninStep::Password(form) => {
                let first = !matches!(&self.step, SigninStep::Password(previous) if previous.kind == form.kind);
                if first || self.server != form.server {
                    self.server = form.server.clone();
                }
                if first || self.username != form.username {
                    self.username = form.username.clone();
                }
                self.retry_at = form
                    .problem
                    .as_ref()
                    .and_then(|problem| problem.retry_after)
                    .map(|wait| Instant::now() + wait);
                self.focus = if form.server.is_empty() && !form.server_is_fixed() {
                    Field::Server
                } else if form.username.is_empty() {
                    Field::Username
                } else {
                    Field::Password
                };
            }
            SigninStep::Server(form) => {
                if !matches!(self.step, SigninStep::Server(_)) || self.server != form.server {
                    self.server = form.server.clone();
                }
                self.focus = Field::Server;
            }
            SigninStep::NotALink(message) => {
                self.show_code(Some(message.clone()));
                return;
            }
            SigninStep::Cancelled => {
                self.show_welcome();
                return;
            }
            _ => {}
        }
        self.step = step;
    }
}

impl App {
    /// A new sign-in screen on the welcome page.
    pub(crate) fn signin_screen(&self) -> Screen {
        Screen::Signin(Box::new(SigninState::new()))
    }

    fn signin_state(&mut self) -> Option<&mut SigninState> {
        match &mut self.screen {
            Screen::Signin(signin) => Some(signin),
            _ => None,
        }
    }

    /// Polls the flow's steps; `true` when one arrived.
    pub(crate) fn signin_tick(&mut self) -> bool {
        let Some(signin) = self.signin_state() else {
            return false;
        };
        if signin.retry_at.is_some() && signin.seconds_to_wait().is_none() {
            signin.retry_at = None;
        }
        let Some(steps) = signin.steps.as_mut() else {
            return false;
        };
        match steps.has_changed() {
            Ok(true) => {}
            Ok(false) => return false,
            // The flow has ended: its last step is the one shown.
            Err(_) => {
                let last = steps.borrow().clone();
                signin.steps = None;
                if last == signin.step {
                    return false;
                }
                signin.show_step(last);
                return true;
            }
        }
        let step = steps.borrow_and_update().clone();
        let finished = matches!(step, SigninStep::Finished(_));
        signin.show_step(step);
        if finished {
            // The accounts watch shows the rooms as soon as it has the
            // new account.
            signin.steps = None;
        }
        self.open_new_browser_step();
        true
    }

    /// A browser step that just arrived opens its address with the
    /// system's opener, once, and only after the person's `y` to the
    /// question that showed the address.
    fn open_new_browser_step(&mut self) {
        let Some(signin) = self.signin_state() else {
            return;
        };
        let SigninStep::Browser(browser) = &signin.step else {
            return;
        };
        if !std::mem::take(&mut signin.browser_confirmed) {
            return;
        }
        let url = browser.url.expose().to_owned();
        self.effects.push(Effect::Open(url));
    }

    /// Starts a flow and follows its steps.
    pub(crate) fn start_signin(&mut self, start: SigninStart) {
        let Some(signin_api) = self.opened().map(|opened| opened.signin.clone()) else {
            return;
        };
        let Some(signin) = self.signin_state() else {
            return;
        };
        signin.end_flow();
        let flow = signin_api.start(start);
        let mut steps = flow.steps();
        let first = steps.borrow_and_update().clone();
        signin.flow = Some(flow);
        signin.steps = Some(steps);
        signin.page = Page::Flow;
        signin.code_problem = None;
        signin.show_step(first);
        self.open_new_browser_step();
    }

    /// The fixture's first view.
    pub(crate) fn launch_signin(&mut self, initial: InitialView) {
        match initial {
            InitialView::CodeEntry => self.signin_run(Cmd::SignInWithCode),
            InitialView::TypedLink(text) => {
                if let Some(signin) = self.signin_state() {
                    signin.show_code(None);
                    signin.link.push_str(&text);
                }
            }
            InitialView::PastedLink(text) => self.signin_paste(text),
            InitialView::Password => self.signin_run(Cmd::SignInWithPassword),
            InitialView::OtherServer => self.signin_run(Cmd::UseAnotherServer),
            _ => {}
        }
    }

    /// Whether the page takes letters as text (the link and password
    /// fields) or as commands.
    pub(crate) fn signin_takes_text(&self) -> bool {
        match &self.screen {
            Screen::Signin(signin) => {
                signin.page == Page::Code
                    || (signin.page == Page::Flow
                        && !matches!(
                            signin.step,
                            SigninStep::Finished(_)
                                | SigninStep::NotALink(_)
                                | SigninStep::Cancelled
                        ))
            }
            _ => false,
        }
    }

    /// A paste: on the welcome or code page it is a sign-in link and
    /// starts the sign-in (which always asks before a code is used); in a
    /// password form it goes into the field.
    pub(crate) fn signin_paste(&mut self, pasted: String) {
        let Some(signin) = self.signin_state() else {
            return;
        };
        match signin.page {
            Page::Welcome | Page::Code => {
                signin.link.clear();
                if pasted.trim().is_empty() {
                    return;
                }
                self.start_signin(SigninStart::Link(SecretText::new(pasted)));
            }
            Page::Flow => {
                if !matches!(signin.step, SigninStep::Password(_) | SigninStep::Server(_)) {
                    return;
                }
                match signin.focus {
                    Field::Password => {
                        // A password is kept as typed, without its line
                        // breaks.
                        let text: String = pasted.chars().filter(|ch| !ch.is_control()).collect();
                        signin.password.push_str(&text);
                    }
                    _ => {
                        if let Some(field) = signin.field_mut() {
                            field.push_str(&sanitize_line(&pasted));
                        }
                    }
                }
                let mut pasted = pasted;
                pasted.zeroize();
            }
        }
    }

    /// A key on a page that takes text.
    pub(crate) fn signin_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(signin) = self.signin_state() else {
            return;
        };
        if signin.page == Page::Code {
            match key.code {
                KeyCode::Enter => {
                    if signin.link.is_empty() {
                        return;
                    }
                    let link = signin.link.take();
                    self.start_signin(SigninStart::Link(link));
                }
                KeyCode::Esc => signin.show_welcome(),
                KeyCode::Backspace => signin.link.pop(),
                KeyCode::Char('u') if control => signin.link.clear(),
                KeyCode::Char('v') if control => self.paste_hint(),
                KeyCode::Char(ch) if !control && !ch.is_control() => {
                    signin.link.push_str(ch.encode_utf8(&mut [0; 4]));
                }
                _ => {}
            }
            return;
        }
        let answer = match key.code {
            KeyCode::Char(ch) if !control => Some(ch.to_ascii_lowercase()),
            _ => None,
        };
        match signin.step.clone() {
            SigninStep::Confirm(_) => match (answer, key.code) {
                // The only place a code is used, and only while the
                // confirmation shows.
                (Some('y'), _) => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::Continue);
                    }
                }
                (Some('p'), _) => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::UsePassword);
                    }
                }
                (Some('n'), _) | (_, KeyCode::Esc) => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::Cancel);
                    }
                }
                _ => {}
            },
            // The browser opens only after `y` here (the full address is
            // on screen).
            SigninStep::ConfirmBrowser(_) => match (answer, key.code) {
                (Some('y'), _) => {
                    if let Some(flow) = &signin.flow {
                        signin.browser_confirmed = flow.answer(SigninAnswer::Continue);
                    }
                }
                (Some('n'), _) | (_, KeyCode::Esc) => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::Cancel);
                    }
                }
                _ => {}
            },
            SigninStep::Browser(browser) => match (answer, key.code) {
                (Some('o'), _) => self
                    .effects
                    .push(Effect::Open(browser.url.expose().to_owned())),
                (Some('c'), _) => {
                    self.effects
                        .push(Effect::Copy(browser.url.expose().to_owned()));
                    self.status = Some("Copied the sign-in address".to_owned());
                }
                (Some('n'), _) | (_, KeyCode::Esc) => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::Cancel);
                    }
                }
                _ => {}
            },
            // QR codes are drawn in the window only; a flow that shows one
            // here (none does) can only be cancelled.
            SigninStep::QrCode(_) | SigninStep::CheckCode(_) => {
                if key.code == KeyCode::Esc
                    && let Some(flow) = &signin.flow
                {
                    flow.answer(SigninAnswer::Cancel);
                }
            }
            SigninStep::Server(_) => match key.code {
                KeyCode::Esc => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::Cancel);
                    }
                }
                KeyCode::Enter => {
                    let server = signin.server.trim().to_owned();
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::ChooseServer { server });
                    }
                }
                KeyCode::Backspace => {
                    signin.server.pop();
                }
                KeyCode::Char('u') if control => signin.server.clear(),
                KeyCode::Char(ch) if !control && !ch.is_control() => signin.server.push(ch),
                _ => {}
            },
            SigninStep::Starting | SigninStep::Working(_) => {}
            SigninStep::DeviceLinked(_) | SigninStep::Stopped(_) => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    signin.show_welcome();
                }
            }
            SigninStep::AlreadySignedIn(_) => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    signin.show_welcome();
                }
            }
            SigninStep::Password(form) => match key.code {
                KeyCode::Esc => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::Cancel);
                    }
                }
                KeyCode::Char('b')
                    if control
                        && form
                            .methods
                            .as_ref()
                            .is_some_and(|methods| methods.browser.is_some()) =>
                {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::UseBrowser);
                    }
                }
                KeyCode::Char('e') if control && form.server_is_fixed() => {
                    if let Some(flow) = &signin.flow {
                        flow.answer(SigninAnswer::ChangeServer);
                    }
                }
                _ if !form.shows_password() => {}
                KeyCode::Tab | KeyCode::Down => signin.next_field(),
                KeyCode::BackTab | KeyCode::Up => signin.previous_field(),
                KeyCode::Enter if signin.focus != Field::Password => {
                    signin.next_field();
                }
                KeyCode::Enter => self.submit_password(),
                KeyCode::Backspace => match signin.focus {
                    Field::Password => signin.password.pop(),
                    _ => {
                        if let Some(field) = signin.field_mut() {
                            field.pop();
                        }
                    }
                },
                KeyCode::Char('u') if control => match signin.focus {
                    Field::Password => signin.password.clear(),
                    _ => {
                        if let Some(field) = signin.field_mut() {
                            field.clear();
                        }
                    }
                },
                KeyCode::Char(ch) if !control && !ch.is_control() => match signin.focus {
                    Field::Password => signin.password.push_str(ch.encode_utf8(&mut [0; 4])),
                    _ => {
                        if let Some(field) = signin.field_mut() {
                            field.push(ch);
                        }
                    }
                },
                _ => {}
            },
            SigninStep::Finished(_) | SigninStep::NotALink(_) | SigninStep::Cancelled => {}
        }
    }

    /// Whether the form offers the browser (for the command bar).
    fn offers_browser(signin: &SigninState) -> bool {
        matches!(&signin.step, SigninStep::Password(form) if form.methods.as_ref().is_some_and(|methods| methods.browser.is_some()))
    }

    /// Signs in with the form, unless a rate limit is still running.
    fn submit_password(&mut self) {
        let Some(signin) = self.signin_state() else {
            return;
        };
        if !matches!(signin.step, SigninStep::Password(_)) || signin.seconds_to_wait().is_some() {
            return;
        }
        let answer = SigninAnswer::SubmitPassword {
            server: signin.server.trim().to_owned(),
            username: signin.username.trim().to_owned(),
            password: signin.password.take(),
        };
        if let Some(flow) = &signin.flow {
            flow.answer(answer);
        }
    }

    fn paste_hint(&mut self) {
        self.status = Some(
            "Paste with your terminal: Ctrl-Shift-V, or Cmd-V on a Mac (QR code images: use \
             the window)"
                .to_owned(),
        );
    }

    /// A command on the sign-in screens.
    pub(crate) fn signin_run(&mut self, cmd: Cmd) {
        let Some(signin) = self.signin_state() else {
            return;
        };
        match cmd {
            Cmd::SignInWithCode => signin.show_code(None),
            Cmd::SignInWithPassword => self.start_signin(SigninStart::FamilyChatPassword),
            Cmd::UseAnotherServer => self.start_signin(SigninStart::OtherServer),
            Cmd::PasteSignInCode => {
                if signin.page == Page::Welcome {
                    signin.show_code(None);
                }
                self.paste_hint();
            }
            Cmd::GoBack | Cmd::Back => match signin.page {
                Page::Welcome => {}
                Page::Code => signin.show_welcome(),
                Page::Flow => self.signin_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            },
            Cmd::Up if signin.page == Page::Welcome => {
                signin.cursor = signin.cursor.saturating_sub(1);
            }
            Cmd::Down if signin.page == Page::Welcome => {
                signin.cursor = (signin.cursor + 1).min(OPTIONS.len() - 1);
            }
            Cmd::Select if signin.page == Page::Welcome => {
                let option = OPTIONS[signin.cursor.min(OPTIONS.len() - 1)];
                self.signin_run(option);
            }
            Cmd::Select | Cmd::NextField | Cmd::Answer(_) => {
                let key = match cmd {
                    Cmd::NextField => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                    // A control character stands for its Ctrl key (the
                    // command bar's ^B and ^E).
                    Cmd::Answer(ch @ '\u{1}'..='\u{1a}') => KeyEvent::new(
                        KeyCode::Char(char::from(ch as u8 - 1 + b'a')),
                        KeyModifiers::CONTROL,
                    ),
                    Cmd::Answer(ch) => KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                    _ => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                };
                self.signin_key(key);
            }
            _ => {}
        }
    }

    /// A click on the welcome page's option `target`.
    pub(crate) fn signin_click(&mut self, target: usize) {
        if let Some(signin) = self.signin_state()
            && signin.page == Page::Welcome
            && target < OPTIONS.len()
        {
            signin.cursor = target;
            self.signin_run(OPTIONS[target]);
        }
    }

    /// The command bar on the sign-in screens.
    pub(crate) fn signin_commands(&self, signin: &SigninState) -> Vec<CommandEntry> {
        let quit = CommandEntry::with("^C", "Quit", Cmd::Quit);
        let back = CommandEntry::with("Esc", "Back", Cmd::GoBack);
        match signin.page {
            Page::Welcome => vec![
                CommandEntry::with("Enter", "Choose", Cmd::Select),
                CommandEntry::of(Cmd::SignInWithCode),
                CommandEntry::of(Cmd::SignInWithPassword),
                CommandEntry::of(Cmd::UseAnotherServer),
                CommandEntry::of(Cmd::PasteSignInCode),
                CommandEntry::of(Cmd::Palette),
                CommandEntry::of(Cmd::Quit),
            ],
            Page::Code => vec![
                CommandEntry::with("Enter", "SignIn", Cmd::Select),
                CommandEntry::of(Cmd::PasteSignInCode),
                back,
                quit,
            ],
            Page::Flow => match &signin.step {
                SigninStep::Confirm(_) => vec![
                    CommandEntry::with("Y", "Continue", Cmd::Answer('y')),
                    CommandEntry::with("P", "Password", Cmd::Answer('p')),
                    CommandEntry::with("N", "Cancel", Cmd::Answer('n')),
                    quit,
                ],
                SigninStep::Password(form) => {
                    let mut entries = Vec::new();
                    if Self::offers_browser(signin) {
                        entries.push(CommandEntry::with("^B", "Browser", Cmd::Answer('\u{2}')));
                    }
                    if form.shows_password() {
                        entries.push(CommandEntry::with(
                            "Enter",
                            if signin.focus == Field::Password {
                                "SignIn"
                            } else {
                                "Next"
                            },
                            Cmd::Select,
                        ));
                        entries.push(CommandEntry::of(Cmd::NextField));
                    }
                    if form.server_is_fixed() {
                        entries.push(CommandEntry::with("^E", "Server", Cmd::Answer('\u{5}')));
                    }
                    entries.extend([back, quit]);
                    entries
                }
                SigninStep::Server(_) => {
                    vec![CommandEntry::with("Enter", "Find", Cmd::Select), back, quit]
                }
                SigninStep::ConfirmBrowser(_) => vec![
                    CommandEntry::with("Y", "Continue", Cmd::Answer('y')),
                    CommandEntry::with("N", "Cancel", Cmd::Answer('n')),
                    quit,
                ],
                SigninStep::Browser(_) => vec![
                    CommandEntry::with("O", "OpenAgain", Cmd::Answer('o')),
                    CommandEntry::with("C", "CopyAddr", Cmd::Answer('c')),
                    CommandEntry::with("N", "Cancel", Cmd::Answer('n')),
                    quit,
                ],
                SigninStep::DeviceLinked(_) | SigninStep::Stopped(_) => {
                    vec![CommandEntry::with("Enter", "OK", Cmd::Select), back, quit]
                }
                SigninStep::AlreadySignedIn(_) => {
                    vec![CommandEntry::with("Enter", "OK", Cmd::Select), back, quit]
                }
                _ => vec![quit],
            },
        }
    }
}
