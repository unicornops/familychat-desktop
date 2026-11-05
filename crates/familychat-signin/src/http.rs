//! The sign-in's own HTTP client (PLAN.md, section 6.4).
//!
//! Every sign-in request (discovery, the code redemption, the password
//! login, signing out a device the sign-in will not keep) goes through a
//! [`Transport`], never through matrix-sdk, so the SDK's general HTTP
//! behaviour (its retries, its handling of a login response's
//! `well_known`) cannot touch a token or a password. The app's transport is
//! [`ReqwestTransport`], whose client is built with:
//!
//! - **no redirects**: a `307` to `/login` would otherwise re-send the
//!   token or the password to wherever it points; a redirect is an answer
//!   like any other, and the callers treat it as a failure;
//! - **no retries**: reqwest's default retry of protocol NACKs is turned
//!   off, and no pooled connection is reused, so a request is sent at most
//!   once and a single-use code is never replayed;
//! - HTTPS only, TLS 1.2 or later, a connect and a request timeout.
//!
//! Tests inject the harness's resolver and CA through the builder they
//! hand to [`ReqwestTransport::with_builder`]; the settings above are
//! applied on top of it, so no test client can turn them back on. Unit
//! tests use a recording [`Transport`] of their own.
//!
//! Requests and responses hold their bodies and bearer tokens as
//! [`Secret`]s: `Debug` prints the method and the URL's path only.

use std::{fmt, sync::Arc, time::Duration};

use familychat_core::identity::APP_NAME;
use familychat_security::{Secret, redact::redact_url};
use futures_util::future::BoxFuture;
use reqwest::{ClientBuilder, Url, redirect, tls};

/// How long a connection may take to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long one sign-in request may take in all.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
/// The most of a response body the sign-in reads.
const MAX_BODY: usize = 1024 * 1024;

/// A request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
}

/// One sign-in request.
pub struct HttpRequest {
    /// The method.
    pub method: Method,
    /// The full URL.
    pub url: Url,
    /// An `Authorization: Bearer` token.
    pub bearer: Option<Secret<String>>,
    /// A JSON body (it may hold a token or a password).
    pub json: Option<Secret<Vec<u8>>>,
}

impl HttpRequest {
    /// A `GET` of `url`.
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::Get,
            url,
            bearer: None,
            json: None,
        }
    }

    /// A `POST` of `body` to `url`.
    pub fn post_json(url: Url, body: &serde_json::Value) -> Self {
        Self {
            method: Method::Post,
            url,
            bearer: None,
            json: Some(Secret::new(body.to_string().into_bytes())),
        }
    }

    /// With a bearer token.
    pub fn bearer(mut self, token: Secret<String>) -> Self {
        self.bearer = Some(token);
        self
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &redact_url(self.url.as_str()))
            .field("bearer", &self.bearer.as_ref().map(|_| "[redacted]"))
            .field("json", &self.json.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// One answer.
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The `Retry-After` header, when it was a number of seconds.
    pub retry_after: Option<Duration>,
    /// The body (it may hold an access token).
    pub body: Secret<Vec<u8>>,
}

impl HttpResponse {
    /// The body as JSON, if it is.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(self.body.expose()).ok()
    }
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .field("body", &self.body)
            .finish()
    }
}

/// A request that got no answer. The text is safe to log: no URL query,
/// body or token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// Sends sign-in requests. Each request is sent once.
pub trait Transport: Send + Sync {
    /// Sends `request` and reads the answer.
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}

impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        (**self).send(request)
    }
}

/// The app's [`Transport`]: a reqwest client with redirects, retries and
/// connection reuse off.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// The app's transport, with the system's TLS roots.
    pub fn new() -> reqwest::Result<Self> {
        Self::with_builder(reqwest::Client::builder())
    }

    /// A transport over `builder` (tests pass the harness's, with its CA
    /// and resolver), with the sign-in's settings applied on top.
    pub fn with_builder(builder: ClientBuilder) -> reqwest::Result<Self> {
        let client = builder
            .user_agent(format!("{APP_NAME}/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .min_tls_version(tls::Version::TLS_1_2)
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let HttpRequest {
                method,
                url,
                bearer,
                json,
            } = request;
            let mut builder = match method {
                Method::Get => self.client.get(url),
                Method::Post => self.client.post(url),
            };
            if let Some(token) = &bearer {
                builder = builder.bearer_auth(token.expose());
            }
            if let Some(body) = json {
                builder = builder
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.expose().clone());
            }
            let mut response = builder
                .send()
                .await
                .map_err(|error| TransportError(describe(&error)))?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| TransportError(describe(&error)))?
            {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(TransportError("the answer was too large".to_owned()));
                }
                body.extend_from_slice(&chunk);
            }
            let body = Secret::new(body);
            Ok(HttpResponse {
                status,
                retry_after,
                body,
            })
        })
    }
}

/// A reqwest error without its URL (and so without any query).
fn describe(error: &reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "the request timed out"
    } else if error.is_connect() {
        "could not connect"
    } else if error.is_builder() {
        "the request was refused before it was sent"
    } else if error.is_body() || error.is_decode() {
        "could not read the answer"
    } else {
        "the request failed"
    };
    let mut text = kind.to_owned();
    let mut source = std::error::Error::source(error);
    // The innermost cause names the problem (a certificate, a refused
    // connection) without the URL the outer error carries.
    while let Some(next) = source {
        if next.source().is_none() {
            text = format!("{kind}: {next}");
        }
        source = next.source();
    }
    familychat_security::redact::scrub(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_bodies_tokens_and_queries() {
        let request = HttpRequest::post_json(
            Url::parse("https://smith.safechat.family/_matrix/client/v3/login?x=secret-query")
                .expect("url"),
            &serde_json::json!({ "token": "secret-token" }),
        )
        .bearer(Secret::new("secret-bearer".to_owned()));
        let response = HttpResponse {
            status: 200,
            retry_after: None,
            body: Secret::new(b"{\"access_token\":\"secret-access\"}".to_vec()),
        };
        for debug in [format!("{request:?}"), format!("{response:#?}")] {
            assert!(!debug.contains("secret"), "{debug}");
        }
        assert!(format!("{request:?}").contains("/_matrix/client/v3/login"));
    }

    #[tokio::test]
    async fn the_client_refuses_plain_http() {
        let transport = ReqwestTransport::new().expect("builds");
        let error = transport
            .send(HttpRequest::get(
                Url::parse("http://127.0.0.1:9/_matrix/client/versions").expect("url"),
            ))
            .await
            .expect_err("plain HTTP is refused");
        assert!(error.0.contains("refused before it was sent"), "{error}");
    }
}
