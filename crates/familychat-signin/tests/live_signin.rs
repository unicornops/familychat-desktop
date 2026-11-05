//! The sign-in against the live homeserver harness (ADR 0004, ADR 0006):
//! real codes minted on Palpo at Family Chat's image, redeemed through the
//! sign-in's own client, handed to the real account manager, and the
//! front end's access log as the evidence of where each token and password
//! went. Start the harness first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-signin --test live_signin -- --ignored
//! scripts/homeserver-harness.sh down
//! ```
//!
//! The harness has one Palpo, whose server name is `hs1.familychat.test`,
//! so a member's Matrix ID is always on hs1. A custom-domain family's code
//! (whose user would be `@kid:smith.test`) cannot be minted here; the
//! custom domain is proved by password through its well-known, and a code
//! redeemed at hs1 for a link naming `smith.test` is proved to be refused
//! by the user binding. The offline tests (`tests/flows.rs`) cover a
//! custom-domain code end to end against a scripted server.

use std::{
    io::Write,
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use familychat_core::{
    session::AccountKey,
    signin::{
        PasswordForm, PasswordKind, SecretText, SigninAnswer, SigninApi, SigninFlow, SigninStart,
        SigninStep,
    },
    sync::SyncState,
};
use familychat_security::{Keychain, MemoryKeychain};
use familychat_session::{AccountManager, SessionConfig};
use familychat_signin::{
    AccountSink, Allowlist, LiveSignin, ReqwestTransport, Services, device_display_name,
    link::parse_user_id, wording,
};
use familychat_store::{DataDir, Profile, ProfileState};
use familychat_test_support::{HomeserverHarness, edge_client, edge_client_builder};
use reqwest::Method;
use tokio::sync::Mutex as AsyncMutex;

const SETTLE: Duration = Duration::from_secs(60);
const EVIL_HOST: &str = "evil.familychat.example";

// --- Logs ------------------------------------------------------------------------

/// Every log line of this test binary, unredacted, from the first call on:
/// the sign-in and the HTTP stack at TRACE, matrix-sdk at DEBUG.
fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
    static CAPTURED: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    CAPTURED
        .get_or_init(|| {
            let buffer = Arc::new(Mutex::new(Vec::new()));
            let filter: tracing_subscriber::filter::Targets =
                "info,familychat_signin=trace,familychat_session=trace,reqwest=trace,hyper=trace,hyper_util=trace,h2=trace,rustls=trace,matrix_sdk=debug"
                    .parse()
                    .expect("valid directives");
            use tracing_subscriber::layer::SubscriberExt;
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(Captured(buffer.clone()))
                .finish()
                .with(filter);
            tracing::subscriber::set_global_default(subscriber)
                .expect("the test binary's only subscriber");
            buffer
        })
        .clone()
}

fn log_text() -> String {
    let buffer = captured_logs();
    let bytes = buffer.lock().unwrap_or_else(PoisonError::into_inner);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[derive(Clone)]
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

// --- A profile and its sign-in ------------------------------------------------------

/// A throwaway profile with the mock keychain, its account manager on the
/// harness's client, and the sign-in on the harness's builder (with the
/// sign-in's own settings applied on top).
struct Desktop {
    root: std::path::PathBuf,
    manager: Arc<AccountManager>,
    signin: LiveSignin,
}

impl Desktop {
    fn new(harness: &HomeserverHarness, name: &str) -> Result<Self> {
        captured_logs();
        let root = std::env::temp_dir().join(format!(
            "familychat-signin-live-{name}-{}-{}",
            std::process::id(),
            HomeserverHarness::unique_localpart("p")
        ));
        let keychain: Arc<dyn Keychain> = Arc::new(MemoryKeychain::new());
        let profile = match Profile::open(&DataDir::at(&root), keychain)? {
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
            manager,
            signin,
        })
    }

    fn start(&self, start: SigninStart) -> Driven {
        Driven {
            flow: self.signin.start(start),
        }
    }

    fn link(&self, query: &str) -> Driven {
        self.start(SigninStart::Link(SecretText::new(format!(
            "https://safechat.family/app/login?{query}"
        ))))
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
    /// The next step that is not work in progress.
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

    /// Answers, then waits for the step after the work it starts.
    async fn answer(&self, answer: SigninAnswer) -> Result<SigninStep> {
        let mut steps = self.flow.steps();
        steps.borrow_and_update();
        ensure!(self.flow.answer(answer), "the flow has ended");
        steps.changed().await.context("the flow ended")?;
        self.settled().await
    }

    /// Submits the password form, waiting out a rate limit the way a
    /// person would (the sign-in itself never retries).
    async fn submit(&self, server: &str, username: &str, password: &str) -> Result<SigninStep> {
        for _ in 0..6 {
            let step = self
                .answer(SigninAnswer::SubmitPassword {
                    server: server.to_owned(),
                    username: username.to_owned(),
                    password: SecretText::from(password),
                })
                .await?;
            if let SigninStep::Password(PasswordForm {
                problem: Some(problem),
                ..
            }) = &step
                && let Some(wait) = problem.retry_after
            {
                eprintln!("rate limited; waiting {wait:?} as a person would");
                tokio::time::sleep(wait.min(Duration::from_secs(30))).await;
                continue;
            }
            return Ok(step);
        }
        bail!("still rate limited")
    }
}

/// Opens `query`'s link, checks it asks first (`question`, when given),
/// and continues. Palpo has one login bucket for the whole harness, so a
/// redemption the server rate-limits (which the sign-in never retries) is
/// tried again here after the wait, with a new flow from the same link, as
/// a person would; Palpo refuses a rate-limited login before it looks at
/// the token, so the code is still unused.
async fn redeem(
    desktop: &Desktop,
    query: &str,
    question: Option<&str>,
) -> Result<(Driven, SigninStep)> {
    for _ in 0..6 {
        let flow = desktop.link(query);
        match flow.settled().await? {
            SigninStep::Confirm(confirm) => {
                if let Some(question) = question {
                    ensure!(confirm.question == question, "{confirm:?}");
                }
            }
            other => bail!("no confirmation first: {other:?}"),
        }
        let step = flow.answer(SigninAnswer::Continue).await?;
        if let SigninStep::Password(PasswordForm {
            problem: Some(problem),
            ..
        }) = &step
            && let Some(wait) = problem.retry_after
        {
            eprintln!("the code was rate limited; trying again in {wait:?} as a person would");
            flow.flow.answer(SigninAnswer::Cancel);
            tokio::time::sleep(wait.min(Duration::from_secs(30))).await;
            continue;
        }
        return Ok((flow, step));
    }
    bail!("still rate limited")
}

/// Fails if any file under `dir` contains `needle`.
fn assert_not_in_files(dir: &std::path::Path, needle: &str) -> Result<()> {
    let mut pending = vec![dir.to_path_buf()];
    let mut scanned = 0;
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                let bytes = std::fs::read(entry.path())?;
                scanned += bytes.len();
                ensure!(
                    !bytes
                        .windows(needle.len())
                        .any(|window| window == needle.as_bytes()),
                    "{} holds the token",
                    entry.path().display()
                );
            }
        }
    }
    ensure!(scanned > 0, "nothing was scanned under {}", dir.display());
    Ok(())
}

fn form(step: &SigninStep) -> Result<&PasswordForm> {
    match step {
        SigninStep::Password(form) => Ok(form),
        other => bail!("not the password form: {other:?}"),
    }
}

fn problem(step: &SigninStep) -> Result<String> {
    Ok(form(step)?
        .problem
        .as_ref()
        .map(|problem| problem.message.clone())
        .unwrap_or_default())
}

fn finished(step: &SigninStep) -> Result<AccountKey> {
    match step {
        SigninStep::Finished(finished) => Ok(finished.account.clone()),
        other => bail!("the sign-in did not finish: {other:?}"),
    }
}

fn link_query(provider: &str, hint: Option<&str>, code: Option<(&str, &str)>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("account_provider", provider);
    if let Some(hint) = hint {
        query.append_pair("login_hint", &format!("mxid:{hint}"));
    }
    if let Some((hs, token)) = code {
        query.append_pair("hs", hs);
        query.append_pair("token", token);
    }
    query.finish()
}

fn host(url: &str) -> Result<&str> {
    url.strip_prefix("https://").context("an https:// URL")
}

/// A member created through the admin API (no login, so Palpo's one login
/// bucket for the whole harness is spent only by the sign-ins).
async fn member(harness: &HomeserverHarness, prefix: &str) -> Result<(String, String, String)> {
    let localpart = HomeserverHarness::unique_localpart(prefix);
    let password = format!("{localpart}-Pw9");
    let mxid = harness.create_user(&localpart, &password).await?;
    Ok((mxid, localpart, password))
}

/// No login request ever reached the non-allowlisted host, while a request
/// this test makes to it does show in the log (so the log is current and
/// covers that host).
async fn assert_no_login_reached_evil(harness: &HomeserverHarness) -> Result<()> {
    let marker = HomeserverHarness::unique_localpart("evil-probe");
    harness
        .get(HomeserverHarness::url(
            harness.evil_url(),
            &["_matrix", "client", "versions", &marker],
        )?)
        .await?;
    harness
        .wait_for_access("the probe of the non-allowlisted host", |line| {
            line.host == EVIL_HOST && line.path.ends_with(&marker)
        })
        .await?;
    let logins: Vec<_> = harness
        .access_log()?
        .into_iter()
        .filter(|line| line.is_login_to(EVIL_HOST))
        .collect();
    ensure!(logins.is_empty(), "logins reached {EVIL_HOST}: {logins:?}");
    Ok(())
}

// --- The tests ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_code_signs_in_and_its_replay_is_used_or_expired_with_a_password_offered() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?;
    let (mxid, localpart, password) = member(&harness, "code").await?;
    let token = harness.mint_login_token(&mxid, 300_000).await?;
    let query = link_query(hs1, Some(&mxid), Some((hs1, token.secret())));

    // Confirm first, then the code signs in.
    let desktop = Desktop::new(&harness, "code")?;
    let (_flow, step) = redeem(&desktop, &query, Some(&format!("Sign in as {mxid}?"))).await?;
    let key = finished(&step)?;
    desktop.running(&key).await?;
    // The code is kept nowhere on disk.
    assert_not_in_files(&desktop.root, token.secret())?;
    let user = parse_user_id(&mxid).context("valid")?;
    ensure!(desktop.manager.is_signed_in(&user));

    // The device is named for this OS.
    let account = desktop.manager.account_by_key(&key).context("open")?;
    let client = account.client().context("a client")?;
    let devices = client.devices().await?;
    ensure!(
        devices
            .devices
            .iter()
            .any(|device| { device.display_name.as_deref() == Some(device_display_name()) }),
        "no {} device: {:?}",
        device_display_name(),
        devices
            .devices
            .iter()
            .map(|device| device.display_name.clone())
            .collect::<Vec<_>>()
    );

    // The same account again on this computer: refused before anything is
    // sent, so a fresh code stays usable.
    let fresh = harness.mint_login_token(&mxid, 300_000).await?;
    let again = desktop.link(&link_query(hs1, Some(&mxid), Some((hs1, fresh.secret()))));
    match again.settled().await? {
        SigninStep::AlreadySignedIn(message) => {
            ensure!(message.contains(&mxid), "{message}");
        }
        other => bail!("not refused as already signed in: {other:?}"),
    }
    let still = harness.login_with_token(&fresh).await?;
    ensure!(
        still.status == 200,
        "the fresh code was used: {}",
        still.status
    );

    // The replay, on another computer: used or expired, then the password
    // form, pre-filled from account_provider and the hint, signs in.
    let other = Desktop::new(&harness, "replay")?;
    let (replay, step) = redeem(&other, &query, None).await?;
    ensure!(
        problem(&step)? == wording::CODE_USED_OR_EXPIRED,
        "the replay said: {step:?}"
    );
    let prefilled = form(&step)?.clone();
    ensure!(prefilled.kind == PasswordKind::FamilyChat);
    ensure!(
        prefilled.server == hs1 && prefilled.username == localpart,
        "{prefilled:?}"
    );
    harness
        .wait_for_access("the refused replay", |line| {
            line.is_login_to(hs1) && line.status == 403
        })
        .await?;
    let key = finished(
        &replay
            .submit(&prefilled.server, &prefilled.username, &password)
            .await?,
    )?;
    other.running(&key).await?;

    let logs = log_text();
    ensure!(
        logs.contains("redeeming a sign-in code"),
        "the capture is empty"
    );
    for secret in [token.secret(), fresh.secret(), password.as_str()] {
        ensure!(!logs.contains(secret), "a token or password was logged");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_code_for_a_non_allowlisted_hs_is_never_sent_and_the_password_goes_to_account_provider()
-> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?;
    let (mxid, localpart, password) = member(&harness, "evilhs").await?;
    let token = harness.mint_login_token(&mxid, 300_000).await?;

    let desktop = Desktop::new(&harness, "evil-hs")?;
    let flow = desktop.link(&link_query(
        hs1,
        Some(&mxid),
        Some((EVIL_HOST, token.secret())),
    ));
    // No confirmation: the code was dropped when the link was read.
    let step = flow.settled().await?;
    ensure!(
        form(&step)?.notice.as_deref() == Some(wording::CODE_NOT_USED),
        "{step:?}"
    );
    ensure!(form(&step)?.server == hs1, "pre-filled from hs: {step:?}");
    // The password goes to account_provider (hs1), never to hs.
    let key = finished(&flow.submit(hs1, &localpart, &password).await?)?;
    desktop.running(&key).await?;
    assert_no_login_reached_evil(&harness).await?;
    // The code was never redeemed, so it still works.
    let unused = harness.login_with_token(&token).await?;
    ensure!(unused.status == 200, "the code was used: {}", unused.status);

    // A link whose account_provider is the non-allowlisted host: the
    // password form refuses it after the well-known, before any login.
    let crafted = desktop.link(&link_query(
        EVIL_HOST,
        Some(&format!("@{localpart}:{EVIL_HOST}")),
        None,
    ));
    let step = crafted.settled().await?;
    ensure!(form(&step)?.server == EVIL_HOST);
    let step = crafted.submit(EVIL_HOST, &localpart, &password).await?;
    ensure!(problem(&step)? == wording::NOT_FAMILY_CHAT, "{step:?}");
    assert_no_login_reached_evil(&harness).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_code_that_signs_in_someone_else_is_signed_out_and_refused() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?;
    // The token is Alice's; the link's hint names Bob (a crafted link, or a
    // parent's mistake). Palpo signs in Alice, the binding refuses her.
    let alice = harness.new_user("alice").await?;
    let (bob, _, _) = member(&harness, "bob").await?;
    let token = harness.mint_login_token(&alice.user_id, 300_000).await?;

    let desktop = Desktop::new(&harness, "mismatch")?;
    let (_flow, step) = redeem(
        &desktop,
        &link_query(hs1, Some(&bob), Some((hs1, token.secret()))),
        Some(&format!("Sign in as {bob}?")),
    )
    .await?;
    ensure!(
        problem(&step)? == wording::CODE_FOR_ANOTHER_ACCOUNT,
        "{step:?}"
    );
    ensure!(
        desktop
            .manager
            .with_profile(|profile| profile.accounts())?
            .is_empty(),
        "the refused account was kept"
    );
    // Alice has only the device the harness signed in: the code's device
    // was signed out.
    let devices = harness
        .client_api(Method::GET, &["v3", "devices"], Some(&alice.session), None)
        .await?
        .ok("Alice's devices")?;
    let names: Vec<_> = devices.body["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|device| device["device_id"].as_str().unwrap_or_default().to_owned())
        .collect();
    ensure!(
        names == [alice.session.device_id.clone()],
        "the code's device is still signed in: {names:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn account_provider_smith_test_signs_in_through_its_well_known() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?;
    let custom = harness.env().custom_domain.clone();
    let (mxid, localpart, password) = member(&harness, "smith").await?;

    // The panel's hint-only link for a custom-domain family: the hint is
    // on the family's own domain, the password form is pre-filled with it,
    // and smith.test's well-known leads to hs1, which the allowlist takes.
    let desktop = Desktop::new(&harness, "smith")?;
    let flow = desktop.link(&link_query(
        &custom,
        Some(&format!("@{localpart}:{custom}")),
        None,
    ));
    let step = flow.settled().await?;
    ensure!(
        form(&step)?.server == custom && form(&step)?.username == localpart,
        "{step:?}"
    );
    let step = flow.submit(&custom, &localpart, &password).await?;
    let key = finished(&step)?;
    desktop.running(&key).await?;
    let account = desktop.manager.account_by_key(&key).context("open")?;
    ensure!(account.view().user_id.as_str() == mxid);
    ensure!(
        account.view().homeserver_url == harness.hs1_url(),
        "{}",
        account.view().homeserver_url
    );
    harness
        .wait_for_access("smith.test's well-known", |line| {
            line.host == custom && line.path == "/.well-known/matrix/client"
        })
        .await?;
    ensure!(
        !harness
            .access_log()?
            .iter()
            .any(|line| line.is_login_to(&custom)),
        "a login went to the custom domain itself"
    );

    // A code redeemed at hs1 for a link naming smith.test with no hint
    // signs in @...:hs1.familychat.test, whose server is not smith.test:
    // the binding refuses it (one Palpo has one server name, so the harness
    // cannot mint a @...:smith.test code).
    let (other, _, _) = member(&harness, "smithcode").await?;
    let token = harness.mint_login_token(&other, 300_000).await?;
    let coded = Desktop::new(&harness, "smith-code")?;
    let (_flow, step) = redeem(
        &coded,
        &link_query(&custom, None, Some((hs1, token.secret()))),
        Some(&format!("Sign in to {custom}?")),
    )
    .await?;
    ensure!(
        problem(&step)? == wording::CODE_FOR_ANOTHER_ACCOUNT,
        "{step:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_307_on_login_is_not_followed_for_a_code_or_a_password() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?;
    let redirect = host(harness.redirect_url())?.to_owned();
    let (mxid, localpart, password) = member(&harness, "redirect").await?;
    let token = harness.mint_login_token(&mxid, 300_000).await?;

    let desktop = Desktop::new(&harness, "redirect")?;
    let flow = desktop.link(&link_query(
        hs1,
        Some(&mxid),
        Some((&redirect, token.secret())),
    ));
    flow.settled().await?;
    let step = flow.answer(SigninAnswer::Continue).await?;
    ensure!(problem(&step)? == wording::CODE_FAILED, "{step:?}");
    harness
        .wait_for_access("the 307", |line| {
            line.is_login_to(&redirect) && line.status == 307
        })
        .await?;
    // The 307 pointed at the non-allowlisted host; nothing followed it,
    // and the code reached no homeserver, so it still works.
    assert_no_login_reached_evil(&harness).await?;
    let unused = harness.login_with_token(&token).await?;
    ensure!(unused.status == 200, "the code was used: {}", unused.status);

    // A password typed for the redirecting (allowlisted) server: discovery
    // passes, the login answers 307, and the password goes no further.
    let typed = desktop.start(SigninStart::FamilyChatPassword);
    typed.settled().await?;
    let step = typed.submit(&redirect, &localpart, &password).await?;
    ensure!(problem(&step)? == wording::SIGNIN_FAILED, "{step:?}");
    assert_no_login_reached_evil(&harness).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn another_server_signs_in_with_a_password_on_synapse() -> Result<()> {
    let harness = HomeserverHarness::connect()?;
    let synapse = harness.synapse_server_name()?.to_owned();
    let user = harness.new_synapse_user("other").await?;
    let localpart = user
        .user_id
        .strip_prefix('@')
        .and_then(|rest| rest.split(':').next())
        .context("a localpart")?
        .to_owned();

    // As a Family Chat server, Synapse is refused.
    let desktop = Desktop::new(&harness, "synapse")?;
    let family = desktop.start(SigninStart::FamilyChatPassword);
    family.settled().await?;
    let step = family.submit(&synapse, &localpart, &user.password).await?;
    ensure!(problem(&step)? == wording::NOT_FAMILY_CHAT, "{step:?}");
    family.flow.answer(SigninAnswer::Cancel);

    // As another Matrix server, it is found first (Phase 14), offers the
    // password only, then signs in and syncs.
    let other = desktop.start(SigninStart::OtherServer);
    let step = other.settled().await?;
    ensure!(matches!(step, SigninStep::Server(_)), "{step:?}");
    let step = other
        .answer(SigninAnswer::ChooseServer {
            server: synapse.clone(),
        })
        .await?;
    ensure!(form(&step)?.kind == PasswordKind::OtherServer);
    let methods = form(&step)?.methods.clone().context("the server's methods")?;
    ensure!(
        methods.password && methods.browser.is_none() && !methods.qr_code,
        "{methods:?}"
    );
    let key = finished(&other.submit(&synapse, &localpart, &user.password).await?)?;
    desktop.running(&key).await?;
    let account = desktop.manager.account_by_key(&key).context("open")?;
    ensure!(account.view().user_id.as_str() == user.user_id);
    ensure!(account.view().homeserver_url == harness.synapse_url());
    Ok(())
}
