//! The hand-off in one process: a listener on a test profile, and second
//! launches that forward, send garbage, or say nothing. They run on every
//! OS (CI's build job), over the real socket or pipe.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use familychat_core::{requests::AppRequest, signin::SecretText};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

pub(crate) const LINK: &str = "familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:@ana:smith.safechat.family&hs=hs1.smith.safechat.family&token=secret-token-1234";

/// A profile directory of its own, removed afterwards.
pub(crate) struct TestProfile(PathBuf);

impl TestProfile {
    pub(crate) fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fc-{name}-{}-{}",
            std::process::id(),
            nanos % 1_000_000_000
        ));
        std::fs::create_dir_all(&dir).expect("the profile directory");
        Self(dir)
    }

    pub(crate) fn endpoint(&self) -> Endpoint {
        Endpoint::for_profile(&self.0).expect("an endpoint")
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the window was handed.
#[derive(Debug, PartialEq, Eq)]
enum Got {
    Link(String),
    Activate,
}

struct Window {
    got: Arc<Mutex<Vec<Got>>>,
    runtime: tokio::runtime::Runtime,
    listener: Option<Listener>,
}

impl Window {
    /// A window listening on `profile`'s endpoint, taking requests while
    /// `accept` says so.
    fn listen(profile: &TestProfile, accept: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let got = Arc::new(Mutex::new(Vec::new()));
        let deliver: Deliver = {
            let got = got.clone();
            Arc::new(move |request| {
                let entry = match request {
                    AppRequest::OpenLink(link) => Got::Link(link.expose().to_owned()),
                    AppRequest::Activate => Got::Activate,
                };
                got.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(entry);
                accept
            })
        };
        let listener = listen(&profile.endpoint(), runtime.handle(), deliver).expect("listening");
        Self {
            got,
            runtime,
            listener: Some(listener),
        }
    }

    fn got(&self) -> Vec<Got> {
        std::mem::take(&mut *self.got.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Sends `bytes` as a second launch would, raw, and returns the reply
    /// frame's JSON (`None` when the window closed without one).
    fn raw(&self, profile: &TestProfile, bytes: Vec<u8>) -> Option<String> {
        let endpoint = profile.endpoint();
        self.runtime.block_on(async move {
            let mut stream = match platform::connect(&endpoint.inner).await {
                Ok(stream) => stream,
                Err(_) => panic!("the window listens"),
            };
            let _ = stream.write_all(&bytes).await;
            let _ = stream.flush().await;
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).await.ok()?;
            let mut body = vec![0_u8; usize::try_from(u32::from_be_bytes(length)).ok()?];
            stream.read_exact(&mut body).await.ok()?;
            String::from_utf8(body).ok()
        })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // The listener's cleanup runs inside the runtime it was made on.
        let _entered = self.runtime.enter();
        self.listener.take();
    }
}

fn framed(json: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(json.len())
        .expect("short")
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(json);
    bytes
}

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn a_second_launch_forwards_its_link_and_a_bare_one_asks_to_come_forward() {
    let profile = TestProfile::new("forward");
    let window = Window::listen(&profile, true);
    forward(
        &profile.endpoint(),
        AppRequest::OpenLink(SecretText::from(LINK)),
        WAIT,
    )
    .expect("forwarded");
    forward(&profile.endpoint(), AppRequest::Activate, WAIT).expect("forwarded");
    assert_eq!(window.got(), [Got::Link(LINK.to_owned()), Got::Activate]);
}

#[test]
fn a_window_that_declines_is_refused() {
    let profile = TestProfile::new("declined");
    let window = Window::listen(&profile, false);
    let result = forward(
        &profile.endpoint(),
        AppRequest::OpenLink(SecretText::from(LINK)),
        WAIT,
    );
    assert!(matches!(result, Err(ForwardError::Refused)), "{result:?}");
    assert_eq!(window.got().len(), 1);
}

#[test]
fn nobody_listening_is_not_listening_after_the_wait() {
    let profile = TestProfile::new("nobody");
    let started = Instant::now();
    let result = forward(
        &profile.endpoint(),
        AppRequest::Activate,
        Duration::from_millis(300),
    );
    assert!(
        matches!(result, Err(ForwardError::NotListening)),
        "{result:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn malformed_and_oversized_messages_are_refused_and_nothing_is_delivered() {
    let profile = TestProfile::new("malformed");
    let window = Window::listen(&profile, true);
    let refused = Some(r#"{"v":1,"accepted":false}"#.to_owned());
    let cases: Vec<Vec<u8>> = vec![
        framed(b"not json"),
        framed(br#"{"v":1,"link":"familychat://safechat.family/app/login","more":1}"#),
        framed(br#"{"v":9}"#),
        framed(br#"{"v":1,"link":"https://evil.example/app/login?token=x"}"#),
        framed(format!(r#"{{"v":1,"link":"{LINK}{}"}}"#, "a".repeat(5000)).as_bytes()),
    ];
    for bytes in cases {
        assert_eq!(window.raw(&profile, bytes), refused);
    }
    // Over the frame limit: refused from the length alone, before any of
    // it is read.
    let mut huge = u32::try_from(MAX_FRAME_BYTES + 1)
        .expect("small")
        .to_be_bytes()
        .to_vec();
    huge.extend_from_slice(&[b'x'; 64]);
    assert_eq!(window.raw(&profile, huge), refused);
    assert!(window.got().is_empty(), "nothing reached the window");

    // The window still serves a well-formed request afterwards.
    forward(&profile.endpoint(), AppRequest::Activate, WAIT).expect("forwarded");
    assert_eq!(window.got(), [Got::Activate]);
}

#[test]
fn a_silent_client_is_dropped_and_does_not_block_others() {
    let profile = TestProfile::new("silent");
    let window = Window::listen(&profile, true);
    let endpoint = profile.endpoint();
    // Connects and says nothing.
    let silent = window.runtime.block_on(async {
        match platform::connect(&endpoint.inner).await {
            Ok(stream) => stream,
            Err(_) => panic!("the window listens"),
        }
    });
    let started = Instant::now();
    forward(&profile.endpoint(), AppRequest::Activate, WAIT).expect("served meanwhile");
    assert!(
        started.elapsed() < READ_TIMEOUT,
        "not held up by the silent one"
    );
    assert_eq!(window.got(), [Got::Activate]);
    // Once the read timeout passes, the window has closed it.
    let mut silent = silent;
    let closed = window.runtime.block_on(async {
        let mut byte = [0_u8; 1];
        tokio::time::timeout(READ_TIMEOUT * 3, silent.read(&mut byte)).await
    });
    assert!(
        matches!(closed, Ok(Ok(0)) | Ok(Err(_))),
        "closed after the timeout: {closed:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_socket_and_its_directory_are_private_to_the_user() {
    use std::os::unix::fs::MetadataExt;

    let profile = TestProfile::new("private");
    let endpoint = profile.endpoint();
    let window = Window::listen(&profile, true);
    let socket = std::fs::symlink_metadata(endpoint.path()).expect("the socket exists");
    let dir = std::fs::symlink_metadata(endpoint.inner.dir()).expect("its directory exists");
    assert!(std::os::unix::fs::FileTypeExt::is_socket(
        &socket.file_type()
    ));
    assert_eq!(dir.mode() & 0o777, 0o700, "the directory is 0700");
    assert_eq!(socket.mode() & 0o077, 0, "the socket is the owner's only");
    assert_eq!(socket.uid(), dir.uid());
    // The test's own files are ours: the same owner as everything here.
    assert_eq!(
        dir.uid(),
        std::fs::metadata(&profile.0).expect("the profile").uid()
    );
    // The socket goes when the window does.
    drop(window);
    assert!(!endpoint.path().exists());
}

/// A window listening on `endpoint` (a directory of the test's own).
#[cfg(unix)]
fn listen_in(endpoint: Endpoint, runtime: &tokio::runtime::Runtime) -> Listener {
    let deliver: Deliver = Arc::new(|_| true);
    listen(&endpoint, runtime.handle(), deliver).expect("listening")
}

#[cfg(unix)]
#[test]
fn a_loose_directory_is_narrowed_and_a_second_launch_refuses_one() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let profile = TestProfile::new("loose");
    let dir = profile.0.join("run");
    std::fs::create_dir_all(&dir).expect("made");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("loosened");
    let endpoint = Endpoint {
        inner: unix::Endpoint::in_dir(&dir),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime");
    let listener = listen_in(endpoint.clone(), &runtime);
    let mode = std::fs::symlink_metadata(&dir).expect("exists").mode();
    assert_eq!(mode & 0o777, 0o700, "narrowed before listening");
    forward(&endpoint, AppRequest::Activate, WAIT).expect("forwarded");

    // A directory others can enter is refused by a second launch, which
    // sends nothing.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("loosened");
    let result = forward(&endpoint, AppRequest::Activate, Duration::from_millis(200));
    assert!(matches!(result, Err(ForwardError::Unsafe(_))), "{result:?}");
    // So is a socket others can use.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restored");
    std::fs::set_permissions(endpoint.path(), std::fs::Permissions::from_mode(0o666))
        .expect("loosened");
    let result = forward(&endpoint, AppRequest::Activate, Duration::from_millis(200));
    assert!(matches!(result, Err(ForwardError::Unsafe(_))), "{result:?}");
    let _entered = runtime.enter();
    drop(listener);
}

/// The name SDDL text gives `sid`: the alias of a well-known local account
/// (the built-in Administrator and Guest), otherwise the SID itself.
#[cfg(windows)]
fn sddl_name(sid: &str) -> String {
    let local = sid.starts_with("S-1-5-21-");
    match sid.rsplit('-').next() {
        Some("500") if local => "LA".to_owned(),
        Some("501") if local => "LG".to_owned(),
        _ => sid.to_owned(),
    }
}

#[cfg(windows)]
#[test]
fn the_pipe_grants_only_the_current_user() {
    let profile = TestProfile::new("private");
    let sid = windows_security::current_user_sid().expect("the user's SID");
    assert!(sid.starts_with("S-1-5-"), "{sid}");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let _entered = runtime.enter();
    let (incoming, cleanup) = platform::listen(&profile.endpoint().inner).expect("listening");
    let sddl = windows_security::security_of(incoming.server()).expect("its security");
    // How the SDDL names the user: by SID, or by an alias for a well-known
    // account (CI runs as the built-in Administrator, RID 500, shown "LA").
    let user = sddl_name(&sid);
    // Owner: the user. DACL: protected, one entry, allowing the user.
    assert!(sddl.starts_with(&format!("O:{user}D:P")), "{sddl}");
    let dacl = sddl
        .split_once("D:")
        .map(|(_, dacl)| dacl)
        .unwrap_or_default();
    assert_eq!(dacl.matches('(').count(), 1, "one entry: {sddl}");
    assert!(dacl.contains(&format!(";;;{user})")), "{sddl}");
    assert!(dacl.contains("(A;"), "an allow entry: {sddl}");
    for others in ["WD", "AU", "BU", "BA", "SY", "AN", "IU"] {
        assert!(
            !dacl.contains(&format!(";;;{others})")),
            "{others} in {sddl}"
        );
    }
    // The pipe's name is in the profile, and random.
    let name = std::fs::read_to_string(profile.endpoint().path()).expect("the name file");
    assert!(
        name.starts_with(r"\\.\pipe\family.safechat.desktop-"),
        "{name}"
    );
    drop(incoming);
    drop(cleanup);
    assert!(!profile.endpoint().path().exists());
}
