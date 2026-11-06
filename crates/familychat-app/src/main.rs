//! `familychat`: the Family Chat desktop client's entry point and CLI.
//!
//! With no arguments it opens the GPUI window, which opens the profile
//! through [`startup::LiveStartup`] (the keychain, the store, the accounts'
//! sessions and the sign-in, on the background runtime); `familychat tui`
//! runs the terminal UI over the same startup; `familychat accounts` lists
//! the accounts registry and `familychat start-over` moves unreadable data
//! aside. Builds with the `ui-fixture` feature add `familychat
//! launch-ui-fixture VIEW [dark|light]` for the screenshot matrix and
//! `familychat launch-tui-fixture VIEW` for the terminal UI's fixtures. More account and diagnostics commands arrive
//! with the phases that need them (PLAN.md, section 5.1).
//!
//! Every command that uses the profile takes its lock first (PLAN.md,
//! section 5.5), so a second Family Chat on the same profile, window or
//! terminal, stops with "already open" before it touches the keychain or
//! the store. Logging into the profile's `logs/` starts once the lock is
//! held.
//!
//! The window that holds the lock listens for second launches
//! (`familychat-instance`, ADR 0013): `familychat LINK` (how the OS's URL
//! handler starts it: the `.deb`'s desktop entry, and the MSI's handler
//! through `familychat-launcher`; ADR 0014) and `familychat open-link` (which
//! reads the link from standard input, so a token is never in the shell's
//! history or the process list) forward a sign-in link to it and exit, and
//! a bare `familychat` asks it to come forward. The window shows the
//! sign-in's confirmation; nothing is redeemed without it. A link is never
//! printed or logged.

use std::{
    io::{IsTerminal as _, Read as _},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use familychat_assets::FamilyChatAssets;
use familychat_core::{
    AppBootstrap, RuntimeSummary, ThemeSummary, UpstreamPins,
    identity::{APP_NAME, BINARY_NAME},
    requests::{
        AppRequest, MAX_LINK_BYTES, RequestSender, looks_like_signin_link, request_channel,
    },
    signin::SecretText,
};
use familychat_instance::{Endpoint, ForwardError};
use familychat_security::{Keychain, OsKeychain, logging};
use familychat_session::BackgroundRuntime;
use familychat_signin::{Allowlist, LoginLink};
use familychat_store::{DataDir, LockError, Profile, ProfileLock, ProfileState};
use familychat_theme::{ThemeFamily, bundled};
use zeroize::Zeroize as _;

mod startup;
#[cfg(all(feature = "ui-fixture", feature = "tui"))]
mod tui_fixture;
#[cfg(feature = "ui-fixture")]
mod ui_fixture;

/// The app's version, as released.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Zed commit GPUI is built from, read from `Cargo.lock` by `build.rs`.
const ZED_UPSTREAM_COMMIT: &str = env!("FAMILYCHAT_ZED_COMMIT");
/// Zed stable release tag GPUI is pinned to (see docs/upstream/zed.md).
const ZED_TAG: &str = env!("FAMILYCHAT_ZED_TAG");
/// Tokio worker threads for the background runtime.
const RUNTIME_WORKER_THREADS: usize = 4;
/// How long a second launch tries to reach a window that is still
/// starting before it says the profile is open elsewhere.
const HANDOFF_WAIT: Duration = Duration::from_secs(2);
/// What a link that isn't a usable sign-in link is called (never the link
/// itself: it may hold a code).
const NOT_A_LINK: &str = "That isn't a Family Chat sign-in link.";

fn main() -> Result<()> {
    match Command::parse_args(std::env::args().skip(1))? {
        // The lock is held until the window closes; the window opens the
        // keychain and the store through it. If another window has the
        // profile, the link (or "come forward") goes to it instead.
        Command::Gui(link) => open_window(link.map(LinkArgument::into_secret)),
        Command::OpenLink => open_window(Some(read_link_from_stdin()?)),
        #[cfg(feature = "ui-fixture")]
        Command::LaunchUiFixture { view, theme } => launch_ui_fixture(&view, theme),
        #[cfg(feature = "ui-fixture")]
        Command::ListUiFixtures => {
            println!("{}", ui_fixture::VIEWS.join("\n"));
            Ok(())
        }
        #[cfg(feature = "tui")]
        Command::Tui(options) => launch_tui(hold_profile()?, options),
        #[cfg(all(feature = "ui-fixture", feature = "tui"))]
        Command::LaunchTuiFixture(view) => tui_fixture::launch(&view, VERSION),
        #[cfg(all(feature = "ui-fixture", feature = "tui"))]
        Command::ListTuiFixtures => {
            println!("{}", familychat_tui::fixture_views::VIEWS.join("\n"));
            Ok(())
        }
        Command::Accounts => {
            let state = open_profile(hold_profile()?)?;
            println!("{}", accounts_report(state)?);
            Ok(())
        }
        Command::StartOver => {
            let state = open_profile(hold_profile()?)?;
            println!("{}", start_over(state)?);
            Ok(())
        }
        Command::Version => {
            println!("{}", version_text());
            Ok(())
        }
        Command::Help => {
            print_help();
            Ok(())
        }
    }
}

/// A sign-in link given on the command line. Compares by text (tests);
/// `Debug` hides it.
struct LinkArgument(SecretText);

impl LinkArgument {
    fn into_secret(self) -> SecretText {
        self.0
    }
}

impl PartialEq for LinkArgument {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose() == other.0.expose()
    }
}

impl Eq for LinkArgument {}

impl std::fmt::Debug for LinkArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkArgument([redacted])")
    }
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// The window, with a sign-in link to start with (the OS's URL
    /// handler passes it).
    Gui(Option<LinkArgument>),
    /// The window, with a link read from standard input.
    OpenLink,
    #[cfg(feature = "ui-fixture")]
    LaunchUiFixture {
        view: String,
        theme: Option<familychat_core::ThemeAppearance>,
    },
    #[cfg(feature = "ui-fixture")]
    ListUiFixtures,
    #[cfg(feature = "tui")]
    Tui(TuiOptions),
    #[cfg(all(feature = "ui-fixture", feature = "tui"))]
    LaunchTuiFixture(String),
    #[cfg(all(feature = "ui-fixture", feature = "tui"))]
    ListTuiFixtures,
    Accounts,
    StartOver,
    Version,
    Help,
}

impl Command {
    fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let Some(mut command) = args.next() else {
            return Ok(Self::Gui(None));
        };
        if looks_like_signin_link(&command) {
            // A link: never echoed, even in an error.
            if args.next().is_some() {
                command.zeroize();
                bail!("a sign-in link takes no other arguments");
            }
            return Ok(Self::Gui(Some(LinkArgument(SecretText::new(command)))));
        }
        if might_be_a_link(&command) {
            command.zeroize();
            bail!("{NOT_A_LINK}");
        }
        let parsed = match command.as_str() {
            #[cfg(feature = "ui-fixture")]
            "launch-ui-fixture" => {
                let view = args
                    .next()
                    .context("launch-ui-fixture needs a view (or --list)")?;
                if view == "--list" {
                    Self::ListUiFixtures
                } else {
                    ui_fixture::check_view(&view)?;
                    let theme = ui_fixture::parse_theme(args.next().as_deref())?;
                    Self::LaunchUiFixture { view, theme }
                }
            }
            #[cfg(feature = "tui")]
            "tui" => {
                let mut options = TuiOptions::default();
                for flag in args.by_ref() {
                    match flag.as_str() {
                        "--bell" => options.bell = true,
                        "--ascii" => options.ascii = true,
                        "--no-mouse" => options.no_mouse = true,
                        other => bail!("`tui` takes --bell, --ascii and --no-mouse, not `{other}`"),
                    }
                }
                Self::Tui(options)
            }
            #[cfg(all(feature = "ui-fixture", feature = "tui"))]
            "launch-tui-fixture" => {
                let view = args
                    .next()
                    .context("launch-tui-fixture needs a view (or --list)")?;
                if view == "--list" {
                    Self::ListTuiFixtures
                } else {
                    tui_fixture::check_view(&view)?;
                    Self::LaunchTuiFixture(view)
                }
            }
            "accounts" => Self::Accounts,
            "start-over" => Self::StartOver,
            "open-link" => Self::OpenLink,
            "--version" | "-V" | "version" => Self::Version,
            "--help" | "-h" | "help" => Self::Help,
            other => bail!("unknown command `{other}`; run `{BINARY_NAME} help` for the commands"),
        };
        if let Some(mut extra) = args.next() {
            if looks_like_signin_link(&extra) || might_be_a_link(&extra) {
                extra.zeroize();
                bail!(
                    "`{command}` takes no arguments (`{BINARY_NAME} open-link` reads the link from standard input)"
                );
            }
            bail!("`{command}` takes no arguments, but got `{extra}`");
        }
        Ok(parsed)
    }
}

/// `familychat --version`: the version and the GPUI it was built with.
fn version_text() -> String {
    format!("{BINARY_NAME} {VERSION} (GPUI from Zed {ZED_TAG}, {ZED_UPSTREAM_COMMIT})")
}

fn print_help() {
    println!("{APP_NAME} commands:");
    println!("  {BINARY_NAME}               Open the {APP_NAME} window");
    #[cfg(feature = "tui")]
    println!(
        "  {BINARY_NAME} tui           Use {APP_NAME} in the terminal (--bell, --ascii, --no-mouse)"
    );
    println!(
        "  {BINARY_NAME} open-link     Sign in with a link read from standard input (paste it, then Enter)"
    );
    println!("  {BINARY_NAME} LINK          Open the window with a sign-in link");
    println!("  {BINARY_NAME} accounts      List the signed-in accounts");
    println!("  {BINARY_NAME} start-over    Move data that can't be read aside and start again");
    #[cfg(feature = "ui-fixture")]
    println!(
        "  {BINARY_NAME} launch-ui-fixture VIEW [dark|light]  Open the window on a fixture (--list)"
    );
    #[cfg(all(feature = "ui-fixture", feature = "tui"))]
    println!("  {BINARY_NAME} launch-tui-fixture VIEW  Open the terminal UI on a fixture (--list)");
    println!("  {BINARY_NAME} --version     Show the version");
    println!("  {BINARY_NAME} help          Show this help");
}

/// Takes the profile's lock, then starts logging into its `logs/`. A
/// profile another process has open stops here, before the keychain or the
/// store is touched.
fn hold_profile() -> Result<ProfileLock> {
    let dir = DataDir::resolve()?;
    let lock = ProfileLock::acquire(&dir)?;
    start_logging(&dir);
    Ok(lock)
}

/// Starts logging into the profile's `logs/` (only with its lock held).
fn start_logging(dir: &DataDir) {
    // The app works without a log file; it says so once.
    if let Err(error) = logging::init(&dir.logs_dir()) {
        eprintln!("warning: no log file will be kept: {error}");
    }
    tracing::info!(version = VERSION, "{BINARY_NAME} started");
}

/// Text that may be a mistyped link (and so may hold a code): refused
/// without being echoed.
fn might_be_a_link(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("://") || lower.starts_with("familychat:") || lower.contains("token=")
}

/// Checks `link` is a sign-in link Family Chat can use (the sign-in's own
/// parser; the window applies every rule again when it starts the sign-in).
fn check_link(link: &SecretText) -> Result<()> {
    match LoginLink::parse(link.expose(), &Allowlist::for_build()) {
        Ok(_) => Ok(()),
        Err(_) => bail!("{NOT_A_LINK}"),
    }
}

/// `familychat open-link`: one line from standard input (the link pasted
/// and Enter, or piped), at most a few kilobytes. On a terminal it asks
/// first; what is typed stays off the shell's history.
fn read_link_from_stdin() -> Result<SecretText> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("Paste the sign-in link, then press Enter:");
    }
    let limit = u64::try_from(MAX_LINK_BYTES + 2).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    let mut handle = stdin.lock().take(limit);
    let mut byte = [0_u8; 1];
    while handle.read(&mut byte).context("failed to read the link")? == 1 {
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
    }
    byte.zeroize();
    if bytes.len() > MAX_LINK_BYTES {
        bytes.zeroize();
        bail!("{NOT_A_LINK}");
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            error.into_bytes().zeroize();
            bail!("{NOT_A_LINK}");
        }
    };
    let link = SecretText::new(text.trim().to_owned());
    let mut text = text;
    text.zeroize();
    if !looks_like_signin_link(link.expose()) {
        bail!("{NOT_A_LINK}");
    }
    Ok(link)
}

/// The window, with `link` to start a sign-in with: this process becomes
/// the window when the profile is free; otherwise the link (or a request
/// to come forward) goes to the window that has it.
fn open_window(link: Option<SecretText>) -> Result<()> {
    if let Some(link) = &link {
        check_link(link)?;
    }
    let dir = DataDir::resolve()?;
    match ProfileLock::acquire(&dir) {
        Ok(lock) => {
            start_logging(&dir);
            launch_gui(lock, link)
        }
        Err(already @ LockError::AlreadyOpen { .. }) => hand_off(&dir, link, already),
        Err(error) => Err(error.into()),
    }
}

/// From a second launch: forwards `link` (or "come forward") to the
/// window that has the profile, and returns. `already` is what to say when
/// nothing listens (the terminal UI has the profile).
fn hand_off(dir: &DataDir, link: Option<SecretText>, already: LockError) -> Result<()> {
    let has_link = link.is_some();
    let Ok(endpoint) = Endpoint::for_profile(dir.root()) else {
        return Err(already.into());
    };
    let request = link.map_or(AppRequest::Activate, AppRequest::OpenLink);
    match familychat_instance::forward(&endpoint, request, HANDOFF_WAIT) {
        Ok(()) if has_link => {
            println!("{APP_NAME} is already open; the sign-in continues in its window.");
            Ok(())
        }
        Ok(()) => {
            println!("{APP_NAME} is already open.");
            Ok(())
        }
        Err(ForwardError::NotListening) => Err(already.into()),
        Err(ForwardError::Refused) => bail!("{NOT_A_LINK}"),
        Err(error @ ForwardError::Unsafe(_)) => {
            bail!("{error}; nothing was sent to the open {APP_NAME}")
        }
        Err(error @ ForwardError::Wire(_)) => bail!("{error}"),
    }
}

/// The requests a second launch hands over, checked and passed to the
/// window: a link the sign-in's parser refuses is declined here.
fn deliver_to(window: RequestSender) -> familychat_instance::Deliver {
    let allowlist = Allowlist::for_build();
    Arc::new(move |request| {
        if let AppRequest::OpenLink(link) = &request
            && LoginLink::parse(link.expose(), &allowlist).is_err()
        {
            return false;
        }
        window.try_send(request).is_ok()
    })
}

/// Opens the profile with the OS keychain.
fn open_profile(lock: ProfileLock) -> Result<ProfileState> {
    let keychain: Arc<dyn Keychain> = Arc::new(OsKeychain::new());
    Ok(Profile::open_locked(lock, keychain)?)
}

/// `accounts`: the registry, or what is stopping it from being read.
fn accounts_report(state: ProfileState) -> Result<String> {
    let profile = ready_or_explain(state)?;
    let accounts = profile.accounts()?;
    if accounts.is_empty() {
        return Ok("No accounts yet.".to_owned());
    }
    Ok(accounts
        .iter()
        .map(|account| {
            format!(
                "{}  {}  ({})",
                account.user_id, account.homeserver_url, account.id
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `start-over`: moves data that can't be read aside and starts a new
/// store. Refuses when the data can be read, or the keychain can't be
/// reached (the data may be fine; only the keychain is missing).
fn start_over(state: ProfileState) -> Result<String> {
    match state {
        ProfileState::Unreadable(unreadable) => {
            let started = unreadable.start_over()?;
            let next = match started.state {
                ProfileState::Ready(_) => format!("{APP_NAME} has started again; sign in again."),
                ProfileState::KeychainUnavailable(blocked) => blocked.message(),
                ProfileState::Unreadable(unreadable) => unreadable.message(),
            };
            Ok(format!(
                "Moved the data that can't be read to {}. Delete it once you are sure its key \
                 won't turn up. {next}",
                started.moved_to.display()
            ))
        }
        ProfileState::KeychainUnavailable(blocked) => {
            Err(anyhow!("{} Not starting over.", blocked.message()))
        }
        ProfileState::Ready(_) => {
            bail!("{APP_NAME}'s data opens with its key; there is nothing to start over")
        }
    }
}

/// The open profile, or an error that says what the user can do.
fn ready_or_explain(state: ProfileState) -> Result<Profile> {
    match state {
        ProfileState::Ready(profile) => Ok(profile),
        ProfileState::KeychainUnavailable(blocked) => Err(anyhow!("{}", blocked.message())),
        ProfileState::Unreadable(unreadable) => Err(anyhow!(
            "{} Run `{BINARY_NAME} start-over` to do that.",
            unreadable.message()
        )),
    }
}

/// The window: the background runtime, then GPUI over the live startup,
/// which opens the profile `lock` holds with the OS keychain. When the
/// window closes the accounts' syncs stop before the runtime does.
fn launch_gui(lock: ProfileLock, link: Option<SecretText>) -> Result<()> {
    let runtime = BackgroundRuntime::start(RUNTIME_WORKER_THREADS)?;
    let bootstrap = build_bootstrap(runtime.summary(), Some(lock.dir().logs_dir()))?;
    let (requests, inbox) = request_channel();
    if let Some(link) = link {
        let _ = requests.try_send(AppRequest::OpenLink(link));
    }
    // Second launches hand their links to this window. Without the
    // listener the window still works; they say it's already open.
    let listener = Endpoint::for_profile(lock.dir().root()).and_then(|endpoint| {
        familychat_instance::listen(&endpoint, runtime.handle(), deliver_to(requests.clone()))
    });
    let listener = match listener {
        Ok(listener) => Some(listener),
        Err(error) => {
            tracing::warn!(%error, "second launches can't hand their links to this window");
            None
        }
    };
    let keychain: Arc<dyn Keychain> = Arc::new(OsKeychain::new());
    let startup = Arc::new(startup::LiveStartup::new(
        lock,
        keychain,
        runtime.handle().clone(),
    ));
    familychat_ui::run(
        FamilyChatAssets,
        bootstrap,
        startup.clone(),
        familychat_ui::LaunchOptions {
            requests: Some(familychat_ui::RequestInbox::new(requests, inbox)),
            ..familychat_ui::LaunchOptions::default()
        },
    );
    {
        let _entered = runtime.handle().enter();
        drop(listener);
    }
    if let Some(manager) = startup.manager() {
        runtime.handle().block_on(manager.shutdown());
    }
    drop(startup);
    drop(runtime);
    Ok(())
}

/// `launch-ui-fixture`: the window on a fixture, with no profile, keychain
/// or network.
#[cfg(feature = "ui-fixture")]
fn launch_ui_fixture(view: &str, theme: Option<familychat_core::ThemeAppearance>) -> Result<()> {
    let (startup, options) = ui_fixture::startup(view, theme)?;
    // A stand-in profile's logs, for Settings → Diagnostics: the fixture
    // has no profile.
    let bootstrap = build_bootstrap(
        RuntimeSummary {
            worker_threads: 0,
            bridge_verified: false,
        },
        Some(std::path::PathBuf::from(
            "/home/ana/.local/share/familychat/logs",
        )),
    )?;
    familychat_ui::run(FamilyChatAssets, bootstrap, startup, options);
    Ok(())
}

/// What `familychat tui` was asked for.
#[cfg(feature = "tui")]
#[derive(Debug, Default, PartialEq, Eq)]
struct TuiOptions {
    /// `--bell`: ring the terminal bell for new messages.
    bell: bool,
    /// `--ascii`: draw with ASCII only.
    ascii: bool,
    /// `--no-mouse`: leave the mouse to the terminal.
    no_mouse: bool,
}

/// The program copies are piped to, instead of OSC 52 (`wl-copy`, `xclip
/// -selection clipboard`); run without a shell.
#[cfg(feature = "tui")]
const COPY_COMMAND_VAR: &str = "FAMILYCHAT_TUI_COPY_COMMAND";

/// `tui`: the terminal UI over the same startup as the window: the
/// background runtime, then `LiveStartup` opening the profile `lock` holds
/// with the OS keychain (the keychain and lost-key states included). When
/// the terminal UI ends the accounts' syncs stop before the runtime does.
#[cfg(feature = "tui")]
fn launch_tui(lock: ProfileLock, options: TuiOptions) -> Result<()> {
    let runtime = BackgroundRuntime::start(RUNTIME_WORKER_THREADS)?;
    let keychain: Arc<dyn Keychain> = Arc::new(OsKeychain::new());
    let startup = Arc::new(startup::LiveStartup::new(
        lock,
        keychain,
        runtime.handle().clone(),
    ));
    let settings = familychat_tui::Settings {
        bell: options.bell,
        mouse: !options.no_mouse,
        ascii: options.ascii,
        copy: familychat_tui::CopySettings {
            copy_command: std::env::var(COPY_COMMAND_VAR)
                .ok()
                .filter(|command| !command.trim().is_empty()),
            ..familychat_tui::CopySettings::default()
        },
    };
    let result = familychat_tui::run(familychat_tui::Launch {
        startup: startup.clone(),
        version: VERSION.to_owned(),
        settings,
        initial: familychat_tui::InitialView::Default,
    });
    if let Some(manager) = startup.manager() {
        runtime.handle().block_on(manager.shutdown());
    }
    drop(startup);
    drop(runtime);
    result
}

fn build_bootstrap(
    runtime: RuntimeSummary,
    log_dir: Option<std::path::PathBuf>,
) -> Result<AppBootstrap> {
    Ok(AppBootstrap {
        app_version: VERSION.to_owned(),
        runtime,
        dark_theme: bundled_theme(bundled::DARK).context("the bundled dark theme")?,
        light_theme: bundled_theme(bundled::LIGHT).context("the bundled light theme")?,
        upstream: UpstreamPins {
            zed_commit: ZED_UPSTREAM_COMMIT.to_owned(),
            zed_tag: ZED_TAG.to_owned(),
        },
        log_dir,
    })
}

/// The first variant of a bundled theme family.
fn bundled_theme(json: &str) -> Result<ThemeSummary> {
    let family = ThemeFamily::from_json_str(json).context("failed to parse the theme")?;
    let theme = family
        .default_theme()
        .context("the theme family has no variants")?;
    Ok(family.summary(theme))
}

#[cfg(test)]
mod tests {
    use familychat_core::ThemeAppearance;
    use familychat_security::{KeychainEntry, KeychainFailure, MemoryKeychain};
    use familychat_store::NewAccount;

    use super::*;

    fn parse(args: &[&str]) -> Result<Command> {
        Command::parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_the_commands() {
        assert_eq!(parse(&[]).expect("no arguments"), Command::Gui(None));
        assert_eq!(parse(&["open-link"]).expect("open-link"), Command::OpenLink);
        #[cfg(feature = "tui")]
        {
            assert_eq!(
                parse(&["tui"]).expect("tui"),
                Command::Tui(TuiOptions::default())
            );
            assert_eq!(
                parse(&["tui", "--bell", "--ascii", "--no-mouse"]).expect("tui"),
                Command::Tui(TuiOptions {
                    bell: true,
                    ascii: true,
                    no_mouse: true,
                })
            );
            assert!(parse(&["tui", "--colour"]).is_err());
        }
        for version in ["--version", "-V", "version"] {
            assert_eq!(parse(&[version]).expect("version"), Command::Version);
        }
        for help in ["--help", "-h", "help"] {
            assert_eq!(parse(&[help]).expect("help"), Command::Help);
        }
    }

    #[test]
    fn parses_the_profile_commands() {
        assert_eq!(parse(&["accounts"]).expect("accounts"), Command::Accounts);
        assert_eq!(
            parse(&["start-over"]).expect("start-over"),
            Command::StartOver
        );
        assert!(parse(&["accounts", "--all"]).is_err());
    }

    /// The profile in a fresh temporary directory, with the mock keychain:
    /// the commands' logic without the real keychain.
    struct TestProfile {
        root: std::path::PathBuf,
        keychain: Arc<MemoryKeychain>,
    }

    impl TestProfile {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Self {
                root: std::env::temp_dir().join(format!(
                    "familychat-app-{name}-{}-{nanos}",
                    std::process::id()
                )),
                keychain: Arc::new(MemoryKeychain::new()),
            }
        }

        fn open(&self) -> ProfileState {
            Profile::open(&DataDir::at(&self.root), self.keychain.clone()).expect("opens")
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn accounts_lists_the_registry() {
        let test = TestProfile::new("accounts");
        assert_eq!(
            accounts_report(test.open()).expect("ready"),
            "No accounts yet."
        );
        let ProfileState::Ready(profile) = test.open() else {
            panic!("ready");
        };
        let id = profile
            .add_account(&NewAccount {
                user_id: "@alice:example.org".to_owned(),
                homeserver_url: "https://matrix.example.org".to_owned(),
            })
            .expect("added");
        drop(profile);
        assert_eq!(
            accounts_report(test.open()).expect("ready"),
            format!("@alice:example.org  https://matrix.example.org  ({id})")
        );
    }

    #[test]
    fn accounts_explains_a_locked_keychain_and_a_missing_key() {
        let test = TestProfile::new("accounts-states");
        test.keychain.fail_with(Some(KeychainFailure::Locked));
        let error = accounts_report(test.open()).expect_err("locked");
        assert!(error.to_string().contains("keychain is locked"), "{error}");
        let error = start_over(test.open()).expect_err("not with a locked keychain");
        assert!(error.to_string().contains("Not starting over"), "{error}");

        test.keychain.fail_with(None);
        assert!(
            start_over(test.open()).is_err(),
            "readable data is not moved"
        );
        test.keychain
            .delete(&KeychainEntry::AppStore)
            .expect("the key is gone");
        let error = accounts_report(test.open()).expect_err("unreadable");
        assert!(
            error.to_string().contains("familychat start-over"),
            "{error}"
        );
        let report = start_over(test.open()).expect("started over");
        assert!(
            report.starts_with("Moved the data that can't be read to"),
            "{report}"
        );
        assert!(report.contains("unreadable-"), "{report}");
        assert_eq!(
            accounts_report(test.open()).expect("ready"),
            "No accounts yet."
        );
    }

    #[test]
    fn refuses_unknown_commands_and_extra_arguments() {
        let unknown = parse(&["sync"]).expect_err("an unknown command");
        assert!(unknown.to_string().contains("unknown command `sync`"));
        let extra = parse(&["--version", "now"]).expect_err("an extra argument");
        assert!(extra.to_string().contains("takes no arguments"));
    }

    const LINK: &str = "familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:@ana:smith.safechat.family&hs=hs1.smith.safechat.family&token=secret-token";

    #[test]
    fn a_link_argument_opens_the_window_with_it_and_is_never_echoed() {
        assert_eq!(
            parse(&[LINK]).expect("a link"),
            Command::Gui(Some(LinkArgument(SecretText::from(LINK))))
        );
        let https = LINK.replace("familychat://", "https://");
        assert!(matches!(parse(&[&https]), Ok(Command::Gui(Some(_)))));
        assert!(!format!("{:?}", parse(&[LINK])).contains("secret"));
        for args in [
            vec![LINK, "--extra"],
            vec!["open-link", LINK],
            vec!["familychat:/broken?token=secret-token\u{7}"],
            vec!["https://evil.example/app/login?token=secret-token"],
            vec!["token=secret-token"],
        ] {
            let error = parse(&args).expect_err("refused");
            assert!(!error.to_string().contains("secret"), "{error}");
        }
        assert!(check_link(&SecretText::from(LINK)).is_ok());
        let error = check_link(&SecretText::from(
            "familychat://safechat.family/app/elsewhere?token=secret-token",
        ))
        .expect_err("not a sign-in link");
        assert_eq!(error.to_string(), NOT_A_LINK);
    }

    #[test]
    fn the_window_is_handed_only_sign_in_links() {
        let (sender, mut receiver) = request_channel();
        let deliver = deliver_to(sender);
        assert!(deliver(AppRequest::OpenLink(SecretText::from(LINK))));
        assert!(deliver(AppRequest::Activate));
        assert!(!deliver(AppRequest::OpenLink(SecretText::from(
            "familychat://safechat.family/app/login?token=no-account-provider"
        ))));
        assert!(
            matches!(receiver.try_recv(), Ok(AppRequest::OpenLink(link)) if link.expose() == LINK)
        );
        assert!(matches!(receiver.try_recv(), Ok(AppRequest::Activate)));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn the_version_names_the_gpui_pin() {
        let text = version_text();
        assert!(text.starts_with(&format!("familychat {VERSION} (GPUI from Zed v")));
        // build.rs read a real tag and commit from Cargo.lock.
        assert_ne!(ZED_TAG, "unknown");
        assert_eq!(ZED_UPSTREAM_COMMIT.len(), 40);
    }

    #[test]
    fn the_bootstrap_carries_both_bundled_themes() {
        let bootstrap = build_bootstrap(
            RuntimeSummary {
                worker_threads: 1,
                bridge_verified: true,
            },
            None,
        )
        .expect("the bundled themes parse");
        assert_eq!(
            bootstrap.theme(ThemeAppearance::Dark).appearance,
            ThemeAppearance::Dark
        );
        assert_eq!(
            bootstrap.theme(ThemeAppearance::Light).appearance,
            ThemeAppearance::Light
        );
    }
}
