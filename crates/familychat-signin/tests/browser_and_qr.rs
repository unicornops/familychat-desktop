//! The browser and QR code sign-ins offline (ADR 0010): a scripted
//! homeserver and authorization server, the test as the browser (it
//! follows the address the flow hands the frontend to the loopback
//! redirect), and a fake account manager whose QR sign-ins check the
//! guard and the post-login check the flow hands them.

mod common;

use common::*;
use familychat_core::signin::{
    BrowserKind, BrowserPurpose, BrowserStep, PasswordForm, QrPurpose, SigninAnswer, SigninStart,
    SigninStep, Working,
};
use familychat_security::AuthKind;
use familychat_signin::{http::Method, methods::MSC4108_FEATURE, oauth::CLIENT_NAME, wording};
use reqwest::Url;
use serde_json::{Value, json};

const ISSUER: &str = "https://auth.smith.safechat.family/";

/// An authorization server's metadata at `issuer`.
fn metadata(issuer: &str) -> Value {
    json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}authorize"),
        "token_endpoint": format!("{issuer}oauth2/token"),
        "registration_endpoint": format!("{issuer}oauth2/registration"),
        "revocation_endpoint": format!("{issuer}oauth2/revoke"),
        "device_authorization_endpoint": format!("{issuer}oauth2/device"),
        "grant_types_supported": ["authorization_code", "refresh_token", "urn:ietf:params:oauth:grant-type:device_code"],
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["plain", "S256"],
    })
}

/// A Family Chat homeserver at `base` with an authorization server at
/// `issuer` (MAS, as family-chat #239 plans) and the MSC4108 rendezvous.
fn oauth_homeserver(harness: &Harness, base: &str, issuer: &str) {
    harness
        .server
        // Discovery asks for /versions, and the probe again.
        .on(
            Method::Get,
            &format!("{base}/_matrix/client/versions"),
            Reply::Json(
                200,
                json!({ "versions": ["v1.12"], "unstable_features": { MSC4108_FEATURE: true } }),
            ),
        )
        .on(
            Method::Get,
            &format!("{base}/_matrix/client/versions"),
            Reply::Json(
                200,
                json!({ "versions": ["v1.12"], "unstable_features": { MSC4108_FEATURE: true } }),
            ),
        )
        .on(
            Method::Get,
            &format!("{base}/_matrix/client/v3/login"),
            Reply::Json(
                200,
                json!({ "flows": [{ "type": "m.login.password" }, { "type": "m.login.sso", "oauth_aware_preferred": true }, { "type": "m.login.token" }] }),
            ),
        )
        .on(
            Method::Get,
            &format!("{base}/_matrix/client/v1/auth_metadata"),
            Reply::Json(200, metadata(issuer)),
        );
}

async fn methods_form(flow: &mut Flow, server: &str) -> PasswordForm {
    let step = choose_server(flow, server).await;
    password_form(&step).clone()
}

fn browser_step(step: &SigninStep) -> &BrowserStep {
    match step {
        SigninStep::Browser(browser) => browser,
        other => panic!("not the browser step: {other:?}"),
    }
}

fn query(url: &Url, name: &str) -> String {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| panic!("no {name} in the address"))
}

/// The browser coming back to the loopback redirect with `query`.
async fn come_back(redirect: &str, query: &str) -> u16 {
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a client");
    client
        .get(format!("{redirect}?{query}"))
        .send()
        .await
        .expect("the loopback answers")
        .status()
        .as_u16()
}

fn posts_to(harness: &Harness, url: &str) -> Vec<Seen> {
    harness
        .server
        .posts()
        .into_iter()
        .filter(|seen| seen.url == url)
        .collect()
}

#[tokio::test]
async fn palpo_offers_the_password_only() {
    // Family Chat's Palpo today: password and token logins, no OAuth
    // server, the rendezvous turned off (PLAN.md section 6.10).
    let harness = Harness::new();
    harness
        .server
        .homeserver(SMITH)
        .on(
            Method::Get,
            &format!("{SMITH}/_matrix/client/v3/login"),
            Reply::Json(
                200,
                json!({ "flows": [{ "type": "m.login.password" }, { "type": "m.login.token" }] }),
            ),
        )
        .on(
            Method::Get,
            &format!("{SMITH}/_matrix/client/versions"),
            Reply::Json(
                200,
                json!({ "versions": ["v1.12"], "unstable_features": { MSC4108_FEATURE: false } }),
            ),
        );
    let mut flow = harness.start(SigninStart::OtherServer);
    let form = methods_form(&mut flow, "smith.safechat.family").await;
    let methods = form.methods.expect("the server's methods");
    assert!(methods.password);
    assert_eq!(methods.browser, None);
    assert!(!methods.qr_code);
    // Asking for what is not offered changes nothing.
    flow.answer(SigninAnswer::UseBrowser);
    flow.answer(SigninAnswer::UseQrCode);
    flow.answer(SigninAnswer::Cancel);
    assert_eq!(flow.ended().await, SigninStep::Cancelled);
    assert!(harness.server.posts().is_empty());
}

#[tokio::test]
async fn an_oauth_sign_in_opens_the_browser_only_after_continue_and_keeps_its_client() {
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    harness
        .server
        .on(
            Method::Post,
            &format!("{ISSUER}oauth2/registration"),
            Reply::Json(201, json!({ "client_id": "01CLIENT" })),
        )
        .on(
            Method::Post,
            &format!("{ISSUER}oauth2/token"),
            Reply::Json(
                200,
                json!({ "access_token": ACCESS, "refresh_token": "mar_refresh", "token_type": "Bearer", "expires_in": 300 }),
            ),
        );
    let mut flow = harness.start(SigninStart::OtherServer);
    let form = methods_form(&mut flow, "smith.safechat.family").await;
    let methods = form.methods.clone().expect("the server's methods");
    // With an OAuth server, the browser is the way (its compatibility
    // password login stays hidden) and the QR code is offered.
    assert!(!methods.password && !form.shows_password());
    let browser = methods.browser.expect("the browser");
    assert_eq!(browser.kind, BrowserKind::OAuth);
    assert_eq!(browser.address, ISSUER);
    assert!(methods.qr_code);

    flow.answer(SigninAnswer::UseBrowser);
    match flow.settled().await {
        SigninStep::ConfirmBrowser(confirm) => {
            assert_eq!(confirm.address, ISSUER);
            assert_eq!(confirm.question, wording::CONFIRM_BROWSER);
            assert_eq!(confirm.purpose, BrowserPurpose::SignIn);
        }
        other => panic!("{other:?}"),
    }
    // Nothing has been sent to the authorization server yet.
    assert!(harness.server.posts().is_empty());
    assert!(
        harness
            .server
            .seen()
            .iter()
            .all(|seen| !seen.url.starts_with(ISSUER)),
        "{:?}",
        harness.server.seen()
    );

    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let browser = browser_step(&step).clone();
    assert_eq!(browser.address, ISSUER);
    assert!(!format!("{step:?}").contains("state="));
    let url = Url::parse(browser.url.expose()).expect("an address");
    assert!(url.as_str().starts_with(&format!("{ISSUER}authorize?")));
    assert_eq!(query(&url, "client_id"), "01CLIENT");
    assert_eq!(query(&url, "code_challenge_method"), "S256");
    let redirect = query(&url, "redirect_uri");
    assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
    let registration = posts_to(&harness, &format!("{ISSUER}oauth2/registration"));
    assert_eq!(registration.len(), 1);
    let registered: Value =
        serde_json::from_str(registration[0].body.as_deref().expect("a body")).expect("JSON");
    assert_eq!(registered["client_name"], CLIENT_NAME);
    assert_eq!(registered["redirect_uris"], json!([redirect]));

    // The device the scope names is the one whoami must report.
    let scope = query(&url, "scope");
    let device = scope
        .rsplit("device:")
        .next()
        .expect("a device scope")
        .to_owned();
    harness.server.on(
        Method::Get,
        &format!("{SMITH}/_matrix/client/v3/account/whoami"),
        Reply::Json(200, json!({ "user_id": ANA, "device_id": device })),
    );
    // Another program on this computer without the state changes nothing.
    assert_eq!(come_back(&redirect, "code=stolen&state=guess").await, 400);
    let state = query(&url, "state");
    assert_eq!(
        come_back(&redirect, &format!("code=the-code&state={state}")).await,
        200
    );
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));

    let token = posts_to(&harness, &format!("{ISSUER}oauth2/token"));
    assert_eq!(token.len(), 1, "the code is exchanged once");
    let form_body = token[0].body.as_deref().expect("a body");
    assert!(form_body.contains("grant_type=authorization_code"));
    assert!(form_body.contains("code=the-code"));
    assert!(form_body.contains("code_verifier="));
    assert!(!form_body.contains("stolen"));
    assert_eq!(
        harness.accounts.added(),
        [(ANA.to_owned(), SMITH.to_owned(), AuthKind::OAuth)]
    );
    assert_eq!(
        *lock(&harness.accounts.client_ids),
        [Some("01CLIENT".to_owned())]
    );
    harness.server.assert_never_sent("stolen");
}

#[tokio::test]
async fn a_family_chat_homeservers_issuer_outside_the_allowlist_is_never_offered() {
    for issuer in [
        "https://account.matrix.org/",
        "https://safechat.family/",
        "http://auth.smith.safechat.family/",
    ] {
        let harness = Harness::new();
        oauth_homeserver(&harness, SMITH, issuer);
        let mut flow = harness.start(SigninStart::OtherServer);
        let form = methods_form(&mut flow, "smith.safechat.family").await;
        let methods = form.methods.expect("methods");
        assert_eq!(methods.browser, None, "{issuer}");
        assert!(!methods.qr_code, "{issuer}");
        // The server's password login is offered again instead.
        assert!(methods.password, "{issuer}");
        assert!(
            harness
                .server
                .seen()
                .iter()
                .all(|seen| seen.url.starts_with(SMITH)),
            "{issuer}: {:?}",
            harness.server.seen()
        );
    }
    // Another server's issuer may be anywhere over HTTPS.
    let harness = Harness::new();
    let base = "https://matrix-client.example.org";
    harness.server.well_known("example.org", base);
    oauth_homeserver(&harness, base, "https://account.example.org/");
    let mut flow = harness.start(SigninStart::OtherServer);
    let form = methods_form(&mut flow, "example.org").await;
    assert_eq!(
        form.methods
            .and_then(|methods| methods.browser)
            .map(|browser| browser.address),
        Some("https://account.example.org/".to_owned())
    );
}

#[tokio::test]
async fn cancelling_before_the_browser_sends_nothing_and_a_declined_sign_in_says_so() {
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    for _ in 0..2 {
        harness.server.on(
            Method::Post,
            &format!("{ISSUER}oauth2/registration"),
            Reply::Json(201, json!({ "client_id": "01CLIENT" })),
        );
    }
    let mut flow = harness.start(SigninStart::OtherServer);
    methods_form(&mut flow, "smith.safechat.family").await;
    flow.answer(SigninAnswer::UseBrowser);
    assert!(matches!(
        flow.settled().await,
        SigninStep::ConfirmBrowser(_)
    ));
    flow.answer(SigninAnswer::Cancel);
    let step = flow.settled().await;
    assert_eq!(password_form(&step).problem, None);
    assert!(harness.server.posts().is_empty());

    flow.answer(SigninAnswer::UseBrowser);
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let url = Url::parse(browser_step(&step).url.expose()).expect("an address");
    let redirect = query(&url, "redirect_uri");
    let state = query(&url, "state");
    assert_eq!(
        come_back(&redirect, &format!("error=access_denied&state={state}")).await,
        200
    );
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::BROWSER_DECLINED)
    );
    assert!(posts_to(&harness, &format!("{ISSUER}oauth2/token")).is_empty());

    // Cancel while the browser is open goes back to the form too.
    flow.answer(SigninAnswer::UseBrowser);
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    assert!(matches!(flow.settled().await, SigninStep::Browser(_)));
    flow.answer(SigninAnswer::Cancel);
    assert!(matches!(flow.settled().await, SigninStep::Password(_)));
}

#[tokio::test]
async fn a_redirect_at_the_authorization_server_is_not_followed() {
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    harness.server.on(
        Method::Post,
        &format!("{ISSUER}oauth2/registration"),
        Reply::Json(307, json!({})),
    );
    let mut flow = harness.start(SigninStart::OtherServer);
    methods_form(&mut flow, "smith.safechat.family").await;
    flow.answer(SigninAnswer::UseBrowser);
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::BROWSER_FAILED)
    );
    assert_eq!(harness.server.posts().len(), 1, "sent once, not followed");
}

#[tokio::test]
async fn single_sign_on_redeems_its_token_at_the_checked_homeserver() {
    let harness = Harness::new();
    let base = "https://matrix-client.example.org";
    harness
        .server
        .well_known("example.org", base)
        .homeserver(base)
        .on(
            Method::Get,
            &format!("{base}/_matrix/client/v3/login"),
            Reply::Json(
                200,
                json!({ "flows": [{ "type": "m.login.sso" }, { "type": "m.login.token" }] }),
            ),
        )
        .login(base, Reply::Json(200, credentials("@a:example.org")));
    let mut flow = harness.start(SigninStart::OtherServer);
    let form = methods_form(&mut flow, "example.org").await;
    let methods = form.methods.expect("methods");
    assert!(!methods.password && !methods.qr_code);
    let browser = methods.browser.expect("single sign-on");
    assert_eq!(browser.kind, BrowserKind::Sso);
    assert_eq!(browser.address, format!("{base}/"));
    flow.answer(SigninAnswer::UseBrowser);
    flow.settled().await;
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let url = Url::parse(browser_step(&step).url.expose()).expect("an address");
    assert!(
        url.as_str()
            .starts_with(&format!("{base}/_matrix/client/v3/login/sso/redirect?"))
    );
    let redirect = Url::parse(&query(&url, "redirectUrl")).expect("a redirect");
    let state = query(&redirect, "state");
    let mut back = redirect.clone();
    back.set_query(None);
    // Without the state (another program), the token is not taken.
    assert_eq!(come_back(back.as_str(), "loginToken=forged").await, 400);
    assert_eq!(
        come_back(
            back.as_str(),
            &format!("state={state}&loginToken=the-sso-token")
        )
        .await,
        200
    );
    assert!(matches!(flow.ended().await, SigninStep::Finished(_)));
    let login = harness.server.posts();
    assert_eq!(login.len(), 1);
    assert_eq!(login[0].url, format!("{base}/_matrix/client/v3/login"));
    let body: Value =
        serde_json::from_str(login[0].body.as_deref().expect("a body")).expect("JSON");
    assert_eq!(body["type"], "m.login.token");
    assert_eq!(body["token"], "the-sso-token");
    harness.server.assert_never_sent("forged");
    assert_eq!(harness.accounts.added()[0].2, AuthKind::Sso);
}

#[tokio::test]
async fn a_qr_code_sign_in_checks_its_addresses_and_the_check_code() {
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    *lock(&harness.accounts.guard_probes) = vec![
        format!("{SMITH}/_synapse/client/rendezvous/01ABC"),
        "https://evil.example/rendezvous".to_owned(),
        "http://smith.safechat.family/rendezvous".to_owned(),
    ];
    let mut flow = harness.start(SigninStart::OtherServer);
    methods_form(&mut flow, "smith.safechat.family").await;
    flow.answer(SigninAnswer::UseQrCode);
    match flow.settled().await {
        SigninStep::QrCode(qr) => {
            assert_eq!(qr.purpose, QrPurpose::SignIn);
            assert_eq!(qr.qr, fake_qr());
        }
        other => panic!("{other:?}"),
    }
    harness.accounts.scan.notify_one();
    assert_eq!(*lock(&harness.accounts.guard_answers), [true, false, false]);
    let (homeserver, metadata) = lock(&harness.accounts.qr_requests)[0].clone();
    assert_eq!(homeserver, format!("{SMITH}/"));
    assert_eq!(metadata["client_name"], CLIENT_NAME);
    assert!(metadata.get("redirect_uris").is_none());

    assert!(matches!(flow.settled().await, SigninStep::CheckCode(_)));
    for typed in ["4", "4a", "123", ""] {
        flow.answer(SigninAnswer::SubmitCheckCode(typed.to_owned()));
        match flow.settled().await {
            SigninStep::CheckCode(form) => assert_eq!(
                form.problem.as_deref(),
                Some(wording::CHECK_CODE_INVALID),
                "{typed}"
            ),
            other => panic!("{typed}: {other:?}"),
        }
    }
    flow.answer(SigninAnswer::SubmitCheckCode(" 42 ".to_owned()));
    match flow.ended().await {
        SigninStep::Finished(finished) => assert_eq!(finished.user_id.as_str(), ANA),
        other => panic!("{other:?}"),
    }
    assert_eq!(harness.accounts.added()[0].2, AuthKind::QrCode);
}

#[tokio::test]
async fn a_qr_code_sign_in_into_another_server_than_the_family_is_refused() {
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    // The other device signs in someone, but the post-login check sees
    // the homeserver: here the fake hands it the family's, so the check
    // passes; an already signed-in account is refused instead.
    lock(&harness.accounts.signed_in).push(parse(ANA));
    let mut flow = harness.start(SigninStart::OtherServer);
    methods_form(&mut flow, "smith.safechat.family").await;
    flow.answer(SigninAnswer::UseQrCode);
    flow.settled().await;
    harness.accounts.scan.notify_one();
    flow.settled().await;
    flow.answer(SigninAnswer::SubmitCheckCode("42".to_owned()));
    assert_eq!(
        flow.ended().await,
        SigninStep::AlreadySignedIn(wording::already_signed_in(&parse(ANA)))
    );

    // A wrong code: back to the form, saying so.
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    let mut flow = harness.start(SigninStart::OtherServer);
    methods_form(&mut flow, "smith.safechat.family").await;
    flow.answer(SigninAnswer::UseQrCode);
    flow.settled().await;
    harness.accounts.scan.notify_one();
    flow.settled().await;
    flow.answer(SigninAnswer::SubmitCheckCode("17".to_owned()));
    let step = flow.settled().await;
    assert_eq!(
        password_form(&step)
            .problem
            .as_ref()
            .map(|p| p.message.as_str()),
        Some(wording::CHECK_CODE_MISMATCH)
    );
    assert!(harness.accounts.added().is_empty());
}

fn parse(user: &str) -> familychat_core::OwnedUserId {
    familychat_signin::link::parse_user_id(user).expect("a user")
}

#[tokio::test]
async fn linking_a_phone_opens_only_the_accounts_own_authorization_server() {
    let harness = Harness::new();
    oauth_homeserver(&harness, SMITH, ISSUER);
    *lock(&harness.accounts.homeserver) = Some(Url::parse(SMITH).expect("url"));
    *lock(&harness.accounts.approval_uri) = Some(format!("{ISSUER}link?code=ABC123"));
    let mut flow = harness.start(SigninStart::LinkDevice(
        familychat_core::session::AccountKey("ana".to_owned()),
    ));
    match flow.settled().await {
        SigninStep::QrCode(qr) => assert_eq!(qr.purpose, QrPurpose::LinkDevice),
        other => panic!("{other:?}"),
    }
    harness.accounts.scan.notify_one();
    assert!(matches!(flow.settled().await, SigninStep::CheckCode(_)));
    flow.answer(SigninAnswer::SubmitCheckCode("42".to_owned()));
    match flow.settled().await {
        SigninStep::ConfirmBrowser(confirm) => {
            assert_eq!(confirm.purpose, BrowserPurpose::ApproveDevice);
            assert_eq!(confirm.address, ISSUER);
        }
        other => panic!("{other:?}"),
    }
    flow.answer(SigninAnswer::Continue);
    let step = flow.settled().await;
    let browser = browser_step(&step);
    assert_eq!(browser.purpose, BrowserPurpose::ApproveDevice);
    assert_eq!(browser.url.expose(), format!("{ISSUER}link?code=ABC123"));
    harness.accounts.scan.notify_one();
    assert_eq!(
        flow.ended().await,
        SigninStep::DeviceLinked(wording::LINKED.to_owned())
    );

    // A new device that names somewhere else is stopped before anything
    // opens.
    for elsewhere in [
        "https://evil.example/link?code=ABC123",
        "https://other.safechat.family/link",
        "http://auth.smith.safechat.family/link",
    ] {
        let harness = Harness::new();
        oauth_homeserver(&harness, SMITH, ISSUER);
        *lock(&harness.accounts.homeserver) = Some(Url::parse(SMITH).expect("url"));
        *lock(&harness.accounts.approval_uri) = Some(elsewhere.to_owned());
        let mut flow = harness.start(SigninStart::LinkDevice(
            familychat_core::session::AccountKey("ana".to_owned()),
        ));
        flow.settled().await;
        harness.accounts.scan.notify_one();
        flow.settled().await;
        flow.answer(SigninAnswer::SubmitCheckCode("42".to_owned()));
        let mut seen_steps = Vec::new();
        let ended = loop {
            let step = flow.settled().await;
            if step.is_final() {
                break step;
            }
            seen_steps.push(step);
        };
        assert_eq!(
            ended,
            SigninStep::Stopped(wording::LINK_WRONG_ADDRESS.to_owned()),
            "{elsewhere}"
        );
        assert!(
            seen_steps.iter().all(|step| !matches!(
                step,
                SigninStep::ConfirmBrowser(_) | SigninStep::Browser(_)
            )),
            "{elsewhere}: {seen_steps:?}"
        );
    }
}

#[tokio::test]
async fn linking_a_phone_where_the_server_cant_says_so() {
    let harness = Harness::new();
    harness.server.homeserver(SMITH);
    *lock(&harness.accounts.homeserver) = Some(Url::parse(SMITH).expect("url"));
    let flow = harness.start(SigninStart::LinkDevice(
        familychat_core::session::AccountKey("ana".to_owned()),
    ));
    assert_eq!(
        flow.ended().await,
        SigninStep::Stopped(wording::LINK_NOT_SUPPORTED.to_owned())
    );
}

#[tokio::test]
async fn a_family_server_that_takes_no_password_offers_the_browser() {
    let harness = Harness::new();
    harness.server.well_known("smith.ie", SMITH).login(
        SMITH,
        Reply::Json(
            400,
            json!({ "errcode": "M_UNKNOWN", "error": "Unknown login type" }),
        ),
    );
    oauth_homeserver(&harness, SMITH, ISSUER);
    let mut flow = harness.start(SigninStart::FamilyChatPassword);
    flow.settled().await;
    flow.submit("smith.ie", "ana");
    let step = flow.settled().await;
    let form = password_form(&step);
    assert_eq!(
        form.problem.as_ref().map(|p| p.message.as_str()),
        Some(wording::USE_BROWSER_INSTEAD)
    );
    let methods = form.methods.clone().expect("methods");
    assert_eq!(
        methods.browser.map(|browser| browser.address),
        Some(ISSUER.to_owned())
    );
    assert!(methods.qr_code);
}

#[tokio::test]
async fn work_in_progress_has_its_sentences() {
    for working in [
        Working::PreparingBrowser,
        Working::PreparingQrCode,
        Working::WaitingForOtherDevice,
        Working::SharingKeys,
    ] {
        assert!(working.message().ends_with('…'), "{working:?}");
    }
}
