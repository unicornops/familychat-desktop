//! End-to-end encryption in the terminal (ADR 0008, "Encryption"; ADR
//! 0009), the window's `EncryptionView` in text. It draws the session's
//! [`EncryptionStatus`] and [`VerificationFlow`]s and sends the person's
//! answers back; every rule and all the crypto are the session's.
//!
//! - **Confirm it's you** takes the rooms' place while this device isn't
//!   confirmed: another device (`d`, comparing emoji), the recovery key
//!   (`r`, typed into a masked field), or skipping (`s`, after a warning,
//!   then a banner over the rooms; `V` brings the screen back).
//! - **Your server needs your password** takes its place when the server
//!   asked for it before the cross-signing identity could be made.
//! - The dialogs: the verification (waiting, a request from another
//!   device, the seven emoji with their names, done or why it stopped),
//!   skipping, and setting recovery up with the key shown once.
//!
//! A recovery key or a password is typed into a [`SecretField`] (masked,
//! wiped when taken or dropped) and handed on as a [`SecretText`]; the
//! recovery key shown once is dropped (wiped) when its dialog closes.
//! None is logged. A QR code isn't drawn: a terminal's cells don't make a
//! code a phone reliably reads, so the terminal compares emoji.

use familychat_core::{
    encryption::{
        AuthOutcome, EncryptionStatus, IdentitySetup, RecoveryState, SecretText,
        VerificationAnswer, VerificationFlow, VerificationStep, wording,
    },
    session::{AccountKey, Reply, SessionApi, SessionError},
};
use familychat_security::terminal::sanitize_line;
use tokio::sync::{oneshot, watch};

use crate::{
    app::{App, CommandEntry, Effect},
    commands::Cmd,
    signin::SecretField,
};

/// A page of "Confirm it's you".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    /// The ways to confirm.
    Choose,
    /// Typing the recovery key.
    RecoveryKey,
}

/// The dialog over the rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialog {
    /// None.
    None,
    /// "Skip confirming?"
    SkipWarning,
    /// A verification, ours or another device's.
    Verification,
    /// "Set up recovery".
    SetUpRecovery,
    /// The new recovery key, shown once.
    RecoveryKey,
}

/// The encryption of the account shown.
#[derive(Debug)]
pub(crate) struct EncryptionState {
    status_rx: Option<watch::Receiver<EncryptionStatus>>,
    pub(crate) status: EncryptionStatus,
    /// The person skipped "Confirm it's you" this time.
    pub(crate) skipped: bool,
    pub(crate) page: Page,
    /// The recovery key or the password being typed.
    pub(crate) field: SecretField,
    /// A request is under way.
    pub(crate) working: bool,
    /// What went wrong on the page or in the dialog.
    pub(crate) problem: Option<String>,
    pub(crate) dialog: Dialog,
    pub(crate) flow: Option<VerificationFlow>,
    steps: Option<watch::Receiver<VerificationStep>>,
    pub(crate) step: VerificationStep,
    /// An answer was sent and its step hasn't come back yet.
    awaiting: bool,
    flow_reply: Option<Reply<VerificationFlow>>,
    recover_reply: Option<Reply<()>>,
    identity_reply: Option<Reply<AuthOutcome>>,
    recovery_reply: Option<Reply<SecretText>>,
    /// The new recovery key, while its dialog shows.
    pub(crate) recovery_key: Option<SecretText>,
    /// It was copied.
    pub(crate) key_copied: bool,
}

fn poll<T>(reply: &mut Option<oneshot::Receiver<T>>) -> Option<Result<T, SessionError>> {
    let receiver = reply.as_mut()?;
    match receiver.try_recv() {
        Ok(value) => {
            *reply = None;
            Some(Ok(value))
        }
        Err(oneshot::error::TryRecvError::Empty) => None,
        Err(oneshot::error::TryRecvError::Closed) => {
            *reply = None;
            Some(Err(SessionError::ShutDown))
        }
    }
}

impl EncryptionState {
    /// The encryption of `account`, following its status.
    pub(crate) fn new(session: Option<&dyn SessionApi>, account: &AccountKey) -> Self {
        let status_rx = session.and_then(|session| session.encryption(account));
        let status = status_rx
            .as_ref()
            .map(|status| status.borrow().clone())
            .unwrap_or_default();
        Self {
            status_rx,
            status,
            skipped: false,
            page: Page::Choose,
            field: SecretField::default(),
            working: false,
            problem: None,
            dialog: Dialog::None,
            flow: None,
            steps: None,
            step: VerificationStep::Waiting,
            awaiting: false,
            flow_reply: None,
            recover_reply: None,
            identity_reply: None,
            recovery_reply: None,
            recovery_key: None,
            key_copied: false,
        }
    }

    /// Whether a screen takes the rooms' place.
    pub(crate) fn takes_over(&self) -> bool {
        !self.skipped
            && (self.status.needs_confirmation()
                || self.status.identity == IdentitySetup::NeedsPassword)
    }

    /// Whether the password page is the one taking over.
    pub(crate) fn asks_for_password(&self) -> bool {
        !self.status.needs_confirmation() && self.status.identity == IdentitySetup::NeedsPassword
    }

    /// Whether the screen showing takes text (the recovery key or the
    /// password).
    pub(crate) fn takes_text(&self) -> bool {
        self.dialog == Dialog::None
            && self.takes_over()
            && (self.asks_for_password() || self.page == Page::RecoveryKey)
    }

    /// Answers asked for that haven't arrived.
    pub(crate) fn pending(&self) -> usize {
        usize::from(self.flow_reply.is_some())
            + usize::from(self.recover_reply.is_some())
            + usize::from(self.identity_reply.is_some())
            + usize::from(self.recovery_reply.is_some())
            + usize::from(self.awaiting)
    }

    fn set_status(&mut self, status: EncryptionStatus) {
        let was_confirmed = !self.status.needs_confirmation();
        self.status = status;
        if self.status.needs_confirmation() && was_confirmed {
            // A new reason to confirm (the identity was reset elsewhere):
            // ask again.
            self.skipped = false;
        }
        if !self.status.needs_confirmation() && self.page == Page::RecoveryKey {
            self.page = Page::Choose;
            self.field.clear();
        }
    }

    fn follow(&mut self, flow: VerificationFlow) {
        let mut steps = flow.steps();
        self.step = steps.borrow_and_update().clone();
        self.steps = Some(steps);
        self.flow = Some(flow);
    }

    /// Closes the dialog: an unfinished verification is cancelled, and the
    /// recovery key it showed is wiped.
    pub(crate) fn close_dialog(&mut self) {
        if self.dialog == Dialog::Verification
            && let Some(flow) = self.flow.take()
            && !self.step.is_final()
        {
            flow.answer(VerificationAnswer::Cancel);
        }
        self.flow = None;
        self.steps = None;
        self.awaiting = false;
        self.flow_reply = None;
        self.recovery_key = None;
        self.key_copied = false;
        self.dialog = Dialog::None;
        self.problem = None;
        self.working = false;
    }

    /// A request from another of the account's devices. One at a time: a
    /// newer one while a verification is open is declined.
    pub(crate) fn incoming(&mut self, flow: VerificationFlow) {
        if self.dialog == Dialog::Verification && !self.step.is_final() {
            flow.answer(VerificationAnswer::Cancel);
            return;
        }
        self.close_dialog();
        self.dialog = Dialog::Verification;
        self.follow(flow);
    }

    /// The banner over the rooms, if one applies: its sentence and the
    /// key that answers it.
    pub(crate) fn banner(&self) -> Option<(String, bool)> {
        let status = &self.status;
        if self.skipped
            && (status.needs_confirmation() || status.identity == IdentitySetup::NeedsPassword)
        {
            Some((
                format!(
                    "Not confirmed: {} Press V to confirm it's you.",
                    wording::NOT_CONFIRMED_BANNER
                ),
                true,
            ))
        } else if let IdentitySetup::Failed(message) = &status.identity {
            Some((
                format!(
                    "Encryption isn't set up: {} Press t to try again.",
                    sanitize_line(message)
                ),
                true,
            ))
        } else if status.offers_recovery_setup() {
            Some((
                format!("{} Press ^R to set it up.", wording::RECOVERY_BANNER),
                false,
            ))
        } else {
            None
        }
    }
}

impl App {
    fn encryption_mut(&mut self) -> Option<&mut EncryptionState> {
        self.rooms_state().map(|rooms| &mut rooms.encryption)
    }

    fn encryption_ref(&self) -> Option<&EncryptionState> {
        self.rooms_ref().map(|rooms| &rooms.encryption)
    }

    fn account_key(&self) -> Option<AccountKey> {
        self.rooms_ref().map(|rooms| rooms.account.key.clone())
    }

    fn session(&self) -> Option<std::sync::Arc<dyn SessionApi>> {
        self.opened().map(|opened| opened.session.clone())
    }

    /// Whether an encryption screen or dialog has the keys.
    pub(crate) fn encryption_has_keys(&self) -> bool {
        self.encryption_ref()
            .is_some_and(|state| state.dialog != Dialog::None || state.takes_over())
    }

    /// Polls the status and the answers; `true` when anything arrived.
    pub(crate) fn encryption_tick(&mut self) -> bool {
        let mut notice = None;
        let Some(state) = self.encryption_mut() else {
            return false;
        };
        let mut changed = false;
        if let Some(status) = state.status_rx.as_mut()
            && status.has_changed().unwrap_or(false)
        {
            let status = status.borrow_and_update().clone();
            state.set_status(status);
            changed = true;
        }
        if let Some(result) = poll(&mut state.flow_reply) {
            changed = true;
            state.working = false;
            match result.and_then(|answer| answer) {
                Ok(flow) => state.follow(flow),
                Err(error) => {
                    state.step = VerificationStep::Cancelled(sanitize_line(&error.to_string()));
                }
            }
        }
        if let Some(steps) = state.steps.as_mut() {
            match steps.has_changed() {
                Ok(true) => {
                    state.step = steps.borrow_and_update().clone();
                    state.awaiting = false;
                    changed = true;
                }
                Ok(false) => {}
                // The flow ended: its last step stays readable.
                Err(_) => {
                    state.step = steps.borrow().clone();
                    state.steps = None;
                    state.awaiting = false;
                    changed = true;
                }
            }
        }
        if let Some(result) = poll(&mut state.recover_reply) {
            changed = true;
            state.working = false;
            match result.and_then(|answer| answer) {
                Ok(()) => {
                    state.page = Page::Choose;
                    notice = Some("Confirmed. This device can now read your messages.".to_owned());
                }
                Err(error) => state.problem = Some(sanitize_line(&error.to_string())),
            }
        }
        if let Some(result) = poll(&mut state.identity_reply) {
            changed = true;
            state.working = false;
            match result.and_then(|answer| answer) {
                Ok(AuthOutcome::Done) => state.problem = None,
                Ok(AuthOutcome::WrongPassword) => {
                    state.problem = Some(wording::WRONG_PASSWORD.to_owned());
                }
                Ok(AuthOutcome::PasswordNeeded) => {
                    state.problem = Some(wording::NO_PASSWORD.to_owned());
                }
                Err(error) => state.problem = Some(sanitize_line(&error.to_string())),
            }
        }
        if let Some(result) = poll(&mut state.recovery_reply) {
            changed = true;
            state.working = false;
            match result.and_then(|answer| answer) {
                Ok(key) => {
                    state.recovery_key = Some(key);
                    state.key_copied = false;
                    state.dialog = Dialog::RecoveryKey;
                }
                Err(error) => state.problem = Some(sanitize_line(&error.to_string())),
            }
        }
        if let Some(notice) = notice {
            self.status = Some(notice);
        }
        changed
    }

    /// A verification request from another device: shown when it is for
    /// the account on screen, declined otherwise.
    pub(crate) fn verification_request(&mut self, account: AccountKey, flow: VerificationFlow) {
        if self.account_key().as_ref() != Some(&account) {
            flow.answer(VerificationAnswer::Cancel);
            return;
        }
        self.prompt = None;
        if let Some(state) = self.encryption_mut() {
            state.incoming(flow);
        }
    }

    /// `V`: "Confirm it's you" again, or a word that there's nothing to.
    pub(crate) fn confirm_identity(&mut self) {
        let Some(state) = self.encryption_mut() else {
            return;
        };
        if state.status.needs_confirmation()
            || state.status.identity == IdentitySetup::NeedsPassword
        {
            state.skipped = false;
            state.page = Page::Choose;
            state.problem = None;
        } else {
            self.status = Some("This device is already confirmed.".to_owned());
        }
    }

    /// `^R`: "Set up recovery".
    pub(crate) fn set_up_recovery(&mut self) {
        let Some(state) = self.encryption_mut() else {
            return;
        };
        if state.status.recovery == RecoveryState::Enabled {
            self.status = Some("Recovery is already set up.".to_owned());
            return;
        }
        state.close_dialog();
        state.dialog = Dialog::SetUpRecovery;
    }

    /// `t` after the identity couldn't be made: try again.
    pub(crate) fn retry_identity(&mut self) -> bool {
        let (Some(session), Some(account)) = (self.session(), self.account_key()) else {
            return false;
        };
        let Some(state) = self.encryption_mut() else {
            return false;
        };
        if !matches!(state.status.identity, IdentitySetup::Failed(_)) {
            return false;
        }
        state.identity_reply = Some(session.set_up_identity(&account, None));
        self.status = Some("Trying again…".to_owned());
        true
    }

    fn verify_this_device(&mut self) {
        let (Some(session), Some(account)) = (self.session(), self.account_key()) else {
            return;
        };
        let Some(state) = self.encryption_mut() else {
            return;
        };
        state.close_dialog();
        state.dialog = Dialog::Verification;
        state.step = VerificationStep::Waiting;
        state.working = true;
        state.flow_reply = Some(session.verify_this_device(&account));
    }

    fn submit_secret(&mut self) {
        let (Some(session), Some(account)) = (self.session(), self.account_key()) else {
            return;
        };
        let Some(state) = self.encryption_mut() else {
            return;
        };
        if state.working {
            return;
        }
        let password = state.asks_for_password();
        let secret = state.field.take();
        if secret.expose().trim().is_empty() {
            state.problem = Some(
                if password {
                    wording::NO_PASSWORD
                } else {
                    wording::NO_RECOVERY_KEY
                }
                .to_owned(),
            );
            return;
        }
        state.working = true;
        state.problem = None;
        if password {
            state.identity_reply = Some(session.set_up_identity(&account, Some(secret)));
        } else {
            state.recover_reply = Some(session.recover(&account, secret));
        }
    }

    /// A paste while the recovery key or the password is being typed: into
    /// the field, without line breaks; the pasted text is wiped.
    pub(crate) fn encryption_paste(&mut self, mut pasted: String) {
        if let Some(state) = self.encryption_mut()
            && state.takes_text()
        {
            let text: String = pasted.chars().filter(|ch| !ch.is_control()).collect();
            state.field.push_str(&text);
            let mut text = text;
            zeroize::Zeroize::zeroize(&mut text);
        }
        zeroize::Zeroize::zeroize(&mut pasted);
    }

    /// A key while an encryption screen or dialog has the keys; `false`
    /// when it is the app's (quit, the palette).
    pub(crate) fn encryption_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(state) = self.encryption_mut() else {
            return false;
        };
        if state.dialog == Dialog::None && state.takes_text() {
            match key.code {
                KeyCode::Enter => self.submit_secret(),
                KeyCode::Esc => {
                    state.field.clear();
                    state.problem = None;
                    if state.asks_for_password() {
                        state.dialog = Dialog::SkipWarning;
                    } else {
                        state.page = Page::Choose;
                    }
                }
                KeyCode::Backspace => state.field.pop(),
                KeyCode::Char('u') if control => state.field.clear(),
                KeyCode::Char(ch) if !control && !ch.is_control() => {
                    state.field.push_str(ch.encode_utf8(&mut [0; 4]));
                }
                _ => return control && key.code == KeyCode::Char('c'),
            }
            return true;
        }
        let answer = match key.code {
            KeyCode::Char(ch) if !control => Some(ch.to_ascii_lowercase()),
            KeyCode::Enter => Some('\n'),
            KeyCode::Esc => Some('\u{1b}'),
            _ => None,
        };
        match answer {
            Some(ch) => self.encryption_answer(ch),
            None => false,
        }
    }

    /// The answer `ch` (a letter, `'\n'` for Enter, ESC for Esc) on the
    /// encryption screen or dialog; `false` when it isn't one of theirs.
    pub(crate) fn encryption_answer(&mut self, ch: char) -> bool {
        let Some(state) = self.encryption_mut() else {
            return false;
        };
        let esc = ch == '\u{1b}';
        match state.dialog {
            Dialog::SkipWarning => {
                match ch {
                    'y' => {
                        state.skipped = true;
                        state.page = Page::Choose;
                        state.field.clear();
                        state.close_dialog();
                    }
                    'n' => state.close_dialog(),
                    _ if esc => state.close_dialog(),
                    _ => {}
                }
                true
            }
            Dialog::Verification => {
                let step = state.step.clone();
                let answer = match (&step, ch) {
                    (step, '\n' | '\u{1b}') if step.is_final() => {
                        state.close_dialog();
                        return true;
                    }
                    (VerificationStep::Incoming, 'y') => Some(VerificationAnswer::Accept),
                    (VerificationStep::Ready { .. }, 'e' | '\n') => {
                        Some(VerificationAnswer::CompareEmoji)
                    }
                    (VerificationStep::CompareEmoji(_), 'y') => Some(VerificationAnswer::Match),
                    (VerificationStep::CompareEmoji(_), 'n') => Some(VerificationAnswer::NoMatch),
                    (VerificationStep::Scanned, 'y') => Some(VerificationAnswer::ConfirmScanned),
                    (step, 'n') if !step.is_final() => Some(VerificationAnswer::Cancel),
                    (step, '\u{1b}') if !step.is_final() => Some(VerificationAnswer::Cancel),
                    _ => None,
                };
                if let Some(answer) = answer {
                    match &state.flow {
                        Some(flow) => state.awaiting = flow.answer(answer),
                        // Our request hasn't been answered yet: stop
                        // waiting for it.
                        None if answer == VerificationAnswer::Cancel => state.close_dialog(),
                        None => {}
                    }
                }
                true
            }
            Dialog::SetUpRecovery => {
                match ch {
                    'y' | '\n' if !state.working => {
                        let (Some(session), Some(account)) = (self.session(), self.account_key())
                        else {
                            return true;
                        };
                        let Some(state) = self.encryption_mut() else {
                            return true;
                        };
                        state.working = true;
                        state.problem = None;
                        state.recovery_reply = Some(session.enable_recovery(&account));
                    }
                    'n' if !state.working => state.close_dialog(),
                    _ if esc && !state.working => state.close_dialog(),
                    _ => {}
                }
                true
            }
            Dialog::RecoveryKey => {
                match ch {
                    'c' => {
                        if let Some(key) = &state.recovery_key {
                            let text = key.expose().to_owned();
                            state.key_copied = true;
                            self.effects.push(Effect::Copy(text));
                            self.status = Some(
                                "Copied your recovery key. Keep it somewhere safe.".to_owned(),
                            );
                        }
                    }
                    'w' => {
                        state.close_dialog();
                        self.status = Some("Recovery is set up.".to_owned());
                    }
                    '\n' if state.key_copied => {
                        state.close_dialog();
                        self.status = Some("Recovery is set up.".to_owned());
                    }
                    '\n' | '\u{1b}' => {
                        self.status = Some(
                            "Copy the key with c, or write it down and press w, before you \
                             close this."
                                .to_owned(),
                        );
                    }
                    _ => {}
                }
                true
            }
            Dialog::None if state.takes_over() && !state.asks_for_password() => {
                match ch {
                    'd' => self.verify_this_device(),
                    'r' if state.status.can_use_recovery_key() => {
                        state.page = Page::RecoveryKey;
                        state.problem = None;
                    }
                    's' => state.dialog = Dialog::SkipWarning,
                    _ if esc => state.dialog = Dialog::SkipWarning,
                    _ => return false,
                }
                true
            }
            Dialog::None => false,
        }
    }

    /// The command bar while an encryption screen or dialog has the keys.
    pub(crate) fn encryption_commands(&self) -> Option<Vec<CommandEntry>> {
        let state = self.encryption_ref()?;
        let answer = |key: &str, label: &'static str, ch: char| {
            CommandEntry::with(key, label, Cmd::Answer(ch))
        };
        Some(match state.dialog {
            Dialog::SkipWarning => vec![answer("Y", "Skip", 'y'), answer("N", "Back", 'n')],
            Dialog::Verification => match &state.step {
                VerificationStep::Incoming => {
                    vec![answer("Y", "Accept", 'y'), answer("N", "Decline", 'n')]
                }
                VerificationStep::Ready { .. } => {
                    vec![answer("E", "Emoji", 'e'), answer("N", "Cancel", 'n')]
                }
                VerificationStep::CompareEmoji(_) => {
                    vec![answer("Y", "TheyMatch", 'y'), answer("N", "NoMatch", 'n')]
                }
                VerificationStep::Scanned => {
                    vec![answer("Y", "Yes", 'y'), answer("N", "No", 'n')]
                }
                step if step.is_final() => vec![answer("Enter", "Close", '\n')],
                _ => vec![answer("N", "Cancel", 'n')],
            },
            Dialog::SetUpRecovery if state.working => vec![CommandEntry::of(Cmd::Quit)],
            Dialog::SetUpRecovery => vec![answer("Y", "MakeKey", 'y'), answer("N", "NotNow", 'n')],
            Dialog::RecoveryKey => {
                let mut entries = vec![answer("C", "Copy", 'c'), answer("W", "WroteIt", 'w')];
                if state.key_copied {
                    entries.push(answer("Enter", "Done", '\n'));
                }
                entries
            }
            Dialog::None if state.takes_text() => vec![
                CommandEntry::with("Enter", "OK", Cmd::Select),
                CommandEntry::with("Esc", "Back", Cmd::GoBack),
                CommandEntry::with("^C", "Quit", Cmd::Quit),
            ],
            Dialog::None if state.takes_over() => {
                let mut entries = vec![CommandEntry::of(Cmd::UseDevice)];
                if state.status.can_use_recovery_key() {
                    entries.push(CommandEntry::of(Cmd::UseRecoveryKey));
                }
                entries.extend([
                    CommandEntry::of(Cmd::Skip),
                    CommandEntry::of(Cmd::Palette),
                    CommandEntry::of(Cmd::Quit),
                ]);
                entries
            }
            Dialog::None => return None,
        })
    }

    /// A command while an encryption screen or dialog has the keys (the
    /// bar's, or the palette's).
    pub(crate) fn encryption_run(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Answer(ch) => self.encryption_answer(ch),
            Cmd::UseDevice => self.encryption_answer('d'),
            Cmd::UseRecoveryKey => self.encryption_answer('r'),
            Cmd::Skip => self.encryption_answer('s'),
            Cmd::Select
                if self
                    .encryption_ref()
                    .is_some_and(EncryptionState::takes_text) =>
            {
                self.submit_secret();
                true
            }
            Cmd::GoBack
                if self
                    .encryption_ref()
                    .is_some_and(EncryptionState::takes_text) =>
            {
                self.encryption_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                ))
            }
            _ => false,
        }
    }
}
