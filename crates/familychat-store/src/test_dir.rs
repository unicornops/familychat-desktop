//! Throwaway data directories for tests, removed when dropped.

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::paths::{DataDir, LOCK_FILE_NAME};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// A data directory under the OS temporary directory, unique to the test.
pub(crate) struct TestDir(DataDir);

impl TestDir {
    pub(crate) fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(DataDir::at(std::env::temp_dir().join(format!(
            "familychat-store-{name}-{}-{nanos}-{sequence}",
            std::process::id()
        ))))
    }

    pub(crate) fn data_dir(&self) -> &DataDir {
        &self.0
    }

    /// Every file under the directory but the lock, with its bytes.
    pub(crate) fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut pending = vec![self.0.root().to_owned()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).expect("readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if entry.file_name() != LOCK_FILE_NAME {
                    files.insert(path.clone(), fs::read(&path).expect("readable"));
                }
            }
        }
        files
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.root());
    }
}

/// Runs `attempt` until it is not refused because the profile is held, for
/// up to five seconds.
///
/// On Unix a lock this process has just dropped can still look held for a
/// moment: when another test spawns a child process, the child has a copy
/// of every open descriptor between `fork` and `exec`, and an `flock` lasts
/// until its last copy closes. Only tests that drop a lock and take it
/// again while other tests spawn processes see that; the app never does.
pub(crate) fn eventually<T, E>(
    mut attempt: impl FnMut() -> Result<T, E>,
    held: impl Fn(&E) -> bool,
) -> Result<T, E> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match attempt() {
            Err(error) if held(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}
