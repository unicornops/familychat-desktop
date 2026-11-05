//! What reaches the running window from outside it (PLAN.md, section 5.5;
//! ADR 0013): a sign-in link from a second launch (forwarded over the
//! user-only local socket or named pipe), from macOS (an Apple Event,
//! through GPUI's `on_open_urls`), or from the first launch's own command
//! line; or a second launch that only asks the window to come forward.
//!
//! The window starts a sign-in with the link, so every rule of the
//! contract applies to it and the confirmation always comes first: a
//! request never redeems anything by itself.

use std::fmt;

use tokio::sync::mpsc;

use crate::{identity::URL_SCHEME, signin::SecretText};

/// How many requests wait for the window before more are dropped.
pub const REQUEST_BUFFER: usize = 8;

/// The longest link a request carries. The panel's links are a few
/// hundred bytes.
pub const MAX_LINK_BYTES: usize = 4096;

/// A request for the running window.
pub enum AppRequest {
    /// Start a sign-in with this link (it may hold a code).
    OpenLink(SecretText),
    /// Come forward.
    Activate,
}

impl fmt::Debug for AppRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenLink(_) => f.write_str("OpenLink([redacted])"),
            Self::Activate => f.write_str("Activate"),
        }
    }
}

/// The sending half of the window's requests.
pub type RequestSender = mpsc::Sender<AppRequest>;
/// The receiving half, which the window drains.
pub type RequestReceiver = mpsc::Receiver<AppRequest>;

/// A channel for the window's requests, holding up to [`REQUEST_BUFFER`].
pub fn request_channel() -> (RequestSender, RequestReceiver) {
    mpsc::channel(REQUEST_BUFFER)
}

/// Whether `text` is shaped like a sign-in link: `familychat:` or
/// `https://safechat.family/`, at most [`MAX_LINK_BYTES`], with no
/// control characters. Only a first filter (what a command line or the OS
/// hands over may be anything); `familychat-signin`'s parser decides.
pub fn looks_like_signin_link(text: &str) -> bool {
    let text = text.trim();
    let lower = text.get(..32).unwrap_or(text).to_ascii_lowercase();
    text.len() <= MAX_LINK_BYTES
        && !text.chars().any(char::is_control)
        && (lower.starts_with(&format!("{URL_SCHEME}:"))
            || lower.starts_with("https://safechat.family/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_shapes() {
        for link in [
            "familychat://safechat.family/app/login?account_provider=smith.safechat.family",
            "  https://safechat.family/app/login?account_provider=x ",
            "FamilyChat://safechat.family/app/login",
        ] {
            assert!(looks_like_signin_link(link), "{link}");
        }
        for text in [
            "",
            "file:///etc/passwd",
            "https://evil.example/app/login",
            "https://safechat.family.evil.example/app/login",
            "familychat://safechat.family/app/login?token=a\nb",
            "--version",
        ] {
            assert!(!looks_like_signin_link(text), "{text:?}");
        }
        let long = format!(
            "familychat://safechat.family/app/login?token={}",
            "a".repeat(MAX_LINK_BYTES)
        );
        assert!(!looks_like_signin_link(&long));
    }

    #[test]
    fn debug_hides_the_link() {
        let request = AppRequest::OpenLink(SecretText::from("familychat://x?token=secret"));
        assert!(!format!("{request:?}").contains("secret"));
    }
}
