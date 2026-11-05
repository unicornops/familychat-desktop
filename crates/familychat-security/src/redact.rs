//! Log redaction (PLAN.md, section 5.4): no tokens, message bodies, query
//! strings or keys in any log line.
//!
//! Two layers:
//!
//! - **At the call site**, values that must never be logged are wrapped:
//!   secrets are [`Secret`](crate::Secret)s, whose `Debug` prints
//!   `[redacted]`, and anything else private (a message body, a room
//!   name) goes in [`Redacted`], whose `Debug` and `Display` print the same.
//! - **At the writer**, every formatted line passes through [`scrub`]
//!   before it reaches the log file ([`crate::logging`]), which removes what
//!   slips through: URL query strings, fragments and user info, the whole
//!   of a `familychat://` link, `Bearer` credentials, the values of
//!   token-, password- and key-named fields, and Matrix access and refresh
//!   tokens by their prefixes.

use std::fmt;

use crate::secret::REDACTED;

/// A value that is never printed: `Debug` and `Display` both print
/// `[redacted]`. For private values that are not secrets, such as message
/// bodies, so a log call can name the field without leaking it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Redacted<T>(pub T);

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Field and parameter names whose values are redacted wherever they
/// appear as `name=value`, `name: value` or `"name": "value"`. Matched
/// without regard to case, `-` or `_`, and as the end of a longer name
/// (`x-access-token`, `m.login.token`'s `token`).
const SECRET_NAMES: &[&str] = &[
    "accesstoken",
    "refreshtoken",
    "logintoken",
    "token",
    "password",
    "passphrase",
    "secret",
    "key",
    "authorization",
    "cookie",
];

/// Prefixes of Matrix access and refresh tokens (Synapse's `syt_`/`syr_`,
/// MAS's `mat_`/`mar_`, compatibility tokens `mct_`), redacted wherever
/// they appear.
const TOKEN_PREFIXES: &[&str] = &["syt_", "syr_", "mat_", "mar_", "mct_"];

/// URL schemes whose links are sign-in material in full: everything after
/// the scheme is redacted.
const SECRET_SCHEMES: &[&str] = &["familychat"];

/// `line` with secrets and query strings removed, as written to the log.
pub fn scrub(line: &str) -> String {
    let line = scrub_urls(line);
    let line = scrub_bearer(&line);
    let line = scrub_named_values(&line);
    scrub_token_prefixes(&line)
}

/// `url` as it may be logged: scheme, host, port and path, without user
/// info, query or fragment. A `familychat://` link is redacted whole.
pub fn redact_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        // Not a URL with an authority; drop anything after `?` or `#`.
        return strip_query(url).to_owned();
    };
    let scheme = &url[..scheme_end];
    if SECRET_SCHEMES
        .iter()
        .any(|secret| scheme.eq_ignore_ascii_case(secret))
    {
        return format!("{scheme}://{REDACTED}");
    }
    let rest = strip_query(&url[scheme_end + 3..]);
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, ""),
    };
    // User info is everything up to the last `@` of the authority.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let mut out = format!("{scheme}://{host}{path}");
    if rest.len() < url[scheme_end + 3..].len() {
        out.push_str("?[redacted]");
    }
    out
}

/// `text` up to its first `?` or `#`.
fn strip_query(text: &str) -> &str {
    text.find(['?', '#']).map_or(text, |end| &text[..end])
}

/// Every `scheme://…` in `line` through [`redact_url`]. A URL runs to the
/// next whitespace, quote, `<`, `>` or backtick.
fn scrub_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(found) = rest.find("://") {
        // Back up over the scheme: ASCII letters, digits, `+`, `-`, `.`.
        let scheme_start = rest[..found]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || "+-.".contains(*c))
            .last()
            .map_or(found, |(index, _)| index);
        if scheme_start == found {
            out.push_str(&rest[..found + 3]);
            rest = &rest[found + 3..];
            continue;
        }
        let url_end = rest[found + 3..]
            .find(|c: char| c.is_whitespace() || "\"'<>`".contains(c))
            .map_or(rest.len(), |end| found + 3 + end);
        out.push_str(&rest[..scheme_start]);
        out.push_str(&redact_url(&rest[scheme_start..url_end]));
        rest = &rest[url_end..];
    }
    out.push_str(rest);
    out
}

/// `Bearer <credential>` with the credential redacted.
fn scrub_bearer(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let mut out = String::with_capacity(line.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = lower[search..].find("bearer") {
        let start = search + found;
        let after = start + "bearer".len();
        let value_start = after + count_leading(&line[after..], |c| c == ' ' || c == '\t');
        search = after;
        let in_word = line.as_bytes()[..start]
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());
        if in_word || value_start == after || line[value_start..].starts_with(REDACTED) {
            continue;
        }
        let value_end = value_start + value_len(&line[value_start..]);
        if value_end == value_start {
            continue;
        }
        out.push_str(&line[copied..value_start]);
        out.push_str(REDACTED);
        copied = value_end;
        search = value_end;
    }
    out.push_str(&line[copied..]);
    out
}

/// The values after secret-named fields (see [`SECRET_NAMES`]).
fn scrub_named_values(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut copied = 0;
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        // A name is a run of ASCII letters, digits, `_`, `-` and `.`.
        if !is_name_byte(bytes[index]) || (index > 0 && is_name_byte(bytes[index - 1])) {
            index += 1;
            continue;
        }
        let name_end = index + count_leading(&line[index..], is_name_byte_char);
        let name = &line[index..name_end];
        // `"name": "value"`: a closing quote may follow the name.
        let mut cursor = name_end;
        if line[cursor..].starts_with('"') {
            cursor += 1;
        }
        cursor += count_leading(&line[cursor..], |c| c == ' ');
        let separator = line[cursor..].chars().next();
        if !matches!(separator, Some('=') | Some(':')) || !is_secret_name(name) {
            index = name_end;
            continue;
        }
        cursor += 1;
        cursor += count_leading(&line[cursor..], |c| c == ' ');
        if line[cursor..].starts_with(REDACTED) {
            index = cursor + REDACTED.len();
            continue;
        }
        let quoted = line[cursor..].starts_with('"');
        if quoted {
            cursor += 1;
        }
        let value_len = if quoted {
            quoted_len(&line[cursor..])
        } else {
            value_len(&line[cursor..])
        };
        if value_len == 0 {
            index = cursor;
            continue;
        }
        out.push_str(&line[copied..cursor]);
        out.push_str(REDACTED);
        copied = cursor + value_len;
        index = copied;
    }
    out.push_str(&line[copied..]);
    out
}

/// Words that start with a Matrix token prefix (see [`TOKEN_PREFIXES`]).
fn scrub_token_prefixes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut copied = 0;
    let bytes = line.as_bytes();
    for index in 0..bytes.len() {
        if index < copied || (index > 0 && is_name_byte(bytes[index - 1])) {
            continue;
        }
        if !line.is_char_boundary(index) {
            continue;
        }
        let rest = &line[index..];
        if !TOKEN_PREFIXES.iter().any(|prefix| rest.starts_with(prefix)) {
            continue;
        }
        let end = index + value_len(rest);
        out.push_str(&line[copied..index]);
        out.push_str(REDACTED);
        copied = end;
    }
    out.push_str(&line[copied..]);
    out
}

fn is_secret_name(name: &str) -> bool {
    let normalised: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    SECRET_NAMES
        .iter()
        .any(|secret| normalised.ends_with(secret))
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_name_byte_char(c: char) -> bool {
    c.is_ascii() && is_name_byte(c as u8)
}

/// Length in bytes of the leading run of `text` matching `predicate`.
fn count_leading(text: &str, predicate: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .find(|(_, c)| !predicate(*c))
        .map_or(text.len(), |(index, _)| index)
}

/// An unquoted value runs to whitespace or a delimiter.
fn value_len(text: &str) -> usize {
    count_leading(text, |c| !c.is_whitespace() && !"&,;\"'}])>".contains(c))
}

/// A quoted value runs to the closing quote, skipping escaped quotes.
fn quoted_len(text: &str) -> usize {
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return index,
            _ => escaped = false,
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_prints_nothing() {
        let body = Redacted("the message body");
        assert_eq!(format!("{body}"), REDACTED);
        assert_eq!(format!("{body:?}"), REDACTED);
    }

    #[test]
    fn urls_lose_user_info_query_and_fragment() {
        assert_eq!(
            redact_url(
                "https://alice:pw@matrix.example.org:8448/_matrix/client/v3/sync?since=s1&access_token=abc#x"
            ),
            "https://matrix.example.org:8448/_matrix/client/v3/sync?[redacted]"
        );
        assert_eq!(
            redact_url("https://matrix.example.org/_matrix/client/versions"),
            "https://matrix.example.org/_matrix/client/versions"
        );
        assert_eq!(
            redact_url("https://example.org#frag"),
            "https://example.org?[redacted]"
        );
        assert_eq!(
            redact_url("familychat://login?hs=safechat.family&token=abc"),
            "familychat://[redacted]"
        );
        assert_eq!(
            redact_url("FamilyChat://anything"),
            "FamilyChat://[redacted]"
        );
        assert_eq!(redact_url("/path?q=1"), "/path");
    }

    #[test]
    fn scrub_removes_every_kind_of_secret() {
        let cases = [
            (
                "GET https://hs.example.org/_matrix/client/v3/login?loginToken=LT-12345 failed",
                "LT-12345",
            ),
            ("Authorization: Bearer syt_abc_def_123", "syt_abc_def_123"),
            (
                "authorization: bearer opaque.token.value",
                "opaque.token.value",
            ),
            ("access_token=abc123secret&since=1", "abc123secret"),
            ("refresh_token: ref-secret-9", "ref-secret-9"),
            (
                r#"{"password": "hunter2 with spaces", "user": "a"}"#,
                "hunter2 with spaces",
            ),
            (r#"{"access_token":"quoted\"secret"}"#, "quoted\\\"secret"),
            ("passphrase=0011223344556677 next", "0011223344556677"),
            ("X-Access-Token: header-secret", "header-secret"),
            ("store key=deadbeefcafe", "deadbeefcafe"),
            (
                "token rotated to syr_new_refresh_value",
                "syr_new_refresh_value",
            ),
            ("mas token mat_0123456789", "mat_0123456789"),
            ("opened familychat://login?token=abc in place", "token=abc"),
        ];
        for (line, secret) in cases {
            let scrubbed = scrub(line);
            assert!(
                !scrubbed.contains(secret),
                "{secret:?} survived in {scrubbed:?} (from {line:?})"
            );
            assert!(scrubbed.contains(REDACTED), "{scrubbed:?}");
        }
    }

    #[test]
    fn scrub_keeps_what_is_safe() {
        let line = "sync with https://hs.example.org/_matrix/client/v3/sync took 120ms (rooms=4)";
        assert_eq!(scrub(line), line);
        assert_eq!(scrub("keys uploaded: 4"), "keys uploaded: 4");
        assert_eq!(scrub("monkey business"), "monkey business");
        assert_eq!(scrub("tokens: none"), "tokens: none");
        assert_eq!(scrub("a :// stray"), "a :// stray");
        assert_eq!(scrub("ümlaut 🙂 syt"), "ümlaut 🙂 syt");
    }
}
