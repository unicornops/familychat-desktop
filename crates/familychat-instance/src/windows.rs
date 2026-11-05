//! The hand-off on Windows: a named pipe only the current user can open.
//!
//! Pipes live in one namespace for every user of the machine, so the pipe
//! is protected three ways:
//!
//! - its DACL grants only the current user's SID, and is protected (no
//!   inherited entries), so no other account can open it
//!   (`windows_security`);
//! - it is created as the name's first instance and refuses remote
//!   clients, so it can't join a pipe someone else made;
//! - its name is random (`family.safechat.desktop-<128 bits>`), written in
//!   the profile directory (`familychat.pipe`), which only the user can
//!   read, so another user can't take the name first and wait for a
//!   second launch to send them a link.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

use crate::{InstanceError, windows_security};

/// The file in the profile that names the pipe.
const NAME_FILE: &str = "familychat.pipe";

/// What every pipe name starts with.
const PIPE_PREFIX: &str = r"\\.\pipe\family.safechat.desktop-";

/// `ERROR_PIPE_BUSY`: every instance is in use; try again.
const ERROR_PIPE_BUSY: i32 = 231;

/// A profile's pipe-name file.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    name_file: PathBuf,
}

impl Endpoint {
    pub(crate) fn for_profile(profile: &Path) -> Result<Self, InstanceError> {
        Ok(Self {
            name_file: profile.join(NAME_FILE),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.name_file
    }
}

/// A new random pipe name.
fn new_name() -> Result<String, InstanceError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{PIPE_PREFIX}{hex}"))
}

/// The pipe name in the profile's file, if it is one of ours.
fn read_name(file: &Path) -> io::Result<String> {
    let name = fs::read_to_string(file)?;
    let name = name.trim();
    let valid = name
        .strip_prefix(PIPE_PREFIX)
        .is_some_and(|hex| hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()));
    if valid {
        Ok(name.to_owned())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a pipe name",
        ))
    }
}

/// Removes the name file when the window stops.
pub(crate) struct Cleanup {
    name_file: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.name_file);
    }
}

/// The pipe's next instance, waiting for a second launch.
pub(crate) struct Incoming {
    name: String,
    sddl: String,
    next: NamedPipeServer,
}

impl Incoming {
    pub(crate) async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        self.next.connect().await?;
        let fresh = create(&self.name, &self.sddl, false)?;
        Ok(std::mem::replace(&mut self.next, fresh))
    }

    /// The pipe's current instance (for the permission test).
    #[cfg(test)]
    pub(crate) fn server(&self) -> &NamedPipeServer {
        &self.next
    }
}

fn create(name: &str, sddl: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(crate::MAX_CONNECTIONS + 1);
    windows_security::create_pipe(&options, name, sddl)
}

pub(crate) fn listen(endpoint: &Endpoint) -> Result<(Incoming, Cleanup), InstanceError> {
    let sddl = windows_security::user_only_sddl()?;
    let name = new_name()?;
    let next = create(&name, &sddl, true)?;
    // Written once the pipe exists, through a temporary file, so a second
    // launch never reads half a name.
    let temporary = endpoint.name_file.with_extension("pipe.tmp");
    fs::write(&temporary, &name)?;
    fs::rename(&temporary, &endpoint.name_file)?;
    Ok((
        Incoming { name, sddl, next },
        Cleanup {
            name_file: endpoint.name_file.clone(),
        },
    ))
}

/// Why a second launch can't connect.
pub(crate) enum ConnectError {
    /// Nothing is listening (yet).
    NotYet,
    /// Never on Windows (the DACL is the server's to set), kept for the
    /// shape the Unix side shares.
    #[allow(dead_code)]
    Unsafe(InstanceError),
}

pub(crate) async fn connect(endpoint: &Endpoint) -> Result<NamedPipeClient, ConnectError> {
    let name = read_name(&endpoint.name_file).map_err(|_| ConnectError::NotYet)?;
    match ClientOptions::new().open(&name) {
        Ok(client) => Ok(client),
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => Err(ConnectError::NotYet),
        Err(_) => Err(ConnectError::NotYet),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_random_and_checked() {
        let first = new_name().expect("a name");
        let second = new_name().expect("a name");
        assert_ne!(first, second);
        assert!(first.starts_with(PIPE_PREFIX));
        let dir = std::env::temp_dir().join(format!("familychat-pipe-name-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a directory");
        let file = dir.join(NAME_FILE);
        fs::write(&file, &first).expect("written");
        assert_eq!(read_name(&file).expect("read"), first);
        fs::write(&file, r"\\.\pipe\someone-else").expect("written");
        assert!(read_name(&file).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
