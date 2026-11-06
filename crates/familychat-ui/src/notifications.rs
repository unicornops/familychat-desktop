//! New-message notifications, posted through GPUI's system notifications
//! (`UNUserNotificationCenter` on macOS, toasts on Windows, the
//! freedesktop notification service on Linux); a click brings the window
//! forward with the room open (PLAN.md, section 5.9; ADR 0013).
//!
//! The session reports each notification on its receiver
//! (`SessionApi::notifications`). The shell waits a moment after the first
//! of a burst so the rest of it (one sync's worth) arrives too, then hands
//! the batch to `familychat-notify`'s [`Notifier`] with the room the
//! person is looking at, and posts what it says. Each notification has an
//! opaque tag and the room it opens stays here, so no Matrix id reaches
//! the operating system's notification store. A click on a notification
//! from an earlier run, or for a room the notifier no longer remembers,
//! only brings the window forward.
//!
//! Whether notifications show at all, and whether they show the message's
//! text, are the profile's settings (`SettingsApi`), changed in Settings →
//! Notifications (ADR 0015).
//!
//! Adapted from Zmail's `zmail-ui` (`src/notifications.rs`,
//! unicornops/zmail, GPL-3.0-only like this project).

use std::time::{Duration, Instant};

use familychat_core::{RoomId, notifications::IncomingNotification, session::AccountKey};
use familychat_notify::{Notifier, RoomTarget};
use gpui::{App, Global, SystemNotification, SystemNotificationResponse};

use crate::shell::ShellView;

/// How long the shell waits after the first notification of a burst for
/// the rest of it, so a room's messages from one sync make one
/// notification.
pub(crate) const BURST_WAIT: Duration = Duration::from_millis(150);

/// The app's notifier, shared with the click handler.
pub(crate) struct Notifications {
    notifier: Notifier,
    /// Settings → Notifications → Show notifications.
    enabled: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            notifier: Notifier::default(),
            enabled: true,
        }
    }
}

impl Global for Notifications {}

/// Sets up the notifier and the click handler. Once per window (the
/// handler is the app's; a later call replaces it).
pub(crate) fn init(cx: &mut App) {
    if cx.try_global::<Notifications>().is_none() {
        cx.set_global(Notifications::default());
    }
    cx.on_system_notification_response(open_clicked);
}

/// Shows the message's text in notifications, or "New message".
pub(crate) fn set_show_content(show: bool, cx: &mut App) {
    cx.global_mut::<Notifications>()
        .notifier
        .set_show_content(show);
}

/// Posts notifications, or none at all.
pub(crate) fn set_enabled(enabled: bool, cx: &mut App) {
    cx.global_mut::<Notifications>().enabled = enabled;
}

/// Posts what `batch` calls for, given the room the person is looking at;
/// nothing when notifications are turned off.
pub(crate) fn post(
    batch: Vec<IncomingNotification>,
    focused: Option<(&AccountKey, &RoomId)>,
    cx: &mut App,
) {
    if !cx.global::<Notifications>().enabled {
        return;
    }
    let posted = cx
        .global_mut::<Notifications>()
        .notifier
        .notify(batch, focused, Instant::now());
    for notification in posted {
        cx.show_system_notification(SystemNotification {
            tag: notification.tag.into(),
            title: notification.title.into(),
            body: notification.body.into(),
            actions: Vec::new(),
        });
    }
}

/// The room is in front of the person: its count starts again and its
/// notification comes down.
pub(crate) fn room_seen(account: &AccountKey, room: &RoomId, cx: &mut App) {
    if let Some(tag) = cx
        .global_mut::<Notifications>()
        .notifier
        .room_seen(account, room)
    {
        cx.dismiss_system_notification(&tag);
    }
}

/// Brings the window forward on the notification's room.
fn open_clicked(response: SystemNotificationResponse, cx: &mut App) {
    let target: Option<RoomTarget> = cx
        .try_global::<Notifications>()
        .and_then(|notifications| notifications.notifier.target(&response.tag))
        .cloned();
    let Some(shell) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<ShellView>())
    else {
        return;
    };
    cx.activate(true);
    let _ = shell.update(cx, |shell, window, cx| {
        window.activate_window();
        if let Some(target) = target {
            shell.open_notified_room(&target, window, cx);
        }
    });
}
