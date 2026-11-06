#!/usr/bin/env bash
# The expected texts hold Markdown backticks, not commands.
# shellcheck disable=SC2016
set -euo pipefail

# test-release-signing-notes.sh - Runs scripts/release-signing-notes.sh over
# signing files written here and checks the section it renders: signed,
# ad hoc and unsigned files, the warning that names every file that isn't
# signed, and the malformed inputs it must refuse. No network needed.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTES="${SCRIPT_DIR}/release-signing-notes.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

# run NAME FILE... - runs the script; leaves its stdout in $WORK/NAME.md,
# its stderr in $WORK/NAME.err and the exit status in $status.
run() {
  local name="$1"
  shift
  status=0
  GITHUB_REPOSITORY=unicornops/familychat-desktop \
    "${NOTES}" "$@" > "${WORK}/${name}.md" 2> "${WORK}/${name}.err" || status=$?
}

expect_status() {
  [[ "${status}" == "$2" ]] || fail "$1: exit status ${status}, expected $2; stderr: $(cat "${WORK}/$1.err")"
}

expect_in() {
  grep -qF -- "$2" "${WORK}/$1.md" || fail "$1: the section should contain: $2
$(cat "${WORK}/$1.md")"
}

expect_not_in() {
  if grep -qF -- "$2" "${WORK}/$1.md"; then
    fail "$1: the section should not contain: $2"
  fi
}

cat > "${WORK}/signing-Linux.txt" <<'TXT'
# Linux packages are not signed during the alpha.
familychat-linux-x86_64.tar.gz|unsigned|Linux packages are not signed during the alpha
familychat-linux-x86_64.deb|unsigned|Linux packages are not signed during the alpha
TXT
cat > "${WORK}/signing-macOS.txt" <<'TXT'
familychat-macos.dmg|signed|Developer ID, notarised and stapled

familychat-macos.tar.gz|signed|Developer ID and notarised (checked online)
TXT
cat > "${WORK}/signing-Windows.txt" <<'TXT'
familychat-windows-x86_64.msi|unsigned|Azure Artifact Signing is not set up yet
familychat-windows-x86_64.zip|unsigned|Azure Artifact Signing is not set up yet
TXT
cat > "${WORK}/signing-macOS-adhoc.txt" <<'TXT'
familychat-macos.dmg|adhoc|no Developer ID certificate was configured
TXT
cat > "${WORK}/signing-all-signed.txt" <<'TXT'
familychat-macos.dmg|signed|Developer ID, notarised and stapled
familychat-windows-x86_64.msi|signed|Azure Artifact Signing
TXT

# A release as it is today: macOS signed, Linux and Windows not.
run today "${WORK}/signing-Linux.txt" "${WORK}/signing-macOS.txt" "${WORK}/signing-Windows.txt"
expect_status today 0
expect_in today "<!-- familychat-signing -->"
expect_in today '| `familychat-macos.dmg` | Signed: Developer ID, notarised and stapled |'
expect_in today '| `familychat-linux-x86_64.deb` | **Not signed**: Linux packages are not signed during the alpha |'
expect_in today '| `familychat-windows-x86_64.msi` | **Not signed**: Azure Artifact Signing is not set up yet |'
expect_in today 'not signed** with a certificate your system trusts: `familychat-linux-x86_64.tar.gz`, `familychat-linux-x86_64.deb`, `familychat-windows-x86_64.msi`, `familychat-windows-x86_64.zip`.'
expect_not_in today 'familychat-macos.dmg`, '
expect_not_in today '# Linux packages'
expect_in today 'gh attestation verify FILE --repo unicornops/familychat-desktop'
expect_in today 'sha256sum -c SHA256SUMS --ignore-missing'
[[ "$(grep -c '^| `' "${WORK}/today.md")" == 6 ]] || fail "today: expected six file rows"

# Ad hoc counts as not signed.
run adhoc "${WORK}/signing-macOS-adhoc.txt"
expect_status adhoc 0
expect_in adhoc '| `familychat-macos.dmg` | **Not signed** (ad hoc signature only): no Developer ID certificate was configured |'
expect_in adhoc 'trusts: `familychat-macos.dmg`.'

# Everything signed: no warning.
run all-signed "${WORK}/signing-all-signed.txt"
expect_status all-signed 0
expect_in all-signed "Every file is signed."
expect_not_in all-signed "not signed"

# Malformed input fails rather than claim a signature.
printf 'familychat-macos.dmg|notarised|Developer ID\n' > "${WORK}/bad-state.txt"
run bad-state "${WORK}/bad-state.txt"
expect_status bad-state 1
printf 'familychat-macos.dmg|signed\n' > "${WORK}/no-detail.txt"
run no-detail "${WORK}/no-detail.txt"
expect_status no-detail 1
printf 'familychat-macos.dmg|signed|a|b\n' > "${WORK}/extra-field.txt"
run extra-field "${WORK}/extra-field.txt"
expect_status extra-field 1
printf '# nothing\n\n' > "${WORK}/empty.txt"
run empty "${WORK}/empty.txt"
expect_status empty 1
run missing "${WORK}/does-not-exist.txt"
expect_status missing 1
run no-arguments
expect_status no-arguments 1

if [[ "${failures}" -gt 0 ]]; then
  echo "test-release-signing-notes: ${failures} failure(s)" >&2
  exit 1
fi
echo "test-release-signing-notes: all checks passed"
