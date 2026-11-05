//! The harness's MAS (Phase 14, ADR 0010; the `test-harness` feature):
//! users made through MAS's admin API, and [`MasBrowser`], which plays the
//! person's browser for a live test.
//!
//! MAS's pages are plain HTML forms, so the browser is an HTTP client with
//! a cookie jar: it follows MAS's redirects itself (the edge client never
//! does), fills the login form, and consents (an app's sign-in, a new
//! device's, a single sign-on), exactly the requests a browser makes. It
//! stops at the redirect that leaves the harness, the app's loopback
//! address, and hands it back; [`MasBrowser::come_back`] then goes there as
//! the browser would. Nothing here talks to the app: the app only sees a
//! browser coming back.
//!
//! The pages and their fields are MAS 1.28.0's
//! (`docs/upstream/homeservers.md` records what they are).

use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Method, Url, header};
use serde_json::{Value, json};

use crate::{HomeserverHarness, admin_client};

const FORM: &str = "application/x-www-form-urlencoded";

/// `pairs` as a form body.
fn encode_form(pairs: &[(&str, &str)]) -> String {
    let mut url = Url::parse("http://form.invalid/").unwrap_or_else(|_| unreachable!("a constant"));
    url.query_pairs_mut().extend_pairs(pairs);
    url.query().unwrap_or_default().to_owned()
}

/// A user on MAS, made for one test.
#[derive(Clone)]
pub struct MasUser {
    /// The Matrix ID, on `synapse-mas.familychat.test`.
    pub user_id: String,
    /// The username MAS's login form takes.
    pub localpart: String,
    /// The password.
    pub password: String,
}

impl fmt::Debug for MasUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MasUser")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

impl HomeserverHarness {
    /// `https://synapse-mas.familychat.test`: Synapse with MAS.
    pub fn synapse_mas_url(&self) -> &str {
        &self.env().synapse_mas_url
    }

    /// Synapse with MAS's server name, `synapse-mas.familychat.test`.
    pub fn synapse_mas_server_name(&self) -> Result<&str> {
        self.env()
            .synapse_mas_url
            .strip_prefix("https://")
            .context("the Synapse with MAS URL is not https://")
    }

    /// `https://mas.familychat.test`: MAS, the issuer.
    pub fn mas_url(&self) -> &str {
        &self.env().mas_url
    }

    /// An access token for MAS's admin API (client credentials).
    async fn mas_admin_token(&self) -> Result<String> {
        let url = HomeserverHarness::url(self.mas_url(), &["oauth2", "token"])?;
        let response = self
            .client()
            .post(url)
            .basic_auth(
                &self.env().mas_admin_client,
                Some(self.env().mas_admin_secret()),
            )
            .header(header::CONTENT_TYPE, FORM)
            .body(encode_form(&[
                ("grant_type", "client_credentials"),
                ("scope", "urn:mas:admin"),
            ]))
            .send()
            .await
            .context("MAS's token endpoint")?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        ensure!(status == 200, "MAS refused the admin client ({status}): {body}");
        body.get("access_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("no admin access token")
    }

    /// Makes a MAS user with a password through MAS's admin API; MAS makes
    /// them on Synapse too.
    pub async fn new_mas_user(&self, prefix: &str) -> Result<MasUser> {
        let localpart = HomeserverHarness::unique_localpart(prefix);
        let password = format!("{localpart}-Password-1");
        let token = self.mas_admin_token().await?;
        let users = HomeserverHarness::url(self.mas_url(), &["api", "admin", "v1", "users"])?;
        let response = self
            .client()
            .post(users.clone())
            .bearer_auth(&token)
            .json(&json!({ "username": localpart }))
            .send()
            .await
            .context("MAS's admin API")?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        ensure!(status == 201, "creating {localpart} on MAS answered {status}: {body}");
        let id = body
            .pointer("/data/id")
            .and_then(Value::as_str)
            .context("MAS named no user id")?
            .to_owned();
        let set_password = HomeserverHarness::url(
            self.mas_url(),
            &["api", "admin", "v1", "users", &id, "set-password"],
        )?;
        let response = self
            .client()
            .post(set_password)
            .bearer_auth(&token)
            .json(&json!({ "password": password, "skip_password_check": true }))
            .send()
            .await
            .context("MAS's admin API")?;
        ensure!(
            response.status() == 204,
            "setting {localpart}'s password answered {}",
            response.status()
        );
        Ok(MasUser {
            user_id: format!("@{localpart}:{}", self.synapse_mas_server_name()?),
            localpart,
            password,
        })
    }

    /// A browser with no cookies, for a person signing in at MAS.
    pub fn browser(&self) -> Result<MasBrowser> {
        Ok(MasBrowser {
            client: self.client().clone(),
            loopback: admin_client()?,
            cookies: BTreeMap::new(),
        })
    }
}

/// One page as the browser got it.
#[derive(Debug, Clone)]
pub struct Page {
    /// Where it is.
    pub url: Url,
    /// Its status.
    pub status: u16,
    /// Where a redirect points.
    pub location: Option<Url>,
    /// Its HTML (or JSON).
    pub body: String,
}

impl Page {
    /// The value of the hidden form field `name` (MAS's `csrf`).
    pub fn field(&self, name: &str) -> Option<String> {
        let marker = format!("name=\"{name}\"");
        let start = self.body.find(&marker)?;
        let tag_start = self.body[..start].rfind('<')?;
        let tag_end = start + self.body[start..].find('>')?;
        let tag = &self.body[tag_start..tag_end];
        let value = tag.split("value=\"").nth(1)?;
        Some(html_unescape(&value[..value.find('"')?]))
    }

    fn is_redirect(&self) -> bool {
        (300..400).contains(&self.status)
    }
}

fn html_unescape(text: &str) -> String {
    text.replace("&#x2F;", "/")
        .replace("&#x3D;", "=")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// A person's browser at MAS (see the module's notes).
pub struct MasBrowser {
    client: Client,
    loopback: Client,
    cookies: BTreeMap<String, String>,
}

impl fmt::Debug for MasBrowser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MasBrowser")
            .field("cookies", &self.cookies.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl MasBrowser {
    async fn request(&mut self, method: Method, url: Url, form: Option<&[(&str, &str)]>) -> Result<Page> {
        let mut request = self.client.request(method.clone(), url.clone());
        if !self.cookies.is_empty() {
            let cookies = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header(header::COOKIE, cookies);
        }
        if let Some(form) = form {
            request = request
                .header(header::CONTENT_TYPE, FORM)
                .body(encode_form(form));
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("the browser's {method} {}", url.path()))?;
        for set in response.headers().get_all(header::SET_COOKIE) {
            let Ok(set) = set.to_str() else { continue };
            let pair = set.split(';').next().unwrap_or_default();
            if let Some((name, value)) = pair.split_once('=') {
                let expired = set.to_ascii_lowercase().contains("max-age=0");
                if value.is_empty() || expired {
                    self.cookies.remove(name.trim());
                } else {
                    self.cookies
                        .insert(name.trim().to_owned(), value.trim().to_owned());
                }
            }
        }
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|location| url.join(location))
            .transpose()
            .context("a redirect that is not an address")?;
        let body = response.text().await.unwrap_or_default();
        Ok(Page {
            url,
            status,
            location,
            body,
        })
    }

    /// Follows redirects within the harness (HTTPS); stops at a page, or
    /// at a redirect elsewhere (the app's loopback address).
    async fn follow(&mut self, mut page: Page) -> Result<Page> {
        for _ in 0..20 {
            match &page.location {
                Some(next) if page.is_redirect() && next.scheme() == "https" => {
                    let next = next.clone();
                    page = self.request(Method::GET, next, None).await?;
                }
                _ => return Ok(page),
            }
        }
        bail!("too many redirects from {}", page.url)
    }

    async fn get(&mut self, url: &str) -> Result<Page> {
        let url = Url::parse(url).context("not an address")?;
        let page = self.request(Method::GET, url, None).await?;
        self.follow(page).await
    }

    async fn post(&mut self, page: &Page, form: &[(&str, &str)]) -> Result<Page> {
        let posted = self
            .request(Method::POST, page.url.clone(), Some(form))
            .await?;
        self.follow(posted).await
    }

    /// Fills MAS's login form on `page` if that is what it is.
    async fn log_in_if_asked(&mut self, page: Page, user: &MasUser) -> Result<Page> {
        if !(page.url.path() == "/login" && page.body.contains("name=\"password\"")) {
            return Ok(page);
        }
        let csrf = page.field("csrf").context("the login form has no csrf")?;
        let next = self
            .post(
                &page,
                &[
                    ("csrf", &csrf),
                    ("username", &user.localpart),
                    ("password", &user.password),
                ],
            )
            .await?;
        ensure!(
            next.url.path() != "/login" || next.is_redirect(),
            "MAS refused the login for {}: {}",
            user.user_id,
            next.status
        );
        Ok(next)
    }

    /// Signs `user` in at the authorization address an app opened, and
    /// allows the app: returns the redirect MAS sends the browser to (the
    /// app's loopback address, with the code).
    pub async fn sign_in_at(&mut self, authorization_url: &str, user: &MasUser) -> Result<Url> {
        let page = self.get(authorization_url).await?;
        let page = self.log_in_if_asked(page, user).await?;
        ensure!(
            page.url.path().starts_with("/consent/"),
            "expected MAS's consent page, got {} ({})",
            page.url.path(),
            page.status
        );
        let csrf = page.field("csrf").context("the consent form has no csrf")?;
        let back = self.post(&page, &[("csrf", &csrf)]).await?;
        ensure!(back.is_redirect(), "the consent answered {}", back.status);
        back.location
            .context("the consent sent the browser nowhere")
    }

    /// Approves a new device's sign-in (the device authorization grant) at
    /// the address the QR sign-in gave, signing `user` in first if MAS asks.
    pub async fn approve_device(&mut self, verification_url: &str, user: &MasUser) -> Result<()> {
        let page = self.get(verification_url).await?;
        let page = self.log_in_if_asked(page, user).await?;
        ensure!(
            page.url.path().starts_with("/device/"),
            "expected MAS's device page, got {} ({})",
            page.url.path(),
            page.status
        );
        let csrf = page.field("csrf").context("the device form has no csrf")?;
        let done = self
            .post(
                &page,
                &[
                    ("csrf", &csrf),
                    ("confirm_device", "on"),
                    ("action", "consent"),
                ],
            )
            .await?;
        ensure!(
            done.status == 200,
            "approving the device answered {}",
            done.status
        );
        Ok(())
    }

    /// Completes a single sign-on (MAS's compatibility layer) at the
    /// address an app opened: returns the redirect with the login token.
    pub async fn complete_sso(&mut self, sso_url: &str, user: &MasUser) -> Result<Url> {
        let page = self.get(sso_url).await?;
        let page = self.log_in_if_asked(page, user).await?;
        ensure!(
            page.url.path().starts_with("/complete-compat-sso/"),
            "expected MAS's single sign-on page, got {} ({})",
            page.url.path(),
            page.status
        );
        let csrf = page.field("csrf").context("the form has no csrf")?;
        let back = self.post(&page, &[("csrf", &csrf)]).await?;
        back.location
            .context("the single sign-on sent the browser nowhere")
    }

    /// Goes where MAS sent the browser: the app's loopback redirect (plain
    /// HTTP on 127.0.0.1). The status the app's page answered.
    pub async fn come_back(&self, redirect: &Url) -> Result<u16> {
        ensure!(
            redirect.scheme() == "http" && redirect.host_str() == Some("127.0.0.1"),
            "MAS sent the browser to {}, not the app's loopback",
            redirect.origin().ascii_serialization()
        );
        let mut redirect = redirect.clone();
        redirect.set_fragment(None);
        let response = self
            .loopback
            .get(redirect)
            .send()
            .await
            .context("the app's loopback redirect")?;
        Ok(response.status().as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_hidden_field_in_any_attribute_order() {
        let page = Page {
            url: Url::parse("https://mas.familychat.test/login").expect("url"),
            status: 200,
            location: None,
            body: r#"<form method="POST"><input type="hidden" name="csrf" value="a&#x2F;b" />
                <input name="other" value="x"><input value="y" name="z"></form>"#
                .to_owned(),
        };
        assert_eq!(page.field("csrf").as_deref(), Some("a/b"));
        assert_eq!(page.field("other").as_deref(), Some("x"));
        assert_eq!(page.field("z").as_deref(), Some("y"));
        assert_eq!(page.field("missing"), None);
    }
}
