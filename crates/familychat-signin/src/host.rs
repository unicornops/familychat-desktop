//! Server names, parsed strictly as bare `hostname[:port]` (PLAN.md, section
//! 6.5; the contract's "Which servers a client may talk to").
//!
//! A server name comes from somewhere a person or a link controls: typed
//! into the sign-in form, the server half of a Matrix ID, a link's
//! `account_provider` or `hs`. It is never handed to a URL parser as it
//! arrives, because URL parsers disagree about `\`, `@`, `%` and the like
//! (the iOS fork's backslash bypass was exactly that: a parser that read
//! `evil.example\@x.safechat.family` as one host where the check saw
//! another). Instead [`ServerName::parse`] accepts only:
//!
//! - an optional leading `https://` (in any case), and nothing else that
//!   looks like a scheme;
//! - a DNS name of ASCII letters, digits, `-` and `.`: labels of 1 to 63
//!   characters that neither start nor end with `-`, at most 253
//!   characters in all, no empty label and no trailing dot;
//! - an optional `:port` of 1 to 5 digits, 1 to 65535, no leading zero.
//!
//! Everything else is refused, including `\`, `@`, `%`, `/`, `?`, `#`,
//! whitespace, control characters, non-ASCII text (look-alike letters), IP
//! literals in brackets and `_`. Letters are lower-cased, as DNS compares
//! them. The URL built from a name ([`ServerName::https_url`]) is checked
//! to round-trip to exactly the same host and port.

use std::fmt;

use reqwest::Url;

/// Why a server name was refused. The wording is the same for every reason
/// (the user sees [`crate::wording::INVALID_SERVER`]); the reason is for
/// tests and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostError {
    /// Nothing was given.
    Empty,
    /// A scheme other than a leading `https://`.
    Scheme,
    /// A character a bare host never has (`\`, `@`, `%`, `/`, `?`, `#`,
    /// whitespace, a control character, non-ASCII, `_`, brackets...).
    Character,
    /// An empty, too long or badly formed label, or a name too long.
    Label,
    /// A port that is not 1 to 65535 written plainly.
    Port,
    /// The URL built from the name did not come back as the same host.
    RoundTrip,
}

/// The longest DNS name.
const MAX_NAME: usize = 253;
/// The longest DNS label.
const MAX_LABEL: usize = 63;

/// A bare, lower-cased `hostname[:port]`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ServerName {
    host: String,
    port: Option<u16>,
}

impl ServerName {
    /// Parses `text` strictly (the module's rules). Surrounding whitespace
    /// is refused too: a form trims what was typed before parsing it, a
    /// link's parameters are taken as they are.
    pub fn parse(text: &str) -> Result<Self, HostError> {
        if text.is_empty() {
            return Err(HostError::Empty);
        }
        let rest = strip_https(text);
        if rest.is_empty() {
            return Err(HostError::Empty);
        }
        // Every character must be one a bare host or its port has. This
        // catches `://` of any other scheme (`/`), user info (`@`),
        // percent-encoding (`%`), paths, queries, fragments, backslashes,
        // whitespace, brackets and non-ASCII before anything is split.
        if !rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
        {
            return Err(if rest.contains("://") {
                HostError::Scheme
            } else {
                HostError::Character
            });
        }
        let (host, port) = match rest.split_once(':') {
            None => (rest, None),
            Some((host, port)) => (host, Some(parse_port(port)?)),
        };
        check_dns_name(host)?;
        let name = Self {
            host: host.to_ascii_lowercase(),
            port,
        };
        name.https_url()?;
        Ok(name)
    }

    /// The host, lower-cased.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port, when one was given.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// `https://host[:port]/`, checked to name exactly this host and port.
    pub fn https_url(&self) -> Result<Url, HostError> {
        let url = Url::parse(&format!("https://{self}/")).map_err(|_| HostError::RoundTrip)?;
        let same_host = matches!(url.host(), Some(url::Host::Domain(host)) if host == self.host);
        // `Url` drops the scheme's default port, so `:443` comes back as
        // no port at all.
        let same_port = url.port_or_known_default() == Some(self.port.unwrap_or(443));
        if !same_host
            || !same_port
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(HostError::RoundTrip);
        }
        Ok(url)
    }

    /// Whether the host is `suffix` itself or under it. `suffix` is a
    /// lower-case DNS name.
    pub(crate) fn host_is_under(&self, suffix: &str) -> bool {
        host_is_under(&self.host, suffix)
    }
}

/// Whether `host` (lower-case) is a strict subdomain of `suffix`: it ends in
/// `.suffix` with at least one label before it. The apex itself is not
/// under it.
pub(crate) fn host_is_under(host: &str, suffix: &str) -> bool {
    host.len() > suffix.len() + 1
        && host.ends_with(suffix)
        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
}

impl fmt::Display for ServerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.port {
            Some(port) => write!(f, "{}:{port}", self.host),
            None => f.write_str(&self.host),
        }
    }
}

impl fmt::Debug for ServerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerName({self})")
    }
}

/// `text` without a leading `https://` in any case.
fn strip_https(text: &str) -> &str {
    const PREFIX: &str = "https://";
    match text.get(..PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(PREFIX) => &text[PREFIX.len()..],
        _ => text,
    }
}

fn parse_port(port: &str) -> Result<u16, HostError> {
    if port.is_empty()
        || port.len() > 5
        || port.starts_with('0')
        || !port.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(HostError::Port);
    }
    port.parse::<u16>().map_err(|_| HostError::Port)
}

fn check_dns_name(host: &str) -> Result<(), HostError> {
    if host.is_empty() || host.len() > MAX_NAME {
        return Err(HostError::Label);
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > MAX_LABEL
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(HostError::Label);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> String {
        ServerName::parse(text)
            .unwrap_or_else(|error| panic!("{text:?} was refused: {error:?}"))
            .to_string()
    }

    #[test]
    fn accepts_bare_hosts_and_ports() {
        assert_eq!(parsed("smith.safechat.family"), "smith.safechat.family");
        assert_eq!(parsed("smith.ie"), "smith.ie");
        assert_eq!(parsed("https://smith.ie"), "smith.ie");
        assert_eq!(parsed("HTTPS://Smith.IE"), "smith.ie");
        assert_eq!(parsed("matrix.org:8448"), "matrix.org:8448");
        assert_eq!(parsed("localhost"), "localhost");
        assert_eq!(parsed("a-b.c-d.example"), "a-b.c-d.example");
        assert_eq!(parsed("xn--bcher-kva.example"), "xn--bcher-kva.example");
        assert_eq!(parsed("x.example:65535"), "x.example:65535");
        assert_eq!(parsed("x.example:1"), "x.example:1");
    }

    #[test]
    fn refuses_everything_else() {
        let cases: &[(&str, HostError)] = &[
            ("", HostError::Empty),
            ("https://", HostError::Empty),
            // The iOS review's backslash bypass, and its relatives.
            ("evil.example\\@smith.safechat.family", HostError::Character),
            ("evil.example\\.safechat.family", HostError::Character),
            ("smith.safechat.family\\", HostError::Character),
            ("user@smith.safechat.family", HostError::Character),
            ("smith.safechat.family@evil.example", HostError::Character),
            ("evil.example%2F.safechat.family", HostError::Character),
            ("smith%2Esafechat.family", HostError::Character),
            ("smith.safechat.family/", HostError::Character),
            ("smith.safechat.family/path", HostError::Character),
            ("smith.safechat.family?x=1", HostError::Character),
            ("smith.safechat.family#x", HostError::Character),
            ("evil.example#.safechat.family", HostError::Character),
            ("smith.safechat.family ", HostError::Character),
            (" smith.safechat.family", HostError::Character),
            ("smith .safechat.family", HostError::Character),
            ("smith.safechat.family\n", HostError::Character),
            ("smith.safechat.family\t", HostError::Character),
            ("smith.safechat.family\0", HostError::Character),
            ("smith\u{3002}safechat.family", HostError::Character),
            ("smіth.safechat.family", HostError::Character),
            ("büch.example", HostError::Character),
            ("under_score.example", HostError::Character),
            ("[::1]", HostError::Character),
            ("[::1]:8448", HostError::Character),
            ("http://smith.safechat.family", HostError::Scheme),
            ("ftp://smith.safechat.family", HostError::Scheme),
            ("https://https://smith.safechat.family", HostError::Scheme),
            ("//smith.safechat.family", HostError::Character),
            ("https:smith.safechat.family", HostError::Port),
            ("javascript:alert(1)", HostError::Character),
            ("smith.safechat.family.", HostError::Label),
            (".smith.safechat.family", HostError::Label),
            ("smith..safechat.family", HostError::Label),
            ("-smith.safechat.family", HostError::Label),
            ("smith-.safechat.family", HostError::Label),
            ("smith.safechat.family:", HostError::Port),
            ("smith.safechat.family:0", HostError::Port),
            ("smith.safechat.family:08448", HostError::Port),
            ("smith.safechat.family:65536", HostError::Port),
            ("smith.safechat.family:123456", HostError::Port),
            ("smith.safechat.family:84a8", HostError::Port),
            ("smith.safechat.family:1:2", HostError::Port),
            (":8448", HostError::Label),
        ];
        for (text, reason) in cases {
            assert_eq!(ServerName::parse(text), Err(*reason), "{text:?}");
        }
        let long_label = format!("{}.example", "a".repeat(64));
        assert_eq!(ServerName::parse(&long_label), Err(HostError::Label));
        let long_name = format!("{}example", "abc.".repeat(63));
        assert_eq!(ServerName::parse(&long_name), Err(HostError::Label));
    }

    #[test]
    fn a_numeric_name_that_a_url_parser_reads_as_an_address_is_refused() {
        // `Url` reads these as IPv4 addresses, so the round trip fails and
        // nothing is sent to an address the name did not spell.
        for text in ["127.0.0.1", "0x7f000001", "2130706433", "1.2.3.4:8448"] {
            assert_eq!(
                ServerName::parse(text),
                Err(HostError::RoundTrip),
                "{text:?}"
            );
        }
    }

    #[test]
    fn builds_the_https_url_it_names() {
        let name = ServerName::parse("Smith.Safechat.Family:8448").expect("parses");
        assert_eq!(
            name.https_url().expect("url").as_str(),
            "https://smith.safechat.family:8448/"
        );
        let name = ServerName::parse("smith.ie:443").expect("parses");
        assert_eq!(name.https_url().expect("url").as_str(), "https://smith.ie/");
    }

    #[test]
    fn subdomains_only() {
        assert!(host_is_under("smith.safechat.family", "safechat.family"));
        assert!(host_is_under("a.b.safechat.family", "safechat.family"));
        assert!(!host_is_under("safechat.family", "safechat.family"));
        assert!(!host_is_under("evilsafechat.family", "safechat.family"));
        assert!(!host_is_under(".safechat.family", "safechat.family"));
        assert!(!host_is_under(
            "safechat.family.evil.example",
            "safechat.family"
        ));
        assert!(!host_is_under("family", "safechat.family"));
    }
}
