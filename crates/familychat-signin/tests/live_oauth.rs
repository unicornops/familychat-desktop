//! The browser and QR code sign-ins against the live harness (ADR 0010):
//! Synapse with MAS (`synapse-mas.familychat.test`, allowlisted, so the
//! Family Chat rules apply) for OAuth 2.0 and MSC4108, and Family Chat's
//! Palpo, where neither is offered. The person's browser is played by
//! `familychat-test-support`'s `MasBrowser` (MAS's login and consent
//! pages); the other device is a second profile's account, or a bare SDK
//! client, as a phone would be. Start the harness first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-signin --test live_oauth -- --ignored --nocapture
//! scripts/homeserver-harness.sh down
//! ```

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use familychat_core::{
    encryption::{DeviceTrust, IdentitySetup, QrModules},
    session::AccountKey,
    signin::{
        BrowserKind, BrowserPurpose, PasswordForm, QrPurpose, SigninAnswer, SigninApi, SigninFlow,
        SigninStart, SigninStep,
    },
    sync::SyncState,
};
use familychat_security::{AuthKind, Keychain, MemoryKeychain, keychain::load_session};
use familychat_session::{AccountManager, SessionConfig};
use familychat_signin::{
    Allowlist, LiveSignin, ReqwestTransport, Services, device_display_name, methods::probe,
    oauth::client_metadata,
};
use familychat_store::{DataDir, Profile, ProfileState};
use familychat_test_support::{HomeserverHarness, MasUser, edge_client, edge_client_builder};
use futures_util::StreamExt as _;
use matrix_sdk::authentication::oauth::{
    ClientRegistrationData,
    qrcode::{GrantLoginProgress, LoginProgress, QrCodeData},
};
use reqwest::Url;
use tokio::sync::Mutex as AsyncMutex;

const SETTLE: Duration = Duration::from_secs(90);
/// MAS's access tokens in the harness last a minute (its least).
const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(60);
const MAS_ISSUER: &str = "https://mas.familychat.test/";

// --- A profile and its sign-in --------------------------------------------------

struct Desktop {
    root: std::path::PathBuf,
    keychain: Arc<dyn Keychain>,
    manager: Arc<AccountManager>,
    signin: LiveSignin,
}

impl Desktop {
    fn new(harness: &HomeserverHarness, name: &str) -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "familychat-oauth-live-{name}-{}-{}",
            std::process::id(),
            HomeserverHarness::unique_localpart("p")
        ));
        let keychain: Arc<dyn Keychain> = Arc::new(MemoryKeychain::new());
        let profile = match Profile::open(&DataDir::at(&root), keychain.clone())? {
            ProfileState::Ready(profile) => profile,
            other => bail!("the test profile did not open: {other:?}"),
        };
        let manager = Arc::new(AccountManager::new(
            profile,
            SessionConfig::with_http_client(edge_client(harness.env())?),
        ));
        let transport = ReqwestTransport::with_builder(edge_client_builder(harness.env())?)?;
        let signin = LiveSignin::with_services(
            Services {
                transport: Arc::new(transport),
                accounts: manager.clone(),
                allowlist: Allowlist::for_build(),
                gate: Arc::new(AsyncMutex::new(())),
                device_name: device_display_name().to_owned(),
            },
            tokio::runtime::Handle::current(),
        );
        Ok(Self {
            root,
            keychain,
            manager,
            signin,
        })
    }

    fn start(&self, start: SigninStart) -> Driven {
        Driven {
            flow: self.signin.start(start),
        }
    }

    async fn running(&self, key: &AccountKey) -> Result<()> {
        let account = self
            .manager
            .account_by_key(key)
            .context("the new account is open")?;
        let mut status = account.status();
        tokio::time::timeout(SETTLE, async {
            loop {
                if status.borrow_and_update().sync == SyncState::Running {
                    return Ok(());
                }
                status.changed().await.context("the account went away")?;
            }
        })
        .await
        .context("the new account never synced")?
    }

    /// Waits until the account has its cross-signing identity and trusts
    /// this device (the session makes it after a sign-in, ADR 0009).
    async fn identity_ready(&self, key: &AccountKey) -> Result<()> {
        let account = self.manager.account_by_key(key).context("open")?;
        let mut status = account.encryption();
        tokio::time::timeout(SETTLE, async {
            loop {
                let now = status.borrow_and_update().clone();
                if now.identity == IdentitySetup::Ready && now.device == DeviceTrust::Verified {
                    return Ok(());
                }
                if matches!(
                    now.identity,
                    IdentitySetup::NeedsPassword | IdentitySetup::Failed(_)
                ) {
                    bail!("the identity was not made: {now:?}");
                }
                status.changed().await.context("the account went away")?;
            }
        })
        .await
        .context("the identity was never ready")?
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Driven {
    flow: SigninFlow,
}

impl Driven {
    async fn settled(&self) -> Result<SigninStep> {
        let mut steps = self.flow.steps();
        tokio::time::timeout(SETTLE, async {
            loop {
                let step = steps.borrow_and_update().clone();
                if !matches!(step, SigninStep::Starting | SigninStep::Working(_)) {
                    return Ok(step);
                }
                steps.changed().await.context("the flow ended")?;
            }
        })
        .await
        .with_context(|| format!("the flow did not settle: {:?}", self.flow.step()))?
    }

    /// Waits for a step that `wanted` accepts (later steps may replace
    /// earlier ones before they are seen, so this waits for a state).
    async fn until(&self, what: &str, wanted: impl Fn(&SigninStep) -> bool) -> Result<SigninStep> {
        let mut steps = self.flow.steps();
        tokio::time::timeout(SETTLE, async {
            loop {
                let step = steps.borrow_and_update().clone();
                if wanted(&step) {
                    return Ok(step);
                }
                if step.is_final() {
                    bail!("the flow ended before {what}: {step:?}");
                }
                steps.changed().await.context("the flow ended")?;
            }
        })
        .await
        .with_context(|| format!("never {what}: {:?}", self.flow.step()))?
    }

    fn answer(&self, answer: SigninAnswer) -> Result<()> {
        ensure!(self.flow.answer(answer), "the flow has ended");
        Ok(())
    }
}

fn form(step: &SigninStep) -> Result<&PasswordForm> {
    match step {
        SigninStep::Password(form) => Ok(form),
        other => bail!("not the form: {other:?}"),
    }
}

/// "Use another Matrix server" for `server`, to the form with its ways.
async fn choose(desktop: &Desktop, server: &str) -> Result<(Driven, PasswordForm)> {
    let flow = desktop.start(SigninStart::OtherServer);
    let step = flow.settled().await?;
    ensure!(matches!(step, SigninStep::Server(_)), "{step:?}");
    flow.answer(SigninAnswer::ChooseServer {
        server: server.to_owned(),
    })?;
    let step = flow
        .until("the server's ways", |step| {
            matches!(step, SigninStep::Password(_))
        })
        .await?;
    let form = form(&step)?.clone();
    Ok((flow, form))
}

/// Signs `user` in on `desktop` with OAuth in the (scripted) browser.
async fn oauth_sign_in(
    harness: &HomeserverHarness,
    desktop: &Desktop,
    user: &MasUser,
) -> Result<AccountKey> {
    let (flow, form) = choose(desktop, harness.synapse_mas_server_name()?).await?;
    let methods = form.methods.context("the server's ways")?;
    let browser = methods.browser.context("the browser is offered")?;
    ensure!(browser.kind == BrowserKind::OAuth && browser.address == MAS_ISSUER);
    flow.answer(SigninAnswer::UseBrowser)?;
    match flow
        .until("the browser question", |step| {
            matches!(step, SigninStep::ConfirmBrowser(_))
        })
        .await?
    {
        SigninStep::ConfirmBrowser(confirm) => {
            ensure!(confirm.address == MAS_ISSUER, "{confirm:?}");
            ensure!(confirm.purpose == BrowserPurpose::SignIn);
        }
        other => bail!("not the browser question: {other:?}"),
    }
    flow.answer(SigninAnswer::Continue)?;
    let SigninStep::Browser(step) = flow
        .until("the browser", |step| matches!(step, SigninStep::Browser(_)))
        .await?
    else {
        unreachable!()
    };
    let mut browser = harness.browser()?;
    let redirect = browser.sign_in_at(step.url.expose(), user).await?;
    ensure!(browser.come_back(&redirect).await? == 200, "the app's page");
    match flow
        .until("the end", |step| {
            step.is_final() || matches!(step, SigninStep::Password(_))
        })
        .await?
    {
        SigninStep::Finished(finished) => {
            ensure!(finished.user_id.as_str() == user.user_id, "{finished:?}");
            desktop.running(&finished.account).await?;
            Ok(finished.account)
        }
        other => bail!("the OAuth sign-in did not finish: {other:?}"),
    }
}

/// The bytes a drawn QR code holds, read back as a phone's camera would.
fn scan(qr: &QrModules) -> Result<QrCodeData> {
    const SCALE: usize = 4;
    const QUIET: usize = 4;
    let side = (qr.width + 2 * QUIET) * SCALE;
    let mut image = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| {
        let (x, y) = (x / SCALE, y / SCALE);
        let dark = x >= QUIET && y >= QUIET && qr.is_dark(x - QUIET, y - QUIET);
        if dark { 0 } else { 255 }
    });
    let grids = image.detect_grids();
    let grid = grids.first().context("no QR code in the drawing")?;
    let mut bytes = Vec::new();
    grid.decode_to(&mut bytes)
        .map_err(|error| anyhow::anyhow!("the QR code did not decode: {error:?}"))?;
    QrCodeData::from_bytes(&bytes).context("the QR code is not a sign-in code")
}

fn registration() -> Result<ClientRegistrationData> {
    let metadata = serde_json::value::to_raw_value(&client_metadata(None))?;
    Ok(ClientRegistrationData::new(
        matrix_sdk::ruma::serde::Raw::from_json(metadata),
    ))
}

// --- The tests --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_browser_and_qr_codes_are_offered_only_where_they_work() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let transport = ReqwestTransport::with_builder(edge_client_builder(harness.env())?)?;
    let allowlist = Allowlist::for_build();
    for (base, family) in [
        (harness.hs1_url().to_owned(), true),
        (harness.synapse_url().to_owned(), false),
        (harness.synapse_mas_url().to_owned(), true),
    ] {
        let url = Url::parse(&base)?;
        let homeserver = match allowlist.check(&url) {
            Ok(checked) => checked,
            Err(_) => familychat_signin::CheckedHomeserver::other_server(&url)
                .map_err(|refusal| anyhow::anyhow!("{refusal:?}"))?,
        };
        ensure!(homeserver.is_family_chat() == family, "{base}");
        let offer = probe(&transport, &homeserver, &allowlist).await;
        eprintln!("{base}: {:?}", offer.methods);
        if base == harness.synapse_mas_url() {
            let browser = offer.methods.browser.context("MAS's browser sign-in")?;
            ensure!(browser.kind == BrowserKind::OAuth && browser.address == MAS_ISSUER);
            ensure!(offer.methods.qr_code, "MSC4108 on Synapse with MAS");
            ensure!(
                !offer.methods.password,
                "the password stays hidden with OAuth"
            );
        } else {
            // Family Chat's Palpo today (PLAN.md section 6.10), and
            // Synapse without MAS or single sign-on: the password only.
            ensure!(offer.methods.password, "{base}");
            ensure!(
                offer.methods.browser.is_none(),
                "{base}: {:?}",
                offer.methods
            );
            ensure!(!offer.methods.qr_code, "{base}: {:?}", offer.methods);
        }
    }

    // As the window and the terminal see it, for the family's Palpo.
    let desktop = Desktop::new(&harness, "palpo")?;
    let (flow, form) = choose(&desktop, &harness.env().server_name).await?;
    let methods = form.methods.context("the server's ways")?;
    ensure!(methods.password && methods.browser.is_none() && !methods.qr_code);
    flow.answer(SigninAnswer::Cancel)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn an_oauth_sign_in_keeps_its_tokens_refreshed_and_its_sign_out_revokes() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let user = harness.new_mas_user("oauth").await?;
    let desktop = Desktop::new(&harness, "oauth")?;
    let key = oauth_sign_in(&harness, &desktop, &user).await?;
    let account = desktop.manager.account_by_key(&key).context("open")?;
    let first = load_session(desktop.keychain.as_ref(), account.id())?.context("kept")?;
    ensure!(first.auth_kind == AuthKind::OAuth, "{:?}", first.auth_kind);
    ensure!(first.oauth_client_id.is_some(), "the client id is kept");
    ensure!(first.refresh_token.is_some(), "MAS issued a refresh token");
    ensure!(first.homeserver_url == harness.synapse_mas_url());

    // MAS's access token expires; the account keeps working, and the new
    // tokens are in the keychain.
    eprintln!("waiting {ACCESS_TOKEN_TTL:?} for the access token to expire");
    tokio::time::sleep(ACCESS_TOKEN_TTL + Duration::from_secs(5)).await;
    let client = account.client().context("the client")?.clone();
    let whoami = client.whoami().await.context("whoami after the expiry")?;
    ensure!(whoami.user_id.as_str() == user.user_id);
    let renewed = load_session(desktop.keychain.as_ref(), account.id())?.context("kept")?;
    ensure!(
        renewed.access_token.expose() != first.access_token.expose(),
        "the access token was refreshed"
    );
    ensure!(
        renewed.refresh_token.as_ref().map(|token| token.expose())
            != first.refresh_token.as_ref().map(|token| token.expose()),
        "the rotated refresh token was kept"
    );
    let tokens = client.session_tokens().context("tokens")?;
    ensure!(&tokens.access_token == renewed.access_token.expose());
    desktop.running(&key).await?;

    // Signing out revokes the session at MAS: its token stops working
    // (once Synapse has dropped what it cached of the token).
    client.logout().await.context("the OAuth sign-out")?;
    let whoami = HomeserverHarness::url(
        harness.synapse_mas_url(),
        &["_matrix", "client", "v3", "account", "whoami"],
    )?;
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let reply = harness
            .client()
            .get(whoami.clone())
            .bearer_auth(renewed.access_token.expose())
            .send()
            .await?;
        if reply.status() == 401 {
            break;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the revoked token still answered {}",
            reply.status()
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn this_computer_signs_in_with_a_qr_code_a_signed_in_device_scans() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let user = harness.new_mas_user("qr-in").await?;
    // The phone: signed in already, with the account's identity.
    let phone = Desktop::new(&harness, "qr-phone")?;
    let phone_key = oauth_sign_in(&harness, &phone, &user).await?;
    phone.identity_ready(&phone_key).await?;
    let phone_client = phone
        .manager
        .account_by_key(&phone_key)
        .and_then(|account| account.client().cloned())
        .context("the phone's client")?;

    let desktop = Desktop::new(&harness, "qr-desktop")?;
    let (flow, form) = choose(&desktop, harness.synapse_mas_server_name()?).await?;
    ensure!(form.methods.as_ref().is_some_and(|methods| methods.qr_code));
    flow.answer(SigninAnswer::UseQrCode)?;
    let SigninStep::QrCode(step) = flow
        .until("the QR code", |step| matches!(step, SigninStep::QrCode(_)))
        .await?
    else {
        unreachable!()
    };
    ensure!(step.purpose == QrPurpose::SignIn);
    let data = scan(&step.qr)?;

    // The phone grants it, as Element X's "link a new device" does.
    let (codes, mut check_codes) = tokio::sync::mpsc::unbounded_channel();
    let (uris, mut approvals) = tokio::sync::mpsc::unbounded_channel();
    let grant = tokio::spawn(async move {
        let oauth = phone_client.oauth();
        let grant = oauth.grant_login_with_qr_code().scan(&data);
        let mut progress = grant.subscribe_to_progress();
        let watcher = tokio::spawn(async move {
            while let Some(step) = progress.next().await {
                match step {
                    GrantLoginProgress::EstablishingSecureChannel(code) => {
                        let _ = codes.send(code.check_code);
                    }
                    GrantLoginProgress::WaitingForAuth {
                        verification_uri,
                        continuation_sender,
                    } => {
                        let _ = continuation_sender.confirm().await;
                        let _ = uris.send(verification_uri.to_string());
                    }
                    _ => {}
                }
            }
        });
        let result = grant.await.map_err(|error| error.to_string());
        watcher.abort();
        result
    });

    // The phone shows the check code; the person types it here.
    let code = tokio::time::timeout(SETTLE, check_codes.recv())
        .await
        .context("the phone never showed a check code")?
        .context("the phone stopped")?;
    flow.until("the check code", |step| {
        matches!(step, SigninStep::CheckCode(_))
    })
    .await?;
    flow.answer(SigninAnswer::SubmitCheckCode(format!("{code:02}")))?;
    // The person approves the new sign-in in the phone's browser.
    let approval = tokio::time::timeout(SETTLE, approvals.recv())
        .await
        .context("the phone never asked for the approval")?
        .context("the phone stopped")?;
    harness.browser()?.approve_device(&approval, &user).await?;

    let step = flow
        .until("the end", |step| {
            step.is_final() || matches!(step, SigninStep::Password(_))
        })
        .await?;
    let SigninStep::Finished(finished) = step else {
        bail!("the QR sign-in did not finish: {step:?}");
    };
    ensure!(finished.user_id.as_str() == user.user_id);
    tokio::time::timeout(SETTLE, grant)
        .await
        .context("the phone's grant never ended")??
        .map_err(|error| anyhow::anyhow!("the phone's grant failed: {error}"))?;
    desktop.running(&finished.account).await?;
    // The phone handed over the account's keys: this computer is
    // confirmed without asking.
    desktop.identity_ready(&finished.account).await?;
    let account = desktop
        .manager
        .account_by_key(&finished.account)
        .context("open")?;
    let kept = load_session(desktop.keychain.as_ref(), account.id())?.context("kept")?;
    ensure!(kept.auth_kind == AuthKind::QrCode && kept.oauth_client_id.is_some());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_signed_in_computer_signs_a_new_phone_in_with_its_qr_code() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let user = harness.new_mas_user("qr-out").await?;
    let desktop = Desktop::new(&harness, "link-desktop")?;
    let key = oauth_sign_in(&harness, &desktop, &user).await?;
    desktop.identity_ready(&key).await?;

    let flow = desktop.start(SigninStart::LinkDevice(key.clone()));
    let SigninStep::QrCode(step) = flow
        .until("the QR code", |step| matches!(step, SigninStep::QrCode(_)))
        .await?
    else {
        unreachable!()
    };
    ensure!(step.purpose == QrPurpose::LinkDevice);
    let data = scan(&step.qr)?;

    // The new phone: a bare SDK client that scans the code.
    let store = std::env::temp_dir().join(format!(
        "familychat-oauth-live-phone-{}",
        HomeserverHarness::unique_localpart("p")
    ));
    let phone = matrix_sdk::Client::builder()
        .homeserver_url(harness.synapse_mas_url())
        .http_client(edge_client(harness.env())?)
        .sqlite_store(&store, None)
        .handle_refresh_tokens()
        .build()
        .await?;
    let registration = registration()?;
    let (codes, mut check_codes) = tokio::sync::mpsc::unbounded_channel();
    let login_phone = phone.clone();
    let login = tokio::spawn(async move {
        let oauth = login_phone.oauth();
        let login = oauth.login_with_qr_code(Some(&registration)).scan(&data);
        let mut progress = login.subscribe_to_progress();
        let watcher = tokio::spawn(async move {
            while let Some(step) = progress.next().await {
                if let LoginProgress::EstablishingSecureChannel(code) = step {
                    let _ = codes.send(code.check_code);
                }
            }
        });
        let result = login.await.map_err(|error| error.to_string());
        watcher.abort();
        result
    });

    let code = tokio::time::timeout(SETTLE, check_codes.recv())
        .await
        .context("the phone never showed a check code")?
        .context("the phone stopped")?;
    flow.until("the check code", |step| {
        matches!(step, SigninStep::CheckCode(_))
    })
    .await?;
    flow.answer(SigninAnswer::SubmitCheckCode(format!("{code:02}")))?;
    match flow
        .until("the approval question", |step| {
            matches!(step, SigninStep::ConfirmBrowser(_))
        })
        .await?
    {
        SigninStep::ConfirmBrowser(confirm) => {
            ensure!(confirm.purpose == BrowserPurpose::ApproveDevice);
            ensure!(confirm.address == MAS_ISSUER, "{confirm:?}");
        }
        _ => unreachable!(),
    }
    flow.answer(SigninAnswer::Continue)?;
    let SigninStep::Browser(browser) = flow
        .until("the browser", |step| matches!(step, SigninStep::Browser(_)))
        .await?
    else {
        unreachable!()
    };
    ensure!(browser.url.expose().starts_with(MAS_ISSUER));
    harness
        .browser()?
        .approve_device(browser.url.expose(), &user)
        .await?;

    let end = flow.until("the end", SigninStep::is_final).await?;
    ensure!(matches!(end, SigninStep::DeviceLinked(_)), "{end:?}");
    tokio::time::timeout(SETTLE, login)
        .await
        .context("the phone's sign-in never ended")??
        .map_err(|error| anyhow::anyhow!("the phone's sign-in failed: {error}"))?;
    ensure!(phone.user_id().map(ToString::to_string).as_deref() == Some(user.user_id.as_str()));
    let status = phone
        .encryption()
        .cross_signing_status()
        .await
        .context("the phone's cross-signing")?;
    ensure!(
        status.is_complete(),
        "the phone got the account's keys: {status:?}"
    );
    let _ = std::fs::remove_dir_all(&store);
    Ok(())
}
