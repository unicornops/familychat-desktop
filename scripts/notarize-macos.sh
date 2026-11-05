#!/usr/bin/env bash
set -euo pipefail

# notarize-macos.sh - Submits a Developer ID-signed archive to Apple's
# notary service, waits for the verdict and staples the ticket (ADR 0014).
# Adapted from the "Notarize the macOS app" step of Zmail's release
# workflow (Copyright UnicornOps, GPL-3.0-only): submit with retries, poll
# the submission instead of `notarytool submit --wait` (which gives up on
# the first dropped request), print Apple's log when it is refused.
#
# Usage: scripts/notarize-macos.sh SUBMISSION [STAPLE_TARGET...]
#
#   SUBMISSION     a .zip (made with `ditto -c -k`) or a signed .dmg
#   STAPLE_TARGET  an .app or .dmg to staple the ticket to once Apple
#                  accepts it (a bare executable cannot be stapled: its
#                  ticket is checked online)
#
# Reads APPLE_ID (the Apple ID), APPLE_PASSWORD (its app-specific
# password) and TEAM_ID from the environment; none is ever printed.

SUBMISSION="${1:?usage: notarize-macos.sh SUBMISSION [STAPLE_TARGET...]}"
shift

: "${APPLE_ID:?APPLE_ID is not set}"
: "${APPLE_PASSWORD:?APPLE_PASSWORD is not set}"
: "${TEAM_ID:?TEAM_ID is not set}"

if [[ ! -f "${SUBMISSION}" ]]; then
  echo "notarize-macos: no ${SUBMISSION}" >&2
  exit 1
fi

AUTH=(--apple-id "${APPLE_ID}" --team-id "${TEAM_ID}" --password "${APPLE_PASSWORD}")
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

retry() {
  local attempt
  for attempt in 1 2 3; do
    "$@" && return 0
    echo "::warning::Notarization attempt ${attempt} failed: $1 $2"
    sleep $((attempt * 30))
  done
  return 1
}

submit() {
  xcrun notarytool submit "${SUBMISSION}" "${AUTH[@]}" --output-format json > "${WORK}/submission.json"
}

retry submit
SUBMISSION_ID="$(jq -r '.id' "${WORK}/submission.json")"
echo "Notarization submission for $(basename "${SUBMISSION}"): ${SUBMISSION_ID}"

STATUS=""
DEADLINE=$((SECONDS + 45 * 60))
while true; do
  if xcrun notarytool info "${SUBMISSION_ID}" "${AUTH[@]}" --output-format json > "${WORK}/result.json"; then
    STATUS="$(jq -r '.status' "${WORK}/result.json")"
    echo "Notarization status: ${STATUS}"
    [[ "${STATUS}" != "In Progress" ]] && break
  else
    echo "::warning::Couldn't fetch the notarization status; trying again"
  fi
  if [[ "${SECONDS}" -ge "${DEADLINE}" ]]; then
    echo "::error::Notarization still not finished after 45 minutes (submission ${SUBMISSION_ID})"
    exit 1
  fi
  sleep 30
done

if [[ "${STATUS}" != "Accepted" ]]; then
  xcrun notarytool log "${SUBMISSION_ID}" "${AUTH[@]}" || true
  echo "::error::Notarization finished with status ${STATUS}"
  exit 1
fi

for target in "$@"; do
  retry xcrun stapler staple "${target}"
  xcrun stapler validate "${target}"
done
echo "Notarized $(basename "${SUBMISSION}")${*:+ and stapled $*}"
