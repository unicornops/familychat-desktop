//! `SigninView`: the sign-in screens (PLAN.md, sections 6.2 and 6.7).
//!
//! The view draws and answers; `familychat-signin` decides (ADR 0006). It
//! has three pages:
//!
//! - **Welcome**: "Sign in with a code from your parent" first, then "Sign
//!   in with your password" and "Use another Matrix server".
//! - **Code**: paste the sign-in link, or paste or drop the QR code image.
//!   An image is read here, on this computer (`crate::qr`); the text it
//!   holds becomes `SigninStart::Link`, exactly as a pasted link does. The
//!   link field is masked and its text taken out of it once it is used.
//! - **Flow**: the current [`SigninStep`] of a started flow: the
//!   confirmation ("Sign in as …?", always asked before a code is
//!   redeemed: the only place this view sends `SigninAnswer::Continue`),
//!   the work in progress, the password form (with the used-or-expired
//!   message and the rate-limit countdown from
//!   [`SigninProblem::retry_after`](familychat_core::signin::SigninProblem)),
//!   and the final steps.
//!
//! Phase 14 (ADR 0010) adds "Use another Matrix server"'s server step, the
//! server's ways to sign in on the form (only those it offers: "Continue in
//! your browser", "Sign in with a QR code", the password), the "Continue in
//! your browser?" dialog with the full address (the only place the view
//! sends `Continue` for a browser), the wait while the browser is open (the
//! view opens the address once, when that step arrives, and again only if
//! asked), the QR code to scan and the check code, and the end of signing a
//! new device in ([`SigninStart::LinkDevice`], shown over the rooms).
//!
//! Nothing here retries a sign-in, chooses a server or reads a link's
//! parameters. A flow that ends `Finished` is reported to the shell with
//! [`SigninEvent::Finished`].
//!
//! A link a second launch forwards while an account is signed in opens a
//! sign-in that returns to the rooms ([`SigninView::returning_to_rooms`]):
//! where it would go back to the welcome page (Cancel, Back, "Already
//! signed in"), it reports [`SigninEvent::Closed`] instead.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use familychat_core::{
    commands::command,
    identity::APP_NAME,
    session::AccountKey,
    signin::{
        BrowserKind, BrowserPurpose, BrowserStep, CheckCodeForm, ConfirmBrowser, Finished,
        PasswordForm, PasswordKind, QrCodeStep, QrPurpose, SecretText, ServerForm, SigninAnswer,
        SigninApi, SigninFlow, SigninStart, SigninStep, Working,
    },
};
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClipboardEntry, Context, Entity, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, FontWeight, IntoElement, Render, Role, SharedString,
    Subscription, Task, Transformation, Window, div, percentage, prelude::*, px, rgb,
};
use zeroize::Zeroize;

use crate::a11y::{AccessibleExt, Politeness, announcement_key, live_region, readable};
use crate::actions::{
    GoBack, PasteSignInCode, SignInWithCode, SignInWithPassword, UseAnotherServer,
};
use crate::components::text_input::Paste;
use crate::components::{
    Avatar, Banner, BannerTone, Button, ButtonSize, ComponentTheme, HitTargetExt, Kbd, Modal,
    ModalState, TextInput, TextInputEvent,
};
use crate::encryption::qr_code;
use crate::qr::{self, QrError};
use crate::styles::{
    ICON_SIZE_LG, ICON_SIZE_MD, ICON_SIZE_SM, RADIUS_LG, RADIUS_XL, SPACE_1, SPACE_2, SPACE_3,
    SPACE_4, SPACE_6, SPACE_8, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE,
    TypeStyleExt, UiColors, contrast::mix,
};
use crate::widgets::icon;

/// Key context of the sign-in screens.
pub(crate) const KEY_CONTEXT: &str = "Signin";

/// The card the sign-in pages sit in.
pub(crate) const CARD_WIDTH: f32 = 480.0;

/// What the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    /// The three ways to sign in.
    Welcome,
    /// The code page.
    Code,
    /// A started flow's current step.
    Flow,
}

/// What the view tells the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SigninEvent {
    /// A flow signed an account in.
    Finished(Finished),
    /// A sign-in that returns to the rooms is done without a new account.
    Closed,
}

/// The sign-in screens.
pub(crate) struct SigninView {
    signin: Arc<dyn SigninApi>,
    focus_handle: FocusHandle,
    page: Page,
    /// What went wrong on the code page (not a link, no QR code).
    code_problem: Option<SharedString>,
    /// An image is being read.
    reading_image: bool,
    link_input: Entity<TextInput>,
    server_input: Entity<TextInput>,
    username_input: Entity<TextInput>,
    password_input: Entity<TextInput>,
    /// The other device's two-digit check code (ADR 0010).
    check_code_input: Entity<TextInput>,
    /// The latest form with the server's methods, drawn behind the
    /// browser dialog.
    last_form: Option<PasswordForm>,
    /// The person said Continue on "Continue in your browser?" in this
    /// view: the browser step that follows opens its address, once.
    browser_confirmed: bool,
    /// The running flow; dropping it cancels it.
    flow: Option<SigninFlow>,
    /// Its latest step.
    step: SigninStep,
    /// The confirmation dialog, open while the step is `Confirm`.
    confirm_modal: ModalState,
    /// While a rate limit lasts: when the server said to try again.
    retry_at: Option<Instant>,
    flow_task: Task<()>,
    image_task: Task<()>,
    countdown_task: Task<()>,
    /// Opened over the rooms (a forwarded link): ends by going back to
    /// them rather than to the welcome page.
    returns_to_rooms: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SigninEvent> for SigninView {}

impl Focusable for SigninView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SigninView {
    /// The welcome page over `signin`.
    pub(crate) fn new(
        signin: Arc<dyn SigninApi>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let link_input = cx.new(|cx| {
            TextInput::new(cx)
                .masked()
                .label("Sign-in link")
                .leading_icon("link")
                .placeholder("Paste the sign-in link")
        });
        let server_input = cx.new(|cx| TextInput::new(cx).leading_icon("house"));
        let username_input = cx.new(|cx| {
            TextInput::new(cx)
                .label("Username or Matrix ID")
                .leading_icon("user")
                .placeholder("ana, or @ana:yourfamily.safechat.family")
        });
        let password_input = cx.new(|cx| {
            TextInput::new(cx)
                .masked()
                .label("Password")
                .leading_icon("lock")
                .placeholder("Password")
        });
        let check_code_input = cx.new(|cx| {
            TextInput::new(cx)
                .label("Check code")
                .leading_icon("key-round")
                .placeholder("Two numbers, like 07")
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &link_input,
                window,
                |this, _, event, window, cx| match event {
                    TextInputEvent::Submitted => this.use_link(window, cx),
                    TextInputEvent::Changed => cx.notify(),
                },
            ),
            cx.subscribe_in(&password_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    this.submit_password(window, cx);
                }
            }),
            cx.subscribe_in(&username_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    this.submit_password(window, cx);
                }
            }),
            cx.subscribe_in(&server_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    if matches!(this.step, SigninStep::Server(_)) {
                        this.choose_server(window, cx);
                    } else {
                        this.submit_password(window, cx);
                    }
                }
            }),
            cx.subscribe_in(&check_code_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    this.submit_check_code(window, cx);
                }
            }),
        ];
        let focus_handle = cx.focus_handle();
        Self {
            signin,
            focus_handle,
            page: Page::Welcome,
            code_problem: None,
            reading_image: false,
            link_input,
            server_input,
            username_input,
            password_input,
            check_code_input,
            last_form: None,
            browser_confirmed: false,
            flow: None,
            step: SigninStep::Starting,
            confirm_modal: ModalState::new(cx),
            retry_at: None,
            flow_task: Task::ready(()),
            image_task: Task::ready(()),
            countdown_task: Task::ready(()),
            returns_to_rooms: false,
            _subscriptions: subscriptions,
        }
    }

    /// A sign-in opened over the rooms, which goes back to them instead of
    /// the welcome page.
    pub(crate) fn returning_to_rooms(mut self) -> Self {
        self.returns_to_rooms = true;
        self
    }

    /// Whether a code or a password is being used right now (a new link
    /// must not interrupt it).
    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self.step,
            SigninStep::Working(_)
                | SigninStep::ConfirmBrowser(_)
                | SigninStep::Browser(_)
                | SigninStep::QrCode(_)
                | SigninStep::CheckCode(_)
        ) || self.reading_image
    }

    /// What the view shows.
    #[cfg(test)]
    pub(crate) fn page(&self) -> Page {
        self.page
    }

    /// The flow's latest step.
    #[cfg(test)]
    pub(crate) fn step(&self) -> &SigninStep {
        &self.step
    }

    /// What went wrong on the code page.
    #[cfg(test)]
    pub(crate) fn code_problem(&self) -> Option<&str> {
        self.code_problem.as_deref()
    }

    /// The link field (tests).
    #[cfg(test)]
    pub(crate) fn link_input(&self) -> &Entity<TextInput> {
        &self.link_input
    }

    /// The password field (tests).
    #[cfg(test)]
    pub(crate) fn password_input(&self) -> &Entity<TextInput> {
        &self.password_input
    }

    /// Whether an image is being read.
    #[cfg(test)]
    pub(crate) fn is_reading_image(&self) -> bool {
        self.reading_image
    }

    /// Moves keyboard focus into the view.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    // -- Pages -------------------------------------------------------------

    /// Back to the welcome page, ending any flow.
    pub(crate) fn show_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_flow(window, cx);
        if self.returns_to_rooms {
            cx.emit(SigninEvent::Closed);
            return;
        }
        self.page = Page::Welcome;
        self.code_problem = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// The code page, with `problem` shown on it.
    pub(crate) fn show_code(
        &mut self,
        problem: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_flow(window, cx);
        self.page = Page::Code;
        self.code_problem = problem;
        self.link_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// The code page after an image that held no sign-in code.
    pub(crate) fn show_image_problem(
        &mut self,
        error: QrError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_code(Some(error.message().into()), window, cx);
    }

    fn end_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping the flow cancels it; a code not yet redeemed is wiped
        // unsent.
        self.flow = None;
        self.flow_task = Task::ready(());
        self.step = SigninStep::Starting;
        self.last_form = None;
        self.browser_confirmed = false;
        self.retry_at = None;
        self.countdown_task = Task::ready(());
        if self.confirm_modal.is_open() {
            self.confirm_modal.close(window, cx);
        }
    }

    // -- Starting flows ----------------------------------------------------

    /// Starts a flow and follows its steps.
    pub(crate) fn start(
        &mut self,
        start: SigninStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_flow(window, cx);
        self.reading_image = false;
        self.image_task = Task::ready(());
        self.password_input
            .update(cx, |input, cx| input.take_secret(cx).zeroize());
        let flow = self.signin.start(start);
        let mut steps = flow.steps();
        self.flow = Some(flow);
        self.page = Page::Flow;
        self.code_problem = None;
        let first = steps.borrow_and_update().clone();
        self.show_step(first, window, cx);
        self.flow_task = cx.spawn_in(window, async move |this, cx| {
            while steps.changed().await.is_ok() {
                let step = steps.borrow_and_update().clone();
                let done = step.is_final();
                if this
                    .update_in(cx, |this, window, cx| this.show_step(step, window, cx))
                    .is_err()
                    || done
                {
                    break;
                }
            }
        });
        cx.notify();
    }

    /// "Sign in with a code from your parent".
    pub(crate) fn sign_in_with_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Code {
            self.show_code(None, window, cx);
        }
    }

    /// "Sign in with your password".
    pub(crate) fn sign_in_with_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start(SigninStart::FamilyChatPassword, window, cx);
    }

    /// "Use another Matrix server".
    pub(crate) fn use_another_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start(SigninStart::OtherServer, window, cx);
    }

    /// "Sign in a new device" for `account` (shown over the rooms).
    pub(crate) fn link_device(
        &mut self,
        account: AccountKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start(SigninStart::LinkDevice(account), window, cx);
    }

    /// The link field's text, as a sign-in link. The field is emptied.
    pub(crate) fn use_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Code {
            return;
        }
        let text = self
            .link_input
            .update(cx, |input, cx| input.take_secret(cx));
        if text.trim().is_empty() {
            return;
        }
        self.start(SigninStart::Link(SecretText::new(text)), window, cx);
    }

    /// Pastes from the clipboard: an image is read for its QR code, a
    /// copied image file likewise, and text is used as a link.
    pub(crate) fn paste_signin_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_modal.is_open() {
            return;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let bytes = image.bytes.clone();
                    self.read_image(ImageSource::Bytes(bytes), window, cx);
                    return;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    if let Some(path) = paths.paths().first() {
                        self.read_image(ImageSource::File(path.clone()), window, cx);
                        return;
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if let Some(text) = item.text().filter(|text| !text.trim().is_empty()) {
            self.start(SigninStart::Link(SecretText::new(text)), window, cx);
        }
    }

    /// Secondary+V in the link field: an image on the clipboard is read
    /// for its QR code; text pastes into the field as usual.
    fn paste_into_link_field(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let holds_image = cx.read_from_clipboard().is_some_and(|item| {
            item.entries().iter().any(|entry| {
                matches!(
                    entry,
                    ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
                )
            })
        });
        if holds_image {
            cx.stop_propagation();
            self.paste_signin_code(window, cx);
        }
    }

    /// Files dropped on the window: the first is read for its QR code.
    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_modal.is_open() || matches!(self.step, SigninStep::Working(_)) {
            return;
        }
        if let Some(path) = paths.paths().first() {
            self.read_image(ImageSource::File(path.clone()), window, cx);
        }
    }

    /// Reads `source` on the background executor and, if it holds a QR
    /// code, signs in with its text.
    pub(crate) fn read_image(
        &mut self,
        source: ImageSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Code {
            self.show_code(None, window, cx);
        }
        self.reading_image = true;
        self.code_problem = None;
        cx.notify();
        let read = cx.background_spawn(async move { source.decode() });
        self.image_task = cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.reading_image = false;
                match result {
                    Ok(text) => this.start(SigninStart::Link(SecretText::new(text)), window, cx),
                    Err(error) => this.show_image_problem(error, window, cx),
                }
            });
        });
    }

    // -- Answers -------------------------------------------------------------

    fn answer(&self, answer: SigninAnswer) {
        if let Some(flow) = &self.flow {
            flow.answer(answer);
        }
    }

    /// Continue on the confirmation: the only place this view lets a code
    /// be redeemed, or a browser open, and only while the confirmation is
    /// showing.
    pub(crate) fn confirm_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.step,
            SigninStep::Confirm(_) | SigninStep::ConfirmBrowser(_)
        ) || !self.confirm_modal.is_open()
        {
            return;
        }
        self.browser_confirmed = matches!(self.step, SigninStep::ConfirmBrowser(_));
        self.confirm_modal.close(window, cx);
        self.answer(SigninAnswer::Continue);
        cx.notify();
    }

    /// "Use my password instead" on the confirmation.
    pub(crate) fn decline_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.step, SigninStep::Confirm(_)) {
            return;
        }
        self.confirm_modal.close(window, cx);
        self.answer(SigninAnswer::UsePassword);
    }

    /// Cancel: ends the flow (the sign-in shows `Cancelled`, which goes
    /// back to the welcome page).
    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.flow.is_some() && !self.step.is_final() {
            if self.confirm_modal.is_open() {
                self.confirm_modal.close(window, cx);
            }
            self.answer(SigninAnswer::Cancel);
        } else {
            self.show_welcome(window, cx);
        }
    }

    /// Back (Escape): one step towards the welcome page.
    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.page {
            Page::Welcome => {}
            Page::Code => self.show_welcome(window, cx),
            Page::Flow => match self.step {
                SigninStep::Working(_) => {}
                _ => self.cancel(window, cx),
            },
        }
    }

    /// "Use another Matrix server"'s Continue: finds the typed server.
    pub(crate) fn choose_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.step, SigninStep::Server(_)) {
            return;
        }
        let server = self.server_input.read(cx).text().to_owned();
        self.answer(SigninAnswer::ChooseServer { server });
        self.focus(window, cx);
        cx.notify();
    }

    /// "Change server": back to the server step.
    pub(crate) fn change_server(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.step, SigninStep::Password(form) if form.server_is_fixed()) {
            self.answer(SigninAnswer::ChangeServer);
            cx.notify();
        }
    }

    /// "Continue in your browser": the flow asks first.
    pub(crate) fn use_browser(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.step, SigninStep::Password(form) if form.methods.as_ref().is_some_and(|methods| methods.browser.is_some()))
        {
            self.answer(SigninAnswer::UseBrowser);
            cx.notify();
        }
    }

    /// "Sign in with a QR code".
    pub(crate) fn use_qr_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.step, SigninStep::Password(form) if form.methods.as_ref().is_some_and(|methods| methods.qr_code))
        {
            self.answer(SigninAnswer::UseQrCode);
            cx.notify();
        }
    }

    /// The check code field's Continue. The field is emptied.
    pub(crate) fn submit_check_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.step, SigninStep::CheckCode(_)) {
            return;
        }
        let code = self
            .check_code_input
            .update(cx, |input, cx| input.take_secret(cx));
        self.answer(SigninAnswer::SubmitCheckCode(code));
        self.focus(window, cx);
        cx.notify();
    }

    /// Opens the browser step's address again, if the person asks.
    pub(crate) fn open_browser_again(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let SigninStep::Browser(browser) = &self.step {
            cx.open_url(browser.url.expose());
        }
    }

    /// The check code field (tests).
    #[cfg(test)]
    pub(crate) fn check_code_input(&self) -> &Entity<TextInput> {
        &self.check_code_input
    }

    /// The server field (tests).
    #[cfg(test)]
    pub(crate) fn server_input(&self) -> &Entity<TextInput> {
        &self.server_input
    }

    /// Signs in with the password form, unless a rate limit is still
    /// running.
    pub(crate) fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.step, SigninStep::Password(_)) || self.seconds_to_wait().is_some() {
            return;
        }
        let server = self.server_input.read(cx).text().to_owned();
        let username = self.username_input.read(cx).text().to_owned();
        let password = self
            .password_input
            .update(cx, |input, cx| input.take_secret(cx));
        self.answer(SigninAnswer::SubmitPassword {
            server,
            username,
            password: SecretText::new(password),
        });
        self.focus(window, cx);
        cx.notify();
    }

    // -- Steps ---------------------------------------------------------------

    fn show_step(&mut self, step: SigninStep, window: &mut Window, cx: &mut Context<Self>) {
        match &step {
            SigninStep::Confirm(_) | SigninStep::ConfirmBrowser(_) => {
                self.page = Page::Flow;
                if !self.confirm_modal.is_open() {
                    self.confirm_modal.open(window, cx);
                }
            }
            SigninStep::Password(form) => {
                let first = !matches!(&self.step, SigninStep::Password(previous) if previous.kind == form.kind && previous.methods == form.methods);
                if form.methods.is_some() {
                    self.last_form = Some(form.clone());
                }
                self.fill_form(form, first, window, cx);
            }
            SigninStep::Server(form) => {
                let first = !matches!(self.step, SigninStep::Server(_));
                self.fill_server(form, first, window, cx);
            }
            SigninStep::Browser(browser) => {
                // Opened once, when the step arrives, and only after the
                // person's own Continue in the dialog that showed the
                // address.
                if std::mem::take(&mut self.browser_confirmed) {
                    cx.open_url(browser.url.expose());
                }
                self.focus(window, cx);
            }
            SigninStep::CheckCode(_) => {
                self.check_code_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            SigninStep::QrCode(_) => self.focus(window, cx),
            SigninStep::Finished(finished) => cx.emit(SigninEvent::Finished(finished.clone())),
            SigninStep::NotALink(message) => {
                let message = SharedString::from(message.clone());
                self.show_code(Some(message), window, cx);
                return;
            }
            SigninStep::Cancelled => {
                self.show_welcome(window, cx);
                return;
            }
            SigninStep::Starting
            | SigninStep::Working(_)
            | SigninStep::AlreadySignedIn(_)
            | SigninStep::DeviceLinked(_)
            | SigninStep::Stopped(_) => {}
        }
        if !matches!(step, SigninStep::Confirm(_) | SigninStep::ConfirmBrowser(_))
            && self.confirm_modal.is_open()
        {
            self.confirm_modal.close(window, cx);
        }
        self.step = step;
        cx.notify();
    }

    /// Puts the form's fields into the inputs, starts the countdown of a
    /// rate limit, and focuses the first field to fill.
    fn fill_form(
        &mut self,
        form: &PasswordForm,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (label, icon_name) = match form.kind {
            PasswordKind::FamilyChat => ("Your family's address", "house"),
            PasswordKind::OtherServer => ("Homeserver", "server"),
        };
        let placeholder = form.server_placeholder.clone();
        let server = form.server.clone();
        self.server_input.update(cx, |input, cx| {
            input.set_label(label, cx);
            input.set_leading_icon(icon_name, cx);
            input.set_placeholder(placeholder, cx);
            if first || input.text() != server {
                input.set_text(server, cx);
            }
        });
        if first || self.username_input.read(cx).text() != form.username {
            let username = form.username.clone();
            self.username_input
                .update(cx, |input, cx| input.set_text(username, cx));
        }
        self.retry_at = form
            .problem
            .as_ref()
            .and_then(|problem| problem.retry_after)
            .map(|wait| Instant::now() + wait);
        self.start_countdown(window, cx);
        let target = if form.server.is_empty() {
            &self.server_input
        } else if form.username.is_empty() {
            &self.username_input
        } else {
            &self.password_input
        };
        target.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Puts the server step's field into the server input.
    fn fill_server(
        &mut self,
        form: &ServerForm,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = form.server_placeholder.clone();
        let server = form.server.clone();
        self.server_input.update(cx, |input, cx| {
            input.set_label("Homeserver", cx);
            input.set_leading_icon("server", cx);
            input.set_placeholder(placeholder, cx);
            if first || input.text() != server {
                input.set_text(server, cx);
            }
            input.focus(window, cx);
        });
    }

    /// Whole seconds left of a rate limit, while one is running.
    pub(crate) fn seconds_to_wait(&self) -> Option<u64> {
        let retry_at = self.retry_at?;
        let left = retry_at.saturating_duration_since(Instant::now());
        (!left.is_zero()).then(|| left.as_secs_f64().ceil() as u64)
    }

    fn start_countdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_at.is_none() {
            self.countdown_task = Task::ready(());
            return;
        }
        self.countdown_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let running = this.seconds_to_wait().is_some();
                        if !running {
                            this.retry_at = None;
                        }
                        running
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        });
    }

    // -- Rendering -------------------------------------------------------------

    fn render_welcome(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_6))
            .child(brand(colors))
            .child(
                div()
                    .id("signin-options")
                    .accessible(Role::Group, "Ways to sign in")
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_3))
                    .child(option_row(
                        "signin-option-code",
                        "SignInWithCode",
                        "Ask your parent for a sign-in code, then paste the link or the QR code \
                         image.",
                        true,
                        cx.listener(|this, _, window, cx| this.sign_in_with_code(window, cx)),
                        colors,
                    ))
                    .child(option_row(
                        "signin-option-password",
                        "SignInWithPassword",
                        "Use your family's address, your username and your password.",
                        false,
                        cx.listener(|this, _, window, cx| this.sign_in_with_password(window, cx)),
                        colors,
                    ))
                    .child(option_row(
                        "signin-option-other",
                        "UseAnotherServer",
                        "Sign in to any Matrix homeserver.",
                        false,
                        cx.listener(|this, _, window, cx| this.use_another_server(window, cx)),
                        colors,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(SPACE_2))
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(icon("shield-check", ICON_SIZE_SM, colors.muted))
                    .child(readable(
                        "signin-privacy",
                        format!("{APP_NAME} talks only to your family's server."),
                    )),
            )
            .into_any_element()
    }

    fn render_code(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let has_link = !self.link_input.read(cx).text().trim().is_empty();
        let reading = self.reading_image;
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(back_button(cx))
            .child(heading(
                "signin-code-heading",
                "Sign in with a code from your parent",
                "Your parent can show a sign-in QR code in the Family Chat panel. Copy the image \
                 or take a screenshot of it, then paste it here or drop the file on this window.",
                colors,
            ))
            .when_some(self.code_problem.clone(), |this, problem| {
                this.child(announce_problem("signin-code-problem-alert", &problem))
                    .child(Banner::new("signin-code-problem", problem).tone(BannerTone::Danger))
            })
            .child(
                div()
                    .id("signin-drop-zone")
                    .accessible(Role::Group, "QR code image")
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(SPACE_3))
                    .px(px(SPACE_6))
                    .py(px(SPACE_6))
                    .rounded(px(RADIUS_LG))
                    .border_2()
                    .border_dashed()
                    .border_color(rgb(colors.control_border))
                    .bg(rgb(mix(colors.surface, colors.accent, 0.04)))
                    .drag_over::<ExternalPaths>({
                        let accent = colors.accent;
                        let tint = mix(colors.surface, colors.accent, 0.12);
                        move |style, _, _, _| style.border_color(rgb(accent)).bg(rgb(tint))
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        this.drop_files(paths, window, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(56.0))
                            .rounded_full()
                            .bg(rgb(mix(colors.surface, colors.accent, 0.14)))
                            .child(if reading {
                                spinner("signin-reading-spinner", ICON_SIZE_LG, colors.accent)
                            } else {
                                icon("scan-qr-code", ICON_SIZE_LG, colors.accent).into_any_element()
                            }),
                    )
                    .child(div().type_style(TYPE_BODY_STRONG).child(if reading {
                        "Reading the QR code…"
                    } else {
                        "Paste or drop the QR code image"
                    }))
                    .child(
                        div()
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child("The image is read on this computer and never uploaded."),
                    )
                    .child(
                        Button::new("signin-paste", "Paste from clipboard")
                            .icon("clipboard-paste")
                            .disabled(reading)
                            .shortcut(primary_key("PasteSignInCode"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.paste_signin_code(window, cx);
                            })),
                    ),
            )
            .child(divider("or", colors))
            .child(
                div()
                    .id("signin-link-form")
                    .accessible(Role::Form, "Sign-in link")
                    .capture_action(cx.listener(Self::paste_into_link_field))
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(field_label("Sign-in link", colors))
                    .child(
                        div()
                            .flex()
                            .gap(px(SPACE_2))
                            .child(div().flex_1().min_w_0().child(self.link_input.clone()))
                            .child(
                                Button::new("signin-link-continue", "Continue")
                                    .primary()
                                    .disabled(!has_link || reading)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.use_link(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child(
                                "The link is hidden while you type, because it holds your \
                                 code.",
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_password(
        &self,
        form: &PasswordForm,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, detail) = match (form.kind, &form.methods) {
            (PasswordKind::FamilyChat, _) => (
                "Sign in with your password",
                "Type your family's address, then the username and password your parent set up \
                 for you.",
            ),
            (PasswordKind::OtherServer, None) => (
                "Use another Matrix server",
                "Sign in to any Matrix homeserver with your username and password.",
            ),
            (PasswordKind::OtherServer, Some(_)) => (
                "Use another Matrix server",
                "Choose how to sign in. Only the ways this server offers are shown.",
            ),
        };
        let shows_password = form.shows_password();
        let methods = form.methods.clone();
        let browser = methods.as_ref().and_then(|methods| methods.browser.clone());
        let qr = methods.as_ref().is_some_and(|methods| methods.qr_code);
        let wait = self.seconds_to_wait();
        let submit_label: SharedString = match wait {
            Some(seconds) => format!("Try again in {seconds} s").into(),
            None => "Sign in".into(),
        };
        div()
            .id("signin-password-form")
            .accessible(Role::Form, title)
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(back_button(cx))
            .child(heading("signin-password-heading", title, detail, colors))
            .when_some(form.notice.clone(), |this, notice| {
                this.child(Banner::new("signin-password-notice", notice).tone(BannerTone::Info))
            })
            .when_some(form.problem.clone(), |this, problem| {
                let tone = if problem.retry_after.is_some() {
                    BannerTone::Caution
                } else {
                    BannerTone::Danger
                };
                this.child(announce_problem(
                    "signin-password-problem-alert",
                    &problem.message,
                ))
                .child(Banner::new("signin-password-problem", problem.message.clone()).tone(tone))
            })
            .child(if form.server_is_fixed() {
                fixed_server(&form.server, colors, cx)
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(field_label(
                        match form.kind {
                            PasswordKind::FamilyChat => "Your family's address",
                            PasswordKind::OtherServer => "Homeserver",
                        },
                        colors,
                    ))
                    .child(self.server_input.clone())
                    .into_any_element()
            })
            .when_some(browser, |this, browser| {
                this.child(method_button(
                    "signin-use-browser",
                    "Continue in your browser",
                    "globe",
                    match browser.kind {
                        BrowserKind::OAuth => format!("You sign in at {}", browser.address),
                        BrowserKind::Sso => {
                            format!("Your server's single sign-on, at {}", browser.address)
                        }
                    },
                    !shows_password,
                    cx.listener(|this, _, window, cx| this.use_browser(window, cx)),
                    colors,
                ))
            })
            .when(qr, |this| {
                this.child(method_button(
                    "signin-use-qr",
                    "Sign in with a QR code",
                    "scan-qr-code",
                    "A phone where you're signed in scans it".to_owned(),
                    false,
                    cx.listener(|this, _, window, cx| this.use_qr_code(window, cx)),
                    colors,
                ))
            })
            .when(
                shows_password
                    && (form
                        .methods
                        .as_ref()
                        .is_some_and(|m| m.browser.is_some() || m.qr_code)),
                |this| this.child(divider("or", colors)),
            )
            .when(shows_password, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_2))
                        .child(field_label("Username or Matrix ID", colors))
                        .child(self.username_input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_2))
                        .child(field_label("Password", colors))
                        .child(self.password_input.clone()),
                )
                .child(
                    Button::new("signin-submit", submit_label)
                        .primary()
                        .size(ButtonSize::Large)
                        .full_width()
                        .disabled(wait.is_some())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_password(window, cx)),
                        ),
                )
            })
            .when_some(wait, |this, seconds| {
                // Shown, not announced: the problem banner said it once,
                // and a screen reader reading every second would be noise.
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(SPACE_2))
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.warning))
                        .child(icon("clock", ICON_SIZE_SM, colors.warning))
                        .child(readable(
                            "signin-wait",
                            format!("You can sign in again in {seconds} seconds."),
                        )),
                )
            })
            .into_any_element()
    }

    fn render_server(
        &self,
        form: &ServerForm,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("signin-server-form")
            .accessible(Role::Form, "Use another Matrix server")
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(back_button(cx))
            .child(heading(
                "signin-server-heading",
                "Use another Matrix server",
                "Type the server's address. Family Chat then shows the ways it offers to sign in.",
                colors,
            ))
            .when_some(form.problem.clone(), |this, problem| {
                this.child(announce_problem(
                    "signin-server-problem-alert",
                    &problem.message,
                ))
                .child(
                    Banner::new("signin-server-problem", problem.message.clone())
                        .tone(BannerTone::Danger),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(field_label("Homeserver", colors))
                    .child(self.server_input.clone()),
            )
            .child(
                Button::new("signin-server-continue", "Continue")
                    .primary()
                    .size(ButtonSize::Large)
                    .full_width()
                    .on_click(cx.listener(|this, _, window, cx| this.choose_server(window, cx))),
            )
            .into_any_element()
    }

    fn render_browser(
        &self,
        browser: &BrowserStep,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = match browser.purpose {
            BrowserPurpose::SignIn => "Waiting for your browser",
            BrowserPurpose::ApproveDevice => "Approve the new device",
        };
        div()
            .id("signin-browser")
            .accessible(Role::Status, title)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_4))
            .py(px(SPACE_6))
            .text_center()
            .child(spinner(
                "signin-browser-spinner",
                ICON_SIZE_LG,
                colors.accent,
            ))
            .child(
                div()
                    .id("signin-browser-heading")
                    .accessible(Role::Heading, title)
                    .aria_level(1)
                    .type_style(TYPE_HEADLINE)
                    .child(title),
            )
            .child(
                div()
                    .type_style(TYPE_BODY)
                    .text_color(rgb(colors.muted))
                    .child(readable("signin-browser-message", browser.message.clone())),
            )
            .child(address_box(
                "signin-browser-address",
                &browser.address,
                colors,
            ))
            .child(
                div()
                    .flex()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new("signin-browser-again", "Open the browser again")
                            .icon("external-link")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_browser_again(window, cx);
                            })),
                    )
                    .child(
                        Button::new("signin-browser-cancel", "Cancel")
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_qr(
        &self,
        step: &QrCodeStep,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, detail): (&'static str, String) = match step.purpose {
            QrPurpose::SignIn => (
                "Sign in with a QR code",
                format!(
                    "On a phone where you're signed in to {}, choose to sign in another device, \
                     then scan this code.",
                    step.server
                ),
            ),
            QrPurpose::LinkDevice => (
                "Sign in a new device",
                "On the new phone, choose to sign in with a QR code, then scan this code. It \
                 gets your keys from this computer."
                    .to_owned(),
            ),
        };
        div()
            .id("signin-qr-page")
            .accessible(Role::Group, title)
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(
                        div()
                            .id("signin-qr-heading")
                            .accessible(Role::Heading, title)
                            .aria_level(1)
                            .type_style(TYPE_HEADLINE)
                            .child(title),
                    )
                    .child(
                        div()
                            .type_style(TYPE_BODY)
                            .text_color(rgb(colors.muted))
                            .child(readable("signin-qr-detail", detail)),
                    ),
            )
            .child(qr_code(
                "signin-qr",
                "A QR code for your other device to scan",
                &step.qr,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(SPACE_2))
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(icon("clock", ICON_SIZE_SM, colors.muted))
                    .child("The code works once, for a few minutes."),
            )
            .child(
                div().flex().justify_center().child(
                    Button::new("signin-qr-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_check_code(
        &self,
        form: &CheckCodeForm,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("signin-check-code-form")
            .accessible(Role::Form, "Check code")
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "signin-check-code-heading",
                "Type the code from your other device",
                "Your other device shows two numbers once it has scanned the code. They prove the \
                 two devices are talking to each other.",
                colors,
            ))
            .when_some(form.problem.clone(), |this, problem| {
                this.child(announce_problem(
                    "signin-check-code-problem-alert",
                    &problem,
                ))
                .child(Banner::new("signin-check-code-problem", problem).tone(BannerTone::Danger))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(field_label("Check code", colors))
                    .child(self.check_code_input.clone()),
            )
            .child(
                div()
                    .flex()
                    .gap(px(SPACE_2))
                    .justify_end()
                    .child(
                        Button::new("signin-check-code-cancel", "Cancel")
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        Button::new("signin-check-code-continue", "Continue")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_check_code(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The end of signing a new device in: done, or why it stopped.
    fn render_link_end(
        &self,
        message: &str,
        linked: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, icon_name, fill, tint) = if linked {
            (
                "New device signed in",
                "circle-check",
                colors.success_background,
                colors.success,
            )
        } else {
            (
                "The new device isn't signed in",
                "triangle-alert",
                mix(colors.background, colors.warning, 0.14),
                colors.warning,
            )
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_4))
            .py(px(SPACE_6))
            .text_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(56.0))
                    .rounded_full()
                    .bg(rgb(fill))
                    .child(icon(icon_name, ICON_SIZE_LG, tint)),
            )
            .child(
                div()
                    .id("signin-link-heading")
                    .accessible(Role::Heading, title)
                    .aria_level(1)
                    .type_style(TYPE_HEADLINE)
                    .child(title),
            )
            .child(
                div()
                    .type_style(TYPE_BODY)
                    .text_color(rgb(colors.muted))
                    .child(readable("signin-link-message", message.to_owned())),
            )
            .child(
                Button::new(
                    "signin-link-done",
                    if self.returns_to_rooms {
                        "Back to your rooms"
                    } else {
                        "Back to sign-in"
                    },
                )
                .primary()
                .on_click(cx.listener(|this, _, window, cx| this.show_welcome(window, cx))),
            )
            .into_any_element()
    }

    fn render_working(&self, message: &'static str, colors: &UiColors) -> AnyElement {
        div()
            .id("signin-working")
            .accessible(Role::Status, message)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_4))
            .py(px(SPACE_8))
            .child(brand_mark(colors, 56.0))
            .child(spinner(
                "signin-working-spinner",
                ICON_SIZE_LG,
                colors.accent,
            ))
            .child(div().type_style(TYPE_BODY_STRONG).child(message))
            .child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child("This takes a few seconds."),
            )
            .into_any_element()
    }

    fn render_already_signed_in(
        &self,
        message: &str,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_4))
            .py(px(SPACE_6))
            .text_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(56.0))
                    .rounded_full()
                    .bg(rgb(colors.success_background))
                    .child(icon("circle-check", ICON_SIZE_LG, colors.success)),
            )
            .child(
                div()
                    .id("signin-already-heading")
                    .accessible(Role::Heading, "Already signed in")
                    .aria_level(1)
                    .type_style(TYPE_HEADLINE)
                    .child("Already signed in"),
            )
            .child(
                div()
                    .type_style(TYPE_BODY)
                    .text_color(rgb(colors.muted))
                    .child(readable("signin-already-message", message.to_owned())),
            )
            .child(
                Button::new(
                    "signin-already-ok",
                    if self.returns_to_rooms {
                        "Back to your rooms"
                    } else {
                        "Back to sign-in"
                    },
                )
                .primary()
                .on_click(cx.listener(|this, _, window, cx| this.show_welcome(window, cx))),
            )
            .into_any_element()
    }

    /// "Continue in your browser?": the full address, Continue or Cancel.
    fn render_confirm_browser(
        &self,
        confirm: &ConfirmBrowser,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        Modal::new("signin-confirm-browser", self.confirm_modal.focus_handle())
            .title(confirm.question.clone())
            .description(confirm.detail.clone())
            .persistent()
            .child(address_box(
                "signin-confirm-browser-address",
                &confirm.address,
                &colors,
            ))
            .footer(
                Button::new("signin-confirm-browser-cancel", "Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
            )
            .footer(
                Button::new("signin-confirm-browser-continue", "Continue")
                    .primary()
                    .icon("external-link")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_code(window, cx);
                    })),
            )
            .on_dismiss(cx.listener(|this, _, window, cx| this.cancel(window, cx)))
            .into_any_element()
    }

    fn render_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let SigninStep::ConfirmBrowser(confirm) = &self.step {
            return self
                .confirm_modal
                .is_open()
                .then(|| self.render_confirm_browser(&confirm.clone(), cx));
        }
        let SigninStep::Confirm(confirm) = &self.step else {
            return None;
        };
        if !self.confirm_modal.is_open() {
            return None;
        }
        let colors = ComponentTheme::get(cx).colors;
        let who = confirm
            .user_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| confirm.server.clone());
        let initial = confirm
            .user_id
            .as_ref()
            .map(|user| user.localpart().to_owned())
            .unwrap_or_else(|| confirm.server.clone());
        Some(
            Modal::new("signin-confirm", self.confirm_modal.focus_handle())
                .title(confirm.question.clone())
                .description(confirm.detail.clone())
                .persistent()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_3))
                        .p(px(SPACE_3))
                        .rounded(px(RADIUS_LG))
                        .bg(rgb(colors.surface))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .child(Avatar::new(initial).address(who.clone()).size(40.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .type_style(TYPE_BODY_STRONG)
                                        .child(readable("signin-confirm-who", who)),
                                )
                                .child(
                                    div()
                                        .type_style(TYPE_CAPTION)
                                        .text_color(rgb(colors.muted))
                                        .child(format!("on {}", confirm.server)),
                                ),
                        ),
                )
                .footer(
                    div().flex_1().child(
                        Button::new("signin-confirm-password", "Use my password instead")
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.decline_code(window, cx);
                            })),
                    ),
                )
                .footer(
                    Button::new("signin-confirm-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                )
                .footer(
                    Button::new("signin-confirm-continue", "Continue")
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_code(window, cx);
                        })),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.cancel(window, cx)))
                .into_any_element(),
        )
    }
}

/// Where a QR image comes from.
pub(crate) enum ImageSource {
    /// Bytes from the clipboard.
    Bytes(Vec<u8>),
    /// A dropped or copied file.
    File(PathBuf),
}

impl ImageSource {
    /// Reads and decodes the image; runs on the background executor. The
    /// bytes are wiped once read.
    fn decode(self) -> Result<String, QrError> {
        let mut bytes = match self {
            Self::Bytes(bytes) => bytes,
            Self::File(path) => {
                let size = std::fs::metadata(&path)
                    .map_err(|_| QrError::NotAnImage)?
                    .len();
                if size > qr::MAX_IMAGE_BYTES as u64 {
                    return Err(QrError::TooLarge);
                }
                std::fs::read(&path).map_err(|_| QrError::NotAnImage)?
            }
        };
        let result = qr::decode_signin_qr(&bytes);
        bytes.zeroize();
        result
    }
}

impl Render for SigninView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let body = match (&self.page, &self.step) {
            (Page::Welcome, _) => self.render_welcome(&colors, cx),
            (Page::Code, _) | (Page::Flow, SigninStep::Confirm(_) | SigninStep::Starting) => {
                self.render_code(&colors, cx)
            }
            (Page::Flow, SigninStep::Working(working)) => {
                self.render_working(working.message(), &colors)
            }
            (Page::Flow, SigninStep::Password(form)) => {
                let form = form.clone();
                self.render_password(&form, &colors, cx)
            }
            (Page::Flow, SigninStep::AlreadySignedIn(message)) => {
                let message = message.clone();
                self.render_already_signed_in(&message, &colors, cx)
            }
            (Page::Flow, SigninStep::Server(form)) => {
                let form = form.clone();
                self.render_server(&form, &colors, cx)
            }
            (Page::Flow, SigninStep::ConfirmBrowser(confirm)) => match self.last_form.clone() {
                Some(form) if confirm.purpose == BrowserPurpose::SignIn => {
                    self.render_password(&form, &colors, cx)
                }
                _ if confirm.purpose == BrowserPurpose::SignIn => {
                    self.render_working(Working::PreparingBrowser.message(), &colors)
                }
                _ => self.render_working(Working::WaitingForOtherDevice.message(), &colors),
            },
            (Page::Flow, SigninStep::Browser(browser)) => {
                let browser = browser.clone();
                self.render_browser(&browser, &colors, cx)
            }
            (Page::Flow, SigninStep::QrCode(step)) => {
                let step = step.clone();
                self.render_qr(&step, &colors, cx)
            }
            (Page::Flow, SigninStep::CheckCode(form)) => {
                let form = form.clone();
                self.render_check_code(&form, &colors, cx)
            }
            (Page::Flow, SigninStep::DeviceLinked(message)) => {
                let message = message.clone();
                self.render_link_end(&message, true, &colors, cx)
            }
            (Page::Flow, SigninStep::Stopped(message)) => {
                let message = message.clone();
                self.render_link_end(&message, false, &colors, cx)
            }
            (
                Page::Flow,
                SigninStep::Finished(_) | SigninStep::NotALink(_) | SigninStep::Cancelled,
            ) => self.render_working("Opening your account…", &colors),
        };
        let confirm = self.render_confirm(cx);
        div()
            .id("signin")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            // While the confirmation is open it is the only way on: the
            // other commands wait until it is answered.
            .when(!self.confirm_modal.is_open(), |this| {
                this.on_action(cx.listener(|this, _: &SignInWithCode, window, cx| {
                    this.sign_in_with_code(window, cx);
                }))
                .on_action(cx.listener(|this, _: &SignInWithPassword, window, cx| {
                    this.sign_in_with_password(window, cx);
                }))
                .on_action(cx.listener(|this, _: &UseAnotherServer, window, cx| {
                    this.use_another_server(window, cx);
                }))
                .on_action(cx.listener(
                    |this, _: &PasteSignInCode, window, cx| {
                        this.paste_signin_code(window, cx);
                    },
                ))
            })
            .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_files(paths, window, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .px(px(SPACE_6))
            .py(px(SPACE_8))
            .child(
                div()
                    .id("signin-card")
                    .relative()
                    .accessible(Role::Main, "Sign in")
                    .w_full()
                    .max_w(px(CARD_WIDTH))
                    .my_auto()
                    .flex()
                    .flex_col()
                    .p(px(SPACE_8))
                    .rounded(px(RADIUS_XL))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .shadow_lg()
                    .child(body),
            )
            .children(confirm)
    }
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

/// An invisible alert that reads a problem out when it appears (the banner
/// beside it shows it). Its id changes with the text, so a new problem is a
/// new announcement.
fn announce_problem(prefix: &'static str, message: &str) -> impl IntoElement {
    live_region(
        (prefix, announcement_key(message)),
        message.to_owned(),
        Politeness::Assertive,
    )
}

/// The first key of a command, from the commands table.
pub(crate) fn primary_key(id: &str) -> SharedString {
    command(id)
        .and_then(|command| command.gui_keys.first())
        .map(|key| SharedString::from(*key))
        .unwrap_or_default()
}

/// The Family Chat mark: a speech bubble with a heart on the accent.
pub(crate) fn brand_mark(colors: &UiColors, size: f32) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(size * 0.28))
        .bg(rgb(colors.accent))
        .child(icon(
            "message-circle-heart",
            size * 0.56,
            colors.accent_foreground,
        ))
}

/// The mark, the name and what the screen is for.
fn brand(colors: &UiColors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SPACE_3))
        .text_center()
        .child(brand_mark(colors, 64.0))
        .child(
            div()
                .id("signin-welcome-heading")
                .accessible(Role::Heading, format!("Welcome to {APP_NAME}"))
                .aria_level(1)
                .type_style(TYPE_HEADLINE)
                .text_size(px(26.0))
                .line_height(px(34.0))
                .font_weight(FontWeight::BOLD)
                .child(format!("Welcome to {APP_NAME}")),
        )
        .child(
            div()
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .child("Chat with your family. Choose how you'd like to sign in."),
        )
}

/// A page heading with the sentence under it.
fn heading(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    colors: &UiColors,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .child(
            div()
                .id(id)
                .accessible(Role::Heading, title)
                .aria_level(1)
                .type_style(TYPE_HEADLINE)
                .child(title),
        )
        .child(
            div()
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .child(detail),
        )
}

/// A form field's visible label.
fn field_label(label: &'static str, colors: &UiColors) -> impl IntoElement {
    div()
        .type_style(TYPE_CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(colors.muted))
        .child(label)
}

/// A horizontal rule with a word in the middle.
fn divider(word: &'static str, colors: &UiColors) -> impl IntoElement {
    let line = || div().flex_1().h(px(1.0)).bg(rgb(colors.border));
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .child(line())
        .child(
            div()
                .type_style(TYPE_CAPTION)
                .text_color(rgb(colors.muted))
                .child(word),
        )
        .child(line())
}

/// A full address in a box, as a confirmation shows it: the whole of it,
/// selectable by a screen reader, never shortened.
fn address_box(id: &'static str, address: &str, colors: &UiColors) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .p(px(SPACE_3))
        .rounded(px(RADIUS_LG))
        .bg(rgb(colors.surface))
        .border_1()
        .border_color(rgb(colors.border))
        .child(icon("globe", ICON_SIZE_MD, colors.accent))
        .child(
            div()
                .min_w_0()
                .type_style(TYPE_BODY_STRONG)
                .child(readable(id, address.to_owned())),
        )
}

/// The server a form is for, chosen on the server step, with "Change
/// server".
fn fixed_server(server: &str, colors: &UiColors, cx: &mut Context<SigninView>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .p(px(SPACE_3))
        .rounded(px(RADIUS_LG))
        .bg(rgb(colors.surface))
        .border_1()
        .border_color(rgb(colors.border))
        .child(icon("server", ICON_SIZE_MD, colors.accent))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child("Homeserver"),
                )
                .child(
                    div()
                        .type_style(TYPE_BODY_STRONG)
                        .child(readable("signin-fixed-server", server.to_owned())),
                ),
        )
        .child(
            Button::new("signin-change-server", "Change server")
                .ghost()
                .size(ButtonSize::Small)
                .on_click(cx.listener(|this, _, window, cx| this.change_server(window, cx))),
        )
        .into_any_element()
}

/// A way to sign in the server offers: a large button with a sentence
/// under it.
fn method_button(
    id: &'static str,
    label: &'static str,
    icon_name: &'static str,
    detail: String,
    primary: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    colors: &UiColors,
) -> impl IntoElement {
    let button = Button::new(id, label)
        .icon(icon_name)
        .size(ButtonSize::Large)
        .full_width()
        .on_click(on_click);
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_1))
        .child(if primary { button.primary() } else { button })
        .child(
            div()
                .type_style(TYPE_CAPTION)
                .text_color(rgb(colors.muted))
                .text_center()
                .child(detail),
        )
}

/// Back to the previous page (Escape).
fn back_button(cx: &mut Context<SigninView>) -> impl IntoElement {
    div().flex().child(
        Button::new("signin-back", "Back")
            .ghost()
            .icon("arrow-left")
            .size(ButtonSize::Small)
            .shortcut(primary_key("GoBack"))
            .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
    )
}

/// A spinning loader icon (still while motion is reduced).
pub(crate) fn spinner(id: &'static str, size: f32, color: u32) -> AnyElement {
    icon("loader", size, color)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1200)).repeat(),
            |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

/// One way to sign in on the welcome page: an icon, the command's name and
/// a sentence, with its key.
fn option_row(
    id: &'static str,
    command_id: &'static str,
    detail: &'static str,
    emphasised: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    colors: &UiColors,
) -> impl IntoElement {
    let Some(command) = command(command_id) else {
        return div().into_any_element();
    };
    let tint = if emphasised {
        mix(colors.background, colors.accent, 0.08)
    } else {
        colors.background
    };
    let hover = mix(
        colors.background,
        colors.accent,
        if emphasised { 0.14 } else { 0.06 },
    );
    let border = if emphasised {
        mix(colors.border, colors.accent, 0.6)
    } else {
        colors.border
    };
    let icon_fill = if emphasised {
        colors.accent
    } else {
        mix(colors.surface, colors.accent, 0.12)
    };
    let icon_color = if emphasised {
        colors.accent_foreground
    } else {
        colors.accent
    };
    let key = command.gui_keys.first().copied().unwrap_or_default();
    div()
        .id(id)
        .hit_target(move || id.to_owned())
        .accessible(Role::Button, format!("{}. {detail}", command.name))
        .aria_keyshortcuts(key)
        .tab_index(0)
        .flex()
        .items_center()
        .gap(px(SPACE_4))
        .p(px(SPACE_4))
        .rounded(px(RADIUS_LG))
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(tint))
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(hover)))
        .focus_visible({
            let ring = colors.focus_ring;
            move |style| style.border_color(rgb(ring)).border_2()
        })
        .on_click(on_click)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(40.0))
                .rounded(px(RADIUS_LG))
                .bg(rgb(icon_fill))
                .child(icon(command.icon, ICON_SIZE_MD, icon_color)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(SPACE_1))
                .child(div().type_style(TYPE_BODY_STRONG).child(command.name))
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(detail),
                ),
        )
        .child(Kbd::new(key))
        .child(icon("chevron-right", ICON_SIZE_SM, colors.muted))
        .into_any_element()
}
