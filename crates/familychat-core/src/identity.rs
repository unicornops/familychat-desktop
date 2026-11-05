//! The app's names and identifiers, in one place so they are easy to
//! change (PLAN.md, section 15, Open decision 2).
//!
//! The bundle id matches the one the Family Chat Electron desktop build
//! already uses, since this app replaces it. Registering the URL scheme
//! with the OS is Phase 12; the data directory is used from Phase 1.

/// The app's name as people see it: window titles, menus, notifications.
pub const APP_NAME: &str = "Family Chat";

/// The application id: the macOS bundle id, the Windows app user model id,
/// the Linux desktop file and Wayland app id, and the keychain service.
pub const APP_ID: &str = "family.safechat.desktop";

/// The executable's name, and the Debian package's.
pub const BINARY_NAME: &str = "familychat";

/// The URL scheme sign-in links arrive on (`familychat://...`).
pub const URL_SCHEME: &str = "familychat";

/// The data directory's name under the platform's application data
/// directory on macOS and Windows (`~/Library/Application Support/Family
/// Chat`, `%APPDATA%\Family Chat`).
pub const DATA_DIR_NAME: &str = "Family Chat";

/// The data directory's name under `$XDG_DATA_HOME` on Linux and other
/// Unix systems.
pub const XDG_DATA_DIR_NAME: &str = "familychat";

/// The environment variable that overrides the data directory, as
/// `ZMAIL_DATA_DIR` does for Zmail.
pub const DATA_DIR_ENV: &str = "FAMILYCHAT_DATA_DIR";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_well_formed() {
        // A reverse-DNS id with no spaces or capitals: what Info.plist,
        // Windows AUMIDs, desktop files and Wayland app ids all accept.
        assert!(APP_ID.split('.').count() >= 3);
        assert!(
            APP_ID
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
        );
        // RFC 3986: a scheme is a letter followed by letters, digits, `+`,
        // `-` or `.`; lower case is the canonical form.
        assert!(URL_SCHEME.starts_with(|c: char| c.is_ascii_lowercase()));
        assert!(
            URL_SCHEME
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c))
        );
        assert!(!BINARY_NAME.contains(char::is_whitespace));
        assert!(!XDG_DATA_DIR_NAME.contains(char::is_whitespace));
    }
}
