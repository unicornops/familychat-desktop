//! Which homeservers a Family Chat sign-in may talk to (PLAN.md, section
//! 6.5; the contract's "Which servers a client may talk to").
//!
//! The allowlist is a set of DNS suffixes, matched as strict subdomains: a
//! host is allowed when it ends in `.<suffix>` with at least one label in
//! front, so the apex (`safechat.family`, the website, not a homeserver)
//! and look-alikes (`evilsafechat.family`, `safechat.family.evil.example`)
//! are not. It is applied to the **resolved** homeserver URL, after
//! discovery, never to a server name: a custom-domain family
//! (`account_provider=smith.ie`) is allowed because its well-known points
//! at `smith.safechat.family`.
//!
//! The environments:
//!
//! | Build | Suffixes |
//! | --- | --- |
//! | every build (production) | `safechat.family` |
//! | the `staging` feature | adds `familychat.dev` (Family Chat staging) |
//! | the `test-harness` feature | adds `familychat.test` (the homeserver harness) |
//!
//! Neither feature can reach a release build of the app
//! (`scripts/check-release-features.sh`), and a release build with
//! `test-harness` does not compile.
//!
//! A [`CheckedHomeserver`] is the only thing a credential can be sent to:
//! [`Allowlist::check`] makes one for Family Chat, and
//! [`CheckedHomeserver::other_server`] for "Use another Matrix server"
//! (HTTPS, nothing else), so no code path can send a password or a token
//! to a URL that skipped the check.

use reqwest::Url;

use crate::host::{ServerName, host_is_under};

/// Family Chat's production homeservers: `*.safechat.family`.
pub const PRODUCTION_SUFFIX: &str = "safechat.family";
/// Family Chat staging's homeservers, with the `staging` feature.
pub const STAGING_SUFFIX: &str = "familychat.dev";
/// The homeserver harness's, with the `test-harness` feature.
pub const HARNESS_SUFFIX: &str = "familychat.test";

/// The homeserver suffixes a Family Chat sign-in accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    suffixes: Vec<&'static str>,
}

/// Why a homeserver URL was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotAllowed {
    /// Not `https://`.
    NotHttps,
    /// An IP address, not a name.
    NotAName,
    /// User info in the URL.
    UserInfo,
    /// The host is not under any of the suffixes.
    NotFamilyChat,
}

impl Allowlist {
    /// Production's: `*.safechat.family` only.
    pub fn production() -> Self {
        Self {
            suffixes: vec![PRODUCTION_SUFFIX],
        }
    }

    /// This build's: production's, plus staging's with the `staging`
    /// feature and the harness's with `test-harness`.
    pub fn for_build() -> Self {
        #[allow(unused_mut)]
        let mut allowlist = Self::production();
        #[cfg(feature = "staging")]
        allowlist.suffixes.push(STAGING_SUFFIX);
        #[cfg(feature = "test-harness")]
        allowlist.suffixes.push(HARNESS_SUFFIX);
        allowlist
    }

    /// The suffixes.
    pub fn suffixes(&self) -> &[&'static str] {
        &self.suffixes
    }

    /// Whether `name`'s host is under one of the suffixes (its port is not
    /// checked). For a link's `hs`, which is the host the token goes to.
    pub fn allows_name(&self, name: &ServerName) -> bool {
        self.suffixes
            .iter()
            .any(|suffix| name.host_is_under(suffix))
    }

    /// Whether an account's homeserver URL (as the registry keeps it) is
    /// one of Family Chat's: what Settings shows the panel's guidance for
    /// (ADR 0015). Anything that doesn't parse is not.
    pub fn is_family_chat(&self, homeserver_url: &str) -> bool {
        Url::parse(homeserver_url).is_ok_and(|url| self.check(&url).is_ok())
    }

    /// Checks a resolved homeserver URL: `https://`, a host name (not an
    /// address) under one of the suffixes, no user info. The checked URL
    /// keeps its path, without a query or fragment.
    pub fn check(&self, url: &Url) -> Result<CheckedHomeserver, NotAllowed> {
        let url = https_base(url)?;
        let allowed = match url.host() {
            Some(url::Host::Domain(host)) => self
                .suffixes
                .iter()
                .any(|suffix| host_is_under(host, suffix)),
            _ => false,
        };
        if !allowed {
            return Err(NotAllowed::NotFamilyChat);
        }
        Ok(CheckedHomeserver {
            url,
            family_chat: true,
        })
    }
}

/// A homeserver URL that may be sent credentials: one under the allowlist,
/// or for another Matrix server, any `https://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedHomeserver {
    url: Url,
    family_chat: bool,
}

impl CheckedHomeserver {
    /// Another Matrix server's homeserver: `https://` with a host name and
    /// no user info, and nothing more.
    pub fn other_server(url: &Url) -> Result<Self, NotAllowed> {
        Ok(Self {
            url: https_base(url)?,
            family_chat: false,
        })
    }

    /// The base URL, ending in `/`.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The base URL as an account keeps it: without the final `/`.
    pub fn as_account_url(&self) -> String {
        self.url.as_str().trim_end_matches('/').to_owned()
    }

    /// Whether it passed the Family Chat allowlist.
    pub fn is_family_chat(&self) -> bool {
        self.family_chat
    }

    /// The Client-Server API endpoint at `path` (relative, such as
    /// `_matrix/client/v3/login`).
    pub fn endpoint(&self, path: &str) -> Url {
        let mut url = self.url.clone();
        let base = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base}/{path}"));
        url
    }
}

/// `url` as a homeserver base: `https://`, a host name, no user info, no
/// query or fragment, a path ending in `/`.
fn https_base(url: &Url) -> Result<Url, NotAllowed> {
    if url.scheme() != "https" {
        return Err(NotAllowed::NotHttps);
    }
    if !matches!(url.host(), Some(url::Host::Domain(_))) {
        return Err(NotAllowed::NotAName);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NotAllowed::UserInfo);
    }
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a URL")
    }

    #[test]
    fn an_account_url_is_family_chat_only_under_the_allowlist() {
        let allowlist = Allowlist::production();
        assert!(allowlist.is_family_chat("https://smith.safechat.family"));
        assert!(allowlist.is_family_chat("https://smith.safechat.family/"));
        for other in [
            "https://matrix.example.org",
            "http://smith.safechat.family",
            "https://safechat.family.example.org",
            "not a url",
            "",
        ] {
            assert!(!allowlist.is_family_chat(other), "{other}");
        }
    }

    #[test]
    fn production_allows_family_subdomains_only() {
        let allowlist = Allowlist::production();
        for allowed in [
            "https://smith.safechat.family",
            "https://smith.safechat.family/",
            "https://SMITH.safechat.FAMILY/",
            "https://smith.safechat.family:8448/",
            "https://a.b.safechat.family/matrix/",
        ] {
            let checked = allowlist.check(&url(allowed)).expect(allowed);
            assert!(checked.is_family_chat());
            assert!(checked.url().path().ends_with('/'), "{allowed}");
        }
        for (refused, reason) in [
            // The apex is the website, never a homeserver (the Android
            // review's apex fallback).
            ("https://safechat.family", NotAllowed::NotFamilyChat),
            ("https://safechat.family./", NotAllowed::NotFamilyChat),
            ("https://smith.safechat.family./", NotAllowed::NotFamilyChat),
            ("https://evilsafechat.family", NotAllowed::NotFamilyChat),
            (
                "https://safechat.family.evil.example",
                NotAllowed::NotFamilyChat,
            ),
            ("https://smith.familychat.dev", NotAllowed::NotFamilyChat),
            ("https://hs1.familychat.test", NotAllowed::NotFamilyChat),
            ("https://evil.example", NotAllowed::NotFamilyChat),
            ("http://smith.safechat.family", NotAllowed::NotHttps),
            ("wss://smith.safechat.family", NotAllowed::NotHttps),
            ("https://127.0.0.1", NotAllowed::NotAName),
            ("https://[::1]", NotAllowed::NotAName),
            (
                "https://smith.safechat.family@evil.example",
                NotAllowed::UserInfo,
            ),
            (
                "https://evil.example@smith.safechat.family",
                NotAllowed::UserInfo,
            ),
            (
                "https://evil.example:x@smith.safechat.family",
                NotAllowed::UserInfo,
            ),
        ] {
            assert_eq!(allowlist.check(&url(refused)), Err(reason), "{refused}");
        }
    }

    #[test]
    fn a_backslash_in_a_resolved_url_cannot_smuggle_a_host() {
        // WHATWG URL parsing treats `\` as `/` in special schemes, so the
        // host is what comes before it, and that host is what is checked.
        let parsed = url("https://evil.example\\@smith.safechat.family/");
        assert_eq!(parsed.host_str(), Some("evil.example"));
        assert_eq!(
            Allowlist::production().check(&parsed),
            Err(NotAllowed::NotFamilyChat)
        );
    }

    #[test]
    fn this_build_adds_the_harness_for_tests() {
        // The crate's tests build with `test-harness`.
        let allowlist = Allowlist::for_build();
        assert!(allowlist.suffixes().contains(&PRODUCTION_SUFFIX));
        assert!(allowlist.suffixes().contains(&HARNESS_SUFFIX));
        assert_eq!(
            allowlist.suffixes().contains(&STAGING_SUFFIX),
            cfg!(feature = "staging")
        );
        assert!(allowlist.check(&url("https://hs1.familychat.test")).is_ok());
        assert!(allowlist.check(&url("https://familychat.test")).is_err());
        assert!(
            allowlist
                .check(&url("https://evil.familychat.example"))
                .is_err()
        );
    }

    #[test]
    fn names_are_checked_the_same_way() {
        let allowlist = Allowlist::production();
        let name = |text| ServerName::parse(text).expect("parses");
        assert!(allowlist.allows_name(&name("smith.safechat.family")));
        assert!(allowlist.allows_name(&name("smith.safechat.family:8448")));
        assert!(!allowlist.allows_name(&name("safechat.family")));
        assert!(!allowlist.allows_name(&name("smith.ie")));
        assert!(!allowlist.allows_name(&name("evilsafechat.family")));
    }

    #[test]
    fn other_servers_need_https_and_a_name() {
        assert!(CheckedHomeserver::other_server(&url("https://matrix.org")).is_ok());
        assert_eq!(
            CheckedHomeserver::other_server(&url("http://matrix.org")),
            Err(NotAllowed::NotHttps)
        );
        assert_eq!(
            CheckedHomeserver::other_server(&url("https://10.0.0.1")),
            Err(NotAllowed::NotAName)
        );
        let checked = CheckedHomeserver::other_server(&url("https://matrix.org")).expect("allowed");
        assert!(!checked.is_family_chat());
    }

    #[test]
    fn endpoints_keep_the_base_path() {
        let checked = Allowlist::production()
            .check(&url("https://smith.safechat.family/base?x=1#y"))
            .expect("allowed");
        assert_eq!(
            checked.endpoint("_matrix/client/v3/login").as_str(),
            "https://smith.safechat.family/base/_matrix/client/v3/login"
        );
        assert_eq!(
            checked.as_account_url(),
            "https://smith.safechat.family/base"
        );
        let root = Allowlist::production()
            .check(&url("https://smith.safechat.family"))
            .expect("allowed");
        assert_eq!(
            root.endpoint("_matrix/client/versions").as_str(),
            "https://smith.safechat.family/_matrix/client/versions"
        );
        assert_eq!(root.as_account_url(), "https://smith.safechat.family");
    }
}
