# Homeserver Upstream Record

The live homeserver harness ([ADR 0004](../adr/0004-homeserver-harness.md),
`docker/homeserver-harness`) runs real homeservers. Its images are pinned
by tag and digest in `docker/homeserver-harness/docker-compose.yml`, which
is the source of truth; this page records why each pin is what it is and
how to move it.

## Current Pins

| Service | Image | Digest | Follows |
| --- | --- | --- | --- |
| `palpo` | `ghcr.io/unicornops/palpo:sha-b188d47` | `sha256:cb50e0fb431d616980a1e4425115dc4039fab7dedbd8e1e3c4de808663842493` | family-chat's `PALPO_IMAGE` |
| `postgres` | `postgres:18-alpine` | `sha256:10a5c69a83035ab5239f5620d708520e8b28f5aa1f2b20143e6024532edadfb5` | family-chat's `PALPO_POSTGRES_IMAGE` |
| `synapse` | `matrixdotorg/synapse:v1.164.2` | `sha256:f0df87cff259872637b4b5e5174b9977e5e3046dd421505407803b5e45c5f842` | Synapse's latest stable release |
| `synapse-mas` | `matrixdotorg/synapse:v1.164.2` | `sha256:f0df87cff259872637b4b5e5174b9977e5e3046dd421505407803b5e45c5f842` | the `synapse` pin (the same image) |
| `mas` | `ghcr.io/element-hq/matrix-authentication-service:1.28.0` | `sha256:494c67f08ee8344368f4ed336611dea02b26c4e058bf37a212d090d50b612659` | MAS's latest stable release (the multi-arch index; the tag has no `v`) |
| `edge` | `nginx:1.30.6-alpine` | `sha256:0e52bd054126cdc040b1276c0af347aeb02a41b9cfe305d6094767ac30e97949` | nginx stable (family-chat's edge runs `nginx:alpine`) |

The Palpo image is built from the `familychat` branch of
[unicornops/palpo](https://github.com/unicornops/palpo) at `b188d47` (the
`sha-` tag is the commit). It is public on ghcr.io: anonymous pulls work,
so CI needs no registry credentials for it.

## Following Family Chat's Palpo

Family Chat chooses the Palpo every new family runs; the harness must run
the same one. family-chat sets it in
`infra/kubernetes/platform/configmap.yaml` (`PALPO_IMAGE`, and
`PALPO_POSTGRES_IMAGE` for the sidecar). When family-chat moves it:

1. Read the new tag from family-chat's `configmap.yaml`.
2. Resolve its digest, for example:

   ```bash
   docker buildx imagetools inspect ghcr.io/unicornops/palpo:sha-NEW
   ```

3. Update the `palpo` image line in
   `docker/homeserver-harness/docker-compose.yml` and the table above.
4. Check `docker/homeserver-harness/palpo.toml.in` against family-chat's
   `render_palpo_config` (`control-panel/apps/provisioning/homeserver_config.py`)
   at the same family-chat commit, and `docker/homeserver-harness/nginx.conf`
   against its `render_nginx_edge_config`. Palpo ignores unknown config
   keys silently, so check every key against the new image's source
   revision (`crates/server/src/config/` in unicornops/palpo), not another
   version's documentation.
5. Re-read PLAN.md section 6.10 against the new revision (spec versions in
   `crates/server/src/routing/client.rs`, login types in
   `routing/client/session.rs`, suspension in `hoops/auth.rs` and
   `user/password.rs`, cross-signing UIA in
   `routing/client/key/device_signing.rs`).
6. Run the live tests (`scripts/homeserver-harness.sh up`, then
   `cargo test -p familychat-test-support --test live_homeserver_harness -- --ignored`);
   a failure there is a change in what Family Chat's servers do, and goes
   to PLAN.md and the affected phase, not only to the test.

family-chat lives outside GitHub, so this repository's CI cannot compare
the pins; moving them is a deliberate pull request
(`chore(deps): follow family-chat's Palpo to sha-NEW`).

## Moving The Other Pins

- **Synapse:** a stable release (`vX.Y.Z`, never an `rc` or `develop`).
  Resolve the digest with `docker buildx imagetools inspect`, update the
  compose file and the table, and run the live tests.
- **Postgres:** stay on the major version family-chat runs
  (`postgres:18-alpine`); refresh the digest for security updates.
- **nginx:** the current stable branch (even minor version), `-alpine`.
- **MAS:** a stable release, resolved with
  `docker buildx imagetools inspect ghcr.io/element-hq/matrix-authentication-service:X.Y.Z`
  (the index digest). Check the rendered config with the new image's
  `mas-cli config check`, and run the live OAuth tests: they fill MAS's
  forms (below), so a changed page fails them first.

Dependabot does not watch these images.

## What The Harness Found At `sha-b188d47`

Read from the source at the pin and checked by the live tests:

- `/versions` advertises `r0.5.0` to `v1.12`, native sliding sync
  (`org.matrix.simplified_msc3575`) and authenticated media; `GET /login`
  offers `m.login.token`.
- A replayed or unknown login token is 403 `M_FORBIDDEN`.
- The first cross-signing upload of an account skips user-interactive
  auth (MSC3967); replacing the keys asks for the password stage.
- Suspension (`PUT /_synapse/admin/v1/suspend/{mxid}`) is checked in
  Palpo's access-token authentication for every request, so a suspended
  member's reads are refused with `M_USER_SUSPENDED` as well as their
  sends, not only the sends PLAN.md section 6.8 expects.
- Rate limits are per TCP peer: login per family-chat's `[rc_login]`
  (burst 20), registration at Palpo's default (burst 3, about one every
  six seconds).

## What The Harness Found At MAS 1.28.0 With Synapse 1.164.2

Worked out with a native MAS and Synapse behind nginx (Phase 14) and
checked by the live tests (`tests/live_oauth.rs`):

- Synapse takes the stable `matrix_authentication_service` section
  (`experimental_features.msc3861` is now a configuration error) and then
  answers 404 for every `login`, `logout` and `refresh` path, which the
  front end sends to MAS. `msc4108_enabled` needs MAS; `/versions` then
  lists `org.matrix.msc4108`. Synapse also implements MSC4388's rendezvous
  (`msc4388_mode`) without advertising it; the pinned matrix-sdk can't use
  it, so the harness leaves it off.
- `GET /_matrix/client/v1/auth_metadata` (and MSC2965's unstable path) is
  MAS's OpenID configuration; Synapse caches it.
- Dynamic registration accepts Family Chat Desktop's native client with a
  `http://127.0.0.1:<port>/callback` redirect under MAS's default policy,
  and matches loopback ports exactly; identical metadata gets the same
  client id.
- The browser's pages are forms with a `csrf` field and no `action` (each
  posts to itself), behind the `session` and `csrf` cookies: `/login`
  (`username`, `password`), `/consent/<grant>` (`csrf` only, then a 303 to
  the redirect, with a trailing empty `#`), `/device/<grant>` (`csrf`,
  `confirm_device=on`, `action=consent`; 200 when granted), and
  `/complete-compat-sso/<id>` for single sign-on.
- The device grant's pending poll answers 403 `authorization_pending`;
  `verification_uri_complete` exists only with
  `oauth.device_code_user_code_auto_fill_enabled`.
- Revoking the access token ends the whole session; Synapse may answer
  from its cache for a moment after, so the tests wait for the 401.
- The default login rate limit is a burst of 3 per address, too few for
  the tests.
