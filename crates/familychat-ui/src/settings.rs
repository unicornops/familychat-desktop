//! The Settings window (PLAN.md, section 13.13; ADR 0015): one per app,
//! opened with Secondary+, (comma), from the command palette, the title
//! bar's settings button, or the macOS application menu. Adapted from
//! Zmail's `zmail-ui` (`src/settings.rs`, unicornops/zmail, GPL-3.0-only
//! like this project): a section list on the leading side and the open
//! section beside it, Zmail's setting rows and headings.
//!
//! | Section | What it holds |
//! |---------|---------------|
//! | Account | who is signed in, this device, a Family Chat account's guidance (the panel, after the link question), Sign out |
//! | Sessions | the account's devices: verify, sign out (the devices view, drawn in the page) |
//! | Security & privacy | recovery (set up, change the key), room key export and import, resetting the identity, read receipts, typing notices, notification text |
//! | Appearance | theme, text size, motion, contrast |
//! | Notifications | whether they show, and whether they show the text |
//! | Diagnostics | the version, the GPUI pin, where the logs are and opening their folder; nothing is uploaded |
//!
//! The window draws from the same `Opened` services as the main window:
//! settings are saved through `SettingsApi` (the main window follows the
//! same watches and applies them, so both redraw), the account through
//! `SessionApi`, and the encryption through an [`EncryptionView`] of its
//! own over the account, whose dialogs show in this window. Nothing here
//! decides a rule: signing out is the session's, links are
//! `links::classify`'s.

mod sections;

use std::sync::Arc;

use anyhow::Result;
use familychat_core::{
    AppBootstrap,
    identity::{APP_ID, PANEL_URL},
    links::{LinkAction, LinkPrompt, classify},
    session::{AccountDetails, AccountView, SessionApi, SessionError, SignedOut},
    settings::{AppearanceSettings, NotificationSettings, PrivacySettings, SettingsApi},
};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    Global, IntoElement, KeyDownEvent, Orientation, Render, Role, ScrollHandle, SharedString, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, point, prelude::*, px,
    relative, rgb, size,
};
use tokio::sync::{oneshot, watch};

use crate::a11y::{AccessibleExt, readable};
use crate::components::title_bar::TRAFFIC_LIGHT_POSITION;
use crate::components::{
    Button, ComponentTheme, HitTargetExt, Modal, ModalState, Scrollbar, TitleBar, Toast, ToastHost,
    ToastKind, window_frame,
};
use crate::encryption::{EncryptionLaunch, EncryptionView};
use crate::shell::ShellView;
use crate::styles::{
    ICON_SIZE_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TYPE_BODY, TYPE_BODY_STRONG,
    TYPE_CAPTION, TYPE_TITLE, TextScale, TypeStyleExt, UiColors, rems_from_px,
};
use crate::widgets::{EllipsisExt, HoverExt, icon, interactive_background};

/// The key context of the Settings window's root.
pub(crate) const KEY_CONTEXT: &str = "SettingsWindow";
/// The Settings window's title (the screenshot script finds it by this).
pub(crate) const WINDOW_TITLE: &str = "Settings";
/// A new Settings window's size.
const SETTINGS_WINDOW_WIDTH: f32 = 860.0;
const SETTINGS_WINDOW_HEIGHT: f32 = 640.0;
/// The smallest the Settings window gets.
const SETTINGS_WINDOW_MIN_WIDTH: f32 = 560.0;
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 420.0;
/// Width of the section list.
const NAV_WIDTH: f32 = 216.0;
/// Height of a row in the section list.
const NAV_ROW_HEIGHT: f32 = 32.0;
/// Widest a section's content gets, so rows stay readable in a wide window.
const CONTENT_MAX_WIDTH: f32 = 680.0;
/// Narrowest a setting's label and description get before its control
/// wraps below them.
const SETTING_TEXT_MIN_WIDTH: f32 = 240.0;

/// A section of the Settings window, in list order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Who is signed in, this device, the panel, signing out.
    #[default]
    Account,
    /// The account's devices.
    Sessions,
    /// Recovery, room keys, the identity, receipts, typing, notification
    /// text.
    Privacy,
    /// Theme, text size, motion, contrast.
    Appearance,
    /// Whether notifications show, and what they say.
    Notifications,
    /// Version, the GPUI pin, the logs.
    Diagnostics,
}

impl SettingsSection {
    /// Every section, in list order.
    pub const ALL: [SettingsSection; 6] = [
        SettingsSection::Account,
        SettingsSection::Sessions,
        SettingsSection::Privacy,
        SettingsSection::Appearance,
        SettingsSection::Notifications,
        SettingsSection::Diagnostics,
    ];

    /// The name in the section list and the section's heading.
    pub fn label(self) -> &'static str {
        match self {
            Self::Account => "Account",
            Self::Sessions => "Sessions",
            Self::Privacy => "Security & privacy",
            Self::Appearance => "Appearance",
            Self::Notifications => "Notifications",
            Self::Diagnostics => "Diagnostics",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Account => "user",
            Self::Sessions => "laptop",
            Self::Privacy => "shield",
            Self::Appearance => "sun",
            Self::Notifications => "bell",
            Self::Diagnostics => "info",
        }
    }

    /// The id its row and its targets are named by.
    pub fn id(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Sessions => "sessions",
            Self::Privacy => "privacy",
            Self::Appearance => "appearance",
            Self::Notifications => "notifications",
            Self::Diagnostics => "diagnostics",
        }
    }
}

/// A dialog to open with the Settings window, for the screenshot matrix
/// and the view tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOpen {
    /// "Sign out of …?"
    SignOut,
    /// The question before the Family Chat panel opens in the browser.
    PanelLink,
    /// One of the encryption dialogs (resetting the recovery key or the
    /// identity, …).
    Encryption(EncryptionLaunch),
}

/// Where the Settings window opens (`familychat launch-ui-fixture`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsLaunch {
    /// The section.
    pub section: SettingsSection,
    /// A dialog to open on it.
    pub open: Option<SettingsOpen>,
}

// ---------------------------------------------------------------------------
// The host: what Settings needs from the main window
// ---------------------------------------------------------------------------

/// What the Settings window works with: the open profile's services, the
/// app's build details, the account the main window shows and the main
/// window itself. The main window installs it once the profile is open.
pub(crate) struct SettingsHost {
    session: Arc<dyn SessionApi>,
    settings: Arc<dyn SettingsApi>,
    bootstrap: AppBootstrap,
    account: Option<AccountView>,
    shell: WindowHandle<ShellView>,
    window: Option<WindowHandle<SettingsWindow>>,
}

impl Global for SettingsHost {}

impl SettingsHost {
    /// Installs the host for the main window `shell`.
    pub(crate) fn install(
        session: Arc<dyn SessionApi>,
        settings: Arc<dyn SettingsApi>,
        bootstrap: AppBootstrap,
        shell: WindowHandle<ShellView>,
        cx: &mut App,
    ) {
        let window = cx.try_global::<Self>().and_then(|host| host.window);
        let account = cx
            .try_global::<Self>()
            .and_then(|host| host.account.clone());
        cx.set_global(Self {
            session,
            settings,
            bootstrap,
            account,
            shell,
            window,
        });
    }
}

/// The main window shows `account` now (or nobody): an open Settings
/// window follows.
pub(crate) fn set_account(account: Option<AccountView>, cx: &mut App) {
    let Some(host) = cx.try_global::<SettingsHost>() else {
        return;
    };
    if host.account == account {
        return;
    }
    let window = host.window;
    cx.global_mut::<SettingsHost>().account = account.clone();
    if let Some(handle) = window {
        let _ = handle.update(cx, |root, window, cx| root.set_account(account, window, cx));
    }
}

/// Opens the Settings window on `section` (or where it was), or brings the
/// open one to the front. Does nothing before the profile is open.
pub(crate) fn open(section: Option<SettingsSection>, cx: &mut App) {
    let Some((existing, shell)) = cx
        .try_global::<SettingsHost>()
        .map(|host| (host.window, host.shell))
    else {
        return;
    };
    if let Some(handle) = existing {
        let shown = handle.update(cx, |root, window, cx| {
            if let Some(section) = section {
                root.show_section(section, window, cx);
            }
            window.activate_window();
        });
        if shown.is_ok() {
            return;
        }
    }
    match SettingsWindow::open(section.unwrap_or_default(), cx) {
        Ok(handle) => {
            if cx.has_global::<SettingsHost>() {
                cx.global_mut::<SettingsHost>().window = Some(handle);
            }
        }
        Err(error) => {
            let _ = shell.update(cx, |shell, _, cx| {
                shell.show_toast(
                    Toast::new(format!("Settings couldn't open: {error:#}")).kind(ToastKind::Error),
                    cx,
                );
            });
        }
    }
}

/// Opens the Settings window as `launch` says (the screenshot matrix and
/// the view tests).
pub(crate) fn open_launch(launch: SettingsLaunch, cx: &mut App) {
    open(Some(launch.section), cx);
    let Some(open) = launch.open else {
        return;
    };
    if let Some(handle) = window(cx) {
        let _ = handle.update(cx, |root, window, cx| root.open_dialog(open, window, cx));
    }
}

/// The open Settings window, if any (a closed one's handle is not).
pub(crate) fn window(cx: &App) -> Option<WindowHandle<SettingsWindow>> {
    let handle = cx.try_global::<SettingsHost>()?.window?;
    cx.windows()
        .iter()
        .any(|window| window.window_id() == handle.window_id())
        .then_some(handle)
}

// ---------------------------------------------------------------------------
// SettingsWindow
// ---------------------------------------------------------------------------

/// A dialog of the Settings window's own (the encryption's are its view's).
#[derive(Debug, Clone, PartialEq)]
enum Dialog {
    None,
    SignOut,
    Link(LinkPrompt),
}

/// The Settings window's root view.
pub(crate) struct SettingsWindow {
    section: SettingsSection,
    nav_focus: FocusHandle,
    scroll: ScrollHandle,
    toasts: Entity<ToastHost>,
    session: Arc<dyn SessionApi>,
    settings: Arc<dyn SettingsApi>,
    bootstrap: AppBootstrap,
    account: Option<AccountView>,
    /// What the session says about the account, once it answers.
    details: Option<Result<AccountDetails, String>>,
    encryption: Option<Entity<EncryptionView>>,
    notifications: NotificationSettings,
    privacy: PrivacySettings,
    appearance: AppearanceSettings,
    dialog: Dialog,
    modal: ModalState,
    signing_out: bool,
    sign_out_problem: Option<String>,
    details_task: Task<()>,
    save_task: Task<()>,
    sign_out_task: Task<()>,
    _follow_tasks: Vec<Task<()>>,
    _encryption_changes: Option<gpui::Subscription>,
}

impl SettingsWindow {
    fn open(section: SettingsSection, cx: &mut App) -> Result<WindowHandle<Self>> {
        let bounds = Bounds::centered(
            None,
            size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
            cx,
        );
        cx.open_window(
            WindowOptions {
                // The main window's chrome: Family Chat draws the title bar.
                titlebar: Some(TitlebarOptions {
                    title: Some(WINDOW_TITLE.into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(
                        px(TRAFFIC_LIGHT_POSITION.0),
                        px(TRAFFIC_LIGHT_POSITION.1),
                    )),
                }),
                app_owns_titlebar_drag: true,
                window_decorations: Some(crate::window_decorations()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(
                    px(SETTINGS_WINDOW_MIN_WIDTH),
                    px(SETTINGS_WINDOW_MIN_HEIGHT),
                )),
                app_id: Some(APP_ID.to_owned()),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| Self::new(section, window, cx)),
        )
    }

    fn new(section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = cx.global::<SettingsHost>();
        let session = host.session.clone();
        let settings = host.settings.clone();
        let bootstrap = host.bootstrap.clone();
        let account = host.account.clone();
        let nav_focus = cx.focus_handle();
        window.focus(&nav_focus, cx);
        let mut notifications = settings.notification_settings();
        let mut privacy = settings.privacy_settings();
        let mut appearance = settings.appearance_settings();
        let current = (
            *notifications.borrow_and_update(),
            *privacy.borrow_and_update(),
            *appearance.borrow_and_update(),
        );
        let follow_tasks = vec![
            follow(notifications, window, cx, |this, value| {
                this.notifications = value;
            }),
            follow(privacy, window, cx, |this, value| this.privacy = value),
            follow(appearance, window, cx, |this, value| {
                this.appearance = value
            }),
        ];
        // The window goes when it closes; the host forgets it.
        cx.on_release(|_, cx| {
            if cx.has_global::<SettingsHost>() {
                cx.global_mut::<SettingsHost>().window = None;
            }
        })
        .detach();
        let mut view = Self {
            section,
            nav_focus,
            scroll: ScrollHandle::new(),
            toasts: cx.new(|_| ToastHost::new()),
            session,
            settings,
            bootstrap,
            account: None,
            details: None,
            encryption: None,
            notifications: current.0,
            privacy: current.1,
            appearance: current.2,
            dialog: Dialog::None,
            modal: ModalState::new(cx),
            signing_out: false,
            sign_out_problem: None,
            details_task: Task::ready(()),
            save_task: Task::ready(()),
            sign_out_task: Task::ready(()),
            _follow_tasks: follow_tasks,
            _encryption_changes: None,
        };
        view.set_account(account, window, cx);
        view.section_opened(cx);
        view
    }

    /// Shows `account` (the main window's), or nobody.
    fn set_account(
        &mut self,
        account: Option<AccountView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.account.as_ref().map(|account| &account.key)
            == account.as_ref().map(|account| &account.key)
        {
            return;
        }
        self.account = account;
        self.details = None;
        self.encryption = None;
        self._encryption_changes = None;
        if let Some(account) = &self.account {
            let session = self.session.clone();
            let key = account.key.clone();
            let toasts = self.toasts.clone();
            let encryption =
                cx.new(|cx| EncryptionView::new(session, key, toasts, window, cx).inline());
            self._encryption_changes = Some(cx.observe(&encryption, |_, _, cx| cx.notify()));
            self.encryption = Some(encryption);
            let reply = self.session.account_details(&account.key);
            self.details_task = cx.spawn(async move |this, cx| {
                let details = reply
                    .await
                    .unwrap_or(Err(SessionError::ShutDown))
                    .map_err(|error| error.to_string());
                let _ = this.update(cx, |this, cx| {
                    this.details = Some(details);
                    cx.notify();
                });
            });
        }
        self.section_opened(cx);
        cx.notify();
    }

    /// The open section.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn section(&self) -> SettingsSection {
        self.section
    }

    /// The account's encryption view, while someone is signed in.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn encryption(&self) -> Option<&Entity<EncryptionView>> {
        self.encryption.as_ref()
    }

    /// The account's details, once the session answered.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn details(&self) -> Option<&Result<AccountDetails, String>> {
        self.details.as_ref()
    }

    /// The settings as this window shows them.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn shown_settings(
        &self,
    ) -> (NotificationSettings, PrivacySettings, AppearanceSettings) {
        (self.notifications, self.privacy, self.appearance)
    }

    /// Whether one of this window's own dialogs is open, and which.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn dialog_name(&self) -> Option<&'static str> {
        match self.dialog {
            Dialog::None => None,
            Dialog::SignOut => Some("sign-out"),
            Dialog::Link(_) => Some("link"),
        }
    }

    /// The address the link question shows, while it does.
    #[cfg(test)]
    pub(crate) fn link_target(&self) -> Option<&str> {
        match &self.dialog {
            Dialog::Link(prompt) => Some(&prompt.target),
            _ => None,
        }
    }

    /// The messages of the toasts showing.
    #[cfg(test)]
    pub(crate) fn toast_messages(&self, cx: &App) -> Vec<String> {
        self.toasts
            .read(cx)
            .visible()
            .into_iter()
            .map(|(_, message)| message.to_string())
            .collect()
    }

    /// Shows `section`.
    pub(crate) fn show_section(
        &mut self,
        section: SettingsSection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.section != section {
            self.section = section;
            self.scroll.set_offset(point(px(0.0), px(0.0)));
            self.section_opened(cx);
        }
        cx.notify();
    }

    /// What a section needs when it opens: the devices are listed again.
    fn section_opened(&mut self, cx: &mut Context<Self>) {
        if self.section == SettingsSection::Sessions
            && let Some(encryption) = &self.encryption
        {
            encryption.update(cx, |encryption, cx| encryption.refresh_devices(cx));
        }
    }

    fn step_section(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let sections = SettingsSection::ALL;
        let index = sections
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or(0);
        let next = if forward {
            (index + 1).min(sections.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.show_section(sections[next], window, cx);
    }

    fn toast(&self, toast: Toast, cx: &mut Context<Self>) {
        self.toasts.update(cx, |toasts, cx| toasts.push(toast, cx));
    }

    /// Whether a dialog (this window's or the encryption's) is open.
    fn dialog_open(&self, cx: &App) -> bool {
        self.modal.is_open()
            || self
                .encryption
                .as_ref()
                .is_some_and(|encryption| encryption.read(cx).dialog_open())
    }

    // -- Dialogs -------------------------------------------------------------

    /// Opens `open` (the screenshot matrix and the view tests).
    pub(crate) fn open_dialog(
        &mut self,
        open: SettingsOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match open {
            SettingsOpen::SignOut => self.ask_to_sign_out(window, cx),
            SettingsOpen::PanelLink => self.open_panel(window, cx),
            SettingsOpen::Encryption(launch) => {
                if let Some(encryption) = self.encryption.clone() {
                    encryption.update(cx, |encryption, cx| encryption.launch(launch, window, cx));
                }
            }
        }
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_out {
            return;
        }
        self.dialog = Dialog::None;
        self.sign_out_problem = None;
        self.modal.close(window, cx);
        window.focus(&self.nav_focus, cx);
        cx.notify();
    }

    /// The panel's address, after the question every link out of the app
    /// asks (ADR 0007).
    pub(crate) fn open_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match classify(PANEL_URL, "the Family Chat panel") {
            LinkAction::Confirm(prompt) => {
                self.dialog = Dialog::Link(prompt);
                self.modal.open(window, cx);
                cx.notify();
            }
            LinkAction::Navigate(_) | LinkAction::Refuse => {}
        }
    }

    fn open_prompted_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Dialog::Link(prompt) = &self.dialog {
            cx.open_url(&prompt.target);
        }
        self.close_dialog(window, cx);
    }

    /// "Sign out of …?"
    pub(crate) fn ask_to_sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account.is_none() {
            return;
        }
        self.sign_out_problem = None;
        self.dialog = Dialog::SignOut;
        self.modal.open(window, cx);
        cx.notify();
    }

    /// Signs the account out and removes it from this computer; the window
    /// closes and the main window says how it went.
    pub(crate) fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        if self.signing_out {
            return;
        }
        self.signing_out = true;
        self.sign_out_problem = None;
        let reply = self.session.sign_out(&account.key);
        self.sign_out_task = cx.spawn_in(window, async move |this, cx| {
            let result = reply.await.unwrap_or(Err(SessionError::ShutDown));
            let _ = this.update_in(cx, |this, window, cx| {
                this.signing_out = false;
                match result {
                    Ok(signed_out) => this.signed_out(signed_out, window, cx),
                    Err(error) => {
                        this.sign_out_problem = Some(error.to_string());
                        cx.notify();
                    }
                }
            });
        });
        cx.notify();
    }

    fn signed_out(&mut self, signed_out: SignedOut, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = Dialog::None;
        self.modal.close(window, cx);
        let shell = cx.try_global::<SettingsHost>().map(|host| host.shell);
        if let Some(shell) = shell {
            let kind = match signed_out {
                SignedOut::Everywhere => ToastKind::Success,
                SignedOut::OnlyHere => ToastKind::Info,
            };
            let _ = shell.update(cx, |shell, _, cx| {
                shell.show_toast(Toast::new(signed_out.message()).kind(kind), cx);
            });
        }
        set_account(None, cx);
        window.remove_window();
    }

    // -- Saving --------------------------------------------------------------

    /// Shows a change at once, saves it, and takes it back with the reason
    /// when the profile couldn't keep it.
    fn save<T: Copy + 'static>(
        &mut self,
        saved: oneshot::Receiver<Result<(), String>>,
        previous: T,
        restore: fn(&mut Self, T),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_task = cx.spawn_in(window, async move |this, cx| {
            let result = saved
                .await
                .unwrap_or_else(|_| Err("Family Chat is closing.".to_owned()));
            if let Err(problem) = result {
                let _ = this.update(cx, |this, cx| {
                    restore(this, previous);
                    this.toast(Toast::new(problem).kind(ToastKind::Error), cx);
                    cx.notify();
                });
            }
        });
        cx.notify();
    }

    pub(crate) fn set_notifications(
        &mut self,
        notifications: NotificationSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.notifications;
        self.notifications = notifications;
        let saved = self.settings.set_notification_settings(notifications);
        self.save(
            saved,
            previous,
            |this, value| this.notifications = value,
            window,
            cx,
        );
    }

    pub(crate) fn set_privacy(
        &mut self,
        privacy: PrivacySettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.privacy;
        self.privacy = privacy;
        let saved = self.settings.set_privacy_settings(privacy);
        self.save(
            saved,
            previous,
            |this, value| this.privacy = value,
            window,
            cx,
        );
    }

    pub(crate) fn set_appearance(
        &mut self,
        appearance: AppearanceSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.appearance;
        self.appearance = appearance;
        let saved = self.settings.set_appearance_settings(appearance);
        self.save(
            saved,
            previous,
            |this, value| this.appearance = value,
            window,
            cx,
        );
    }

    // -- Keys ----------------------------------------------------------------

    /// Up and Down in the section list; Escape (or Secondary+W) closes the
    /// window when no dialog is open.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let plain = !keystroke.modifiers.modified();
        if self.dialog_open(cx) {
            return;
        }
        if keystroke.key == "escape" && plain
            || keystroke.key == "w" && keystroke.modifiers.secondary(cx)
        {
            cx.stop_propagation();
            window.remove_window();
            return;
        }
        if self.nav_focus.is_focused(window) && plain {
            match keystroke.key.as_str() {
                "down" => self.step_section(true, window, cx),
                "up" => self.step_section(false, window, cx),
                "home" => self.show_section(SettingsSection::ALL[0], window, cx),
                "end" => self.show_section(SettingsSection::Diagnostics, window, cx),
                _ => return,
            }
            cx.stop_propagation();
        }
    }

    // -- Rendering -----------------------------------------------------------

    fn render_nav(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let focused = self.nav_focus.is_focused(window);
        div()
            .id("settings-nav")
            .track_focus(&self.nav_focus.clone().tab_index(0).tab_stop(true))
            .accessible(Role::TabList, SharedString::from("Settings sections"))
            .aria_orientation(Orientation::Vertical)
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_1))
            // Grows with the text size, but leaves the content most of the
            // window.
            .w(rems_from_px(NAV_WIDTH))
            .max_w(relative(0.36))
            .h_full()
            .p(px(SPACE_2))
            .bg(rgb(colors.surface))
            .border_r_1()
            .border_color(rgb(colors.border))
            .children(SettingsSection::ALL.into_iter().map(|section| {
                let selected = section == self.section;
                // A full accent row while the list has focus, grey when it
                // does not.
                let (fill, text) = if selected && focused {
                    (colors.selection_background, colors.selection_foreground)
                } else {
                    (
                        interactive_background(selected, colors.surface, &colors),
                        colors.foreground,
                    )
                };
                let id = format!("settings-nav-{}", section.id());
                div()
                    .id(SharedString::from(id.clone()))
                    .hit_target(move || id.clone())
                    .accessible(Role::Tab, SharedString::from(section.label()))
                    .aria_selected(selected)
                    // Up and Down move between sections while focus stays
                    // on the list.
                    .when(selected && focused, |this| this.aria_active_descendant())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .min_h(TextScale::get(cx).fit(NAV_ROW_HEIGHT, TYPE_BODY))
                    .px(px(SPACE_2))
                    .rounded(px(ComponentTheme::get(cx).platform.control_radius))
                    .bg(rgb(fill))
                    .hover_bg(selected, &colors)
                    .cursor_pointer()
                    .type_style(if selected {
                        TYPE_BODY_STRONG
                    } else {
                        TYPE_BODY
                    })
                    .text_color(rgb(text))
                    .on_click(cx.listener(move |root, _, window, cx| {
                        window.focus(&root.nav_focus, cx);
                        root.show_section(section, window, cx);
                    }))
                    .child(icon(section.icon(), ICON_SIZE_SM, text))
                    .child(div().flex_1().min_w_0().ellipsis().child(section.label()))
            }))
    }

    fn render_section(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            SettingsSection::Account => self.render_account(cx),
            SettingsSection::Sessions => self.render_sessions(cx),
            SettingsSection::Privacy => self.render_privacy(cx),
            SettingsSection::Appearance => self.render_appearance(cx),
            SettingsSection::Notifications => self.render_notifications(cx),
            SettingsSection::Diagnostics => self.render_diagnostics(window, cx),
        }
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.modal.is_open() {
            return None;
        }
        let colors = ComponentTheme::get(cx).colors;
        match &self.dialog {
            Dialog::None => None,
            Dialog::SignOut => Some(self.render_sign_out_dialog(&colors, cx)),
            Dialog::Link(prompt) => {
                let root = cx.entity().downgrade();
                let close = {
                    let root = root.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let _ = root.update(cx, |this, cx| this.close_dialog(window, cx));
                    }
                };
                Some(crate::rooms::link_prompt_dialog(
                    prompt,
                    &self.modal,
                    &colors,
                    close,
                    move |window, cx| {
                        let _ = root.update(cx, |this, cx| this.open_prompted_link(window, cx));
                    },
                ))
            }
        }
    }

    fn render_sign_out_dialog(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let who = self
            .account
            .as_ref()
            .map(|account| account.user_id.to_string())
            .unwrap_or_default();
        // Nothing else can bring the encrypted history back: no recovery
        // and no other device.
        let unprotected = self.encryption.as_ref().is_some_and(|encryption| {
            let status = encryption.read(cx).status();
            !status.can_use_recovery_key() && !status.other_devices
        });
        Modal::new("settings-sign-out", self.modal.focus_handle())
            .title(format!("Sign out of {who}?"))
            .description(sections::SIGN_OUT_DETAIL)
            .width(px(500.0))
            .when(unprotected, |this| {
                this.child(
                    crate::components::Banner::new(
                        "settings-sign-out-unprotected",
                        sections::SIGN_OUT_UNPROTECTED,
                    )
                    .tone(crate::components::BannerTone::Caution),
                )
            })
            .children(self.sign_out_problem.as_ref().map(|problem| {
                crate::components::Banner::new("settings-sign-out-problem", problem.clone())
                    .tone(crate::components::BannerTone::Danger)
                    .into_any_element()
            }))
            .when(self.signing_out, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_2))
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(crate::signin::spinner(
                            "settings-sign-out-spinner",
                            ICON_SIZE_SM,
                            colors.muted,
                        ))
                        .child(readable(
                            "settings-sign-out-working",
                            "Signing out and removing the account from this computer…",
                        )),
                )
            })
            .footer(
                Button::new("settings-sign-out-cancel", "Cancel")
                    .disabled(self.signing_out)
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            )
            .footer(
                Button::new("settings-sign-out-confirm", "Sign out")
                    .danger()
                    .icon("log-out")
                    .disabled(self.signing_out)
                    .on_click(cx.listener(|this, _, window, cx| this.sign_out(window, cx))),
            )
            .on_dismiss(cx.listener(|this, _, window, cx| this.close_dialog(window, cx)))
            .into_any_element()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let section = self.render_section(window, cx);
        let encryption_dialog = self
            .encryption
            .clone()
            .and_then(|encryption| encryption.update(cx, |view, cx| view.render_dialog(cx)));
        let root = div()
            .id("settings-window")
            .relative()
            .key_context(KEY_CONTEXT)
            .on_key_down(cx.listener(Self::key_down))
            .accessible(Role::Application, WINDOW_TITLE)
            .size_full()
            .font_family(theme.platform.ui_font.family())
            .text_color(rgb(colors.foreground))
            .bg(rgb(colors.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(
                        TitleBar::new("settings-title-bar").child(
                            div()
                                .id("settings-title")
                                .accessible(Role::Heading, WINDOW_TITLE)
                                .flex_1()
                                .px(px(SPACE_2))
                                .type_style(TYPE_BODY_STRONG)
                                .child(WINDOW_TITLE),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_nav(window, cx))
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(
                                        div()
                                            .id("settings-content")
                                            .accessible(
                                                Role::TabPanel,
                                                SharedString::from(self.section.label()),
                                            )
                                            .size_full()
                                            .overflow_y_scroll()
                                            .track_scroll(&self.scroll)
                                            .child(
                                                div()
                                                    .w_full()
                                                    .max_w(rems_from_px(CONTENT_MAX_WIDTH))
                                                    .px(px(SPACE_6))
                                                    .py(px(SPACE_4))
                                                    .child(section),
                                            ),
                                    )
                                    .child(Scrollbar::vertical("settings-scrollbar", &self.scroll)),
                            ),
                    ),
            )
            .child(self.toasts.clone())
            .children(encryption_dialog)
            .children(self.render_dialog(cx));
        window_frame(root, window, cx)
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.nav_focus.clone()
    }
}

/// Follows a settings watch into the window, with `apply`.
fn follow<T: Copy + Send + Sync + 'static>(
    mut receiver: watch::Receiver<T>,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
    apply: fn(&mut SettingsWindow, T),
) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        while receiver.changed().await.is_ok() {
            let value = *receiver.borrow_and_update();
            if this
                .update(cx, |this, cx| {
                    apply(this, value);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Shared section layout (Zmail's)
// ---------------------------------------------------------------------------

/// A muted value on the trailing side of a row, such as a version.
pub(crate) fn row_value(
    id: impl Into<gpui::ElementId>,
    value: String,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .type_style(TYPE_CAPTION)
        .text_color(rgb(colors.muted))
        .child(readable(id, value))
}

/// A section's heading.
pub(crate) fn section_heading(title: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id("settings-section-heading")
        .pb(px(SPACE_2))
        .type_style(TYPE_TITLE)
        .accessible(Role::Heading, title.to_owned())
        .aria_level(1)
        .child(title.to_owned())
}

/// A group heading inside a section, such as "Encryption".
pub(crate) fn group_heading(title: &str, colors: &UiColors) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!("settings-group-{title}")))
        .accessible(Role::Heading, title.to_owned())
        .aria_level(2)
        .pt(px(SPACE_6))
        .pb(px(SPACE_1))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(colors.muted))
        .child(title.to_uppercase())
}

/// One setting: its label and a sentence about it on the leading side, the
/// control on the trailing side, and a divider below. When the two do not
/// fit side by side (large text in a narrow window), the control wraps
/// below the text.
pub(crate) fn setting_row(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SPACE_4))
        .py(px(SPACE_3))
        .border_b_1()
        .border_color(rgb(colors.border))
        .child(setting_text(label, description, colors).min_w(rems_from_px(SETTING_TEXT_MIN_WIDTH)))
        .child(div().flex_none().max_w_full().child(control))
}

/// A setting's label and description, both readable.
pub(crate) fn setting_text(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    colors: &UiColors,
) -> gpui::Div {
    let label = label.into();
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_1))
        .flex_1()
        .min_w_0()
        .child(div().type_style(TYPE_BODY_STRONG).child(readable(
            SharedString::from(format!("setting-label-{label}")),
            label,
        )))
        .when_some(description, |this, description| {
            this.child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable(
                        SharedString::from(format!("setting-description-{description}")),
                        description,
                    )),
            )
        })
}
