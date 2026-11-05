//! `SignedInView`: what the window shows once an account is signed in: its
//! rooms and timelines ([`RoomsView`]), with a banner above them when the
//! account's sync needs saying something about (offline, paused, signed
//! out, not supported, an error), and Try again where a retry helps.
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
use crate::components::{Banner, BannerTone, Button, ComponentTheme, ToastHost};
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
    _status_task: Task<()>,
}

impl SignedInView {
    /// The view of `account`, following its status.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountView,
        toasts: Entity<ToastHost>,
        launch: Option<RoomsLaunch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let receiver = session.account_status(&account.key);
        let status = receiver
            .as_ref()
            .map(|receiver| receiver.borrow().clone())
            .unwrap_or_default();
        let task = match receiver {
            Some(receiver) => follow(receiver, window, cx),
            None => Task::ready(()),
        };
        let rooms = {
            let session = session.clone();
            let account = account.clone();
            cx.new(|cx| RoomsView::new(session, account, toasts, launch, window, cx))
        };
        Self {
            session,
            account,
            status,
            rooms,
            _status_task: task,
        }
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
        self.rooms.read(cx).focus_handle(cx)
    }
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
        div()
            .id("signed-in")
            .size_full()
            .flex()
            .flex_col()
            .children(banner)
            .child(div().flex_1().min_h_0().child(self.rooms.clone()))
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
