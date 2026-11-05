//! The single-instance hand-off (PLAN.md, section 5.5; ADR 0013).
//!
//! One process opens a profile at a time (its lock, ADR 0003). The window
//! that holds it also listens on the profile's [`Endpoint`], a local
//! socket only its user can reach:
//!
//! - **Unix**: a Unix socket inside a directory only the user can enter
//!   (0700): `$XDG_RUNTIME_DIR/familychat/` on Linux (the profile directory
//!   when there is no runtime directory), the per-user temporary directory
//!   (`$TMPDIR/familychat/`) on macOS, named after the profile's path. The
//!   socket itself is 0600; both sides check the directory is private and
//!   that the socket belongs to the directory's owner.
//! - **Windows**: a named pipe whose DACL grants only the current user
//!   (protected, no inherited entries), created as the name's first
//!   instance and refusing remote clients, under a random name written in
//!   the profile directory (which only the user can read), so another user
//!   can neither reach the pipe nor take its name first.
//!
//! A second launch ([`forward`]) sends its request (a sign-in link, or
//! "come forward") and waits for the reply; the running window
//! ([`listen`]) checks it strictly (`wire`), hands it to the window and
//! answers. A link is a bearer credential: nothing here logs it, prints
//! it or keeps it, and its buffers are wiped.

#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::{
    fmt, io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use familychat_core::requests::AppRequest;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Handle,
    sync::Semaphore,
    task::JoinHandle,
};
use tracing::{debug, info, warn};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_security;
pub mod wire;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub use wire::{MAX_FRAME_BYTES, READ_TIMEOUT, WireError};

/// How many second launches the window serves at once; more wait their
/// turn (each is answered or dropped within [`READ_TIMEOUT`]).
const MAX_CONNECTIONS: usize = 4;

/// How often a second launch tries to reach a window that is starting.
const CONNECT_RETRY: Duration = Duration::from_millis(100);

/// Hands a request to the window; `true` when it took it.
pub type Deliver = Arc<dyn Fn(AppRequest) -> bool + Send + Sync>;

/// Why the hand-off could not be set up or used.
#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    /// The place the endpoint would go is not private to this user.
    #[error("the hand-off's directory is not private: {0}")]
    NotPrivate(String),
    /// The endpoint's path is too long for a Unix socket.
    #[error("the hand-off's socket path is too long")]
    PathTooLong,
    /// The OS refused.
    #[error("the hand-off failed: {0}")]
    Io(#[from] io::Error),
}

/// Why a second launch's request didn't reach the window.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// Nothing listens (the terminal UI has the profile, or the window is
    /// from before the hand-off, or it is still starting).
    #[error("no Family Chat window is listening for this profile")]
    NotListening,
    /// The endpoint is not private to this user: nothing was sent.
    #[error("{0}")]
    Unsafe(InstanceError),
    /// The window answered no (it didn't accept the link).
    #[error("the Family Chat window didn't accept the request")]
    Refused,
    /// The conversation failed part-way.
    #[error("the Family Chat window didn't answer: {0}")]
    Wire(#[from] WireError),
}

/// Where a profile's running window listens.
#[derive(Clone)]
pub struct Endpoint {
    inner: platform::Endpoint,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The path names nobody, but on Windows the pipe's name is the
        // profile's secret.
        f.write_str("Endpoint")
    }
}

impl Endpoint {
    /// The endpoint of the profile in `profile` (its data directory, which
    /// must exist).
    pub fn for_profile(profile: &Path) -> Result<Self, InstanceError> {
        Ok(Self {
            inner: platform::Endpoint::for_profile(profile)?,
        })
    }

    /// Where it lives: the socket on Unix, the file holding the pipe's
    /// name on Windows (for tests and diagnostics).
    pub fn path(&self) -> &Path {
        self.inner.path()
    }
}

/// The window's listener; stops (and removes its socket) when dropped.
pub struct Listener {
    task: JoinHandle<()>,
    _cleanup: platform::Cleanup,
}

impl fmt::Debug for Listener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Listener")
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts listening on `endpoint`, on `runtime`, handing each accepted
/// request to `deliver`. Call it while holding the profile's lock: any
/// endpoint left from before (a crash) is replaced.
pub fn listen(
    endpoint: &Endpoint,
    runtime: &Handle,
    deliver: Deliver,
) -> Result<Listener, InstanceError> {
    let _entered = runtime.enter();
    let (incoming, cleanup) = platform::listen(&endpoint.inner)?;
    let task = runtime.spawn(accept_loop(incoming, deliver));
    info!("listening for second launches");
    Ok(Listener {
        task,
        _cleanup: cleanup,
    })
}

/// What a connection came to (for the logs: no text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// The request was handed to the window.
    Delivered,
    /// The window didn't take it.
    Declined,
    /// The message was refused.
    Refused(WireError),
}

async fn accept_loop(mut incoming: platform::Incoming, deliver: Deliver) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = match incoming.accept().await {
            Ok(stream) => stream,
            Err(error) => {
                warn!(%error, "a second launch could not connect");
                tokio::time::sleep(CONNECT_RETRY).await;
                continue;
            }
        };
        let Ok(permit) = permits.clone().acquire_owned().await else {
            return;
        };
        let deliver = deliver.clone();
        tokio::spawn(async move {
            let served = serve(stream, &deliver).await;
            debug!(served = ?served, "served a second launch");
            drop(permit);
        });
    }
}

/// Reads one request from `stream`, delivers it and answers.
pub(crate) async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    deliver: &Deliver,
) -> Served {
    let served = match wire::read_frame(&mut stream).await {
        Ok(json) => {
            let decoded = wire::decode_request(&json);
            wire::wipe(json);
            match decoded {
                Ok(request) => {
                    if deliver(request) {
                        Served::Delivered
                    } else {
                        Served::Declined
                    }
                }
                Err(error) => Served::Refused(error),
            }
        }
        Err(error) => Served::Refused(error),
    };
    if !matches!(
        served,
        Served::Refused(WireError::Closed | WireError::TimedOut)
    ) {
        let reply = wire::encode_reply(served == Served::Delivered);
        let _ = wire::write_all(&mut stream, &reply).await;
    }
    served
}

/// From a second launch: sends `request` to the window listening on
/// `endpoint`, trying for up to `wait` while the window starts. Blocks;
/// runs its own small runtime.
pub fn forward(
    endpoint: &Endpoint,
    request: AppRequest,
    wait: Duration,
) -> Result<(), ForwardError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ForwardError::NotListening)?;
    runtime.block_on(forward_async(endpoint, request, wait))
}

async fn forward_async(
    endpoint: &Endpoint,
    request: AppRequest,
    wait: Duration,
) -> Result<(), ForwardError> {
    let deadline = Instant::now() + wait;
    let mut stream = loop {
        match platform::connect(&endpoint.inner).await {
            Ok(stream) => break stream,
            Err(platform::ConnectError::Unsafe(error)) => return Err(ForwardError::Unsafe(error)),
            Err(platform::ConnectError::NotYet) if Instant::now() < deadline => {
                tokio::time::sleep(CONNECT_RETRY).await;
            }
            Err(platform::ConnectError::NotYet) => return Err(ForwardError::NotListening),
        }
    };
    let framed = wire::encode_request(&request);
    drop(request);
    let sent = wire::write_all(&mut stream, &framed).await;
    wire::wipe(framed);
    sent.map_err(|_| ForwardError::Wire(WireError::Closed))?;
    let reply = wire::read_frame(&mut stream).await?;
    if wire::decode_reply(&reply)? {
        Ok(())
    } else {
        Err(ForwardError::Refused)
    }
}

#[cfg(test)]
mod tests;
