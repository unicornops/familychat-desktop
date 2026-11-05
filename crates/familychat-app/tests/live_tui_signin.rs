//! The terminal UI's sign-in against the live homeserver harness (ADR
//! 0004, ADR 0008): `familychat-tui`'s app state driven key by key, over
//! the real `LiveSignin` and `LiveSession` on a throwaway profile with the
//! mock keychain, exactly as `familychat tui` wires them. A code is minted
//! on Palpo at Family Chat's image and pasted; a password is typed. The
//! front end's access log is the evidence of when and where each login
//! went. Start the harness first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-app --test live_tui_signin -- --ignored
//! scripts/homeserver-harness.sh down
//! ```
//!
//! Each test also creates a room whose name carries escape sequences and
//! invites the new account, so the rooms the terminal reads are a real
//! server's, filtered.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use familychat_core::startup::{OpenState, Opened, StartupApi};
use familychat_security::{Keychain, MemoryKeychain};
use familychat_session::{AccountManager, LiveSession, SessionConfig};
use familychat_signin::{Allowlist, LiveSignin, ReqwestTransport, Services, device_display_name};
use familychat_store::{DataDir, Profile, ProfileState};
use familychat_test_support::{HomeserverHarness, edge_client, edge_client_builder};
use familychat_tui::{App, AppOptions, Clock, Environment, InitialView, Settings};
use ratatui::{Terminal, backend::TestBackend};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

/// How long a step may take on a busy CI runner.
const SETTLE: Duration = Duration::from_secs(90);

/// One test at a time: Palpo has one login bucket for the whole harness.
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

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

/// A throwaway profile, its account manager on the harness's client, the
/// sign-in on the harness's builder, and the terminal UI over them.
struct LiveTerminal {
    root: std::path::PathBuf,
    manager: Arc<AccountManager>,
    app: App,
    terminal: Terminal<TestBackend>,
}

impl LiveTerminal {
    fn new(harness: &HomeserverHarness, name: &str) -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "familychat-tui-live-{name}-{}-{}",
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
        let session = LiveSession::new(manager.clone(), handle);
        let startup = Arc::new(Ready(Opened {
            session: Arc::new(session),
            signin: Arc::new(signin),
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
            app,
            terminal: Terminal::new(TestBackend::new(120, 40))?,
        })
    }

    /// The screen's text.
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

    /// Ticks and draws until `done` says so (or the screen shows one of
    /// `problems`, which is returned).
    async fn until(
        &mut self,
        what: &str,
        done: impl Fn(&App, &str) -> bool,
        problems: &[&str],
    ) -> Result<Option<String>> {
        let deadline = Instant::now() + SETTLE;
        loop {
            self.app.tick();
            let text = self.text()?;
            if done(&self.app, &text) {
                return Ok(None);
            }
            if let Some(problem) = problems.iter().find(|problem| text.contains(**problem)) {
                return Ok(Some((*problem).to_owned()));
            }
            ensure!(Instant::now() < deadline, "never {what}:\n{text}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Ticks until the screen says `wanted`.
    async fn shows(&mut self, wanted: &str) -> Result<()> {
        self.until(wanted, |_, text| text.contains(wanted), &[])
            .await
            .map(drop)
    }

    /// Waits out a rate limit's countdown, as a person would.
    async fn wait_out_the_countdown(&mut self) -> Result<()> {
        eprintln!("rate limited; waiting as a person would");
        self.until(
            "the countdown ends",
            |_, text| !text.contains("You can sign in again"),
            &[],
        )
        .await
        .map(drop)
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
}

impl Drop for LiveTerminal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn host(url: &str) -> Result<&str> {
    url.strip_prefix("https://").context("an https:// URL")
}

/// A `host[:port]`'s host, as the access log names it.
fn hostname(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

/// Whether the terminal shows this account's rooms.
fn signed_in_as(app: &App, mxid: &str) -> bool {
    app.account()
        .is_some_and(|account| account.user_id.as_str() == mxid)
}

/// Percent-encodes a query value.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The logins that reached `host` so far.
fn logins(harness: &HomeserverHarness, host: &str) -> Result<usize> {
    Ok(harness
        .access_log()?
        .into_iter()
        .filter(|line| line.is_login_to(host))
        .count())
}

/// A member, and a room another member made with a hostile name and
/// invited them to.
async fn member_with_an_invite(
    harness: &HomeserverHarness,
    prefix: &str,
) -> Result<(String, String, String, String)> {
    let localpart = HomeserverHarness::unique_localpart(prefix);
    let password = format!("{localpart}-Pw9");
    let mxid = harness.create_user(&localpart, &password).await?;
    let parent = harness.new_user(&format!("{prefix}-parent")).await?;
    let marker = HomeserverHarness::unique_localpart("Room");
    let name = format!("Tea\x1b[2J\x1b]0;pwned\x07 {marker}");
    harness
        .create_room(&parent.session, &name, &[mxid.as_str()])
        .await?;
    Ok((mxid, localpart, password, format!("Tea {marker}")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_terminal_signs_in_with_a_pasted_code_only_after_y() -> Result<()> {
    let _serial = SERIAL.lock().await;
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?.to_owned();
    let (mxid, _, _, room) = member_with_an_invite(&harness, "tuicode").await?;
    let mut tui = LiveTerminal::new(&harness, "code")?;
    tui.shows("Welcome to Family Chat").await?;

    for attempt in 0..6 {
        let token = harness.mint_login_token(&mxid, 300_000).await?;
        let link = format!(
            "https://safechat.family/app/login?account_provider={}&login_hint={}&hs={}&token={}",
            encode(&hs1),
            encode(&format!("mxid:{mxid}")),
            encode(&hs1),
            encode(token.secret())
        );
        let before = logins(&harness, hostname(&hs1))?;
        // Bracketed paste: the terminal hands the link over as one event.
        tui.app.handle_event(Event::Paste(link));
        tui.shows(&format!("Sign in as {mxid}?")).await?;
        // Asked first: nothing was sent, and Enter doesn't answer.
        tui.key(KeyCode::Enter);
        tokio::time::sleep(Duration::from_secs(1)).await;
        tui.app.tick();
        ensure!(
            logins(&harness, hostname(&hs1))? == before,
            "a login left before the confirmation was answered"
        );
        ensure!(
            !tui.text()?.contains(token.secret()),
            "the code is on screen"
        );
        tui.key(KeyCode::Char('y'));
        match tui
            .until(
                "signed in",
                |app, _| signed_in_as(app, &mxid),
                &["Too many sign-in attempts"],
            )
            .await?
        {
            None => break,
            Some(_) if attempt < 5 => {
                // Palpo refuses a rate-limited login before it reads the
                // token, so the next code (a new one) is tried afresh.
                tui.wait_out_the_countdown().await?;
                tui.key(KeyCode::Esc);
            }
            Some(problem) => bail!("still {problem}"),
        }
    }
    // Signed in: the rooms, the new account's invitation among them, its
    // hostile name filtered.
    tui.shows(&room).await?;
    let text = tui.text()?;
    ensure!(
        !text.contains("pwned"),
        "the room name was not filtered:\n{text}"
    );
    ensure!(signed_in_as(&tui.app, &mxid));
    ensure!(logins(&harness, hostname(&hs1))? >= 1);
    ensure!(logins(&harness, "evil.familychat.example")? == 0);
    tui.manager.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn the_terminal_signs_in_with_a_typed_password() -> Result<()> {
    let _serial = SERIAL.lock().await;
    let harness = HomeserverHarness::connect()?;
    let hs1 = host(harness.hs1_url())?.to_owned();
    let (mxid, localpart, password, room) = member_with_an_invite(&harness, "tuipass").await?;
    let mut tui = LiveTerminal::new(&harness, "password")?;
    tui.shows("Welcome to Family Chat").await?;
    // p: "Sign in with your password", the table's key.
    tui.key(KeyCode::Char('p'));
    tui.shows("Your family's address").await?;
    tui.typed(&hs1);
    tui.key(KeyCode::Tab);
    tui.typed(&localpart);
    tui.key(KeyCode::Tab);
    for attempt in 0..6 {
        tui.typed(&password);
        ensure!(
            !tui.text()?.contains(&password),
            "the password is on screen"
        );
        tui.key(KeyCode::Enter);
        match tui
            .until(
                "signed in",
                |app, _| signed_in_as(app, &mxid),
                &["Too many sign-in attempts", "don't match"],
            )
            .await?
        {
            None => break,
            Some(problem) if problem.starts_with("Too many") && attempt < 5 => {
                tui.wait_out_the_countdown().await?;
            }
            Some(problem) => bail!("the password was refused: {problem}"),
        }
    }
    tui.shows(&room).await?;
    let text = tui.text()?;
    ensure!(
        !text.contains("pwned"),
        "the room name was not filtered:\n{text}"
    );
    ensure!(signed_in_as(&tui.app, &mxid));
    ensure!(logins(&harness, "evil.familychat.example")? == 0);
    tui.manager.shutdown().await;
    Ok(())
}
