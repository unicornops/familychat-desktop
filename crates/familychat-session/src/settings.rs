//! [`LiveSettings`]: the [`SettingsApi`] over the profile's app store
//! (ADR 0013, ADR 0015). The settings are plaintext preferences (they
//! identify nobody, ADR 0003), read once when the profile opens and written
//! on the blocking pool, never on the UI thread.

use std::sync::Arc;

use familychat_core::settings::{
    AppearanceSettings, ContrastChoice, MotionChoice, NOT_SAVED, NotificationSettings,
    PrivacySettings, SettingsApi, ThemeChoice, nearest_text_size,
};
use familychat_store::{Preference, Profile, StoreError};
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
/// A switch that is on.
const ON: &str = "on";
/// A switch that is off.
const OFF: &str = "off";

fn switch(on: bool) -> &'static str {
    if on { ON } else { OFF }
}

/// The profile's settings.
#[derive(Debug)]
pub struct LiveSettings {
    manager: Arc<AccountManager>,
    handle: Handle,
    notifications: watch::Sender<NotificationSettings>,
    privacy: watch::Sender<PrivacySettings>,
    appearance: watch::Sender<AppearanceSettings>,
}

/// Reads one preference; one that can't be read is `None` (the default).
fn read(profile: &Profile, preference: Preference) -> Option<String> {
    match profile.preference(preference) {
        Ok(value) => value,
        Err(error) => {
            warn!(%error, ?preference, "a setting could not be read");
            None
        }
    }
}

/// Every setting, as the profile has them.
fn load(profile: &Profile) -> (NotificationSettings, PrivacySettings, AppearanceSettings) {
    let off = |preference| read(profile, preference).as_deref() == Some(OFF);
    let notifications = NotificationSettings {
        enabled: !off(Preference::Notifications),
        show_content: read(profile, Preference::NotificationContent).as_deref() != Some(HIDE),
    };
    let privacy = PrivacySettings {
        read_receipts: !off(Preference::ReadReceipts),
        typing_notices: !off(Preference::TypingNotices),
    };
    let appearance = AppearanceSettings {
        theme: read(profile, Preference::Theme)
            .map(|value| ThemeChoice::parse(&value))
            .unwrap_or_default(),
        text_size: read(profile, Preference::TextSize)
            .and_then(|value| value.parse().ok())
            .map(nearest_text_size)
            .unwrap_or(100),
        motion: read(profile, Preference::Motion)
            .map(|value| MotionChoice::parse(&value))
            .unwrap_or_default(),
        contrast: read(profile, Preference::Contrast)
            .map(|value| ContrastChoice::parse(&value))
            .unwrap_or_default(),
    };
    (notifications, privacy, appearance)
}

impl LiveSettings {
    /// Reads the settings from the profile `manager` holds; saves run on
    /// `handle`'s blocking pool. A value that can't be read is the
    /// default.
    pub fn load(manager: Arc<AccountManager>, handle: Handle) -> Self {
        let (notifications, privacy, appearance) = manager.with_profile(load);
        Self {
            manager,
            handle,
            notifications: watch::channel(notifications).0,
            privacy: watch::channel(privacy).0,
            appearance: watch::channel(appearance).0,
        }
    }

    /// Writes `values` on the blocking pool, then publishes `settings` on
    /// `sender`; the answer says whether it worked.
    fn save<T: Send + Sync + 'static>(
        &self,
        sender: &watch::Sender<T>,
        settings: T,
        values: Vec<(Preference, String)>,
    ) -> oneshot::Receiver<Result<(), String>> {
        let (answer, receiver) = oneshot::channel();
        let manager = self.manager.clone();
        let sender = sender.clone();
        let save = self.handle.spawn_blocking(move || {
            manager.with_profile(|profile| {
                values
                    .iter()
                    .try_for_each(|(preference, value)| profile.set_preference(*preference, value))
            })
        });
        self.handle.spawn(async move {
            let result = match save.await {
                Ok(Ok(())) => {
                    sender.send_replace(settings);
                    Ok(())
                }
                Ok(Err::<(), StoreError>(error)) => {
                    warn!(%error, "a setting could not be saved");
                    Err(NOT_SAVED.to_owned())
                }
                Err(_) => Err("Family Chat is closing.".to_owned()),
            };
            let _ = answer.send(result);
        });
        receiver
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
        let content = if settings.show_content { SHOW } else { HIDE };
        self.save(
            &self.notifications,
            settings,
            vec![
                (Preference::NotificationContent, content.to_owned()),
                (
                    Preference::Notifications,
                    switch(settings.enabled).to_owned(),
                ),
            ],
        )
    }

    fn privacy_settings(&self) -> watch::Receiver<PrivacySettings> {
        self.privacy.subscribe()
    }

    fn set_privacy_settings(
        &self,
        settings: PrivacySettings,
    ) -> oneshot::Receiver<Result<(), String>> {
        self.save(
            &self.privacy,
            settings,
            vec![
                (
                    Preference::ReadReceipts,
                    switch(settings.read_receipts).to_owned(),
                ),
                (
                    Preference::TypingNotices,
                    switch(settings.typing_notices).to_owned(),
                ),
            ],
        )
    }

    fn appearance_settings(&self) -> watch::Receiver<AppearanceSettings> {
        self.appearance.subscribe()
    }

    fn set_appearance_settings(
        &self,
        settings: AppearanceSettings,
    ) -> oneshot::Receiver<Result<(), String>> {
        let settings = AppearanceSettings {
            text_size: nearest_text_size(settings.text_size),
            ..settings
        };
        self.save(
            &self.appearance,
            settings,
            vec![
                (Preference::Theme, settings.theme.as_str().to_owned()),
                (Preference::TextSize, settings.text_size.to_string()),
                (Preference::Motion, settings.motion.as_str().to_owned()),
                (Preference::Contrast, settings.contrast.as_str().to_owned()),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use familychat_core::settings::TEXT_SIZES;
    use familychat_security::MemoryKeychain;
    use familychat_store::{DataDir, ProfileState};

    use super::*;
    use crate::{BackgroundRuntime, SessionConfig};

    fn profile_dir(name: &str) -> DataDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        DataDir::at(std::env::temp_dir().join(format!(
            "familychat-settings-{name}-{}-{nanos}",
            std::process::id()
        )))
    }

    fn manager(dir: &DataDir, keychain: &Arc<MemoryKeychain>) -> Arc<AccountManager> {
        let ProfileState::Ready(profile) =
            Profile::open(dir, keychain.clone()).expect("the profile opens")
        else {
            panic!("the profile is ready");
        };
        Arc::new(AccountManager::new(
            profile,
            SessionConfig::new().expect("a client"),
        ))
    }

    fn saved(receiver: oneshot::Receiver<Result<(), String>>, runtime: &BackgroundRuntime) {
        runtime
            .handle()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), receiver)
                    .await
                    .expect("in time")
            })
            .expect("answered")
            .expect("saved");
    }

    #[test]
    fn every_setting_is_saved_and_read_back_when_the_profile_opens_again() {
        let runtime = BackgroundRuntime::start(1).expect("the runtime starts");
        let dir = profile_dir("round-trip");
        let keychain = Arc::new(MemoryKeychain::new());
        let notifications = NotificationSettings {
            enabled: false,
            show_content: false,
        };
        let privacy = PrivacySettings {
            read_receipts: false,
            typing_notices: false,
        };
        let appearance = AppearanceSettings {
            theme: ThemeChoice::Light,
            text_size: TEXT_SIZES[3],
            motion: MotionChoice::Reduce,
            contrast: ContrastChoice::Increased,
        };
        {
            let manager = manager(&dir, &keychain);
            let settings = LiveSettings::load(manager, runtime.handle().clone());
            assert_eq!(
                *settings.appearance_settings().borrow(),
                AppearanceSettings::default()
            );
            saved(settings.set_notification_settings(notifications), &runtime);
            saved(settings.set_privacy_settings(privacy), &runtime);
            saved(settings.set_appearance_settings(appearance), &runtime);
            assert_eq!(*settings.privacy_settings().borrow(), privacy);
        }
        let manager = manager(&dir, &keychain);
        let settings = LiveSettings::load(manager, runtime.handle().clone());
        assert_eq!(*settings.notification_settings().borrow(), notifications);
        assert_eq!(*settings.privacy_settings().borrow(), privacy);
        assert_eq!(*settings.appearance_settings().borrow(), appearance);
        let _ = std::fs::remove_dir_all(dir.root());
    }
}
