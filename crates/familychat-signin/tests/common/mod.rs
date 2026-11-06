//! What the sign-in's offline tests share: a scripted homeserver (a
//! recording [`Transport`]), a fake account manager, and a driver for one
//! flow.

#![allow(dead_code)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use familychat_core::{
    OwnedUserId,
    session::AccountKey,
    signin::{SecretText, SigninAnswer, SigninStart, SigninStep},
};
use familychat_security::{AccountSession, AuthKind};
use familychat_session::{
    PostLoginCheck, QrAnswer, QrEvent, QrLoginError, QrLoginRequest, UrlGuard,
};
use familychat_signin::{
    AccountSink, AddFailure, Allowlist, HttpRequest, HttpResponse, Services, Transport,
    TransportError, http::Method, link::parse_user_id, run_flow,
};
use futures_util::future::BoxFuture;
use reqwest::Url;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, watch};

pub const TOKEN: &str = "lt-Zq8vN3cKp1";
pub const ACCESS: &str = "issued-access-Hk2m9";
pub const PASSWORD: &str = "pw-Tr7uX4";
pub const DEVICE: &str = "Family Chat (Test)";

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// --- A scripted homeserver ------------------------------------------------------

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: Method,
    pub url: String,
    pub bearer: Option<String>,
    pub body: Option<String>,
}

pub enum Reply {
    Json(u16, Value),
    Text(u16, &'static str),
    Unreachable,
    /// Waits for the notify before answering.
    After(Arc<Notify>, u16, Value),
}

#[derive(Default)]
pub struct FakeServer {
    pub routes: Mutex<Vec<(Method, String, VecDeque<Reply>)>>,
    pub seen: Mutex<Vec<Seen>>,
}

impl FakeServer {
    pub fn on(&self, method: Method, url: &str, reply: Reply) -> &Self {
        let mut routes = lock(&self.routes);
        match routes.iter_mut().find(|(m, u, _)| *m == method && u == url) {
            Some((_, _, replies)) => replies.push_back(reply),
            None => routes.push((method, url.to_owned(), VecDeque::from([reply]))),
        }
        self
    }

    /// A Family Chat homeserver at `base` (`https://host`): `/versions`.
    pub fn homeserver(&self, base: &str) -> &Self {
        self.on(
            Method::Get,
            &format!("{base}/_matrix/client/versions"),
            Reply::Json(200, json!({ "versions": ["v1.12"] })),
        )
    }

    /// `name`'s well-known, delegating to `base`.
    pub fn well_known(&self, name: &str, base: &str) -> &Self {
        self.on(
            Method::Get,
            &format!("https://{name}/.well-known/matrix/client"),
            Reply::Json(200, json!({ "m.homeserver": { "base_url": base } })),
        )
    }

    pub fn login(&self, base: &str, reply: Reply) -> &Self {
        self.on(
            Method::Post,
            &format!("{base}/_matrix/client/v3/login"),
            reply,
        )
    }

    pub fn logout(&self, base: &str) -> &Self {
        self.on(
            Method::Post,
            &format!("{base}/_matrix/client/v3/logout"),
            Reply::Json(200, json!({})),
        )
    }

    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.seen).clone()
    }

    pub fn posts(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.method == Method::Post)
            .collect()
    }

    /// Fails if any request carried `needle` anywhere.
    pub fn assert_never_sent(&self, needle: &str) {
        for seen in self.seen() {
            let text = format!("{} {:?} {:?}", seen.url, seen.bearer, seen.body);
            assert!(!text.contains(needle), "{needle} was sent: {seen:?}");
        }
    }
}

pub fn credentials(user_id: &str) -> Value {
    json!({ "user_id": user_id, "access_token": ACCESS, "device_id": "NEWDEVICE" })
}

impl Transport for FakeServer {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let seen = Seen {
                method: request.method,
                url: request.url.to_string(),
                bearer: request.bearer.as_ref().map(|token| token.expose().clone()),
                body: request
                    .json
                    .as_ref()
                    .or(request.form.as_ref())
                    .map(|body| String::from_utf8_lossy(body.expose()).into_owned()),
            };
            lock(&self.seen).push(seen.clone());
            let reply = lock(&self.routes)
                .iter_mut()
                .find(|(method, url, _)| *method == seen.method && *url == seen.url)
                .and_then(|(_, _, replies)| replies.pop_front());
            let respond = |status: u16, body: Vec<u8>| {
                Ok(HttpResponse {
                    status,
                    retry_after: None,
                    body: familychat_security::Secret::new(body),
                })
            };
            match reply {
                None => respond(404, br#"{"errcode":"M_UNRECOGNIZED"}"#.to_vec()),
                Some(Reply::Json(status, body)) => respond(status, body.to_string().into_bytes()),
                Some(Reply::Text(status, body)) => respond(status, body.as_bytes().to_vec()),
                Some(Reply::Unreachable) => Err(TransportError("could not connect".to_owned())),
                Some(Reply::After(notify, status, body)) => {
                    notify.notified().await;
                    respond(status, body.to_string().into_bytes())
                }
            }
        })
    }
}

// --- A fake account manager -----------------------------------------------------

#[derive(Default)]
pub struct FakeAccounts {
    pub signed_in: Mutex<Vec<OwnedUserId>>,
    pub added: Mutex<Vec<(String, String, AuthKind)>>,
    pub failure: Mutex<Option<AddFailure>>,
    /// For each account added, whether the sign-in handed its password on
    /// (for the cross-signing set-up), and whether it was the one typed.
    pub passwords: Mutex<Vec<Option<bool>>>,
    /// For each account added, the OAuth client id it keeps.
    pub client_ids: Mutex<Vec<Option<String>>>,
    /// The QR code sign-ins asked for: the homeserver and the client
    /// metadata.
    pub qr_requests: Mutex<Vec<(String, Value)>>,
    /// Addresses to put to each QR sign-in's guard, and its answers.
    pub guard_probes: Mutex<Vec<String>>,
    pub guard_answers: Mutex<Vec<bool>>,
    /// Who a QR sign-in signs in (default `@ana:smith.safechat.family`).
    pub qr_user: Mutex<Option<String>>,
    /// The approval address a new device sends when it is linked.
    pub approval_uri: Mutex<Option<String>>,
    /// An open account's homeserver, for linking a device.
    pub homeserver: Mutex<Option<Url>>,
    /// The other device scans the fake QR code when this is notified.
    pub scan: Notify,
}

/// The check code the fake QR sign-ins expect.
pub const CHECK_CODE: u8 = 42;

/// A small QR code for the fake sign-ins.
pub fn fake_qr() -> familychat_core::encryption::QrModules {
    familychat_core::encryption::QrModules {
        width: 2,
        dark: vec![true, false, false, true],
    }
}

/// Waits for the check code, then for `wanted` if given; `Err` on a
/// wrong code or a cancel.
async fn fake_answers(
    answers: &mut mpsc::UnboundedReceiver<QrAnswer>,
    wanted: Option<QrAnswer>,
) -> Result<(), QrLoginError> {
    match answers.recv().await {
        Some(QrAnswer::CheckCode(CHECK_CODE)) => {}
        Some(QrAnswer::CheckCode(_)) => return Err(QrLoginError::CheckCodeMismatch),
        _ => return Err(QrLoginError::Cancelled),
    }
    if let Some(wanted) = wanted {
        match answers.recv().await {
            Some(answer) if answer == wanted => {}
            _ => return Err(QrLoginError::Cancelled),
        }
    }
    Ok(())
}

impl FakeAccounts {
    pub fn added(&self) -> Vec<(String, String, AuthKind)> {
        lock(&self.added).clone()
    }
}

impl AccountSink for FakeAccounts {
    fn is_signed_in(&self, user_id: &familychat_core::UserId) -> bool {
        lock(&self.signed_in).iter().any(|known| known == user_id)
    }

    fn add(
        &self,
        session: AccountSession,
        check: Box<dyn PostLoginCheck>,
        password: Option<familychat_security::Secret<String>>,
    ) -> BoxFuture<'_, Result<AccountKey, AddFailure>> {
        Box::pin(async move {
            lock(&self.passwords).push(
                password
                    .as_ref()
                    .map(|password| password.expose() == PASSWORD),
            );
            // As the account manager does: the check sees the client's
            // homeserver and user once the session is restored.
            let homeserver = Url::parse(&session.homeserver_url).expect("a URL");
            let user = parse_user_id(&session.user_id).expect("a user");
            if let Err(refusal) = check.check(&homeserver, &user) {
                return Err(AddFailure::Refused(refusal));
            }
            if let Some(failure) = lock(&self.failure).take() {
                return Err(failure);
            }
            assert_eq!(session.access_token.expose(), ACCESS);
            lock(&self.client_ids).push(session.oauth_client_id.clone());
            lock(&self.added).push((session.user_id, session.homeserver_url, session.auth_kind));
            Ok(AccountKey("new-account".to_owned()))
        })
    }

    fn add_with_qr_code(
        &self,
        request: QrLoginRequest,
        check: Box<dyn PostLoginCheck>,
        events: mpsc::Sender<QrEvent>,
        mut answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> BoxFuture<'_, Result<(AccountKey, OwnedUserId), QrLoginError>> {
        Box::pin(async move {
            self.probe_guard(&request.guard);
            lock(&self.qr_requests).push((
                request.homeserver.to_string(),
                request.client_metadata.clone(),
            ));
            let _ = events
                .send(QrEvent::Ready {
                    qr: fake_qr(),
                    data: familychat_security::Secret::new(b"MATRIX".to_vec()),
                })
                .await;
            self.scan.notified().await;
            let _ = events.send(QrEvent::Scanned).await;
            fake_answers(&mut answers, None).await?;
            let _ = events
                .send(QrEvent::WaitingForApproval {
                    user_code: "ABC123".to_owned(),
                })
                .await;
            let _ = events.send(QrEvent::SyncingSecrets).await;
            let user = lock(&self.qr_user)
                .clone()
                .unwrap_or_else(|| ANA.to_owned());
            let user = parse_user_id(&user).expect("a user");
            check
                .check(&request.homeserver, &user)
                .map_err(QrLoginError::Refused)?;
            if self.is_signed_in(&user) {
                return Err(QrLoginError::AlreadySignedIn(user));
            }
            lock(&self.added).push((
                user.to_string(),
                request.homeserver.to_string(),
                AuthKind::QrCode,
            ));
            Ok((AccountKey("qr-account".to_owned()), user))
        })
    }

    fn link_device(
        &self,
        _account: &AccountKey,
        guard: UrlGuard,
        events: mpsc::Sender<QrEvent>,
        mut answers: mpsc::UnboundedReceiver<QrAnswer>,
    ) -> BoxFuture<'_, Result<(), QrLoginError>> {
        Box::pin(async move {
            self.probe_guard(&guard);
            let _ = events
                .send(QrEvent::Ready {
                    qr: fake_qr(),
                    data: familychat_security::Secret::new(b"MATRIX".to_vec()),
                })
                .await;
            self.scan.notified().await;
            let _ = events.send(QrEvent::Scanned).await;
            fake_answers(&mut answers, None).await?;
            let uri = lock(&self.approval_uri)
                .clone()
                .expect("an approval address");
            let _ = events
                .send(QrEvent::ApproveInBrowser {
                    verification_uri: familychat_security::Secret::new(uri),
                })
                .await;
            match answers.recv().await {
                Some(QrAnswer::Continue) => {}
                _ => return Err(QrLoginError::Cancelled),
            }
            // The new device signs in once the test has seen the browser.
            self.scan.notified().await;
            let _ = events.send(QrEvent::SyncingSecrets).await;
            Ok(())
        })
    }

    fn homeserver_of(&self, _account: &AccountKey) -> Option<Url> {
        lock(&self.homeserver).clone()
    }
}

impl FakeAccounts {
    fn probe_guard(&self, guard: &UrlGuard) {
        let answers = lock(&self.guard_probes)
            .iter()
            .map(|url| guard(&Url::parse(url).expect("a URL")))
            .collect();
        *lock(&self.guard_answers) = answers;
    }
}

// --- Driving a flow -------------------------------------------------------------

pub struct Harness {
    pub server: Arc<FakeServer>,
    pub accounts: Arc<FakeAccounts>,
    pub services: Arc<Services>,
}

impl Harness {
    pub fn new() -> Self {
        let server = Arc::new(FakeServer::default());
        let accounts = Arc::new(FakeAccounts::default());
        let services = Arc::new(Services {
            transport: server.clone(),
            accounts: accounts.clone(),
            allowlist: Allowlist::production(),
            gate: Arc::new(tokio::sync::Mutex::new(())),
            device_name: DEVICE.to_owned(),
        });
        Self {
            server,
            accounts,
            services,
        }
    }

    pub fn start(&self, start: SigninStart) -> Flow {
        let (steps, receiver) = watch::channel(SigninStep::Starting);
        let (answers, answers_receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_flow(
            self.services.clone(),
            start,
            steps,
            answers_receiver,
        ));
        Flow {
            steps: receiver,
            answers: Some(answers),
            task,
        }
    }

    pub fn link(&self, query: &str) -> Flow {
        self.start(SigninStart::Link(SecretText::new(format!(
            "https://safechat.family/app/login?{query}"
        ))))
    }
}

pub struct Flow {
    pub steps: watch::Receiver<SigninStep>,
    pub answers: Option<mpsc::UnboundedSender<SigninAnswer>>,
    pub task: tokio::task::JoinHandle<()>,
}

impl Flow {
    /// Waits for the next step that is not work in progress: after at least
    /// one change since the last step this test looked at.
    pub async fn settled(&mut self) -> SigninStep {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if self.steps.changed().await.is_err() {
                    return self.steps.borrow().clone();
                }
                let step = self.steps.borrow_and_update().clone();
                if !matches!(step, SigninStep::Starting | SigninStep::Working(_)) {
                    return step;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the flow did not settle: {:?}", self.steps.borrow()))
    }

    pub fn answer(&self, answer: SigninAnswer) {
        let sent = self
            .answers
            .as_ref()
            .is_some_and(|answers| answers.send(answer).is_ok());
        assert!(sent, "the flow has ended");
    }

    pub fn submit(&self, server: &str, username: &str) {
        self.answer(SigninAnswer::SubmitPassword {
            server: server.to_owned(),
            username: username.to_owned(),
            password: SecretText::from(PASSWORD),
        });
    }

    /// Waits for the flow to reach a final step.
    pub async fn ended(mut self) -> SigninStep {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let step = self.steps.borrow_and_update().clone();
                if step.is_final() {
                    return step;
                }
                if self.steps.changed().await.is_err() {
                    return self.steps.borrow().clone();
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the flow did not end: {:?}", self.steps.borrow()))
    }

    /// Drops the answers' sender, as a frontend that goes away does, and
    /// returns the step the flow ended on.
    pub async fn abandon(mut self) -> SigninStep {
        drop(self.answers.take());
        tokio::time::timeout(Duration::from_secs(10), &mut self.task)
            .await
            .expect("the flow ends")
            .expect("the flow did not panic");
        self.steps.borrow().clone()
    }
}

/// "Use another Matrix server" for `server`: the server step, then the
/// form with what the server offers.
pub async fn choose_server(flow: &mut Flow, server: &str) -> SigninStep {
    let step = flow.settled().await;
    assert!(matches!(step, SigninStep::Server(_)), "{step:?}");
    flow.answer(SigninAnswer::ChooseServer {
        server: server.to_owned(),
    });
    flow.settled().await
}

pub fn password_form(step: &SigninStep) -> &familychat_core::signin::PasswordForm {
    match step {
        SigninStep::Password(form) => form,
        other => panic!("not the password form: {other:?}"),
    }
}

pub fn link_query(provider: &str, hint: Option<&str>, hs: Option<&str>) -> String {
    let mut query = format!("account_provider={provider}");
    if let Some(hint) = hint {
        query.push_str(&format!("&login_hint=mxid:{hint}"));
    }
    if let Some(hs) = hs {
        query.push_str(&format!("&hs={hs}&token={TOKEN}"));
    }
    query
}

pub const SMITH: &str = "https://smith.safechat.family";
pub const ANA: &str = "@ana:smith.safechat.family";
