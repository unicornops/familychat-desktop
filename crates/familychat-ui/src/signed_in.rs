//! `SignedInView`: what the window shows once an account is signed in: its
//! rooms and timelines ([`RoomsView`]), with a banner above them when the
//! account's sync needs saying something about (offline, paused, signed
//! out, not supported, an error), and Try again where a retry helps.
//!
//! It also holds the account's [`EncryptionView`] (ADR 0009): "Confirm
//! it's you" (or the page asking for the password) takes the rooms' place
//! while it applies, its banners sit with the sync banner, its dialogs over
//! everything, and the verification requests the account's other devices
//! send open its verification dialog.
//!
//! [`sync_indicator`] is the small status pill the title bar shows for the
//! account. Each state has its own colour and icon; the suspended state
//! (`M_USER_SUSPENDED`, a Family Chat curfew or an administrator) is
//! "Paused" with a pause icon in the info colour, never an error.

use std::sync::Arc;

use familychat_core::{
    session::{AccountView, SessionApi},
    sync::{AccountStatus, SyncState},
};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Render,
    Role, Task, Window, div, prelude::*, px, rgb,
};
use tokio::sync::watch;

use familychat_actions::ComposerState;

use crate::a11y::{A11yFlags, AccessibleExt, Politeness};
use crate::actions::{
    ConfirmIdentity, ExportRoomKeys, ImportRoomKeys, ManageDevices, SetUpRecovery,
};
use crate::components::{Banner, BannerTone, Button, ComponentTheme, ToastHost};
use crate::encryption::EncryptionView;
use crate::rooms::{RoomsLaunch, RoomsView};
use crate::signin::spinner;
use crate::styles::{
    ICON_SIZE_SM, RADIUS_FULL, SPACE_1, SPACE_2, SPACE_4, TYPE_CAPTION, TypeStyleExt, UiColors,
    contrast::mix,
};
use crate::widgets::icon;

/// The signed-in window: the sync banner and the rooms.
pub(crate) struct SignedInView {
    session: Arc<dyn SessionApi>,
    account: AccountView,
    status: AccountStatus,
    rooms: Entity<RoomsView>,
    encryption: Entity<EncryptionView>,
    /// Whether the encryption screen showed when last looked at.
    encryption_screen: bool,
    _status_task: Task<()>,
    _verification_task: Task<()>,
    _encryption_changes: gpui::Subscription,
}

impl SignedInView {
    /// The view of `account`, following its status.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountView,
        privacy: watch::Receiver<familychat_core::settings::PrivacySettings>,
        toasts: Entity<ToastHost>,
        mut launch: Option<RoomsLaunch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let encryption_launch = launch.as_mut().and_then(|launch| launch.encryption.take());
        let receiver = session.account_status(&account.key);
        let status = receiver
            .as_ref()
            .map(|receiver| receiver.borrow().clone())
            .unwrap_or_default();
        let task = match receiver {
            Some(receiver) => follow(receiver, window, cx),
            None => Task::ready(()),
        };
        let encryption = {
            let session = session.clone();
            let key = account.key.clone();
            let toasts = toasts.clone();
            cx.new(|cx| EncryptionView::new(session, key, toasts, window, cx))
        };
        let rooms = {
            let session = session.clone();
            let account = account.clone();
            cx.new(|cx| RoomsView::new(session, account, privacy, toasts, launch, window, cx))
        };
        let encryption_changes =
            cx.observe_in(&encryption, window, |this, encryption, window, cx| {
                let screen = encryption.read(cx).takes_over();
                if screen != this.encryption_screen {
                    this.encryption_screen = screen;
                    let focus = this.focus_handle(cx);
                    window.focus(&focus, cx);
                }
                cx.notify();
            });
        let verification_task = follow_verification_requests(
            session.verification_requests(),
            account.key.clone(),
            encryption.clone(),
            window,
            cx,
        );
        if let Some(launch) = encryption_launch {
            encryption.update(cx, |encryption, cx| encryption.launch(launch, window, cx));
        }
        let encryption_screen = encryption.read(cx).takes_over();
        Self {
            session,
            account,
            status,
            rooms,
            encryption,
            encryption_screen,
            _status_task: task,
            _verification_task: verification_task,
            _encryption_changes: encryption_changes,
        }
    }

    /// The account's encryption screens and dialogs.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn encryption(&self) -> &Entity<EncryptionView> {
        &self.encryption
    }

    /// The account.
    pub(crate) fn account(&self) -> &AccountView {
        &self.account
    }

    /// Its latest status.
    pub(crate) fn status(&self) -> &AccountStatus {
        &self.status
    }

    /// The rooms.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rooms(&self) -> &Entity<RoomsView> {
        &self.rooms
    }

    /// Asks the session to try the sync again now.
    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        self.session.retry_sync(&self.account.key);
        cx.notify();
    }

    /// The banner the sync state needs, if any.
    fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sync = &self.status.sync;
        let can_retry = matches!(
            sync,
            SyncState::Offline { .. }
                | SyncState::Error { .. }
                | SyncState::NotSupported
                | SyncState::SignedOut { .. }
        );
        let tone = match sync {
            SyncState::Idle | SyncState::Connecting | SyncState::Running => None,
            SyncState::Suspended { .. } => Some(BannerTone::Info),
            SyncState::Offline { .. } => Some(BannerTone::Caution),
            SyncState::NotSupported | SyncState::SignedOut { .. } | SyncState::Error { .. } => {
                Some(BannerTone::Danger)
            }
        };
        let banner = tone.map(|tone| {
            let message = sync.message().unwrap_or_default();
            // A curfew's banner says what the person sees: "Chat is
            // paused" (the composer is read-only meanwhile).
            let title = match sync {
                SyncState::Suspended { .. } => ComposerState::PAUSED_TITLE,
                other => other.label(),
            };
            Banner::new("signed-in-sync", message)
                .tone(tone)
                .title(title)
                .when(can_retry, |banner| {
                    banner.action(
                        Button::new("signed-in-retry", "Try again")
                            .icon("refresh-cw")
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    )
                })
                .into_any_element()
        });
        let problem = self.status.problem.clone().map(|problem| {
            Banner::new("signed-in-problem", problem.message())
                .tone(BannerTone::Caution)
                .into_any_element()
        });
        if banner.is_none() && problem.is_none() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .px(px(SPACE_4))
                .py(px(SPACE_2))
                .children(banner)
                .children(problem)
                .into_any_element(),
        )
    }
}

impl Focusable for SignedInView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        let encryption = self.encryption.read(cx);
        if encryption.takes_over() {
            encryption.focus_handle().clone()
        } else {
            self.rooms.read(cx).focus_handle(cx)
        }
    }
}

/// Opens each verification request another of the account's devices sends
/// in the encryption view's dialog.
fn follow_verification_requests(
    mut requests: tokio::sync::mpsc::Receiver<(
        familychat_core::session::AccountKey,
        familychat_core::encryption::VerificationFlow,
    )>,
    account: familychat_core::session::AccountKey,
    encryption: Entity<EncryptionView>,
    window: &mut Window,
    cx: &mut Context<SignedInView>,
) -> Task<()> {
    cx.spawn_in(window, async move |_, cx| {
        while let Some((for_account, flow)) = requests.recv().await {
            if for_account != account {
                flow.answer(familychat_core::encryption::VerificationAnswer::Cancel);
                continue;
            }
            if encryption
                .update_in(cx, |encryption, window, cx| {
                    encryption.incoming(flow, window, cx)
                })
                .is_err()
            {
                break;
            }
        }
    })
}

/// Follows `receiver` into the view's status.
fn follow(
    mut receiver: watch::Receiver<AccountStatus>,
    window: &mut Window,
    cx: &mut Context<SignedInView>,
) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        while receiver.changed().await.is_ok() {
            let status = receiver.borrow_and_update().clone();
            if this
                .update(cx, |this, cx| {
                    this.status = status;
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    })
}

/// How a state looks: its icon, its colour and its fill.
fn look(sync: &SyncState, colors: &UiColors) -> (&'static str, u32, u32) {
    match sync {
        SyncState::Running => ("circle-check", colors.success, colors.success_background),
        SyncState::Idle | SyncState::Connecting => (
            "loader",
            colors.info,
            mix(colors.background, colors.info, 0.12),
        ),
        SyncState::Offline { .. } => ("wifi-off", colors.warning, colors.warning_background),
        SyncState::Suspended { .. } => (
            "circle-pause",
            colors.info,
            mix(colors.background, colors.info, 0.12),
        ),
        SyncState::NotSupported | SyncState::SignedOut { .. } | SyncState::Error { .. } => {
            ("alert-circle", colors.danger, colors.danger_background)
        }
    }
}

/// The sync-state pill for the title bar.
pub(crate) fn sync_indicator(sync: &SyncState, colors: &UiColors) -> AnyElement {
    let (icon_name, color, fill) = look(sync, colors);
    let label = sync.label();
    div()
        .id("sync-indicator")
        .accessible(Role::Status, format!("Sync: {label}"))
        .a11y_flags(A11yFlags {
            live: Some(Politeness::Polite),
            ..A11yFlags::default()
        })
        .flex()
        .flex_none()
        .items_center()
        .gap(px(SPACE_1))
        .px(px(SPACE_2))
        .py(px(2.0))
        .rounded(px(RADIUS_FULL))
        .bg(rgb(fill))
        .text_color(rgb(color))
        .type_style(TYPE_CAPTION)
        .child(if matches!(sync, SyncState::Connecting | SyncState::Idle) {
            spinner("sync-indicator-spinner", ICON_SIZE_SM, color)
        } else {
            icon(icon_name, ICON_SIZE_SM, color).into_any_element()
        })
        .child(label)
        .into_any_element()
}

impl Render for SignedInView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let banner = self.render_banner(cx);
        let _ = ComponentTheme::get(cx);
        let screen = self.encryption.read(cx).takes_over();
        let (body, encryption_banners, dialog) = self.encryption.update(cx, |encryption, cx| {
            let body = screen.then(|| encryption.render_screen(cx));
            let banners = if screen {
                Vec::new()
            } else {
                encryption.render_banners(cx)
            };
            (body, banners, encryption.render_dialog(cx))
        });
        let encryption_banners = (!encryption_banners.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .px(px(SPACE_4))
                .py(px(SPACE_2))
                .children(encryption_banners)
        });
        let encryption = self.encryption.clone();
        div()
            .id("signed-in")
            .size_full()
            .flex()
            .flex_col()
            .on_action({
                let encryption = encryption.clone();
                move |_: &ConfirmIdentity, window, cx| {
                    encryption.update(cx, |view, cx| view.confirm_identity(window, cx));
                }
            })
            .on_action({
                let encryption = encryption.clone();
                move |_: &SetUpRecovery, window, cx| {
                    encryption.update(cx, |view, cx| view.set_up_recovery(window, cx));
                }
            })
            .on_action({
                let encryption = encryption.clone();
                move |_: &ManageDevices, window, cx| {
                    encryption.update(cx, |view, cx| view.manage_devices(window, cx));
                }
            })
            .on_action({
                let encryption = encryption.clone();
                move |_: &ExportRoomKeys, window, cx| {
                    encryption.update(cx, |view, cx| view.export_keys(window, cx));
                }
            })
            .on_action({
                let encryption = encryption.clone();
                move |_: &ImportRoomKeys, window, cx| {
                    encryption.update(cx, |view, cx| view.import_keys(window, cx));
                }
            })
            .children(banner)
            .children(encryption_banners)
            .child(match body {
                Some(screen) => div().flex_1().min_h_0().child(screen),
                None => div().flex_1().min_h_0().child(self.rooms.clone()),
            })
            .children(dialog)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn suspended_looks_like_neither_an_error_nor_offline() {
        let colors = UiColors::fallback();
        let suspended = look(
            &SyncState::Suspended {
                recheck_in: Duration::from_secs(30),
            },
            &colors,
        );
        let error = look(
            &SyncState::Error {
                message: "x".to_owned(),
                retry_in: Duration::from_secs(1),
            },
            &colors,
        );
        let offline = look(
            &SyncState::Offline {
                retry_in: Duration::from_secs(1),
            },
            &colors,
        );
        assert_eq!(suspended.0, "circle-pause");
        assert_ne!(suspended, error);
        assert_ne!(suspended, offline);
        assert_ne!(suspended.1, colors.danger);
    }
}
