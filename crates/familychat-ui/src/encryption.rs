//! `EncryptionView`: end-to-end encryption in the window (PLAN.md, section
//! 5.7; ADR 0009). It draws the session's [`EncryptionStatus`] and
//! [`VerificationFlow`]s and sends the person's answers back; every rule
//! and every bit of crypto is the session's.
//!
//! - **Confirm it's you** takes the window's place while this device isn't
//!   confirmed: another device (emoji, or a QR code for a phone to scan),
//!   or the recovery key; skipping asks first, then a banner over the
//!   rooms says the device isn't confirmed.
//! - **Your server needs your password** takes its place when the server
//!   asked for the password before the account's cross-signing identity
//!   could be made (a sign-in with a code on a server without MSC3967).
//! - The dialogs: the verification, setting recovery up (the key shown
//!   once, with Copy and Save), the devices (verify, sign out), and room
//!   key export and import (a passphrase-protected file).
//!
//! Passwords, recovery keys and passphrases are typed into masked fields
//! (the recovery key's isn't masked, so it can be checked), taken out of
//! them with `take_secret` and handed on as [`SecretText`]; the recovery
//! key shown once is dropped (wiped) when its dialog closes. None is logged.

use std::{path::PathBuf, sync::Arc};

use familychat_core::{
    encryption::{
        AuthOutcome, DeviceView, EncryptionStatus, IdentitySetup, QrModules, SasEmoji, SecretText,
        VerificationAnswer, VerificationFlow, VerificationStep, wording,
    },
    identity::APP_NAME,
    session::{AccountKey, SessionApi, SessionError},
};
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    FontWeight, IntoElement, PathPromptOptions, Role, SharedString, Subscription, Task, Window,
    div, prelude::*, px, rgb,
};
use tokio::sync::watch;

use crate::a11y::{AccessibleExt, Politeness, live_region, readable};
use crate::components::{
    Banner, BannerTone, Button, ButtonSize, ComponentTheme, HitTargetExt, Modal, ModalState,
    TextInput, TextInputEvent, Toast, ToastHost, ToastKind,
};
use crate::signin::spinner;
use crate::styles::{
    ICON_SIZE_MD, ICON_SIZE_SM, RADIUS_LG, RADIUS_XL, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    SPACE_8, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE, TypeStyleExt, UiColors,
    contrast::mix,
};
use crate::widgets::icon;

/// The confirmation screen's card width.
const CARD_WIDTH: f32 = 480.0;
/// One QR module, in pixels: 41 modules (the code and its quiet zone) make
/// a code a phone reads from across a desk.
const QR_MODULE: f32 = 5.0;
/// The quiet zone around a QR code, in modules.
const QR_QUIET: usize = 4;
/// One emoji's cell in the comparison.
const EMOJI_CELL: f32 = 64.0;
/// The verification dialog: seven cells and their gaps, and the padding.
const EMOJI_DIALOG_WIDTH: f32 = 7.0 * EMOJI_CELL + 6.0 * SPACE_2 + 2.0 * SPACE_6 + 8.0;

/// What the window opens on, for the screenshot matrix and the view tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionLaunch {
    /// The recovery key page of "Confirm it's you".
    RecoveryKeyEntry,
    /// The warning before skipping.
    SkipWarning,
    /// Confirming skipped: the rooms with the banner.
    Skipped,
    /// A verification of this device, from its first step.
    Verify,
    /// Setting recovery up, before the key is made.
    SetUpRecovery,
    /// The new recovery key, shown once.
    RecoveryKeyShown,
    /// The devices.
    Devices,
    /// Signing the old tablet out, with the password the server asks for.
    SignOutDevice,
    /// Exporting room keys.
    Export,
    /// Importing room keys, with a file chosen.
    Import,
}

/// A page of "Confirm it's you".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    /// The ways to confirm.
    Choose,
    /// Typing the recovery key.
    RecoveryKey,
}

/// The dialog showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dialog {
    /// None.
    None,
    /// "Skip confirming?"
    SkipWarning,
    /// A verification (incoming or ours).
    Verification,
    /// Setting recovery up.
    SetUpRecovery,
    /// The new recovery key.
    RecoveryKey,
    /// The devices.
    Devices,
    /// Signing a device out.
    SignOut(DeviceView),
    /// Exporting room keys.
    Export,
    /// Importing room keys.
    Import,
}

/// The encryption screens and dialogs of one account.
pub(crate) struct EncryptionView {
    session: Arc<dyn SessionApi>,
    account: AccountKey,
    toasts: Entity<ToastHost>,
    status: EncryptionStatus,
    /// The person skipped "Confirm it's you" this time.
    skipped: bool,
    page: Page,
    /// A request is under way.
    working: bool,
    /// What went wrong on the page or in the dialog.
    problem: Option<String>,
    dialog: Dialog,
    modal: ModalState,
    focus_handle: FocusHandle,
    flow: Option<VerificationFlow>,
    step: VerificationStep,
    /// The verification is of another device, from the devices dialog.
    verifying_other: bool,
    /// The recovery key, while its dialog shows.
    recovery_key: Option<SecretText>,
    /// It was copied or saved.
    key_kept: bool,
    devices: Option<Result<Vec<DeviceView>, String>>,
    /// The server asked for the password to sign a device out.
    sign_out_needs_password: bool,
    import_file: Option<PathBuf>,
    password_input: Entity<TextInput>,
    key_input: Entity<TextInput>,
    passphrase_input: Entity<TextInput>,
    confirm_input: Entity<TextInput>,
    _status_task: Task<()>,
    flow_task: Task<()>,
    work_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EncryptionView {
    /// The encryption of `account`, following its status.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountKey,
        toasts: Entity<ToastHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let receiver = session.encryption(&account);
        let status = receiver
            .as_ref()
            .map(|receiver| receiver.borrow().clone())
            .unwrap_or_default();
        let status_task = match receiver {
            Some(receiver) => follow(receiver, window, cx),
            None => Task::ready(()),
        };
        let password_input = cx.new(|cx| {
            TextInput::new(cx)
                .masked()
                .label("Password")
                .leading_icon("lock")
                .placeholder("Password")
        });
        let key_input = cx.new(|cx| {
            TextInput::new(cx)
                .label("Recovery key")
                .leading_icon("key-round")
                .placeholder("EsTj 3yST y93F …")
        });
        let passphrase_input = cx.new(|cx| {
            TextInput::new(cx)
                .masked()
                .label("Passphrase")
                .leading_icon("lock")
                .placeholder("Passphrase")
        });
        let confirm_input = cx.new(|cx| {
            TextInput::new(cx)
                .masked()
                .label("Passphrase again")
                .leading_icon("lock")
                .placeholder("Passphrase again")
        });
        let subscriptions = vec![
            cx.subscribe_in(&password_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    match this.dialog {
                        Dialog::SignOut(_) => this.sign_out(window, cx),
                        _ => this.submit_password(window, cx),
                    }
                }
            }),
            cx.subscribe_in(&key_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted {
                    this.submit_recovery_key(window, cx);
                }
            }),
            cx.subscribe_in(&confirm_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted && this.dialog == Dialog::Export {
                    this.export(window, cx);
                }
            }),
            cx.subscribe_in(&passphrase_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submitted && this.dialog == Dialog::Import {
                    this.import(window, cx);
                }
            }),
        ];
        Self {
            session,
            account,
            toasts,
            status,
            skipped: false,
            page: Page::Choose,
            working: false,
            problem: None,
            dialog: Dialog::None,
            modal: ModalState::new(cx),
            focus_handle: cx.focus_handle(),
            flow: None,
            step: VerificationStep::Waiting,
            verifying_other: false,
            recovery_key: None,
            key_kept: false,
            devices: None,
            sign_out_needs_password: false,
            import_file: None,
            password_input,
            key_input,
            passphrase_input,
            confirm_input,
            _status_task: status_task,
            flow_task: Task::ready(()),
            work_task: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    // -- State ---------------------------------------------------------------

    /// The dialog showing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn dialog(&self) -> &Dialog {
        &self.dialog
    }

    /// The page of "Confirm it's you".
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn page(&self) -> Page {
        self.page
    }

    /// The verification's step.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn step(&self) -> &VerificationStep {
        &self.step
    }

    /// The devices, once listed.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn devices(&self) -> Option<&Result<Vec<DeviceView>, String>> {
        self.devices.as_ref()
    }

    /// The problem on the page or in the dialog.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether the recovery key is showing, and whether it was kept.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn recovery_key_state(&self) -> (bool, bool) {
        (self.recovery_key.is_some(), self.key_kept)
    }

    /// The fields, for the view tests.
    #[cfg(test)]
    pub(crate) fn inputs(&self) -> [&Entity<TextInput>; 4] {
        [
            &self.password_input,
            &self.key_input,
            &self.passphrase_input,
            &self.confirm_input,
        ]
    }

    /// Whether a screen takes the window's place instead of the rooms.
    pub(crate) fn takes_over(&self) -> bool {
        !self.skipped
            && (self.status.needs_confirmation()
                || self.status.identity == IdentitySetup::NeedsPassword)
    }

    /// The focus handle of the screen while it shows.
    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn set_status(&mut self, status: EncryptionStatus, cx: &mut Context<Self>) {
        let was_confirmed = !self.status.needs_confirmation();
        self.status = status;
        if self.status.needs_confirmation() && was_confirmed {
            // A new reason to confirm (another device reset the identity):
            // ask again.
            self.skipped = false;
        }
        if !self.status.needs_confirmation() && self.page == Page::RecoveryKey {
            self.page = Page::Choose;
        }
        cx.notify();
    }

    fn toast(&self, toast: Toast, cx: &mut Context<Self>) {
        self.toasts.update(cx, |toasts, cx| toasts.push(toast, cx));
    }

    fn open_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.problem = None;
        self.working = false;
        let reopen = self.modal.is_open();
        self.dialog = dialog;
        if !reopen {
            self.modal.open(window, cx);
        }
        cx.notify();
    }

    /// Closes the dialog; the recovery key it showed is wiped.
    pub(crate) fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog == Dialog::Verification
            && let Some(flow) = self.flow.take()
            && !self.step.is_final()
        {
            flow.answer(VerificationAnswer::Cancel);
        }
        self.flow = None;
        self.flow_task = Task::ready(());
        self.recovery_key = None;
        self.key_kept = false;
        self.import_file = None;
        self.sign_out_needs_password = false;
        for input in [
            &self.password_input,
            &self.passphrase_input,
            &self.confirm_input,
        ] {
            input.update(cx, |input, cx| drop(input.take_secret(cx)));
        }
        self.dialog = Dialog::None;
        self.problem = None;
        self.working = false;
        self.modal.close(window, cx);
        cx.notify();
    }

    // -- Commands ------------------------------------------------------------

    /// "Confirm it's you": the screen again, or a word that there is
    /// nothing to confirm.
    pub(crate) fn confirm_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.needs_confirmation() {
            self.skipped = false;
            self.page = Page::Choose;
            self.problem = None;
            window.focus(&self.focus_handle, cx);
            cx.notify();
        } else if self.status.identity == IdentitySetup::NeedsPassword {
            self.skipped = false;
            cx.notify();
        } else {
            self.toast(Toast::new("This device is already confirmed."), cx);
        }
    }

    /// "Set up recovery".
    pub(crate) fn set_up_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.recovery == familychat_core::encryption::RecoveryState::Enabled {
            self.toast(Toast::new("Recovery is already set up."), cx);
            return;
        }
        self.open_dialog(Dialog::SetUpRecovery, window, cx);
    }

    /// "Your devices".
    pub(crate) fn manage_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(Dialog::Devices, window, cx);
        self.load_devices(cx);
    }

    /// "Export room keys".
    pub(crate) fn export_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(Dialog::Export, window, cx);
        let field = self.passphrase_input.read(cx).focus_handle(cx);
        window.focus(&field, cx);
    }

    /// "Import room keys".
    pub(crate) fn import_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(Dialog::Import, window, cx);
    }

    /// Opens what `launch` names (the screenshot matrix, the view tests).
    pub(crate) fn launch(
        &mut self,
        launch: EncryptionLaunch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match launch {
            EncryptionLaunch::RecoveryKeyEntry => self.show_recovery_key_entry(window, cx),
            EncryptionLaunch::SkipWarning => self.ask_to_skip(window, cx),
            EncryptionLaunch::Skipped => {
                self.skipped = true;
                cx.notify();
            }
            EncryptionLaunch::Verify => self.verify_this_device(window, cx),
            EncryptionLaunch::SetUpRecovery => self.set_up_recovery(window, cx),
            EncryptionLaunch::RecoveryKeyShown => {
                self.open_dialog(Dialog::SetUpRecovery, window, cx);
                self.make_recovery_key(cx);
            }
            EncryptionLaunch::Devices => self.manage_devices(window, cx),
            EncryptionLaunch::SignOutDevice => {
                self.manage_devices(window, cx);
                let tablet = DeviceView {
                    device_id: "OLDTABLET".to_owned(),
                    display_name: Some("Old tablet".to_owned()),
                    is_this_device: false,
                    verified: false,
                    last_seen_ms: None,
                };
                self.ask_to_sign_out(tablet, window, cx);
                self.sign_out(window, cx);
            }
            EncryptionLaunch::Export => self.export_keys(window, cx),
            EncryptionLaunch::Import => {
                self.import_keys(window, cx);
                self.import_file = Some(PathBuf::from("family-chat-room-keys.txt"));
            }
        }
    }

    // -- Confirm it's you ----------------------------------------------------

    pub(crate) fn show_recovery_key_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::RecoveryKey;
        self.problem = None;
        let field = self.key_input.read(cx).focus_handle(cx);
        window.focus(&field, cx);
        cx.notify();
    }

    fn back_to_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Choose;
        self.problem = None;
        self.key_input
            .update(cx, |input, cx| drop(input.take_secret(cx)));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(crate) fn ask_to_skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(Dialog::SkipWarning, window, cx);
    }

    /// Skips confirming for now: the rooms, with a banner.
    pub(crate) fn skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.skipped = true;
        self.page = Page::Choose;
        self.close_dialog(window, cx);
    }

    /// Confirms this device with the typed recovery key.
    pub(crate) fn submit_recovery_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let key = self.key_input.update(cx, |input, cx| input.take_secret(cx));
        if key.trim().is_empty() {
            self.problem = Some(wording::NO_RECOVERY_KEY.to_owned());
            cx.notify();
            return;
        }
        self.working = true;
        self.problem = None;
        let reply = self.session.recover(&self.account, SecretText::new(key));
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(()) => {
                        this.page = Page::Choose;
                        this.toast(
                            Toast::new("Confirmed. This device can now read your messages.")
                                .kind(ToastKind::Success),
                            cx,
                        );
                    }
                    Err(error) => this.problem = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Answers the server's request for the password, so the identity can
    /// be made.
    pub(crate) fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let password = self
            .password_input
            .update(cx, |input, cx| input.take_secret(cx));
        if password.is_empty() {
            self.problem = Some(wording::NO_PASSWORD.to_owned());
            cx.notify();
            return;
        }
        self.working = true;
        self.problem = None;
        let reply = self
            .session
            .set_up_identity(&self.account, Some(SecretText::new(password)));
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(AuthOutcome::Done) => {}
                    Ok(AuthOutcome::WrongPassword) => {
                        this.problem = Some(wording::WRONG_PASSWORD.to_owned());
                    }
                    Ok(AuthOutcome::PasswordNeeded) => {
                        this.problem = Some(wording::NO_PASSWORD.to_owned());
                    }
                    Err(error) => this.problem = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Tries making the identity again after a failure.
    pub(crate) fn retry_identity(&mut self, cx: &mut Context<Self>) {
        let reply = self.session.set_up_identity(&self.account, None);
        self.work_task = cx.spawn(async move |_, _| {
            let _ = reply.await;
        });
    }

    // -- Verification --------------------------------------------------------

    /// Asks the account's other devices to verify this one.
    pub(crate) fn verify_this_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.verifying_other = false;
        let reply = self.session.verify_this_device(&self.account);
        self.start_flow(reply, window, cx);
    }

    fn verify_device(&mut self, device_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.verifying_other = true;
        let reply = self.session.verify_device(&self.account, &device_id);
        self.start_flow(reply, window, cx);
    }

    fn start_flow(
        &mut self,
        reply: familychat_core::session::Reply<VerificationFlow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step = VerificationStep::Waiting;
        self.flow = None;
        self.open_dialog(Dialog::Verification, window, cx);
        self.working = true;
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update_in(cx, |this, window, cx| {
                this.working = false;
                match result {
                    Ok(flow) => this.follow_flow(flow, window, cx),
                    Err(error) => {
                        this.step = VerificationStep::Cancelled(error.to_string());
                        cx.notify();
                    }
                }
            });
        });
    }

    /// Shows an incoming request (another device asks to verify).
    pub(crate) fn incoming(
        &mut self,
        flow: VerificationFlow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog == Dialog::Verification && !self.step.is_final() {
            // One at a time: the newer one is declined.
            flow.answer(VerificationAnswer::Cancel);
            return;
        }
        if self.modal.is_open() {
            self.close_dialog(window, cx);
        }
        self.verifying_other = false;
        self.open_dialog(Dialog::Verification, window, cx);
        self.follow_flow(flow, window, cx);
    }

    fn follow_flow(&mut self, flow: VerificationFlow, window: &mut Window, cx: &mut Context<Self>) {
        let mut steps = flow.steps();
        self.step = steps.borrow_and_update().clone();
        self.flow = Some(flow);
        self.flow_task = cx.spawn_in(window, async move |this, cx| {
            while steps.changed().await.is_ok() {
                let step = steps.borrow_and_update().clone();
                let done = step.is_final();
                if this
                    .update(cx, |this, cx| {
                        this.step = step;
                        cx.notify();
                    })
                    .is_err()
                    || done
                {
                    break;
                }
            }
        });
        cx.notify();
    }

    /// Answers the verification.
    pub(crate) fn answer(&mut self, answer: VerificationAnswer, cx: &mut Context<Self>) {
        if let Some(flow) = &self.flow {
            flow.answer(answer);
        }
        cx.notify();
    }

    // -- Recovery ------------------------------------------------------------

    /// Makes the recovery key.
    pub(crate) fn make_recovery_key(&mut self, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        self.working = true;
        self.problem = None;
        let reply = self.session.enable_recovery(&self.account);
        self.work_task = cx.spawn(async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(key) => {
                        this.recovery_key = Some(key);
                        this.key_kept = false;
                        this.dialog = Dialog::RecoveryKey;
                    }
                    Err(error) => this.problem = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Copies the recovery key.
    pub(crate) fn copy_recovery_key(&mut self, cx: &mut Context<Self>) {
        let Some(key) = &self.recovery_key else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(key.expose().to_owned()));
        self.key_kept = true;
        self.toast(
            Toast::new("Copied. Paste it somewhere safe, then copy something else over it.")
                .kind(ToastKind::Success),
            cx,
        );
        cx.notify();
    }

    /// Saves the recovery key to a file the person chooses.
    pub(crate) fn save_recovery_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = &self.recovery_key else {
            return;
        };
        let contents = SecretText::new(format!(
            "{APP_NAME} recovery key\n\n{}\n\nKeep it somewhere safe. Anyone with it can read \
             your messages.\n",
            key.expose()
        ));
        let chosen = cx.prompt_for_new_path(&documents_dir(), Some("Family Chat recovery key.txt"));
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.toast(
                            Toast::new(format!("The file picker couldn't open: {error}"))
                                .kind(ToastKind::Error),
                            cx,
                        );
                    });
                    return;
                }
            };
            let written = cx
                .background_executor()
                .spawn(async move { write_private(&path, contents.expose()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match written {
                    Ok(()) => {
                        this.key_kept = true;
                        this.toast(
                            Toast::new("Recovery key saved.").kind(ToastKind::Success),
                            cx,
                        );
                    }
                    Err(_) => this.toast(
                        Toast::new(
                            "The recovery key couldn't be saved there. Choose another place.",
                        )
                        .kind(ToastKind::Error),
                        cx,
                    ),
                }
                cx.notify();
            });
        });
    }

    // -- Devices -------------------------------------------------------------

    fn load_devices(&mut self, cx: &mut Context<Self>) {
        let reply = self.session.devices(&self.account);
        self.work_task = cx.spawn(async move |this, cx| {
            let result = reply
                .await
                .unwrap_or(Err(SessionError::ShutDown))
                .map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| {
                this.devices = Some(result);
                cx.notify();
            });
        });
    }

    fn ask_to_sign_out(&mut self, device: DeviceView, window: &mut Window, cx: &mut Context<Self>) {
        self.sign_out_needs_password = false;
        self.open_dialog(Dialog::SignOut(device), window, cx);
    }

    /// Signs the device in the dialog out, with the password when the
    /// server asked for it.
    pub(crate) fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Dialog::SignOut(device) = self.dialog.clone() else {
            return;
        };
        if self.working {
            return;
        }
        let password = if self.sign_out_needs_password {
            let typed = self
                .password_input
                .update(cx, |input, cx| input.take_secret(cx));
            if typed.is_empty() {
                self.problem = Some(wording::NO_PASSWORD.to_owned());
                cx.notify();
                return;
            }
            Some(SecretText::new(typed))
        } else {
            None
        };
        self.working = true;
        self.problem = None;
        let reply = self
            .session
            .sign_out_device(&self.account, &device.device_id, password);
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update_in(cx, |this, window, cx| {
                this.working = false;
                match result {
                    Ok(AuthOutcome::Done) => {
                        this.toast(
                            Toast::new(format!("Signed {} out.", device.label()))
                                .kind(ToastKind::Success),
                            cx,
                        );
                        this.sign_out_needs_password = false;
                        this.dialog = Dialog::Devices;
                        this.load_devices(cx);
                    }
                    Ok(AuthOutcome::PasswordNeeded) => {
                        this.sign_out_needs_password = true;
                        let field = this.password_input.read(cx).focus_handle(cx);
                        window.focus(&field, cx);
                    }
                    Ok(AuthOutcome::WrongPassword) => {
                        this.problem = Some(wording::WRONG_PASSWORD.to_owned());
                    }
                    Err(error) => this.problem = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    // -- Room keys -----------------------------------------------------------

    /// Exports the room keys to a file the person chooses.
    pub(crate) fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let (first, second) = (
            self.passphrase_input.read(cx).text().to_owned(),
            self.confirm_input.read(cx).text().to_owned(),
        );
        let problem = if first.is_empty() {
            Some(wording::NO_PASSPHRASE)
        } else if first.chars().count() < 8 {
            Some(wording::PASSPHRASE_TOO_SHORT)
        } else if first != second {
            Some(wording::PASSPHRASES_DIFFER)
        } else {
            None
        };
        drop(SecretText::new(first));
        drop(SecretText::new(second));
        if let Some(problem) = problem {
            self.problem = Some(problem.to_owned());
            cx.notify();
            return;
        }
        let chosen = cx.prompt_for_new_path(&documents_dir(), Some("family-chat-room-keys.txt"));
        let session = self.session.clone();
        let account = self.account.clone();
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(path))) => path,
                _ => return,
            };
            let Ok(passphrase) = this.update(cx, |this, cx| {
                this.working = true;
                this.problem = None;
                this.confirm_input
                    .update(cx, |input, cx| drop(input.take_secret(cx)));
                cx.notify();
                this.passphrase_input
                    .update(cx, |input, cx| SecretText::new(input.take_secret(cx)))
            }) else {
                return;
            };
            let result = session
                .export_room_keys(&account, path, passphrase)
                .await
                .unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update_in(cx, |this, window, cx| {
                this.working = false;
                match result {
                    Ok(()) => {
                        this.toast(Toast::new(wording::EXPORTED).kind(ToastKind::Success), cx);
                        this.close_dialog(window, cx);
                    }
                    Err(error) => this.problem = Some(error.to_string()),
                }
                cx.notify();
            });
        });
    }

    /// Picks the file to import.
    pub(crate) fn choose_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = picked.await
                && let Some(path) = paths.pop()
            {
                let _ = this.update(cx, |this, cx| {
                    this.import_file = Some(path);
                    this.problem = None;
                    cx.notify();
                });
            }
        });
    }

    /// Imports the chosen file with the typed passphrase.
    pub(crate) fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let Some(path) = self.import_file.clone() else {
            self.problem = Some("Choose the file to import first.".to_owned());
            cx.notify();
            return;
        };
        let passphrase = self
            .passphrase_input
            .update(cx, |input, cx| input.take_secret(cx));
        if passphrase.is_empty() {
            self.problem = Some(wording::NO_PASSPHRASE.to_owned());
            cx.notify();
            return;
        }
        self.working = true;
        self.problem = None;
        let reply = self
            .session
            .import_room_keys(&self.account, path, SecretText::new(passphrase));
        self.work_task = cx.spawn_in(window, async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update_in(cx, |this, window, cx| {
                this.working = false;
                match result {
                    Ok(import) => {
                        this.toast(Toast::new(import.message()).kind(ToastKind::Success), cx);
                        this.close_dialog(window, cx);
                    }
                    Err(error) => this.problem = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    // -- Rendering: the screens ----------------------------------------------

    /// The screen that takes the window's place.
    pub(crate) fn render_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = ComponentTheme::get(cx).colors;
        let body = if self.status.identity == IdentitySetup::NeedsPassword {
            self.render_password_page(&colors, cx)
        } else {
            match self.page {
                Page::Choose => self.render_choices(&colors, cx),
                Page::RecoveryKey => self.render_key_entry(&colors, cx),
            }
        };
        div()
            .id("encryption-screen")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .px(px(SPACE_6))
            .py(px(SPACE_8))
            .child(
                div()
                    .id("encryption-card")
                    .accessible(Role::Main, wording::CONFIRM_TITLE)
                    .w_full()
                    .max_w(px(CARD_WIDTH))
                    .my_auto()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_4))
                    .p(px(SPACE_8))
                    .rounded(px(RADIUS_XL))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .shadow_lg()
                    .child(body),
            )
            .into_any_element()
    }

    fn render_choices(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let has_devices = self.status.other_devices;
        let has_key = self.status.can_use_recovery_key();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(shield_heading(
                "encryption-confirm-heading",
                wording::CONFIRM_TITLE,
                wording::CONFIRM_DETAIL,
                colors,
            ))
            .child(
                div()
                    .id("encryption-options")
                    .accessible(Role::Group, "Ways to confirm")
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_3))
                    .child(choice_row(
                        "encryption-use-device",
                        "laptop",
                        wording::USE_ANOTHER_DEVICE,
                        if has_devices {
                            wording::USE_ANOTHER_DEVICE_DETAIL
                        } else {
                            "Sign in on your phone or another computer first, then verify from \
                             there."
                        },
                        true,
                        cx.listener(|this, _, window, cx| this.verify_this_device(window, cx)),
                        colors,
                    ))
                    .when(has_key, |this| {
                        this.child(choice_row(
                            "encryption-use-key",
                            "key-round",
                            wording::USE_RECOVERY_KEY,
                            wording::USE_RECOVERY_KEY_DETAIL,
                            false,
                            cx.listener(|this, _, window, cx| {
                                this.show_recovery_key_entry(window, cx);
                            }),
                            colors,
                        ))
                    }),
            )
            .child(
                div().flex().justify_center().child(
                    Button::new("encryption-skip", wording::SKIP)
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| this.ask_to_skip(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_key_entry(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("encryption-key-form")
            .accessible(Role::Form, wording::ENTER_KEY_TITLE)
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(
                div().flex().child(
                    Button::new("encryption-key-back", "Back")
                        .ghost()
                        .icon("arrow-left")
                        .size(ButtonSize::Small)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.back_to_choices(window, cx)),
                        ),
                ),
            )
            .child(shield_heading(
                "encryption-key-heading",
                wording::ENTER_KEY_TITLE,
                wording::ENTER_KEY_DETAIL,
                colors,
            ))
            .children(problem_banner(
                "encryption-key-problem",
                self.problem.as_deref(),
            ))
            .child(field("Recovery key", &self.key_input, colors))
            .child(
                Button::new("encryption-key-submit", "Confirm")
                    .primary()
                    .size(ButtonSize::Large)
                    .full_width()
                    .disabled(self.working)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.submit_recovery_key(window, cx)),
                    ),
            )
            .when(self.working, |this| {
                this.child(working_line(
                    "encryption-key-working",
                    "Checking your recovery key…",
                    colors,
                ))
            })
            .into_any_element()
    }

    fn render_password_page(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("encryption-password-form")
            .accessible(Role::Form, wording::PASSWORD_TITLE)
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(shield_heading(
                "encryption-password-heading",
                wording::PASSWORD_TITLE,
                wording::PASSWORD_DETAIL,
                colors,
            ))
            .children(problem_banner(
                "encryption-password-problem",
                self.problem.as_deref(),
            ))
            .child(field("Password", &self.password_input, colors))
            .child(
                Button::new("encryption-password-submit", "Set up encryption")
                    .primary()
                    .size(ButtonSize::Large)
                    .full_width()
                    .disabled(self.working)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_password(window, cx))),
            )
            .child(
                div().flex().justify_center().child(
                    Button::new("encryption-password-skip", "Not now")
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| this.ask_to_skip(window, cx))),
                ),
            )
            .into_any_element()
    }

    /// The banners over the rooms: this device isn't confirmed, the
    /// identity couldn't be made, recovery isn't set up.
    pub(crate) fn render_banners(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut banners = Vec::new();
        let status = &self.status;
        if self.skipped
            && (status.needs_confirmation() || status.identity == IdentitySetup::NeedsPassword)
        {
            banners.push(
                Banner::new("encryption-not-confirmed", wording::NOT_CONFIRMED_BANNER)
                    .tone(BannerTone::Caution)
                    .title("Not confirmed")
                    .action(
                        Button::new("encryption-banner-confirm", wording::CONFIRM_TITLE)
                            .icon("shield-check")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_identity(window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        } else if let IdentitySetup::Failed(message) = &status.identity {
            banners.push(
                Banner::new("encryption-setup-failed", message.clone())
                    .tone(BannerTone::Caution)
                    .title("Encryption isn't set up")
                    .action(
                        Button::new("encryption-banner-retry", "Try again")
                            .icon("refresh-cw")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_identity(cx))),
                    )
                    .into_any_element(),
            );
        } else if status.offers_recovery_setup() {
            banners.push(
                Banner::new("encryption-recovery-offer", wording::RECOVERY_BANNER)
                    .tone(BannerTone::Info)
                    .title("Recovery")
                    .action(
                        Button::new("encryption-banner-recovery", wording::RECOVERY_TITLE)
                            .icon("key-round")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_up_recovery(window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        banners
    }

    // -- Rendering: the dialogs ----------------------------------------------

    /// The dialog showing, if any.
    pub(crate) fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.modal.is_open() {
            return None;
        }
        let colors = ComponentTheme::get(cx).colors;
        let close = cx.listener(|this, _, window, cx| this.close_dialog(window, cx));
        let modal = match &self.dialog {
            Dialog::None => return None,
            Dialog::SkipWarning => Modal::new("encryption-skip-dialog", self.modal.focus_handle())
                .title(wording::SKIP_WARNING_TITLE)
                .description(wording::SKIP_WARNING)
                .footer(
                    Button::new("encryption-skip-back", "Go back")
                        .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
                )
                .footer(
                    Button::new("encryption-skip-confirm", "Skip anyway")
                        .danger()
                        .on_click(cx.listener(|this, _, window, cx| this.skip(window, cx))),
                ),
            Dialog::Verification => self.verification_dialog(&colors, cx),
            Dialog::SetUpRecovery => self.recovery_dialog(&colors, cx),
            Dialog::RecoveryKey => self.recovery_key_dialog(&colors, cx),
            Dialog::Devices => self.devices_dialog(&colors, cx),
            Dialog::SignOut(device) => self.sign_out_dialog(device, &colors, cx),
            Dialog::Export => self.export_dialog(&colors, cx),
            Dialog::Import => self.import_dialog(&colors, cx),
        };
        let persistent = matches!(self.dialog, Dialog::RecoveryKey);
        Some(
            modal
                .when(persistent, Modal::persistent)
                .on_dismiss(move |event, window, cx| close(event, window, cx))
                .into_any_element(),
        )
    }

    fn verification_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> Modal {
        let with = self
            .flow
            .as_ref()
            .map(|flow| flow.with.label())
            .unwrap_or_else(|| "your other device".to_owned());
        let incoming = matches!(self.step, VerificationStep::Incoming);
        let title: SharedString = if incoming {
            wording::INCOMING_TITLE.into()
        } else if self.verifying_other {
            format!("Verify {with}").into()
        } else {
            wording::VERIFY_TITLE.into()
        };
        let other_device = self.verifying_other;
        // Wide enough for the seven emoji in one row.
        let modal = Modal::new("encryption-verification", self.modal.focus_handle())
            .title(title)
            .width(px(EMOJI_DIALOG_WIDTH));
        let cancel = || {
            Button::new("encryption-verify-cancel", "Cancel")
                .on_click(cx.listener(|this, _, _, cx| this.answer(VerificationAnswer::Cancel, cx)))
        };
        match &self.step {
            VerificationStep::Waiting => modal
                .child(working_line(
                    "encryption-verify-waiting",
                    if self.working {
                        "Sending the request…"
                    } else {
                        wording::WAITING
                    },
                    colors,
                ))
                .footer(cancel()),
            VerificationStep::Incoming => modal
                .child(announce(
                    "encryption-verify-incoming",
                    format!("{with} wants to verify this device. Accept only if it's yours and you're using it now."),
                ))
                .footer(
                    Button::new("encryption-verify-decline", "Decline").on_click(cx.listener(
                        |this, _, _, cx| this.answer(VerificationAnswer::Cancel, cx),
                    )),
                )
                .footer(
                    Button::new("encryption-verify-accept", "Accept")
                        .primary()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.answer(VerificationAnswer::Accept, cx);
                        })),
                ),
            VerificationStep::Ready { qr } => {
                let text = if qr.is_some() {
                    wording::SCAN_THIS
                } else {
                    wording::READY_NO_QR
                };
                modal
                    .child(readable("encryption-verify-ready", text))
                    .children(qr.as_ref().map(qr_code))
                    .footer(cancel())
                    .footer(
                        Button::new("encryption-verify-emoji", "Compare emoji")
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.answer(VerificationAnswer::CompareEmoji, cx);
                            })),
                    )
            }
            VerificationStep::Starting => modal
                .child(working_line(
                    "encryption-verify-starting",
                    "Getting the emoji ready…",
                    colors,
                ))
                .footer(cancel()),
            VerificationStep::CompareEmoji(emoji) => modal
                .child(readable("encryption-verify-compare", wording::COMPARE))
                .child(emoji_grid(emoji, colors))
                .footer(
                    Button::new("encryption-verify-no-match", "They don't match")
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.answer(VerificationAnswer::NoMatch, cx);
                        })),
                )
                .footer(
                    Button::new("encryption-verify-match", "They match")
                        .primary()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.answer(VerificationAnswer::Match, cx);
                        })),
                ),
            VerificationStep::Confirming => modal
                .child(working_line(
                    "encryption-verify-confirming",
                    wording::CONFIRMING,
                    colors,
                ))
                .footer(cancel()),
            VerificationStep::Scanned => modal
                .child(announce("encryption-verify-scanned", wording::SCANNED.to_owned()))
                .footer(
                    Button::new("encryption-verify-not-scanned", "No").on_click(cx.listener(
                        |this, _, _, cx| this.answer(VerificationAnswer::Cancel, cx),
                    )),
                )
                .footer(
                    Button::new("encryption-verify-scanned-yes", "Yes, it does")
                        .primary()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.answer(VerificationAnswer::ConfirmScanned, cx);
                        })),
                ),
            VerificationStep::Done => modal
                .child(outcome(
                    "encryption-verify-done",
                    "circle-check",
                    colors.success,
                    if other_device {
                        wording::DONE_OTHER
                    } else {
                        wording::DONE
                    },
                ))
                .footer(
                    Button::new("encryption-verify-close", "Done")
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
                ),
            VerificationStep::Cancelled(message) => modal
                .child(outcome(
                    "encryption-verify-cancelled",
                    "alert-circle",
                    colors.danger,
                    message,
                ))
                .footer(
                    Button::new("encryption-verify-close", "Close")
                        .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
                ),
        }
    }

    fn recovery_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> Modal {
        Modal::new("encryption-recovery", self.modal.focus_handle())
            .title(wording::RECOVERY_TITLE)
            .description(wording::RECOVERY_DETAIL)
            .width(px(480.0))
            .children(problem_banner(
                "encryption-recovery-problem",
                self.problem.as_deref(),
            ))
            .when(self.working, |this| {
                this.child(working_line(
                    "encryption-recovery-working",
                    "Making your recovery key and backing up your message keys…",
                    colors,
                ))
            })
            .footer(
                Button::new("encryption-recovery-cancel", "Not now")
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
            .footer(
                Button::new("encryption-recovery-make", "Make my recovery key")
                    .primary()
                    .icon("key-round")
                    .disabled(self.working)
                    .on_click(cx.listener(|this, _, _, cx| this.make_recovery_key(cx))),
            )
    }

    fn recovery_key_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> Modal {
        let key = self
            .recovery_key
            .as_ref()
            .map(|key| key.expose().to_owned())
            .unwrap_or_default();
        Modal::new("encryption-recovery-key", self.modal.focus_handle())
            .title(wording::RECOVERY_KEY_TITLE)
            .description(wording::RECOVERY_KEY_DETAIL)
            .width(px(520.0))
            .child(
                div()
                    .id("encryption-recovery-key-text")
                    .accessible(Role::Label, format!("Recovery key: {key}"))
                    .p(px(SPACE_4))
                    .rounded(px(RADIUS_LG))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .bg(rgb(colors.surface))
                    .font_family(crate::rooms::rich_text::MONO_FONT)
                    .text_size(px(16.0))
                    .line_height(px(26.0))
                    .text_center()
                    .child(key),
            )
            .child(
                div()
                    .flex()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new("encryption-recovery-copy", "Copy")
                            .icon("copy")
                            .on_click(cx.listener(|this, _, _, cx| this.copy_recovery_key(cx))),
                    )
                    .child(
                        Button::new("encryption-recovery-save", "Save to a file…")
                            .icon("save")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_recovery_key(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable(
                        "encryption-recovery-key-hint",
                        if self.key_kept {
                            "Kept. You can close this now."
                        } else {
                            "Copy it or save it before you close this."
                        },
                    )),
            )
            .footer(
                Button::new("encryption-recovery-done", "Done")
                    .primary()
                    .disabled(!self.key_kept)
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
    }

    fn devices_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> Modal {
        let modal = Modal::new("encryption-devices", self.modal.focus_handle())
            .title(wording::DEVICES_TITLE)
            .description("Every device signed in to your account. Verify the ones you use, and sign out the ones you don't.")
            .width(px(560.0));
        let body = match &self.devices {
            None => working_line(
                "encryption-devices-loading",
                "Listing your devices…",
                colors,
            ),
            Some(Err(message)) => problem_banner("encryption-devices-problem", Some(message))
                .unwrap_or_else(|| div().into_any_element()),
            Some(Ok(devices)) => {
                let this_confirmed =
                    self.status.device == familychat_core::encryption::DeviceTrust::Verified;
                div()
                    .id("encryption-device-list")
                    .accessible(Role::List, wording::DEVICES_TITLE)
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .children(devices.iter().enumerate().map(|(index, device)| {
                        self.device_row(index, device, this_confirmed, colors, cx)
                    }))
                    .into_any_element()
            }
        };
        modal.child(body).footer(
            Button::new("encryption-devices-done", "Done")
                .primary()
                .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
        )
    }

    fn device_row(
        &self,
        index: usize,
        device: &DeviceView,
        this_confirmed: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = device.label();
        let seen = if device.is_this_device {
            "This device".to_owned()
        } else {
            device
                .last_seen_ms
                .map(|at| format!("Last seen {}", seen_label(at)))
                .unwrap_or_else(|| "Not seen recently".to_owned())
        };
        let trust = if device.verified {
            "Verified"
        } else {
            "Not verified"
        };
        let label = format!("{name}. {seen}. {trust}.");
        let (trust_icon, trust_color) = if device.verified {
            ("shield-check", colors.success)
        } else {
            ("shield-alert", colors.warning)
        };
        let verify = (!device.is_this_device && !device.verified && this_confirmed).then(|| {
            let device_id = device.device_id.clone();
            Button::new(("encryption-device-verify", index as u64), "Verify")
                .size(ButtonSize::Small)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.verify_device(device_id.clone(), window, cx);
                }))
        });
        let sign_out = (!device.is_this_device).then(|| {
            let device = device.clone();
            Button::new(("encryption-device-sign-out", index as u64), "Sign out")
                .size(ButtonSize::Small)
                .danger()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.ask_to_sign_out(device.clone(), window, cx);
                }))
        });
        div()
            .id(("encryption-device", index as u64))
            .accessible(Role::ListItem, label)
            .flex()
            .items_center()
            .gap(px(SPACE_3))
            .p(px(SPACE_3))
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(rgb(colors.border))
            .when(device.is_this_device, |this| {
                this.bg(rgb(mix(colors.background, colors.accent, 0.06)))
            })
            .child(icon(
                if device.is_this_device {
                    "laptop"
                } else {
                    "smartphone"
                },
                ICON_SIZE_MD,
                colors.muted,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(SPACE_1))
                    .child(div().type_style(TYPE_BODY_STRONG).truncate().child(name))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_2))
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child(seen)
                            .child("·")
                            .child(icon(trust_icon, ICON_SIZE_SM, trust_color))
                            .child(div().text_color(rgb(trust_color)).child(trust)),
                    ),
            )
            .children(verify)
            .children(sign_out)
            .into_any_element()
    }

    fn sign_out_dialog(
        &self,
        device: &DeviceView,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Modal {
        Modal::new("encryption-sign-out", self.modal.focus_handle())
            .title(format!("Sign out {}?", device.label()))
            .description(wording::SIGN_OUT_DEVICE_DETAIL)
            .width(px(480.0))
            .when(self.sign_out_needs_password, |this| {
                this.child(
                    Banner::new(
                        "encryption-sign-out-password",
                        wording::SIGN_OUT_NEEDS_PASSWORD,
                    )
                    .tone(BannerTone::Info),
                )
                .child(field("Password", &self.password_input, colors))
            })
            .children(problem_banner(
                "encryption-sign-out-problem",
                self.problem.as_deref(),
            ))
            .when(self.working, |this| {
                this.child(working_line(
                    "encryption-sign-out-working",
                    "Signing the device out…",
                    colors,
                ))
            })
            .footer(
                Button::new("encryption-sign-out-cancel", "Cancel").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.problem = None;
                        this.sign_out_needs_password = false;
                        this.password_input
                            .update(cx, |input, cx| drop(input.take_secret(cx)));
                        this.dialog = Dialog::Devices;
                        let _ = window;
                        cx.notify();
                    },
                )),
            )
            .footer(
                Button::new("encryption-sign-out-confirm", "Sign out")
                    .danger()
                    .disabled(self.working)
                    .on_click(cx.listener(|this, _, window, cx| this.sign_out(window, cx))),
            )
    }

    fn export_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> Modal {
        Modal::new("encryption-export", self.modal.focus_handle())
            .title(wording::EXPORT_TITLE)
            .description(wording::EXPORT_DETAIL)
            .width(px(480.0))
            .child(field("Passphrase", &self.passphrase_input, colors))
            .child(field("Passphrase again", &self.confirm_input, colors))
            .children(problem_banner(
                "encryption-export-problem",
                self.problem.as_deref(),
            ))
            .when(self.working, |this| {
                this.child(working_line(
                    "encryption-export-working",
                    "Saving the keys…",
                    colors,
                ))
            })
            .footer(
                Button::new("encryption-export-cancel", "Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
            .footer(
                Button::new("encryption-export-save", "Export…")
                    .primary()
                    .icon("download")
                    .disabled(self.working)
                    .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
            )
    }

    fn import_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> Modal {
        let file = self
            .import_file
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        Modal::new("encryption-import", self.modal.focus_handle())
            .title(wording::IMPORT_TITLE)
            .description(wording::IMPORT_DETAIL)
            .width(px(480.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_3))
                    .child(
                        Button::new("encryption-import-choose", "Choose file…")
                            .icon("file-key")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_import_file(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.muted))
                            .child(readable(
                                "encryption-import-file",
                                file.unwrap_or_else(|| "No file chosen".to_owned()),
                            )),
                    ),
            )
            .child(field("Passphrase", &self.passphrase_input, colors))
            .children(problem_banner(
                "encryption-import-problem",
                self.problem.as_deref(),
            ))
            .when(self.working, |this| {
                this.child(working_line(
                    "encryption-import-working",
                    "Reading the keys…",
                    colors,
                ))
            })
            .footer(
                Button::new("encryption-import-cancel", "Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
            .footer(
                Button::new("encryption-import-go", "Import")
                    .primary()
                    .icon("upload")
                    .disabled(self.working || self.import_file.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
            )
    }
}

/// Follows `receiver` into the view's status.
fn follow(
    mut receiver: watch::Receiver<EncryptionStatus>,
    window: &mut Window,
    cx: &mut Context<EncryptionView>,
) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        while receiver.changed().await.is_ok() {
            let status = receiver.borrow_and_update().clone();
            if this
                .update(cx, |this, cx| this.set_status(status, cx))
                .is_err()
            {
                break;
            }
        }
    })
}

/// Where the recovery key and key files are suggested: Documents, or home.
fn documents_dir() -> PathBuf {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let documents = home.join("Documents");
    if documents.is_dir() { documents } else { home }
}

/// Writes `contents` to `path`, readable by the person only on Unix.
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

/// "2 hours ago", "today", "on 3 Oct".
fn seen_label(at_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(at_ms);
    let hours = now.saturating_sub(at_ms) / (60 * 60 * 1000);
    match hours {
        0 => "just now".to_owned(),
        1 => "an hour ago".to_owned(),
        2..=23 => format!("{hours} hours ago"),
        24..=47 => "yesterday".to_owned(),
        _ => format!("{} days ago", hours / 24),
    }
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

/// The shield, a heading and the sentence under it.
fn shield_heading(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    colors: &UiColors,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SPACE_3))
        .text_center()
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(56.0))
                .rounded(px(16.0))
                .bg(rgb(mix(colors.background, colors.accent, 0.14)))
                .child(icon("shield-check", 28.0, colors.accent)),
        )
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

/// A labelled field.
fn field(label: &'static str, input: &Entity<TextInput>, colors: &UiColors) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .child(
            div()
                .type_style(TYPE_CAPTION)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.muted))
                .child(label),
        )
        .child(input.clone())
        .into_any_element()
}

/// A problem, read out when it appears.
fn problem_banner(id: &'static str, problem: Option<&str>) -> Option<AnyElement> {
    let problem = problem?;
    Some(
        div()
            .flex()
            .flex_col()
            .child(live_region(
                (id, crate::a11y::announcement_key(problem)),
                problem.to_owned(),
                Politeness::Assertive,
            ))
            .child(Banner::new(id, problem.to_owned()).tone(BannerTone::Danger))
            .into_any_element(),
    )
}

/// A spinner and what is happening.
fn working_line(id: &'static str, message: &'static str, colors: &UiColors) -> AnyElement {
    div()
        .id(id)
        .accessible(Role::Status, message)
        .flex()
        .items_center()
        .gap(px(SPACE_2))
        .type_style(TYPE_BODY)
        .text_color(rgb(colors.muted))
        .child(spinner(id, ICON_SIZE_SM, colors.muted))
        .child(message)
        .into_any_element()
}

/// Text read out when it appears.
fn announce(id: &'static str, text: String) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .child(live_region(
            (id, crate::a11y::announcement_key(&text)),
            text.clone(),
            Politeness::Polite,
        ))
        .child(div().type_style(TYPE_BODY).child(text))
        .into_any_element()
}

/// The end of a verification.
fn outcome(id: &'static str, icon_name: &'static str, color: u32, text: &str) -> AnyElement {
    div()
        .id(id)
        .accessible(Role::Status, text.to_owned())
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .child(icon(icon_name, ICON_SIZE_MD, color))
        .child(div().flex_1().type_style(TYPE_BODY).child(text.to_owned()))
        .into_any_element()
}

/// The seven emoji to compare.
fn emoji_grid(emoji: &[SasEmoji], colors: &UiColors) -> AnyElement {
    div()
        .id("encryption-emoji")
        .accessible(
            Role::List,
            format!(
                "Emoji to compare: {}",
                emoji
                    .iter()
                    .map(|emoji| emoji.description.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .flex()
        .flex_wrap()
        .justify_center()
        .gap(px(SPACE_2))
        .py(px(SPACE_2))
        .children(emoji.iter().enumerate().map(|(index, emoji)| {
            div()
                .id(("encryption-emoji-item", index as u64))
                .accessible(Role::ListItem, emoji.description.clone())
                .flex()
                .flex_col()
                .items_center()
                .gap(px(SPACE_1))
                .w(px(EMOJI_CELL))
                .py(px(SPACE_2))
                .rounded(px(RADIUS_LG))
                .bg(rgb(colors.surface))
                .child(
                    div()
                        .text_size(px(34.0))
                        .line_height(px(44.0))
                        .child(emoji.symbol.clone()),
                )
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(emoji.description.clone()),
                )
        }))
        .into_any_element()
}

/// A verification QR code: dark modules on white whatever the theme, with
/// its quiet zone, so a phone's camera reads it.
fn qr_code(qr: &QrModules) -> AnyElement {
    let side = qr.width + 2 * QR_QUIET;
    let rows = (0..side).map(|y| {
        div().flex().children((0..side).map(move |x| {
            let dark = x >= QR_QUIET && y >= QR_QUIET && qr.is_dark(x - QR_QUIET, y - QR_QUIET);
            div()
                .size(px(QR_MODULE))
                .bg(rgb(if dark { 0x000000 } else { 0xFFFFFF }))
        }))
    });
    let rows: Vec<_> = rows.collect();
    div()
        .flex()
        .justify_center()
        .child(
            div()
                .id("encryption-qr")
                .accessible(Role::Image, "A QR code for your other device to scan")
                .flex()
                .flex_col()
                .rounded(px(RADIUS_LG))
                .overflow_hidden()
                .children(rows),
        )
        .into_any_element()
}

/// One way to confirm: an icon, a name and a sentence.
fn choice_row(
    id: &'static str,
    icon_name: &'static str,
    title: &'static str,
    detail: &'static str,
    emphasised: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    colors: &UiColors,
) -> AnyElement {
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
    let label: SharedString = format!("{title}. {detail}").into();
    div()
        .id(id)
        .hit_target(move || id.to_owned())
        .accessible(Role::Button, label)
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
                .bg(rgb(if emphasised {
                    colors.accent
                } else {
                    mix(colors.surface, colors.accent, 0.12)
                }))
                .child(icon(
                    icon_name,
                    ICON_SIZE_MD,
                    if emphasised {
                        colors.accent_foreground
                    } else {
                        colors.accent
                    },
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(SPACE_1))
                .child(div().type_style(TYPE_BODY_STRONG).child(title))
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(detail),
                ),
        )
        .child(icon("chevron-right", ICON_SIZE_SM, colors.muted))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_say_when_they_were_seen() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default();
        assert_eq!(seen_label(now), "just now");
        assert_eq!(seen_label(now - 3 * 60 * 60 * 1000), "3 hours ago");
        assert_eq!(seen_label(now - 30 * 60 * 60 * 1000), "yesterday");
        assert_eq!(seen_label(now - 10 * 24 * 60 * 60 * 1000), "10 days ago");
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_recovery_key_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("familychat-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let path = dir.join("key.txt");
        write_private(&path, "EsTj").expect("written");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
