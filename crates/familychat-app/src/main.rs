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

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use familychat_assets::FamilyChatAssets;
use familychat_core::{
    AppBootstrap, RuntimeSummary, ThemeSummary, UpstreamPins,
    identity::{APP_NAME, BINARY_NAME},
};
use familychat_security::{Keychain, OsKeychain, logging};
use familychat_session::BackgroundRuntime;
use familychat_store::{DataDir, Profile, ProfileLock, ProfileState};
use familychat_theme::{ThemeFamily, bundled};

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

fn main() -> Result<()> {
    match Command::parse_args(std::env::args().skip(1))? {
        // The lock is held until the window closes; the window opens the
        // keychain and the store through it.
        Command::Gui => launch_gui(hold_profile()?),
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

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Gui,
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
        let Some(command) = args.next() else {
            return Ok(Self::Gui);
        };
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
            "--version" | "-V" | "version" => Self::Version,
            "--help" | "-h" | "help" => Self::Help,
            other => bail!("unknown command `{other}`; run `{BINARY_NAME} help` for the commands"),
        };
        if let Some(extra) = args.next() {
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
    // The app works without a log file; it says so once.
    if let Err(error) = logging::init(&dir.logs_dir()) {
        eprintln!("warning: no log file will be kept: {error}");
    }
    tracing::info!(version = VERSION, "{BINARY_NAME} started");
    Ok(lock)
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
fn launch_gui(lock: ProfileLock) -> Result<()> {
    let runtime = BackgroundRuntime::start(RUNTIME_WORKER_THREADS)?;
    let bootstrap = build_bootstrap(runtime.summary())?;
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
        familychat_ui::LaunchOptions::default(),
    );
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
    let bootstrap = build_bootstrap(RuntimeSummary {
        worker_threads: 0,
        bridge_verified: false,
    })?;
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

fn build_bootstrap(runtime: RuntimeSummary) -> Result<AppBootstrap> {
    Ok(AppBootstrap {
        app_version: VERSION.to_owned(),
        runtime,
        dark_theme: bundled_theme(bundled::DARK).context("the bundled dark theme")?,
        light_theme: bundled_theme(bundled::LIGHT).context("the bundled light theme")?,
        upstream: UpstreamPins {
            zed_commit: ZED_UPSTREAM_COMMIT.to_owned(),
            zed_tag: ZED_TAG.to_owned(),
        },
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
        assert_eq!(parse(&[]).expect("no arguments"), Command::Gui);
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
        let bootstrap = build_bootstrap(RuntimeSummary {
            worker_threads: 1,
            bridge_verified: true,
        })
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
