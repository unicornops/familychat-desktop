//! The sign-in flows against a scripted homeserver (a recording
//! `Transport`) and a fake account manager, with no network: every rule
//! of the contract's "What each client must implement" that does not need
//! a real server, each test named after its rule (ADR 0006 maps them).
//! The live tests (`tests/live_signin.rs`) prove the same against the
//! homeserver harness, and `tests/logs.rs` the logs.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use common::*;
use familychat_core::{
    session::AccountKey,
    signin::{PasswordKind, SecretText, SigninAnswer, SigninStart, SigninStep, Working},
};
use familychat_security::AuthKind;
use familychat_signin::{AddFailure, Allowlist, http::Method, link::parse_user_id, wording};
use serde_json::{Value, json};
use tokio::sync::Notify;

// --- Rule 1: parsing, and dropping a code unsent --------------------------------

#[tokio::test]
async fn rule1_a_code_for_a_host_outside_the_allowlist_is_dropped_unsent() {
    for hs in [
        "evil.example",
        "safechat.family",
        "evil.example%5C%40smith.safechat.family",
        "evil.example%5C.safechat.family",
        "smith.safechat.family%2F",
    ] {
        let harness = Harness::new();
        let mut flow = harness.link(&link_query("smith.safechat.family", Some(ANA), Some(hs)));
        let step = flow.settled().await;
        let form = password_form(&step);
        assert_eq!(form.notice.as_deref(), Some(wording::CODE_NOT_USED), "{hs}");
        assert_eq!(form.server, "smith.safechat.family");
        assert_eq!(form.username, "ana");
        assert!(
            harness.server.seen().is_empty(),
            "{hs}: {:?}",
            harness.server.seen()
        );
        flow.answer(SigninAnswer::Cancel);
        assert_eq!(flow.ended().await, SigninStep::Cancelled);
    }
}

#[tokio::test]
async fn rule1_text_that_is_not_a_link_is_refused() {
    let harness = Harness::new();
    let flow = harness.start(SigninStart::Link(SecretText::from(
        "Here is your code: https://safechat.family/app/login?account_provider=smith.safechat.family",
    )));
    assert_eq!(
        flow.ended().await,
        SigninStep::NotALink(wording::NOT_A_LINK.to_owned())
    );
    assert!(harness.server.seen().is_empty());
}

#[tokio::test]
async fn rule1_a_hint_on_another_server_is_dropped_and_the_code_binds_to_account_provider() {
    let harness = Harness::new();
    harness
        .server
        .login(SMITH, Reply::Json(200, credentials(ANA)));
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some("@ana:evil.example"),
        Some("smith.safechat.family"),
    ));
    match flow.settled().await {
        SigninStep::Confirm(confirm) => {
            assert_eq!(confirm.user_id, None);
            assert_eq!(confirm.question, "Sign in to smith.safechat.family?");
        }
        other => panic!("{other:?}"),
    }
    flow.answer(SigninAnswer::Continue);
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
}

// --- Rule 2: already signed in --------------------------------------------------

#[tokio::test]
async fn rule2_an_account_already_signed_in_is_not_redeemed() {
    let harness = Harness::new();
    lock(&harness.accounts.signed_in).push(parse_user_id(ANA).expect("valid"));
    let flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    assert_eq!(
        flow.ended().await,
        SigninStep::AlreadySignedIn(
            "You're already signed in as @ana:smith.safechat.family on this computer.".to_owned()
        )
    );
    assert!(harness.server.seen().is_empty());
}

#[tokio::test]
async fn rule2_a_code_without_a_hint_for_a_signed_in_account_signs_the_new_device_out() {
    let harness = Harness::new();
    lock(&harness.accounts.signed_in).push(parse_user_id(ANA).expect("valid"));
    harness
        .server
        .login(SMITH, Reply::Json(200, credentials(ANA)))
        .logout(SMITH);
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        None,
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    assert!(matches!(flow.ended().await, SigninStep::AlreadySignedIn(_)));
    let posts = harness.server.posts();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[1].url.ends_with("/_matrix/client/v3/logout"));
    assert_eq!(posts[1].bearer.as_deref(), Some(ACCESS));
    assert!(harness.accounts.added().is_empty());
}

// --- Rule 3: confirm first ------------------------------------------------------

#[tokio::test]
async fn rule3_a_code_asks_first_and_sends_nothing_until_continue() {
    let harness = Harness::new();
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    match flow.settled().await {
        SigninStep::Confirm(confirm) => {
            assert_eq!(confirm.question, "Sign in as @ana:smith.safechat.family?");
            assert_eq!(confirm.detail, wording::CONFIRM_DETAIL);
            assert_eq!(confirm.server, "smith.safechat.family");
        }
        other => panic!("{other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(harness.server.seen().is_empty());
    flow.answer(SigninAnswer::Cancel);
    assert_eq!(flow.ended().await, SigninStep::Cancelled);
    assert!(harness.server.seen().is_empty());
}

#[tokio::test]
async fn rule3_declining_the_code_offers_the_password_form_and_sends_nothing() {
    let harness = Harness::new();
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::UsePassword);
    let step = flow.settled().await;
    let form = password_form(&step);
    assert_eq!(form.kind, PasswordKind::FamilyChat);
    assert_eq!(form.server, "smith.safechat.family");
    assert_eq!(form.username, "ana");
    assert!(harness.server.seen().is_empty());
}

#[tokio::test]
async fn rule3_a_frontend_that_goes_away_cancels_and_sends_nothing() {
    let harness = Harness::new();
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    assert_eq!(flow.settled().await, SigninStep::Confirm(confirm_ana()));
    assert_eq!(flow.abandon().await, SigninStep::Cancelled);
    assert!(harness.server.seen().is_empty());
}

fn confirm_ana() -> familychat_core::signin::ConfirmCode {
    familychat_core::signin::ConfirmCode {
        question: "Sign in as @ana:smith.safechat.family?".to_owned(),
        detail: wording::CONFIRM_DETAIL.to_owned(),
        user_id: parse_user_id(ANA),
        server: "smith.safechat.family".to_owned(),
    }
}

// --- Rule 4: one POST to hs, once, one at a time --------------------------------

#[tokio::test]
async fn rule4_continue_redeems_the_code_once_at_hs() {
    let harness = Harness::new();
    harness
        .server
        .login(SMITH, Reply::Json(200, credentials(ANA)));
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    // A second Continue (a double click) changes nothing.
    flow.answer(SigninAnswer::Continue);
    match flow.ended().await {
        SigninStep::Finished(finished) => {
            assert_eq!(finished.user_id.as_str(), ANA);
            assert_eq!(finished.account, AccountKey("new-account".to_owned()));
        }
        other => panic!("{other:?}"),
    }
    let seen = harness.server.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].url, format!("{SMITH}/_matrix/client/v3/login"));
    let body: Value = serde_json::from_str(seen[0].body.as_deref().expect("a body")).expect("JSON");
    assert_eq!(
        body,
        json!({ "type": "m.login.token", "token": TOKEN, "initial_device_display_name": DEVICE })
    );
    assert_eq!(
        harness.accounts.added(),
        [(ANA.to_owned(), SMITH.to_owned(), AuthKind::LoginToken)]
    ); // A code has no password to hand on: the session relies on MSC3967
    // for the first cross-signing upload (ADR 0009).
    assert_eq!(*harness.accounts.passwords.lock().expect("lock"), [None]);
}

#[tokio::test]
async fn rule4_a_custom_domain_code_goes_to_hs_without_discovery() {
    let harness = Harness::new();
    harness
        .server
        .login(SMITH, Reply::Json(200, credentials("@kid:smith.ie")));
    let mut flow = harness.link(&link_query(
        "smith.ie",
        Some("@kid:smith.ie"),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
    let seen = harness.server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].url, format!("{SMITH}/_matrix/client/v3/login"));
}

#[tokio::test]
async fn rule4_only_one_redemption_is_in_flight() {
    let harness = Harness::new();
    let release = Arc::new(Notify::new());
    harness
        .server
        .login(SMITH, Reply::After(release.clone(), 200, credentials(ANA)));
    let query = link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    );
    let mut first = harness.link(&query);
    let mut second = harness.link(&query);
    first.settled().await;
    second.settled().await;
    first.answer(SigninAnswer::Continue);
    // Wait until the first is redeeming.
    tokio::time::timeout(Duration::from_secs(10), async {
        while harness.server.seen().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first redeems");
    second.answer(SigninAnswer::Continue);
    let step = second.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::CODE_BUSY)
    );
    release.notify_one();
    assert!(matches!(first.ended().await, SigninStep::Finished(_)));
    assert_eq!(harness.server.posts().len(), 1);
}

#[tokio::test]
async fn rule4_a_redirect_is_not_followed() {
    let harness = Harness::new();
    harness.server.login(SMITH, Reply::Json(307, json!({})));
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::CODE_FAILED)
    );
    assert_eq!(harness.server.seen().len(), 1);
}

// --- Rule 5: bind the user ------------------------------------------------------

#[tokio::test]
async fn rule5_a_code_that_signs_in_someone_else_is_signed_out_and_refused() {
    for (hint, issued) in [
        (Some(ANA), "@bob:smith.safechat.family"),
        (Some(ANA), "@ana:evil.example"),
        (None, "@ana:evil.example"),
        (None, "@ana:other.safechat.family"),
    ] {
        let harness = Harness::new();
        harness
            .server
            .login(SMITH, Reply::Json(200, credentials(issued)))
            .logout(SMITH);
        let mut flow = harness.link(&link_query(
            "smith.safechat.family",
            hint,
            Some("smith.safechat.family"),
        ));
        flow.settled().await;
        flow.answer(SigninAnswer::Continue);
        let step = flow.settled().await;
        assert_eq!(
            password_form(&step)
                .problem
                .as_ref()
                .map(|p| p.message.as_str()),
            Some(wording::CODE_FOR_ANOTHER_ACCOUNT),
            "{issued}"
        );
        let posts = harness.server.posts();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[1].url.ends_with("/logout"));
        assert_eq!(posts[1].bearer.as_deref(), Some(ACCESS));
        assert!(harness.accounts.added().is_empty());
    }
}

// --- Rule 6: used or expired, and signing out after a later failure -------------

#[tokio::test]
async fn rule6_a_401_or_403_says_used_or_expired_and_offers_the_password() {
    for status in [401, 403] {
        let harness = Harness::new();
        harness.server.login(
            SMITH,
            Reply::Json(status, json!({ "errcode": "M_FORBIDDEN" })),
        );
        let mut flow = harness.link(&link_query(
            "smith.safechat.family",
            Some(ANA),
            Some("smith.safechat.family"),
        ));
        flow.settled().await;
        flow.answer(SigninAnswer::Continue);
        let step = flow.settled().await;
        let form = password_form(&step);
        assert_eq!(
            form.problem.as_ref().map(|p| p.message.as_str()),
            Some("That code has been used or has expired. Ask your parent for a new one.")
        );
        assert_eq!(form.server, "smith.safechat.family");
        assert_eq!(form.username, "ana");
    }
}

#[tokio::test]
async fn rule6_a_failure_after_the_credentials_were_issued_signs_the_device_out() {
    for (failure, message) in [
        (AddFailure::NotSupported, wording::NOT_SUPPORTED),
        (
            AddFailure::Failed("the keychain is locked".to_owned()),
            wording::COULD_NOT_SAVE,
        ),
    ] {
        let harness = Harness::new();
        *lock(&harness.accounts.failure) = Some(failure);
        harness
            .server
            .login(SMITH, Reply::Json(200, credentials(ANA)))
            .logout(SMITH);
        let mut flow = harness.link(&link_query(
            "smith.safechat.family",
            Some(ANA),
            Some("smith.safechat.family"),
        ));
        flow.settled().await;
        flow.answer(SigninAnswer::Continue);
        let step = flow.settled().await;
        assert_eq!(
            password_form(&step)
                .problem
                .as_ref()
                .map(|p| p.message.as_str()),
            Some(message)
        );
        let posts = harness.server.posts();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[1].url.ends_with("/logout"));
    }
}

#[tokio::test]
async fn rule6_a_login_answer_without_a_usable_account_is_signed_out() {
    let harness = Harness::new();
    harness
        .server
        .login(
            SMITH,
            Reply::Json(
                200,
                json!({ "access_token": ACCESS, "user_id": "not an id" }),
            ),
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
    let posts = harness.server.posts();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert_eq!(posts[1].bearer.as_deref(), Some(ACCESS));
}

#[tokio::test]
async fn rule6_a_rate_limited_code_shows_the_wait_and_is_not_retried() {
    let harness = Harness::new();
    harness.server.login(
        SMITH,
        Reply::Json(
            429,
            json!({ "errcode": "M_LIMIT_EXCEEDED", "retry_after_ms": 12_000 }),
        ),
    );
    let mut flow = harness.link(&link_query(
        "smith.safechat.family",
        Some(ANA),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let problem = password_form(&step).problem.clone().expect("a problem");
    assert_eq!(
        problem.message,
        "Too many sign-in attempts. Try again in 12 seconds."
    );
    assert_eq!(problem.retry_after, Some(Duration::from_secs(12)));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.server.seen().len(), 1);
}

// --- Rule 7: the password, never to hs ------------------------------------------

#[tokio::test]
async fn rule7_after_a_used_code_the_password_goes_to_account_provider() {
    let harness = Harness::new();
    harness
        .server
        .login(SMITH, Reply::Json(403, json!({ "errcode": "M_FORBIDDEN" })))
        .well_known("smith.ie", SMITH)
        .homeserver(SMITH)
        .login(SMITH, Reply::Json(200, credentials("@kid:smith.ie")));
    let mut flow = harness.link(&link_query(
        "smith.ie",
        Some("@kid:smith.ie"),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let form = password_form(&step).clone();
    assert_eq!(form.server, "smith.ie");
    flow.submit(&form.server, &form.username);
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
    let seen: Vec<_> = harness.server.seen().into_iter().map(|s| s.url).collect();
    assert_eq!(
        seen,
        [
            format!("{SMITH}/_matrix/client/v3/login"),
            "https://smith.ie/.well-known/matrix/client".to_owned(),
            format!("{SMITH}/_matrix/client/versions"),
            format!("{SMITH}/_matrix/client/v3/login"),
        ]
    );
    let body: Value =
        serde_json::from_str(harness.server.posts()[1].body.as_deref().expect("body"))
            .expect("JSON");
    assert_eq!(body["type"], "m.login.password");
    assert_eq!(body["identifier"]["user"], "kid");
    assert_eq!(body["password"], PASSWORD);
    assert_eq!(body["initial_device_display_name"], DEVICE);
    assert_eq!(
        harness.accounts.added(),
        [(
            "@kid:smith.ie".to_owned(),
            SMITH.to_owned(),
            AuthKind::Password
        )]
    ); // The typed password goes with the credentials, once, for the
    // cross-signing set-up's user-interactive auth (ADR 0009).
    assert_eq!(
        *harness.accounts.passwords.lock().expect("lock"),
        [Some(true)]
    );
}

#[tokio::test]
async fn rule7_a_crafted_link_never_collects_a_password_for_hs() {
    // account_provider names a server outside the allowlist, hs a real
    // family; the code fails, and the password form must not send the
    // password to either.
    let harness = Harness::new();
    let victim = "https://victim.safechat.family";
    harness
        .server
        .login(
            victim,
            Reply::Json(403, json!({ "errcode": "M_FORBIDDEN" })),
        )
        .well_known("evil.example", "https://evil.example")
        .homeserver("https://evil.example")
        .login(
            "https://evil.example",
            Reply::Json(200, credentials("@ana:evil.example")),
        );
    let mut flow = harness.link(&link_query(
        "evil.example",
        Some("@ana:evil.example"),
        Some("victim.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let form = password_form(&step).clone();
    assert_eq!(form.server, "evil.example");
    flow.submit(&form.server, &form.username);
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::NOT_FAMILY_CHAT)
    );
    harness.server.assert_never_sent(PASSWORD);
    let posts = harness.server.posts();
    assert_eq!(posts.len(), 1, "only the code: {posts:?}");
    assert_eq!(posts[0].url, format!("{victim}/_matrix/client/v3/login"));
}

#[tokio::test]
async fn rule7_when_discovery_fails_the_password_still_never_goes_to_hs() {
    let harness = Harness::new();
    harness
        .server
        .login(SMITH, Reply::Json(403, json!({ "errcode": "M_FORBIDDEN" })))
        .on(
            Method::Get,
            "https://smith.ie/.well-known/matrix/client",
            Reply::Unreachable,
        );
    let mut flow = harness.link(&link_query(
        "smith.ie",
        Some("@kid:smith.ie"),
        Some("smith.safechat.family"),
    ));
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    flow.submit(&password_form(&step).server, "kid");
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some("Family Chat couldn't reach smith.ie. Check your connection and try again.")
    );
    harness.server.assert_never_sent(PASSWORD);
    assert_eq!(harness.server.posts().len(), 1);
}

// --- The allowlist on the resolved URL ------------------------------------------

#[tokio::test]
async fn allowlist_a_custom_domain_is_allowed_by_where_it_resolves() {
    let harness = Harness::new();
    harness
        .server
        .well_known("smith.ie", SMITH)
        .homeserver(SMITH)
        .login(SMITH, Reply::Json(200, credentials("@kid:smith.ie")));
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step).server_placeholder,
        wording::FAMILY_SERVER_PLACEHOLDER
    );
    flow.submit("  smith.ie  ", "kid");
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
}

#[tokio::test]
async fn allowlist_a_family_address_with_no_well_known_is_its_own_homeserver() {
    let harness = Harness::new();
    harness
        .server
        .homeserver(SMITH)
        .login(SMITH, Reply::Json(200, credentials(ANA)));
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("https://smith.safechat.family/", "ana");
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
}

#[tokio::test]
async fn allowlist_nothing_but_the_well_known_goes_to_a_server_outside_it() {
    for (name, well_known) in [
        (
            "evil.example",
            Some(Reply::Json(
                200,
                json!({ "m.homeserver": { "base_url": "https://evil.example" } }),
            )),
        ),
        (
            "smith.ie",
            Some(Reply::Json(
                200,
                json!({ "m.homeserver": { "base_url": "https://evil.example" } }),
            )),
        ),
        (
            "smith.ie",
            Some(Reply::Json(
                200,
                json!({ "m.homeserver": { "base_url": "http://smith.safechat.family" } }),
            )),
        ),
        (
            "smith.ie",
            Some(Reply::Json(
                200,
                json!({ "m.homeserver": { "base_url": "https://evil.example\\@smith.safechat.family" } }),
            )),
        ),
        (
            "smith.ie",
            Some(Reply::Json(
                200,
                json!({ "m.homeserver": { "base_url": "https://safechat.family" } }),
            )),
        ),
        // The apex: its well-known is the website's HTML, so it would be its
        // own homeserver, which the allowlist refuses.
        (
            "safechat.family",
            Some(Reply::Text(
                200,
                "<!doctype html><title>Family Chat</title>",
            )),
        ),
        // A redirect is not followed; the name is then its own homeserver.
        ("smith.ie", Some(Reply::Json(302, json!({})))),
    ] {
        let harness = Harness::new();
        if let Some(reply) = well_known {
            harness.server.on(
                Method::Get,
                &format!("https://{name}/.well-known/matrix/client"),
                reply,
            );
        }
        let mut flow = harness.start(SigninStart::FamilyChatPassword);
        flow.settled().await;
        flow.submit(name, "ana");
        let step = flow.settled().await;
        assert_eq!(
            password_form(&step)
                .problem
                .as_ref()
                .map(|p| p.message.as_str()),
            Some(wording::NOT_FAMILY_CHAT),
            "{name}"
        );
        let seen = harness.server.seen();
        assert_eq!(seen.len(), 1, "{name}: {seen:?}");
        assert_eq!(
            seen[0].url,
            format!("https://{name}/.well-known/matrix/client")
        );
        harness.server.assert_never_sent(PASSWORD);
    }
}

#[tokio::test]
async fn allowlist_server_names_are_parsed_strictly_before_anything_is_sent() {
    for (server, username, message) in [
        (
            "evil.example\\@smith.safechat.family",
            "ana",
            wording::INVALID_SERVER,
        ),
        ("smith.safechat.family/x", "ana", wording::INVALID_SERVER),
        ("user@smith.safechat.family", "ana", wording::INVALID_SERVER),
        ("smith%2Esafechat.family", "ana", wording::INVALID_SERVER),
        (
            "http://smith.safechat.family",
            "ana",
            wording::INVALID_SERVER,
        ),
        ("", "ana", wording::INVALID_SERVER),
        (
            "",
            "@ana:evil.example\\@smith.safechat.family",
            wording::INVALID_USERNAME,
        ),
        (
            "smith.ie",
            "@ana:smith.safechat.family",
            wording::SERVER_MISMATCH,
        ),
        ("smith.ie", "ana:x", wording::INVALID_USERNAME),
        ("smith.ie", "", wording::NO_USERNAME),
    ] {
        let harness = Harness::new();
        let mut flow = harness.start(SigninStart::FamilyChatPassword);
        flow.settled().await;
        flow.submit(server, username);
        let step = flow.settled().await;
        assert_eq!(
            password_form(&step)
                .problem
                .as_ref()
                .map(|p| p.message.as_str()),
            Some(message),
            "{server} {username}"
        );
        assert!(harness.server.seen().is_empty(), "{server} {username}");
    }
}

#[tokio::test]
async fn allowlist_a_full_matrix_id_names_its_server() {
    let harness = Harness::new();
    harness
        .server
        .well_known("smith.ie", SMITH)
        .homeserver(SMITH)
        .login(SMITH, Reply::Json(200, credentials("@kid:smith.ie")));
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("", "@kid:smith.ie");
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
    let body: Value =
        serde_json::from_str(harness.server.posts()[0].body.as_deref().expect("body"))
            .expect("JSON");
    assert_eq!(body["identifier"]["user"], "@kid:smith.ie");
}

// --- The post-login re-check -----------------------------------------------------

#[tokio::test]
async fn post_login_a_family_chat_sign_in_is_rechecked_against_the_allowlist() {
    // The fake account manager runs the check the flow hands it on the
    // homeserver the session was saved with; a sign-in whose homeserver
    // passed discovery passes it.
    let harness = Harness::new();
    harness
        .server
        .homeserver(SMITH)
        .login(SMITH, Reply::Json(200, credentials(ANA)));
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("smith.safechat.family", "ana");
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
    // And a refusal is shown with its wording; the account manager has
    // signed the device out (tests/mocked_signin.rs), so the flow does not
    // again.
    let harness = Harness::new();
    *lock(&harness.accounts.failure) =
        Some(AddFailure::Refused(wording::NOT_FAMILY_CHAT.to_owned()));
    harness
        .server
        .homeserver(SMITH)
        .login(SMITH, Reply::Json(200, credentials(ANA)));
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("smith.safechat.family", "ana");
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::NOT_FAMILY_CHAT)
    );
    assert_eq!(harness.server.posts().len(), 1);
}

// --- Password sign-in -----------------------------------------------------------

#[tokio::test]
async fn password_errors_are_shown_and_a_rate_limit_is_never_retried() {
    for (reply, message, retry_after) in [
        (
            Reply::Json(403, json!({ "errcode": "M_FORBIDDEN" })),
            wording::WRONG_PASSWORD.to_owned(),
            None,
        ),
        (
            Reply::Json(403, json!({ "errcode": "M_USER_DEACTIVATED" })),
            wording::DEACTIVATED.to_owned(),
            None,
        ),
        (
            Reply::Json(
                429,
                json!({ "errcode": "M_LIMIT_EXCEEDED", "retry_after_ms": 5000 }),
            ),
            "Too many sign-in attempts. Try again in 5 seconds.".to_owned(),
            Some(Duration::from_secs(5)),
        ),
        (
            Reply::Json(400, json!({ "errcode": "M_LIMIT_EXCEEDED" })),
            "Too many sign-in attempts. Try again in a little while.".to_owned(),
            None,
        ),
        (
            Reply::Json(500, json!({})),
            wording::SIGNIN_FAILED.to_owned(),
            None,
        ),
        (
            Reply::Json(308, json!({})),
            wording::SIGNIN_FAILED.to_owned(),
            None,
        ),
    ] {
        let harness = Harness::new();
        harness.server.homeserver(SMITH).login(SMITH, reply);
        let mut flow = harness.start(SigninStart::FamilyChatPassword);
        flow.settled().await;
        flow.submit("smith.safechat.family", "ana");
        let step = flow.settled().await;
        let problem = password_form(&step).problem.clone().expect("a problem");
        assert_eq!(problem.message, message);
        assert_eq!(problem.retry_after, retry_after);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(harness.server.posts().len(), 1, "{message}");
    }
}

#[tokio::test]
async fn other_servers_need_only_https() {
    let harness = Harness::new();
    let base = "https://matrix-client.example.org";
    harness
        .server
        .well_known("example.org", base)
        .homeserver(base)
        .login(base, Reply::Json(200, credentials("@a:example.org")));
    let mut flow = harness.start(SigninStart::OtherServer);
    let step = flow.settled().await;
    assert_eq!(password_form(&step).kind, PasswordKind::OtherServer);
    flow.submit("example.org", "a");
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
    assert_eq!(
        harness.accounts.added(),
        [(
            "@a:example.org".to_owned(),
            base.to_owned(),
            AuthKind::Password
        )]
    );

    let harness = Harness::new();
    harness
        .server
        .well_known("example.org", "http://matrix.example.org");
    let mut flow = harness.start(SigninStart::OtherServer);
    flow.settled().await;
    flow.submit("example.org", "a");
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::NOT_HTTPS)
    );
    assert_eq!(harness.server.seen().len(), 1);
}

#[tokio::test]
async fn a_server_that_is_not_matrix_is_named() {
    let harness = Harness::new();
    harness.server.on(
        Method::Get,
        &format!("{SMITH}/_matrix/client/versions"),
        Reply::Text(200, "<html>"),
    );
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("smith.safechat.family", "ana");
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some("Family Chat couldn't find a Matrix server at smith.safechat.family.")
    );
    harness.server.assert_never_sent(PASSWORD);
}

#[tokio::test]
async fn the_working_steps_are_shown_in_order() {
    let harness = Harness::new();
    let release = Arc::new(Notify::new());
    harness
        .server
        .homeserver(SMITH)
        .login(SMITH, Reply::After(release.clone(), 200, credentials(ANA)));
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("smith.safechat.family", "ana");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if *flow.steps.borrow_and_update() == SigninStep::Working(Working::SigningIn) {
                return;
            }
            flow.steps.changed().await.expect("the flow is running");
        }
    })
    .await
    .expect("signing in");
    release.notify_one();
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
}

// --- Rule 8: tokens never in logs, Debug or panics ------------------------------

#[test]
fn rule8_no_token_in_debug_or_a_panic_message() {
    let allowlist = Allowlist::production();
    let link = familychat_signin::LoginLink::parse(
        &format!(
            "https://safechat.family/app/login?{}",
            link_query(
                "smith.safechat.family",
                Some(ANA),
                Some("smith.safechat.family")
            )
        ),
        &allowlist,
    )
    .expect("parses");
    assert!(link.code.is_some());
    let debug = format!("{link:?} {link:#?}");
    assert!(!debug.contains(TOKEN), "{debug}");

    // A panic that formats the link (an `unwrap` on a `Result` holding it,
    // the way a careless `expect` would) carries no token either.
    let messages = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook_messages = messages.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default();
        lock(&hook_messages).push(message);
    }));
    let result = std::panic::catch_unwind(move || {
        let held: Result<(), familychat_signin::LoginLink> = Err(link);
        #[allow(clippy::unwrap_used, clippy::unnecessary_literal_unwrap)]
        std::hint::black_box(held).unwrap();
    });
    std::panic::set_hook(previous);
    assert!(result.is_err());
    let messages = lock(&messages).clone();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("smith.safechat.family"),
        "{messages:?}"
    );
    assert!(!messages[0].contains(TOKEN), "{messages:?}");
}
