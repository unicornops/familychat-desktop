//! Finding a server name's homeserver (PLAN.md, section 6.5): the
//! `.well-known/matrix/client` document, then the allowlist (for Family
//! Chat) on the URL it names, then `/versions`.
//!
//! Only unauthenticated `GET`s go out here, and only the well-known request
//! goes out before the allowlist check passes: `/versions` is asked of the
//! resolved homeserver after it, so a Family Chat sign-in sends nothing at
//! all to a homeserver outside the allowlist. Like every sign-in request,
//! these follow no redirects.
//!
//! The well-known rules (the Matrix specification's client discovery, as
//! matrix-sdk applies it):
//!
//! - a `200` whose JSON has `m.homeserver.base_url` names the homeserver;
//!   a `base_url` that is not a URL is an error;
//! - anything else (a `404`, a redirect, an HTML page, JSON without
//!   `m.homeserver`) means the name is its own homeserver,
//!   `https://<name>`. For the apex `safechat.family`, whose well-known is
//!   the website's HTML, that is `https://safechat.family`, which the
//!   allowlist refuses (the Android review's apex fallback).

use reqwest::Url;
use tracing::info;

use crate::{
    allowlist::{Allowlist, CheckedHomeserver, NotAllowed},
    host::ServerName,
    http::{HttpRequest, Transport, TransportError},
};

/// Why a server name did not lead to a homeserver the sign-in may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    /// The server could not be reached.
    Unreachable(TransportError),
    /// The well-known document named something that is not a URL.
    BadBaseUrl,
    /// The homeserver is outside the Family Chat allowlist.
    NotFamilyChat,
    /// Another server's homeserver is not `https://` (or not a name).
    NotHttps,
    /// The homeserver did not answer `/versions` like a Matrix server.
    NotAMatrixServer,
}

/// Resolves `name` and checks the homeserver: against `allowlist` for a
/// Family Chat sign-in, or for HTTPS only when `allowlist` is `None`.
pub async fn discover(
    transport: &dyn Transport,
    name: &ServerName,
    allowlist: Option<&Allowlist>,
) -> Result<CheckedHomeserver, DiscoveryError> {
    let base = name.https_url().map_err(|_| DiscoveryError::BadBaseUrl)?;
    let well_known = endpoint(&base, ".well-known/matrix/client");
    let response = transport
        .send(HttpRequest::get(well_known))
        .await
        .map_err(DiscoveryError::Unreachable)?;
    let named = if response.status == 200 {
        response
            .json()
            .and_then(|body| {
                body.get("m.homeserver")?
                    .get("base_url")?
                    .as_str()
                    .map(str::to_owned)
            })
            .map(|base_url| Url::parse(base_url.trim()).map_err(|_| DiscoveryError::BadBaseUrl))
            .transpose()?
    } else {
        None
    };
    let homeserver = named.unwrap_or(base);

    let checked = match allowlist {
        Some(allowlist) => allowlist.check(&homeserver).map_err(|refusal| {
            info!(?refusal, "the homeserver is not a Family Chat server");
            DiscoveryError::NotFamilyChat
        })?,
        None => CheckedHomeserver::other_server(&homeserver).map_err(|refusal| match refusal {
            NotAllowed::NotHttps | NotAllowed::NotAName | NotAllowed::UserInfo => {
                DiscoveryError::NotHttps
            }
            NotAllowed::NotFamilyChat => DiscoveryError::NotFamilyChat,
        })?,
    };

    let versions = transport
        .send(HttpRequest::get(
            checked.endpoint("_matrix/client/versions"),
        ))
        .await
        .map_err(DiscoveryError::Unreachable)?;
    let speaks_matrix = versions.status == 200
        && versions.json().is_some_and(|body| {
            body.get("versions")
                .is_some_and(serde_json::Value::is_array)
        });
    if !speaks_matrix {
        return Err(DiscoveryError::NotAMatrixServer);
    }
    Ok(checked)
}

fn endpoint(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(&format!("/{path}"));
    url
}
