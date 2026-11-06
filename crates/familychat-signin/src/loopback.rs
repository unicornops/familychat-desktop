//! The loopback redirect a browser sign-in comes back to (RFC 8252
//! section 7.3; ADR 0010): a listener on `127.0.0.1` on a port the OS
//! picks, for one sign-in.
//!
//! It is the app's own, not matrix-sdk's `local-server` (axum), and
//! stricter than it: only `GET /callback` with the sign-in's own `state`
//! ends the wait. Anything else (another path, another method, a request
//! from another program on this computer without the state) gets an error
//! page and the wait goes on, so nothing local can end or steer a sign-in
//! it did not start. Each request is read within a size cap and a timeout,
//! and the page the browser shows loads nothing.
//!
//! The query that comes back (an OAuth code, or a single sign-on login
//! token) is a [`Secret`] from the moment it is read.

use std::{net::SocketAddr, time::Duration};

use familychat_security::Secret;
use reqwest::Url;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};
use tracing::{info, warn};

/// The redirect's path.
pub const CALLBACK_PATH: &str = "/callback";
/// The most of a request the listener reads.
const MAX_REQUEST: usize = 16 * 1024;
/// How long one connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What the browser shows once it is back.
const DONE_PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Family Chat</title></head><body style=\"font-family: system-ui, sans-serif; margin: 3em;\"><h1>You can go back to Family Chat</h1><p>You can close this tab now.</p></body></html>";
/// What anything else gets.
const WRONG_PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Family Chat</title></head><body style=\"font-family: system-ui, sans-serif; margin: 3em;\"><h1>This isn't the sign-in Family Chat is waiting for</h1></body></html>";

/// One sign-in's loopback listener; dropping it closes the port.
#[derive(Debug)]
pub struct Loopback {
    listener: TcpListener,
    address: SocketAddr,
}

/// What came back to the redirect: its query's pairs.
pub struct Callback(Vec<(String, Secret<String>)>);

impl Callback {
    /// The value of `name`, if the redirect carried it once.
    pub fn get(&self, name: &str) -> Option<&Secret<String>> {
        let mut found = self.0.iter().filter(|(key, _)| key == name);
        match (found.next(), found.next()) {
            (Some((_, value)), None) => Some(value),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self.0.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_tuple("Callback").field(&names).finish()
    }
}

impl Loopback {
    /// Listens on `127.0.0.1` on a port the OS picks.
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        Ok(Self { listener, address })
    }

    /// The redirect address to register and send: `http://127.0.0.1:<port>/callback`.
    pub fn redirect_uri(&self) -> Url {
        let mut url = Url::parse("http://127.0.0.1/").expect("a constant URL");
        let _ = url.set_port(Some(self.address.port()));
        url.set_path(CALLBACK_PATH);
        url
    }

    /// Waits for the browser to come back with `state`, answering anything
    /// else with an error page.
    pub async fn wait(&self, state: &Secret<String>) -> std::io::Result<Callback> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            match answer(stream, state).await {
                Ok(Some(callback)) => {
                    info!("the browser came back to the sign-in");
                    return Ok(callback);
                }
                Ok(None) => {}
                Err(error) => {
                    info!(%error, "a connection to the loopback redirect failed");
                }
            }
        }
    }
}

/// Reads one request and answers it: `Some` when it is this sign-in's
/// redirect.
async fn answer(
    mut stream: TcpStream,
    state: &Secret<String>,
) -> std::io::Result<Option<Callback>> {
    let head = match tokio::time::timeout(READ_TIMEOUT, read_head(&mut stream)).await {
        Ok(head) => head?,
        Err(_) => return Ok(None),
    };
    let callback = head.as_ref().and_then(|head| parse_callback(head.expose()));
    let ours = callback
        .as_ref()
        .and_then(|callback| callback.get("state"))
        .is_some_and(|got| constant_time_eq(got.expose().as_bytes(), state.expose().as_bytes()));
    let (status, page) = if ours {
        ("200 OK", DONE_PAGE)
    } else {
        warn!("a request reached the loopback redirect without this sign-in's state");
        ("400 Bad Request", WRONG_PAGE)
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    stream.write_all(response.as_bytes()).await?;
    let _ = stream.shutdown().await;
    Ok(if ours { callback } else { None })
}

/// The request line and headers, up to the blank line, within the cap.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<Secret<String>>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > MAX_REQUEST {
            return Ok(None);
        }
    }
    let head = Secret::new(buffer);
    Ok(std::str::from_utf8(head.expose())
        .ok()
        .map(|text| Secret::new(text.to_owned())))
}

/// `GET /callback?…`'s query pairs; `None` for any other request.
fn parse_callback(head: &str) -> Option<Callback> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method != "GET" || !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return None;
    }
    Some(Callback(
        form_urlencoded::parse(query.as_bytes())
            .map(|(name, value)| (name.into_owned(), Secret::new(value.into_owned())))
            .collect(),
    ))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(port: u16, text: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connects");
        stream.write_all(text.as_bytes()).await.expect("writes");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.expect("reads");
        answer
    }

    #[tokio::test]
    async fn only_this_sign_ins_redirect_ends_the_wait() {
        let loopback = Loopback::bind().await.expect("binds");
        let redirect = loopback.redirect_uri();
        assert_eq!(redirect.scheme(), "http");
        assert_eq!(redirect.host_str(), Some("127.0.0.1"));
        assert_eq!(redirect.path(), CALLBACK_PATH);
        let port = redirect.port().expect("a port");
        let state = Secret::new("the-state".to_owned());
        let waiting = tokio::spawn(async move { loopback.wait(&state).await });

        for wrong in [
            "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n",
            "POST /callback?state=the-state&code=c HTTP/1.1\r\n\r\n",
            "GET /callback?code=c HTTP/1.1\r\n\r\n",
            "GET /callback?state=another&code=c HTTP/1.1\r\n\r\n",
            "GET /callback?state=the-state&state=the-state&code=c HTTP/1.1\r\n\r\n",
            "GET /callbackx?state=the-state HTTP/1.1\r\n\r\n",
            "nonsense\r\n\r\n",
        ] {
            let answer = request(port, wrong).await;
            assert!(answer.starts_with("HTTP/1.1 400"), "{wrong}: {answer}");
            assert!(!waiting.is_finished(), "{wrong} ended the wait");
        }
        let answer = request(
            port,
            "GET /callback?code=the%20code&state=the-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.contains("Content-Security-Policy: default-src 'none'"));
        let callback = waiting.await.expect("joins").expect("came back");
        assert_eq!(
            callback.get("code").map(|code| code.expose().as_str()),
            Some("the code")
        );
        assert!(!format!("{callback:?}").contains("the code"));
    }

    #[tokio::test]
    async fn a_request_that_is_too_large_is_refused() {
        let loopback = Loopback::bind().await.expect("binds");
        let port = loopback.redirect_uri().port().expect("a port");
        let state = Secret::new("s".to_owned());
        let waiting = tokio::spawn(async move { loopback.wait(&state).await });
        let huge = format!(
            "GET /callback?state=s&x={} HTTP/1.1\r\n\r\n",
            "a".repeat(40_000)
        );
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connects");
        let _ = stream.write_all(huge.as_bytes()).await;
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer).await;
        assert!(
            answer.is_empty() || answer.starts_with("HTTP/1.1 400"),
            "{answer}"
        );
        assert!(!waiting.is_finished());
        waiting.abort();
    }
}
