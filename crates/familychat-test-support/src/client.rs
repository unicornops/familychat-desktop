//! The harness's test-only HTTP clients (the `test-harness` feature).
//!
//! [`edge_client`] is the only place in the workspace that trusts the
//! harness CA. It trusts nothing else (no system or bundled roots), resolves
//! only the harness hostnames, all to the front end on loopback, and refuses
//! any other name, so a test cannot reach the internet by mistake. Like the
//! code-redeeming client PLAN.md section 6.4 describes, it never follows a
//! redirect or uses a proxy.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use reqwest::{
    Certificate, Client,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect,
};

use crate::HarnessEnv;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolves the harness hostnames to the front end's loopback address and
/// fails every other name.
#[derive(Debug, Clone)]
pub struct HarnessResolver {
    hosts: Vec<String>,
    edge_addr: SocketAddr,
}

impl HarnessResolver {
    /// A resolver for `hosts`, all answered with `edge_addr`.
    pub fn new(hosts: Vec<String>, edge_addr: SocketAddr) -> Self {
        let hosts = hosts
            .into_iter()
            .map(|host| host.to_ascii_lowercase())
            .collect();
        Self { hosts, edge_addr }
    }

    /// The address for `host`, or `None` when it is not a harness host.
    pub fn lookup(&self, host: &str) -> Option<SocketAddr> {
        let host = host.to_ascii_lowercase();
        self.hosts.contains(&host).then_some(self.edge_addr)
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
/// redirects and no proxy.
pub fn edge_client(env: &HarnessEnv) -> Result<Client> {
    let pem = std::fs::read(&env.ca_pem).with_context(|| {
        format!(
            "could not read the harness CA at {}; restart the harness with `scripts/homeserver-harness.sh up`",
            env.ca_pem.display()
        )
    })?;
    let ca = Certificate::from_pem(&pem).context("the harness CA is not a PEM certificate")?;
    Client::builder()
        .https_only(true)
        .tls_certs_only([ca])
        .dns_resolver(Arc::new(HarnessResolver::new(env.hosts(), env.edge_addr)))
        .redirect(redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not build the harness HTTP client")
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
