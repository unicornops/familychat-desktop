//! The session against a mocked homeserver (wiremock), with no harness:
//! the states a real server is hard to put into on demand. They run in
//! `cargo test` on every OS.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use common::{TestProfile, account_session, wait_for_sync};
use familychat_core::sync::SyncState;
use familychat_security::{
    MemoryKeychain,
    keychain::{load_session, save_session},
};
use familychat_session::{AddAccountError, AnyHomeserver, OpenError, PostLoginCheck};
use familychat_store::NewAccount;
use serde_json::json;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path},
};

const USER: &str = "@alice:mock.test";
const DEVICE: &str = "MOCKDEVICE";
const SLIDING_SYNC: &str = "/_matrix/client/unstable/org.matrix.simplified_msc3575/sync";

/// A plain HTTP client for the mock server, which is plain HTTP on
/// loopback (the app's own client refuses anything but HTTPS).
fn mock_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a plain client")
}

/// `/versions`, with or without native sliding sync.
async fn mock_versions(server: &MockServer, sliding_sync: bool) {
    Mock::given(method("GET"))
        .and(path("/_matrix/client/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "versions": ["v1.11", "v1.12"],
            "unstable_features": { "org.matrix.simplified_msc3575": sliding_sync },
        })))
        .with_priority(1)
        .mount(server)
        .await;
}

/// Every other request, refused the way the pinned Palpo refuses a
/// suspended account: 403 `M_USER_SUSPENDED`.
fn suspended() -> ResponseTemplate {
    ResponseTemplate::new(403).set_body_json(json!({
        "errcode": "M_USER_SUSPENDED",
        "error": "This account is suspended.",
    }))
}

/// Puts an account in the registry and its session in the keychain, as a
/// sign-in would have, so the manager restores it.
fn register(
    test: &TestProfile,
    homeserver: &str,
    access: &str,
    refresh: Option<&str>,
) -> Result<()> {
    let profile = test.open()?;
    let id = profile.add_account(&NewAccount {
        user_id: USER.to_owned(),
        homeserver_url: homeserver.to_owned(),
    })?;
    save_session(
        test.keychain.as_ref(),
        id,
        &account_session(USER, DEVICE, homeserver, access, refresh),
    )?;
    Ok(())
}

#[tokio::test]
async fn a_server_without_native_sliding_sync_is_not_supported() -> Result<()> {
    let server = MockServer::start().await;
    mock_versions(&server, false).await;
    let test = TestProfile::new("not-supported");
    register(&test, &server.uri(), "access", None)?;

    let manager = test.manager(mock_client())?;
    let restored = manager.restore_all().await?;
    ensure!(restored.len() == 1, "{restored:?}");
    ensure!(restored[0].result.is_ok(), "{restored:?}");
    let account = manager.account(restored[0].id).context("the account")?;
    let mut status = account.status();
    wait_for_sync(
        &mut status,
        Duration::from_secs(10),
        "not supported",
        |state| *state == SyncState::NotSupported,
    )
    .await?;
    ensure!(
        SyncState::NotSupported
            .message()
            .is_some_and(|message| message.contains("native sliding sync"))
    );

    // Nothing is retried on its own: the only requests are the ones that
    // found out (/versions).
    tokio::time::sleep(Duration::from_secs(2)).await;
    let requests = server.received_requests().await.unwrap_or_default();
    ensure!(
        requests
            .iter()
            .all(|request| request.url.path() == "/_matrix/client/versions"),
        "unexpected requests: {:?}",
        requests
            .iter()
            .map(|r| r.url.path().to_owned())
            .collect::<Vec<_>>()
    );
    manager.shutdown().await;

    // A new sign-in to such a server is refused outright, and forgotten.
    let added = manager
        .add_account(
            account_session("@bob:mock.test", "BOBDEVICE", &server.uri(), "bob", None),
            &AnyHomeserver,
        )
        .await;
    ensure!(
        matches!(added, Err(AddAccountError::Open(OpenError::NotSupported))),
        "{added:?}"
    );
    let registered = manager.with_profile(|profile| profile.accounts())?;
    ensure!(
        registered.len() == 1,
        "the refused sign-in was kept: {registered:?}"
    );
    Ok(())
}

/// family-chat decision 11: a suspended account gets `M_USER_SUSPENDED` on
/// every request on the pinned Palpo, sync included. The account shows the
/// suspended state, and the session checks again after half a minute, not
/// in a loop.
#[tokio::test]
async fn a_suspended_account_is_suspended_not_an_error_and_no_retry_storm() -> Result<()> {
    let server = MockServer::start().await;
    mock_versions(&server, true).await;
    Mock::given(wiremock::matchers::any())
        .respond_with(suspended())
        .with_priority(10)
        .mount(&server)
        .await;
    let test = TestProfile::new("suspended");
    register(&test, &server.uri(), "access", None)?;

    let manager = test.manager(mock_client())?;
    let restored = manager.restore_all().await?;
    let account = manager.account(restored[0].id).context("the account")?;
    let mut status = account.status();
    let state = wait_for_sync(&mut status, Duration::from_secs(20), "suspended", |state| {
        matches!(state, SyncState::Suspended { .. })
    })
    .await?;
    ensure!(
        state
            == SyncState::Suspended {
                recheck_in: Duration::from_secs(30)
            },
        "{state:?}"
    );

    let syncs = || async {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path() == SLIDING_SYNC)
            .count()
    };
    let before = syncs().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = syncs().await;
    ensure!(
        after == before,
        "the suspended account kept syncing: {before} sync requests, then {after}"
    );

    // Asking again checks at once, and the next wait is longer.
    account.retry_sync();
    wait_for_sync(
        &mut status,
        Duration::from_secs(20),
        "the second check",
        |state| {
            *state
                == SyncState::Suspended {
                    recheck_in: Duration::from_secs(60),
                }
        },
    )
    .await?;
    manager.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_revoked_token_signs_the_account_out() -> Result<()> {
    let server = MockServer::start().await;
    mock_versions(&server, true).await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errcode": "M_UNKNOWN_TOKEN",
            "error": "Unknown access token.",
            "soft_logout": false,
        })))
        .with_priority(10)
        .mount(&server)
        .await;
    let test = TestProfile::new("revoked");
    register(&test, &server.uri(), "revoked", None)?;

    let manager = test.manager(mock_client())?;
    let restored = manager.restore_all().await?;
    let account = manager.account(restored[0].id).context("the account")?;
    let mut status = account.status();
    wait_for_sync(
        &mut status,
        Duration::from_secs(20),
        "signed out",
        |state| matches!(state, SyncState::SignedOut { .. }),
    )
    .await?;
    manager.shutdown().await;
    Ok(())
}

/// Answers a request made with the rotated access token, after noting
/// whether the keychain already held that token: the token must be in the
/// keychain before it is used.
struct ChecksTheKeychain {
    keychain: Arc<MemoryKeychain>,
    seen: Arc<Mutex<Vec<bool>>>,
}

impl Respond for ChecksTheKeychain {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let saved = self
            .keychain
            .entry_names()
            .iter()
            .find_map(|name| {
                let id = name.strip_prefix("account-")?.strip_suffix("-session")?;
                let id = id.parse().ok()?;
                load_session(self.keychain.as_ref(), id).ok().flatten()
            })
            .is_some_and(|session| session.access_token.expose() == "access-2");
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(saved);
        // Stop the sync here; this test is about the tokens.
        suspended()
    }
}

#[tokio::test]
async fn rotated_tokens_are_in_the_keychain_before_they_are_used() -> Result<()> {
    let server = MockServer::start().await;
    mock_versions(&server, true).await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-2",
            "refresh_token": "refresh-2",
            "expires_in_ms": 60_000,
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errcode": "M_UNKNOWN_TOKEN",
            "error": "Access token has expired",
            "soft_logout": true,
        })))
        .with_priority(3)
        .mount(&server)
        .await;
    let test = TestProfile::new("rotation");
    let seen = Arc::new(Mutex::new(Vec::new()));
    Mock::given(header("authorization", "Bearer access-2"))
        .respond_with(ChecksTheKeychain {
            keychain: test.keychain.clone(),
            seen: seen.clone(),
        })
        .with_priority(3)
        .mount(&server)
        .await;
    register(&test, &server.uri(), "access-1", Some("refresh-1"))?;

    let manager = test.manager(mock_client())?;
    let restored = manager.restore_all().await?;
    let id = restored[0].id;
    let account = manager.account(id).context("the account")?;
    let mut status = account.status();
    let waited = wait_for_sync(
        &mut status,
        Duration::from_secs(20),
        "the sync to stop",
        |state| matches!(state, SyncState::Suspended { .. }),
    )
    .await;
    waited?;
    manager.shutdown().await;

    let seen = seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    ensure!(!seen.is_empty(), "the rotated token was never used");
    ensure!(
        seen.iter().all(|saved| *saved),
        "the rotated token was used before the keychain had it: {seen:?}"
    );
    let stored = load_session(test.keychain.as_ref(), id)?.context("the stored session")?;
    ensure!(stored.access_token.expose() == "access-2");
    ensure!(
        stored
            .refresh_token
            .as_ref()
            .is_some_and(|token| token.expose() == "refresh-2")
    );
    ensure!(stored.user_id == USER && stored.device_id == DEVICE);
    Ok(())
}

/// Refuses every sign-in, as Phase 4's allowlist re-check refuses a
/// homeserver outside it.
struct RefuseAll;

impl PostLoginCheck for RefuseAll {
    fn check(
        &self,
        _homeserver: &reqwest::Url,
        _user_id: &matrix_sdk::ruma::UserId,
    ) -> Result<(), String> {
        Err("This server isn't a Family Chat server".to_owned())
    }
}

#[tokio::test]
async fn a_refused_sign_in_is_signed_out_and_forgotten() -> Result<()> {
    let server = MockServer::start().await;
    mock_versions(&server, true).await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/logout"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let test = TestProfile::new("refused");
    let manager = test.manager(mock_client())?;
    let added = manager
        .add_account(
            account_session(USER, DEVICE, &server.uri(), "access", None),
            &RefuseAll,
        )
        .await;
    match added {
        Err(AddAccountError::Open(OpenError::Refused(message))) => {
            ensure!(
                message == "This server isn't a Family Chat server",
                "{message}"
            );
        }
        other => anyhow::bail!("the sign-in was not refused: {other:?}"),
    }
    ensure!(
        test.keychain.entry_names() == ["app-store"],
        "{:?}",
        test.keychain.entry_names()
    );
    ensure!(
        manager
            .with_profile(|profile| profile.accounts())?
            .is_empty()
    );
    ensure!(
        !test.root.join("accounts").exists()
            || common::files_under(&test.root.join("accounts"))?.is_empty(),
        "the refused account's files were kept"
    );
    server.verify().await;
    Ok(())
}
