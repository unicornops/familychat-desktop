//! How the session's Matrix clients reach their homeservers.

use std::time::Duration;

use familychat_core::identity::APP_NAME;
use reqwest::{Client, redirect, tls};

/// How long a connection may take to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// What every account's `matrix_sdk::Client` is built with.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The HTTP client every request goes through. The app uses
    /// [`SessionConfig::http_client`]'s; tests inject one that trusts
    /// only their homeserver (the harness's, or a mock's).
    pub http: Client,
}

impl SessionConfig {
    /// The app's configuration, with [`SessionConfig::http_client`].
    pub fn new() -> reqwest::Result<Self> {
        Ok(Self {
            http: Self::http_client()?,
        })
    }

    /// A configuration over another HTTP client (tests).
    pub fn with_http_client(http: Client) -> Self {
        Self { http }
    }

    /// Family Chat's own HTTP client, rather than matrix-sdk's default one
    /// (ADR 0005): HTTPS only, TLS 1.2 or later through rustls with
    /// aws-lc-rs and the system's roots, no redirects followed (a
    /// homeserver API that redirects is refused rather than followed to
    /// another host), a connect timeout, and an honest user agent. The
    /// SDK sets each request's own timeout, so the client has none.
    pub fn http_client() -> reqwest::Result<Client> {
        Client::builder()
            .user_agent(format!("{APP_NAME}/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .min_tls_version(tls::Version::TLS_1_2)
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_app_client_refuses_plain_http() {
        let client = SessionConfig::http_client().expect("builds");
        let error = client
            .get("http://127.0.0.1:9/_matrix/client/versions")
            .send()
            .await
            .expect_err("plain HTTP is refused");
        assert!(error.is_builder(), "{error}");
    }
}
