//! `familychat-launcher`: how Windows starts Family Chat from the Start
//! menu and from a `familychat:` link (ADR 0014).
//!
//! `familychat.exe` is a console program, so that `familychat tui`,
//! `familychat accounts` and `familychat --version` work in a terminal.
//! Started by Explorer or a browser, a console program gets a console
//! window of its own (a Windows Terminal window on Windows 11) for as long
//! as it runs, and closing that window ends Family Chat. The MSI's
//! shortcut and its `familychat` URL handler start this small
//! window-subsystem program instead, which starts `familychat.exe` from
//! its own directory with the same arguments and no console
//! (`CREATE_NO_WINDOW`), and exits. Everything else (the profile lock,
//! the hand-off of a link to a running window, the window) is
//! `familychat.exe`'s, unchanged.
//!
//! The arguments are passed on as they came and never printed or logged:
//! a link's code is in them. The launcher exists only for Windows; on
//! other systems it says so and exits.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

/// The program the launcher starts, next to it.
#[cfg_attr(not(windows), allow(dead_code))]
const APP_EXE: &str = "familychat.exe";

/// Where `familychat.exe` is, given the launcher's own path.
#[cfg_attr(not(windows), allow(dead_code))]
fn app_path(launcher: &Path) -> Option<PathBuf> {
    Some(launcher.parent()?.join(APP_EXE))
}

#[cfg(windows)]
fn main() -> ExitCode {
    use std::{
        ffi::OsString,
        os::windows::process::CommandExt as _,
        process::{Command, Stdio},
    };

    /// `CREATE_NO_WINDOW` from the Win32 process creation flags: the
    /// child gets no console window. (`std` takes the flags as a number;
    /// no Win32 binding is needed for it.)
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Some(app) = std::env::current_exe().ok().as_deref().and_then(app_path) else {
        return ExitCode::FAILURE;
    };
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // No console to report to: a failure to start is the exit code only.
    match Command::new(app)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("familychat-launcher is only used on Windows; run familychat instead.");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_the_app_next_to_itself() {
        let launcher = Path::new("install").join("familychat-launcher.exe");
        assert_eq!(
            app_path(&launcher),
            Some(Path::new("install").join("familychat.exe"))
        );
        assert_eq!(app_path(Path::new("")), None);
    }
}
