//! What a homeserver offers for signing in (ADR 0010), found with
//! unauthenticated `GET`s to the checked homeserver only, so each way
//! shows only where it works:
//!
//! - **password**: `m.login.password` in `GET /login`'s flows, when the
//!   server has no OAuth 2.0 server (with one, its compatibility password
//!   login is left out, as Element X does and MSC3824 asks);
//! - **"Continue in your browser"**: the server's OAuth 2.0 authorization
//!   server (MSC3861: `auth_metadata`, checked by
//!   [`AuthServer::from_metadata`]), or else single sign-on
//!   (`m.login.sso`, redeemed with `m.login.token`);
//! - **"Sign in with a QR code"**: OAuth with the device authorization
//!   grant, and the MSC4108 rendezvous (`org.matrix.msc4108` in
//!   `/versions`' unstable features). MSC4388's rendezvous is not looked
//!   for: the pinned SDK refuses its QR codes.
//!
//! Family Chat's Palpo has none of the last two today (PLAN.md section
//! 6.10), so its families see the password only.

use familychat_core::signin::{BrowserKind, BrowserMethod, ServerMethods};
use serde_json::Value;
use tracing::info;

use crate::{
    allowlist::{Allowlist, CheckedHomeserver},
    http::{HttpRequest, Transport},
    oauth::{AuthServer, AuthServerRefusal, discover_auth_server, display_address},
};

/// The MSC4108 rendezvous's unstable feature flag.
pub const MSC4108_FEATURE: &str = "org.matrix.msc4108";

/// What a homeserver offers, with what the sign-in needs to use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOffer {
    /// What the frontends show.
    pub methods: ServerMethods,
    /// Its checked authorization server, when it has one.
    pub auth: Option<AuthServer>,
    /// Whether single sign-on is the browser sign-in.
    pub sso: bool,
}

/// Asks `homeserver` what it offers.
pub async fn probe(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    allowlist: &Allowlist,
) -> ServerOffer {
    let flows = login_flows(transport, homeserver).await;
    let has_flow = |kind: &str| {
        flows
            .as_ref()
            .is_some_and(|flows| flows.iter().any(|flow| flow == kind))
    };
    let (auth, refused) = match discover_auth_server(transport, homeserver, allowlist).await {
        Ok(auth) => (Some(auth), false),
        Err(AuthServerRefusal::NotOffered) => (None, false),
        Err(refusal) => {
            info!(?refusal, "the homeserver's OAuth server can't be used");
            (None, true)
        }
    };
    let rendezvous = auth
        .as_ref()
        .is_some_and(|auth| auth.device_authorization_endpoint.is_some())
        && unstable_feature(transport, homeserver, MSC4108_FEATURE).await;
    // Single sign-on is the browser sign-in only without an OAuth server:
    // with one it would lead to it, and one that was refused stays refused.
    let sso = auth.is_none() && !refused && has_flow("m.login.sso");
    let browser = match &auth {
        Some(auth) => Some(BrowserMethod {
            kind: BrowserKind::OAuth,
            address: auth.address(),
        }),
        None if sso => Some(BrowserMethod {
            kind: BrowserKind::Sso,
            address: display_address(homeserver.url()),
        }),
        None => None,
    };
    // A server that can't say (no answer to `GET /login`) is asked for a
    // password, as before Phase 14.
    let password = auth.is_none() && (flows.is_none() || has_flow("m.login.password"));
    ServerOffer {
        methods: ServerMethods {
            password,
            browser,
            qr_code: rendezvous,
        },
        auth,
        sso,
    }
}

/// The login types `GET /login` lists; `None` when it does not answer.
async fn login_flows(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
) -> Option<Vec<String>> {
    let response = transport
        .send(HttpRequest::get(
            homeserver.endpoint("_matrix/client/v3/login"),
        ))
        .await
        .ok()?;
    if response.status != 200 {
        return None;
    }
    let body = response.json()?;
    Some(
        body.get("flows")?
            .as_array()?
            .iter()
            .filter_map(|flow| flow.get("type")?.as_str().map(str::to_owned))
            .collect(),
    )
}

/// Whether `/versions` turns `feature` on.
async fn unstable_feature(
    transport: &dyn Transport,
    homeserver: &CheckedHomeserver,
    feature: &str,
) -> bool {
    let Ok(response) = transport
        .send(HttpRequest::get(
            homeserver.endpoint("_matrix/client/versions"),
        ))
        .await
    else {
        return false;
    };
    response.status == 200
        && response.json().is_some_and(|body| {
            body.get("unstable_features")
                .and_then(|features| features.get(feature))
                .and_then(Value::as_bool)
                == Some(true)
        })
}
