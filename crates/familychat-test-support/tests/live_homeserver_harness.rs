//! Live tests against the homeserver harness (ADR 0004). Start it first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-test-support --test live_homeserver_harness -- --ignored
//! scripts/homeserver-harness.sh down
//! ```
//!
//! They pin down what later phases rely on from Palpo at Family Chat's
//! image (PLAN.md, section 6.10) and from the harness itself.

use anyhow::{Context, Result, ensure};
use familychat_test_support::{HomeserverHarness, Reply};
use reqwest::Method;
use serde_json::{Value, json};

fn harness() -> Result<HomeserverHarness> {
    HomeserverHarness::connect()
}

async fn get(harness: &HomeserverHarness, base: &str, path: &[&str]) -> Result<Reply> {
    harness.get(HomeserverHarness::url(base, path)?).await
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn palpo_advertises_the_spec_versions_and_login_types_family_chat_relies_on() -> Result<()> {
    let harness = harness()?;
    let versions = get(
        &harness,
        harness.hs1_url(),
        &["_matrix", "client", "versions"],
    )
    .await?
    .ok("hs1 /versions")?;
    println!("hs1 /versions: {}", versions.body);
    let advertised = strings(&versions.body["versions"]);
    for version in ["v1.11", "v1.12"] {
        ensure!(
            advertised.contains(&version),
            "Palpo no longer advertises {version}: {advertised:?}"
        );
    }
    let unstable = &versions.body["unstable_features"];
    // Native sliding sync (MSC4186), which matrix-sdk's SyncService needs,
    // and authenticated media.
    ensure!(
        unstable["org.matrix.simplified_msc3575"] == true,
        "no native sliding sync: {unstable}"
    );
    ensure!(
        unstable["org.matrix.msc3916.stable"] == true,
        "no authenticated media: {unstable}"
    );
    if let Some(name) = versions.body["server"]["name"].as_str() {
        ensure!(name == "Palpo", "hs1 is not Palpo but {name}");
    }

    let login = harness
        .client_api(Method::GET, &["v3", "login"], None, None)
        .await?
        .ok("hs1 GET /login")?;
    let types: Vec<&str> = login.body["flows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|flow| flow["type"].as_str())
        .collect();
    for login_type in ["m.login.password", "m.login.token"] {
        ensure!(
            types.contains(&login_type),
            "hs1 does not offer {login_type}: {types:?}"
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn palpo_answers_native_sliding_sync() -> Result<()> {
    let harness = harness()?;
    let user = harness.new_user("sliding").await?;
    let body = json!({
        "conn_id": "harness",
        "lists": { "all": { "ranges": [[0, 10]], "timeline_limit": 1 } },
    });
    let reply = harness
        .client_api(
            Method::POST,
            &["unstable", "org.matrix.simplified_msc3575", "sync"],
            Some(&user.session),
            Some(&body),
        )
        .await?
        .ok("native sliding sync")?;
    ensure!(
        reply.str_field("pos").is_some(),
        "the sliding sync response has no pos: {}",
        reply.body
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn token_gated_registration_then_password_login() -> Result<()> {
    let harness = harness()?;
    let localpart = HomeserverHarness::unique_localpart("registered");
    let password = format!("{localpart}-password");
    let mxid = harness.mxid(&localpart);

    let registered = harness.register_user(&localpart, &password).await?;
    ensure!(
        registered.user_id == mxid,
        "registered {}",
        registered.user_id
    );

    let session = harness.login_password(&localpart, &password).await?;
    ensure!(session.user_id == mxid, "signed in as {}", session.user_id);
    ensure!(
        session.device_id != registered.device_id,
        "the login reused the registration's device"
    );
    let whoami = harness
        .client_api(
            Method::GET,
            &["v3", "account", "whoami"],
            Some(&session),
            None,
        )
        .await?
        .ok("whoami")?;
    ensure!(
        whoami.str_field("user_id") == Some(mxid.as_str()),
        "whoami: {}",
        whoami.body
    );

    let wrong = harness
        .client_api(
            Method::POST,
            &["v3", "login"],
            None,
            Some(&json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": localpart },
                "password": "not the password",
            })),
        )
        .await?;
    ensure!(
        wrong.status == 403 && wrong.errcode() == Some("M_FORBIDDEN"),
        "a wrong password answered {}: {}",
        wrong.status,
        wrong.body
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_minted_login_token_redeems_once() -> Result<()> {
    let harness = harness()?;
    let localpart = HomeserverHarness::unique_localpart("coded");
    let mxid = harness
        .create_user(&localpart, &format!("{localpart}-password"))
        .await?;
    // family-chat's LOGIN_TOKEN_TTL_MS.
    let token = harness.mint_login_token(&mxid, 300_000).await?;
    ensure!(
        token.expires_in_ms == 300_000,
        "granted {} ms",
        token.expires_in_ms
    );

    let first = harness.login_with_token(&token).await?;
    ensure!(
        first.status == 200,
        "redeeming the token answered {}: {}",
        first.status,
        first.body
    );
    ensure!(
        first.str_field("user_id") == Some(mxid.as_str()),
        "the token signed in {:?}, not {mxid}",
        first.str_field("user_id")
    );
    ensure!(first.str_field("access_token").is_some(), "no access token");

    let replay = harness.login_with_token(&token).await?;
    ensure!(
        replay.status == 403 && replay.errcode() == Some("M_FORBIDDEN"),
        "replaying the token answered {}: {}",
        replay.status,
        replay.body
    );
    Ok(())
}

/// PLAN.md section 6.10 lists MSC3967 (the first cross-signing upload
/// without user-interactive auth) as unverified on the pin (family-chat
/// #257). A member signed in with a code has no password to give, so this
/// is what lets such an account set up cross-signing.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_code_signed_in_member_uploads_cross_signing_keys_without_uia() -> Result<()> {
    let harness = harness()?;
    let localpart = HomeserverHarness::unique_localpart("crosssign");
    let mxid = harness
        .create_user(&localpart, &format!("{localpart}-password"))
        .await?;
    let token = harness.mint_login_token(&mxid, 300_000).await?;
    let login = harness.login_with_token(&token).await?.ok("token login")?;
    let session = familychat_test_support::Session::from_login_body(&login.body)?;

    let master_key = |public: &str| {
        json!({
            "master_key": {
                "user_id": mxid,
                "usage": ["master"],
                "keys": { format!("ed25519:{public}"): public },
            }
        })
    };
    let first = harness
        .client_api(
            Method::POST,
            &["v3", "keys", "device_signing", "upload"],
            Some(&session),
            Some(&master_key(
                "bWFzdGVyS2V5Rm9yVGhlSGFybmVzc0Nyb3NzU2lnbmluZw",
            )),
        )
        .await?;
    ensure!(
        first.status == 200,
        "the first cross-signing upload answered {} (MSC3967 not honoured?): {}",
        first.status,
        first.body
    );

    // Replacing the keys still needs auth.
    let replace = harness
        .client_api(
            Method::POST,
            &["v3", "keys", "device_signing", "upload"],
            Some(&session),
            Some(&master_key(
                "cmVwbGFjZW1lbnRLZXlGb3JUaGVIYXJuZXNzQ3Jvc3NTaWdu",
            )),
        )
        .await?;
    ensure!(
        replace.status == 401 && replace.body.get("flows").is_some(),
        "replacing the cross-signing keys answered {}: {}",
        replace.status,
        replace.body
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_custom_domain_delegates_to_hs1() -> Result<()> {
    let harness = harness()?;
    let domain = harness.custom_domain_url();

    let client = get(&harness, &domain, &[".well-known", "matrix", "client"])
        .await?
        .ok("smith.test client well-known")?;
    let base_url = client.body["m.homeserver"]["base_url"]
        .as_str()
        .context("no m.homeserver.base_url")?;
    ensure!(
        base_url == harness.hs1_url(),
        "smith.test delegates to {base_url}"
    );
    let server = get(&harness, &domain, &[".well-known", "matrix", "server"])
        .await?
        .ok("smith.test server well-known")?;
    ensure!(
        server.str_field("m.server") == Some("hs1.familychat.test:443"),
        "smith.test's m.server: {}",
        server.body
    );

    // The domain itself serves no Matrix API.
    let api = get(&harness, &domain, &["_matrix", "client", "versions"]).await?;
    ensure!(
        api.status == 404,
        "smith.test answered /versions with {}",
        api.status
    );

    // The delegated host answers, and Palpo's own well-known agrees.
    get(&harness, base_url, &["_matrix", "client", "versions"])
        .await?
        .ok("the delegated homeserver's /versions")?;
    let own = get(
        &harness,
        harness.hs1_url(),
        &[".well-known", "matrix", "client"],
    )
    .await?
    .ok("hs1 client well-known")?;
    ensure!(
        own.body["m.homeserver"]["base_url"].as_str() == Some(harness.hs1_url()),
        "hs1's own well-known: {}",
        own.body
    );
    Ok(())
}

/// The front end exposes only what family-chat's edge proxy does.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn hs1_exposes_only_the_edge_proxy_paths() -> Result<()> {
    let harness = harness()?;
    for (path, status) in [
        (&["_synapse", "admin", "v1", "server_version"][..], 404),
        (&["_synapse", "client", "password_reset"][..], 404),
        (&["_matrix", "federation", "v1", "version"][..], 403),
        (&["index.html"][..], 404),
    ] {
        let reply = get(&harness, harness.hs1_url(), path).await?;
        ensure!(
            reply.status == status,
            "/{} answered {}, not {status}",
            path.join("/"),
            reply.status
        );
    }
    get(
        &harness,
        harness.hs1_url(),
        &["_matrix", "key", "v2", "server"],
    )
    .await?
    .ok("hs1 server keys")?;
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_non_allowlisted_host_is_a_working_homeserver() -> Result<()> {
    let harness = harness()?;
    let well_known = get(
        &harness,
        harness.evil_url(),
        &[".well-known", "matrix", "client"],
    )
    .await?
    .ok("the non-allowlisted host's well-known")?;
    ensure!(
        well_known.body["m.homeserver"]["base_url"].as_str() == Some(harness.evil_url()),
        "its well-known: {}",
        well_known.body
    );
    get(
        &harness,
        harness.evil_url(),
        &["_matrix", "client", "versions"],
    )
    .await?
    .ok("the non-allowlisted host's /versions")?;
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn synapse_answers_versions() -> Result<()> {
    let harness = harness()?;
    let versions = get(
        &harness,
        harness.synapse_url(),
        &["_matrix", "client", "versions"],
    )
    .await?
    .ok("Synapse /versions")?;
    println!("Synapse /versions: {}", versions.body);
    let advertised = strings(&versions.body["versions"]);
    ensure!(
        advertised.contains(&"v1.12"),
        "Synapse versions: {advertised:?}"
    );
    // The answer is Synapse's: its signing key names synapse.test.
    let keys = get(
        &harness,
        harness.synapse_url(),
        &["_matrix", "key", "v2", "server"],
    )
    .await?
    .ok("Synapse server keys")?;
    ensure!(
        keys.str_field("server_name") == Some("synapse.test"),
        "Synapse's keys name {:?}",
        keys.str_field("server_name")
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn members_share_a_room_and_see_each_others_messages() -> Result<()> {
    let harness = harness()?;
    let alice = harness.new_user("alice").await?;
    let bob = harness.new_user("bob").await?;
    let room = harness
        .create_room(&alice.session, "Harness family", &[bob.user_id.as_str()])
        .await?;
    harness.join_room(&bob.session, &room).await?;
    harness
        .send_text(&bob.session, &room, "hello from bob")
        .await?
        .ok("bob's message")?;

    let events = harness.recent_events(&alice.session, &room).await?;
    ensure!(
        events.iter().any(|event| {
            event["type"] == "m.room.message"
                && event["sender"] == bob.user_id.as_str()
                && event["content"]["body"] == "hello from bob"
        }),
        "alice does not see bob's message: {events:?}"
    );
    Ok(())
}

/// A curfew suspends the account (PLAN.md section 6.8): sends fail with
/// `M_USER_SUSPENDED`, and lifting it lets the member send again.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_suspended_member_cannot_send() -> Result<()> {
    let harness = harness()?;
    let member = harness.new_user("curfew").await?;
    let room = harness.create_room(&member.session, "Curfew", &[]).await?;
    harness
        .send_text(&member.session, &room, "before the curfew")
        .await?
        .ok("a send before the suspension")?;

    harness.set_suspended(&member.user_id, true).await?;
    let refused = harness
        .send_text(&member.session, &room, "during the curfew")
        .await?;
    ensure!(
        refused.status == 403 && refused.errcode() == Some("M_USER_SUSPENDED"),
        "a suspended member's send answered {}: {}",
        refused.status,
        refused.body
    );

    // What reads do while suspended: PLAN.md section 6.8 expects them to
    // keep working; this records what the pinned Palpo does.
    let whoami = harness
        .client_api(
            Method::GET,
            &["v3", "account", "whoami"],
            Some(&member.session),
            None,
        )
        .await?;
    println!(
        "while suspended, whoami answers {} {:?}",
        whoami.status,
        whoami.errcode()
    );

    harness.set_suspended(&member.user_id, false).await?;
    harness
        .send_text(&member.session, &room, "after the curfew")
        .await?
        .ok("a send after the suspension is lifted")?;
    Ok(())
}
