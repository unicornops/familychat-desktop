# ADR 0006: Sign-in Rules and Flows

## Status

Proposed with Phase 4 (issue #5); accepted when that pull request merges.
It records PLAN.md, sections 6.2 to 6.6 and 7 (sign-in), as built, and
maps every rule of family-chat's `docs/client-login-links.md` ("What each
client must implement") to the test that proves it.

## Context

A Family Chat sign-in link carries a single-use bearer credential for
someone else's account (usually a child's), and a crafted link or QR code
is the first threat in PLAN.md section 7: it could phish a password, or
sign a child into an account a stranger controls. The contract was
settled in the reviews of the iOS, Android and web forks (family-chat
#254), and each fork had review findings that are now regression cases:

- iOS: an allowlist that fell back to the raw string when Foundation could
  not parse it let `https://evil.example\.safechat.family` and
  `evil.example\@a.safechat.family` through to the SDK, whose WHATWG
  parser reads `\` as `/` and so connected to `evil.example` (the
  "backslash bypass").
- Android: the password path forced the apex `safechat.family` as the
  account provider, overriding the link's `account_provider` and pointing
  at the website, whose well-known is HTML (the "apex fallback"); a later
  fix stopped the fallback from using `hs` instead of `account_provider`.
- All three: a `307` or `308` on `/login` would re-send the token or the
  password wherever it points; the SDKs' `respect_login_well_known` could
  re-point a client after login, which is why the post-login re-check
  exists.

The desktop has two frontends, so the rules must live in one place that
neither frontend can bypass, and the frontends must stay free of
matrix-sdk (ADR 0001).

## Decision

### The crate and its boundary

`familychat-signin` holds every rule, the wording and one state machine
for every flow (`flow::run_flow`). It depends on `familychat-session`
(the hand-off to matrix-sdk), so it is a Matrix crate, and the
dependency-boundary check keeps it out of both frontends (its self-test
proves the check sees matrix-sdk in its graph).

The frontends drive it through `familychat-core::signin`, which has no
network or SDK: `SigninApi::start(SigninStart) -> SigninFlow`, a
`watch::Receiver<SigninStep>` of what to draw (`Confirm`, `Working`,
`Password`, `Finished`, `AlreadySignedIn`, `NotALink`, `Cancelled`) and an
unbounded sender of `SigninAnswer`s (`Continue`, `UsePassword`,
`SubmitPassword`, `Cancel`). `LiveSignin` implements it on the background
runtime; `familychat-test-support`'s `FixtureSignin` is the fixture the
window's screenshots and view tests draw (ADR 0011).
Dropping the flow cancels it and wipes a code not yet redeemed.

### Server names, the allowlist and the environments

`ServerName::parse` is the only way into a server name. It accepts an
optional leading `https://` (any case), a DNS name of ASCII letters,
digits, `-` and `.` (labels of 1 to 63, no empty label, no trailing dot,
at most 253) and an optional `:port` (1 to 65535, no leading zero), and
lower-cases it. It refuses everything else before splitting anything:
`\`, `@`, `%`, `/`, `?`, `#`, whitespace, controls, brackets (so IPv6
literals), `_` and non-ASCII. The URL it builds is checked to come back
with exactly the same host and port, which also refuses names a URL
parser reads as IPv4 addresses (`127.0.0.1`, `0x7f000001`). It is
stricter than the forks in two places: no trailing `/` and no surrounding
whitespace in a link's parameters (the password form trims what is typed
and forgives one trailing `/` before parsing).

`Allowlist` is a set of DNS suffixes matched as strict subdomains:
`safechat.family` in every build, `familychat.dev` with the `staging`
feature, `familychat.test` (the harness) with `test-harness`.
`scripts/check-release-features.sh` fails if a release build of the app
could enable either feature on a Family Chat crate (and self-tests both),
and a release build with `test-harness` does not compile. The allowlist
is applied to the **resolved** homeserver URL (`https://`, a host name,
no user info) and to a link's `hs`. Its only output is a
`CheckedHomeserver`, and the login functions take nothing else, so no
code path can send a credential to an unchecked URL;
`CheckedHomeserver::other_server` is the HTTPS-only check for "Use
another Matrix server".

### Discovery

`GET https://<name>/.well-known/matrix/client`: a `200` whose JSON names
`m.homeserver.base_url` gives the homeserver (a `base_url` that is not a
URL is an error); anything else (`404`, a redirect, HTML) means the name
is its own homeserver. Then the allowlist (Family Chat), then
`GET /_matrix/client/versions` on the resolved homeserver. So the only
request a Family Chat sign-in sends toward a server outside the allowlist
is that unauthenticated well-known `GET`. Discovery follows no redirects
either; a custom domain whose well-known redirects is treated as having
none (Family Chat's domain verification serves it directly).

### Links

`LoginLink::parse` takes `https://safechat.family/app/login?...`,
`familychat://safechat.family/app/login?...` (also `/app/login/`, the
website's fallback page), or either pasted with whitespace around it, and
nothing else (text with words around a link, other hosts, ports, user
info, the web app's form). The query is decoded as the form it is built
as. `account_provider` is required and strict; `login_hint` is kept only
as `mxid:` plus a valid Matrix ID (printable ASCII localpart) on
`account_provider`, otherwise dropped (the contract's wording, which is
iOS's behaviour: the code is kept and then bound to `account_provider`);
`hs` and `token` are kept only together, non-empty, with `hs` strict and
allowlisted, otherwise the token is wiped unsent and the form says why. A
parameter given twice is ambiguous and drops what it belongs to (the
mobile apps take the first value).

### The code flow

As PLAN.md section 6.4: already signed in (by the hint, before anything is
sent; or by the redeemed user, whose new device is then signed out) stops
with `AlreadySignedIn` and keeps nothing; otherwise `Confirm` ("Sign in as
`<mxid>`?", or "Sign in to `<server>`?"). On Continue, under a gate shared
by every flow (one redemption in flight; a second is refused, not
queued), the token goes once to `https://<hs>/_matrix/client/v3/login` as
the contract's body with the device name, through the sign-in's own
`ReqwestTransport`: redirects off, reqwest's retries off
(`retry::never()`), no pooled connection reused, HTTPS only, TLS 1.2+,
timeouts. matrix-sdk is not involved in any sign-in request, so neither
its retries nor its `well_known` handling can touch a credential. Only a
`200` with an access token, a device id and a valid user id is a sign-in;
an answer with an access token and nothing usable is signed out. The user
is bound to the hint, or with no hint to `account_provider`'s server;
otherwise the device is signed out and refused. `401`/`403` is "That code
has been used or has expired. Ask your parent for a new one."; every
failure falls through to the password form.

### Password sign-in

The form a link leads to is pre-filled from `account_provider` and the
hint's localpart; `hs` exists only inside the code, which the password
path never sees, so a password cannot go to it even when discovery fails.
Typed servers and full Matrix IDs go through `ServerName::parse`,
discovery and (for Family Chat) the allowlist before the
`m.login.password` request, which is sent once: `M_LIMIT_EXCEEDED` (or a
`429`) comes back with its wait (`SigninProblem::retry_after`) and is
never retried. The empty Family Chat server field shows
`yourfamily.safechat.family`, never the apex.

### The hand-off and the post-login re-check

Credentials become an `AccountSession` (`AuthKind::LoginToken` or
`Password`) and go to `AccountManager::add_account` through the
`AccountSink` trait, which restores them with `restore_session` and runs a
`PostLoginCheck` before anything syncs: `FamilyChatCheck` (the client's
homeserver under the allowlist, and the user the sign-in bound) for every
Family Chat sign-in, `OtherServerCheck` (HTTPS and the user) otherwise. A
refusal is signed out and forgotten by the session crate; any other
failure after the credentials were issued (no native sliding sync, the
keychain) is signed out by the sign-in (best effort, the outcome logged).

### Phase 14: the browser and QR codes

ADR 0010 adds the sign-ins a server may offer beyond these: OAuth 2.0 and
single sign-on in the browser, and QR codes (MSC4108), shown only where
the server has them. They keep this ADR's rules: discovery and every
credential go only to a `CheckedHomeserver`; every request before the
hand-off goes through the `Transport` (the QR code's secure channel is
the SDK's, with every address it uses checked); for a Family Chat
homeserver the issuer and every address are under the allowlist; the
browser opens only after a confirmation that shows the whole address; and
the post-login re-check runs as for a code. "Use another Matrix server"
asks for the server first, and any homeserver under the allowlist is a
Family Chat one there too (the Family Chat check, not the other server's).

### Secrets

Tokens, access tokens and passwords are `Secret`s (or core's
`SecretText`) from the moment they are parsed, typed or received:
`Debug` prints `[redacted]`, they are wiped when dropped, a code is wiped
as soon as its request body is built, and request bodies are serialised
straight into wiped buffers. No log line, error or step carries one, and
nothing of a flow is persisted.

### Device names

`Family Chat (macOS)`, `Family Chat (Windows)`, `Family Chat (Linux)`.

## The contract, rule by rule

Tests in `crates/familychat-signin` unless named otherwise: `src/…`
unit tests, `tests/flows.rs` (offline, a scripted server and a fake
account manager, every OS), `tests/properties.rs` (proptest),
`tests/mocked_signin.rs` (the real account manager against wiremock),
`tests/logs.rs`, and `tests/live_signin.rs` (the harness, CI's quality
job).

| Contract rule | Tests |
| --- | --- |
| Allowlist `*.safechat.family`, subdomains only, never the apex | `allowlist::tests::production_allows_family_subdomains_only`, `host::tests::subdomains_only`, `allowlist::tests::names_are_checked_the_same_way`, `properties::the_allowlist_takes_strict_subdomains_only` |
| Applied to the resolved homeserver URL, not a name; custom-domain families get in | `flows::allowlist_a_custom_domain_is_allowed_by_where_it_resolves`, `flows::allowlist_a_family_address_with_no_well_known_is_its_own_homeserver`, live `account_provider_smith_test_signs_in_through_its_well_known` |
| A server name parsed strictly as a bare host (`\`, `@`, `%`, `/`, `?`, `#`, whitespace, other schemes refused; optional `https://`) | `host::tests::refuses_everything_else` (the iOS backslash bypass and its relatives), `host::tests::accepts_bare_hosts_and_ports`, `properties::a_forbidden_character_anywhere_is_refused`, `properties::the_host_parser_never_panics_and_what_it_accepts_is_bare`, `properties::well_formed_names_parse_in_any_case_with_or_without_https`, `flows::allowlist_server_names_are_parsed_strictly_before_anything_is_sent` |
| Never hand a raw string to a URL parser that reads it differently | `host::tests::a_numeric_name_that_a_url_parser_reads_as_an_address_is_refused`, `allowlist::tests::a_backslash_in_a_resolved_url_cannot_smuggle_a_host` |
| Resolve through the well-known; only `https://` under the allowlist | `flows::allowlist_nothing_but_the_well_known_goes_to_a_server_outside_it` (another host, `http://`, a backslash `base_url`, the apex's HTML, a redirect), `flows::a_server_that_is_not_matrix_is_named` |
| No password, token or OAuth request before the check; only unauthenticated discovery | `flows::allowlist_nothing_but_the_well_known_goes_to_a_server_outside_it`, `flows::rule1_a_code_for_a_host_outside_the_allowlist_is_dropped_unsent`, live `a_code_for_a_non_allowlisted_hs_is_never_sent_and_the_password_goes_to_account_provider` (the access log) |
| Re-check the homeserver after every login; on a mismatch sign out and refuse | `handoff::tests::the_family_chat_check_refuses_another_server_or_user`, `mocked_signin::the_post_login_recheck_signs_out_a_homeserver_outside_the_allowlist` (the real account manager, the logout seen by the server), `flows::post_login_a_family_chat_sign_in_is_rechecked_against_the_allowlist` |
| Refusal copy "This server isn't a Family Chat server" | `wording::tests::the_contract_sentences`, the flows above |
| 1. Parse the four parameters as a query string | `link::tests::reads_the_link_the_panel_builds`, `link::tests::the_query_decodes_as_a_form`, `link::tests::accepts_the_desktop_scheme_the_fallback_path_and_pasted_whitespace`, `link::tests::refuses_other_urls_and_text`, `link::tests::account_provider_is_required_and_strict`, `properties::links_the_panel_builds_round_trip`, `properties::the_link_parser_never_panics`, `flows::rule1_text_that_is_not_a_link_is_refused` |
| 1. `hs` and `token` only together, `hs` bare and allowlisted, else the token is dropped and the link degrades | `link::tests::a_code_is_kept_only_with_an_allowlisted_bare_hs`, `properties::a_kept_code_is_always_for_an_allowlisted_bare_host`, `flows::rule1_a_code_for_a_host_outside_the_allowlist_is_dropped_unsent`, live `a_code_for_a_non_allowlisted_hs_is_never_sent_and_the_password_goes_to_account_provider` |
| 1. A hint on another server than `account_provider` is dropped | `link::tests::a_hint_for_another_server_is_dropped`, `flows::rule1_a_hint_on_another_server_is_dropped_and_the_code_binds_to_account_provider` |
| 2. Already signed in: say so, don't redeem, don't keep the link | `flows::rule2_an_account_already_signed_in_is_not_redeemed`, `flows::rule2_a_code_without_a_hint_for_a_signed_in_account_signs_the_new_device_out`, `mocked_signin::already_signed_in_is_read_from_the_registry`, live `a_code_signs_in_and_its_replay_…` (a fresh code for a signed-in account stays unused) |
| 3. Ask first: "Sign in as `<mxid>`?" (or the server), Continue or Cancel | `flows::rule3_a_code_asks_first_and_sends_nothing_until_continue`, `flows::rule3_declining_the_code_offers_the_password_form_and_sends_nothing`, `flows::rule3_a_frontend_that_goes_away_cancels_and_sends_nothing`, live (every code test checks the question) |
| 4. `POST m.login.token` to `https://<hs>`, redirects off, no retry, one in flight | `flows::rule4_continue_redeems_the_code_once_at_hs` (the exact body), `flows::rule4_a_custom_domain_code_goes_to_hs_without_discovery`, `flows::rule4_only_one_redemption_is_in_flight`, `flows::rule4_a_redirect_is_not_followed`, `flows::rule6_a_rate_limited_code_shows_the_wait_and_is_not_retried`, `http::tests::the_client_refuses_plain_http`, live `a_307_on_login_is_not_followed_for_a_code_or_a_password` (the access log: the `307`, nothing at the target, the code still unused) |
| 5. Bind `user_id` to the hint, or to `account_provider`; else sign out and refuse | `flows::rule5_a_code_that_signs_in_someone_else_is_signed_out_and_refused`, `link::tests::users_compare_by_localpart_and_server`, live `a_code_that_signs_in_someone_else_is_signed_out_and_refused` (the device is gone from the account) and the second half of `account_provider_smith_test_signs_in_through_its_well_known` |
| 6. `401`/`403` is "used or expired"; any later failure signs the device out | `flows::rule6_a_401_or_403_says_used_or_expired_and_offers_the_password`, `flows::rule6_a_failure_after_the_credentials_were_issued_signs_the_device_out`, `flows::rule6_a_login_answer_without_a_usable_account_is_signed_out`, live `a_code_signs_in_and_its_replay_is_used_or_expired_with_a_password_offered` |
| 7. The password form pre-filled from `account_provider` (resolved and checked) and the hint; never to `hs`, even when discovery fails | `flows::rule7_after_a_used_code_the_password_goes_to_account_provider`, `flows::rule7_a_crafted_link_never_collects_a_password_for_hs`, `flows::rule7_when_discovery_fails_the_password_still_never_goes_to_hs`, live `a_code_for_a_non_allowlisted_hs_is_never_sent_and_the_password_goes_to_account_provider` |
| 8. Never log, persist or restore the token; memory only | `logs::rule8_no_token_or_password_reaches_a_log_line_even_unredacted` (no redaction, TRACE everywhere), `flows::rule8_no_token_in_debug_or_a_panic_message`, `link::tests::debug_never_prints_the_token`, `http::tests::debug_hides_bodies_tokens_and_queries`, `familychat-core` `signin::tests::secrets_never_print`, live (the captured log of the whole binary, and the profile's files, hold no token or password) |
| Desktop: redact a `familychat://` link's query from logs | `familychat-security` `redact::tests::urls_lose_user_info_query_and_fragment`, `redact::tests::scrub_removes_every_kind_of_secret` |
| PLAN.md 6.5: `M_LIMIT_EXCEEDED` shown with its wait, never retried | `flows::password_errors_are_shown_and_a_rate_limit_is_never_retried`, `flows::rule6_a_rate_limited_code_shows_the_wait_and_is_not_retried`, `wording::tests::rate_limits_say_how_long` |
| PLAN.md 6.2: other servers by password (the server first, Phase 14) | `flows::other_servers_need_only_https`, live `another_server_signs_in_with_a_password_on_synapse` |
| PLAN.md 6.6 and 7 for OAuth and QR codes: the allowlist on the issuer and every address, the confirmation before the browser, the re-check | ADR 0010's table (`tests/browser_and_qr.rs`, `tests/properties.rs`, live `tests/live_oauth.rs`) |
| PLAN.md 6.2: device names | `device::tests::names_each_desktop_system`, live `a_code_signs_in_…` (the device's name on the server) |

Not applicable on the desktop: the web app's URL stripping and soft
logout (there is no URL bar or soft-logged-out session to redeem into),
and the mobile apps' universal links (the `familychat://` registration is
Phase 12, the hand-off to a running window Phase 9).

## Consequences

- Both frontends get every rule and sentence by drawing `SigninStep`s;
  Phase 5 (the window) and Phase 10 (the terminal) add no rules.
- The harness proves where credentials went from its access log, so the
  live tests do not trust the client's own account of itself.
- One Palpo has one server name, so no live test redeems a code for a
  `@kid:smith.test` account; the offline flow tests do, against a
  scripted server, and ADR 0004 says how a second Palpo would add it.
- No fuzz target yet: `cargo-fuzz` needs a nightly toolchain and this
  repository pins Zed's stable one. The property tests run the parsers
  over arbitrary strings on every OS instead; PLAN.md section 7's fuzz
  targets remain to be added with a nightly CI job (Phase 13).
- A server whose well-known redirects is treated as having none; if a
  Family Chat custom domain ever needs that, discovery (and only
  discovery, which carries no credential) could follow same-scheme
  redirects to a limit.
- Refresh tokens are not requested at sign-in (the contract's body has
  none); an account whose server issues them anyway is handled by ADR
  0005's rotation.
