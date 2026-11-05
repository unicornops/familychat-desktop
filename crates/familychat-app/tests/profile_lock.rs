//! One process per profile (PLAN.md, section 5.5), through the real binary:
//! while this test holds a profile's lock, `familychat accounts`, the
//! terminal UI and the window each refuse it with "already open".
//!
//! The binary takes the lock before anything else, so these runs never
//! reach the OS keychain. The success path, which does, is covered by the
//! app's unit tests with the mock keychain.

use std::{
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use familychat_store::{DataDir, ProfileLock};

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_second_process_on_the_same_profile_is_told_it_is_already_open() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = TempDir(std::env::temp_dir().join(format!(
        "familychat-app-lock-{}-{nanos}",
        std::process::id()
    )));
    let dir = DataDir::at(&temp.0);
    let held = ProfileLock::acquire(&dir).expect("the profile is free");

    let mut commands = vec![vec!["accounts"], vec!["start-over"], vec![]];
    if cfg!(feature = "tui") {
        commands.push(vec!["tui"]);
    }
    for args in commands {
        let output = Command::new(env!("CARGO_BIN_EXE_familychat"))
            .args(&args)
            .env("FAMILYCHAT_DATA_DIR", dir.root())
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?} must fail: {stderr}");
        assert!(
            stderr.contains("Family Chat is already open with this profile"),
            "{args:?}: {stderr}"
        );
    }
    // Nothing but the lock file was created: no store, no logs.
    let entries: Vec<_> = std::fs::read_dir(dir.root())
        .expect("readable")
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(entries, ["familychat.lock"]);
    drop(held);
}
