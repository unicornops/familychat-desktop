//! OAuth 2.0 sign-in in the browser (MSC3861; ADR 0010): the server's
//! authorization server, checked before anything is sent to it, dynamic
//! client registration naming Family Chat Desktop, the authorization code
//! grant with PKCE (S256) and a loopback redirect, the code exchange, and
//! revoking tokens a sign-in will not keep.
//!
//! Every request goes through the sign-in's own [`Transport`] (no
//! redirects, no retries), to addresses that passed
//! [`CheckedHomeserver::check_related`]: under the allowlist for a Family
//! Chat homeserver, any `https://` host for another server. Once an
//! account is open, matrix-sdk refreshes and revokes its tokens through
//! the SDK's OAuth API with the client id kept in the keychain.
//!
//! The authorization server is found from the homeserver
//! (`GET /_matrix/client/v1/auth_metadata`, then MSC2965's unstable path),
//! never from a well-known the login response carries.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use familychat_core::OwnedUserId;
use familychat_security::Secret;
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tracing::{info, warn};

use crate::{
    allowlist::{Allowlist, CheckedHomeserver, NotAllowed},
    http::{HttpRequest, HttpResponse, Transport, TransportError},
    link::parse_user_id,
    login::{Credentials, OAuthGrant},
};

/// What the client registration calls the app.
pub const CLIENT_NAME: &str = "Family Chat Desktop";
/// The client's home page in its registration.
pub const CLIENT_URI: &str = "https://safechat.family/";
/// The scope for the whole Client-Server API (MSC2967, as the pinned SDK
/// asks for it).
pub const SCOPE_API: &str = "urn:matrix:org.matrix.msc2967.client:api:*";
/// The prefix of the scope that binds the token to a device.
pub const SCOPE_DEVICE_PREFIX: &str = "urn:matrix:org.matrix.msc2967.client:device:";
/// The device authorization grant (RFC 8628), as QR code sign-in uses it.
pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

const AUTH_METADATA_PATHS: [&str; 2] = [
    "_matrix/client/v1/auth_metadata",
    "_matrix/client/unstable/org.matrix.msc2965/auth_metadata",
];

/// A homeserver's OAuth 2.0 authorization server, every address checked
/// by the homeserver's rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthServer {
    /// The issuer, as the person is shown it before the browser opens.
    pub issuer: Url,
    /// Where the browser goes.
    pub authorization_endpoint: Url,
    /// Where codes and refresh tokens are exchanged.
    pub token_endpoint: Url,
    /// Dynamic client registration (RFC 7591), which the app needs: it has
    /// no client id registered beforehand.
    pub registration_endpoint: Url,
    /// Token revocation (RFC 7009).
    pub revocation_endpoint: Option<Url>,
    /// The device authorization grant (RFC 8628), for QR code sign-in.
    pub device_authorization_endpoint: Option<Url>,
}

/// Why a homeserver's authorization server can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthServerRefusal {
    /// The homeserver names none (or the request failed).
    NotOffered,
    /// The metadata misses something the app needs (an endpoint,
    /// `S256`, the `code` response type).
    Unusable(&'static str),
    /// An address is outside what the homeserver's rule allows.
    NotAllowed(&'static str, NotAllowed),
}

impl AuthServer {
    /// Reads and checks the metadata `homeserver` serves.
    pub fn from_metadata(
        metadata: &Value,
        homeserver: &CheckedHomeserver,
        allowlist: &Allowlist,
    ) -> Result<Self, AuthServerRefusal> {
        let address = |field: &'static str| -> Result<Option<Url>, AuthServerRefusal> {
            let Some(text) = metadata.get(field) else {
                return Ok(None);
            };
            let text = text
                .as_str()
                .ok_or(AuthServerRefusal::Unusable("an address that is not text"))?;
            let url = Url::parse(text)
                .map_err(|_| AuthServerRefusal::Unusable("an address that is not a URL"))?;
            homeserver
                .check_related(&url, allowlist)
                .map(Some)
                .map_err(|refusal| AuthServerRefusal::NotAllowed(field, refusal))
        };
        let required = |field: &'static str| {
            address(field)?.ok_or(AuthServerRefusal::Unusable("a missing endpoint"))
        };
        let issuer = required("issuer")?;
        let authorization_endpoint = required("authorization_endpoint")?;
        let token_endpoint = required("token_endpoint")?;
        let registration_endpoint = required("registration_endpoint")?;
        let revocation_endpoint = address("revocation_endpoint")?;
        let device_authorization_endpoint = address("device_authorization_endpoint")?;
        let lists = |field: &str, value: &str| {
            metadata
                .get(field)
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().any(|item| item.as_str() == Some(value)))
        };
        if !lists("code_challenge_methods_supported", "S256") {
            return Err(AuthServerRefusal::Unusable("no S256 PKCE"));
        }
        if !lists("response_types_supported", "code") {
            return Err(AuthServerRefusal::Unusable("no code response type"));
        }
        // The device grant counts only when it is listed (RFC 8414 makes
        // `grant_types_supported` optional, defaulting to the code grant).
        let device_authorization_endpoint = device_authorization_endpoint
            .filter(|_| lists("grant_types_supported", DEVICE_CODE_GRANT));
        Ok(Self {
            issuer,
            authorization_endpoint,
            token_endpoint,
            registration_endpoint,
            revocation_endpoint,
            device_authorization_endpoint,
        })
    }

    /// The issuer's address as the confirmation shows it: scheme, host and
    /// port, and its path when it has one.
    pub fn address(&self) -> String {
        display_address(&self.issuer)
    }
}

/// `url` without its query, as the person is shown it.
pub fn display_address(url: &Url) -> String {
    let mut shown = url.clone();
    shown.set_query(None);
    shown.set_fragment(None);
    shown.to_string()
}

/// Asks `homeserver` for its authorization server and checks it. Only
/// unauthenticated `GET`s to the checked homeserver.
pub async fn discover_auth_server(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    allowlist: &Allowlist,
) -> Result<AuthServer, AuthServerRefusal> {
    for path in AUTH_METADATA_PATHS {
        let response = match transport
            .send(HttpRequest::get(homeserver.endpoint(path)))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                info!(%error, "could not ask the homeserver for its authorization server");
                return Err(AuthServerRefusal::NotOffered);
            }
        };
        if response.status == 200 {
            let Some(metadata) = response.json() else {
                return Err(AuthServerRefusal::Unusable("metadata that is not JSON"));
            };
            return AuthServer::from_metadata(&metadata, homeserver, allowlist);
        }
    }
    Err(AuthServerRefusal::NotOffered)
}

/// The client metadata the app registers with (RFC 7591, MSC2966): a
/// native public client named Family Chat Desktop. `redirect_uri` is the
/// loopback address of a browser sign-in; a QR code sign-in has none.
pub fn client_metadata(redirect_uri: Option<&Url>) -> Value {
    let mut grant_types = vec!["refresh_token", DEVICE_CODE_GRANT];
    let mut metadata = json!({
        "application_type": "native",
        "client_name": CLIENT_NAME,
        "client_uri": CLIENT_URI,
        "token_endpoint_auth_method": "none",
    });
    if let Some(redirect_uri) = redirect_uri {
        grant_types.insert(0, "authorization_code");
        metadata["redirect_uris"] = json!([redirect_uri.as_str()]);
        metadata["response_types"] = json!(["code"]);
    }
    metadata["grant_types"] = json!(grant_types);
    metadata
}

/// Why an OAuth request failed. Safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthFailure {
    /// No answer.
    Unreachable(TransportError),
    /// The server answered with an OAuth error (RFC 6749 section 5.2).
    Refused {
        /// The status.
        status: u16,
        /// The `error` code, if any.
        error: Option<String>,
    },
    /// An answer without what it should carry.
    Unreadable,
}

fn refused(response: &HttpResponse) -> OAuthFailure {
    OAuthFailure::Refused {
        status: response.status,
        error: response
            .json()
            .and_then(|body| body.get("error")?.as_str().map(str::to_owned)),
    }
}

/// Registers the app with `server` (dynamic client registration) and
/// returns its client id.
pub async fn register_client(
    transport: &dyn Transport,
    server: &AuthServer,
    metadata: &Value,
) -> Result<String, OAuthFailure> {
    let response = transport
        .send(HttpRequest::post_json(
            server.registration_endpoint.clone(),
            metadata,
        ))
        .await
        .map_err(OAuthFailure::Unreachable)?;
    if !matches!(response.status, 200 | 201) {
        let failure = refused(&response);
        info!(?failure, "the client registration was refused");
        return Err(failure);
    }
    response
        .json()
        .and_then(|body| body.get("client_id")?.as_str().map(str::to_owned))
        .filter(|id| !id.is_empty())
        .ok_or(OAuthFailure::Unreadable)
}

/// Random URL-safe text from `bytes` bytes of the OS CSPRNG.
fn random_text(bytes: usize) -> Result<String, OAuthFailure> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| OAuthFailure::Unreadable)?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// A fresh `state` for a redirect (a single sign-on's, which has none of
/// its own).
pub fn new_state() -> Result<Secret<String>, OAuthFailure> {
    random_text(24).map(Secret::new)
}

/// A Matrix device id for a new OAuth sign-in: ten capital letters, as
/// the SDK makes them.
pub fn new_device_id() -> Result<String, OAuthFailure> {
    let mut buffer = [0u8; 10];
    getrandom::fill(&mut buffer).map_err(|_| OAuthFailure::Unreadable)?;
    Ok(buffer
        .iter()
        .map(|byte| char::from(b'A' + byte % 26))
        .collect())
}

/// One authorization request in progress: what proves the answer is its
/// own (the state) and what the code exchange needs (the PKCE verifier,
/// the device, the redirect).
pub struct Authorization {
    /// The `state` the redirect must carry back.
    pub state: Secret<String>,
    verifier: Secret<String>,
    /// The device the token is for.
    pub device_id: String,
    /// The loopback redirect.
    pub redirect_uri: Url,
    /// The client id.
    pub client_id: String,
    /// The address to open in the browser.
    pub url: Url,
}

impl fmt::Debug for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authorization").finish_non_exhaustive()
    }
}

impl Authorization {
    /// A new request: a fresh state, PKCE verifier (S256) and device id.
    pub fn new(
        server: &AuthServer,
        client_id: String,
        redirect_uri: Url,
    ) -> Result<Self, OAuthFailure> {
        let state = random_text(24)?;
        let verifier = random_text(48)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let device_id = new_device_id()?;
        let scope = format!("{SCOPE_API} {SCOPE_DEVICE_PREFIX}{device_id}");
        let mut url = server.authorization_endpoint.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", redirect_uri.as_str())
            .append_pair("scope", &scope)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(Self {
            state: Secret::new(state),
            verifier: Secret::new(verifier),
            device_id,
            redirect_uri,
            client_id,
            url,
        })
    }

    /// Exchanges the code the redirect carried for tokens, then asks the
    /// homeserver who they belong to. The code is wiped once its request
    /// is built.
    pub async fn exchange(
        self,
        transport: &dyn Transport,
        server: &AuthServer,
        homeserver: &CheckedHomeserver,
        code: Secret<String>,
    ) -> Result<Credentials, OAuthFailure> {
        let request = HttpRequest::post_form(
            server.token_endpoint.clone(),
            &[
                ("grant_type", "authorization_code"),
                ("code", code.expose()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", &self.client_id),
                ("code_verifier", self.verifier.expose()),
            ],
        );
        drop(code);
        let response = transport
            .send(request)
            .await
            .map_err(OAuthFailure::Unreachable)?;
        if response.status != 200 {
            let failure = refused(&response);
            info!(?failure, "the code exchange was refused");
            return Err(failure);
        }
        let body = response.json().ok_or(OAuthFailure::Unreadable)?;
        let token = |name: &str| {
            body.get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| Secret::new(value.to_owned()))
        };
        let access_token = token("access_token").ok_or(OAuthFailure::Unreadable)?;
        let refresh_token = token("refresh_token");
        let grant = OAuthGrant {
            client_id: self.client_id.clone(),
            revocation_endpoint: server.revocation_endpoint.clone(),
        };
        match whoami(transport, homeserver, &access_token).await {
            Ok((user_id, device_id)) if device_id.as_deref() == Some(&self.device_id) => {
                Ok(Credentials {
                    homeserver: homeserver.clone(),
                    user_id,
                    device_id: self.device_id,
                    access_token,
                    refresh_token,
                    oauth: Some(grant),
                })
            }
            result => {
                warn!(
                    answered = result.is_ok(),
                    "the new token is not this device's; revoking it"
                );
                revoke(transport, &grant, access_token, refresh_token).await;
                Err(OAuthFailure::Unreadable)
            }
        }
    }
}

/// `GET /account/whoami` with `access_token`: the user and the device.
pub async fn whoami(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    access_token: &Secret<String>,
) -> Result<(OwnedUserId, Option<String>), OAuthFailure> {
    let response = transport
        .send(
            HttpRequest::get(homeserver.endpoint("_matrix/client/v3/account/whoami"))
                .bearer(access_token.clone()),
        )
        .await
        .map_err(OAuthFailure::Unreachable)?;
    if response.status != 200 {
        return Err(refused(&response));
    }
    let body = response.json().ok_or(OAuthFailure::Unreadable)?;
    let user_id = body
        .get("user_id")
        .and_then(Value::as_str)
        .and_then(parse_user_id)
        .ok_or(OAuthFailure::Unreadable)?;
    let device_id = body
        .get("device_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((user_id, device_id))
}

/// Revokes an OAuth sign-in's tokens (RFC 7009): best effort, for a
/// sign-in the app will not keep. Never fails; the outcome is logged.
pub async fn revoke(
    transport: &dyn Transport,
    grant: &OAuthGrant,
    access_token: Secret<String>,
    refresh_token: Option<Secret<String>>,
) {
    let Some(endpoint) = &grant.revocation_endpoint else {
        warn!(
            "the authorization server has no revocation endpoint; the tokens stay valid until they expire"
        );
        return;
    };
    for (token, hint) in [
        (Some(access_token), "access_token"),
        (refresh_token, "refresh_token"),
    ] {
        let Some(token) = token else { continue };
        let request = HttpRequest::post_form(
            endpoint.clone(),
            &[
                ("token", token.expose()),
                ("token_type_hint", hint),
                ("client_id", &grant.client_id),
            ],
        );
        drop(token);
        match transport.send(request).await {
            Ok(response) if response.status == 200 => {
                info!(hint, "revoked the token of a sign-in that is not kept");
            }
            Ok(response) => {
                warn!(status = response.status, hint, "could not revoke the token");
            }
            Err(error) => warn!(%error, hint, "could not revoke the token"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(issuer: &str) -> Value {
        json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}authorize"),
            "token_endpoint": format!("{issuer}oauth2/token"),
            "registration_endpoint": format!("{issuer}oauth2/registration"),
            "revocation_endpoint": format!("{issuer}oauth2/revoke"),
            "device_authorization_endpoint": format!("{issuer}oauth2/device"),
            "grant_types_supported": ["authorization_code", "refresh_token", DEVICE_CODE_GRANT],
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["plain", "S256"],
        })
    }

    fn family() -> CheckedHomeserver {
        Allowlist::production()
            .check(&Url::parse("https://smith.safechat.family").expect("url"))
            .expect("allowed")
    }

    #[test]
    fn a_family_chat_homeservers_issuer_must_be_under_the_allowlist() {
        let allowlist = Allowlist::production();
        let server = AuthServer::from_metadata(
            &metadata("https://auth.smith.safechat.family/"),
            &family(),
            &allowlist,
        )
        .expect("allowed");
        assert_eq!(server.address(), "https://auth.smith.safechat.family/");
        assert!(server.device_authorization_endpoint.is_some());

        assert_eq!(
            AuthServer::from_metadata(
                &metadata("https://account.matrix.org/"),
                &family(),
                &allowlist
            ),
            Err(AuthServerRefusal::NotAllowed(
                "issuer",
                NotAllowed::NotFamilyChat
            ))
        );
        // One endpoint elsewhere is enough to refuse it all.
        let mut one_elsewhere = metadata("https://auth.smith.safechat.family/");
        one_elsewhere["token_endpoint"] = json!("https://evil.example/token");
        assert_eq!(
            AuthServer::from_metadata(&one_elsewhere, &family(), &allowlist),
            Err(AuthServerRefusal::NotAllowed(
                "token_endpoint",
                NotAllowed::NotFamilyChat
            ))
        );
        let mut plain = metadata("https://auth.smith.safechat.family/");
        plain["revocation_endpoint"] = json!("http://auth.smith.safechat.family/revoke");
        assert_eq!(
            AuthServer::from_metadata(&plain, &family(), &allowlist),
            Err(AuthServerRefusal::NotAllowed(
                "revocation_endpoint",
                NotAllowed::NotHttps
            ))
        );
    }

    #[test]
    fn what_the_app_needs_is_required() {
        let allowlist = Allowlist::production();
        let other =
            CheckedHomeserver::other_server(&Url::parse("https://matrix.org").expect("url"))
                .expect("allowed");
        assert!(
            AuthServer::from_metadata(&metadata("https://account.matrix.org/"), &other, &allowlist)
                .is_ok()
        );
        for field in [
            "issuer",
            "authorization_endpoint",
            "token_endpoint",
            "registration_endpoint",
        ] {
            let mut missing = metadata("https://account.matrix.org/");
            missing.as_object_mut().expect("object").remove(field);
            assert!(
                AuthServer::from_metadata(&missing, &other, &allowlist).is_err(),
                "{field}"
            );
        }
        let mut no_s256 = metadata("https://account.matrix.org/");
        no_s256["code_challenge_methods_supported"] = json!(["plain"]);
        assert_eq!(
            AuthServer::from_metadata(&no_s256, &other, &allowlist),
            Err(AuthServerRefusal::Unusable("no S256 PKCE"))
        );
        let mut no_device_grant = metadata("https://account.matrix.org/");
        no_device_grant["grant_types_supported"] = json!(["authorization_code"]);
        let server =
            AuthServer::from_metadata(&no_device_grant, &other, &allowlist).expect("usable");
        assert!(server.device_authorization_endpoint.is_none());
    }

    #[test]
    fn the_registration_names_family_chat_desktop() {
        let redirect = Url::parse("http://127.0.0.1:41234/callback").expect("url");
        let metadata = client_metadata(Some(&redirect));
        assert_eq!(metadata["client_name"], CLIENT_NAME);
        assert_eq!(metadata["application_type"], "native");
        assert_eq!(metadata["token_endpoint_auth_method"], "none");
        assert_eq!(metadata["redirect_uris"], json!([redirect.as_str()]));
        assert_eq!(
            metadata["grant_types"],
            json!(["authorization_code", "refresh_token", DEVICE_CODE_GRANT])
        );
        let qr = client_metadata(None);
        assert!(qr.get("redirect_uris").is_none());
        assert_eq!(
            qr["grant_types"],
            json!(["refresh_token", DEVICE_CODE_GRANT])
        );
    }

    #[test]
    fn an_authorization_request_carries_pkce_and_a_device() {
        let server = AuthServer::from_metadata(
            &metadata("https://auth.smith.safechat.family/"),
            &family(),
            &Allowlist::production(),
        )
        .expect("allowed");
        let redirect = Url::parse("http://127.0.0.1:41234/callback").expect("url");
        let first =
            Authorization::new(&server, "client".to_owned(), redirect.clone()).expect("random");
        let second = Authorization::new(&server, "client".to_owned(), redirect).expect("random");
        let pairs: Vec<(String, String)> = first.url.query_pairs().into_owned().collect();
        let get = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(get("response_type").as_deref(), Some("code"));
        assert_eq!(get("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(
            get("code_challenge"),
            Some(URL_SAFE_NO_PAD.encode(Sha256::digest(first.verifier.expose().as_bytes())))
        );
        assert_eq!(get("state").as_deref(), Some(first.state.expose().as_str()));
        assert_eq!(
            get("scope"),
            Some(format!(
                "{SCOPE_API} {SCOPE_DEVICE_PREFIX}{}",
                first.device_id
            ))
        );
        assert_eq!(first.device_id.len(), 10);
        assert!(first.device_id.chars().all(|c| c.is_ascii_uppercase()));
        assert_ne!(first.state.expose(), second.state.expose());
        assert_ne!(first.verifier.expose(), second.verifier.expose());
        assert!(!format!("{first:?}").contains(first.state.expose().as_str()));
    }
}
