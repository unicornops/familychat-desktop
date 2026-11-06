#!/usr/bin/env bash
set -euo pipefail

# create-macos-bundle.sh - Assembles "Family Chat.app" (ADR 0014). Adapted
# from the "Create macOS app bundle" step of Zmail's release workflow
# (Copyright UnicornOps, GPL-3.0-only).
#
# Usage: scripts/create-macos-bundle.sh OUTPUT.app BINARY VERSION ICONS_DIR LICENCES_DIR
#
#   OUTPUT.app    the bundle to create (replaced if it exists)
#   BINARY        the release build of familychat
#   VERSION       CFBundleShortVersionString and CFBundleVersion
#   ICONS_DIR     FamilyChat.icns and Assets.car, from
#                 `scripts/generate-app-icon.sh macos`
#   LICENCES_DIR  scripts/stage-licences.sh's output, copied into
#                 Contents/Resources so the licences travel with the app
#
# The Info.plist is packaging/macos/Info.plist with the version filled in:
# the bundle id UNUserNotificationCenter needs and the familychat URL
# scheme. Signing is the caller's (the release workflow signs the bundle
# with Developer ID and the hardened runtime, or ad hoc). macOS only
# (plutil, assetutil).

OUTPUT="${1:?usage: create-macos-bundle.sh OUTPUT.app BINARY VERSION ICONS_DIR LICENCES_DIR}"
BINARY="${2:?usage: create-macos-bundle.sh OUTPUT.app BINARY VERSION ICONS_DIR LICENCES_DIR}"
VERSION="${3:?usage: create-macos-bundle.sh OUTPUT.app BINARY VERSION ICONS_DIR LICENCES_DIR}"
ICONS_DIR="${4:?usage: create-macos-bundle.sh OUTPUT.app BINARY VERSION ICONS_DIR LICENCES_DIR}"
LICENCES_DIR="${5:?usage: create-macos-bundle.sh OUTPUT.app BINARY VERSION ICONS_DIR LICENCES_DIR}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PLIST_TEMPLATE="${REPO_ROOT}/packaging/macos/Info.plist"

if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "create-macos-bundle: the version must be X.Y.Z, not ${VERSION}" >&2
  exit 1
fi
for file in "${BINARY}" "${ICONS_DIR}/FamilyChat.icns" "${ICONS_DIR}/Assets.car" "${LICENCES_DIR}/LICENSE" "${LICENCES_DIR}/THIRD_PARTY.html"; do
  if [[ ! -f "${file}" ]]; then
    echo "create-macos-bundle: missing ${file}" >&2
    exit 1
  fi
done

rm -rf "${OUTPUT}"
mkdir -p "${OUTPUT}/Contents/MacOS" "${OUTPUT}/Contents/Resources"
cp "${BINARY}" "${OUTPUT}/Contents/MacOS/familychat"
chmod 755 "${OUTPUT}/Contents/MacOS/familychat"
cp "${ICONS_DIR}/FamilyChat.icns" "${ICONS_DIR}/Assets.car" "${OUTPUT}/Contents/Resources/"
cp -R "${LICENCES_DIR}/." "${OUTPUT}/Contents/Resources/"
sed "s/@VERSION@/${VERSION}/g" "${PLIST_TEMPLATE}" > "${OUTPUT}/Contents/Info.plist"
printf 'APPL????' > "${OUTPUT}/Contents/PkgInfo"

plutil -lint "${OUTPUT}/Contents/Info.plist"
if grep -q '@VERSION@' "${OUTPUT}/Contents/Info.plist"; then
  echo "create-macos-bundle: Info.plist still has a placeholder" >&2
  exit 1
fi
# The icon must resolve both ways: CFBundleIconName through the compiled
# asset catalog (what recent macOS, notifications included, uses) and
# CFBundleIconFile through the icns.
assets_info="$(assetutil --info "${OUTPUT}/Contents/Resources/Assets.car")"
if ! grep -Eq '"Name" *: *"AppIcon"' <<<"${assets_info}"; then
  echo "${assets_info}" >&2
  echo "create-macos-bundle: Assets.car has no AppIcon" >&2
  exit 1
fi
echo "create-macos-bundle: built ${OUTPUT} (Family Chat ${VERSION})"
