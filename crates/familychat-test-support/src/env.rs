//! The homeserver harness's settings, as `scripts/homeserver-harness.sh up`
//! writes them to `<state dir>/env` (`export FAMILYCHAT_HARNESS_...='...'`
//! lines) and `scripts/homeserver-harness.sh env` prints them.

use std::{
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

/// Names a settings file to read instead of the default one.
pub const ENV_FILE_VAR: &str = "FAMILYCHAT_HARNESS_ENV_FILE";
/// The harness script's state directory override.
pub const STATE_DIR_VAR: &str = "FAMILYCHAT_HARNESS_STATE_DIR";
/// The harness script's default state directory, relative to the repository.
pub const DEFAULT_STATE_DIR: &str = "target/homeserver-harness";

const EDGE_ADDR: &str = "FAMILYCHAT_HARNESS_EDGE_ADDR";
const CA_PEM: &str = "FAMILYCHAT_HARNESS_CA_PEM";
const SERVER_NAME: &str = "FAMILYCHAT_HARNESS_SERVER_NAME";
const HS1_URL: &str = "FAMILYCHAT_HARNESS_HS1_URL";
const CUSTOM_DOMAIN: &str = "FAMILYCHAT_HARNESS_CUSTOM_DOMAIN";
const EVIL_URL: &str = "FAMILYCHAT_HARNESS_EVIL_URL";
const SYNAPSE_URL: &str = "FAMILYCHAT_HARNESS_SYNAPSE_URL";
const REDIRECT_URL: &str = "FAMILYCHAT_HARNESS_REDIRECT_URL";
const ACCESS_LOG: &str = "FAMILYCHAT_HARNESS_ACCESS_LOG";
const ADMIN_URL: &str = "FAMILYCHAT_HARNESS_ADMIN_URL";
const ADMIN_USER: &str = "FAMILYCHAT_HARNESS_ADMIN_USER";
const ADMIN_TOKEN: &str = "FAMILYCHAT_HARNESS_ADMIN_TOKEN";
const REGISTRATION_TOKEN: &str = "FAMILYCHAT_HARNESS_REGISTRATION_TOKEN";
const SYNAPSE_ADMIN_URL: &str = "FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_URL";
const SYNAPSE_ADMIN_TOKEN: &str = "FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_TOKEN";

/// Where the running harness is and how to reach it.
///
/// `Debug` never prints the admin access token or the registration token.
#[derive(Clone, PartialEq, Eq)]
pub struct HarnessEnv {
    /// The TLS front end on loopback; every harness host resolves to it.
    pub edge_addr: SocketAddr,
    /// The throwaway CA that signed the front end's certificate.
    pub ca_pem: PathBuf,
    /// Palpo's Matrix server name, `hs1.familychat.test`.
    pub server_name: String,
    /// The family's client API base URL, `https://hs1.familychat.test`.
    pub hs1_url: String,
    /// The custom domain whose well-known delegates to hs1, `smith.test`.
    pub custom_domain: String,
    /// A working homeserver outside the allowlist,
    /// `https://evil.familychat.example`.
    pub evil_url: String,
    /// Synapse's client API base URL, `https://synapse.test`.
    pub synapse_url: String,
    /// An allowlisted host whose `/login` answers `307` to the
    /// non-allowlisted one, `https://redirect.familychat.test`.
    pub redirect_url: String,
    /// The front end's access log: one `host method path status` line per
    /// request.
    pub access_log: PathBuf,
    /// Palpo's admin API on loopback (plain HTTP, the control panel's
    /// port-forward stand-in).
    pub admin_url: String,
    /// The admin bot's Matrix ID, `@panel-admin:hs1.familychat.test`.
    pub admin_user: String,
    admin_token: String,
    registration_token: String,
    /// Synapse's admin API on loopback (plain HTTP), for creating Synapse
    /// users.
    pub synapse_admin_url: String,
    synapse_admin_token: String,
}

impl HarnessEnv {
    /// Reads the settings: from the file `FAMILYCHAT_HARNESS_ENV_FILE` names;
    /// else from the process environment when it carries them (after
    /// `eval "$(scripts/homeserver-harness.sh env)"`); else from
    /// `env` in `FAMILYCHAT_HARNESS_STATE_DIR` or the script's default state
    /// directory.
    pub fn load() -> Result<Self> {
        if let Some(path) = std::env::var_os(ENV_FILE_VAR) {
            return Self::from_file(Path::new(&path));
        }
        if std::env::var_os(EDGE_ADDR).is_some() {
            return Self::from_lookup(|key| std::env::var(key).ok());
        }
        let state_dir = std::env::var_os(STATE_DIR_VAR)
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_root().join(DEFAULT_STATE_DIR));
        Self::from_file(&state_dir.join("env"))
    }

    /// Reads a settings file the harness script wrote.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read the homeserver harness settings at {}; start the harness with `scripts/homeserver-harness.sh up`",
                path.display()
            )
        })?;
        let pairs =
            parse_env_file(&text).with_context(|| format!("could not parse {}", path.display()))?;
        Self::from_lookup(|key| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        })
    }

    /// Builds the settings from a variable lookup, checking each value.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str| {
            lookup(key)
                .filter(|value| !value.is_empty())
                .with_context(|| format!("the homeserver harness settings have no {key}"))
        };
        let edge_addr: SocketAddr = get(EDGE_ADDR)?
            .parse()
            .with_context(|| format!("{EDGE_ADDR} is not an address and port"))?;
        if !edge_addr.ip().is_loopback() {
            bail!("{EDGE_ADDR} must be a loopback address, not {edge_addr}");
        }
        let env = Self {
            edge_addr,
            ca_pem: PathBuf::from(get(CA_PEM)?),
            server_name: get(SERVER_NAME)?,
            hs1_url: get(HS1_URL)?,
            custom_domain: get(CUSTOM_DOMAIN)?,
            evil_url: get(EVIL_URL)?,
            synapse_url: get(SYNAPSE_URL)?,
            redirect_url: get(REDIRECT_URL)?,
            access_log: PathBuf::from(get(ACCESS_LOG)?),
            admin_url: get(ADMIN_URL)?,
            admin_user: get(ADMIN_USER)?,
            admin_token: get(ADMIN_TOKEN)?,
            registration_token: get(REGISTRATION_TOKEN)?,
            synapse_admin_url: get(SYNAPSE_ADMIN_URL)?,
            synapse_admin_token: get(SYNAPSE_ADMIN_TOKEN)?,
        };
        for (key, url) in [
            (HS1_URL, &env.hs1_url),
            (EVIL_URL, &env.evil_url),
            (SYNAPSE_URL, &env.synapse_url),
            (REDIRECT_URL, &env.redirect_url),
        ] {
            https_host(url).with_context(|| format!("{key} is not a bare https:// URL"))?;
        }
        check_host(&env.custom_domain)
            .with_context(|| format!("{CUSTOM_DOMAIN} is not a bare host"))?;
        check_host(&env.server_name)
            .with_context(|| format!("{SERVER_NAME} is not a bare host"))?;
        loopback_http(&env.admin_url)
            .with_context(|| format!("{ADMIN_URL} is not an http:// URL on loopback"))?;
        loopback_http(&env.synapse_admin_url)
            .with_context(|| format!("{SYNAPSE_ADMIN_URL} is not an http:// URL on loopback"))?;
        Ok(env)
    }

    /// The hostnames the front end serves, which the test client resolves
    /// to [`HarnessEnv::edge_addr`]: hs1, the custom domain, the
    /// non-allowlisted host, the redirecting host and Synapse's.
    pub fn hosts(&self) -> Vec<String> {
        let mut hosts = vec![self.custom_domain.clone()];
        for url in [
            &self.hs1_url,
            &self.evil_url,
            &self.redirect_url,
            &self.synapse_url,
        ] {
            if let Ok(host) = https_host(url) {
                hosts.push(host.to_owned());
            }
        }
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// The admin bot's access token, for the admin API.
    pub fn admin_token(&self) -> &str {
        &self.admin_token
    }

    /// The token that opens Palpo's token-gated registration.
    pub fn registration_token(&self) -> &str {
        &self.registration_token
    }

    /// The Synapse admin's access token, for Synapse's admin API.
    pub fn synapse_admin_token(&self) -> &str {
        &self.synapse_admin_token
    }
}

impl fmt::Debug for HarnessEnv {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HarnessEnv")
            .field("edge_addr", &self.edge_addr)
            .field("ca_pem", &self.ca_pem)
            .field("server_name", &self.server_name)
            .field("hs1_url", &self.hs1_url)
            .field("custom_domain", &self.custom_domain)
            .field("evil_url", &self.evil_url)
            .field("synapse_url", &self.synapse_url)
            .field("redirect_url", &self.redirect_url)
            .field("access_log", &self.access_log)
            .field("admin_url", &self.admin_url)
            .field("admin_user", &self.admin_user)
            .field("admin_token", &"<redacted>")
            .field("registration_token", &"<redacted>")
            .field("synapse_admin_url", &self.synapse_admin_url)
            .field("synapse_admin_token", &"<redacted>")
            .finish()
    }
}

/// Parses the `export KEY='value'` lines the harness script writes; blank
/// lines and `#` comments are skipped. Values are single-quoted with no
/// quote inside, which is all the script ever writes.
pub fn parse_env_file(text: &str) -> Result<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let assignment = line
            .strip_prefix("export ")
            .with_context(|| format!("line {number} is not an `export` line"))?;
        let (key, value) = assignment
            .split_once('=')
            .with_context(|| format!("line {number} has no `=`"))?;
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            bail!("line {number} has an invalid variable name");
        }
        let value = value
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .filter(|value| !value.contains('\''))
            .with_context(|| format!("line {number}'s value is not single-quoted"))?;
        pairs.push((key.to_owned(), value.to_owned()));
    }
    Ok(pairs)
}

/// The host of a bare `https://host` URL (no port, path, query or user).
fn https_host(url: &str) -> Result<&str> {
    let host = url
        .strip_prefix("https://")
        .context("the URL is not https://")?;
    check_host(host)?;
    Ok(host)
}

fn check_host(host: &str) -> Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        bail!("`{host}` is not a bare hostname");
    }
    Ok(())
}

fn loopback_http(url: &str) -> Result<()> {
    let authority = url
        .strip_prefix("http://")
        .context("the URL is not http://")?;
    let address: SocketAddr = authority.parse().context("no IP address and port")?;
    if !address.ip().is_loopback() {
        bail!("{address} is not loopback");
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
export FAMILYCHAT_HARNESS_EDGE_ADDR='127.0.0.1:18443'
export FAMILYCHAT_HARNESS_CA_PEM='/repo/target/homeserver-harness/ca.pem'
export FAMILYCHAT_HARNESS_SERVER_NAME='hs1.familychat.test'
export FAMILYCHAT_HARNESS_HS1_URL='https://hs1.familychat.test'
export FAMILYCHAT_HARNESS_CUSTOM_DOMAIN='smith.test'
export FAMILYCHAT_HARNESS_EVIL_URL='https://evil.familychat.example'
export FAMILYCHAT_HARNESS_SYNAPSE_URL='https://synapse.test'
export FAMILYCHAT_HARNESS_REDIRECT_URL='https://redirect.familychat.test'
export FAMILYCHAT_HARNESS_ACCESS_LOG='/repo/target/homeserver-harness/edge-logs/access.log'
export FAMILYCHAT_HARNESS_ADMIN_URL='http://127.0.0.1:18008'
export FAMILYCHAT_HARNESS_ADMIN_USER='@panel-admin:hs1.familychat.test'
export FAMILYCHAT_HARNESS_ADMIN_TOKEN='admin-secret'
export FAMILYCHAT_HARNESS_REGISTRATION_TOKEN='registration-secret'
export FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_URL='http://127.0.0.1:18009'
export FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_TOKEN='synapse-secret'
";

    fn sample_env() -> HarnessEnv {
        let pairs = parse_env_file(SAMPLE).expect("the sample parses");
        HarnessEnv::from_lookup(|key| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        })
        .expect("the sample is valid")
    }

    fn lookup_with(replace: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        let pairs = parse_env_file(SAMPLE).expect("the sample parses");
        move |key: &str| {
            if key == replace {
                return Some(value.to_owned());
            }
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn reads_what_the_harness_script_writes() {
        let env = sample_env();
        assert_eq!(env.edge_addr, "127.0.0.1:18443".parse().expect("address"));
        assert_eq!(env.server_name, "hs1.familychat.test");
        assert_eq!(env.hs1_url, "https://hs1.familychat.test");
        assert_eq!(env.admin_token(), "admin-secret");
        assert_eq!(env.registration_token(), "registration-secret");
        assert_eq!(
            env.hosts(),
            [
                "evil.familychat.example",
                "hs1.familychat.test",
                "redirect.familychat.test",
                "smith.test",
                "synapse.test"
            ]
        );
    }

    #[test]
    fn debug_output_redacts_the_tokens() {
        let debug = format!("{:?}", sample_env());
        assert!(!debug.contains("admin-secret"), "{debug}");
        assert!(!debug.contains("registration-secret"), "{debug}");
        assert!(!debug.contains("synapse-secret"), "{debug}");
        assert!(debug.contains("hs1.familychat.test"), "{debug}");
    }

    #[test]
    fn skips_blank_lines_and_comments() {
        let pairs = parse_env_file("\n# written by up\nexport A_1='x'\n\n").expect("parses");
        assert_eq!(pairs, [("A_1".to_owned(), "x".to_owned())]);
    }

    #[test]
    fn rejects_lines_it_does_not_understand() {
        for line in [
            "FAMILYCHAT_HARNESS_X='y'",
            "export FAMILYCHAT_HARNESS_X=y",
            "export FAMILYCHAT_HARNESS_X='y",
            "export FAMILYCHAT_HARNESS_X='a'b'",
            "export lower='y'",
            "export ='y'",
            "export FAMILYCHAT_HARNESS_X",
        ] {
            assert!(parse_env_file(line).is_err(), "accepted {line:?}");
        }
    }

    #[test]
    fn requires_every_setting() {
        let pairs = parse_env_file(SAMPLE).expect("the sample parses");
        for (missing, _) in &pairs {
            let result = HarnessEnv::from_lookup(|key| {
                (key != missing)
                    .then(|| pairs.iter().find(|(name, _)| name == key))
                    .flatten()
                    .map(|(_, value)| value.clone())
            });
            let error = result.expect_err("a missing setting is an error");
            assert!(error.to_string().contains(missing.as_str()), "{error}");
        }
    }

    #[test]
    fn refuses_a_front_end_off_loopback() {
        assert!(HarnessEnv::from_lookup(lookup_with(EDGE_ADDR, "192.0.2.1:18443")).is_err());
        assert!(HarnessEnv::from_lookup(lookup_with(EDGE_ADDR, "localhost:18443")).is_err());
    }

    #[test]
    fn refuses_urls_that_are_not_bare_hosts() {
        for url in [
            "http://hs1.familychat.test",
            "https://hs1.familychat.test/",
            "https://hs1.familychat.test:443",
            "https://user@hs1.familychat.test",
            "https://",
        ] {
            assert!(
                HarnessEnv::from_lookup(lookup_with(HS1_URL, url)).is_err(),
                "accepted {url}"
            );
        }
        assert!(HarnessEnv::from_lookup(lookup_with(CUSTOM_DOMAIN, "smith.test/x")).is_err());
    }

    #[test]
    fn refuses_an_admin_api_off_loopback() {
        for url in [
            "https://127.0.0.1:18008",
            "http://192.0.2.1:18008",
            "http://localhost:18008",
        ] {
            assert!(
                HarnessEnv::from_lookup(lookup_with(ADMIN_URL, url)).is_err(),
                "accepted {url}"
            );
        }
        assert!(HarnessEnv::from_lookup(lookup_with(ADMIN_URL, "http://[::1]:18008")).is_ok());
    }

    #[test]
    fn finds_the_repository_root() {
        assert!(repo_root().join("scripts/homeserver-harness.sh").is_file());
    }
}
