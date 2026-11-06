//! The terminal UI writing and verifying against the live homeserver
//! harness (ADR 0004, ADR 0008, ADR 0009): `familychat-tui`'s app state
//! driven key by key over the real `LiveSession` on a throwaway profile
//! with the mock keychain, as `familychat tui` wires it. The other side
//! of each conversation is a test-support device: a second `LiveSession`
//! on a profile of its own, driven through `SessionApi` as
//! `familychat-session`'s `live_encryption` drives its devices. Start the
//! harness first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-app --test live_tui_writing -- --ignored --nocapture
//! scripts/homeserver-harness.sh down
//! ```
//!
//! - The terminal joins an encrypted room it was invited to, sends a
//!   message the other member reads, shows the other member's answer,
//!   replies to it and reacts to it.
//! - A new device in the terminal confirms itself by emoji ("Confirm it's
//!   you", `d`) with the account's first device, and then a third device
//!   asks the terminal to verify it, which it does by emoji too.

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use familychat_core::{
    OwnedRoomId, OwnedUserId,
    diff::apply_all,
    encryption::{
        DeviceTrust, EncryptionStatus, IdentitySetup, VerificationAnswer, VerificationFlow,
        VerificationStep,
    },
    messaging::{NewRoom, OutgoingMessage, RoomRequest},
    session::{AccountKey, IncomingVerification, SessionApi, SessionError},
    startup::{OpenState, Opened, StartupApi},
    sync::SyncState,
    timeline::{TimelineContent, TimelineEvent, TimelineItem, TimelineItemKind, TimelineUpdate},
};
use familychat_security::{AccountSession, AuthKind, Keychain, MemoryKeychain, Secret};
use familychat_session::{AccountManager, AnyHomeserver, LiveSession, SessionConfig, account_key};
use familychat_signin::{Allowlist, LiveSignin, ReqwestTransport, Services, device_display_name};
use familychat_store::{DataDir, Profile, ProfileState};
use familychat_test_support::{
    HomeserverHarness, Session, TestUser, edge_client, edge_client_builder,
};
use familychat_tui::{App, AppOptions, Clock, Environment, InitialView, Settings};
use ratatui::{Terminal, backend::TestBackend};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

/// How long a step may take on a busy CI runner.
const WAIT: Duration = Duration::from_secs(120);

/// How long a request to confirm a device waits for the other device
/// before it is cancelled and sent again: matrix-sdk 0.19.1 drops a
/// request from a device the receiving device has no keys for yet (ADR
/// 0009), and the receiving device learns them only when its sync reports
/// the device-list change.
const ASK_AGAIN: Duration = Duration::from_secs(15);

/// How many times a request to confirm a device is sent.
const TRIES: usize = 8;

/// One test at a time: the harness is one peer for Palpo's rate limits.
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn unique_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "familychat-tui-writing-{name}-{}-{nanos}",
        std::process::id()
    ))
}

/// A throwaway profile's account manager on the harness's client.
fn manager(harness: &HomeserverHarness, root: &std::path::Path) -> Result<Arc<AccountManager>> {
    let keychain: Arc<dyn Keychain> = Arc::new(MemoryKeychain::new());
    let profile = match Profile::open(&DataDir::at(root), keychain)? {
        ProfileState::Ready(profile) => profile,
        other => bail!("the test profile did not open: {other:?}"),
    };
    Ok(Arc::new(AccountManager::new(
        profile,
        SessionConfig::with_http_client(edge_client(harness.env())?),
    )))
}

/// Adds a password sign-in to `manager`, as the sign-in hands one over
/// (the password goes with it once, for the cross-signing set-up).
async fn add_account(
    harness: &HomeserverHarness,
    manager: &AccountManager,
    user_id: &str,
    session: &Session,
    password: &str,
) -> Result<AccountKey> {
    let id = manager
        .add_account_with_password(
            AccountSession {
                user_id: user_id.to_owned(),
                device_id: session.device_id.clone(),
                homeserver_url: harness.hs1_url().to_owned(),
                access_token: Secret::new(session.access_token().to_owned()),
                refresh_token: session
                    .refresh_token()
                    .map(|token| Secret::new(token.to_owned())),
                auth_kind: AuthKind::Password,
                oauth_client_id: None,
            },
            &AnyHomeserver,
            Some(Secret::new(password.to_owned())),
        )
        .await?;
    Ok(account_key(id))
}

/// The other end: a device on a profile of its own, driven through
/// `SessionApi`.
struct Device {
    root: std::path::PathBuf,
    manager: Arc<AccountManager>,
    session: Arc<LiveSession>,
    key: AccountKey,
}

impl Device {
    async fn open(
        harness: &HomeserverHarness,
        name: &str,
        user_id: &str,
        session: &Session,
        password: &str,
    ) -> Result<Self> {
        let root = unique_dir(name);
        let manager = manager(harness, &root)?;
        let key = add_account(harness, &manager, user_id, session, password).await?;
        let live = Arc::new(LiveSession::new(
            manager.clone(),
            tokio::runtime::Handle::current(),
        ));
        let device = Self {
            root,
            manager,
            session: live,
            key,
        };
        let mut status = device
            .session
            .account_status(&device.key)
            .context("the account has a status")?;
        tokio::time::timeout(WAIT, async {
            while status.borrow_and_update().sync != SyncState::Running {
                if status.changed().await.is_err() {
                    bail!("the account went away");
                }
            }
            Ok(())
        })
        .await
        .context("the device never synced")??;
        Ok(device)
    }

    async fn until_encryption(
        &self,
        what: &str,
        accept: impl Fn(&EncryptionStatus) -> bool,
    ) -> Result<()> {
        let mut status = self
            .session
            .encryption(&self.key)
            .context("an encryption status")?;
        let waited = tokio::time::timeout(WAIT, async {
            loop {
                if accept(&status.borrow_and_update()) {
                    return Ok(());
                }
                if status.changed().await.is_err() {
                    bail!("the account went away while waiting for {what}");
                }
            }
        })
        .await;
        match waited {
            Ok(result) => result,
            Err(_) => bail!("no {what} within {WAIT:?}: {:?}", status.borrow()),
        }
    }

    async fn shutdown(self) {
        self.manager.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A timeline the other end follows.
struct Followed {
    updates: mpsc::Receiver<TimelineUpdate>,
    items: Vec<TimelineItem>,
    _controls: familychat_core::session::TimelineControls,
}

impl Followed {
    async fn open(device: &Device, room: &OwnedRoomId) -> Result<Self> {
        let subscription = tokio::time::timeout(WAIT, async {
            loop {
                match device.session.timeline(&device.key, room).await {
                    Ok(Ok(subscription)) => return Ok(subscription),
                    Ok(Err(SessionError::UnknownRoom)) => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    Ok(Err(error)) => bail!("the timeline couldn't open: {error}"),
                    Err(_) => bail!("the session went away"),
                }
            }
        })
        .await
        .context("the room never reached the device")??;
        let (updates, controls) = subscription.split();
        Ok(Self {
            updates,
            items: Vec::new(),
            _controls: controls,
        })
    }

    /// Waits until an event passes `accept`, and returns it.
    async fn until(
        &mut self,
        what: &str,
        accept: impl Fn(&TimelineEvent) -> bool,
    ) -> Result<TimelineEvent> {
        let waited = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(event) = self.items.iter().find_map(|item| match &item.kind {
                    TimelineItemKind::Event(event) if accept(event) => Some(event.clone()),
                    _ => None,
                }) {
                    return Ok(event);
                }
                match self.updates.recv().await.context("the timeline closed")? {
                    TimelineUpdate::Items(diffs) => apply_all(&mut self.items, diffs)?,
                    TimelineUpdate::Pagination(_) => {}
                }
            }
        })
        .await;
        match waited {
            Ok(result) => result,
            Err(_) => {
                let shown: Vec<String> = self
                    .items
                    .iter()
                    .filter_map(|item| match &item.kind {
                        TimelineItemKind::Event(event) => Some(format!("{:?}", event.content)),
                        _ => None,
                    })
                    .collect();
                bail!("no {what} within {WAIT:?}; the timeline shows {shown:?}")
            }
        }
    }
}

fn body(event: &TimelineEvent) -> Option<&str> {
    match &event.content {
        TimelineContent::Message { body, .. } => Some(body.as_str()),
        _ => None,
    }
}

/// The live services, opened at once.
struct Ready(Opened);

impl StartupApi for Ready {
    fn open(&self) -> oneshot::Receiver<OpenState> {
        let (answer, receiver) = oneshot::channel();
        let _ = answer.send(OpenState::Ready(self.0.clone()));
        receiver
    }

    fn start_over(&self) -> oneshot::Receiver<OpenState> {
        self.open()
    }
}

/// The terminal UI over an account already signed in on a throwaway
/// profile.
struct LiveTerminal {
    root: std::path::PathBuf,
    manager: Arc<AccountManager>,
    /// The terminal's own session and account, for the facts the test
    /// checks before it acts (never to drive the terminal).
    session: Arc<LiveSession>,
    key: AccountKey,
    app: App,
    terminal: Terminal<TestBackend>,
}

impl LiveTerminal {
    async fn signed_in(
        harness: &HomeserverHarness,
        name: &str,
        user: &str,
        session: &Session,
        password: &str,
    ) -> Result<Self> {
        let root = unique_dir(name);
        let manager = manager(harness, &root)?;
        let key = add_account(harness, &manager, user, session, password).await?;
        let handle = tokio::runtime::Handle::current();
        let transport = ReqwestTransport::with_builder(edge_client_builder(harness.env())?)?;
        let signin = LiveSignin::with_services(
            Services {
                transport: Arc::new(transport),
                accounts: manager.clone(),
                allowlist: Allowlist::for_build(),
                gate: Arc::new(AsyncMutex::new(())),
                device_name: device_display_name().to_owned(),
            },
            handle.clone(),
        );
        let live = Arc::new(LiveSession::new(manager.clone(), handle));
        let startup = Arc::new(Ready(Opened {
            session: live.clone(),
            signin: Arc::new(signin),
            settings: Arc::new(familychat_core::settings::MemorySettings::default()),
            notice: None,
        }));
        let app = App::new(AppOptions {
            startup,
            env: Environment {
                color: true,
                unicode: true,
                over_ssh: false,
                macos: false,
            },
            clock: Clock::local(),
            settings: Settings::default(),
            version: "live".to_owned(),
            initial: InitialView::Default,
            downloads: root.clone(),
        });
        Ok(Self {
            root,
            manager,
            session: live,
            key,
            app,
            terminal: Terminal::new(TestBackend::new(120, 40))?,
        })
    }

    fn text(&mut self) -> Result<String> {
        let app = &mut self.app;
        self.terminal
            .draw(|frame| familychat_tui::draw(app, frame))?;
        let buffer = self.terminal.backend().buffer();
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            let line: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        Ok(out)
    }

    /// Ticks and draws until `done`.
    async fn until(&mut self, what: &str, done: impl Fn(&App, &str) -> bool) -> Result<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            self.app.tick();
            let text = self.text()?;
            if done(&self.app, &text) {
                return Ok(text);
            }
            ensure!(Instant::now() < deadline, "never {what}:\n{text}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn shows(&mut self, wanted: &str) -> Result<String> {
        self.until(wanted, |_, text| text.contains(wanted)).await
    }

    /// Ticks and draws for up to `wait`, saying whether it showed `wanted`.
    async fn shows_within(&mut self, wanted: &str, wait: Duration) -> Result<bool> {
        let deadline = Instant::now() + wait;
        loop {
            self.app.tick();
            if self.text()?.contains(wanted) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Ticks and draws for up to `wait`, until `incoming` hands over a
    /// request.
    async fn request_within(
        &mut self,
        incoming: &mut mpsc::Receiver<IncomingVerification>,
        wait: Duration,
    ) -> Result<Option<VerificationFlow>> {
        let deadline = Instant::now() + wait;
        loop {
            self.app.tick();
            self.text()?;
            match incoming.try_recv() {
                Ok((_, flow)) => return Ok(Some(flow)),
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => bail!("the requests stopped"),
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn key(&mut self, code: KeyCode) {
        self.app
            .handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn typed(&mut self, text: &str) {
        for ch in text.chars() {
            self.key(KeyCode::Char(ch));
        }
    }

    async fn shutdown(self) {
        self.manager.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Whether `[` selected someone else's message (the status line says
/// whose).
fn selected_theirs(app: &App) -> bool {
    app.status()
        .is_some_and(|status| status.starts_with("Selected a message from"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_terminal_joins_sends_receives_replies_and_reacts() -> Result<()> {
    let _serial = SERIAL.lock().await;
    let harness = HomeserverHarness::connect()?;
    let ana: TestUser = harness.new_user("tuiwrite").await?;
    let friend: TestUser = harness.new_user("tuifriend").await?;
    let other = Device::open(
        &harness,
        "friend",
        &friend.user_id,
        &friend.session,
        &friend.password,
    )
    .await?;
    let room_name = HomeserverHarness::unique_localpart("Tea");
    let room = other
        .session
        .create_room(
            &other.key,
            NewRoom {
                name: Some(room_name.clone()),
                invite: vec![OwnedUserId::try_from(ana.user_id.as_str())?],
                direct: false,
                encrypted: true,
            },
        )
        .await
        .context("create_room answers")??;
    let mut followed = Followed::open(&other, &room).await?;

    let mut tui =
        LiveTerminal::signed_in(&harness, "ana", &ana.user_id, &ana.session, &ana.password).await?;
    // A new password sign-in with no other device: nothing to confirm.
    tui.until("the rooms", |app, _| app.account().is_some())
        .await?;
    // The invitation, under Invites; Enter opens it, a joins.
    tui.key(KeyCode::Char('4'));
    tui.shows(&room_name).await?;
    tui.key(KeyCode::Enter);
    tui.shows("Press a to join it").await?;
    tui.key(KeyCode::Char('a'));
    // Joined: the composer takes the keys.
    tui.shows(&format!("Message {room_name}")).await?;

    let first = format!(
        "Hello from the terminal {}",
        HomeserverHarness::unique_localpart("m")
    );
    tui.typed(&first);
    tui.key(KeyCode::Enter);
    followed
        .until("the terminal's message, decrypted", |event| {
            body(event) == Some(first.as_str())
        })
        .await?;

    // The other member answers; the terminal shows it.
    let answer = format!(
        "Hi from your friend {}",
        HomeserverHarness::unique_localpart("m")
    );
    other
        .session
        .room_action(
            &other.key,
            &room,
            RoomRequest::Send {
                message: OutgoingMessage::plain(&answer),
                reply_to: None,
            },
        )
        .await
        .context("room_action answers")??;
    tui.shows(&answer).await?;
    let answered = followed
        .until("the friend's message, sent", |event| {
            body(event) == Some(answer.as_str()) && event.event_id.is_some()
        })
        .await?;

    // Esc, [ selects the newest message (the friend's), R replies.
    tui.key(KeyCode::Esc);
    tui.key(KeyCode::Char('['));
    ensure!(
        selected_theirs(&tui.app),
        "the friend's message is selected: {:?}",
        tui.app.status()
    );
    tui.key(KeyCode::Char('R'));
    tui.shows("Replying to").await?;
    let reply = format!(
        "Replying from the terminal {}",
        HomeserverHarness::unique_localpart("m")
    );
    tui.typed(&reply);
    tui.key(KeyCode::Enter);
    // The terminal shows its own reply: it takes in the session's news
    // only when it ticks, so the test waits on the terminal before it
    // looks at its timeline again.
    tui.shows(&reply).await?;
    let replied = followed
        .until("the terminal's reply", |event| {
            body(event) == Some(reply.as_str())
        })
        .await?;
    ensure!(
        replied
            .in_reply_to
            .as_ref()
            .is_some_and(|preview| Some(&preview.event_id) == answered.event_id.as_ref()),
        "the reply answers the friend's message: {:?}",
        replied
            .in_reply_to
            .as_ref()
            .map(|preview| &preview.event_id)
    );

    // + and 1 react to the friend's message with 👍.
    tui.key(KeyCode::Esc);
    let mut found = false;
    for _ in 0..6 {
        tui.key(KeyCode::Char('['));
        if selected_theirs(&tui.app) {
            found = true;
            break;
        }
    }
    ensure!(found, "the friend's message is selected again");
    tui.key(KeyCode::Char('+'));
    tui.key(KeyCode::Char('1'));
    followed
        .until("the terminal's reaction", |event| {
            body(event) == Some(answer.as_str())
                && event
                    .reactions
                    .iter()
                    .any(|reaction| reaction.key == "👍" && reaction.count == 1)
        })
        .await?;
    // And the terminal shows its own reaction under the friend's message.
    tui.shows("👍").await?;

    tui.shutdown().await;
    other.shutdown().await;
    Ok(())
}

/// Waits until `session` knows a sibling device signed by the account's
/// self-signing key. matrix-sdk-crypto 0.19.1 sends "Confirm it's you"
/// (`OwnUserIdentity::request_verification`) only to the account's devices
/// its store has seen signed (`filter_devices_to_request`), so a device
/// that has just signed in and asks too early reaches nobody. On a timeout
/// the devices it saw are in the error.
async fn knows_a_signed_sibling(session: &LiveSession, key: &AccountKey) -> Result<()> {
    let deadline = Instant::now() + WAIT;
    loop {
        let devices = session
            .devices(key)
            .await
            .context("devices answers")?
            .map_err(|error| anyhow::anyhow!("the devices couldn't be listed: {error}"))?;
        if devices
            .iter()
            .any(|seen| !seen.is_this_device && seen.verified)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let seen: Vec<String> = devices
                .iter()
                .map(|seen| {
                    format!(
                        "{} (this device: {}, signed: {})",
                        seen.device_id, seen.is_this_device, seen.verified
                    )
                })
                .collect();
            bail!("no signed sibling device within {WAIT:?}; the device saw {seen:?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Waits for a verification step that passes `accept`.
async fn step(
    flow: &VerificationFlow,
    what: &str,
    accept: impl Fn(&VerificationStep) -> bool,
) -> Result<VerificationStep> {
    let mut steps = flow.steps();
    let waited = tokio::time::timeout(WAIT, async {
        loop {
            let current = steps.borrow_and_update().clone();
            if accept(&current) {
                return Ok(current);
            }
            if let VerificationStep::Cancelled(why) = &current {
                bail!("the verification stopped while waiting for {what}: {why}");
            }
            if steps.changed().await.is_err() {
                bail!("the verification went away while waiting for {what}");
            }
        }
    })
    .await;
    match waited {
        Ok(result) => result,
        Err(_) => bail!("no {what} within {WAIT:?}; the step is {:?}", flow.step()),
    }
}

/// Checks the terminal shows every emoji of `emoji`'s names.
fn shows_names(text: &str, step: &VerificationStep) -> Result<()> {
    let VerificationStep::CompareEmoji(emoji) = step else {
        bail!("not comparing emoji: {step:?}");
    };
    ensure!(emoji.len() == 7, "seven emoji: {emoji:?}");
    for one in emoji {
        ensure!(
            text.contains(&one.description) && text.contains(&one.symbol),
            "the terminal doesn't show {} {}:\n{text}",
            one.symbol,
            one.description
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_terminal_verifies_by_emoji_with_another_device() -> Result<()> {
    let _serial = SERIAL.lock().await;
    let harness = HomeserverHarness::connect()?;
    let user = harness.new_user("tuiverify").await?;
    let first = Device::open(
        &harness,
        "verify-first",
        &user.user_id,
        &user.session,
        &user.password,
    )
    .await?;
    first
        .until_encryption("a confirmed first device", |status| {
            status.identity == IdentitySetup::Ready && status.device == DeviceTrust::Verified
        })
        .await?;
    // The first device listens for requests (and never declines one).
    let mut first_incoming = first.session.verification_requests();

    // The terminal is a new device of the same account: "Confirm it's you".
    let login = harness
        .login_password(&user.user_id, &user.password)
        .await?;
    let mut tui = LiveTerminal::signed_in(
        &harness,
        "verify-tui",
        &user.user_id,
        &login,
        &user.password,
    )
    .await?;
    tui.shows("Confirm it's you").await?;
    tui.shows("Use another device").await?;
    knows_a_signed_sibling(&tui.session, &tui.key).await?;
    // d: another device. The first device gets the request and accepts.
    // A request the first device drops (ASK_AGAIN) is cancelled with n and
    // sent again, as a person would.
    let mut asked = 0;
    let theirs = loop {
        asked += 1;
        tui.key(KeyCode::Char('d'));
        if let Some(flow) = tui.request_within(&mut first_incoming, ASK_AGAIN).await? {
            break flow;
        }
        ensure!(
            asked < TRIES,
            "no request reached the first device in {asked} tries"
        );
        // n stops it ("You stopped the verification."), Enter closes it.
        tui.key(KeyCode::Char('n'));
        tui.shows("You stopped the verification").await?;
        tui.key(KeyCode::Enter);
        tui.shows("Use another device").await?;
    };
    eprintln!("the terminal's request got through on try {asked}");
    step(&theirs, "the incoming request", |step| {
        *step == VerificationStep::Incoming
    })
    .await?;
    ensure!(theirs.answer(VerificationAnswer::Accept));
    tui.shows("E Compare emoji").await?;
    // e: the terminal starts the emoji comparison.
    tui.key(KeyCode::Char('e'));
    let compared = step(&theirs, "the emoji", |step| {
        matches!(step, VerificationStep::CompareEmoji(_))
    })
    .await?;
    let text = tui.shows("Check that these emoji appear").await?;
    shows_names(&text, &compared)?;
    eprintln!("the terminal showed the emoji and their names:\n{text}");
    // Both say they match.
    tui.key(KeyCode::Char('y'));
    ensure!(theirs.answer(VerificationAnswer::Match));
    step(&theirs, "done on the first device", |step| {
        *step == VerificationStep::Done
    })
    .await?;
    tui.shows("Verified.").await?;
    tui.key(KeyCode::Enter);
    // Confirmed: the rooms, not "Confirm it's you".
    tui.until("the device confirmed", |app, text| {
        app.account().is_some()
            && !text.contains("Confirm it's you")
            && !text.contains("Use another device")
    })
    .await?;

    // A third device asks the terminal to verify it.
    let third_login = harness
        .login_password(&user.user_id, &user.password)
        .await?;
    let third = Device::open(
        &harness,
        "verify-third",
        &user.user_id,
        &third_login,
        &user.password,
    )
    .await?;
    third
        .until_encryption(
            "a third device asked to confirm",
            EncryptionStatus::needs_confirmation,
        )
        .await?;
    knows_a_signed_sibling(&third.session, &third.key).await?;
    let mut asked = 0;
    let ours = loop {
        asked += 1;
        let ours = third
            .session
            .verify_this_device(&third.key)
            .await
            .context("verify_this_device answers")??;
        if tui
            .shows_within("wants to verify this device", ASK_AGAIN)
            .await?
        {
            break ours;
        }
        ensure!(
            asked < TRIES,
            "no request reached the terminal in {asked} tries"
        );
        ensure!(ours.answer(VerificationAnswer::Cancel));
    };
    eprintln!("the third device's request got through on try {asked}");
    tui.key(KeyCode::Char('y'));
    step(&ours, "the terminal's acceptance", |step| {
        matches!(step, VerificationStep::Ready { .. })
    })
    .await?;
    ensure!(ours.answer(VerificationAnswer::CompareEmoji));
    let compared = step(&ours, "the emoji on the third device", |step| {
        matches!(step, VerificationStep::CompareEmoji(_))
    })
    .await?;
    let text = tui.shows("Check that these emoji appear").await?;
    shows_names(&text, &compared)?;
    ensure!(ours.answer(VerificationAnswer::Match));
    tui.key(KeyCode::Char('y'));
    step(&ours, "done on the third device", |step| {
        *step == VerificationStep::Done
    })
    .await?;
    tui.shows("Verified.").await?;
    third
        .until_encryption("the third device confirmed", |status| {
            status.device == DeviceTrust::Verified
        })
        .await?;
    drop(first_incoming);

    tui.shutdown().await;
    third.shutdown().await;
    first.shutdown().await;
    Ok(())
}
