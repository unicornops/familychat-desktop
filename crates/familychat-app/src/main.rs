//! `familychat`: the Family Chat desktop client's entry point and CLI.
//!
//! With no arguments it opens the GPUI window; `familychat tui` runs the
//! terminal UI. Account and diagnostics commands arrive with the phases
//! that need them (PLAN.md, section 5.1).

use anyhow::{Context, Result, bail};
use familychat_assets::FamilyChatAssets;
use familychat_core::{
    AppBootstrap, RuntimeSummary, ThemeSummary, UpstreamPins,
    identity::{APP_NAME, BINARY_NAME},
};
use familychat_session::BackgroundRuntime;
use familychat_theme::{ThemeFamily, bundled};

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
        Command::Gui => launch_gui(),
        #[cfg(feature = "tui")]
        Command::Tui => launch_tui(),
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
    #[cfg(feature = "tui")]
    Tui,
    Version,
    Help,
}

impl Command {
    fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let Some(command) = args.next() else {
            return Ok(Self::Gui);
        };
        let parsed = match command.as_str() {
            #[cfg(feature = "tui")]
            "tui" => Self::Tui,
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
    println!("  {BINARY_NAME} tui           Use {APP_NAME} in the terminal");
    println!("  {BINARY_NAME} --version     Show the version");
    println!("  {BINARY_NAME} help          Show this help");
}

fn launch_gui() -> Result<()> {
    let runtime = BackgroundRuntime::start(RUNTIME_WORKER_THREADS)?;
    let bootstrap = build_bootstrap(runtime.summary())?;
    familychat_ui::run(FamilyChatAssets, bootstrap);
    drop(runtime);
    Ok(())
}

/// `tui`: the terminal UI.
#[cfg(feature = "tui")]
fn launch_tui() -> Result<()> {
    familychat_tui::run(familychat_tui::Launch {
        version: VERSION.to_owned(),
    })
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

    use super::*;

    fn parse(args: &[&str]) -> Result<Command> {
        Command::parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_the_commands() {
        assert_eq!(parse(&[]).expect("no arguments"), Command::Gui);
        #[cfg(feature = "tui")]
        assert_eq!(parse(&["tui"]).expect("tui"), Command::Tui);
        for version in ["--version", "-V", "version"] {
            assert_eq!(parse(&[version]).expect("version"), Command::Version);
        }
        for help in ["--help", "-h", "help"] {
            assert_eq!(parse(&[help]).expect("help"), Command::Help);
        }
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
