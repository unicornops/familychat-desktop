//! Property tests for the server-name and link parsers (PLAN.md, section
//! 9): whatever the input, they never panic, never accept a character a
//! bare host cannot have, and never keep a code for a host outside the
//! allowlist. The fork reviews' regression cases are unit tests next to
//! the parsers (`src/host.rs`, `src/link.rs`); here they are generalised.

use familychat_core::UserId;
use familychat_signin::{
    Allowlist, CheckedHomeserver, LoginLink, ServerName,
    link::user_server,
    oauth::{AuthServer, AuthServerRefusal},
};
use proptest::prelude::*;

/// A DNS label.
fn label() -> impl Strategy<Value = String> {
    "[a-z0-9]([a-z0-9-]{0,10}[a-z0-9])?"
}

/// A dotted DNS name of one to four labels whose last label has a letter
/// (so a URL parser cannot read it as an address).
fn dns_name() -> impl Strategy<Value = String> {
    (prop::collection::vec(label(), 0..3), "[a-z][a-z0-9]{0,6}").prop_map(|(labels, tld)| {
        let mut parts = labels;
        parts.push(tld);
        parts.join(".")
    })
}

/// What a bare host can never contain.
fn forbidden() -> impl Strategy<Value = char> {
    prop::sample::select(vec![
        '\\', '@', '%', '/', '?', '#', ' ', '\t', '\n', '\r', '\u{0}', '[', ']', '_', '\u{a0}',
        '\u{3002}', 'і', '\u{200b}',
    ])
}

fn query_value() -> impl Strategy<Value = String> {
    prop_oneof![
        dns_name(),
        dns_name().prop_map(|name| format!("{name}.safechat.family")),
        any::<String>(),
        "[ -~]{0,40}",
    ]
}

fn encode(pairs: &[(&str, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

/// What an authorization server's metadata names, with `issuer` as every
/// address's host.
fn metadata(issuer: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}authorize"),
        "token_endpoint": format!("{issuer}token"),
        "registration_endpoint": format!("{issuer}register"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    // ADR 0010: for a Family Chat homeserver, an OAuth issuer (and so
    // every address a browser or QR sign-in uses) is accepted only under
    // the allowlist, over HTTPS, with a name; whatever else it is, it is
    // refused, and the parse never panics.
    #[test]
    fn a_family_chat_issuer_is_accepted_only_under_the_allowlist(
        name in dns_name(),
        scheme in prop::sample::select(vec!["https", "http"]),
        under in any::<bool>(),
        text in any::<String>(),
    ) {
        let allowlist = Allowlist::production();
        let homeserver = allowlist
            .check(&reqwest::Url::parse("https://smith.safechat.family").expect("url"))
            .expect("allowed");
        let host = if under { format!("{name}.safechat.family") } else { name.clone() };
        let issuer = format!("{scheme}://{host}/");
        let result = AuthServer::from_metadata(&metadata(&issuer), &homeserver, &allowlist);
        let allowed = scheme == "https" && host.ends_with(".safechat.family");
        prop_assert_eq!(result.is_ok(), allowed, "{}", issuer);
        if !allowed {
            prop_assert!(matches!(result, Err(AuthServerRefusal::NotAllowed("issuer", _))));
        }
        // Arbitrary text as an issuer is never accepted for a family.
        if !text.starts_with("https://") {
            prop_assert!(AuthServer::from_metadata(&metadata(&text), &homeserver, &allowlist).is_err());
        }
        // Another server's may be any HTTPS host.
        let other = CheckedHomeserver::other_server(
            &reqwest::Url::parse("https://matrix.org").expect("url"),
        )
        .expect("allowed");
        prop_assert_eq!(
            AuthServer::from_metadata(&metadata(&issuer), &other, &allowlist).is_ok(),
            scheme == "https"
        );
    }

    #[test]
    fn the_host_parser_never_panics_and_what_it_accepts_is_bare(text in any::<String>()) {
        if let Ok(name) = ServerName::parse(&text) {
            let shown = name.to_string();
            prop_assert!(shown.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b':')), "{shown}");
            prop_assert_eq!(ServerName::parse(&shown), Ok(name.clone()));
            let url = name.https_url().expect("it round-tripped once");
            prop_assert_eq!(url.host_str(), Some(name.host()));
        }
    }

    #[test]
    fn a_forbidden_character_anywhere_is_refused(
        before in dns_name(),
        bad in forbidden(),
        after in dns_name(),
        https in any::<bool>(),
    ) {
        let text = format!("{}{before}{bad}{after}", if https { "https://" } else { "" });
        prop_assert!(ServerName::parse(&text).is_err(), "{:?}", text);
        let suffixed = format!("{before}{bad}.safechat.family");
        prop_assert!(ServerName::parse(&suffixed).is_err(), "{:?}", suffixed);
    }

    #[test]
    fn well_formed_names_parse_in_any_case_with_or_without_https(
        name in dns_name(),
        port in proptest::option::of(1u16..),
        upper in any::<bool>(),
        https in any::<bool>(),
    ) {
        let mut text = if upper { name.to_ascii_uppercase() } else { name.clone() };
        if let Some(port) = port {
            text = format!("{text}:{port}");
        }
        if https {
            text = format!("https://{text}");
        }
        let parsed = ServerName::parse(&text).expect("a well-formed name parses");
        prop_assert_eq!(parsed.host(), name.as_str());
        prop_assert_eq!(parsed.port(), port);
    }

    #[test]
    fn the_allowlist_takes_strict_subdomains_only(name in dns_name(), glue in "[a-z0-9-]{0,3}") {
        let allowlist = Allowlist::production();
        let under = ServerName::parse(&format!("{name}.safechat.family")).expect("parses");
        prop_assert!(allowlist.allows_name(&under));
        // Glued on without a dot, or the suffix only part of the host.
        if let Ok(glued) = ServerName::parse(&format!("{name}{glue}safechat.family")) {
            prop_assert!(!allowlist.allows_name(&glued), "{glued:?}");
        }
        if name.ends_with("safechat.family") {
            return Ok(());
        }
        if let Ok(after) = ServerName::parse(&format!("{name}.safechat.family.{name}")) {
            prop_assert!(!allowlist.allows_name(&after), "{after:?}");
        }
        prop_assert!(!allowlist.allows_name(&ServerName::parse("safechat.family").expect("parses")));
    }

    #[test]
    fn the_link_parser_never_panics(text in any::<String>()) {
        let _ = LoginLink::parse(&text, &Allowlist::production());
        let _ = LoginLink::parse(&format!("https://safechat.family/app/login?{text}"), &Allowlist::production());
    }

    #[test]
    fn a_kept_code_is_always_for_an_allowlisted_bare_host(
        provider in query_value(),
        hint in query_value(),
        hs in query_value(),
        token in query_value(),
        hint_prefix in any::<bool>(),
    ) {
        let hint = if hint_prefix { format!("mxid:@ana:{hint}") } else { hint };
        let query = encode(&[
            ("account_provider", provider),
            ("login_hint", hint),
            ("hs", hs),
            ("token", token),
        ]);
        let allowlist = Allowlist::production();
        if let Ok(link) = LoginLink::parse(&format!("https://safechat.family/app/login?{query}"), &allowlist) {
            if let Some(code) = &link.code {
                prop_assert!(allowlist.allows_name(&code.hs), "{:?}", code.hs);
                prop_assert!(!code.token.expose().is_empty());
                prop_assert!(code.hs.https_url().is_ok());
            }
            if let Some(hint) = &link.login_hint {
                prop_assert_eq!(user_server(hint), Some(link.account_provider.clone()));
            }
            let debug = format!("{link:?}");
            if let Some(code) = &link.code {
                // The token must not show, unless it is also part of what
                // may show (a random token can be a substring of the server
                // name, as "x.safechat.family" is of "ax.safechat.family").
                let token = code.token.expose();
                let public = format!("{:?} {:?} {:?}", code.hs, link.account_provider, link.login_hint);
                prop_assert!(
                    !debug.contains(token.as_str()) || token.len() < 4 || public.contains(token.as_str()),
                    "the token shows in {:?}",
                    debug
                );
            }
        }
    }

    #[test]
    fn links_the_panel_builds_round_trip(
        family in label(),
        localpart in "[a-z0-9._=/+-]{1,20}",
        token in "[A-Za-z0-9_-]{8,40}",
        custom in any::<bool>(),
    ) {
        let serving = format!("{family}.safechat.family");
        let server_name = if custom { format!("{family}.ie") } else { serving.clone() };
        let mxid = format!("@{localpart}:{server_name}");
        prop_assume!(UserId::parse(mxid.as_str()).is_ok());
        let query = encode(&[
            ("account_provider", server_name.clone()),
            ("login_hint", format!("mxid:{mxid}")),
            ("hs", serving.clone()),
            ("token", token.clone()),
        ]);
        for base in ["https://safechat.family/app/login", "familychat://safechat.family/app/login"] {
            let link = LoginLink::parse(&format!("{base}?{query}"), &Allowlist::production()).expect("parses");
            prop_assert_eq!(link.account_provider.to_string(), server_name.clone());
            prop_assert_eq!(link.login_hint.as_deref().map(UserId::as_str), Some(mxid.as_str()));
            let code = link.code.expect("a code");
            prop_assert_eq!(code.hs.to_string(), serving.clone());
            prop_assert_eq!(code.token.expose(), &token);
        }
    }
}
