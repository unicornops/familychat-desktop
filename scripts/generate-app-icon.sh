#!/usr/bin/env bash
set -euo pipefail

# generate-app-icon.sh - Renders Family Chat's app icon
# (packaging/icons/familychat.svg) for each platform's package. Adapted from
# Zmail's scripts/generate-app-icon.sh (Copyright UnicornOps, GPL-3.0-only),
# which mirrors the Shuffleboard icon pipeline; the Linux and Windows modes
# are this project's.
#
#   scripts/generate-app-icon.sh macos OUTPUT.icns [OUTPUT-Assets.car]
#       The macOS icns (sips resizes the 1024 px render into the Apple
#       iconset sizes and iconutil assembles it) and, with a second
#       argument, a compiled asset catalog with an "AppIcon" set (actool),
#       which the bundle names with CFBundleIconName: recent macOS takes the
#       app icon from there, notifications included, and keeps the icns as
#       the fallback. macOS only.
#
#   scripts/generate-app-icon.sh linux OUTPUT_DIR
#       The freedesktop hicolor theme under OUTPUT_DIR:
#       hicolor/<N>x<N>/apps/family.safechat.desktop.png for the sizes
#       desktops look for, and hicolor/scalable/apps/family.safechat.desktop.svg.
#       The .deb installs them under /usr/share/icons.
#
#   scripts/generate-app-icon.sh windows OUTPUT.ico
#       A Windows icon with 16 to 256 px images (ImageMagick). The result
#       is committed as packaging/windows/familychat.ico, which build.rs
#       embeds in the executables and the MSI shows; run this after
#       changing the SVG and commit the new file.
#
# The SVG is rasterised with rsvg-convert (librsvg: `brew install librsvg`,
# `apt-get install librsvg2-bin`); on macOS, qlmanage is the fallback.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SOURCE_SVG="${REPO_ROOT}/packaging/icons/familychat.svg"
# The app id (familychat_core::identity::APP_ID) names the Linux icon.
ICON_NAME="family.safechat.desktop"

usage() {
  echo "usage: scripts/generate-app-icon.sh macos OUTPUT.icns [OUTPUT-Assets.car]" >&2
  echo "       scripts/generate-app-icon.sh linux OUTPUT_DIR" >&2
  echo "       scripts/generate-app-icon.sh windows OUTPUT.ico" >&2
  exit 1
}

MODE="${1:-}"
OUTPUT="${2:-}"
[[ -n "${MODE}" && -n "${OUTPUT}" ]] || usage

if [[ ! -f "${SOURCE_SVG}" ]]; then
  echo "Error: icon source not found at ${SOURCE_SVG}" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

# render SIZE OUTPUT.png - the SVG at SIZE x SIZE pixels.
render() {
  local size="$1" output="$2"
  if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w "${size}" -h "${size}" "${SOURCE_SVG}" -o "${output}"
    return
  fi
  if [[ "$(uname -s)" == Darwin ]]; then
    # qlmanage names its thumbnail <file>.svg.png in the output directory.
    local ql_dir="${WORK_DIR}/ql-${size}"
    mkdir -p "${ql_dir}"
    qlmanage -t -s "${size}" -o "${ql_dir}" "${SOURCE_SVG}" >/dev/null 2>&1 || true
    local candidate
    for candidate in "${ql_dir}"/*.png; do
      if [[ -f "${candidate}" ]]; then
        mv "${candidate}" "${output}"
        return
      fi
    done
  fi
  echo "Error: could not rasterise ${SOURCE_SVG}; install librsvg (rsvg-convert)" >&2
  exit 1
}

macos_icon() {
  local icns="$1" assets_car="${2:-}"
  local iconset="${WORK_DIR}/AppIcon.iconset"
  mkdir -p "${iconset}" "$(dirname "${icns}")"

  local source_png="${WORK_DIR}/icon-1024.png"
  render 1024 "${source_png}"
  if ! sips -g pixelWidth "${source_png}" 2>/dev/null | grep -q 1024; then
    sips -z 1024 1024 "${source_png}" --out "${source_png}" >/dev/null
  fi

  # Apple iconset sizes (pt x scale = px): 16, 32, 128, 256, 512 at 1x and 2x.
  local size double
  for size in 16 32 128 256 512; do
    sips -z "${size}" "${size}" "${source_png}" --out "${iconset}/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "${double}" "${double}" "${source_png}" --out "${iconset}/icon_${size}x${size}@2x.png" >/dev/null
  done

  iconutil -c icns -o "${icns}" "${iconset}"
  echo "Built ${icns}"

  [[ -n "${assets_car}" ]] || return 0

  # The asset catalog: the iconset's PNGs as AppIcon.appiconset.
  local catalog="${WORK_DIR}/Assets.xcassets"
  local appiconset="${catalog}/AppIcon.appiconset"
  mkdir -p "${appiconset}"
  cp "${iconset}"/*.png "${appiconset}/"
  cat > "${catalog}/Contents.json" <<'JSON'
{
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON
  {
    echo '{'
    echo '  "images" : ['
    local first=1 scale file
    for size in 16 32 128 256 512; do
      for scale in 1 2; do
        if [[ "${scale}" == 1 ]]; then file="icon_${size}x${size}.png"; else file="icon_${size}x${size}@2x.png"; fi
        [[ "${first}" == 1 ]] || echo ','
        first=0
        printf '    { "filename" : "%s", "idiom" : "mac", "scale" : "%sx", "size" : "%sx%s" }' \
          "${file}" "${scale}" "${size}" "${size}"
      done
    done
    echo
    echo '  ],'
    echo '  "info" : { "author" : "xcode", "version" : 1 }'
    echo '}'
  } > "${appiconset}/Contents.json"

  local compiled="${WORK_DIR}/compiled"
  mkdir -p "${compiled}"
  xcrun actool "${catalog}" \
    --compile "${compiled}" \
    --platform macosx \
    --minimum-deployment-target 12.0 \
    --app-icon AppIcon \
    --output-partial-info-plist "${WORK_DIR}/partial-info.plist" \
    --output-format human-readable-text \
    --notices --warnings --errors

  if [[ ! -f "${compiled}/Assets.car" ]]; then
    echo "Error: actool did not produce Assets.car" >&2
    exit 1
  fi
  mkdir -p "$(dirname "${assets_car}")"
  cp "${compiled}/Assets.car" "${assets_car}"
  echo "Built ${assets_car}"
}

linux_icons() {
  local output_dir="$1"
  local size dir
  # The sizes GNOME, KDE and the freedesktop icon theme spec look for.
  for size in 16 24 32 48 64 128 256 512; do
    dir="${output_dir}/hicolor/${size}x${size}/apps"
    mkdir -p "${dir}"
    render "${size}" "${dir}/${ICON_NAME}.png"
  done
  dir="${output_dir}/hicolor/scalable/apps"
  mkdir -p "${dir}"
  cp "${SOURCE_SVG}" "${dir}/${ICON_NAME}.svg"
  echo "Built the hicolor icons in ${output_dir}"
}

windows_icon() {
  local ico="$1"
  local size pngs=()
  for size in 16 24 32 48 64 128 256; do
    render "${size}" "${WORK_DIR}/icon-${size}.png"
    pngs+=("${WORK_DIR}/icon-${size}.png")
  done
  mkdir -p "$(dirname "${ico}")"
  if command -v magick >/dev/null 2>&1; then
    magick "${pngs[@]}" "${ico}"
  else
    convert "${pngs[@]}" "${ico}"
  fi
  echo "Built ${ico}"
}

case "${MODE}" in
  macos) macos_icon "${OUTPUT}" "${3:-}" ;;
  linux) linux_icons "${OUTPUT}" ;;
  windows) windows_icon "${OUTPUT}" ;;
  *) usage ;;
esac
