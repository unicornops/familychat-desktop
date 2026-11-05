//! The harness's test-only HTTP clients (the `test-harness` feature).
//!
//! [`edge_client`] (and [`edge_client_builder`], the same settings before
//! they are built) is the only place in the workspace that trusts the
//! harness CA. It trusts nothing else (no system or bundled roots), resolves
//! only the harness hostnames, all to the front end on loopback, and refuses
//! any other name, so a test cannot reach the internet by mistake. Like the
//! code-redeeming client PLAN.md section 6.4 describes, it never follows a
//! redirect, and it uses no proxy except the egress test's recording proxy
//! (`FAMILYCHAT_HARNESS_EGRESS_PROXY`, `scripts/egress-proxy.py`), which
//! records every host it is asked for and tunnels only the harness hosts
//! to the front end (ADR 0004, "Egress").

use std::{
    io::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use reqwest::{
    Certificate, Client, ClientBuilder, Proxy,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect,
};

use crate::HarnessEnv;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolves the harness hostnames to the front end's loopback address and
/// fails every other name, recording each refusal in the egress log when
/// there is one.
#[derive(Debug, Clone)]
pub struct HarnessResolver {
    hosts: Vec<String>,
    edge_addr: SocketAddr,
    refusal_log: Option<PathBuf>,
}

impl HarnessResolver {
    /// A resolver for `hosts`, all answered with `edge_addr`.
    pub fn new(hosts: Vec<String>, edge_addr: SocketAddr) -> Self {
        let hosts = hosts
            .into_iter()
            .map(|host| host.to_ascii_lowercase())
            .collect();
        Self {
            hosts,
            edge_addr,
            refusal_log: None,
        }
    }

    /// The resolver, appending `REFUSED name` to `path` for each name it
    /// refuses (the egress check fails the build on any).
    pub fn with_refusal_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.refusal_log = Some(path.into());
        self
    }

    /// The address for `host`, or `None` when it is not a harness host
    /// (recorded in the refusal log).
    pub fn lookup(&self, host: &str) -> Option<SocketAddr> {
        let host = host.to_ascii_lowercase();
        let answer = self.hosts.contains(&host).then_some(self.edge_addr);
        if answer.is_none()
            && let Some(path) = &self.refusal_log
        {
            record_refusal(path, &host);
        }
        answer
    }
}

/// Appends `REFUSED host` to the egress log. A log that can't be written
/// is reported on standard error; the lookup is refused either way.
fn record_refusal(path: &Path, host: &str) {
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        })
        .and_then(|mut log| writeln!(log, "REFUSED {host}"));
    if let Err(error) = written {
        eprintln!(
            "could not record the refused name {host} in {}: {error}",
            path.display()
        );
    }
}

impl Resolve for HarnessResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let answer = self.lookup(name.as_str());
        let host = name.as_str().to_owned();
        Box::pin(async move {
            match answer {
                Some(addr) => Ok(Box::new(std::iter::once(addr)) as Addrs),
                None => Err(format!("{host} is not a homeserver harness host").into()),
            }
        })
    }
}

/// The client for everything behind the harness front end: HTTPS only,
/// trusting only the harness CA, resolving only the harness hosts, with no
/// redirects and no proxy but the egress test's.
pub fn edge_client(env: &HarnessEnv) -> Result<Client> {
    edge_client_builder(env)?
        .build()
        .context("could not build the harness HTTP client")
}

/// [`edge_client`]'s builder, for a client that adds settings of its own:
/// the sign-in's transport (`familychat_signin::ReqwestTransport::with_builder`)
/// applies its own redirect, retry and timeout settings on top.
pub fn edge_client_builder(env: &HarnessEnv) -> Result<ClientBuilder> {
    let pem = std::fs::read(&env.ca_pem).with_context(|| {
        format!(
            "could not read the harness CA at {}; restart the harness with `scripts/homeserver-harness.sh up`",
            env.ca_pem.display()
        )
    })?;
    let ca = Certificate::from_pem(&pem).context("the harness CA is not a PEM certificate")?;
    let mut resolver = HarnessResolver::new(env.hosts(), env.edge_addr);
    if let Some(log) = &env.egress_log {
        resolver = resolver.with_refusal_log(log);
    }
    let builder = Client::builder()
        .https_only(true)
        .tls_certs_only([ca])
        .dns_resolver(Arc::new(resolver))
        .redirect(redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    // Through the recording proxy, the proxy is the only address the
    // client connects to: it tunnels the harness hosts to the front end
    // and refuses (and records) every other one.
    Ok(match &env.egress_proxy {
        Some(proxy) => builder.proxy(
            Proxy::all(proxy.as_str())
                .with_context(|| format!("the egress proxy {proxy} is not a proxy URL"))?,
        ),
        None => builder.no_proxy(),
    })
}

/// The client for Palpo's admin API on loopback (plain HTTP, the stand-in
/// for the control panel's port-forward), with no redirects and no proxy.
pub fn admin_client() -> Result<Client> {
    Client::builder()
        .redirect(redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not build the harness admin HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> HarnessResolver {
        HarnessResolver::new(
            vec!["hs1.familychat.test".to_owned(), "smith.test".to_owned()],
            "127.0.0.1:18443".parse().expect("address"),
        )
    }

    #[tokio::test]
    async fn resolves_only_the_harness_hosts() {
        let resolver = resolver();
        let name: Name = "HS1.familychat.test".parse().expect("name");
        let addrs: Vec<_> = resolver.resolve(name).await.expect("resolves").collect();
        assert_eq!(addrs, ["127.0.0.1:18443".parse().expect("address")]);

        for other in ["example.org", "familychat.test", "evil.smith.test"] {
            let name: Name = other.parse().expect("name");
            assert!(resolver.resolve(name).await.is_err(), "resolved {other}");
        }
    }

    #[tokio::test]
    async fn records_every_refused_name() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("familychat-egress-{}-{nanos}", std::process::id()));
        let log = dir.join("resolver.log");
        let resolver = resolver().with_refusal_log(&log);
        for name in [
            "hs1.familychat.test",
            "Example.ORG",
            "smith.test",
            "matrix.org",
        ] {
            let name: Name = name.parse().expect("name");
            let _ = resolver.resolve(name).await;
        }
        let recorded = std::fs::read_to_string(&log).expect("the refusals are recorded");
        assert_eq!(recorded, "REFUSED example.org\nREFUSED matrix.org\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_a_missing_ca() {
        let lookup = |key: &str| {
            Some(
                match key {
                    "FAMILYCHAT_HARNESS_EDGE_ADDR" => "127.0.0.1:18443",
                    "FAMILYCHAT_HARNESS_CA_PEM" => "/nonexistent/harness/ca.pem",
                    "FAMILYCHAT_HARNESS_SERVER_NAME" => "hs1.familychat.test",
                    "FAMILYCHAT_HARNESS_CUSTOM_DOMAIN" => "smith.test",
                    "FAMILYCHAT_HARNESS_ADMIN_URL" => "http://127.0.0.1:18008",
                    "FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_URL" => "http://127.0.0.1:18009",
                    "FAMILYCHAT_HARNESS_HS1_URL" => "https://hs1.familychat.test",
                    "FAMILYCHAT_HARNESS_EVIL_URL" => "https://evil.familychat.example",
                    "FAMILYCHAT_HARNESS_SYNAPSE_URL" => "https://synapse.test",
                    "FAMILYCHAT_HARNESS_REDIRECT_URL" => "https://redirect.familychat.test",
                    _ => "value",
                }
                .to_owned(),
            )
        };
        let env = HarnessEnv::from_lookup(lookup).expect("valid settings");
        let error = edge_client(&env).expect_err("no CA, no client");
        assert!(error.to_string().contains("harness CA"), "{error}");
    }
}
