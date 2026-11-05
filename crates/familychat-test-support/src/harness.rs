//! [`HomeserverHarness`], the driver for the live homeserver harness (the
//! `test-harness` feature).
//!
//! `scripts/homeserver-harness.sh up` is the single source of truth for
//! starting the harness and bootstrapping its admin bot; the driver reads
//! the settings it wrote ([`HarnessEnv`]) and drives Palpo the way Family
//! Chat's control panel does: members created and suspended through the
//! Synapse-compatible admin API, login tokens minted with
//! `POST /_synapse/admin/v1/users/{mxid}/login_token`. Everything a member's
//! client would do goes through the TLS front end.

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, RequestBuilder, Url};
use serde_json::{Value, json};

use crate::{HarnessEnv, admin_client, edge_client};

/// The device name test sessions sign in with.
const DEVICE_NAME: &str = "Family Chat (harness)";

/// The status and JSON body of one response. A body that is not JSON (an
/// nginx error page) is kept as a JSON string.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The HTTP status code.
    pub status: u16,
    /// The body.
    pub body: Value,
}

impl Reply {
    /// The Matrix `errcode`, if the body has one.
    pub fn errcode(&self) -> Option<&str> {
        self.body.get("errcode").and_then(Value::as_str)
    }

    /// A string field of the body.
    pub fn str_field(&self, field: &str) -> Option<&str> {
        self.body.get(field).and_then(Value::as_str)
    }

    /// Fails unless the status is 200, with the body in the error.
    pub fn ok(self, what: &str) -> Result<Self> {
        if self.status != 200 {
            bail!("{what} answered {}: {}", self.status, self.body);
        }
        Ok(self)
    }
}

/// A signed-in device. `Debug` never prints the access token.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    /// The Matrix ID the server signed in.
    pub user_id: String,
    /// The new device's ID.
    pub device_id: String,
    access_token: String,
}

impl Session {
    /// The session from a login or registration response body.
    pub fn from_login_body(body: &Value) -> Result<Self> {
        let field = |name: &str| {
            body.get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("the login response has no {name}: {body}"))
        };
        Ok(Self {
            user_id: field("user_id")?,
            device_id: field("device_id")?,
            access_token: field("access_token")?,
        })
    }

    /// The access token, for an `Authorization: Bearer` header.
    pub fn access_token(&self) -> &str {
        &self.access_token
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("user_id", &self.user_id)
            .field("device_id", &self.device_id)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

/// A single-use `m.login.token`. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginToken {
    secret: String,
    /// The lifetime the server granted, in milliseconds.
    pub expires_in_ms: u64,
}

impl LoginToken {
    /// The token itself, for the `m.login.token` login body.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for LoginToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginToken")
            .field("secret", &"<redacted>")
            .field("expires_in_ms", &self.expires_in_ms)
            .finish()
    }
}

/// A member made for one test: created with the admin API and signed in
/// with their password through the front end.
#[derive(Debug, Clone)]
pub struct TestUser {
    /// The Matrix ID.
    pub user_id: String,
    /// The password the account was created with.
    pub password: String,
    /// The signed-in session.
    pub session: Session,
}

/// The running harness. Start it first with
/// `scripts/homeserver-harness.sh up`.
#[derive(Debug, Clone)]
pub struct HomeserverHarness {
    env: HarnessEnv,
    edge: Client,
    admin: Client,
}

impl HomeserverHarness {
    /// Connects to the running harness through its settings
    /// ([`HarnessEnv::load`]).
    pub fn connect() -> Result<Self> {
        Self::with_env(HarnessEnv::load()?)
    }

    /// Connects with explicit settings.
    pub fn with_env(env: HarnessEnv) -> Result<Self> {
        Ok(Self {
            edge: edge_client(&env)?,
            admin: admin_client()?,
            env,
        })
    }

    /// The settings.
    pub fn env(&self) -> &HarnessEnv {
        &self.env
    }

    /// The front end's client, for requests a test makes itself.
    pub fn client(&self) -> &Client {
        &self.edge
    }

    /// `https://hs1.familychat.test`.
    pub fn hs1_url(&self) -> &str {
        &self.env.hs1_url
    }

    /// `https://smith.test`, the custom domain.
    pub fn custom_domain_url(&self) -> String {
        format!("https://{}", self.env.custom_domain)
    }

    /// `https://evil.familychat.example`.
    pub fn evil_url(&self) -> &str {
        &self.env.evil_url
    }

    /// `https://synapse.test`.
    pub fn synapse_url(&self) -> &str {
        &self.env.synapse_url
    }

    /// The Matrix ID of `localpart` on hs1.
    pub fn mxid(&self, localpart: &str) -> String {
        format!("@{localpart}:{}", self.env.server_name)
    }

    /// A localpart no other test in this harness run uses.
    pub fn unique_localpart(prefix: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{nanos:x}-{count}")
    }

    /// `base` with each of `segments` appended as one percent-encoded path
    /// segment.
    pub fn url(base: &str, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(base).with_context(|| format!("{base} is not a URL"))?;
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("{base} cannot take a path"))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// A GET through the front end.
    pub async fn get(&self, url: Url) -> Result<Reply> {
        send(self.edge.get(url)).await
    }

    /// A Client-Server API request on hs1, as `session` when given.
    pub async fn client_api(
        &self,
        method: Method,
        segments: &[&str],
        session: Option<&Session>,
        body: Option<&Value>,
    ) -> Result<Reply> {
        let mut path = vec!["_matrix", "client"];
        path.extend_from_slice(segments);
        let mut request = self
            .edge
            .request(method, Self::url(&self.env.hs1_url, &path)?);
        if let Some(session) = session {
            request = request.bearer_auth(session.access_token());
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        send(request).await
    }

    async fn admin_api(&self, method: Method, segments: &[&str], body: &Value) -> Result<Reply> {
        let mut path = vec!["_synapse", "admin"];
        path.extend_from_slice(segments);
        let request = self
            .admin
            .request(method, Self::url(&self.env.admin_url, &path)?)
            .bearer_auth(self.env.admin_token())
            .json(body);
        send(request).await
    }

    // --- Members ---------------------------------------------------------------

    /// Creates (or updates) a member with a password through the admin API,
    /// as the control panel does (`PUT /_synapse/admin/v2/users/{mxid}`).
    /// Returns the Matrix ID.
    pub async fn create_user(&self, localpart: &str, password: &str) -> Result<String> {
        let mxid = self.mxid(localpart);
        let reply = self
            .admin_api(
                Method::PUT,
                &["v2", "users", &mxid],
                &json!({ "password": password }),
            )
            .await?;
        if !matches!(reply.status, 200 | 201) {
            bail!("creating {mxid} answered {}: {}", reply.status, reply.body);
        }
        Ok(mxid)
    }

    /// Creates a member and signs them in with their password.
    pub async fn new_user(&self, prefix: &str) -> Result<TestUser> {
        let localpart = Self::unique_localpart(prefix);
        let password = format!("{localpart}-password");
        let user_id = self.create_user(&localpart, &password).await?;
        let session = self.login_password(&user_id, &password).await?;
        Ok(TestUser {
            user_id,
            password,
            session,
        })
    }

    /// Registers through Palpo's token-gated registration on hs1, the way the
    /// control panel bootstraps its admin bot: the first request is answered
    /// with a UIA session offering `m.login.registration_token`, the second
    /// completes it with the harness's registration token.
    ///
    /// Palpo throttles registration per TCP peer (a burst of three), and
    /// every client request reaches it from the front end, so a harness run
    /// can afford only a handful of these; [`HomeserverHarness::new_user`]
    /// uses the admin API instead.
    pub async fn register_user(&self, localpart: &str, password: &str) -> Result<Session> {
        let mut body = json!({
            "username": localpart,
            "password": password,
            "initial_device_display_name": DEVICE_NAME,
        });
        let challenge = self
            .client_api(Method::POST, &["v3", "register"], None, Some(&body))
            .await?;
        if challenge.status != 401 {
            bail!(
                "registration without a token answered {}, not a UIA challenge: {}",
                challenge.status,
                challenge.body
            );
        }
        let offered = challenge.body["flows"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|flow| {
                flow["stages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|stage| stage == "m.login.registration_token")
            });
        if !offered {
            bail!(
                "registration does not offer m.login.registration_token: {}",
                challenge.body
            );
        }
        let session = challenge
            .str_field("session")
            .context("the registration challenge has no session")?;
        body["auth"] = json!({
            "type": "m.login.registration_token",
            "token": self.env.registration_token(),
            "session": session,
        });
        let reply = self
            .client_api(Method::POST, &["v3", "register"], None, Some(&body))
            .await?
            .ok("token-gated registration")?;
        Session::from_login_body(&reply.body)
    }

    /// Signs in with `m.login.password` through the front end. `user` is a
    /// localpart or a Matrix ID.
    pub async fn login_password(&self, user: &str, password: &str) -> Result<Session> {
        let body = json!({
            "type": "m.login.password",
            "identifier": { "type": "m.id.user", "user": user },
            "password": password,
            "initial_device_display_name": DEVICE_NAME,
        });
        let reply = self
            .client_api(Method::POST, &["v3", "login"], None, Some(&body))
            .await?
            .ok("password login")?;
        Session::from_login_body(&reply.body)
    }

    /// Mints a single-use `m.login.token` for `mxid`, as family-chat's
    /// `mint_login_token` does.
    pub async fn mint_login_token(&self, mxid: &str, expires_in_ms: u64) -> Result<LoginToken> {
        let reply = self
            .admin_api(
                Method::POST,
                &["v1", "users", mxid, "login_token"],
                &json!({ "expires_in_ms": expires_in_ms }),
            )
            .await?
            .ok("minting a login token")?;
        let secret = reply
            .str_field("login_token")
            .context("the login_token response carried no token")?
            .to_owned();
        let expires_in_ms = reply.body["expires_in_ms"]
            .as_u64()
            .unwrap_or(expires_in_ms);
        Ok(LoginToken {
            secret,
            expires_in_ms,
        })
    }

    /// Redeems a login token with `m.login.token` through the front end and
    /// returns the raw reply, so a test can check a refusal.
    pub async fn login_with_token(&self, token: &LoginToken) -> Result<Reply> {
        let body = json!({
            "type": "m.login.token",
            "token": token.secret(),
            "initial_device_display_name": DEVICE_NAME,
        });
        self.client_api(Method::POST, &["v3", "login"], None, Some(&body))
            .await
    }

    /// Suspends or unsuspends `mxid` through the admin API
    /// (`PUT /_synapse/admin/v1/suspend/{mxid}`), as a curfew does.
    pub async fn set_suspended(&self, mxid: &str, suspended: bool) -> Result<()> {
        self.admin_api(
            Method::PUT,
            &["v1", "suspend", mxid],
            &json!({ "suspend": suspended }),
        )
        .await?
        .ok("suspending a user")?;
        Ok(())
    }

    // --- Rooms -----------------------------------------------------------------

    /// Creates a private room as `session`, inviting `invite`. Returns the
    /// room ID.
    pub async fn create_room(
        &self,
        session: &Session,
        name: &str,
        invite: &[&str],
    ) -> Result<String> {
        let body = json!({ "name": name, "preset": "private_chat", "invite": invite });
        let reply = self
            .client_api(
                Method::POST,
                &["v3", "createRoom"],
                Some(session),
                Some(&body),
            )
            .await?
            .ok("creating a room")?;
        Ok(reply
            .str_field("room_id")
            .context("createRoom returned no room_id")?
            .to_owned())
    }

    /// Joins `room_id` as `session`.
    pub async fn join_room(&self, session: &Session, room_id: &str) -> Result<()> {
        self.client_api(
            Method::POST,
            &["v3", "join", room_id],
            Some(session),
            Some(&json!({})),
        )
        .await?
        .ok("joining a room")?;
        Ok(())
    }

    /// Sends an `m.text` message as `session` and returns the raw reply, so
    /// a test can check a refusal.
    pub async fn send_text(&self, session: &Session, room_id: &str, text: &str) -> Result<Reply> {
        let txn = Self::unique_localpart("txn");
        self.client_api(
            Method::PUT,
            &["v3", "rooms", room_id, "send", "m.room.message", &txn],
            Some(session),
            Some(&json!({ "msgtype": "m.text", "body": text })),
        )
        .await
    }

    /// The latest timeline events of `room_id` as `session` sees them, newest
    /// first.
    pub async fn recent_events(&self, session: &Session, room_id: &str) -> Result<Vec<Value>> {
        let mut url = Self::url(
            &self.env.hs1_url,
            &["_matrix", "client", "v3", "rooms", room_id, "messages"],
        )?;
        url.query_pairs_mut()
            .append_pair("dir", "b")
            .append_pair("limit", "20");
        let reply = send(self.edge.get(url).bearer_auth(session.access_token()))
            .await?
            .ok("reading a room's messages")?;
        Ok(reply.body["chunk"].as_array().cloned().unwrap_or_default())
    }
}

async fn send(request: RequestBuilder) -> Result<Reply> {
    let response = request.send().await.context("the request failed")?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .context("could not read the response body")?;
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    Ok(Reply { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_each_path_segment() {
        let url = HomeserverHarness::url(
            "https://hs1.familychat.test",
            &["_matrix", "client", "v3", "rooms", "!a/b?c#d:x", "send"],
        )
        .expect("url");
        assert_eq!(
            url.as_str(),
            "https://hs1.familychat.test/_matrix/client/v3/rooms/!a%2Fb%3Fc%23d:x/send"
        );
    }

    #[test]
    fn debug_output_redacts_credentials() {
        let session = Session::from_login_body(&json!({
            "user_id": "@a:hs1.familychat.test",
            "device_id": "DEV",
            "access_token": "secret-access",
        }))
        .expect("session");
        let token = LoginToken {
            secret: "secret-login".to_owned(),
            expires_in_ms: 1,
        };
        let debug = format!("{session:?} {token:?}");
        assert!(!debug.contains("secret-"), "{debug}");
        assert!(debug.contains("@a:hs1.familychat.test"), "{debug}");
    }

    #[test]
    fn localparts_are_unique() {
        let first = HomeserverHarness::unique_localpart("alice");
        let second = HomeserverHarness::unique_localpart("alice");
        assert_ne!(first, second);
        assert!(first.starts_with("alice-"));
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }

    #[test]
    fn reply_reads_matrix_errors() {
        let reply = Reply {
            status: 403,
            body: json!({ "errcode": "M_FORBIDDEN", "error": "no" }),
        };
        assert_eq!(reply.errcode(), Some("M_FORBIDDEN"));
        assert!(reply.ok("x").is_err());
    }
}
