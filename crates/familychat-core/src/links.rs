//! Links in messages (PLAN.md, section 5.8; ADR 0007): which ones open in
//! the app, which ones ask first, and what the question says.
//!
//! - A `matrix:` URI or a `https://matrix.to/#/` link names a user, a room
//!   or an event: [`classify`] answers [`LinkAction::Navigate`] and the
//!   frontend opens it in the app, with no request to anyone.
//! - An `https`, `http` or `mailto` address is [`LinkAction::Confirm`]: the
//!   frontend shows a [`LinkPrompt`] with the full address, and hands it to
//!   the operating system only after the person agrees. When the link's
//!   text looks like a web address on another site (the usual phishing
//!   trick), the prompt says so.
//! - Anything else (`javascript:`, `file:`, `data:`, relative addresses) is
//!   [`LinkAction::Refuse`]: the sanitiser already drops them from HTML,
//!   and this is the second check.
//!
//! [`written_urls`], `trim_trailing_punctuation`, [`LinkPrompt`] and the
//! look-alike check are adapted from Zmail's `zmail-ui`
//! (`src/reader/links.rs`).

use std::ops::Range;

use ruma::{
    MatrixToUri, MatrixUri, OwnedEventId, OwnedRoomOrAliasId, OwnedUserId, matrix_uri::MatrixId,
};
use url::Url;

/// A Matrix link: what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixLink {
    /// A user.
    User(OwnedUserId),
    /// A room, by id or alias.
    Room(OwnedRoomOrAliasId),
    /// An event in a room.
    Event {
        /// The room.
        room: OwnedRoomOrAliasId,
        /// The event.
        event: OwnedEventId,
    },
}

/// What opening a link does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAction {
    /// Open it in the app.
    Navigate(MatrixLink),
    /// Ask first, showing the full address.
    Confirm(LinkPrompt),
    /// Never open it.
    Refuse,
}

/// What opening the link `href`, whose text is `text`, does.
pub fn classify(href: &str, text: &str) -> LinkAction {
    let href = href.trim();
    if let Some(link) = matrix_link(href) {
        return LinkAction::Navigate(link);
    }
    match external_target(href) {
        Some(target) => LinkAction::Confirm(LinkPrompt::new(text, &target)),
        None => LinkAction::Refuse,
    }
}

/// The Matrix id a `matrix:` URI or `matrix.to` link names.
pub fn matrix_link(href: &str) -> Option<MatrixLink> {
    let lower = href.to_ascii_lowercase();
    let id = if lower.starts_with("matrix:") {
        MatrixUri::parse(href).ok()?.id().clone()
    } else if lower.starts_with("https://matrix.to/#/") {
        MatrixToUri::parse(href).ok()?.id().clone()
    } else {
        return None;
    };
    Some(match id {
        MatrixId::User(user) => MatrixLink::User(user),
        MatrixId::Room(room) => MatrixLink::Room(room.into()),
        MatrixId::RoomAlias(alias) => MatrixLink::Room(alias.into()),
        MatrixId::Event(room, event) => MatrixLink::Event { room, event },
        _ => return None,
    })
}

/// The address to hand to the operating system for a link: `http` and
/// `https` with a host, and `mailto` with an address. Normalised by the URL
/// parser (lowercase scheme and host, punycode).
pub fn external_target(href: &str) -> Option<String> {
    let url = Url::parse(href.trim()).ok()?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Some(url.to_string()),
        "mailto" if !url.path().is_empty() => Some(url.to_string()),
        _ => None,
    }
}

/// The ranges of the web and `mailto:` addresses written in `text`.
pub fn written_urls(text: &str) -> Vec<Range<usize>> {
    const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];
    let lower = text.to_ascii_lowercase();
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((start, scheme)) = SCHEMES
            .iter()
            .filter_map(|scheme| lower[cursor..].find(scheme).map(|at| (cursor + at, scheme)))
            .min_by_key(|(start, _)| *start)
        else {
            break;
        };
        // "xhttps://" is not an address.
        let after_word = text[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_alphanumeric());
        let end = start
            + text[start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '`'))
                .unwrap_or(text.len() - start);
        let end = start + trim_trailing_punctuation(&text[start..end]).len();
        if after_word && end > start + scheme.len() + 1 {
            ranges.push(start..end);
        }
        cursor = end.max(start + scheme.len());
    }
    ranges
}

/// Drops sentence punctuation after an address ("see https://x.org.") and
/// a closing bracket that has no opening one inside the address.
fn trim_trailing_punctuation(url: &str) -> &str {
    let mut url = url;
    loop {
        let Some(last) = url.chars().next_back() else {
            return url;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            return url;
        }
    }
}

/// What the open-link dialog shows about a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPrompt {
    /// The full target address, shown whole before the choice.
    pub target: String,
    /// The target's host (`None` for `mailto:`).
    pub host: Option<String>,
    /// The site the link's text shows, when it differs from the target's.
    pub shown_host: Option<String>,
}

impl LinkPrompt {
    /// The dialog for a link whose text is `text` and target `target`.
    pub fn new(text: &str, target: &str) -> Self {
        let host = Url::parse(target)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        let shown_host = text_host(text).filter(|shown| match &host {
            Some(host) => site(shown) != site(host),
            // Text that looks like a website on a mailto: link.
            None => true,
        });
        Self {
            target: target.to_owned(),
            host,
            shown_host,
        }
    }

    /// Whether the link's text shows a different site from its target.
    pub fn is_mismatched(&self) -> bool {
        self.shown_host.is_some()
    }

    /// Whether it writes an email rather than opening a page.
    pub fn is_mailto(&self) -> bool {
        self.host.is_none()
    }

    /// The dialog's title.
    pub fn title(&self) -> &'static str {
        if self.is_mailto() {
            "Write to this address?"
        } else {
            "Open this link?"
        }
    }

    /// The sentence under the title.
    pub fn description(&self) -> String {
        match &self.host {
            Some(host) => format!("It goes to {host} and opens in your browser."),
            None => "It opens in your email app.".to_owned(),
        }
    }

    /// The warning when the text shows another site: its heading and its
    /// sentence.
    pub fn warning(&self) -> Option<(&'static str, String)> {
        let shown = self.shown_host.as_ref()?;
        let goes_to = self
            .host
            .as_deref()
            .map_or_else(|| "an email address".to_owned(), str::to_owned);
        Some((
            "This link is not what it shows",
            format!("The link's text shows {shown}, but it goes to {goes_to}."),
        ))
    }

    /// The button that opens it.
    pub fn open_label(&self) -> &'static str {
        if self.is_mailto() {
            "Write Email"
        } else {
            "Open Link"
        }
    }
}

/// A host compared without a leading "www.", in lowercase.
fn site(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    host.strip_prefix("www.").unwrap_or(&host).to_owned()
}

/// The host a link's text looks like it names: "https://bank.example/x",
/// "www.bank.example" or "bank.example". `None` for ordinary words.
fn text_host(text: &str) -> Option<String> {
    let text = text
        .trim()
        .trim_matches(|ch| matches!(ch, '<' | '>' | '(' | ')' | '"'));
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let candidate = if lower.starts_with("http://") || lower.starts_with("https://") {
        text.to_owned()
    } else if lower.starts_with("mailto:") || text.contains('@') {
        return None;
    } else {
        // "bank.example" or "www.bank.example/login": a dotted name with
        // a letters-only top-level domain.
        let host = text.split(['/', '?', '#']).next().unwrap_or_default();
        let tld = host.rsplit('.').next().unwrap_or_default();
        let dotted = host.contains('.')
            && tld.len() >= 2
            && tld.chars().all(|ch| ch.is_ascii_alphabetic())
            && host
                .chars()
                .all(|ch| ch.is_alphanumeric() || ch == '.' || ch == '-');
        if !dotted {
            return None;
        }
        format!("https://{text}")
    };
    Url::parse(&candidate)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(body: &str) -> Vec<&str> {
        written_urls(body)
            .into_iter()
            .map(|range| &body[range])
            .collect()
    }

    #[test]
    fn written_addresses_are_links_without_trailing_punctuation() {
        let body = "See https://example.com/changelog. Or (http://example.org/a_(b)), \
                    mailto:help@example.com, and xhttps://no.example.";
        assert_eq!(
            texts(body),
            [
                "https://example.com/changelog",
                "http://example.org/a_(b)",
                "mailto:help@example.com"
            ]
        );
    }

    #[test]
    fn matrix_links_navigate_in_the_app() {
        assert_eq!(
            classify("https://matrix.to/#/@ana:smith.safechat.family", "Ana"),
            LinkAction::Navigate(MatrixLink::User(
                "@ana:smith.safechat.family".try_into().expect("a user id")
            ))
        );
        assert!(matches!(
            classify("matrix:r/family:smith.safechat.family", "#family"),
            LinkAction::Navigate(MatrixLink::Room(_))
        ));
        assert!(matches!(
            classify(
                "https://matrix.to/#/!room:smith.safechat.family/$event?via=smith.safechat.family",
                "this"
            ),
            LinkAction::Navigate(MatrixLink::Event { .. })
        ));
        assert!(matches!(
            classify("matrix:roomid/room:example.org", "x"),
            LinkAction::Navigate(MatrixLink::Room(_))
        ));
        // A matrix.to look-alike on another host is an ordinary link.
        assert!(matches!(
            classify("https://matrix.to.evil.example/#/@ana:x.org", "Ana"),
            LinkAction::Confirm(_)
        ));
        // A broken Matrix link is not opened anywhere.
        assert_eq!(classify("matrix:nonsense", "x"), LinkAction::Refuse);
    }

    #[test]
    fn web_and_mail_links_ask_and_everything_else_is_refused() {
        let LinkAction::Confirm(prompt) = classify("https://example.org/x?a=1", "here") else {
            panic!("a web link asks");
        };
        assert_eq!(prompt.target, "https://example.org/x?a=1");
        assert_eq!(prompt.title(), "Open this link?");
        assert_eq!(
            prompt.description(),
            "It goes to example.org and opens in your browser."
        );
        let LinkAction::Confirm(mail) = classify("mailto:help@example.com", "help") else {
            panic!("a mail link asks");
        };
        assert!(mail.is_mailto());
        assert_eq!(mail.open_label(), "Write Email");
        for href in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "#top",
            "/relative",
            "vbscript:x",
            "https://",
        ] {
            assert_eq!(classify(href, "x"), LinkAction::Refuse, "{href}");
        }
    }

    #[test]
    fn a_link_showing_another_site_is_mismatched() {
        let prompt = LinkPrompt::new("https://bank.example/login", "https://evil.example/x");
        assert_eq!(prompt.host.as_deref(), Some("evil.example"));
        assert_eq!(prompt.shown_host.as_deref(), Some("bank.example"));
        assert!(
            prompt
                .warning()
                .is_some_and(|(_, sentence)| sentence.contains("evil.example"))
        );

        // The userinfo trick: the host is what follows the @.
        let prompt = LinkPrompt::new("bank.example", "https://bank.example@evil.example/");
        assert!(prompt.is_mismatched());
        assert_eq!(prompt.host.as_deref(), Some("evil.example"));

        for (text, target) in [
            ("Pay your bill", "https://evil.example/x"),
            ("www.bank.example", "https://bank.example/login"),
            (
                "https://example.com/changelog",
                "https://example.com/changelog",
            ),
            ("help@example.com", "mailto:help@example.com"),
            ("v1.2", "https://example.com/"),
        ] {
            assert!(!LinkPrompt::new(text, target).is_mismatched(), "{text}");
        }
    }
}
