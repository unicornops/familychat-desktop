#!/usr/bin/env bash
set -euo pipefail

# homeserver-harness.sh - the local homeserver harness for the live tests
# (ADR 0004, docker/homeserver-harness).
#
# Usage: scripts/homeserver-harness.sh COMMAND
#
#   up                          start a fresh harness (a running one is
#                               replaced), wait until every host answers and
#                               bootstrap the admin bot
#   down                        stop the harness and remove its state
#   reset                       down, then up
#   env                         print the harness settings as shell exports
#   mint-token USER [TTL_MS]    mint a single-use m.login.token for USER (a
#                               localpart or @user:hs1.familychat.test) with
#                               the admin API and print it
#   create-user LOCALPART PASS  create (or update) a Palpo user with the
#                               admin API, as the control panel does
#   logs                        print every service's log
#   ps                          show the services
#
# The harness is Palpo at the image Family Chat runs, with its Postgres
# sidecar, Synapse, and an nginx TLS front end for these hosts, all on
# 127.0.0.1:${FAMILYCHAT_HARNESS_EDGE_PORT:-18443}:
#
#   hs1.familychat.test       a Family Chat family's edge proxy (Palpo)
#   smith.test                a custom domain: only the well-known
#                             documents, delegating to hs1.familychat.test
#   evil.familychat.example   Palpo again, outside the allowlist
#   redirect.familychat.test  allowlisted, but its /login answers 307 to
#                             evil.familychat.example
#   synapse.test              Synapse
#
# Palpo's own HTTP port is published on 127.0.0.1:${FAMILYCHAT_HARNESS_ADMIN_PORT:-18008}
# for the admin API only, standing in for the control panel's port-forward,
# and Synapse's on 127.0.0.1:${FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_PORT:-18009}
# for its admin API (creating Synapse users) only.
#
# Synapse issues refresh tokens to logins that ask for them, with access
# tokens that last 20 seconds, so the live tests see the SDK rotate them;
# its login and message rate limits are raised for the tests.
#
# `up` writes what a test needs to $FAMILYCHAT_HARNESS_STATE_DIR (default
# target/homeserver-harness):
#
#   ca.pem       the throwaway CA, made on every `up`; nothing else is signed
#                by it and its key is deleted straight away
#   env          FAMILYCHAT_HARNESS_* shell exports, read by
#                familychat-test-support's HomeserverHarness
#   edge-logs/access.log
#                the front end's access log, one `host method path status`
#                line per request (no query strings), which the sign-in
#                tests read to prove where a token or password went
#
# Environment:
#   FAMILYCHAT_HARNESS_STATE_DIR      where state is written
#   FAMILYCHAT_HARNESS_EDGE_PORT      the TLS front end's port (default 18443)
#   FAMILYCHAT_HARNESS_ADMIN_PORT     Palpo's admin port (default 18008)
#   FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_PORT
#                                     Synapse's admin port (default 18009)
#   FAMILYCHAT_HARNESS_READY_TIMEOUT  seconds to wait for readiness (default 180)
#   DOCKERHUB_USERNAME/TOKEN          optional Docker Hub login, so image pulls
#                                     use a personal quota (as in Zmail)
#
# The structure (retry, the TLS generation, the env file) follows Zmail's
# scripts/proton-harness.sh (https://github.com/unicornops/zmail,
# GPL-3.0-only).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
HARNESS_DIR="${REPO_ROOT}/docker/homeserver-harness"
COMPOSE_FILE="${HARNESS_DIR}/docker-compose.yml"

STATE_DIR="${FAMILYCHAT_HARNESS_STATE_DIR:-${REPO_ROOT}/target/homeserver-harness}"
# Docker bind mounts need an absolute path.
[[ "${STATE_DIR}" == /* ]] || STATE_DIR="${PWD}/${STATE_DIR}"
export FAMILYCHAT_HARNESS_STATE_DIR="${STATE_DIR}"
EDGE_PORT="${FAMILYCHAT_HARNESS_EDGE_PORT:-18443}"
ADMIN_PORT="${FAMILYCHAT_HARNESS_ADMIN_PORT:-18008}"
SYNAPSE_ADMIN_PORT="${FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_PORT:-18009}"
export FAMILYCHAT_HARNESS_EDGE_PORT="${EDGE_PORT}" FAMILYCHAT_HARNESS_ADMIN_PORT="${ADMIN_PORT}" \
  FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_PORT="${SYNAPSE_ADMIN_PORT}"
READY_TIMEOUT="${FAMILYCHAT_HARNESS_READY_TIMEOUT:-180}"

EDGE_ADDR="127.0.0.1:${EDGE_PORT}"
ADMIN_URL="http://127.0.0.1:${ADMIN_PORT}"
SYNAPSE_ADMIN_URL="http://127.0.0.1:${SYNAPSE_ADMIN_PORT}"
SERVER_NAME="hs1.familychat.test"
CUSTOM_DOMAIN="smith.test"
EVIL_HOST="evil.familychat.example"
REDIRECT_HOST="redirect.familychat.test"
SYNAPSE_HOST="synapse.test"
HOSTS=("${SERVER_NAME}" "${CUSTOM_DOMAIN}" "${EVIL_HOST}" "${REDIRECT_HOST}" "${SYNAPSE_HOST}")
# family-chat's HOMESERVER_ADMIN_USER.
ADMIN_LOCALPART="panel-admin"
ADMIN_MXID="@${ADMIN_LOCALPART}:${SERVER_NAME}"
# The Synapse admin the tests create Synapse users with.
SYNAPSE_ADMIN_LOCALPART="harness-admin"

TLS_DIR="${STATE_DIR}/tls"
CA_PEM="${STATE_DIR}/ca.pem"
ENV_FILE="${STATE_DIR}/env"
EDGE_LOG_DIR="${STATE_DIR}/edge-logs"
ACCESS_LOG="${EDGE_LOG_DIR}/access.log"
PALPO_TOML="${STATE_DIR}/palpo.toml"

log() {
  echo "homeserver-harness: $*" >&2
}

die() {
  log "error: $*"
  exit 1
}

require() {
  local tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null 2>&1 || die "'${tool}' is required but not installed"
  done
}

# retry ATTEMPTS DELAY COMMAND... - runs COMMAND until it succeeds, sleeping
# DELAY * attempt seconds between tries.
retry() {
  local attempts="$1" delay="$2" attempt
  shift 2
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    if "$@"; then
      return 0
    fi
    if ((attempt < attempts)); then
      log "attempt ${attempt}/${attempts} of '$*' failed; retrying in $((delay * attempt))s"
      sleep $((delay * attempt))
    fi
  done
  log "'$*' failed after ${attempts} attempts"
  return 1
}

compose() {
  docker compose --file "${COMPOSE_FILE}" "$@"
}

# The images named in the compose file, pinned by tag and digest.
images() {
  sed -n 's/^[[:space:]]*image:[[:space:]]*\([^[:space:]]*\).*$/\1/p' "${COMPOSE_FILE}"
}

# --- TLS -------------------------------------------------------------------

# A new CA and server certificate for every start, so no key outlives the
# harness run. The leaf is a plain end-entity certificate naming every
# harness host (rustls refuses a CA certificate as a server certificate).
generate_tls() {
  require openssl
  rm -rf "${TLS_DIR}"
  mkdir -p "${TLS_DIR}"
  local extensions="${TLS_DIR}/server.ext" san="" host
  for host in "${HOSTS[@]}"; do
    san="${san:+${san}, }DNS:${host}"
  done
  cat > "${extensions}" <<EOF
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = ${san}
EOF
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -days 2 -subj "/CN=familychat homeserver-harness throwaway CA" \
    -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -keyout "${TLS_DIR}/ca-key.pem" -out "${TLS_DIR}/ca.pem" 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -subj "/CN=${SERVER_NAME}" \
    -keyout "${TLS_DIR}/server-key.pem" -out "${TLS_DIR}/server.csr" 2>/dev/null
  openssl x509 -req -in "${TLS_DIR}/server.csr" -days 2 \
    -CA "${TLS_DIR}/ca.pem" -CAkey "${TLS_DIR}/ca-key.pem" -CAcreateserial \
    -extfile "${extensions}" -out "${TLS_DIR}/server.pem" 2>/dev/null
  # The CA key is no longer needed; nothing else may be signed by this CA.
  rm -f "${TLS_DIR}/ca-key.pem" "${TLS_DIR}/server.csr" "${TLS_DIR}/ca.srl" "${extensions}"
  chmod 0644 "${TLS_DIR}/server-key.pem" "${TLS_DIR}/server.pem" "${TLS_DIR}/ca.pem"
  cp "${TLS_DIR}/ca.pem" "${CA_PEM}"
}

# --- Configuration -----------------------------------------------------------

# palpo.toml with a fresh registration token. family-chat reuses the admin
# bot's password as the registration token; so does the harness.
render_palpo_config() {
  local token="$1"
  sed "s/@REGISTRATION_TOKEN@/${token}/" "${HARNESS_DIR}/palpo.toml.in" > "${PALPO_TOML}"
  # The palpo user in the container reads it through a bind mount.
  chmod 0644 "${PALPO_TOML}"
}

# Synapse writes its own config, signing key and log config into the
# synapse-data volume with the image's `generate` mode.
generate_synapse_config() {
  retry 3 5 compose run --rm --no-deps synapse generate >/dev/null \
    || die "could not generate Synapse's config"
  # The harness's additions, appended to the generated homeserver.yaml
  # (`generate` writes none of these keys).
  compose run --rm --no-deps -T --entrypoint sh synapse -c 'cat >> /data/homeserver.yaml' <<'YAML' \
    || die "could not configure Synapse"

# --- familychat homeserver harness (scripts/homeserver-harness.sh) ---
# Refresh tokens for logins that ask for them, with short-lived access
# tokens, so the live tests see matrix-sdk rotate them.
refreshable_access_token_lifetime: 20s
# Every client request reaches Synapse from the edge, one address; the
# live tests sign in and send more than the defaults allow.
rc_login:
  address:
    per_second: 10
    burst_count: 100
  account:
    per_second: 10
    burst_count: 100
  failed_attempts:
    per_second: 10
    burst_count: 100
rc_message:
  per_second: 100
  burst_count: 1000
YAML
}

# --- Images ------------------------------------------------------------------

docker_login() {
  if [[ -n "${DOCKERHUB_USERNAME:-}" && -n "${DOCKERHUB_TOKEN:-}" ]]; then
    echo "${DOCKERHUB_TOKEN}" | docker login --username "${DOCKERHUB_USERNAME}" --password-stdin >/dev/null \
      || log "Docker Hub login failed; pulling anonymously"
  fi
}

# Pulls each pinned image with retries, so a transient registry hiccup or an
# exhausted anonymous quota shows as its own clear failure.
pull_images() {
  local image
  docker_login
  while IFS= read -r image; do
    [[ -n "${image}" ]] || continue
    if docker image inspect "${image}" >/dev/null 2>&1; then
      continue
    fi
    log "pulling ${image}"
    retry 3 20 docker pull --quiet "${image}" >/dev/null || die "could not pull ${image}"
  done < <(images)
}

# --- Readiness -----------------------------------------------------------------

# edge_curl HOST PATH [CURL ARGS...] - a request to HOST through the TLS
# front end, trusting only the harness CA.
edge_curl() {
  local host="$1" path="$2"
  shift 2
  curl --silent --show-error --max-time 10 --noproxy '*' \
    --cacert "${CA_PEM}" --connect-to "${host}:443:127.0.0.1:${EDGE_PORT}" \
    "$@" "https://${host}${path}"
}

admin_curl() {
  local path="$1"
  shift
  curl --silent --show-error --max-time 10 --noproxy '*' "$@" "${ADMIN_URL}${path}"
}

# wait_for DESCRIPTION COMMAND... - polls COMMAND until it succeeds or the
# readiness timeout passes.
wait_for() {
  local description="$1" deadline=$((SECONDS + READY_TIMEOUT))
  shift
  while ((SECONDS < deadline)); do
    if "$@" >/dev/null 2>&1; then
      log "ready: ${description}"
      return 0
    fi
    sleep 2
  done
  log "not ready after ${READY_TIMEOUT}s: ${description}"
  compose ps >&2 || true
  compose logs --no-color --tail 200 >&2 || true
  die "the harness did not become ready"
}

palpo_admin_port_ready() {
  admin_curl /_matrix/client/versions --fail
}

edge_ready() {
  edge_curl "${SERVER_NAME}" /_matrix/client/versions --fail
}

synapse_ready() {
  edge_curl "${SYNAPSE_HOST}" /_matrix/client/versions --fail
}

synapse_admin_port_ready() {
  curl --silent --show-error --max-time 10 --noproxy '*' --fail \
    "${SYNAPSE_ADMIN_URL}/_matrix/client/versions"
}

# --- Admin bootstrap -----------------------------------------------------------

# post_json URL BODY [CURL ARGS...] - prints the response body, then a line
# with the HTTP status.
post_json() {
  local url="$1" body="$2"
  shift 2
  curl --silent --show-error --max-time 30 --noproxy '*' \
    --header 'Content-Type: application/json' --data "${body}" \
    --write-out '\n%{http_code}' "$@" "${url}"
}

# The admin bot, exactly as family-chat's PalpoType.bootstrap_admin makes
# it: token-gated registration, the users.is_admin flip in Postgres, then a
# password login whose access token the tests use.
bootstrap_admin() {
  local password="$1" body response status payload session output token
  body="$(jq -nc --arg user "${ADMIN_LOCALPART}" --arg password "${password}" \
    '{username: $user, password: $password, initial_device_display_name: "family-chat control panel", inhibit_login: true}')"
  response="$(post_json "${ADMIN_URL}/_matrix/client/v3/register" "${body}")"
  status="${response##*$'\n'}"
  payload="${response%$'\n'*}"
  if [[ "${status}" == 401 ]]; then
    session="$(jq -r '.session // empty' <<<"${payload}")"
    [[ -n "${session}" ]] || die "registration answered 401 without a UIA session: ${payload}"
    body="$(jq -c --arg token "${password}" --arg session "${session}" \
      '. + {auth: {type: "m.login.registration_token", token: $token, session: $session}}' <<<"${body}")"
    response="$(post_json "${ADMIN_URL}/_matrix/client/v3/register" "${body}")"
    status="${response##*$'\n'}"
    payload="${response%$'\n'*}"
  fi
  if [[ "${status}" != 200 ]] && [[ "$(jq -r '.errcode // empty' <<<"${payload}")" != M_USER_IN_USE ]]; then
    die "admin bot registration failed (${status}): ${payload}"
  fi

  # The mxid is built from constants, so it carries no SQL metacharacters.
  compose exec -T postgres psql -U palpo -d palpo -tAc \
    "UPDATE users SET is_admin = true WHERE id = '${ADMIN_MXID}'" >/dev/null \
    || die "could not grant ${ADMIN_MXID} server admin"
  output="$(compose exec -T postgres psql -U palpo -d palpo -tAc \
    "SELECT is_admin FROM users WHERE id = '${ADMIN_MXID}'")" || output=""
  [[ "${output}" == t ]] || die "${ADMIN_MXID} is not a server admin after the update: '${output}'"

  body="$(jq -nc --arg user "${ADMIN_LOCALPART}" --arg password "${password}" \
    '{type: "m.login.password", identifier: {type: "m.id.user", user: $user}, password: $password, initial_device_display_name: "family-chat control panel"}')"
  response="$(post_json "${ADMIN_URL}/_matrix/client/v3/login" "${body}")"
  status="${response##*$'\n'}"
  payload="${response%$'\n'*}"
  token="$(jq -r '.access_token // empty' <<<"${payload}")"
  [[ "${status}" == 200 && -n "${token}" ]] || die "admin bot login failed (${status}): ${payload}"

  # Prove the flip took: an admin-only route answers.
  response="$(admin_curl "/_synapse/admin/v1/users/$(urlencode "${ADMIN_MXID}")/joined_rooms" \
    --header "Authorization: Bearer ${token}" --write-out '\n%{http_code}')"
  status="${response##*$'\n'}"
  [[ "${status}" == 200 ]] || die "the admin API refused the admin bot (${status}): ${response%$'\n'*}"
  printf '%s' "${token}"
}

# A Synapse server admin, made with the image's register_new_matrix_user
# and the shared secret `generate` wrote, then signed in for its token.
bootstrap_synapse_admin() {
  local password="$1" body response status payload token
  compose exec -T synapse register_new_matrix_user --config /data/homeserver.yaml \
    --user "${SYNAPSE_ADMIN_LOCALPART}" --password "${password}" --admin http://localhost:8008 >/dev/null \
    || die "could not create the Synapse admin"
  body="$(jq -nc --arg user "${SYNAPSE_ADMIN_LOCALPART}" --arg password "${password}" \
    '{type: "m.login.password", identifier: {type: "m.id.user", user: $user}, password: $password, initial_device_display_name: "homeserver harness"}')"
  response="$(post_json "${SYNAPSE_ADMIN_URL}/_matrix/client/v3/login" "${body}")"
  status="${response##*$'\n'}"
  payload="${response%$'\n'*}"
  token="$(jq -r '.access_token // empty' <<<"${payload}")"
  [[ "${status}" == 200 && -n "${token}" ]] || die "Synapse admin login failed (${status}): ${payload}"
  printf '%s' "${token}"
}

urlencode() {
  jq -rn --arg value "$1" '$value | @uri'
}

write_env() {
  local admin_token="$1" registration_token="$2" synapse_admin_token="$3"
  (
    umask 077
    cat > "${ENV_FILE}" <<EOF
export FAMILYCHAT_HARNESS_EDGE_ADDR='${EDGE_ADDR}'
export FAMILYCHAT_HARNESS_CA_PEM='${CA_PEM}'
export FAMILYCHAT_HARNESS_SERVER_NAME='${SERVER_NAME}'
export FAMILYCHAT_HARNESS_HS1_URL='https://${SERVER_NAME}'
export FAMILYCHAT_HARNESS_CUSTOM_DOMAIN='${CUSTOM_DOMAIN}'
export FAMILYCHAT_HARNESS_EVIL_URL='https://${EVIL_HOST}'
export FAMILYCHAT_HARNESS_REDIRECT_URL='https://${REDIRECT_HOST}'
export FAMILYCHAT_HARNESS_ACCESS_LOG='${ACCESS_LOG}'
export FAMILYCHAT_HARNESS_SYNAPSE_URL='https://${SYNAPSE_HOST}'
export FAMILYCHAT_HARNESS_ADMIN_URL='${ADMIN_URL}'
export FAMILYCHAT_HARNESS_ADMIN_USER='${ADMIN_MXID}'
export FAMILYCHAT_HARNESS_ADMIN_TOKEN='${admin_token}'
export FAMILYCHAT_HARNESS_REGISTRATION_TOKEN='${registration_token}'
export FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_URL='${SYNAPSE_ADMIN_URL}'
export FAMILYCHAT_HARNESS_SYNAPSE_ADMIN_TOKEN='${synapse_admin_token}'
EOF
  )
}

# --- Commands ------------------------------------------------------------------

up() {
  require docker curl jq openssl
  docker info >/dev/null 2>&1 || die "the Docker daemon is not reachable"
  pull_images
  down
  mkdir -p "${STATE_DIR}" "${EDGE_LOG_DIR}"
  # nginx in the container writes the access log; the tests read it.
  chmod 0777 "${EDGE_LOG_DIR}"
  generate_tls
  local registration_token admin_token synapse_admin_token
  registration_token="$(openssl rand -hex 32)"
  render_palpo_config "${registration_token}"
  generate_synapse_config
  compose up --detach --remove-orphans \
    || { compose logs --no-color --tail 200 >&2 || true; die "could not start the harness"; }
  wait_for "Palpo on ${ADMIN_URL}" palpo_admin_port_ready
  wait_for "https://${SERVER_NAME} through the edge on ${EDGE_ADDR}" edge_ready
  wait_for "https://${SYNAPSE_HOST} through the edge" synapse_ready
  wait_for "Synapse on ${SYNAPSE_ADMIN_URL}" synapse_admin_port_ready
  admin_token="$(bootstrap_admin "${registration_token}")"
  synapse_admin_token="$(bootstrap_synapse_admin "$(openssl rand -hex 32)")"
  write_env "${admin_token}" "${registration_token}" "${synapse_admin_token}"
  log "up: ${HOSTS[*]} on ${EDGE_ADDR} (CA ${CA_PEM}); admin API ${ADMIN_URL} as ${ADMIN_MXID}; Synapse admin API ${SYNAPSE_ADMIN_URL}; settings in ${ENV_FILE}"
}

down() {
  compose down --volumes --remove-orphans --timeout 5 >/dev/null 2>&1 || true
  rm -rf "${TLS_DIR}" "${CA_PEM}" "${ENV_FILE}" "${PALPO_TOML}"
  # The access log is the container's (root's on most Docker hosts), in a
  # directory the script made; remove what can be removed.
  rm -rf "${EDGE_LOG_DIR}" 2>/dev/null || true
}

load_env() {
  [[ -f "${ENV_FILE}" ]] || die "the harness is not up; run 'scripts/homeserver-harness.sh up'"
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
}

# The mxid for a localpart or a full Matrix ID on hs1.
mxid_for() {
  if [[ "$1" == @* ]]; then
    printf '%s' "$1"
  else
    printf '@%s:%s' "$1" "${SERVER_NAME}"
  fi
}

mint_token() {
  [[ $# -ge 1 ]] || die "usage: scripts/homeserver-harness.sh mint-token USER [TTL_MS]"
  require curl jq
  load_env
  local mxid ttl="${2:-300000}" response status payload
  mxid="$(mxid_for "$1")"
  [[ "${ttl}" =~ ^[0-9]+$ ]] || die "TTL_MS must be a number of milliseconds"
  response="$(post_json "${ADMIN_URL}/_synapse/admin/v1/users/$(urlencode "${mxid}")/login_token" \
    "{\"expires_in_ms\": ${ttl}}" --header "Authorization: Bearer ${FAMILYCHAT_HARNESS_ADMIN_TOKEN}")"
  status="${response##*$'\n'}"
  payload="${response%$'\n'*}"
  [[ "${status}" == 200 ]] || die "minting a login token for ${mxid} failed (${status}): ${payload}"
  jq -er '.login_token' <<<"${payload}"
}

create_user() {
  [[ $# -eq 2 ]] || die "usage: scripts/homeserver-harness.sh create-user LOCALPART PASSWORD"
  require curl jq
  load_env
  local mxid response status
  mxid="$(mxid_for "$1")"
  response="$(curl --silent --show-error --max-time 30 --noproxy '*' --request PUT \
    --header 'Content-Type: application/json' \
    --header "Authorization: Bearer ${FAMILYCHAT_HARNESS_ADMIN_TOKEN}" \
    --data "$(jq -nc --arg password "$2" '{password: $password}')" \
    --write-out '\n%{http_code}' "${ADMIN_URL}/_synapse/admin/v2/users/$(urlencode "${mxid}")")"
  status="${response##*$'\n'}"
  [[ "${status}" == 200 || "${status}" == 201 ]] || die "creating ${mxid} failed (${status}): ${response%$'\n'*}"
  echo "${mxid}"
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  reset) down && up ;;
  env) load_env && cat "${ENV_FILE}" ;;
  mint-token) shift && mint_token "$@" ;;
  create-user) shift && create_user "$@" ;;
  logs) compose logs --no-color --timestamps ;;
  ps) compose ps ;;
  *)
    echo "usage: scripts/homeserver-harness.sh [up|down|reset|env|mint-token USER [TTL_MS]|create-user LOCALPART PASSWORD|logs|ps]" >&2
    exit 1
    ;;
esac
