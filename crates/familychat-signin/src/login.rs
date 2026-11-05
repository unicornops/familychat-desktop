//! `POST /_matrix/client/v3/login`, for a code (`m.login.token`) and a
//! password (`m.login.password`), and signing out a device the sign-in
//! will not keep (PLAN.md, sections 6.4 and 6.5).
//!
//! Both logins go to a [`CheckedHomeserver`] only, through the sign-in's
//! own [`Transport`] (no redirects, no retries), and are sent once: a
//! rate limit (`M_LIMIT_EXCEEDED`) comes back to the user with its wait,
//! and nothing here tries again. Only a `200` with an access token, a
//! device id and a valid user id is a sign-in; a login response's
//! `well_known` is ignored, so it can never move the account to another
//! server.

use std::{fmt, time::Duration};

use familychat_core::OwnedUserId;
use familychat_security::Secret;
use serde::Serialize;
use tracing::{info, warn};

use crate::{
    allowlist::CheckedHomeserver,
    http::{HttpRequest, HttpResponse, Method, Transport, TransportError},
    link::parse_user_id,
};

const LOGIN_PATH: &str = "_matrix/client/v3/login";
const LOGOUT_PATH: &str = "_matrix/client/v3/logout";

/// What a successful login issued. `Debug` never prints the tokens.
pub struct Credentials {
    /// Where they were issued.
    pub homeserver: CheckedHomeserver,
    /// The account signed in.
    pub user_id: OwnedUserId,
    /// The new device.
    pub device_id: String,
    /// The access token.
    pub access_token: Secret<String>,
    /// The refresh token, when the server issued one.
    pub refresh_token: Option<Secret<String>>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("homeserver", &self.homeserver.url().as_str())
            .field("user_id", &self.user_id)
            .field("device_id", &self.device_id)
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .finish()
    }
}

/// Why a login did not sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginFailure {
    /// A `401` or `403`: for a code, used or expired; for a password, the
    /// wrong one (or a deactivated account, by its `errcode`).
    Rejected {
        /// The status.
        status: u16,
        /// The Matrix `errcode`, if any.
        errcode: Option<String>,
    },
    /// `M_LIMIT_EXCEEDED` (or a `429`), with the wait the server asked for.
    RateLimited(Option<Duration>),
    /// A redirect, which is never followed.
    Redirected(u16),
    /// Any other answer.
    Refused {
        /// The status.
        status: u16,
        /// The Matrix `errcode`, if any.
        errcode: Option<String>,
    },
    /// A `200` that did not carry usable credentials (any access token it
    /// carried has been signed out).
    Unreadable,
    /// No answer.
    Unreachable(TransportError),
}

/// The `m.login.token` body (the contract's *Redeeming a token*).
#[derive(Serialize)]
struct TokenLogin<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    token: &'a str,
    initial_device_display_name: &'a str,
}

/// The `m.login.password` body.
#[derive(Serialize)]
struct PasswordLogin<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    identifier: UserIdentifier<'a>,
    password: &'a str,
    initial_device_display_name: &'a str,
}

#[derive(Serialize)]
struct UserIdentifier<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    user: &'a str,
}

/// Serialises `body` straight into a buffer that is wiped when dropped.
fn secret_json(body: &impl Serialize) -> Secret<Vec<u8>> {
    let mut buffer = Vec::with_capacity(512);
    // Serialising these plain structs to a Vec cannot fail.
    let _ = serde_json::to_writer(&mut buffer, body);
    Secret::new(buffer)
}

/// Redeems a sign-in code at `homeserver`. The token is consumed: it is
/// wiped as soon as the request is built, whatever happens next.
pub async fn redeem_token(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    token: Secret<String>,
    device_name: &str,
) -> Result<Credentials, LoginFailure> {
    let body = secret_json(&TokenLogin {
        kind: "m.login.token",
        token: token.expose(),
        initial_device_display_name: device_name,
    });
    drop(token);
    info!("redeeming a sign-in code");
    login(transport, homeserver, body).await
}

/// Signs in with a password at `homeserver`. `user` is a localpart or a
/// full Matrix ID.
pub async fn login_password(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    user: &str,
    password: &Secret<String>,
    device_name: &str,
) -> Result<Credentials, LoginFailure> {
    let body = secret_json(&PasswordLogin {
        kind: "m.login.password",
        identifier: UserIdentifier {
            kind: "m.id.user",
            user,
        },
        password: password.expose(),
        initial_device_display_name: device_name,
    });
    info!("signing in with a password");
    login(transport, homeserver, body).await
}

async fn login(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    body: Secret<Vec<u8>>,
) -> Result<Credentials, LoginFailure> {
    let request = HttpRequest {
        method: Method::Post,
        url: homeserver.endpoint(LOGIN_PATH),
        bearer: None,
        json: Some(body),
    };
    let response = transport
        .send(request)
        .await
        .map_err(LoginFailure::Unreachable)?;
    if response.status != 200 {
        let failure = failure(&response);
        info!(status = response.status, failure = ?failure, "the login was not accepted");
        return Err(failure);
    }
    match credentials(homeserver, &response) {
        Ok(credentials) => {
            info!("the login was accepted");
            Ok(credentials)
        }
        Err(Some(access_token)) => {
            warn!("the login answer had an access token but no usable account; signing it out");
            logout_token(transport, homeserver, access_token).await;
            Err(LoginFailure::Unreadable)
        }
        Err(None) => {
            warn!("the login answer carried no credentials");
            Err(LoginFailure::Unreadable)
        }
    }
}

/// The credentials in a `200`, or the access token alone when the rest is
/// missing or invalid (so it can be signed out).
fn credentials(
    homeserver: &CheckedHomeserver,
    response: &HttpResponse,
) -> Result<Credentials, Option<Secret<String>>> {
    let Some(body) = response.json() else {
        return Err(None);
    };
    let text = |name: &str| {
        body.get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let Some(access_token) = text("access_token").map(Secret::new) else {
        return Err(None);
    };
    let user_id = text("user_id").as_deref().and_then(parse_user_id);
    let device_id = text("device_id");
    match (user_id, device_id) {
        (Some(user_id), Some(device_id)) => Ok(Credentials {
            homeserver: homeserver.clone(),
            user_id,
            device_id,
            access_token,
            refresh_token: text("refresh_token").map(Secret::new),
        }),
        _ => Err(Some(access_token)),
    }
}

fn failure(response: &HttpResponse) -> LoginFailure {
    let body = response.json();
    let errcode = body
        .as_ref()
        .and_then(|body| body.get("errcode"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if errcode.as_deref() == Some("M_LIMIT_EXCEEDED") || response.status == 429 {
        let wait = body
            .as_ref()
            .and_then(|body| body.get("retry_after_ms"))
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_millis)
            .or(response.retry_after);
        return LoginFailure::RateLimited(wait);
    }
    match response.status {
        300..=399 => LoginFailure::Redirected(response.status),
        401 | 403 => LoginFailure::Rejected {
            status: response.status,
            errcode,
        },
        status => LoginFailure::Refused { status, errcode },
    }
}

/// Signs out the device `credentials` belong to: best effort, for a
/// sign-in the app will not keep (PLAN.md, section 6.4, rule 6). Never
/// fails; the outcome is logged.
pub async fn logout(transport: &dyn Transport, credentials: Credentials) {
    let Credentials {
        homeserver,
        access_token,
        ..
    } = credentials;
    logout_token(transport, &homeserver, access_token).await;
}

async fn logout_token(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    access_token: Secret<String>,
) {
    let request = HttpRequest {
        method: Method::Post,
        url: homeserver.endpoint(LOGOUT_PATH),
        bearer: Some(access_token),
        json: Some(Secret::new(b"{}".to_vec())),
    };
    match transport.send(request).await {
        Ok(response) if response.status == 200 => {
            info!("signed out the device the sign-in will not keep");
        }
        Ok(response) => {
            warn!(
                status = response.status,
                "could not sign out the device the sign-in will not keep"
            );
        }
        Err(error) => {
            warn!(%error, "could not sign out the device the sign-in will not keep");
        }
    }
}
