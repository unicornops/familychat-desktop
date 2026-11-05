//! The profile lock (PLAN.md, section 5.5): `familychat.lock`, held
//! exclusively by the one process that has the profile open.
//!
//! matrix-sdk's crypto store must have one writer, so unlike Zmail (whose
//! GUI and terminal UI share a store and coordinate with a sync lock) a
//! second Family Chat on the same profile is refused at once, whichever
//! frontend it is. The OS releases the lock when the process ends, so a
//! crash never leaves the profile locked. The locking itself follows
//! Zmail's `zmail-store` `lock.rs`: `std::fs::File::try_lock`, which is
//! `flock` on Unix and `LockFileEx` on Windows.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use familychat_core::identity::APP_NAME;

use crate::paths::DataDir;

/// The profile is held, by this process or another.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another Family Chat (window or terminal) has this profile open.
    #[error("{APP_NAME} is already open with this profile ({}); quit it first", .profile.display())]
    AlreadyOpen {
        /// The profile's data directory.
        profile: PathBuf,
    },
    /// The lock file could not be created or locked.
    #[error("failed to lock {}: {source}", .path.display())]
    Io {
        /// The lock file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
}

/// The held profile lock; released when dropped (or when the process ends).
#[derive(Debug)]
pub struct ProfileLock {
    _file: fs::File,
    dir: DataDir,
}

impl ProfileLock {
    /// Takes the lock of the profile in `dir`, creating the directory if
    /// needed. Never waits: a profile another process holds is
    /// [`LockError::AlreadyOpen`] at once.
    pub fn acquire(dir: &DataDir) -> Result<Self, LockError> {
        let path = dir.lock_path();
        let io_error = |source| LockError::Io {
            path: path.clone(),
            source,
        };
        dir.create().map_err(io_error)?;
        let file = open(&path).map_err(io_error)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                dir: dir.clone(),
            }),
            Err(fs::TryLockError::WouldBlock) => Err(LockError::AlreadyOpen {
                profile: dir.root().to_owned(),
            }),
            Err(fs::TryLockError::Error(source)) => Err(LockError::Io { path, source }),
        }
    }

    /// The profile this lock holds.
    pub fn dir(&self) -> &DataDir {
        &self.dir
    }
}

fn open(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir;

    #[test]
    fn a_second_handle_is_refused_until_the_first_is_dropped() {
        let dir = test_dir::TestDir::new("lock-handle");
        let first = ProfileLock::acquire(dir.data_dir()).expect("the profile is free");
        assert_eq!(first.dir(), dir.data_dir());
        match ProfileLock::acquire(dir.data_dir()) {
            Err(LockError::AlreadyOpen { profile }) => {
                assert_eq!(profile, dir.data_dir().root());
            }
            other => panic!("expected already open, got {other:?}"),
        }
        drop(first);
        test_dir::eventually(
            || ProfileLock::acquire(dir.data_dir()),
            |error| matches!(error, LockError::AlreadyOpen { .. }),
        )
        .expect("free again once dropped");
    }

    #[test]
    fn the_message_says_the_app_is_already_open() {
        let error = LockError::AlreadyOpen {
            profile: PathBuf::from("/profile"),
        };
        assert!(
            error
                .to_string()
                .starts_with("Family Chat is already open with this profile"),
            "{error}"
        );
    }

    /// Set in the child process `a_second_process_is_refused_the_profile`
    /// starts: the profile the child tries to lock.
    const CHILD_PROFILE: &str = "FAMILYCHAT_TEST_LOCK_CHILD_PROFILE";

    #[test]
    fn a_second_process_is_refused_the_profile() {
        let dir = test_dir::TestDir::new("lock-process");
        let held = ProfileLock::acquire(dir.data_dir()).expect("the profile is free");
        let child = || {
            std::process::Command::new(std::env::current_exe().expect("the test binary"))
                .args(["--exact", "lock::tests::lock_child", "--nocapture"])
                .env(CHILD_PROFILE, dir.data_dir().root())
                .output()
                .expect("the child runs")
        };
        let output = child();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "child failed: {stdout}");
        assert!(
            stdout.contains("profile lock: already open"),
            "child: {stdout}"
        );
        drop(held);
        let stdout = test_dir::eventually(
            || {
                let stdout = String::from_utf8_lossy(&child().stdout).into_owned();
                if stdout.contains("profile lock: acquired") {
                    Ok(stdout)
                } else {
                    Err(stdout)
                }
            },
            |stdout| stdout.contains("profile lock: already open"),
        );
        assert!(stdout.is_ok(), "child: {stdout:?}");
    }

    /// The child half of `a_second_process_is_refused_the_profile`; does
    /// nothing in a normal test run.
    #[test]
    fn lock_child() {
        let Some(profile) = std::env::var_os(CHILD_PROFILE) else {
            return;
        };
        match ProfileLock::acquire(&DataDir::at(profile)) {
            Ok(_) => println!("profile lock: acquired"),
            Err(LockError::AlreadyOpen { .. }) => println!("profile lock: already open"),
            Err(error) => println!("profile lock: {error}"),
        }
    }
}
