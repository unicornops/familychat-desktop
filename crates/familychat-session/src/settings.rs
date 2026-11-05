//! [`LiveSettings`]: the [`SettingsApi`] over the profile's app store
//! (ADR 0013). The settings are plaintext preferences (they identify
//! nobody, ADR 0003), read once when the profile opens and written on the
//! blocking pool, never on the UI thread.

use std::sync::Arc;

use familychat_core::settings::{NotificationSettings, SettingsApi};
use familychat_store::Preference;
use tokio::{
    runtime::Handle,
    sync::{oneshot, watch},
};
use tracing::warn;

use crate::AccountManager;

/// The stored value that hides a notification's text.
const HIDE: &str = "hide";
/// The stored value that shows it.
const SHOW: &str = "show";

/// The profile's settings.
#[derive(Debug)]
pub struct LiveSettings {
    manager: Arc<AccountManager>,
    handle: Handle,
    notifications: watch::Sender<NotificationSettings>,
}

impl LiveSettings {
    /// Reads the settings from the profile `manager` holds; saves run on
    /// `handle`'s blocking pool. A value that can't be read is the
    /// default.
    pub fn load(manager: Arc<AccountManager>, handle: Handle) -> Self {
        let stored =
            manager.with_profile(|profile| profile.preference(Preference::NotificationContent));
        let notifications = match stored {
            Ok(value) => NotificationSettings {
                show_content: value.as_deref() != Some(HIDE),
            },
            Err(error) => {
                warn!(%error, "the notification settings could not be read");
                NotificationSettings::default()
            }
        };
        Self {
            manager,
            handle,
            notifications: watch::channel(notifications).0,
        }
    }
}

impl SettingsApi for LiveSettings {
    fn notification_settings(&self) -> watch::Receiver<NotificationSettings> {
        self.notifications.subscribe()
    }

    fn set_notification_settings(
        &self,
        settings: NotificationSettings,
    ) -> oneshot::Receiver<Result<(), String>> {
        let (answer, receiver) = oneshot::channel();
        let manager = self.manager.clone();
        let notifications = self.notifications.clone();
        let save = self.handle.spawn_blocking(move || {
            let value = if settings.show_content { SHOW } else { HIDE };
            manager.with_profile(|profile| {
                profile.set_preference(Preference::NotificationContent, value)
            })
        });
        self.handle.spawn(async move {
            let result = match save.await {
                Ok(Ok(())) => {
                    notifications.send_replace(settings);
                    Ok(())
                }
                Ok(Err(error)) => {
                    warn!(%error, "the notification settings could not be saved");
                    Err("Family Chat couldn't save that setting. Try again.".to_owned())
                }
                Err(_) => Err("Family Chat is closing.".to_owned()),
            };
            let _ = answer.send(result);
        });
        receiver
    }
}
