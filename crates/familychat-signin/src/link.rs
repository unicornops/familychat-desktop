//! Family Chat sign-in links (PLAN.md, sections 6.3 and 6.4; family-chat's
//! `docs/client-login-links.md`).
//!
//! ```text
//! https://safechat.family/app/login?account_provider=<server_name>&login_hint=mxid:<mxid>[&hs=<serving_hostname>&token=<login_token>]
//! ```
//!
//! [`LoginLink::parse`] accepts that link, the same query on
//! `familychat://safechat.family/app/login?...`, and either one pasted with
//! whitespace around it. The host is fixed (`safechat.family`, the host the
//! mobile apps' universal links claim), the path is `/app/login` (or
//! `/app/login/`, the website's fallback page), and the query is read as
//! the standard percent-encoded form it is built as, never split on `:`.
//!
//! The rules (the contract's client rule 1), each with its test below:
//!
//! - `account_provider` is required and must be a well-formed server name
//!   ([`ServerName::parse`]); otherwise the text is not a sign-in link.
//! - `login_hint` is kept only when it is `mxid:` and a valid Matrix ID
//!   whose server is `account_provider`; otherwise it is dropped and the
//!   link is used without it.
//! - `hs` and `token` are kept only together, and only when `hs` is a bare
//!   `hostname[:port]` under the allowlist and the token is not empty;
//!   otherwise the token is dropped (wiped, never sent) and the link
//!   degrades to the hint form ([`LoginLink::dropped_code`] says so).
//! - A parameter given twice is ambiguous: a second `account_provider`
//!   refuses the link, a second `login_hint` drops the hint, and a second
//!   `hs` or `token` drops the code. (The mobile apps take the first value;
//!   the desktop is stricter, since nothing the panel builds repeats one.)
//!
//! The token is a [`Secret`]: `Debug` prints `[redacted]`, it is wiped when
//! dropped, and no error here carries any of the link's text.

use std::fmt;

use familychat_core::{OwnedUserId, UserId};
use familychat_security::Secret;
use reqwest::Url;

use crate::{allowlist::Allowlist, host::ServerName};

/// The host every sign-in link names.
pub const LINK_HOST: &str = "safechat.family";
/// The path of a sign-in link, and the website's fallback page's.
const LINK_PATHS: [&str; 2] = ["/app/login", "/app/login/"];
/// The schemes a link may arrive with: the universal link, and the
/// desktop's own `familychat://` (`familychat_core::identity::URL_SCHEME`).
const LINK_SCHEMES: [&str; 2] = ["https", familychat_core::identity::URL_SCHEME];

/// Why text is not a usable sign-in link. Carries none of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// Not a URL at all.
    NotAUrl,
    /// A URL, but not `https://safechat.family/app/login` or
    /// `familychat://safechat.family/app/login`.
    NotASigninLink,
    /// No `account_provider`, or it was given twice.
    NoAccountProvider,
    /// The `account_provider` is not a bare server name.
    BadAccountProvider,
}

/// Why a link's code was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DroppedCode {
    /// A token without `hs`, or `hs` without a token.
    Incomplete,
    /// `hs` is not a bare `hostname[:port]`.
    BadHost,
    /// `hs` is not under the allowlist.
    NotFamilyChat,
    /// `hs` or `token` was given twice.
    Repeated,
}

/// A link's sign-in code: the host to redeem it at, and the single-use
/// token. `Debug` never prints the token.
pub struct SigninCode {
    /// The host that answers the client API (`hs`), under the allowlist.
    pub hs: ServerName,
    /// The `m.login.token`.
    pub token: Secret<String>,
}

impl fmt::Debug for SigninCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigninCode")
            .field("hs", &self.hs)
            .field("token", &self.token)
            .finish()
    }
}

/// A parsed sign-in link.
#[derive(Debug)]
pub struct LoginLink {
    /// The family's Matrix server name, which discovery resolves.
    pub account_provider: ServerName,
    /// The member the link is for, when the hint was usable.
    pub login_hint: Option<OwnedUserId>,
    /// The code, when the link carried a usable one.
    pub code: Option<SigninCode>,
    /// Why a code the link carried was dropped.
    pub dropped_code: Option<DroppedCode>,
}

impl LoginLink {
    /// Parses a link, or text that is one with whitespace around it.
    pub fn parse(text: &str, allowlist: &Allowlist) -> Result<Self, LinkError> {
        let text = text.trim();
        // A link never has whitespace inside it; text around one is not a
        // link (and a URL parser would quietly encode it into the token).
        if text.chars().any(char::is_whitespace) {
            return Err(LinkError::NotAUrl);
        }
        let url = Url::parse(text).map_err(|_| LinkError::NotAUrl)?;
        if !LINK_SCHEMES.contains(&url.scheme())
            || url.host_str() != Some(LINK_HOST)
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || !LINK_PATHS.contains(&url.path())
        {
            return Err(LinkError::NotASigninLink);
        }

        let mut account_provider = Values::default();
        let mut login_hint = Values::default();
        let mut hs = Values::default();
        let mut token = Values::default();
        for (name, value) in url.query_pairs() {
            let slot = match name.as_ref() {
                "account_provider" => &mut account_provider,
                "login_hint" => &mut login_hint,
                "hs" => &mut hs,
                "token" => &mut token,
                _ => continue,
            };
            slot.push(value.into_owned());
        }

        let account_provider = match account_provider.single() {
            Some(value) => ServerName::parse(value).map_err(|_| LinkError::BadAccountProvider)?,
            None => return Err(LinkError::NoAccountProvider),
        };
        let login_hint = login_hint
            .single()
            .and_then(|hint| parse_hint(hint, &account_provider));

        let (code, dropped_code) = match (hs.count, token.count) {
            (0, 0) => (None, None),
            (1, 1) => match code(hs.take(), token.take(), allowlist) {
                Ok(code) => (Some(code), None),
                Err(dropped) => (None, Some(dropped)),
            },
            (0 | 1, 0 | 1) => (None, Some(DroppedCode::Incomplete)),
            _ => (None, Some(DroppedCode::Repeated)),
        };
        Ok(Self {
            account_provider,
            login_hint,
            code,
            dropped_code,
        })
    }
}

/// The values one parameter was given, wiped when dropped (a token is one
/// of them).
#[derive(Default)]
struct Values {
    first: Option<Secret<String>>,
    count: usize,
}

impl Values {
    fn push(&mut self, value: String) {
        self.count += 1;
        let value = Secret::new(value);
        if self.first.is_none() {
            self.first = Some(value);
        }
    }

    /// The value, when it was given exactly once.
    fn single(&self) -> Option<&str> {
        (self.count == 1)
            .then_some(self.first.as_ref())
            .flatten()
            .map(|value| value.expose().as_str())
    }

    fn take(&mut self) -> Option<Secret<String>> {
        self.first.take()
    }
}

fn code(
    hs: Option<Secret<String>>,
    token: Option<Secret<String>>,
    allowlist: &Allowlist,
) -> Result<SigninCode, DroppedCode> {
    let (Some(hs), Some(token)) = (hs, token) else {
        return Err(DroppedCode::Incomplete);
    };
    if token.expose().is_empty() {
        return Err(DroppedCode::Incomplete);
    }
    let hs = ServerName::parse(hs.expose()).map_err(|_| DroppedCode::BadHost)?;
    if !allowlist.allows_name(&hs) {
        return Err(DroppedCode::NotFamilyChat);
    }
    Ok(SigninCode { hs, token })
}

/// The hint's Matrix ID, when it is `mxid:` and a valid ID on
/// `account_provider`.
fn parse_hint(hint: &str, account_provider: &ServerName) -> Option<OwnedUserId> {
    let mxid = hint.strip_prefix("mxid:")?;
    let user_id = parse_user_id(mxid)?;
    (user_server(&user_id)? == *account_provider).then_some(user_id)
}

/// A Matrix ID whose server half is also a strict server name, and whose
/// localpart is printable ASCII (the specification's historical grammar,
/// which ruma accepts more loosely: no spaces or control characters).
pub fn parse_user_id(text: &str) -> Option<OwnedUserId> {
    let user_id = UserId::parse(text).ok()?;
    let printable = user_id
        .localpart()
        .bytes()
        .all(|byte| (0x21..=0x7e).contains(&byte) && byte != b':');
    if !printable || user_id.localpart().is_empty() {
        return None;
    }
    user_server(&user_id)?;
    Some(user_id)
}

/// The server half of a Matrix ID, parsed strictly.
pub fn user_server(user_id: &UserId) -> Option<ServerName> {
    ServerName::parse(user_id.server_name().as_str()).ok()
}

/// Whether two Matrix IDs name the same account: the same localpart, and
/// the same server (which compares without regard to case).
pub fn same_user(a: &UserId, b: &UserId) -> bool {
    a.localpart() == b.localpart()
        && match (user_server(a), user_server(b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "lt-secret-token-value";

    fn allowlist() -> Allowlist {
        Allowlist::production()
    }

    fn link(query: &str) -> Result<LoginLink, LinkError> {
        LoginLink::parse(
            &format!("https://safechat.family/app/login?{query}"),
            &allowlist(),
        )
    }

    fn full(provider: &str, hint: &str, hs: &str) -> String {
        format!("account_provider={provider}&login_hint=mxid:{hint}&hs={hs}&token={TOKEN}")
    }

    #[test]
    fn reads_the_link_the_panel_builds() {
        // What qr_login.build_app_login_url makes: urlencode() of the four.
        let parsed = link(&format!(
            "account_provider=smith.safechat.family&login_hint=mxid%3A%40ana%3Asmith.safechat.family&hs=smith.safechat.family&token={TOKEN}"
        ))
        .expect("parses");
        assert_eq!(parsed.account_provider.to_string(), "smith.safechat.family");
        assert_eq!(
            parsed.login_hint.as_deref().map(UserId::as_str),
            Some("@ana:smith.safechat.family")
        );
        let code = parsed.code.expect("a code");
        assert_eq!(code.hs.to_string(), "smith.safechat.family");
        assert_eq!(code.token.expose(), TOKEN);
        assert_eq!(parsed.dropped_code, None);
    }

    #[test]
    fn a_custom_domain_family_keeps_its_code() {
        let parsed =
            link(&full("smith.ie", "@kid:smith.ie", "smith.safechat.family")).expect("parses");
        assert_eq!(parsed.account_provider.to_string(), "smith.ie");
        assert!(parsed.login_hint.is_some());
        assert!(parsed.code.is_some());
    }

    #[test]
    fn accepts_the_desktop_scheme_the_fallback_path_and_pasted_whitespace() {
        let query = full(
            "smith.safechat.family",
            "@ana:smith.safechat.family",
            "smith.safechat.family",
        );
        for text in [
            format!("familychat://safechat.family/app/login?{query}"),
            format!("https://safechat.family/app/login/?{query}"),
            format!("  \n https://safechat.family/app/login?{query}\t\n"),
            format!("HTTPS://SafeChat.Family/app/login?{query}"),
        ] {
            let parsed = LoginLink::parse(&text, &allowlist()).expect("parses");
            assert!(parsed.code.is_some(), "{text}");
        }
    }

    #[test]
    fn refuses_other_urls_and_text() {
        let query = full(
            "smith.safechat.family",
            "@ana:smith.safechat.family",
            "smith.safechat.family",
        );
        for text in [
            String::new(),
            "not a link".to_owned(),
            query.clone(),
            format!("http://safechat.family/app/login?{query}"),
            format!("https://evil.example/app/login?{query}"),
            format!("https://smith.safechat.family/app/login?{query}"),
            format!("https://safechat.family.evil.example/app/login?{query}"),
            format!("https://safechat.family:8443/app/login?{query}"),
            format!("https://user@safechat.family/app/login?{query}"),
            format!("https://safechat.family/app/other?{query}"),
            format!("https://safechat.family/app/login/x?{query}"),
            format!("https://app.safechat.family/?loginToken={TOKEN}&hs=smith.safechat.family"),
            format!("family.safechat.app://safechat.family/app/login?{query}"),
            format!("https://safechat.family/app/login?{query} trailing words"),
        ] {
            assert!(
                LoginLink::parse(&text, &allowlist()).is_err(),
                "accepted {text:?}"
            );
        }
    }

    #[test]
    fn account_provider_is_required_and_strict() {
        assert_eq!(
            link("login_hint=mxid:@a:smith.ie").err(),
            Some(LinkError::NoAccountProvider)
        );
        assert_eq!(
            link("account_provider=smith.ie&account_provider=evil.example").err(),
            Some(LinkError::NoAccountProvider)
        );
        for provider in [
            "evil.example%5C%40smith.safechat.family",
            "evil.example%2F.safechat.family",
            "smith.ie%2F",
            "user%40smith.ie",
            "smith.ie%20",
            "",
        ] {
            assert_eq!(
                link(&format!("account_provider={provider}")).err(),
                Some(LinkError::BadAccountProvider),
                "{provider}"
            );
        }
        // Any well-formed name is accepted here, the apex included:
        // discovery and the allowlist decide (the iOS app's choice).
        assert!(link("account_provider=safechat.family").is_ok());
        assert!(link("account_provider=Smith.IE").is_ok());
        assert!(link("account_provider=https%3A%2F%2Fsmith.ie").is_ok());
    }

    #[test]
    fn a_hint_for_another_server_is_dropped() {
        for hint in [
            "mxid:@kid:evil.example",
            "mxid:@kid:smith.safechat.family",
            "mxid:@kid:smith.ie:8448",
            "@kid:smith.ie",
            "mxid:kid",
            "mxid:",
            "email:kid@smith.ie",
            "mxid:@kid:smith.ie%5C",
            "mxid:@k%20id:smith.ie",
        ] {
            let parsed = link(&format!(
                "account_provider=smith.ie&login_hint={hint}&hs=smith.safechat.family&token={TOKEN}"
            ))
            .expect("parses");
            assert_eq!(parsed.login_hint, None, "{hint}");
            // The code is kept: with no hint, the user's server is bound to
            // account_provider instead.
            assert!(parsed.code.is_some(), "{hint}");
        }
        let parsed =
            link("account_provider=smith.ie&login_hint=mxid:@kid:SMITH.IE").expect("parses");
        assert!(
            parsed.login_hint.is_some(),
            "the server compares without case"
        );
        let parsed = link(
            "account_provider=smith.ie&login_hint=mxid:@a:smith.ie&login_hint=mxid:@b:smith.ie",
        )
        .expect("parses");
        assert_eq!(parsed.login_hint, None);
    }

    #[test]
    fn a_code_is_kept_only_with_an_allowlisted_bare_hs() {
        let dropped = |query: &str| {
            let parsed = link(query).expect("parses");
            assert!(parsed.code.is_none(), "{query}");
            parsed.dropped_code
        };
        let base =
            "account_provider=smith.safechat.family&login_hint=mxid:@a:smith.safechat.family";
        for (rest, reason) in [
            (format!("&token={TOKEN}"), DroppedCode::Incomplete),
            (
                "&hs=smith.safechat.family".to_owned(),
                DroppedCode::Incomplete,
            ),
            (
                "&hs=smith.safechat.family&token=".to_owned(),
                DroppedCode::Incomplete,
            ),
            (
                format!("&hs=evil.example&token={TOKEN}"),
                DroppedCode::NotFamilyChat,
            ),
            (
                format!("&hs=safechat.family&token={TOKEN}"),
                DroppedCode::NotFamilyChat,
            ),
            (
                format!("&hs=evilsafechat.family&token={TOKEN}"),
                DroppedCode::NotFamilyChat,
            ),
            (
                format!("&hs=smith.ie&token={TOKEN}"),
                DroppedCode::NotFamilyChat,
            ),
            (format!("&hs=127.0.0.1&token={TOKEN}"), DroppedCode::BadHost),
            (
                format!("&hs=evil.example%5C%40smith.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=evil.example%5C.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=evil.example%2F.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=evil.example%3F.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=evil.example%23.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=user%40smith.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=smith.safechat.family%0A&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=smith.safechat.family%2Fpath&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=http%3A%2F%2Fsmith.safechat.family&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=%5B%3A%3A1%5D&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=smith.safechat.family.&token={TOKEN}"),
                DroppedCode::BadHost,
            ),
            (
                format!("&hs=smith.safechat.family&hs=evil.example&token={TOKEN}"),
                DroppedCode::Repeated,
            ),
            (
                format!("&hs=smith.safechat.family&token={TOKEN}&token=other"),
                DroppedCode::Repeated,
            ),
        ] {
            assert_eq!(dropped(&format!("{base}{rest}")), Some(reason), "{rest}");
        }
        // A port and an https:// prefix are fine.
        let parsed = link(&format!(
            "{base}&hs=https%3A%2F%2Fsmith.safechat.family%3A8448&token={TOKEN}"
        ))
        .expect("parses");
        assert_eq!(
            parsed.code.expect("kept").hs.to_string(),
            "smith.safechat.family:8448"
        );
        // A hint-only link has nothing to drop.
        assert_eq!(link(base).expect("parses").dropped_code, None);
    }

    #[test]
    fn the_query_decodes_as_a_form() {
        // `+` is a space in a form, so this hint is not a valid ID.
        let parsed =
            link("account_provider=smith.ie&login_hint=mxid:@a+b:smith.ie").expect("parses");
        assert_eq!(parsed.login_hint, None);
        let parsed = link("account_provider=smith.ie&login_hint=mxid%3A%40a%2Bb%3Asmith.ie")
            .expect("parses");
        assert_eq!(
            parsed.login_hint.as_deref().map(UserId::as_str),
            Some("@a+b:smith.ie")
        );
        // Unknown parameters are ignored.
        assert!(link("account_provider=smith.ie&utm_source=x").is_ok());
    }

    #[test]
    fn debug_never_prints_the_token() {
        let parsed = link(&full(
            "smith.safechat.family",
            "@ana:smith.safechat.family",
            "smith.safechat.family",
        ))
        .expect("parses");
        for debug in [format!("{parsed:?}"), format!("{parsed:#?}")] {
            assert!(!debug.contains(TOKEN), "{debug}");
            assert!(debug.contains("smith.safechat.family"), "{debug}");
        }
    }

    #[test]
    fn users_compare_by_localpart_and_server() {
        let a = parse_user_id("@ana:Smith.IE").expect("valid");
        let b = parse_user_id("@ana:smith.ie").expect("valid");
        let c = parse_user_id("@Ana:smith.ie").expect("valid");
        let d = parse_user_id("@ana:smith.ie:8448").expect("valid");
        assert!(same_user(&a, &b));
        assert!(!same_user(&b, &c));
        assert!(!same_user(&b, &d));
        assert!(parse_user_id("@ana:evil.example\\").is_none());
        assert!(parse_user_id("@ana:[::1]").is_none());
    }
}
