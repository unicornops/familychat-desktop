//! Signing out and removing an account (PLAN.md, section 5.4; ADR 0015),
//! against a mocked homeserver with the mock keychain and a temporary
//! profile: the server is told when it can be, and everything the account
//! left on this computer goes either way (its keychain entries, its SDK
//! stores and caches, its drafts and its registry row).

mod common;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use common::{TestProfile, account_session, files_under};
use familychat_core::{
    messaging::Draft,
    session::{SessionApi, SessionError, SignedOut},
    timeline::MessageBody,
};
use familychat_security::keychain::save_session;
use familychat_session::{LiveSession, account_key};
use familychat_store::NewAccount;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const USER: &str = "@alice:mock.test";
const DEVICE: &str = "MOCKDEVICE";
const LOGOUT: &str = "/_matrix/client/v3/logout";

fn mock_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a plain client")
}

/// `/versions` with native sliding sync, a logout answered `logout`, and
/// every other request refused (the sync stops early; nothing else is
/// needed).
async fn mock_server(logout: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/_matrix/client/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "versions": ["v1.11", "v1.12"],
            "unstable_features": { "org.matrix.simplified_msc3575": true },
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(LOGOUT))
        .respond_with(logout)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "errcode": "M_FORBIDDEN",
            "error": "Not in this test.",
        })))
        .with_priority(10)
        .mount(&server)
        .await;
    server
}

/// A registered account with its session in the keychain, opened, with a
/// draft kept and files in its SDK and cache directories.
async fn signed_in(
    test: &TestProfile,
    homeserver: &str,
) -> Result<(
    Arc<familychat_session::AccountManager>,
    LiveSession,
    familychat_core::session::AccountKey,
)> {
    {
        let profile = test.open()?;
        let id = profile.add_account(&NewAccount {
            user_id: USER.to_owned(),
            homeserver_url: homeserver.to_owned(),
        })?;
        save_session(
            test.keychain.as_ref(),
            id,
            &account_session(USER, DEVICE, homeserver, "access", None),
        )?;
    }
    let manager = Arc::new(test.manager(mock_client())?);
    let restored = manager.restore_all().await?;
    ensure!(
        restored.len() == 1 && restored[0].result.is_ok(),
        "{restored:?}"
    );
    let id = restored[0].id;
    let key = account_key(id);
    let session = LiveSession::new(manager.clone(), tokio::runtime::Handle::current());
    let room = familychat_core::RoomId::parse("!kitchen:mock.test")?;
    session
        .save_draft(
            &key,
            &room,
            Some(Draft {
                text: MessageBody::new("Back at six"),
                mentions: Vec::new(),
                reply_to: None,
                editing: None,
            }),
        )
        .await
        .context("answered")??;
    ensure!(
        session
            .draft(&key, &room)
            .await
            .context("answered")??
            .is_some(),
        "the draft is kept"
    );
    let dir = test.dir();
    ensure!(
        !files_under(&dir.account_sdk_dir(id))?.is_empty(),
        "the SDK's stores are on disk"
    );
    Ok((manager, session, key))
}

/// Everything the account left, checked gone: the keychain holds only the
/// app store's passphrase, the registry is empty and so is `accounts/`.
fn assert_gone(test: &TestProfile, manager: &familychat_session::AccountManager) -> Result<()> {
    ensure!(
        test.keychain.entry_names() == ["app-store"],
        "keychain entries left: {:?}",
        test.keychain.entry_names()
    );
    ensure!(
        manager
            .with_profile(|profile| profile.accounts())?
            .is_empty(),
        "the registry still has the account"
    );
    let accounts = test.dir().accounts_dir();
    let left = if accounts.exists() {
        files_under(&accounts)?
    } else {
        Vec::new()
    };
    ensure!(left.is_empty(), "files left: {left:?}");
    Ok(())
}

#[tokio::test]
async fn signing_out_tells_the_server_and_removes_everything() -> Result<()> {
    let server = mock_server(ResponseTemplate::new(200).set_body_json(json!({}))).await;
    let test = TestProfile::new("sign-out");
    let (manager, session, key) = signed_in(&test, &server.uri()).await?;
    let accounts = session.accounts();
    ensure!(accounts.borrow().len() == 1);

    let signed_out = tokio::time::timeout(Duration::from_secs(30), session.sign_out(&key))
        .await
        .context("in time")?
        .context("answered")??;
    ensure!(signed_out == SignedOut::Everywhere, "{signed_out:?}");
    let logouts = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == LOGOUT)
        .count();
    ensure!(logouts == 1, "{logouts} logout requests");
    ensure!(accounts.borrow().is_empty(), "the account left the list");
    assert_gone(&test, &manager)?;

    // It is gone for the session too, and a second sign-out says so.
    ensure!(session.account_status(&key).is_none());
    let again = session.sign_out(&key).await.context("answered")?;
    ensure!(again == Err(SessionError::UnknownAccount), "{again:?}");
    Ok(())
}

#[tokio::test]
async fn a_server_that_refuses_the_sign_out_does_not_keep_the_account_here() -> Result<()> {
    let server = mock_server(ResponseTemplate::new(500)).await;
    let test = TestProfile::new("sign-out-refused");
    let (manager, session, key) = signed_in(&test, &server.uri()).await?;

    let signed_out = tokio::time::timeout(Duration::from_secs(30), session.sign_out(&key))
        .await
        .context("in time")?
        .context("answered")??;
    ensure!(signed_out == SignedOut::OnlyHere, "{signed_out:?}");
    ensure!(SignedOut::OnlyHere.message().contains("another device"));
    assert_gone(&test, &manager)?;
    Ok(())
}

#[tokio::test]
async fn a_server_that_has_gone_does_not_keep_the_account_here() -> Result<()> {
    // Nothing listens on the discard port: the server can't be reached at
    // all, from the start.
    let test = TestProfile::new("sign-out-offline");
    let (manager, session, key) = signed_in(&test, "http://127.0.0.1:9").await?;

    let signed_out = tokio::time::timeout(Duration::from_secs(30), session.sign_out(&key))
        .await
        .context("in time")?
        .context("answered")??;
    ensure!(signed_out == SignedOut::OnlyHere, "{signed_out:?}");
    assert_gone(&test, &manager)?;
    Ok(())
}

#[tokio::test]
async fn files_a_sign_out_left_behind_are_swept_at_the_next_start() -> Result<()> {
    let test = TestProfile::new("sign-out-sweep");
    let leftover = test
        .dir()
        .account_cache_dir(familychat_security::AccountId::generate()?);
    std::fs::create_dir_all(&leftover)?;
    std::fs::write(
        leftover.join("media.sqlite3"),
        b"ciphertext whose key is gone",
    )?;
    let manager = test.manager(mock_client())?;
    manager.restore_all().await?;
    ensure!(
        !leftover.exists(),
        "the leftover directory was swept at start"
    );
    Ok(())
}
