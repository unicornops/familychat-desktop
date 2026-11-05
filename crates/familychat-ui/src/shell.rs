//! `ShellView`: the window's root.
//!
//! It asks the app's `StartupApi` to open the profile (off the UI thread)
//! and shows what comes back: the profile screens while it is not ready,
//! the sign-in while no account is signed in, and the signed-in view once
//! one is. It draws the title bar (Zmail's, so GNOME on Wayland, which has
//! no server-side decorations, still gets one) and the window frame, keeps
//! the theme in step with the OS appearance, and owns the command palette
//! and the toasts.

use std::sync::Arc;

use familychat_core::{
    AppBootstrap, ThemeAppearance,
    commands::{COMMANDS, CommandScope},
    identity::APP_NAME,
    session::AccountView,
    startup::{OpenState, Opened, StartupApi},
};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Render,
    Role, Subscription, Task, Window, div, prelude::*, px, rgb,
};

use crate::a11y::AccessibleExt;
use crate::actions::ToggleCommandPalette;
use crate::command_palette::{CommandPalette, CommandPaletteEvent, PALETTE_WIDTH, PaletteEntry};
use crate::components::{
    ComponentTheme, IconButton, Modal, ModalState, TitleBar, Toast, ToastHost, ToastKind,
    window_frame,
};
use crate::profile_screens::{self, ProfileAction, ProfileScreen};
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
    _subscriptions: Vec<Subscription>,
}

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
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
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
            _subscriptions: vec![appearance_changes],
        };
        view.sync_theme(cx);
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
        if let Some(initial) = self.initial_view.take() {
            self.apply_initial_view(initial, window, cx);
        }
    }

    fn show_signin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(opened) = &self.opened else {
            return;
        };
        let signin = opened.signin.clone();
        let view = cx.new(|cx| SigninView::new(signin, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let SigninEvent::Finished(finished) = event;
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
        self.screen = Screen::Signin(view);
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
        let view = cx.new(|cx| SignedInView::new(session, account, toasts, launch, window, cx));
        let focus = view.focus_handle(cx);
        self.screen = Screen::SignedIn(view);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn apply_initial_view(
        &mut self,
        initial: InitialView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(signin) = self.signin().cloned() else {
            return;
        };
        let palette = initial == InitialView::CommandPalette;
        signin.update(cx, |signin, cx| match initial {
            InitialView::Default | InitialView::CommandPalette | InitialView::Rooms(_) => {}
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
            .children(self.render_palette(cx));
        window_frame(root, window, cx)
    }
}
