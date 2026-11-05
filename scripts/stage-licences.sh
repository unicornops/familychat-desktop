#!/usr/bin/env bash
set -euo pipefail

# stage-licences.sh - Gathers what every Family Chat release artefact
# carries besides the program (ADR 0014): Family Chat's LICENSE,
# THIRD_PARTY.html (the crates' licences, from `cargo about generate`) and
# licenses/ with the notices of what the binary embeds: the Inter font's
# SIL OFL 1.1 (Linux builds), Lucide's ISC/MIT and the Apache-2.0 licence of
# the code ported from Zed (crates/familychat-ui/LICENSE-APACHE).
#
# Usage: scripts/stage-licences.sh OUTPUT_DIR
#
# Expects THIRD_PARTY.html at the repository root (the release workflow
# downloads it from its notices job; locally run
# `cargo about generate --locked --fail about.hbs -o THIRD_PARTY.html`).
# The archives, the .deb, the macOS bundle and the MSI all copy from
# OUTPUT_DIR, so the list lives here only;
# crates/familychat-app/tests/packaging.rs fails when a licence file in the
# repository is missing from it.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUTPUT_DIR="${1:?usage: scripts/stage-licences.sh OUTPUT_DIR}"

# SOURCE DESTINATION (relative to OUTPUT_DIR), one per line.
LICENCES=(
  "LICENSE LICENSE"
  "THIRD_PARTY.html THIRD_PARTY.html"
  "assets/fonts/inter/LICENSE.txt licenses/Inter-OFL.txt"
  "assets/icons/LICENSE.txt licenses/Lucide-LICENSE.txt"
  "crates/familychat-ui/LICENSE-APACHE licenses/GPUI-Apache-2.0.txt"
)

if [[ ! -f "${REPO_ROOT}/THIRD_PARTY.html" ]]; then
  echo "stage-licences: no THIRD_PARTY.html at the repository root; run cargo about generate first" >&2
  exit 1
fi

rm -rf "${OUTPUT_DIR}"
mkdir -p "${OUTPUT_DIR}/licenses"
for entry in "${LICENCES[@]}"; do
  read -r source destination <<<"${entry}"
  cp "${REPO_ROOT}/${source}" "${OUTPUT_DIR}/${destination}"
done
echo "stage-licences: staged ${#LICENCES[@]} files in ${OUTPUT_DIR}"
