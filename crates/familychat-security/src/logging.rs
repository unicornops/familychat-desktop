//! Local logs and the crash log (PLAN.md, sections 5.4 and 8).
//!
//! `tracing` events go to `logs/familychat.log` in the data directory and
//! nowhere else: no stderr (it would garble the terminal UI), no remote
//! logging. Every line passes through [`redact::scrub`](crate::redact::scrub)
//! on its way to the file. By default Family Chat's own crates log at
//! `info` and everything else, matrix-sdk included, at `warn`;
//! `FAMILYCHAT_LOG` replaces that with `target=level` directives
//! (`familychat=debug,matrix_sdk=info`).
//!
//! A panic appends one line to `logs/crash.log`: the time, the thread, the
//! location and the message, scrubbed the same way. The log and the crash
//! log are each kept under a size limit by moving the old file to `.old` at
//! start. Nothing is uploaded.
//!
//! The crash log hook follows Zmail's (`zmail-app`'s `install_panic_log`),
//! with the redaction added; the logs live under the data directory as
//! PLAN.md lays out, rather than in a separate state directory.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Mutex,
    time::SystemTime,
};

use tracing_subscriber::{filter::Targets, fmt::MakeWriter, layer::SubscriberExt};

use crate::redact::scrub;

/// The environment variable with `target=level` log directives.
pub const LOG_ENV: &str = "FAMILYCHAT_LOG";

/// What is logged when [`LOG_ENV`] is not set: Family Chat's crates at
/// `info`, matrix-sdk and everything else at `warn`.
pub const DEFAULT_DIRECTIVES: &str = "warn,familychat=info,matrix_sdk=warn";

/// The log file's name in the log directory.
pub const LOG_FILE_NAME: &str = "familychat.log";

/// The crash log's name in the log directory.
pub const CRASH_LOG_FILE_NAME: &str = "crash.log";

/// The log moves to `familychat.log.old` at start once it is this big.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// The crash log moves to `crash.log.old` at start once it is this big.
const MAX_CRASH_LOG_BYTES: u64 = 1024 * 1024;

/// Why logging could not start. The app runs without a log file then.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The log directory or file could not be created.
    #[error("failed to open the log in {path}: {source}")]
    Io {
        /// What could not be opened.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// Another subscriber is already installed in this process.
    #[error("logging is already set up in this process")]
    AlreadySet,
}

/// The files logging writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFiles {
    /// `familychat.log`.
    pub log: PathBuf,
    /// `crash.log`.
    pub crash: PathBuf,
}

/// Starts logging to `log_dir` (the data directory's `logs/`) and installs
/// the crash log. Call once, after the profile lock is held, so two
/// processes never rotate the same files.
pub fn init(log_dir: &Path) -> Result<LogFiles, LogError> {
    create_private_dir(log_dir).map_err(|source| LogError::Io {
        path: log_dir.to_owned(),
        source,
    })?;
    let files = LogFiles {
        log: log_dir.join(LOG_FILE_NAME),
        crash: log_dir.join(CRASH_LOG_FILE_NAME),
    };
    rotate(&files.log, MAX_LOG_BYTES);
    let file = open_private_append(&files.log).map_err(|source| LogError::Io {
        path: files.log.clone(),
        source,
    })?;
    let requested = std::env::var(LOG_ENV).ok();
    let (filter, refused) = filter_from(requested.as_deref());
    tracing::subscriber::set_global_default(subscriber(Mutex::new(file), filter))
        .map_err(|_| LogError::AlreadySet)?;
    if refused {
        tracing::warn!("{LOG_ENV} is not a list of target=level directives; using the defaults");
    }
    install_crash_log(&files.crash);
    Ok(files)
}

/// The directives in `requested`, or [`DEFAULT_DIRECTIVES`] when there are
/// none or they do not parse (the `bool` is then `true`).
pub fn filter_from(requested: Option<&str>) -> (Targets, bool) {
    let default = || Targets::from_str(DEFAULT_DIRECTIVES).unwrap_or_default();
    match requested.map(str::trim).filter(|text| !text.is_empty()) {
        None => (default(), false),
        Some(text) => match Targets::from_str(text) {
            Ok(targets) => (targets, false),
            Err(_) => (default(), true),
        },
    }
}

/// The subscriber [`init`] installs, writing plain-text lines through
/// [`RedactingMakeWriter`] to `make_writer`. Tests build one over a buffer.
pub fn subscriber<M>(make_writer: M, filter: Targets) -> impl tracing::Subscriber + Send + Sync
where
    M: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        // `filter` decides; the builder's own cap would stop at `info`.
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .with_writer(RedactingMakeWriter(make_writer))
        .finish()
        .with(filter)
}

/// Wraps a [`MakeWriter`] so every line is [`scrub`]bed before it is
/// written.
#[derive(Debug)]
pub struct RedactingMakeWriter<M>(pub M);

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = LineRedactor<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        LineRedactor {
            inner: self.0.make_writer(),
            buffer: Vec::new(),
        }
    }
}

/// Collects what one event writes and passes it on, scrubbed, when flushed
/// or dropped. The formatter writes each event through a fresh writer, so
/// a secret split across `write` calls is still seen whole.
pub struct LineRedactor<W: Write> {
    inner: W,
    buffer: Vec<u8>,
}

impl<W: Write> LineRedactor<W> {
    fn emit(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buffer);
        let mut scrubbed = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let (body, end) = match line.strip_suffix('\n') {
                Some(body) => (body, "\n"),
                None => (line, ""),
            };
            scrubbed.push_str(&scrub(body));
            scrubbed.push_str(end);
        }
        self.buffer.clear();
        self.inner.write_all(scrubbed.as_bytes())?;
        self.inner.flush()
    }
}

impl<W: Write> Write for LineRedactor<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()
    }
}

impl<W: Write> Drop for LineRedactor<W> {
    fn drop(&mut self) {
        let _ = self.emit();
    }
}

/// Installs the panic hook that appends to `crash_log`, after Rust's
/// default hook has printed to stderr.
pub fn install_crash_log(crash_log: &Path) {
    rotate(crash_log, MAX_CRASH_LOG_BYTES);
    let crash_log = crash_log.to_owned();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let _ = append_crash(&crash_log, info);
    }));
}

/// The crash log line for a panic.
fn crash_line(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic with a non-string payload");
    let location = info.location().map_or_else(
        || "unknown location".to_owned(),
        |location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        },
    );
    let thread = std::thread::current()
        .name()
        .map_or_else(|| "unnamed".to_owned(), str::to_owned);
    let timestamp = crate::time::rfc3339_millis(SystemTime::now());
    // A message can span lines; the crash log keeps one line per panic.
    let message = scrub(&message.replace(['\r', '\n'], " "));
    format!("{timestamp} thread '{thread}' panicked at {location}: {message}\n")
}

fn append_crash(crash_log: &Path, info: &std::panic::PanicHookInfo<'_>) -> io::Result<()> {
    open_private_append(crash_log)?.write_all(crash_line(info).as_bytes())
}

/// Moves `path` to `<path>.old` once it is bigger than `limit`, replacing
/// an older `.old`.
fn rotate(path: &Path, limit: u64) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() > limit {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let _ = fs::rename(path, PathBuf::from(old));
    }
}

/// Creates `path` and its parents; on Unix the last directory is readable
/// by its owner only.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// Opens `path` for appending, creating it readable by its owner only on
/// Unix.
fn open_private_append(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::{debug, error, info, warn};

    use super::*;
    use crate::{Passphrase, Redacted, Secret};

    /// A [`MakeWriter`] over a shared buffer: what the log file would hold.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("buffer")).into_owned()
        }
    }

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("buffer").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Self;

        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "familychat-logging-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn captured_logs_contain_no_secret() {
        let captured = Captured::default();
        let (filter, _) = filter_from(Some("trace"));
        let passphrase = Passphrase::generate().expect("random");
        let token = Secret::new("syt_YWxpY2U_TOKENVALUE_1234".to_owned());
        let secrets = [
            passphrase.to_hex().to_string(),
            token.expose().clone(),
            "LOGINTOKEN-from-the-link".to_owned(),
            "refresh-ROTATED-5678".to_owned(),
            "pa55w0rd-of-the-user".to_owned(),
            "Hello Grandma, the door code is 4321".to_owned(),
            "bearer-OPAQUE-credential".to_owned(),
            "since=s72594_4483_1934".to_owned(),
        ];

        tracing::subscriber::with_default(subscriber(captured.clone(), filter), || {
            info!(?passphrase, ?token, "keychain entries loaded");
            info!(
                url = "https://hs.example.org/_matrix/client/v3/login?loginToken=LOGINTOKEN-from-the-link",
                "redeeming"
            );
            debug!("rotated refresh_token=refresh-ROTATED-5678 for the account");
            warn!(r#"login failed for {{"user": "@a:b", "password": "pa55w0rd-of-the-user"}}"#);
            info!(body = %Redacted("Hello Grandma, the door code is 4321"), "message sent");
            error!("request had Authorization: Bearer bearer-OPAQUE-credential");
            info!(
                "GET https://hs.example.org/_matrix/client/v3/sync?since=s72594_4483_1934 took 3s"
            );
            info!(link = "familychat://login?hs=safechat.family&token=LOGINTOKEN-from-the-link");
            // A secret written by Display, as a careless call site would.
            info!("token is {}", token.expose());
        });

        let text = captured.text();
        assert_eq!(text.lines().count(), 9, "every event was logged:\n{text}");
        for secret in &secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{secret:?} leaked:\n{text}"
            );
        }
        // What is safe to know is still there.
        assert!(
            text.contains("https://hs.example.org/_matrix/client/v3/sync?[redacted]"),
            "{text}"
        );
        assert!(text.contains("keychain entries loaded"), "{text}");
        assert!(
            text.contains("passphrase=[redacted] token=[redacted]\n"),
            "{text}"
        );
        assert!(text.contains("familychat://[redacted]"), "{text}");
    }

    #[test]
    fn the_default_filter_keeps_matrix_sdk_at_warn() {
        let captured = Captured::default();
        let (filter, refused) = filter_from(None);
        assert!(!refused);
        tracing::subscriber::with_default(subscriber(captured.clone(), filter), || {
            info!(target: "matrix_sdk::sliding_sync", "sdk chatter");
            warn!(target: "matrix_sdk::encryption", "sdk warning");
            info!(target: "familychat_store", "store opened");
            debug!(target: "familychat_store", "store detail");
            info!(target: "hyper", "transport chatter");
        });
        let text = captured.text();
        assert!(!text.contains("sdk chatter"), "{text}");
        assert!(text.contains("sdk warning"), "{text}");
        assert!(text.contains("store opened"), "{text}");
        assert!(!text.contains("store detail"), "{text}");
        assert!(!text.contains("transport chatter"), "{text}");
    }

    #[test]
    fn bad_directives_fall_back_to_the_defaults() {
        let (_, refused) = filter_from(Some("familychat=loud"));
        assert!(refused);
        let (_, refused) = filter_from(Some("familychat=debug,matrix_sdk=info"));
        assert!(!refused);
        let (_, refused) = filter_from(Some("  "));
        assert!(!refused);
    }

    #[test]
    fn logs_are_rotated_at_their_limit() {
        let directory = temp_dir("rotate");
        create_private_dir(&directory).expect("dir");
        let log = directory.join(LOG_FILE_NAME);
        fs::write(&log, vec![b'x'; 16]).expect("written");
        rotate(&log, 32);
        assert!(log.exists(), "under the limit, kept");
        rotate(&log, 8);
        assert!(!log.exists());
        assert!(directory.join("familychat.log.old").exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn log_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = temp_dir("private");
        create_private_dir(&directory).expect("dir");
        let file = directory.join(CRASH_LOG_FILE_NAME);
        open_private_append(&file).expect("opened");
        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&file), 0o600);
        let _ = fs::remove_dir_all(directory);
    }

    /// Set in the child process `a_panic_is_written_to_the_crash_log_redacted`
    /// starts: the crash log the child writes.
    const CHILD_CRASH_LOG: &str = "FAMILYCHAT_TEST_CRASH_LOG";

    #[test]
    fn a_panic_is_written_to_the_crash_log_redacted() {
        let directory = temp_dir("crash");
        create_private_dir(&directory).expect("dir");
        let crash_log = directory.join(CRASH_LOG_FILE_NAME);
        // The hook is process-wide, so the panic happens in a child.
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "logging::tests::crash_log_child", "--nocapture"])
            .env(CHILD_CRASH_LOG, &crash_log)
            .output()
            .expect("the child runs");
        assert!(!output.status.success(), "the child panicked");
        let text = fs::read_to_string(&crash_log).expect("the crash log was written");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(
            text.contains("thread 'logging::tests::crash_log_child' panicked at"),
            "{text}"
        );
        assert!(text.contains("access_token=[redacted]"), "{text}");
        assert!(!text.contains("CRASH-SECRET"), "{text}");
        let _ = fs::remove_dir_all(directory);
    }

    /// The child half of `a_panic_is_written_to_the_crash_log_redacted`;
    /// does nothing in a normal test run.
    #[test]
    fn crash_log_child() {
        let Some(path) = std::env::var_os(CHILD_CRASH_LOG) else {
            return;
        };
        install_crash_log(Path::new(&path));
        panic!("request failed with access_token=CRASH-SECRET\nsecond line");
    }
}
