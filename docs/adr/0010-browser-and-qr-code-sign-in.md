# ADR 0010: Sign-in in the Browser and with a QR Code

## Status

Proposed with Phase 14 (issue #15); accepted when that pull request
merges. It adds to ADR 0006 (every sign-in rule lives in
`familychat-signin`) the sign-ins a server may offer beyond a password and
a Family Chat code: OAuth 2.0 (MSC3861) and single sign-on in the
system's browser, and QR code sign-in (MSC4108), ready for Family Chat
when family-chat #239 gives each family a Matrix Authentication Service
(MAS). The harness gains Synapse with MAS (ADR 0004).

## Context

- More and more Matrix servers sign in through an OAuth 2.0 authorization
  server (MSC3861; matrix.org and Element's servers run MAS), some only
  through it. family-chat #239 plans one MAS per family.
- A QR code lets a signed-in phone sign a new device in, handing it the
  account's keys, so the new device is confirmed at once (MSC4108; MSC4388
  is its successor). Element X does this on mobile.
- Family Chat's Palpo offers neither today (PLAN.md section 6.10): no
  `auth_metadata`, and its rendezvous discovery answers `false`. A family
  must see exactly what it sees now.
- These sign-ins send people to web pages and hand tokens between
  devices, so the threats of PLAN.md section 7 apply with new paths: a
  crafted server or a stranger's device could send the person's browser,
  or a Family Chat account's tokens, somewhere that is not Family Chat.
- matrix-sdk 0.19.1 has an OAuth API (refresh, revocation, the QR flows)
  and an optional `local-server` feature (an axum server for the loopback
  redirect). Its QR flows implement MSC4108's channel only: an MSC4388 QR
  code is refused (`UnsupportedQrCodeType`), although Synapse 1.164
  implements MSC4388's rendezvous (unadvertised, `msc4388_mode`).

## Decision

### Where it lives

- **Rules** in `familychat-signin`, as ever: `methods` (what a server
  offers), `oauth` (the authorization server, checked; registration; PKCE;
  the code exchange; revocation), `loopback` (the redirect listener), and
  the flow (`flow.rs`'s Phase 14 half): the server step, the methods, the
  browser and QR sign-ins, signing a new device in.
- **The SDK parts** in `familychat-session`: restoring an OAuth session
  (`AuthKind::OAuth`/`QrCode` with the client id, through the SDK's OAuth
  API, so its refresh and sign-out use the server's OAuth endpoints), and
  `qr_login` (`AccountManager::add_account_with_qr_code` and
  `link_device_with_qr_code` over the SDK's `login_with_qr_code().generate()`
  and `grant_login_with_qr_code().generate()`), checking each address
  through a `UrlGuard` the sign-in hands it.
- **What the frontends draw** in `familychat-core::signin`: `ServerMethods`
  on the password form, and the steps `Server`, `ConfirmBrowser`,
  `Browser` (with a `BrowserUrl`, whose `Debug` hides it), `QrCode`,
  `CheckCode`, `DeviceLinked` and `Stopped`; the answers `ChooseServer`,
  `ChangeServer`, `UseBrowser`, `UseQrCode` and `SubmitCheckCode`;
  `SigninStart::LinkDevice`.
- **No new matrix-sdk features**: `qrcode` was on already (Phase 8);
  `local-server` (axum, tower) is not used, because the sign-in's own
  loopback listener is smaller and stricter (below). New dependencies are
  already in the graph: `base64`, `sha2` (the SDK's line),
  `form_urlencoded` and `getrandom` in `familychat-signin`; `rqrr` (the
  window's) and `matrix-sdk` as its dev-dependencies, for the live tests.

### What a server offers

`methods::probe` asks the checked homeserver, with unauthenticated `GET`s
only, before anything else is sent:

| Way | Shown when |
| --- | --- |
| Password | `m.login.password` in `GET /login`'s flows (or no answer), and no OAuth server: with one, its compatibility password login is left out, as Element X does (MSC3824) |
| "Continue in your browser", OAuth | `GET /_matrix/client/v1/auth_metadata` (then MSC2965's unstable path) names an authorization server that passes the checks below, with dynamic registration, `S256` and the `code` response type |
| "Continue in your browser", single sign-on | no OAuth server (and none refused), and `m.login.sso` in the flows |
| "Sign in with a QR code" | the OAuth server lists the device authorization grant with its endpoint, and `/versions` turns `org.matrix.msc4108` on |

"Use another Matrix server" now asks for the server first (the `Server`
step), finds it (discovery as before; a homeserver under the allowlist is
a Family Chat one, whichever way in) and shows the form with its ways
(`PasswordForm::methods`; the server fixed, with Change server). "Sign in
with your password" stays one form (the code fallback depends on it); a
Family Chat server that refuses the password as an unknown login type
(400 or 404, `M_UNRECOGNIZED`/`M_UNKNOWN`, as an OAuth-only server does)
offers its ways on the same form instead. On Palpo nothing changes: the
capability tests prove the browser and QR code are not offered there.

### The issuer and the allowlist

For a **Family Chat homeserver** (one that passed the allowlist), every
address its sign-ins use must pass the **same allowlist**: `https://`, a
host name (not an address), no user info, a strict subdomain of
`safechat.family` (and the build's staging or harness suffix). That is the
OAuth issuer and each endpoint the app uses (authorization, token,
registration, revocation, device authorization), the rendezvous a QR code
names, a homeserver the other device names during a QR sign-in, and a new
device's approval address (which must also be on the issuer's or the
device endpoint's origin). One address outside refuses the server's OAuth
altogether: no browser, no QR code, and no single sign-on fallback (which
would lead to the same server). `CheckedHomeserver::check_related` is the
one check.

It is not tighter (the homeserver's own host, or the same family's): #239
has not fixed MAS's address, and both `auth.smith.safechat.family` and a
shared `auth.safechat.family` fit. It is not looser: a Family Chat
account's browser sign-in never starts at, and its tokens never come from,
a page outside Family Chat's domains. What it asks of family-chat #239:
serve each family's MAS under `*.safechat.family`. Tightening it later to
the family's own subdomains is a one-line change of `check_related`.

For **another server**, each address needs `https://` and a host name
(the SDK's own `validate_urls` asks the same), and nothing more, as for
its homeserver.

### OAuth in the browser

All of it before the hand-off goes through the sign-in's own `Transport`
(no redirects, no retries, ADR 0006), never matrix-sdk:

1. "Continue in your browser" asks first: `ConfirmBrowser`, "Continue in
   your browser?", with the issuer's **full address**. Nothing has been
   sent to the issuer yet.
2. On Continue (one sign-in at a time, the code's gate): a loopback
   listener on `127.0.0.1` on a port the OS picks; dynamic client
   registration (RFC 7591) as a native public client named **Family Chat
   Desktop** (`client_uri` `https://safechat.family/`), its redirect
   `http://127.0.0.1:<port>/callback` (MAS matches loopback ports exactly,
   so each sign-in registers its own); a fresh state, a PKCE verifier
   (S256) and a device id (`urn:matrix:org.matrix.msc2967.client:device:…`,
   with the API scope, as the pinned SDK asks).
3. `Browser`: the frontend opens the authorization address **once**, and
   only after its own Continue in that dialog (a browser step a frontend
   did not confirm opens nothing, so a fixture or a coalesced step cannot
   open a browser), shows the issuer, and offers to open it again (the
   terminal also to copy it, for a browser elsewhere). It never shows,
   logs or keeps the address, which carries the state.
4. The loopback (`loopback.rs`) ends the wait only for `GET /callback`
   with this sign-in's state; anything else (another path or method, a
   program on this computer without the state, a request over 16 KiB or
   slower than 10 s) gets an error page and the wait goes on. The page the
   browser gets loads nothing (`default-src 'none'`). Ten minutes is the
   most the flow waits; Cancel goes back to the form.
5. `error=access_denied` is "declined"; a code is exchanged once at the
   token endpoint with the verifier; `whoami` must report the scoped
   device, or the tokens are revoked. Then the hand-off: `AuthKind::OAuth`
   with the client id, the post-login check (`FamilyChatCheck` with the
   user, or `OtherServerCheck`), as every sign-in.

After the hand-off the SDK owns the session: the account restores through
`client.restore_session(OAuthSession)`, refreshes through the server's
token endpoint (each rotation written to the keychain inside the refresh,
ADR 0005), and **signing out revokes** (the SDK's `OAuth::logout`, which
ends MAS's whole session); credentials the sign-in does not keep are
revoked at the checked revocation endpoint instead of `/logout`. The
refresh reads the server's metadata again from the homeserver, which was
checked; an attacker who controls a checked homeserver's metadata controls
the account anyway.

### Single sign-on

The same confirmation (the homeserver's address), loopback and wait; the
browser goes to `/_matrix/client/v3/login/sso/redirect` with the loopback
as `redirectUrl`, carrying a state of the app's (servers append
`loginToken` and keep what is there). The login token is redeemed with
`m.login.token` at the checked homeserver through the `Transport`
(`AuthKind::Sso`), exactly as a Family Chat code is (ADR 0006).

### QR codes

- **This computer signs in** ("Sign in with a QR code"): the session makes
  the account's stores, the SDK makes the rendezvous on the checked
  homeserver and the code; the guard checks the rendezvous before it is
  shown (`QrCode`); once a signed-in device scans it, the person types its
  two-digit check code (`CheckCode`; anything but two digits is asked
  again; a wrong one ends the channel, "That code didn't match"); the
  other device approves the device grant; the guard checks the homeserver
  the SDK is about to use (MSC4108 lets the other device name one); the
  account's secrets arrive and the device is confirmed. The post-login
  check (the homeserver, no bound user) runs before anything of the
  account is stored; a refusal, or an account signed in already, signs the
  new device out (revoking it) and forgets it. The client that signed in is
  the account's client (`Account::open` with `signed_in`), because the
  device's keys are in its stores.
- **Signing a new device in** ("Sign in a new device", the `LinkDevice`
  command, from an open account): the account's server must offer QR codes
  (else `Stopped`); the code is shown, the new device's check code typed;
  the new device then sends the address to approve it at, which must be on
  the account's authorization server (or `Stopped`, nothing opened); "Approve
  the new device in your browser?" with the issuer's address; on Continue
  the browser opens it and the SDK carries on; the account's secrets go to
  the new device once the server has made it. The account needs its
  identity on this computer ("Confirm it's you" first, otherwise).
- The drawing is `QrModules`, as the verification's, in a dark-on-white
  box; the check code and the server's user code are the only things
  shown. MSC4388 waits for the SDK.

### The frontends

- **The window**: the server step; the form with "Continue in your
  browser" (primary when there is no password) and "Sign in with a QR
  code", each with its sentence; the "Continue in your browser?" dialog
  (the only place the view sends `Continue` for a browser); the wait; the
  QR code; the check code; signing a new device in over the rooms, ending
  with "New device signed in" or why it stopped. Fixture views and
  screenshots for each, in dark and light, with view tests for names and
  24 px targets.
- **The terminal**: the server step, the form's ways (Ctrl-B for the
  browser, Ctrl-E to change the server), the question answered `Continue`
  only by `y` with the full address, and the wait (the system's opener, `o`
  again, `c` to copy). It prints no address with a query. QR codes are the
  window's: the form says so, and `LinkDevice` is in `commands::LATER`
  (a QR code a phone can read from a terminal, sized to its font, is later
  work).

## The rules, rule by rule

Tests in `crates/familychat-signin` unless named otherwise.

| Rule | Tests |
| --- | --- |
| Each way shown only where the server has it; none on Palpo | `browser_and_qr::palpo_offers_the_password_only`, live `live_oauth::the_browser_and_qr_codes_are_offered_only_where_they_work` (Palpo, Synapse, Synapse with MAS), `familychat-test-support` live `only_synapse_with_mas_offers_oauth_and_the_qr_code_rendezvous` |
| A Family Chat homeserver's issuer and endpoints only under the allowlist; another server's over HTTPS | `oauth::tests::a_family_chat_homeservers_issuer_must_be_under_the_allowlist`, `allowlist::tests::related_addresses_follow_the_homeservers_rule`, `properties::a_family_chat_issuer_is_accepted_only_under_the_allowlist`, `browser_and_qr::a_family_chat_homeservers_issuer_outside_the_allowlist_is_never_offered` (nothing sent off the homeserver) |
| The browser opens only after Continue on the full address | `browser_and_qr::an_oauth_sign_in_opens_the_browser_only_after_continue_and_keeps_its_client`, `browser_and_qr::cancelling_before_the_browser_sends_nothing_and_a_declined_sign_in_says_so`, `familychat-ui` `view_tests::browser_and_qr::*`, `familychat-tui` `another_servers_browser_sign_in_opens_only_after_y_and_prints_no_address_query` |
| The loopback takes only this sign-in's state | `loopback::tests::only_this_sign_ins_redirect_ends_the_wait`, `loopback::tests::a_request_that_is_too_large_is_refused`, `browser_and_qr::an_oauth_sign_in_…` and `single_sign_on_redeems_its_token_at_the_checked_homeserver` (another program's request) |
| No redirects or retries at the authorization server | `browser_and_qr::a_redirect_at_the_authorization_server_is_not_followed` |
| PKCE S256, a fresh state and device; registration names Family Chat Desktop | `oauth::tests::an_authorization_request_carries_pkce_and_a_device`, `oauth::tests::the_registration_names_family_chat_desktop` |
| OAuth sign-in, refresh in the keychain, sign-out revokes | live `an_oauth_sign_in_keeps_its_tokens_refreshed_and_its_sign_out_revokes`; `familychat-security` `sessions_round_trip_and_never_print` (the client id kept) |
| QR codes: addresses guarded, check code typed, the post-login check, signed in already | `browser_and_qr::a_qr_code_sign_in_checks_its_addresses_and_the_check_code`, `a_qr_code_sign_in_into_another_server_than_the_family_is_refused`, live `this_computer_signs_in_with_a_qr_code_a_signed_in_device_scans` |
| A new device's approval address only on the account's authorization server | `browser_and_qr::linking_a_phone_opens_only_the_accounts_own_authorization_server`, `linking_a_phone_where_the_server_cant_says_so`, live `a_signed_in_computer_signs_a_new_phone_in_with_its_qr_code` |
| A Family Chat server without passwords offers its other ways | `browser_and_qr::a_family_server_that_takes_no_password_offers_the_browser` |
| Wording | `wording::tests::the_browser_and_qr_sentences_name_no_secret_and_end_cleanly` |

## Consequences

- Other servers that sign in with OAuth or single sign-on work, in the
  window and the terminal; QR codes work in the window, both ways.
- Family Chat families see nothing new until their server offers it;
  then the same code paths apply the Family Chat rules, and the post-login
  check runs as for a code.
- The harness runs Synapse with MAS (ADR 0004), so the OAuth and QR flows
  are tested end to end against the real thing, the browser played by an
  HTTP client over MAS's forms (MAS 1.28.0's; `docs/upstream/homeservers.md`).
- Single sign-on is tested offline only: the harness's only single sign-on
  is MAS's compatibility layer, which the app does not offer next to OAuth.
- MSC4388 is not supported until the SDK supports its channel; its QR
  codes are refused and its rendezvous not looked for.
- The password form of "Sign in with your password" still asks for the
  password first; if #239 makes families OAuth-only, that page may want
  the server step too (a frontend change; the flow already offers the ways).
- The live tests found a stall in the pinned SDK: its encryption set-up
  after a restore (turning backups on) run at the same time as the sync's
  first key upload, or as making the account's identity, could leave both
  waiting for good (an account in about five). The supervisor now lets
  that set-up finish (at most 15 seconds, so an account offline still
  syncs when it can) before it starts the sync and the identity set-up
  (ADR 0005); the QR code sign-in's client is already past it.
- Registering a client per sign-in leaves registrations on the server
  (MAS deduplicates identical metadata; a new loopback port is a new
  client).
