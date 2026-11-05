//! The single-instance hand-off through the real binary (ADR 0013): while
//! this test holds a profile's lock and listens on its endpoint, as the
//! window does, a second `familychat` forwards its sign-in link (from
//! standard input with `open-link`, or as its argument, as an OS URL
//! handler passes it) or asks the window to come forward, and exits. It
//! never prints the link, never writes the profile's logs, and refuses
//! text that isn't a sign-in link before sending anything.
//!
//! Runs on every OS in CI's build job (Unix sockets on Linux and macOS,
//! the named pipe on Windows).

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use familychat_core::requests::AppRequest;
use familychat_instance::{Deliver, Endpoint, listen};
use familychat_store::{DataDir, ProfileLock};

const TOKEN: &str = "secret-token-7d1e";

fn link() -> String {
    format!(
        "familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:@ana:smith.safechat.family&hs=hs1.smith.safechat.family&token={TOKEN}"
    )
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_profile(name: &str) -> TempDir {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    TempDir(std::env::temp_dir().join(format!(
        "fc-app-{name}-{}-{}",
        std::process::id(),
        nanos % 1_000_000_000
    )))
}

/// What the "window" was handed.
#[derive(Debug, PartialEq, Eq)]
enum Got {
    Link(String),
    Activate,
}

/// Runs the binary on the profile, with `stdin`.
fn familychat(dir: &DataDir, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_familychat"))
        .args(args)
        .env("FAMILYCHAT_DATA_DIR", dir.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    {
        let mut input = child.stdin.take().expect("its stdin");
        if let Some(stdin) = stdin {
            input.write_all(stdin.as_bytes()).expect("written");
        }
    }
    child.wait_with_output().expect("the binary finishes")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_second_launch_forwards_its_link_to_the_window_and_exits() {
    let temp = temp_profile("handoff");
    let dir = DataDir::at(&temp.0);
    let held = ProfileLock::acquire(&dir).expect("the profile is free");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
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
            true
        })
    };
    let endpoint = Endpoint::for_profile(dir.root()).expect("an endpoint");
    let listener = listen(&endpoint, runtime.handle(), deliver).expect("listening");
    let taken = || std::mem::take(&mut *got.lock().unwrap_or_else(PoisonError::into_inner));

    // `open-link`: the link on standard input, never on the command line.
    let output = familychat(&dir, &["open-link"], Some(&format!("  {}\n", link())));
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("Family Chat is already open; the sign-in continues"),
        "{}",
        text(&output)
    );
    assert_eq!(taken(), [Got::Link(link())]);

    // As the OS's URL handler starts it: the link as the argument.
    let output = familychat(&dir, &[&link()], None);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(taken(), [Got::Link(link())]);
    let https = link().replace("familychat://", "https://");
    let output = familychat(&dir, &[&https], None);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(taken(), [Got::Link(https)]);

    // A bare launch asks the window to come forward.
    let output = familychat(&dir, &[], None);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(taken(), [Got::Activate]);

    // Text that isn't a sign-in link is refused before anything is sent,
    // and never echoed.
    for (args, stdin) in [
        (
            vec!["open-link"],
            Some(format!("https://evil.example/login?token={TOKEN}\n")),
        ),
        (vec!["open-link"], Some(format!("not a link {TOKEN}\n"))),
        (
            vec!["open-link"],
            Some(format!("{}{}\n", link(), "a".repeat(5000))),
        ),
        (vec!["open-link"], Some(String::new())),
    ] {
        let output = familychat(&dir, &args, stdin.as_deref());
        assert!(!output.status.success(), "{args:?} must fail");
        assert!(
            text(&output).contains("That isn't a Family Chat sign-in link."),
            "{}",
            text(&output)
        );
        assert!(!text(&output).contains(TOKEN), "{}", text(&output));
    }
    let not_a_signin_link = format!("familychat://safechat.family/app/elsewhere?token={TOKEN}");
    let output = familychat(&dir, &[&not_a_signin_link], None);
    assert!(!output.status.success());
    assert!(!text(&output).contains(TOKEN), "{}", text(&output));
    let output = familychat(&dir, &["open-link", &link()], None);
    assert!(!output.status.success());
    assert!(!text(&output).contains(TOKEN), "{}", text(&output));
    assert!(taken().is_empty(), "nothing else reached the window");

    // The terminal UI still refuses a profile the window has.
    if cfg!(feature = "tui") {
        let output = familychat(&dir, &["tui"], None);
        assert!(!output.status.success());
        assert!(
            text(&output).contains("Family Chat is already open with this profile"),
            "{}",
            text(&output)
        );
    }

    // The second launches wrote nothing into the profile: no logs, no
    // store, and the token is in no file.
    assert!(!dir.logs_dir().exists(), "a second launch never logs");
    for entry in std::fs::read_dir(dir.root()).expect("readable").flatten() {
        if entry.path().is_file()
            && let Ok(contents) = std::fs::read(entry.path())
        {
            assert!(
                !String::from_utf8_lossy(&contents).contains(TOKEN),
                "{} holds the token",
                entry.path().display()
            );
        }
    }

    {
        let _entered = runtime.enter();
        drop(listener);
    }
    drop(held);
}

#[test]
fn with_nobody_listening_a_second_launch_says_the_profile_is_open() {
    let temp = temp_profile("handoff-none");
    let dir = DataDir::at(&temp.0);
    let held = ProfileLock::acquire(&dir).expect("the profile is free");
    let output = familychat(&dir, &["open-link"], Some(&format!("{}\n", link())));
    assert!(!output.status.success());
    assert!(
        text(&output).contains("Family Chat is already open with this profile"),
        "{}",
        text(&output)
    );
    assert!(!text(&output).contains(TOKEN), "{}", text(&output));
    drop(held);
}
