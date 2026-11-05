//! The hand-off to the real account manager against a mocked homeserver
//! (wiremock, plain HTTP on loopback), with no harness: the post-login
//! re-check (PLAN.md, section 6.6) refusing a homeserver outside the
//! allowlist signs the new device out through matrix-sdk and forgets the
//! account, and the manager answers "already signed in" from its registry.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail, ensure};
use familychat_security::{AccountSession, AuthKind, Keychain, MemoryKeychain, Secret};
use familychat_session::{AccountManager, SessionConfig};
use familychat_signin::{
    AccountSink, AddFailure, Allowlist, FamilyChatCheck, link::parse_user_id, wording,
};
use familychat_store::{DataDir, NewAccount, Profile, ProfileState};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

struct TestProfile {
    root: std::path::PathBuf,
    keychain: Arc<MemoryKeychain>,
}

impl TestProfile {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            root: std::env::temp_dir().join(format!(
                "familychat-signin-{name}-{}-{nanos}",
                std::process::id()
            )),
            keychain: Arc::new(MemoryKeychain::new()),
        }
    }

    fn manager(&self) -> Result<AccountManager> {
        let keychain: Arc<dyn Keychain> = self.keychain.clone();
        let profile = match Profile::open(&DataDir::at(&self.root), keychain)? {
            ProfileState::Ready(profile) => profile,
            other => bail!("the test profile did not open: {other:?}"),
        };
        let http = reqwest::Client::builder().no_proxy().build()?;
        Ok(AccountManager::new(
            profile,
            SessionConfig::with_http_client(http),
        ))
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn the_post_login_recheck_signs_out_a_homeserver_outside_the_allowlist() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/_matrix/client/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "versions": ["v1.12"],
            "unstable_features": { "org.matrix.simplified_msc3575": true },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/logout"))
        .and(header("authorization", "Bearer issued-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let test = TestProfile::new("recheck");
    let manager = test.manager()?;
    let user = parse_user_id("@ana:smith.safechat.family").expect("valid");
    // The mock is http://127.0.0.1, as a login response's well_known could
    // have re-pointed a client: the check refuses it.
    let session = AccountSession {
        user_id: user.to_string(),
        device_id: "NEWDEVICE".to_owned(),
        homeserver_url: server.uri(),
        access_token: Secret::new("issued-access".to_owned()),
        refresh_token: None,
        auth_kind: AuthKind::LoginToken,
    };
    let added = manager
        .add(
            session,
            Box::new(FamilyChatCheck::new(Allowlist::production(), user.clone())),
        )
        .await;
    ensure!(
        added == Err(AddFailure::Refused(wording::NOT_FAMILY_CHAT.to_owned())),
        "{added:?}"
    );
    ensure!(!manager.is_signed_in(&user), "the refused account was kept");
    ensure!(
        test.keychain.entry_names() == ["app-store"],
        "{:?}",
        test.keychain.entry_names()
    );
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn already_signed_in_is_read_from_the_registry() -> Result<()> {
    let test = TestProfile::new("signed-in");
    let manager = test.manager()?;
    manager.with_profile(|profile| {
        profile.add_account(&NewAccount {
            user_id: "@ana:smith.safechat.family".to_owned(),
            homeserver_url: "https://smith.safechat.family".to_owned(),
        })
    })?;
    let same = parse_user_id("@ana:Smith.SafeChat.Family").expect("valid");
    let other = parse_user_id("@bob:smith.safechat.family").expect("valid");
    ensure!(manager.is_signed_in(&same));
    ensure!(!manager.is_signed_in(&other));
    Ok(())
}
