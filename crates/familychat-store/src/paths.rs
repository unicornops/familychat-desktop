//! Where a profile's files live (PLAN.md, section 5.4):
//!
//! ```text
//! familychat.lock                  profile lock (exclusive)
//! familychat.sqlite3               app store: accounts registry, preferences, UI state
//! accounts/<account-uuid>/sdk/     matrix-sdk state, crypto and event cache stores (Phase 3)
//! logs/                            redacted logs, crash.log
//! unreadable-<time>/               data moved aside by "start over"
//! ```
//!
//! The directory is `~/Library/Application Support/Family Chat` on macOS,
//! `$XDG_DATA_HOME/familychat` (or `~/.local/share/familychat`) on Linux
//! and other Unix systems, and `%APPDATA%\Family Chat` on Windows.
//! `FAMILYCHAT_DATA_DIR` overrides it, as `ZMAIL_DATA_DIR` does for Zmail,
//! whose resolution rules these copy.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use familychat_core::identity::DATA_DIR_ENV;
use familychat_security::AccountId;

/// The lock file's name.
pub const LOCK_FILE_NAME: &str = "familychat.lock";

/// The app store's file name.
pub const STORE_FILE_NAME: &str = "familychat.sqlite3";

/// The directory holding each account's directory.
pub const ACCOUNTS_DIR_NAME: &str = "accounts";

/// The log directory's name.
pub const LOGS_DIR_NAME: &str = "logs";

/// The prefix of directories that "start over" moves unreadable data into.
pub const UNREADABLE_PREFIX: &str = "unreadable-";

/// The data directory could not be worked out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("can't find a data directory: {0}; set {DATA_DIR_ENV} to choose one")]
pub struct DataDirError(String);

/// A profile's data directory and the paths inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// The directory `FAMILYCHAT_DATA_DIR` names, or the platform's.
    pub fn resolve() -> Result<Self, DataDirError> {
        resolve_from(
            std::env::var_os(DATA_DIR_ENV),
            std::env::var_os("HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("APPDATA"),
        )
        .map(Self::at)
    }

    /// The data directory at `root`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `familychat.lock`.
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE_NAME)
    }

    /// `familychat.sqlite3`.
    pub fn store_path(&self) -> PathBuf {
        self.root.join(STORE_FILE_NAME)
    }

    /// `accounts/`.
    pub fn accounts_dir(&self) -> PathBuf {
        self.root.join(ACCOUNTS_DIR_NAME)
    }

    /// `accounts/<uuid>/`.
    pub fn account_dir(&self, account: AccountId) -> PathBuf {
        self.accounts_dir().join(account.to_string())
    }

    /// `accounts/<uuid>/sdk/`, for matrix-sdk's stores.
    pub fn account_sdk_dir(&self, account: AccountId) -> PathBuf {
        self.account_dir(account).join("sdk")
    }

    /// `logs/`.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_DIR_NAME)
    }

    /// Creates the directory (owner-only on Unix) if it is not there.
    pub fn create(&self) -> io::Result<()> {
        familychat_security::logging::create_private_dir(&self.root)
    }
}

/// The data directory from the environment's values, as [`DataDir::resolve`]
/// reads them; separate so the rules are tested without changing the
/// process environment. The unused parameters keep one signature on every
/// platform.
pub fn resolve_from(
    data_dir: Option<OsString>,
    home: Option<OsString>,
    xdg_data_home: Option<OsString>,
    appdata: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    if let Some(dir) = data_dir.filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    platform_dir(home, xdg_data_home, appdata)
}

#[cfg(target_os = "macos")]
fn platform_dir(
    home: Option<OsString>,
    _xdg_data_home: Option<OsString>,
    _appdata: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    let home = home
        .filter(|home| !home.is_empty())
        .ok_or_else(|| DataDirError("HOME is not set".to_owned()))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join(familychat_core::identity::DATA_DIR_NAME))
}

#[cfg(target_os = "windows")]
fn platform_dir(
    _home: Option<OsString>,
    _xdg_data_home: Option<OsString>,
    appdata: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    let appdata = appdata
        .filter(|appdata| !appdata.is_empty())
        .ok_or_else(|| DataDirError("APPDATA is not set".to_owned()))?;
    Ok(PathBuf::from(appdata).join(familychat_core::identity::DATA_DIR_NAME))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_dir(
    home: Option<OsString>,
    xdg_data_home: Option<OsString>,
    _appdata: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    // The XDG spec: a relative $XDG_DATA_HOME is invalid and ignored.
    if let Some(xdg) = xdg_data_home.filter(|xdg| Path::new(xdg).is_absolute()) {
        return Ok(PathBuf::from(xdg).join(familychat_core::identity::XDG_DATA_DIR_NAME));
    }
    let home = home
        .filter(|home| !home.is_empty())
        .ok_or_else(|| DataDirError("neither XDG_DATA_HOME nor HOME is set".to_owned()))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join(familychat_core::identity::XDG_DATA_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(text: &str) -> Option<OsString> {
        Some(OsString::from(text))
    }

    #[test]
    fn the_override_wins_when_set() {
        assert_eq!(
            resolve_from(os("/tmp/fc-profile"), os("/home/a"), None, None).expect("dir"),
            PathBuf::from("/tmp/fc-profile")
        );
        // An empty override is no override.
        assert_ne!(
            resolve_from(
                os(""),
                os("/home/a"),
                None,
                os("C:\\Users\\a\\AppData\\Roaming")
            )
            .expect("dir"),
            PathBuf::from("")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_application_support() {
        assert_eq!(
            resolve_from(None, os("/Users/a"), os("/ignored"), None).expect("dir"),
            PathBuf::from("/Users/a/Library/Application Support/Family Chat")
        );
        assert!(resolve_from(None, None, None, None).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uses_appdata() {
        assert_eq!(
            resolve_from(None, None, None, os(r"C:\Users\a\AppData\Roaming")).expect("dir"),
            PathBuf::from(r"C:\Users\a\AppData\Roaming\Family Chat")
        );
        assert!(resolve_from(None, os(r"C:\Users\a"), None, None).is_err());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_uses_xdg_data_home_then_home() {
        assert_eq!(
            resolve_from(None, os("/home/a"), os("/data"), None).expect("dir"),
            PathBuf::from("/data/familychat")
        );
        assert_eq!(
            resolve_from(None, os("/home/a"), os("relative"), None).expect("dir"),
            PathBuf::from("/home/a/.local/share/familychat")
        );
        assert_eq!(
            resolve_from(None, os("/home/a"), None, None).expect("dir"),
            PathBuf::from("/home/a/.local/share/familychat")
        );
        let error = resolve_from(None, None, None, None).expect_err("nothing set");
        assert!(error.to_string().contains("FAMILYCHAT_DATA_DIR"), "{error}");
    }

    #[test]
    fn the_layout_matches_the_plan() {
        let dir = DataDir::at("/profile");
        let id: AccountId = "0b5c2a1e-7f3d-4c2b-9a8e-1d2c3b4a5f60".parse().expect("id");
        assert_eq!(dir.lock_path(), Path::new("/profile/familychat.lock"));
        assert_eq!(dir.store_path(), Path::new("/profile/familychat.sqlite3"));
        assert_eq!(dir.logs_dir(), Path::new("/profile/logs"));
        assert_eq!(
            dir.account_sdk_dir(id),
            Path::new("/profile/accounts/0b5c2a1e-7f3d-4c2b-9a8e-1d2c3b4a5f60/sdk")
        );
    }
}
