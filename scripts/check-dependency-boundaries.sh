#!/usr/bin/env bash
set -euo pipefail

# check-dependency-boundaries.sh - fails if a crate crosses a boundary ADR
# 0001 and PLAN.md section 5.1 draw:
#
#   - familychat-ui and familychat-tui never depend on matrix-sdk or any
#     matrix-sdk-* crate, directly or through another crate, and neither
#     lists the other frontend's toolkit (ratatui and crossterm for the
#     window, gpui and gpui_platform for the terminal UI);
#   - familychat-core depends on no matrix-sdk crate at all, so its public
#     API cannot expose an SDK type (ruma identifiers are allowed: they are
#     ruma's, not the SDK's).
#
# familychat-signin (ADR 0006) is a Matrix crate (it hands sign-ins to
# familychat-session), so the first rule keeps it out of both frontends:
# they drive sign-in through familychat-core's SigninApi only. The
# self-tests check the rule can see that.
#
# It reads the graphs cargo builds for each crate's library, on every target
# platform (normal and build dependencies; a frontend's dev-dependencies may
# reach familychat-session for the runtime test, and never ship). Then it
# self-tests: familychat-session's graph must show matrix-sdk and
# familychat-ui's must show gpui, so a change in cargo tree's output cannot
# make the check pass vacuously.
#
# Usage: scripts/check-dependency-boundaries.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}/.."

failed=0

fail() {
  echo "check-dependency-boundaries: $*" >&2
  failed=1
}

# tree PACKAGE [ARGS...] - the package names in PACKAGE's graph, one per line.
tree() {
  local package="$1"
  shift
  cargo tree --locked --target all --edges normal,build --prefix none \
    --format '{p}' --package "${package}" "$@" | awk '{print $1}' | sort -u
}

# direct PACKAGE - the names of PACKAGE's own dependencies, every kind.
direct() {
  cargo tree --locked --target all --edges all --prefix none --depth 1 \
    --format '{p}' --package "$1" | awk 'NR > 1 {print $1}' | sort -u
}

# matching PATTERN - the lines of stdin that are exactly a name PATTERN
# matches.
matching() {
  grep -E "^($1)$" || true
}

readonly SDK='matrix-sdk|matrix-sdk-.*'

for frontend in familychat-ui familychat-tui; do
  graph="$(tree "${frontend}")"
  grep -qx "${frontend}" <<<"${graph}" \
    || { fail "cargo tree printed no ${frontend}; the check cannot run"; continue; }
  found="$(matching "${SDK}" <<<"${graph}")"
  [[ -z "${found}" ]] || fail "${frontend} depends on the Matrix SDK: $(tr '\n' ' ' <<<"${found}")"
  found="$(direct "${frontend}" | matching "${SDK}")"
  [[ -z "${found}" ]] || fail "${frontend}'s manifest lists the Matrix SDK: $(tr '\n' ' ' <<<"${found}")"
done

found="$(direct familychat-ui | matching 'ratatui|crossterm|familychat-tui')"
[[ -z "${found}" ]] || fail "familychat-ui lists the terminal UI's toolkit: $(tr '\n' ' ' <<<"${found}")"
found="$(direct familychat-tui | matching 'gpui|gpui_platform|familychat-ui')"
[[ -z "${found}" ]] || fail "familychat-tui lists the window's toolkit: $(tr '\n' ' ' <<<"${found}")"
found="$(tree familychat-tui | matching 'gpui|gpui_platform')"
[[ -z "${found}" ]] || fail "familychat-tui depends on GPUI: $(tr '\n' ' ' <<<"${found}")"

core="$(tree familychat-core)"
grep -qx familychat-core <<<"${core}" || fail "cargo tree printed no familychat-core; the check cannot run"
found="$(matching "${SDK}" <<<"${core}")"
[[ -z "${found}" ]] || fail "familychat-core depends on the Matrix SDK, so its API could expose SDK types: $(tr '\n' ' ' <<<"${found}")"

# Self-tests.
tree familychat-session | grep -qx matrix-sdk \
  || fail "self-test failed: the check does not see matrix-sdk in familychat-session's graph"
tree familychat-signin | grep -qx matrix-sdk \
  || fail "self-test failed: the check does not see matrix-sdk in familychat-signin's graph"
tree familychat-ui | grep -qx gpui \
  || fail "self-test failed: the check does not see gpui in familychat-ui's graph"

if ((failed)); then
  exit 1
fi
echo "check-dependency-boundaries: the frontends and familychat-core are free of the Matrix SDK, and each frontend of the other's toolkit"
