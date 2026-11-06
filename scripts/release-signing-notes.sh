#!/usr/bin/env bash
set -euo pipefail

# release-signing-notes.sh - Renders the "Downloads and signing" section the
# release workflow adds to a release's notes (ADR 0014), so every release
# says which of its files are signed and which are not.
#
# Usage: scripts/release-signing-notes.sh SIGNING_FILE... > section.md
#
# Each SIGNING_FILE (one per build job: signing-Linux.txt,
# signing-macOS.txt, signing-Windows.txt) has one line per release file:
#
#   FILE|STATE|DETAIL
#
# STATE is `signed` (a signature the OS checks: Developer ID, notarised;
# Authenticode through Azure Artifact Signing), `adhoc` (macOS's ad hoc
# signature, which Gatekeeper does not trust) or `unsigned`. DETAIL says
# what was done, in a few words. Blank lines and lines starting with # are
# skipped; anything else malformed fails the script, so a release never
# claims a signature it lacks. The section starts with the marker
# <!-- familychat-signing --> so the workflow adds it once.

MARKER="<!-- familychat-signing -->"
REPOSITORY="${GITHUB_REPOSITORY:-unicornops/familychat-desktop}"

if [[ $# -eq 0 ]]; then
  echo "usage: scripts/release-signing-notes.sh SIGNING_FILE..." >&2
  exit 1
fi

rows=()
not_trusted=()
for signing_file in "$@"; do
  if [[ ! -f "${signing_file}" ]]; then
    echo "release-signing-notes: no ${signing_file}" >&2
    exit 1
  fi
  while IFS= read -r line || [[ -n "${line}" ]]; do
    [[ -z "${line}" || "${line}" == \#* ]] && continue
    IFS='|' read -r file state detail extra <<<"${line}"
    if [[ -z "${file}" || -z "${detail}" || -n "${extra:-}" || "${file}" == *[[:space:]]* ]]; then
      echo "release-signing-notes: malformed line in ${signing_file}: ${line}" >&2
      exit 1
    fi
    case "${state}" in
      signed) label="Signed" ;;
      adhoc)
        label="**Not signed** (ad hoc signature only)"
        not_trusted+=("${file}")
        ;;
      unsigned)
        label="**Not signed**"
        not_trusted+=("${file}")
        ;;
      *)
        echo "release-signing-notes: unknown state '${state}' in ${signing_file}: ${line}" >&2
        exit 1
        ;;
    esac
    rows+=("| \`${file}\` | ${label}: ${detail} |")
  done < "${signing_file}"
done

if [[ ${#rows[@]} -eq 0 ]]; then
  echo "release-signing-notes: the signing files list no release files" >&2
  exit 1
fi

echo "${MARKER}"
echo "### Downloads and signing"
echo
echo "| File | Signature |"
echo "| --- | --- |"
printf '%s\n' "${rows[@]}"
echo
if [[ ${#not_trusted[@]} -gt 0 ]]; then
  list=""
  for file in "${not_trusted[@]}"; do
    list="${list:+${list}, }\`${file}\`"
  done
  echo "These files are **not signed** with a certificate your system trusts: ${list}. Your system may warn before opening them; check them against \`SHA256SUMS\` and their attestation below first. See [docs/release.md](https://github.com/${REPOSITORY}/blob/main/docs/release.md) for what each warning looks like."
else
  echo "Every file is signed."
fi
echo
echo "Every file is listed in \`SHA256SUMS\` (\`sha256sum -c SHA256SUMS --ignore-missing\`) and has a build provenance attestation from this repository's release workflow: \`gh attestation verify FILE --repo ${REPOSITORY}\`."
