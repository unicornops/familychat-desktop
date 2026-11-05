//! Fuzzes the sign-in link parser and the strict server-name parser
//! (`familychat-signin`'s `link` and `host`; ADR 0006). Whatever the text,
//! neither panics, and what they accept keeps the rules the property tests
//! (`familychat-signin/tests/properties.rs`) state:
//!
//! - a server name is bare (lower-case letters, digits, `.`, `-`, `:`),
//!   parses again to itself, and names exactly its `https://` URL;
//! - a link's code is kept only for a host under the allowlist, with a
//!   token that is not empty, and its hint only for a user on the link's
//!   `account_provider`;
//! - text with whitespace inside it is never a link.

#![no_main]

use familychat_signin::{Allowlist, LoginLink, ServerName, link::user_server};
use libfuzzer_sys::fuzz_target;

fn check_server_name(text: &str) {
    let Ok(name) = ServerName::parse(text) else {
        return;
    };
    let shown = name.to_string();
    assert!(
        shown.bytes().all(|b| b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || matches!(b, b'.' | b'-' | b':')),
        "{shown:?} is not bare"
    );
    assert_eq!(ServerName::parse(&shown).as_ref(), Ok(&name), "{shown:?}");
    let url = name.https_url().expect("an accepted name names its URL");
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(name.host()));
    assert_eq!(url.path(), "/");
}

fn check_link(text: &str, allowlist: &Allowlist) {
    let Ok(link) = LoginLink::parse(text, allowlist) else {
        return;
    };
    assert!(
        !text.trim().chars().any(char::is_whitespace),
        "a link with whitespace inside it was accepted"
    );
    let provider = link.account_provider.to_string();
    assert_eq!(
        ServerName::parse(&provider).as_ref(),
        Ok(&link.account_provider)
    );
    if let Some(code) = &link.code {
        assert!(
            allowlist.allows_name(&code.hs),
            "a code was kept for a host outside the allowlist"
        );
        assert!(!code.token.expose().is_empty(), "an empty token was kept");
        assert!(link.dropped_code.is_none());
    }
    if let Some(hint) = &link.login_hint {
        assert_eq!(
            user_server(hint).as_ref(),
            Some(&link.account_provider),
            "a hint for another server was kept"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    check_server_name(text);
    let allowlist = Allowlist::production();
    check_link(text, &allowlist);
    // The same text as a link's query, so the fuzzer spends its time on
    // the parameters rather than on finding the fixed prefix.
    check_link(
        &format!("https://safechat.family/app/login?{text}"),
        &allowlist,
    );
    check_link(
        &format!("familychat://safechat.family/app/login?{text}"),
        &allowlist,
    );
});
