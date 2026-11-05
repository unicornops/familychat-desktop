//! The account's cross-signing identity against a mocked homeserver
//! (wiremock) that asks for user-interactive auth before the first upload:
//! the server Family Chat's Palpo is not (it honours MSC3967), so the
//! path a code sign-in takes on another server, and a password sign-in's
//! answer, are checked here on every OS (ADR 0009).

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use common::{TestProfile, account_session, captured_logs, log_text};
use familychat_core::{
    encryption::{AuthOutcome, EncryptionStatus, IdentitySetup, SecretText},
    session::SessionApi,
};
use familychat_security::{Secret, keychain::save_session};
use familychat_session::{AnyHomeserver, LiveSession, account_key};
use familychat_store::NewAccount;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

const USER: &str = "@kid:mock.test";
const DEVICE: &str = "KIDLAPTOP";
const PASSWORD: &str = "kid-password-Zp4";
const UPLOAD: &str = "/_matrix/client/v3/keys/device_signing/upload";

fn mock_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a plain client")
}

/// The cross-signing upload: user-interactive auth with `m.login.password`
/// unless the body carries the right password.
struct DeviceSigning {
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Respond for DeviceSigning {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        self.bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(body.clone());
        let auth = &body["auth"];
        let uia = |error: Option<&str>| {
            let mut answer = json!({
                "flows": [{ "stages": ["m.login.password"] }],
                "params": {},
                "session": "uia-session-1",
            });
            if let Some(error) = error {
                answer["errcode"] = json!("M_FORBIDDEN");
                answer["error"] = json!(error);
            }
            ResponseTemplate::new(401).set_body_json(answer)
        };
        if auth.is_null() {
            return uia(None);
        }
        if auth["type"] == "m.login.password"
            && auth["password"] == PASSWORD
            && auth["session"] == "uia-session-1"
            && auth["identifier"]["user"] == USER
        {
            return ResponseTemplate::new(200).set_body_json(json!({}));
        }
        uia(Some("Invalid password"))
    }
}

async fn mock_server(bodies: Arc<Mutex<Vec<Value>>>) -> MockServer {
    let server = MockServer::start().await;
    let ok = |path_: &'static str, verb: &'static str, body: Value| {
        Mock::given(method(verb))
            .and(path(path_))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .with_priority(1)
    };
    ok(
        "/_matrix/client/versions",
        "GET",
        json!({
            "versions": ["v1.11", "v1.12"],
            "unstable_features": { "org.matrix.simplified_msc3575": true },
        }),
    )
    .mount(&server)
    .await;
    ok(
        "/_matrix/client/v3/keys/upload",
        "POST",
        json!({ "one_time_key_counts": { "signed_curve25519": 50 } }),
    )
    .mount(&server)
    .await;
    // The server publishes no identity for the account.
    ok(
        "/_matrix/client/v3/keys/query",
        "POST",
        json!({ "device_keys": {}, "failures": {} }),
    )
    .mount(&server)
    .await;
    ok(
        "/_matrix/client/v3/keys/signatures/upload",
        "POST",
        json!({ "failures": {} }),
    )
    .mount(&server)
    .await;
    Mock::given(method("POST"))
        .and(path(UPLOAD))
        .respond_with(DeviceSigning { bodies })
        .with_priority(1)
        .mount(&server)
        .await;
    // Everything else (the sync, backups, account data) is left out.
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errcode": "M_NOT_FOUND",
            "error": "Not mocked",
        })))
        .with_priority(10)
        .mount(&server)
        .await;
    server
}

async fn identity(
    status: &mut tokio::sync::watch::Receiver<EncryptionStatus>,
    what: &str,
    accept: impl Fn(&IdentitySetup) -> bool,
) -> Result<IdentitySetup> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = status.borrow_and_update().identity.clone();
            if accept(&current) {
                return Ok(current);
            }
            status.changed().await.context("the account went away")?;
        }
    })
    .await
    .with_context(|| format!("no {what}; the status is {:?}", status.borrow()))?
}

#[tokio::test]
async fn a_code_sign_in_is_asked_for_the_password_and_the_password_is_used_once() -> Result<()> {
    let _logs = captured_logs();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let server = mock_server(bodies.clone()).await;
    let test = TestProfile::new("uia-code");
    // A code sign-in: restored with no password to give.
    let profile = test.open()?;
    let id = profile.add_account(&NewAccount {
        user_id: USER.to_owned(),
        homeserver_url: server.uri(),
    })?;
    save_session(
        test.keychain.as_ref(),
        id,
        &account_session(USER, DEVICE, &server.uri(), "access", None),
    )?;
    drop(profile);
    let manager = Arc::new(test.manager(mock_client())?);
    manager.restore_all().await?;
    let session = LiveSession::new(manager.clone(), tokio::runtime::Handle::current());
    let key = account_key(id);
    let mut status = session.encryption(&key).context("a status")?;

    identity(&mut status, "a demand for the password", |identity| {
        *identity == IdentitySetup::NeedsPassword
    })
    .await?;

    let wrong = session
        .set_up_identity(&key, Some(SecretText::from("not-the-password")))
        .await
        .context("an answer")??;
    ensure!(wrong == AuthOutcome::WrongPassword, "{wrong:?}");
    let right = session
        .set_up_identity(&key, Some(SecretText::from(PASSWORD)))
        .await
        .context("an answer")??;
    ensure!(right == AuthOutcome::Done, "{right:?}");
    identity(&mut status, "the identity", |identity| {
        *identity == IdentitySetup::Ready
    })
    .await?;

    let bodies = bodies.lock().expect("bodies").clone();
    let with_password: Vec<&Value> = bodies
        .iter()
        .filter(|body| body["auth"]["password"].is_string())
        .collect();
    ensure!(
        with_password.len() == 2,
        "the password went only with the two answers: {} uploads, {} with a password",
        bodies.len(),
        with_password.len()
    );
    ensure!(
        bodies.iter().any(|body| body["auth"].is_null()),
        "the first upload asked without auth (MSC3967)"
    );
    // No other request carried it.
    let requests = server.received_requests().await.unwrap_or_default();
    let carrying: Vec<String> = requests
        .iter()
        .filter(|request| {
            request.url.path() != UPLOAD
                && String::from_utf8_lossy(&request.body).contains(PASSWORD)
        })
        .map(|request| request.url.path().to_owned())
        .collect();
    ensure!(carrying.is_empty(), "the password went to {carrying:?}");
    ensure!(!log_text().contains(PASSWORD), "the password is in the log");
    manager.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_password_sign_in_answers_the_demand_with_its_password() -> Result<()> {
    let _logs = captured_logs();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let server = mock_server(bodies.clone()).await;
    let test = TestProfile::new("uia-password");
    let manager = Arc::new(test.manager(mock_client())?);
    let id = manager
        .add_account_with_password(
            account_session(USER, DEVICE, &server.uri(), "access", None),
            &AnyHomeserver,
            Some(Secret::new(PASSWORD.to_owned())),
        )
        .await?;
    let session = LiveSession::new(manager.clone(), tokio::runtime::Handle::current());
    let mut status = session.encryption(&account_key(id)).context("a status")?;
    identity(&mut status, "the identity", |identity| {
        *identity == IdentitySetup::Ready
    })
    .await?;
    let bodies = bodies.lock().expect("bodies").clone();
    ensure!(
        bodies.len() == 2
            && bodies[0]["auth"].is_null()
            && bodies[1]["auth"]["password"] == PASSWORD,
        "one upload without auth, then one with the password: {}",
        bodies
            .iter()
            .map(|body| body["auth"]["type"].to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    ensure!(!log_text().contains(PASSWORD), "the password is in the log");
    manager.shutdown().await;
    Ok(())
}
