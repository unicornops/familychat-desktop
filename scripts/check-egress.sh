#!/usr/bin/env bash
# The egress check (PLAN.md section 8, ADR 0004 "Egress"): fails when the
# live tests reached, or tried to reach, any host other than the homeserver
# harness's.
#
#   scripts/check-egress.sh PROXY_LOG [ACCESS_LOG] [RESOLVER_LOG]
#       PROXY_LOG     scripts/egress-proxy.py's log (ALLOW/DENY lines)
#       ACCESS_LOG    the harness front end's access log
#                     (`host method path status`), by default
#                     target/homeserver-harness/edge-logs/access.log
#       RESOLVER_LOG  the harness resolver's refusals (`REFUSED name`,
#                     FAMILYCHAT_HARNESS_EGRESS_LOG); a missing file is an
#                     empty one
#   scripts/check-egress.sh --self-test
#       Proves the check fails the build on an unexpected host: runs the
#       proxy, sends it a harness host and example.org, and expects the
#       check to fail naming example.org (and to pass without it).
#
# The check fails on any DENY line, any host in any of the logs that is not
# a harness homeserver, and a proxy log with no ALLOW line at all (the tests
# did not go through the proxy, so it proves nothing).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# The hosts the harness front end serves (scripts/homeserver-harness.sh,
# HOSTS); nothing else is a harness homeserver.
HARNESS_HOSTS=(
  hs1.familychat.test
  smith.test
  evil.familychat.example
  redirect.familychat.test
  synapse.test
)

usage() {
  echo "usage: scripts/check-egress.sh PROXY_LOG [ACCESS_LOG] [RESOLVER_LOG]" >&2
  echo "       scripts/check-egress.sh --self-test" >&2
  exit 2
}

is_harness_host() {
  local host="$1" known
  for known in "${HARNESS_HOSTS[@]}"; do
    [[ "${host}" == "${known}" ]] && return 0
  done
  return 1
}

# check PROXY_LOG ACCESS_LOG RESOLVER_LOG: prints every unexpected host and
# returns 1 if there is one.
check() {
  local proxy_log="$1" access_log="$2" resolver_log="$3"
  local verdict host rest allowed=0
  local -a unexpected=()

  if [[ ! -f "${proxy_log}" ]]; then
    echo "egress check: no proxy log at ${proxy_log}" >&2
    return 1
  fi
  while read -r verdict host rest; do
    [[ -z "${verdict}" ]] && continue
    case "${verdict}" in
      ALLOW)
        if is_harness_host "${host%:*}"; then
          allowed=$((allowed + 1))
        else
          unexpected+=("proxy ALLOW ${host}")
        fi
        ;;
      DENY) unexpected+=("proxy DENY ${host} ${rest}") ;;
      *) unexpected+=("proxy log line: ${verdict} ${host} ${rest}") ;;
    esac
  done < "${proxy_log}"

  if [[ -f "${access_log}" ]]; then
    while read -r host rest; do
      [[ -z "${host}" ]] && continue
      host="${host%:*}"
      is_harness_host "${host,,}" || unexpected+=("edge ${host} ${rest}")
    done < "${access_log}"
  else
    echo "egress check: no edge access log at ${access_log} (skipped)" >&2
  fi

  if [[ -f "${resolver_log}" ]]; then
    while read -r verdict host rest; do
      [[ -z "${verdict}" ]] && continue
      unexpected+=("resolver ${verdict} ${host}")
    done < "${resolver_log}"
  fi

  if [[ "${#unexpected[@]}" -gt 0 ]]; then
    echo "egress check FAILED: the live tests reached or tried to reach hosts other than the harness homeservers (${HARNESS_HOSTS[*]}):" >&2
    printf '  %s\n' "${unexpected[@]}" | sort | uniq -c >&2
    return 1
  fi
  if [[ "${allowed}" -eq 0 ]]; then
    echo "egress check FAILED: the proxy log has no ALLOW line, so the live tests did not go through the recording proxy" >&2
    return 1
  fi
  echo "egress check passed: ${allowed} connection(s), all to harness homeservers"
}

# The self-test's processes and scratch directory, for its cleanup.
SELF_TEST_WORK=""
EDGE_PID=""
PROXY_PID=""

self_test_cleanup() {
  local pid
  for pid in "${PROXY_PID}" "${EDGE_PID}"; do
    [[ -n "${pid}" ]] && kill "${pid}" 2>/dev/null
  done
  [[ -n "${SELF_TEST_WORK}" ]] && rm -rf "${SELF_TEST_WORK}"
  return 0
}

self_test() {
  local work port status
  work="$(mktemp -d)"
  SELF_TEST_WORK="${work}"
  trap self_test_cleanup EXIT

  # A stand-in for the front end: accepts and closes, which is all a
  # CONNECT needs to be tunnelled.
  python3 - "${work}/edge.port" <<'EOF' &
import socket, sys
server = socket.socket()
server.bind(("127.0.0.1", 0))
server.listen()
open(sys.argv[1], "w").write(f"{server.getsockname()[1]}\n")
while True:
    connection, _ = server.accept()
    connection.close()
EOF
  EDGE_PID=$!
  for _ in $(seq 1 50); do
    [[ -s "${work}/edge.port" ]] && break
    sleep 0.1
  done

  local -a allow=()
  local host
  for host in "${HARNESS_HOSTS[@]}"; do
    allow+=(--allow "${host}")
  done
  python3 "${SCRIPT_DIR}/egress-proxy.py" --listen 127.0.0.1:0 \
    --edge "127.0.0.1:$(cat "${work}/edge.port")" --log "${work}/proxy.log" \
    --port-file "${work}/proxy.port" "${allow[@]}" >"${work}/proxy.out" 2>&1 &
  PROXY_PID=$!
  for _ in $(seq 1 50); do
    [[ -s "${work}/proxy.port" ]] && break
    sleep 0.1
  done
  port="$(cat "${work}/proxy.port")"

  # One harness host (tunnelled, then fails TLS against the stand-in) and
  # one unexpected host (refused and recorded).
  curl -sS -o /dev/null --max-time 5 -x "http://127.0.0.1:${port}" \
    https://hs1.familychat.test/_matrix/client/versions 2>/dev/null || true
  curl -sS -o /dev/null --max-time 5 -x "http://127.0.0.1:${port}" \
    https://example.org/ 2>/dev/null || true
  kill "${PROXY_PID}" 2>/dev/null || true
  wait "${PROXY_PID}" 2>/dev/null || true
  PROXY_PID=""

  echo "--- the proxy recorded:"
  cat "${work}/proxy.log"
  grep -qx 'ALLOW hs1.familychat.test:443' "${work}/proxy.log" \
    || { echo "self-test FAILED: the harness host was not tunnelled" >&2; return 1; }
  grep -qx 'DENY example.org:443' "${work}/proxy.log" \
    || { echo "self-test FAILED: example.org was not refused and recorded" >&2; return 1; }

  echo "--- the check over that log must fail:"
  : > "${work}/access.log"
  set +e
  check "${work}/proxy.log" "${work}/access.log" "${work}/none.log" 2>"${work}/check.err"
  status=$?
  set -e
  cat "${work}/check.err"
  if [[ "${status}" -eq 0 ]] || ! grep -q 'example.org' "${work}/check.err"; then
    echo "self-test FAILED: the check passed with example.org in the log" >&2
    return 1
  fi

  echo "--- an unexpected host in the edge or resolver log must fail it too:"
  grep -x 'ALLOW hs1.familychat.test:443' "${work}/proxy.log" > "${work}/clean.log"
  echo "matrix.org GET /_matrix/client/versions 200" > "${work}/access-bad.log"
  if check "${work}/clean.log" "${work}/access-bad.log" "${work}/none.log" 2>/dev/null; then
    echo "self-test FAILED: the check passed with matrix.org in the access log" >&2
    return 1
  fi
  echo "REFUSED vector.im" > "${work}/resolver-bad.log"
  if check "${work}/clean.log" "${work}/access.log" "${work}/resolver-bad.log" 2>/dev/null; then
    echo "self-test FAILED: the check passed with a refused name" >&2
    return 1
  fi
  : > "${work}/empty.log"
  if check "${work}/empty.log" "${work}/access.log" "${work}/none.log" 2>/dev/null; then
    echo "self-test FAILED: the check passed with no traffic through the proxy" >&2
    return 1
  fi

  echo "--- and pass with only harness hosts:"
  echo "hs1.familychat.test GET /_matrix/client/versions 200" > "${work}/access.log"
  check "${work}/clean.log" "${work}/access.log" "${work}/none.log"
  echo "self-test passed: the egress check fails the build on an unexpected host"
}

case "${1:-}" in
  --self-test) self_test ;;
  "" | -h | --help) usage ;;
  *)
    [[ $# -le 3 ]] || usage
    check "$1" \
      "${2:-${REPO_ROOT}/target/homeserver-harness/edge-logs/access.log}" \
      "${3:-/nonexistent}"
    ;;
esac
