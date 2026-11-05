//! The sign-in's log lines carry no token, access token or password
//! (PLAN.md, section 6.4 rule 8), checked with a subscriber that redacts
//! nothing, at TRACE for every target. Alone in its own test binary, so
//! the subscriber is the process's global one and sees every event.

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use common::*;
use familychat_core::signin::{SigninAnswer, SigninStep};
use serde_json::json;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(bytes);
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
fn rule8_no_token_or_password_reaches_a_log_line_even_unredacted() {
    // A subscriber with no redaction at all, at TRACE for every target,
    // so the scrubber cannot hide a leak: the sign-in itself must never
    // log a secret.
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("the only subscriber");
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            // A redeemed code, a rejected code followed by a password, and
            // a code for someone else.
            let harness = Harness::new();
            harness
                .server
                .login(SMITH, Reply::Json(403, json!({ "errcode": "M_FORBIDDEN" })))
                .homeserver(SMITH)
                .login(
                    SMITH,
                    Reply::Json(200, credentials("@bob:smith.safechat.family")),
                )
                .logout(SMITH);
            let mut flow = harness.link(&link_query(
                "smith.safechat.family",
                Some(ANA),
                Some("smith.safechat.family"),
            ));
            flow.settled().await;
            flow.answer(SigninAnswer::Continue);
            flow.settled().await;
            flow.submit("smith.safechat.family", "bob");
            assert!(matches!(flow.ended().await, SigninStep::Finished(_)));

            let harness = Harness::new();
            harness
                .server
                .login(
                    SMITH,
                    Reply::Json(200, credentials("@bob:smith.safechat.family")),
                )
                .logout(SMITH);
            let mut flow = harness.link(&link_query(
                "smith.safechat.family",
                Some(ANA),
                Some("smith.safechat.family"),
            ));
            flow.settled().await;
            flow.answer(SigninAnswer::Continue);
            flow.settled().await;
        });
    }
    let text = String::from_utf8_lossy(&lock(&captured.0)).into_owned();
    assert!(
        text.contains("redeeming a sign-in code"),
        "the capture saw the flow: {text}"
    );
    for secret in [TOKEN, ACCESS, PASSWORD] {
        assert!(!text.contains(secret), "{secret} was logged:\n{text}");
    }
}
