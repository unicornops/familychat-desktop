//! `SignedInView`: what the window shows once an account is signed in,
//! until the room list and the timeline arrive (Phase 6): the account, its
//! sync state in words, and a retry when the session waits for one.
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
    AnyElement, Context, IntoElement, Render, Role, SharedString, Task, Window, div, prelude::*,
    px, rgb,
};
use tokio::sync::watch;

use crate::a11y::{A11yFlags, AccessibleExt, Politeness, readable};
use crate::components::{Avatar, Banner, BannerTone, Button, ComponentTheme};
use crate::signin::{CARD_WIDTH, brand_mark, spinner};
use crate::styles::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_XL, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6,
    SPACE_8, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE, TypeStyleExt, UiColors,
    contrast::mix,
};
use crate::widgets::icon;

/// The signed-in placeholder.
pub(crate) struct SignedInView {
    session: Arc<dyn SessionApi>,
    account: AccountView,
    status: AccountStatus,
    _status_task: Task<()>,
}

impl SignedInView {
    /// The view of `account`, following its status.
    pub(crate) fn new(
        session: Arc<dyn SessionApi>,
        account: AccountView,
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
        Self {
            session,
            account,
            status,
            _status_task: task,
        }
    }

    /// Its latest status.
    pub(crate) fn status(&self) -> &AccountStatus {
        &self.status
    }

    /// Asks the session to try the sync again now.
    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        self.session.retry_sync(&self.account.key);
        cx.notify();
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
        let colors = ComponentTheme::get(cx).colors;
        let sync = self.status.sync.clone();
        let user = self.account.user_id.to_string();
        let server = self.account.user_id.server_name().to_string();
        let (icon_name, color, fill) = look(&sync, &colors);
        let message: SharedString = sync
            .message()
            .unwrap_or_else(|| match sync {
                SyncState::Running => "New messages arrive as they are sent.".to_owned(),
                _ => "Family Chat is connecting to your server.".to_owned(),
            })
            .into();
        let can_retry = matches!(
            sync,
            SyncState::Offline { .. }
                | SyncState::Error { .. }
                | SyncState::NotSupported
                | SyncState::SignedOut { .. }
        );
        div()
            .id("signed-in")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .px(px(SPACE_6))
            .py(px(SPACE_8))
            .child(
                div()
                    .id("signed-in-card")
                    .accessible(Role::Main, "Your account")
                    .w_full()
                    .max_w(px(CARD_WIDTH))
                    .my_auto()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_6))
                    .p(px(SPACE_8))
                    .rounded(px(RADIUS_XL))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_4))
                            .child(
                                Avatar::new(self.account.user_id.localpart().to_owned())
                                    .address(user.clone())
                                    .size(56.0),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .gap(px(SPACE_1))
                                    .child(
                                        div()
                                            .id("signed-in-heading")
                                            .accessible(Role::Heading, "You're signed in")
                                            .aria_level(1)
                                            .type_style(TYPE_HEADLINE)
                                            .child("You're signed in"),
                                    )
                                    .child(
                                        div()
                                            .type_style(TYPE_BODY)
                                            .text_color(rgb(colors.muted))
                                            .child(readable("signed-in-user", user)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("signed-in-sync")
                            .accessible(Role::Status, format!("{}. {message}", sync.label()))
                            .flex()
                            .items_start()
                            .gap(px(SPACE_3))
                            .p(px(SPACE_4))
                            .rounded(px(RADIUS_LG))
                            .bg(rgb(fill))
                            .child(icon(icon_name, 20.0, color))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(SPACE_1))
                                    .child(
                                        div()
                                            .type_style(TYPE_BODY_STRONG)
                                            .text_color(rgb(colors.foreground))
                                            .child(sync.label()),
                                    )
                                    .child(
                                        div()
                                            .type_style(TYPE_BODY)
                                            .text_color(rgb(colors.foreground))
                                            .child(message),
                                    )
                                    .child(
                                        div()
                                            .type_style(TYPE_CAPTION)
                                            .text_color(rgb(colors.muted))
                                            .child(format!("Server: {server}")),
                                    ),
                            )
                            .when(can_retry, |this| {
                                this.child(
                                    Button::new("signed-in-retry", "Try again")
                                        .icon("refresh-cw")
                                        .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                                )
                            }),
                    )
                    .when_some(self.status.problem.clone(), |this, problem| {
                        this.child(
                            Banner::new("signed-in-problem", problem.message())
                                .tone(BannerTone::Caution),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_3))
                            .pt(px(SPACE_4))
                            .border_t_1()
                            .border_color(rgb(colors.border))
                            .child(brand_mark(&colors, 28.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(colors.muted))
                                    .child(readable(
                                        "signed-in-next",
                                        "Your rooms and messages appear here in the next \
                                         release of this alpha.",
                                    )),
                            ),
                    ),
            )
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
