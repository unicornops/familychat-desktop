//! A hand-off's log lines carry no link and no token (PLAN.md, sections
//! 5.4 and 5.5), checked with a subscriber that redacts nothing, at TRACE
//! for every target. Alone in its own test binary, so the subscriber is
//! the process's global one and sees every event, the listener's runtime
//! threads included.

use std::{
    io::Write,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use familychat_core::{requests::AppRequest, signin::SecretText};
use familychat_instance::{Deliver, Endpoint, ForwardError, forward, listen};

const TOKEN: &str = "secret-token-5f2c";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn no_link_or_token_reaches_a_log_line() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("the only subscriber");

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let profile = std::env::temp_dir().join(format!(
        "fc-inst-logs-{}-{}",
        std::process::id(),
        nanos % 1_000_000_000
    ));
    std::fs::create_dir_all(&profile).expect("the profile");
    let endpoint = Endpoint::for_profile(&profile).expect("an endpoint");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let accepted = Arc::new(Mutex::new(true));
    let deliver: Deliver = {
        let accepted = accepted.clone();
        Arc::new(move |request| {
            drop(request);
            *accepted.lock().unwrap_or_else(PoisonError::into_inner)
        })
    };
    let listener = listen(&endpoint, runtime.handle(), deliver).expect("listening");

    let link = format!(
        "familychat://safechat.family/app/login?account_provider=smith.safechat.family&hs=hs1.smith.safechat.family&token={TOKEN}"
    );
    forward(
        &endpoint,
        AppRequest::OpenLink(SecretText::new(link.clone())),
        Duration::from_secs(5),
    )
    .expect("forwarded");
    *accepted.lock().unwrap_or_else(PoisonError::into_inner) = false;
    let declined = forward(
        &endpoint,
        AppRequest::OpenLink(SecretText::new(link.clone())),
        Duration::from_secs(5),
    );
    assert!(
        matches!(declined, Err(ForwardError::Refused)),
        "{declined:?}"
    );
    // A link the window refuses outright (not a sign-in link) is not
    // logged either.
    let refused = forward(
        &endpoint,
        AppRequest::OpenLink(SecretText::new(format!(
            "familychat://evil.example/?token={TOKEN}"
        ))),
        Duration::from_secs(5),
    );
    assert!(matches!(refused, Err(ForwardError::Refused)), "{refused:?}");

    {
        let _entered = runtime.enter();
        drop(listener);
    }
    drop(runtime);
    let _ = std::fs::remove_dir_all(&profile);

    let logs = String::from_utf8_lossy(
        &captured
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    )
    .into_owned();
    assert!(
        logs.contains("served a second launch"),
        "the hand-off logged at all: {logs}"
    );
    for secret in [
        TOKEN,
        "familychat://",
        "account_provider",
        "smith.safechat.family",
    ] {
        assert!(!logs.contains(secret), "{secret} reached the logs: {logs}");
    }
}
