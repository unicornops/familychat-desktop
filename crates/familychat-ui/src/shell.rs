//! `ShellView`: the window's root.
//!
//! It asks the app's `StartupApi` to open the profile (off the UI thread)
//! and shows what comes back: the profile screens while it is not ready,
//! the sign-in while no account is signed in, and the signed-in view once
//! one is. It draws the title bar (Zmail's, so GNOME on Wayland, which has
//! no server-side decorations, still gets one) and the window frame, keeps
//! the theme in step with the OS appearance, and owns the command palette
//! and the toasts.
//!
//! It also takes what reaches the window from outside (ADR 0013): a
//! sign-in link a second launch forwarded (or macOS handed over) starts a
//! sign-in, over the rooms when someone is signed in, which always shows
//! its confirmation first; the session's notifications are posted to the
//! OS for the rooms the person isn't looking at
//! ([`notifications`](crate::notifications)); and the window's title
//! carries the unread total.

use std::sync::Arc;

use familychat_core::{
    AppBootstrap, OwnedRoomId, ThemeAppearance,
    commands::{COMMANDS, CommandScope},
    identity::APP_NAME,
    notifications::IncomingNotification,
    requests::AppRequest,
    session::{AccountKey, AccountView},
    settings::NotificationSettings,
    signin::{SecretText, SigninStart},
    startup::{OpenState, Opened, StartupApi},
};
use familychat_notify::{RoomTarget, window_title};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Render,
    Role, Subscription, Task, Window, div, prelude::*, px, rgb,
};

use crate::a11y::AccessibleExt;
use crate::actions::{
    LinkDevice, NotificationSettings as OpenNotificationSettings, ToggleCommandPalette,
};
use crate::command_palette::{CommandPalette, CommandPaletteEvent, PALETTE_WIDTH, PaletteEntry};
use crate::components::{
    ComponentTheme, IconButton, Modal, ModalState, TitleBar, Toast, ToastHost, ToastKind,
    window_frame,
};
use crate::notifications;
use crate::profile_screens::{self, ProfileAction, ProfileScreen};
use crate::rooms::RoomOpened;
use crate::signed_in::{SignedInView, sync_indicator};
use crate::signin::{SigninEvent, SigninView, brand_mark, primary_key};
use crate::styles::{SPACE_2, TYPE_BODY_STRONG, TypeStyleExt, UiColors};
use crate::{InitialView, LaunchOptions, appearance_for, qr::QrError};

/// Key context of the window root.
pub(crate) const KEY_CONTEXT: &str = "Shell";

/// What the window shows.
pub(crate) enum Screen {
    /// The profile is opening, or is not ready.
    Profile(ProfileScreen),
    /// Nobody is signed in.
    Signin(Entity<SigninView>),
    /// An account is signed in.
    SignedIn(Entity<SignedInView>),
}

/// The main window.
pub struct ShellView {
    bootstrap: AppBootstrap,
    /// The OS appearance, or the fixture's.
    appearance: ThemeAppearance,
    pinned_appearance: Option<ThemeAppearance>,
    startup: Arc<dyn StartupApi>,
    opened: Option<Opened>,
    screen: Screen,
    focus_handle: FocusHandle,
    /// What the window opens on, until the profile is open.
    initial_view: Option<InitialView>,
    palette: Option<(Entity<CommandPalette>, FocusHandle, Subscription)>,
    palette_modal: ModalState,
    toasts: Entity<ToastHost>,
    open_task: Task<()>,
    accounts_task: Task<()>,
    /// The signed-in rooms, kept while a forwarded link's sign-in shows
    /// over them.
    parked: Option<Entity<SignedInView>>,
    /// A forwarded link that arrived before the profile was open.
    pending_link: Option<SecretText>,
    /// The notification settings, as the profile has them.
    notification_settings: NotificationSettings,
    settings_modal: ModalState,
    settings_problem: Option<String>,
    requests_task: Task<()>,
    notifications_task: Task<()>,
    settings_task: Task<()>,
    save_task: Task<()>,
    unread_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// What the window says when a link arrives in the middle of a sign-in.
pub(crate) const FINISH_SIGNING_IN: &str = "Finish signing in first, then open the link again.";

impl ShellView {
    /// The window over `startup`, following the OS appearance unless the
    /// options pin one.
    pub fn new(
        bootstrap: AppBootstrap,
        startup: Arc<dyn StartupApi>,
        options: LaunchOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance_changes = cx.observe_window_appearance(window, |this, window, cx| {
            this.appearance = this
                .pinned_appearance
                .unwrap_or_else(|| appearance_for(window.appearance()));
            this.sync_theme(cx);
            cx.notify();
        });
        // The room in front of the person counts as seen once the window
        // has focus again.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active()
                && let Some((account, room)) = this.focused_room(window, cx)
            {
                notifications::room_seen(&account, &room, cx);
            }
        });
        notifications::init(cx);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let requests = options
            .requests
            .as_ref()
            .and_then(crate::RequestInbox::take_receiver);
        let mut view = Self {
            appearance: options
                .appearance
                .unwrap_or_else(|| appearance_for(window.appearance())),
            pinned_appearance: options.appearance,
            bootstrap,
            startup,
            opened: None,
            screen: Screen::Profile(ProfileScreen::Opening),
            focus_handle,
            initial_view: Some(options.initial_view),
            palette: None,
            palette_modal: ModalState::new(cx),
            toasts: cx.new(|_| ToastHost::new()),
            open_task: Task::ready(()),
            accounts_task: Task::ready(()),
            parked: None,
            pending_link: None,
            notification_settings: NotificationSettings::default(),
            settings_modal: ModalState::new(cx),
            settings_problem: None,
            requests_task: Task::ready(()),
            notifications_task: Task::ready(()),
            settings_task: Task::ready(()),
            save_task: Task::ready(()),
            unread_task: Task::ready(()),
            _subscriptions: vec![appearance_changes, activation],
        };
        view.sync_theme(cx);
        if let Some(mut requests) = requests {
            view.requests_task = cx.spawn_in(window, async move |this, cx| {
                while let Some(request) = requests.recv().await {
                    if this
                        .update_in(cx, |this, window, cx| this.request(request, window, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        let receiver = view.startup.open();
        view.follow_open(receiver, window, cx);
        view
    }

    /// The theme the window draws with.
    pub fn theme(&self) -> &familychat_core::ThemeSummary {
        self.bootstrap.theme(self.appearance)
    }

    /// Publishes the theme: the components' global, and the palette the
    /// entities among them (the text input) read.
    fn sync_theme(&self, cx: &mut App) {
        ComponentTheme::sync(self.theme(), cx);
        let colors = ComponentTheme::get(cx).colors;
        if cx.try_global::<UiColors>() != Some(&colors) {
            cx.set_global(colors);
        }
    }

    /// What the window shows.
    #[cfg(test)]
    pub(crate) fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The sign-in view, while it shows.
    pub(crate) fn signin(&self) -> Option<&Entity<SigninView>> {
        match &self.screen {
            Screen::Signin(signin) => Some(signin),
            _ => None,
        }
    }

    /// The signed-in view, while it shows.
    pub(crate) fn signed_in(&self) -> Option<&Entity<SignedInView>> {
        match &self.screen {
            Screen::SignedIn(view) => Some(view),
            _ => None,
        }
    }

    /// The open palette.
    #[cfg(test)]
    pub(crate) fn palette(&self) -> Option<&Entity<CommandPalette>> {
        self.palette.as_ref().map(|(palette, _, _)| palette)
    }

    /// The toasts.
    #[cfg(test)]
    pub(crate) fn toasts(&self) -> &Entity<ToastHost> {
        &self.toasts
    }

    // -- Opening the profile -------------------------------------------------

    fn follow_open(
        &mut self,
        receiver: tokio::sync::oneshot::Receiver<OpenState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.screen = Screen::Profile(ProfileScreen::Opening);
        cx.notify();
        self.open_task = cx.spawn_in(window, async move |this, cx| {
            let state = receiver.await.unwrap_or_else(|_| {
                OpenState::Failed(format!("{APP_NAME} stopped while opening its data."))
            });
            let _ = this.update_in(cx, |this, window, cx| this.opened(state, window, cx));
        });
    }

    fn profile_action(
        &mut self,
        action: ProfileAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            ProfileAction::Retry => {
                let receiver = self.startup.open();
                self.follow_open(receiver, window, cx);
            }
            ProfileAction::StartOver => {
                let receiver = self.startup.start_over();
                self.follow_open(receiver, window, cx);
            }
            ProfileAction::Quit => cx.quit(),
        }
    }

    fn opened(&mut self, state: OpenState, window: &mut Window, cx: &mut Context<Self>) {
        let opened = match state {
            OpenState::Ready(opened) => opened,
            OpenState::KeychainUnavailable(message) => {
                self.screen = Screen::Profile(ProfileScreen::Keychain(message.into()));
                return cx.notify();
            }
            OpenState::Unreadable(message) => {
                self.screen = Screen::Profile(ProfileScreen::LostKey(message.into()));
                return cx.notify();
            }
            OpenState::Failed(message) => {
                self.screen = Screen::Profile(ProfileScreen::Failed(message.into()));
                return cx.notify();
            }
        };
        if let Some(notice) = opened.notice.clone() {
            self.toast(Toast::new(notice).kind(ToastKind::Success), cx);
        }
        let mut accounts = opened.session.accounts();
        let first = accounts.borrow_and_update().first().cloned();
        self.opened = Some(opened);
        match first {
            Some(account) => self.show_account(account, window, cx),
            None => self.show_signin(window, cx),
        }
        // A sign-in in another flow, or an account restored late, shows
        // when it arrives.
        self.accounts_task = cx.spawn_in(window, async move |this, cx| {
            while accounts.changed().await.is_ok() {
                let first = accounts.borrow_and_update().first().cloned();
                let keep_going = this
                    .update_in(cx, |this, window, cx| {
                        if let Some(account) = first
                            && this.signed_in().is_none()
                            && this.parked.is_none()
                        {
                            this.show_account(account, window, cx);
                        }
                    })
                    .is_ok();
                if !keep_going {
                    break;
                }
            }
        });
        self.follow_settings(window, cx);
        self.follow_notifications(window, cx);
        if let Some(initial) = self.initial_view.take() {
            self.apply_initial_view(initial, window, cx);
        }
        if let Some(link) = self.pending_link.take() {
            self.start_link(link, window, cx);
        }
    }

    fn show_signin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.new_signin(false, window, cx) {
            self.screen = Screen::Signin(view);
            cx.notify();
        }
    }

    /// A sign-in view over the open profile's sign-in, followed for its
    /// end; `returning` for one shown over the rooms.
    fn new_signin(
        &mut self,
        returning: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SigninView>> {
        let opened = self.opened.as_ref()?;
        let signin = opened.signin.clone();
        let view = cx.new(|cx| {
            let view = SigninView::new(signin, window, cx);
            if returning {
                view.returning_to_rooms()
            } else {
                view
            }
        });
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let finished = match event {
                SigninEvent::Finished(finished) => finished,
                SigninEvent::Closed => {
                    this.back_to_rooms(window, cx);
                    return;
                }
            };
            this.parked = None;
            let account = this.opened.as_ref().and_then(|opened| {
                opened
                    .session
                    .accounts()
                    .borrow()
                    .iter()
                    .find(|account| account.key == finished.account)
                    .cloned()
            });
            let account = account.unwrap_or_else(|| AccountView {
                key: finished.account.clone(),
                user_id: finished.user_id.clone(),
                homeserver_url: String::new(),
            });
            this.show_account(account, window, cx);
        });
        self._subscriptions.push(subscription);
        view.update(cx, |view, cx| view.focus(window, cx));
        Some(view)
    }

    /// Back to the rooms a forwarded link's sign-in showed over.
    fn back_to_rooms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rooms) = self.parked.take() else {
            return;
        };
        let focus = rooms.focus_handle(cx);
        self.screen = Screen::SignedIn(rooms);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn show_account(&mut self, account: AccountView, window: &mut Window, cx: &mut Context<Self>) {
        let Some(opened) = &self.opened else {
            return;
        };
        let session = opened.session.clone();
        let toasts = self.toasts.clone();
        let launch = match self.initial_view.take() {
            Some(InitialView::Rooms(launch)) => Some(launch),
            other => {
                self.initial_view = other;
                None
            }
        };
        let unread = session.unread_total(&account.key);
        let view = cx.new(|cx| SignedInView::new(session, account, toasts, launch, window, cx));
        let rooms = view.read(cx).rooms().clone();
        let opened_rooms = cx.subscribe_in(&rooms, window, |this, rooms, event, window, cx| {
            let RoomOpened(room) = event;
            if window.is_window_active() && this.signed_in().is_some() {
                let account = rooms.read(cx).account().key.clone();
                notifications::room_seen(&account, room, cx);
            }
        });
        self._subscriptions.push(opened_rooms);
        let focus = view.focus_handle(cx);
        self.screen = Screen::SignedIn(view);
        self.parked = None;
        window.focus(&focus, cx);
        self.follow_unread(unread, window, cx);
        cx.notify();
    }

    /// Keeps the window's title on the account's unread total.
    fn follow_unread(
        &mut self,
        unread: Option<tokio::sync::watch::Receiver<u64>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut unread) = unread else {
            window.set_window_title(&window_title(0));
            self.unread_task = Task::ready(());
            return;
        };
        window.set_window_title(&window_title(*unread.borrow_and_update()));
        self.unread_task = cx.spawn_in(window, async move |this, cx| {
            while unread.changed().await.is_ok() {
                let total = *unread.borrow_and_update();
                if this
                    .update_in(cx, |_, window, _| {
                        window.set_window_title(&window_title(total));
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    // -- Requests from outside the window -------------------------------------

    /// A second launch's request, or a link macOS handed over: the window
    /// comes forward, and a link starts a sign-in.
    pub(crate) fn request(
        &mut self,
        request: AppRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.activate(true);
        window.activate_window();
        match request {
            AppRequest::Activate => {}
            AppRequest::OpenLink(link) => {
                if self.opened.is_some() {
                    self.start_link(link, window, cx);
                } else {
                    // The profile opens first; the newest link waits.
                    self.pending_link = Some(link);
                }
            }
        }
    }

    /// Starts a sign-in with a forwarded link: on the sign-in screen, or
    /// over the rooms. The sign-in shows its confirmation before anything
    /// is redeemed; a sign-in already using a code or a password is not
    /// interrupted.
    fn start_link(&mut self, link: SecretText, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
        }
        if self.settings_modal.is_open() {
            self.close_notification_settings(window, cx);
        }
        let signin = match &self.screen {
            Screen::Profile(_) => {
                self.pending_link = Some(link);
                return;
            }
            Screen::Signin(signin) => {
                if signin.read(cx).is_busy() {
                    self.toast(Toast::new(FINISH_SIGNING_IN).kind(ToastKind::Info), cx);
                    return;
                }
                signin.clone()
            }
            Screen::SignedIn(rooms) => {
                let rooms = rooms.clone();
                let Some(signin) = self.new_signin(true, window, cx) else {
                    return;
                };
                self.parked = Some(rooms);
                self.screen = Screen::Signin(signin.clone());
                signin
            }
        };
        signin.update(cx, |signin, cx| {
            signin.start(SigninStart::Link(link), window, cx);
        });
        cx.notify();
    }

    /// "Sign in a new device" (ADR 0010): the signed-in account's QR code
    /// for a new phone, shown over the rooms like a forwarded link's
    /// sign-in, back to them when it ends.
    fn link_device(&mut self, _: &LinkDevice, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::SignedIn(rooms) = &self.screen else {
            return;
        };
        let rooms = rooms.clone();
        let account = rooms.read(cx).rooms().read(cx).account().key.clone();
        if self.palette.is_some() {
            self.close_palette(window, cx);
        }
        let Some(signin) = self.new_signin(true, window, cx) else {
            return;
        };
        self.parked = Some(rooms);
        self.screen = Screen::Signin(signin.clone());
        signin.update(cx, |signin, cx| signin.link_device(account, window, cx));
        cx.notify();
    }

    /// Whether a forwarded link's sign-in shows over the rooms.
    #[cfg(test)]
    pub(crate) fn over_rooms(&self) -> bool {
        self.parked.is_some()
    }

    // -- Notifications --------------------------------------------------------

    /// Posts the session's notifications, a burst at a time.
    fn follow_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(opened) = &self.opened else {
            return;
        };
        let mut incoming = opened.session.notifications();
        self.notifications_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = incoming.recv().await {
                cx.background_executor()
                    .timer(notifications::BURST_WAIT)
                    .await;
                let mut batch: Vec<IncomingNotification> = vec![first];
                while let Ok(more) = incoming.try_recv() {
                    batch.push(more);
                }
                if this
                    .update_in(cx, |this, window, cx| {
                        let focused = this.focused_room(window, cx);
                        notifications::post(
                            batch,
                            focused
                                .as_ref()
                                .map(|(account, room)| (account, room.as_ref())),
                            cx,
                        );
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    /// The room the person is looking at: the open room of the rooms on
    /// screen, while the window has focus.
    pub(crate) fn focused_room(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<(AccountKey, OwnedRoomId)> {
        if !window.is_window_active() {
            return None;
        }
        let rooms = self.signed_in()?.read(cx).rooms().read(cx);
        let room = rooms.room_in_view(window)?.clone();
        Some((rooms.account().key.clone(), room))
    }

    /// A notification was clicked: back to the rooms if a forwarded link's
    /// sign-in showed over them (unless it is busy), and the room opened.
    pub(crate) fn open_notified_room(
        &mut self,
        target: &RoomTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(signin) = self.signin().cloned()
            && self.parked.is_some()
        {
            if signin.read(cx).is_busy() {
                return;
            }
            self.back_to_rooms(window, cx);
        }
        let Some(view) = self.signed_in().cloned() else {
            return;
        };
        let rooms = view.read(cx).rooms().clone();
        if rooms.read(cx).account().key != target.account {
            return;
        }
        rooms.update(cx, |rooms, cx| {
            rooms.open_notified_room(
                target.room_id.clone(),
                target.room_name.clone(),
                target.is_direct,
                target.is_invite,
                window,
                cx,
            );
        });
        notifications::room_seen(&target.account, &target.room_id, cx);
        let focus = rooms.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    // -- Notification settings -----------------------------------------------

    /// Follows the profile's notification settings.
    fn follow_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(opened) = &self.opened else {
            return;
        };
        let mut settings = opened.settings.notification_settings();
        let current = *settings.borrow_and_update();
        self.apply_settings(current, cx);
        self.settings_task = cx.spawn_in(window, async move |this, cx| {
            while settings.changed().await.is_ok() {
                let current = *settings.borrow_and_update();
                if this
                    .update(cx, |this, cx| this.apply_settings(current, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    fn apply_settings(&mut self, settings: NotificationSettings, cx: &mut Context<Self>) {
        self.notification_settings = settings;
        notifications::set_show_content(settings.show_content, cx);
        cx.notify();
    }

    /// The notification settings, and whether their dialog is open.
    #[cfg(test)]
    pub(crate) fn notification_settings(&self) -> (NotificationSettings, bool) {
        (self.notification_settings, self.settings_modal.is_open())
    }

    fn open_notification_settings(
        &mut self,
        _: &OpenNotificationSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_notification_settings(window, cx);
    }

    /// Opens the notification settings dialog (once the profile is open).
    pub(crate) fn show_notification_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.opened.is_none() || self.settings_modal.is_open() {
            return;
        }
        if self.palette.is_some() {
            self.close_palette(window, cx);
        }
        self.settings_problem = None;
        self.settings_modal.open(window, cx);
        cx.notify();
    }

    fn close_notification_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_modal.close(window, cx);
        cx.notify();
    }

    /// Shows or hides the message's text in notifications, and saves it.
    pub(crate) fn toggle_notification_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(opened) = &self.opened else {
            return;
        };
        let settings = NotificationSettings {
            show_content: !self.notification_settings.show_content,
        };
        let saved = opened.settings.set_notification_settings(settings);
        // Shown at once; the profile's answer follows.
        self.apply_settings(settings, cx);
        self.settings_problem = None;
        self.save_task = cx.spawn_in(window, async move |this, cx| {
            let result = saved
                .await
                .unwrap_or_else(|_| Err(format!("{APP_NAME} is closing.")));
            let _ = this.update(cx, |this, cx| {
                if let Err(problem) = result {
                    this.settings_problem = Some(problem);
                    let previous = NotificationSettings {
                        show_content: !settings.show_content,
                    };
                    this.apply_settings(previous, cx);
                }
            });
        });
    }

    fn render_notification_settings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.settings_modal.is_open() {
            return None;
        }
        let colors = ComponentTheme::get(cx).colors;
        let shell = cx.entity().downgrade();
        let close = {
            let shell = shell.clone();
            move |window: &mut Window, cx: &mut App| {
                let _ = shell.update(cx, |shell, cx| {
                    shell.close_notification_settings(window, cx)
                });
            }
        };
        Some(notifications::render_settings(
            &self.settings_modal,
            self.notification_settings.show_content,
            self.settings_problem.as_deref(),
            &colors,
            move |_, window, cx| {
                let _ = shell.update(cx, |shell, cx| {
                    shell.toggle_notification_content(window, cx)
                });
            },
            close,
        ))
    }

    fn apply_initial_view(
        &mut self,
        initial: InitialView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match initial {
            InitialView::NotificationSettings => {
                return self.show_notification_settings(window, cx);
            }
            InitialView::ForwardedLink(text) => {
                return self.request(AppRequest::OpenLink(SecretText::new(text)), window, cx);
            }
            InitialView::LinkDevice => return self.link_device(&LinkDevice, window, cx),
            _ => {}
        }
        let Some(signin) = self.signin().cloned() else {
            return;
        };
        let palette = initial == InitialView::CommandPalette;
        signin.update(cx, |signin, cx| match initial {
            InitialView::Default
            | InitialView::CommandPalette
            | InitialView::Rooms(_)
            | InitialView::NotificationSettings
            | InitialView::ForwardedLink(_)
            | InitialView::LinkDevice => {}
            InitialView::CodeEntry => signin.show_code(None, window, cx),
            InitialView::CodeEntryNoQrCode => {
                signin.show_image_problem(QrError::NoCode, window, cx);
            }
            InitialView::PastedLink(text) => signin.start(
                familychat_core::signin::SigninStart::Link(text.into()),
                window,
                cx,
            ),
            InitialView::Password => signin.sign_in_with_password(window, cx),
            InitialView::OtherServer => signin.use_another_server(window, cx),
        });
        if palette {
            // Once the sign-in has drawn, so its commands are available
            // where focus is (next-frame callbacks run before that frame
            // draws, so this waits for two).
            cx.on_next_frame(window, |_, window, cx| {
                cx.on_next_frame(window, |this, window, cx| this.open_palette(window, cx));
            });
        }
    }

    fn toast(&mut self, toast: Toast, cx: &mut Context<Self>) {
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(toast, cx);
        });
    }

    // -- The command palette -------------------------------------------------

    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
        } else {
            self.open_palette(window, cx);
        }
    }

    /// Opens the palette over the commands that can run where focus is.
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let origin = window
            .focused(cx)
            .unwrap_or_else(|| self.focus_handle.clone());
        let signing_in = self.signin().is_some();
        let signed_in = self.signed_in().is_some();
        let entries: Vec<PaletteEntry> = COMMANDS
            .iter()
            .filter(|command| {
                command.id != "ToggleCommandPalette"
                    && match command.scope {
                        CommandScope::Global => true,
                        CommandScope::SignIn => signing_in,
                        CommandScope::Rooms => signed_in,
                    }
            })
            .filter_map(|command| PaletteEntry::for_command(command, cx))
            .collect();
        // Only what can run from where focus was (or app-wide, as Quit).
        let entries = entries
            .into_iter()
            .filter(|entry| {
                let action = entry.action.as_ref();
                window.is_action_available_in(action, &origin) || cx.is_action_available(action)
            })
            .collect();
        let palette = cx.new(|cx| CommandPalette::new(entries, cx));
        let subscription = cx.subscribe_in(&palette, window, |this, _, event, window, cx| {
            let CommandPaletteEvent::Picked(action) = event;
            let origin = this
                .palette
                .as_ref()
                .map(|(_, origin, _)| origin.clone())
                .unwrap_or_else(|| this.focus_handle.clone());
            this.close_palette(window, cx);
            let action = action.boxed_clone();
            window.defer(cx, move |window, cx| {
                origin.dispatch_action(action.as_ref(), window, cx);
            });
        });
        let field = palette.focus_handle(cx);
        self.palette_modal.open_focusing(&field, window, cx);
        self.palette = Some((palette, origin, subscription));
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_modal.close(window, cx);
        self.palette = None;
        cx.notify();
    }

    fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (palette, _, _) = self.palette.as_ref()?;
        Some(
            Modal::new("command-palette-modal", self.palette_modal.focus_handle())
                .palette()
                .title("Command palette")
                .width(px(PALETTE_WIDTH))
                .child(palette.clone())
                .on_dismiss(cx.listener(|view, _, window, cx| view.close_palette(window, cx)))
                .into_any_element(),
        )
    }

    // -- Rendering ----------------------------------------------------------------

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let sync = self
            .signed_in()
            .map(|view| view.read(cx).status().sync.clone());
        TitleBar::new("title-bar")
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_2))
                    .child(brand_mark(&colors, 18.0))
                    .child(
                        div()
                            .id("title-bar-title")
                            .accessible(Role::Heading, APP_NAME)
                            .type_style(TYPE_BODY_STRONG)
                            .child(APP_NAME),
                    ),
            )
            .when_some(sync, |this, sync| {
                this.child(div().pr(px(SPACE_2)).child(sync_indicator(&sync, &colors)))
            })
            .trailing(
                div().pr(px(SPACE_2)).child(
                    IconButton::new("title-bar-palette", "search", "Command palette")
                        .shortcut(primary_key("ToggleCommandPalette"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_palette(&ToggleCommandPalette, window, cx);
                        })),
                ),
            )
    }
}

impl Focusable for ShellView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_theme(cx);
        let theme = ComponentTheme::get(cx);
        let colors = theme.colors;
        let body: AnyElement = match &self.screen {
            Screen::Profile(screen) => {
                let shell = cx.entity().downgrade();
                profile_screens::render(screen, &colors, move |action, window, cx| {
                    let _ = shell.update(cx, |shell, cx| shell.profile_action(action, window, cx));
                })
            }
            Screen::Signin(signin) => signin.clone().into_any_element(),
            Screen::SignedIn(view) => view.clone().into_any_element(),
        };
        let root = div()
            .id("shell")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::open_notification_settings))
            .on_action(cx.listener(Self::link_device))
            .accessible(Role::Application, APP_NAME)
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .font_family(theme.platform.ui_font.family())
            .bg(rgb(colors.surface))
            .text_color(rgb(colors.foreground))
            .child(self.render_title_bar(cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.toasts.clone())
            .children(self.render_notification_settings(cx))
            .children(self.render_palette(cx));
        window_frame(root, window, cx)
    }
}
