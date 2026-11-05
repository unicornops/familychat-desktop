//! The profile's settings a frontend reads and changes (ADR 0013).
//!
//! Only non-identifying preferences live here: they are the app store's
//! plaintext preferences (ADR 0003), read and written off the UI thread by
//! the app's wiring. Phase 13 adds the rest of the settings screens; Phase
//! 9 needs one: whether notifications show the message's text.

use tokio::sync::{oneshot, watch};

/// How notifications read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationSettings {
    /// Show the message's text in a notification. When off, a notification
    /// names the sender and the room and says "New message".
    pub show_content: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self { show_content: true }
    }
}

/// Reads and saves the settings. Each call returns at once; a save is
/// answered on its receiver once it is on disk.
pub trait SettingsApi: Send + Sync {
    /// The notification settings, as they change.
    fn notification_settings(&self) -> watch::Receiver<NotificationSettings>;

    /// Saves the notification settings; the answer is the sentence to show
    /// when they could not be saved.
    fn set_notification_settings(
        &self,
        settings: NotificationSettings,
    ) -> oneshot::Receiver<Result<(), String>>;
}

/// Settings kept in memory only, for fixtures and tests (and a frontend
/// whose profile has none).
#[derive(Debug)]
pub struct MemorySettings {
    notifications: watch::Sender<NotificationSettings>,
}

impl MemorySettings {
    /// Settings starting at `notifications`.
    pub fn new(notifications: NotificationSettings) -> Self {
        Self {
            notifications: watch::channel(notifications).0,
        }
    }
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self::new(NotificationSettings::default())
    }
}

impl SettingsApi for MemorySettings {
    fn notification_settings(&self) -> watch::Receiver<NotificationSettings> {
        self.notifications.subscribe()
    }

    fn set_notification_settings(
        &self,
        settings: NotificationSettings,
    ) -> oneshot::Receiver<Result<(), String>> {
        self.notifications.send_replace(settings);
        let (answer, receiver) = oneshot::channel();
        let _ = answer.send(Ok(()));
        receiver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_shows_by_default_and_memory_settings_follow_changes() {
        assert!(NotificationSettings::default().show_content);
        let settings = MemorySettings::default();
        let mut receiver = settings.notification_settings();
        let mut saved = settings.set_notification_settings(NotificationSettings {
            show_content: false,
        });
        assert_eq!(saved.try_recv(), Ok(Ok(())));
        assert!(receiver.has_changed().unwrap_or(false));
        assert!(!receiver.borrow_and_update().show_content);
    }
}
