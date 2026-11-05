//! Opening a link the person agreed to open (ADR 0008; ADR 0007, links).
//!
//! What a link does is `familychat-core`'s decision
//! ([`familychat_core::links::classify`]): Matrix links open in the app, web
//! and `mailto:` addresses ask first with the whole address, everything else
//! is refused. This module only hands an address the person confirmed to
//! the operating system's opener.
//!
//! [`open`] is adapted from Zmail's `zmail-tui` (`src/links.rs`), Copyright
//! UnicornOps, GPL-3.0-only like this project; keep this notice. It checks
//! the address again with `familychat-core`'s `external_target`.

use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use familychat_core::links::external_target;

/// Hands `url` to the operating system's opener: `open` on macOS, the URL
/// protocol handler on Windows, `xdg-open` elsewhere. Only an `https`,
/// `http` or `mailto` address opens; it runs without a shell.
pub fn open(url: &str) -> Result<()> {
    let url = external_target(url).context("only web and mail addresses open")?;
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    let mut child = command
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't start the system's link opener")?;
    // Reap it in the background; openers return quickly.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::open;

    #[test]
    fn only_web_and_mail_addresses_are_handed_on() {
        for refused in ["javascript:alert(1)", "file:///etc/passwd", "https://", "x"] {
            assert!(open(refused).is_err(), "{refused}");
        }
    }
}
