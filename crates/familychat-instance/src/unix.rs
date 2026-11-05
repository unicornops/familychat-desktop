//! The hand-off on Unix: a Unix socket in a directory only the user can
//! enter.
//!
//! The directory is `$XDG_RUNTIME_DIR/familychat/` (Linux and the other
//! Unix systems; the runtime directory is the user's own, 0700, by the
//! XDG specification), `$TMPDIR/familychat/` on macOS (the per-user
//! temporary directory under `/var/folders`), or the profile directory
//! itself when there is no runtime directory. The socket is named after
//! the profile's path, so each profile has its own.
//!
//! The window makes the directory 0700 (and refuses one it doesn't own,
//! or whose permissions it can't narrow), replaces a socket left from a
//! crash (it holds the profile's lock, so nothing else can be listening),
//! binds, makes the socket 0600 and checks that the socket and the
//! directory have the same owner. A second launch checks the same before
//! it sends anything.

use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use tokio::net::{UnixListener, UnixStream};

use crate::InstanceError;

/// The directory's name inside the runtime directory.
const DIR_NAME: &str = "familychat";

/// The socket's name when it lives in the profile directory.
const PROFILE_SOCKET: &str = "familychat.sock";

/// Unix socket paths are limited to about 104 bytes (macOS) or 108
/// (Linux); leave room.
const MAX_SOCKET_PATH: usize = 100;

/// A profile's socket and the directory it is in.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    dir: PathBuf,
    socket: PathBuf,
}

impl Endpoint {
    pub(crate) fn for_profile(profile: &Path) -> Result<Self, InstanceError> {
        let profile = fs::canonicalize(profile)?;
        let (dir, socket) = match runtime_base() {
            Some(base) => {
                let dir = base.join(DIR_NAME);
                let name = format!(
                    "{:016x}.sock",
                    fnv1a(profile.as_os_str().as_encoded_bytes())
                );
                let socket = dir.join(name);
                (dir, socket)
            }
            None => (profile.clone(), profile.join(PROFILE_SOCKET)),
        };
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(InstanceError::PathTooLong);
        }
        Ok(Self { dir, socket })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.socket
    }

    /// The directory the socket is in (for the tests).
    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// A socket in a directory of the test's own (the tests loosen its
    /// permissions, which they must never do to the shared one).
    #[cfg(test)]
    pub(crate) fn in_dir(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            socket: dir.join(PROFILE_SOCKET),
        }
    }
}

/// Where the user's runtime files go, if anywhere.
fn runtime_base() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(std::env::temp_dir())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute() && dir.is_dir())
    }
}

/// FNV-1a, 64 bits: a stable name for a path (the same in every build, so
/// an old window and a new launch agree). Not a secret.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The socket a second launch must not reuse.
pub(crate) struct Cleanup {
    socket: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

/// The listening socket.
pub(crate) struct Incoming {
    listener: UnixListener,
}

impl Incoming {
    pub(crate) async fn accept(&mut self) -> io::Result<UnixStream> {
        self.listener.accept().await.map(|(stream, _)| stream)
    }
}

/// Makes `dir` exist with mode 0700 (narrowing it if it is ours).
fn private_dir(dir: &Path) -> Result<(), InstanceError> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() {
        return Err(InstanceError::NotPrivate(
            "it is not a directory".to_owned(),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        // Fails unless the directory is ours.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| {
            InstanceError::NotPrivate("its permissions can't be narrowed".to_owned())
        })?;
    }
    Ok(())
}

/// Checks the directory is private and the socket is its owner's, owner
/// only.
fn check_private(dir: &Path, socket: &Path) -> Result<(), InstanceError> {
    // A missing socket first: nothing listens (yet), whatever the
    // directory is like.
    let socket = fs::symlink_metadata(socket)?;
    let dir = fs::symlink_metadata(dir)?;
    if !dir.file_type().is_dir() || dir.mode() & 0o077 != 0 {
        return Err(InstanceError::NotPrivate(
            "other users can enter its directory".to_owned(),
        ));
    }
    if socket.uid() != dir.uid() {
        return Err(InstanceError::NotPrivate(
            "the socket belongs to someone else".to_owned(),
        ));
    }
    if socket.mode() & 0o077 != 0 {
        return Err(InstanceError::NotPrivate(
            "other users can use the socket".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn listen(endpoint: &Endpoint) -> Result<(Incoming, Cleanup), InstanceError> {
    private_dir(&endpoint.dir)?;
    // A socket left by a crash: the profile's lock says nothing listens.
    match fs::remove_file(&endpoint.socket) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&endpoint.socket)?;
    let cleanup = Cleanup {
        socket: endpoint.socket.clone(),
    };
    fs::set_permissions(&endpoint.socket, fs::Permissions::from_mode(0o600))?;
    check_private(&endpoint.dir, &endpoint.socket)?;
    Ok((Incoming { listener }, cleanup))
}

/// Why a second launch can't connect.
pub(crate) enum ConnectError {
    /// Nothing is listening (yet).
    NotYet,
    /// The endpoint isn't private: nothing may be sent.
    Unsafe(InstanceError),
}

pub(crate) async fn connect(endpoint: &Endpoint) -> Result<UnixStream, ConnectError> {
    match check_private(&endpoint.dir, &endpoint.socket) {
        Ok(()) => {}
        Err(InstanceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ConnectError::NotYet);
        }
        Err(error) => return Err(ConnectError::Unsafe(error)),
    }
    UnixStream::connect(&endpoint.socket)
        .await
        .map_err(|_| ConnectError::NotYet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a(b"/profile/one"), fnv1a(b"/profile/two"));
    }
}
