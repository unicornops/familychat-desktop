//! The profile's settings a frontend reads and changes (ADR 0013, ADR
//! 0015).
//!
//! Only non-identifying preferences live here: they are the app store's
//! plaintext preferences (ADR 0003), read and written off the UI thread by
//! the app's wiring. Three groups, each with its own watch and save:
//!
//! - [`NotificationSettings`]: whether notifications show at all, and
//!   whether they show the message's text (Phase 9);
//! - [`PrivacySettings`]: whether read receipts and typing notices are
//!   sent (`familychat-actions` honours them);
//! - [`AppearanceSettings`]: the theme, the text size, motion and
//!   contrast.

use tokio::sync::{oneshot, watch};

/// How notifications read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationSettings {
    /// Post system notifications at all. When off, nothing is posted; the
    /// window's title still counts the unread messages.
    pub enabled: bool,
    /// Show the message's text in a notification. When off, a notification
    /// names the sender and the room and says "New message".
    pub show_content: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            show_content: true,
        }
    }
}

/// What the account tells the people it talks to, beyond its messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivacySettings {
    /// Send public read receipts. When off, the read position is still
    /// kept (a private receipt, `m.read.private`, and the read marker), so
    /// the account's other devices and the unread counts stay right, but
    /// nobody else sees what was read.
    pub read_receipts: bool,
    /// Tell a room while the person is typing.
    pub typing_notices: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            read_receipts: true,
            typing_notices: true,
        }
    }
}

/// Light, dark, or the operating system's appearance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl ThemeChoice {
    /// Every choice, in the order a frontend offers them.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// What a frontend calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Match system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The choice a stored value names; the default for anything else.
    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// Whether animations follow the system, or are always reduced or full.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MotionChoice {
    /// Follow the operating system's reduce-motion setting.
    #[default]
    System,
    /// Never animate.
    Reduce,
    /// Always animate.
    Full,
}

impl MotionChoice {
    /// Every choice, in the order a frontend offers them.
    pub const ALL: [Self; 3] = [Self::System, Self::Reduce, Self::Full];

    /// What a frontend calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Match system",
            Self::Reduce => "Reduce",
            Self::Full => "Full",
        }
    }

    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Reduce => "reduce",
            Self::Full => "full",
        }
    }

    /// The choice a stored value names; the default for anything else.
    pub fn parse(value: &str) -> Self {
        match value {
            "reduce" => Self::Reduce,
            "full" => Self::Full,
            _ => Self::System,
        }
    }
}

/// Whether the palette follows the system's contrast settings or always
/// raises contrast.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContrastChoice {
    /// Follow the system: macOS Increase Contrast and Windows contrast
    /// themes.
    #[default]
    System,
    /// Always increase contrast (stronger secondary text and borders), on
    /// every platform; a Windows contrast theme still wins.
    Increased,
}

impl ContrastChoice {
    /// Every choice, in the order a frontend offers them.
    pub const ALL: [Self; 2] = [Self::System, Self::Increased];

    /// What a frontend calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Match system",
            Self::Increased => "Increased",
        }
    }

    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Increased => "increased",
        }
    }

    /// The choice a stored value names; the default for anything else.
    pub fn parse(value: &str) -> Self {
        match value {
            "increased" => Self::Increased,
            _ => Self::System,
        }
    }
}

/// The text sizes a frontend offers, in percent (Zmail's: 100% to 200% in
/// 25% steps).
pub const TEXT_SIZES: [u16; 5] = [100, 125, 150, 175, 200];

/// How the window looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceSettings {
    /// Light, dark or the system's.
    pub theme: ThemeChoice,
    /// The text size in percent, one of [`TEXT_SIZES`].
    pub text_size: u16,
    /// Reduced motion.
    pub motion: MotionChoice,
    /// Contrast.
    pub contrast: ContrastChoice,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            text_size: 100,
            motion: MotionChoice::System,
            contrast: ContrastChoice::System,
        }
    }
}

/// The nearest offered text size to `percent` (a stored value that was
/// edited by hand, or from another version, still means something).
pub fn nearest_text_size(percent: u16) -> u16 {
    TEXT_SIZES
        .into_iter()
        .min_by_key(|size| size.abs_diff(percent))
        .unwrap_or(100)
}

/// The sentence a frontend shows when a setting could not be saved.
pub const NOT_SAVED: &str = "Family Chat couldn't save that setting. Try again.";

/// Reads and saves the settings. Each call returns at once; a save is
/// answered on its receiver once it is on disk, and the watch changes
/// then.
pub trait SettingsApi: Send + Sync {
    /// The notification settings, as they change.
    fn notification_settings(&self) -> watch::Receiver<NotificationSettings>;

    /// Saves the notification settings; the answer is the sentence to show
    /// when they could not be saved.
    fn set_notification_settings(
        &self,
        settings: NotificationSettings,
    ) -> oneshot::Receiver<Result<(), String>>;

    /// The privacy settings, as they change.
    fn privacy_settings(&self) -> watch::Receiver<PrivacySettings>;

    /// Saves the privacy settings.
    fn set_privacy_settings(
        &self,
        settings: PrivacySettings,
    ) -> oneshot::Receiver<Result<(), String>>;

    /// The appearance settings, as they change.
    fn appearance_settings(&self) -> watch::Receiver<AppearanceSettings>;

    /// Saves the appearance settings.
    fn set_appearance_settings(
        &self,
        settings: AppearanceSettings,
    ) -> oneshot::Receiver<Result<(), String>>;
}

/// Settings kept in memory only, for fixtures and tests (and a frontend
/// whose profile has none).
#[derive(Debug)]
pub struct MemorySettings {
    notifications: watch::Sender<NotificationSettings>,
    privacy: watch::Sender<PrivacySettings>,
    appearance: watch::Sender<AppearanceSettings>,
    refuse: std::sync::atomic::AtomicBool,
}

impl MemorySettings {
    /// Settings starting at `notifications`, with the default privacy and
    /// appearance settings.
    pub fn new(notifications: NotificationSettings) -> Self {
        Self {
            notifications: watch::channel(notifications).0,
            privacy: watch::channel(PrivacySettings::default()).0,
            appearance: watch::channel(AppearanceSettings::default()).0,
            refuse: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Starts the privacy settings at `privacy`.
    pub fn with_privacy(self, privacy: PrivacySettings) -> Self {
        self.privacy.send_replace(privacy);
        self
    }

    /// Starts the appearance settings at `appearance`.
    pub fn with_appearance(self, appearance: AppearanceSettings) -> Self {
        self.appearance.send_replace(appearance);
        self
    }

    /// Refuses every save from now on (a full disk), answering with
    /// [`NOT_SAVED`] and changing nothing.
    pub fn refuse_saves(&self, refuse: bool) {
        self.refuse
            .store(refuse, std::sync::atomic::Ordering::SeqCst);
    }

    fn save<T>(
        &self,
        sender: &watch::Sender<T>,
        value: T,
    ) -> oneshot::Receiver<Result<(), String>> {
        let (answer, receiver) = oneshot::channel();
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = answer.send(Err(NOT_SAVED.to_owned()));
        } else {
            sender.send_replace(value);
            let _ = answer.send(Ok(()));
        }
        receiver
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
        self.save(&self.notifications, settings)
    }

    fn privacy_settings(&self) -> watch::Receiver<PrivacySettings> {
        self.privacy.subscribe()
    }

    fn set_privacy_settings(
        &self,
        settings: PrivacySettings,
    ) -> oneshot::Receiver<Result<(), String>> {
        self.save(&self.privacy, settings)
    }

    fn appearance_settings(&self) -> watch::Receiver<AppearanceSettings> {
        self.appearance.subscribe()
    }

    fn set_appearance_settings(
        &self,
        settings: AppearanceSettings,
    ) -> oneshot::Receiver<Result<(), String>> {
        self.save(&self.appearance, settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_shows_by_default_and_memory_settings_follow_changes() {
        assert!(NotificationSettings::default().show_content);
        assert!(NotificationSettings::default().enabled);
        let settings = MemorySettings::default();
        let mut receiver = settings.notification_settings();
        let mut saved = settings.set_notification_settings(NotificationSettings {
            enabled: true,
            show_content: false,
        });
        assert_eq!(saved.try_recv(), Ok(Ok(())));
        assert!(receiver.has_changed().unwrap_or(false));
        assert!(!receiver.borrow_and_update().show_content);
    }

    #[test]
    fn receipts_and_typing_are_sent_by_default() {
        let privacy = PrivacySettings::default();
        assert!(privacy.read_receipts && privacy.typing_notices);
        let settings = MemorySettings::default();
        let mut receiver = settings.privacy_settings();
        let off = PrivacySettings {
            read_receipts: false,
            typing_notices: false,
        };
        let mut saved = settings.set_privacy_settings(off);
        assert_eq!(saved.try_recv(), Ok(Ok(())));
        assert_eq!(*receiver.borrow_and_update(), off);
    }

    #[test]
    fn a_refused_save_changes_nothing_and_says_so() {
        let settings = MemorySettings::default();
        settings.refuse_saves(true);
        let mut saved = settings.set_appearance_settings(AppearanceSettings {
            theme: ThemeChoice::Dark,
            ..AppearanceSettings::default()
        });
        assert_eq!(saved.try_recv(), Ok(Err(NOT_SAVED.to_owned())));
        assert_eq!(
            *settings.appearance_settings().borrow(),
            AppearanceSettings::default()
        );
    }

    #[test]
    fn stored_values_round_trip_and_unknown_ones_are_the_default() {
        for theme in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::parse(theme.as_str()), theme);
        }
        for motion in MotionChoice::ALL {
            assert_eq!(MotionChoice::parse(motion.as_str()), motion);
        }
        for contrast in ContrastChoice::ALL {
            assert_eq!(ContrastChoice::parse(contrast.as_str()), contrast);
        }
        assert_eq!(ThemeChoice::parse("sepia"), ThemeChoice::System);
        assert_eq!(MotionChoice::parse(""), MotionChoice::System);
        assert_eq!(ContrastChoice::parse("max"), ContrastChoice::System);
    }

    #[test]
    fn text_sizes_snap_to_an_offered_step() {
        assert_eq!(nearest_text_size(100), 100);
        assert_eq!(nearest_text_size(15), 100);
        assert_eq!(nearest_text_size(140), 150);
        assert_eq!(nearest_text_size(990), 200);
    }
}
