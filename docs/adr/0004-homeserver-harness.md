# ADR 0004: The Local Homeserver Harness

## Status

Accepted on November 5, 2026, with Phase 2 (issue #3). It follows
PLAN.md section 9 (the live harness) and section 13.2, and Zmail's
mail and Proton harnesses (unicornops/zmail, `docker/mail-harness`,
`scripts/proton-harness.sh`).

## Context

From Phase 3 on, sessions, sign-in, encryption and the frontends need
real homeservers to test against, not only mocks: the parts most likely to
go wrong (the code sign-in contract, discovery and the allowlist, curfew
suspension, the edge proxy that hides admin paths) are behaviours of Family
Chat's servers, and those are Palpo behind an nginx edge proxy, configured
by family-chat's control panel. Generic Matrix behaviour should also hold
against a second implementation.

The tests need, from one command on a developer machine and in CI:

- Palpo exactly as a family gets it, with a working admin API, so a test
  can create members, mint login tokens and suspend members the way the
  control panel does;
- real TLS with the hostnames the sign-in rules check (an allowlisted
  family host, a custom domain, a host outside the allowlist), without
  trusting anything machine-wide;
- a way to keep that trust out of every release build.

## Decision

### The harness

`docker/homeserver-harness` (Docker Compose) runs:

| Service | Image | Role |
| --- | --- | --- |
| `postgres` | `postgres:18-alpine` | Palpo's sidecar, as family-chat's `PalpoType` runs it (trust auth, PGDATA in a subdirectory) |
| `palpo` | `ghcr.io/unicornops/palpo:sha-b188d47` | family-chat's `PALPO_IMAGE`; config rendered like `render_palpo_config` |
| `synapse` | `matrixdotorg/synapse:v1.164.2` | "any homeserver" coverage; config from the image's `generate` mode |
| `edge` | `nginx:1.30.6-alpine` | the TLS front end on `127.0.0.1:18443` |

Every image is pinned by tag and digest; `docs/upstream/homeservers.md`
records the pins and how to move them, and how to follow family-chat's
Palpo pin.

The front end serves, with one certificate signed by a throwaway CA:

- `hs1.familychat.test`: a family's edge proxy, the location blocks of
  family-chat's `render_nginx_edge_config` with no approved federation
  peers. Only the well-known documents, `/_matrix/client/`,
  `/_matrix/media/` (11 MiB), `/_matrix/key/` and gated federation reach
  Palpo; `/_synapse/` is a 404, as in production. Palpo's server name is
  `hs1.familychat.test`.
- `smith.test`: a bring-your-own-domain family's domain, which serves only
  `/.well-known/matrix/client` and `/server` delegating to
  `hs1.familychat.test`, as family-chat's domain verification expects.
- `evil.familychat.example`: a working homeserver (the same Palpo) outside
  the allowlist, for the sign-in refusal tests.
- `redirect.familychat.test` (Phase 4): an allowlisted host whose
  `POST /_matrix/client/v3/login` answers `307` to
  `evil.familychat.example`, and whose other client paths are hs1's Palpo,
  so a client that followed redirects would send a token or a password
  outside the allowlist (ADR 0006).
- `synapse.test`: Synapse.

The front end also writes every request to an access log the tests read
(Phase 4): `edge-logs/access.log` in the state directory
(`FAMILYCHAT_HARNESS_ACCESS_LOG`), one `host method path status` line per
request, without query strings. `HomeserverHarness::access_log` and
`wait_for_access` read it; the sign-in tests prove from it which host a
token or a password reached, and that the non-allowlisted host never saw
a login.

Palpo's own HTTP port is published on `127.0.0.1:18008` for the admin API
only, standing in for the control panel's port-forward. Client traffic
always goes through the front end, so Palpo's per-peer rate limits see one
peer, as they see a family's edge proxy.

Synapse's is published on `127.0.0.1:18009`, also for its admin API only
(Phase 3): the script appends the harness's settings to the config
Synapse generates (refresh tokens for logins that ask for them, with
20-second access tokens, so the session tests see matrix-sdk rotate them;
login and message rate limits raised, since every request comes from the
front end), creates a Synapse admin with the image's
`register_new_matrix_user` and the generated shared secret, and writes its
token and the admin URL to the env file (`FAMILYCHAT_HARNESS_SYNAPSE_*`).
`HomeserverHarness::new_synapse_user` creates Synapse users through
`PUT /_synapse/admin/v2/users/{mxid}` and signs them in with a refresh
token.

### The script

`scripts/homeserver-harness.sh up|down|reset|env|mint-token|create-user|logs|ps`
is the single source of truth for starting the harness. `up` pulls the
pinned images (with retries, and a Docker Hub login when
`DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN` are set, as in Zmail), makes a
new CA and leaf certificate and deletes the CA key, renders `palpo.toml`
with a fresh registration token, lets Synapse generate its config, starts
the services, waits for each host, and bootstraps the admin bot exactly as
family-chat's `PalpoType.bootstrap_admin` does:

1. register `@panel-admin` through token-gated `/register` (the
   registration token is the bot's password, as in family-chat);
2. `UPDATE users SET is_admin = true` in Palpo's Postgres;
3. log in with the password and check an admin route answers.

It then writes `target/homeserver-harness/env` (mode 0600):
`FAMILYCHAT_HARNESS_*` exports with the front end's address, the CA path,
the base URLs, the admin API and the admin bot's access token. `env` prints
it; `mint-token USER` mints a login token through
`POST /_synapse/admin/v1/users/{mxid}/login_token` as family-chat's
`mint_login_token` does.

### The driver and the CA trust

`crates/familychat-test-support` holds `HarnessEnv` (the settings parser,
always built) and, behind its `test-harness` feature, `HomeserverHarness`
and the clients:

- `edge_client` trusts only the harness CA (no system or bundled roots),
  resolves only the harness hostnames, all to the front end, and fails any
  other name, is HTTPS-only, and never follows redirects or uses a proxy.
  It is reqwest 0.13 on rustls, the line matrix-sdk 0.19 uses, so Phase 3
  can hand the same client to the SDK.
- `HomeserverHarness` creates members and suspends them through the admin
  API, mints login tokens, registers through the token gate, signs in, and
  creates, joins and sends to rooms as any member.

The feature is enabled only by the crate's own dev-dependency on itself,
so `cargo test --workspace` and clippy build the driver and nothing else
can see it. Three guards keep it out of releases:

- the crate refuses to compile with the feature in a build without debug
  assertions (`compile_error!`), as Zmail's `zmail-proton-api` does;
- `scripts/check-release-features.sh` reads the graph a release build of
  `familychat-app` compiles (default features, normal and build
  dependencies, every target) and fails if it contains
  `familychat-test-support` or any `test-harness` feature; it self-tests
  against the feature turned on. CI's quality job and the release workflow
  run it. (From Phase 5 the app's opt-in `ui-fixture` feature reaches
  `familychat-test-support`'s fixtures, never the harness; the check also
  fails if that feature becomes a default, and self-tests that it sees it,
  ADR 0011);
- the allowlist's `test-harness` feature (PLAN.md section 6.5), when the
  sign-in crate adds it in Phase 4, is held to the same check.

### The live tests

`crates/familychat-test-support/tests/live_homeserver_harness.rs` are
`#[ignore]` tests that need the harness up. CI's quality job (Linux) runs
`up`, the script's other commands, the tests with `-- --ignored`, dumps
every service's log on failure and runs `down` always. They prove:

- Palpo (it names itself through MSC4383) advertises v1.11 and v1.12, native sliding sync
  (`org.matrix.simplified_msc3575`), authenticated media, and the
  `m.login.password` and `m.login.token` login types, and answers a native
  sliding sync request;
- token-gated registration, then password login (a wrong password is 403
  `M_FORBIDDEN`);
- a minted login token redeems once with `m.login.token` (200, the right
  user) and the replay gets 403 `M_FORBIDDEN`;
- a member signed in with a code uploads cross-signing keys without
  user-interactive auth the first time (MSC3967), and needs it to replace
  them;
- `smith.test` delegates to `hs1.familychat.test` and serves no API; the
  edge exposes only the edge proxy's paths; the non-allowlisted host is a
  working homeserver; Synapse answers `/versions` and is Synapse;
- two members share a room and see each other's messages;
- a suspended member's send fails with `M_USER_SUSPENDED`, and lifting the
  suspension lets them send again. At the pin a suspended member's reads
  (`whoami`, and so `/sync`) fail the same way; PLAN.md section 6.8 expects
  reads to keep working, so the test pins Palpo's behaviour and will fail
  when it changes.

## Consequences

- Tests in later phases use real servers through `HomeserverHarness`;
  mocks (matrix-sdk's `MatrixMockServer`) stay for what a real server
  cannot be made to do.
- The harness runs only where Docker can: CI's Linux quality job and
  developer machines. macOS and Windows CI keep to unit and mocked tests.
- A harness run shares Palpo's rate limits: login is family-chat's
  household bucket (burst 20, one every five seconds), registration
  Palpo's default (burst 3), so tests create members through the admin API
  and register through the token gate only where that is the point.
- Palpo moves when family-chat moves `PALPO_IMAGE`, by hand
  (`docs/upstream/homeservers.md`); family-chat's repository is not
  reachable from this repository's CI, so nothing checks the two agree.
- One Palpo serves one server name, so the harness has no family whose
  Matrix server name is its custom domain (`@alice:smith.test` served from
  a `*.familychat.test` host). Phase 4 did not add a second Palpo (another
  Postgres, another admin bootstrap, on every CI run): the live tests prove
  `account_provider=smith.test` by password through its well-known, prove
  the user binding by refusing a code redeemed at hs1 for a link that
  names `smith.test` or another member, and the offline flow tests cover a
  custom-domain code end to end (ADR 0006). A second Palpo is the way to
  add a live custom-domain code sign-in later.
- MAS for OAuth sign-in is not in the harness; Phase 14 adds Synapse with
  MAS.
