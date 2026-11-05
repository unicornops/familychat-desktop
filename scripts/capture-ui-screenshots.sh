#!/usr/bin/env bash
# Captures the UI screenshot matrix of Family Chat under an X server
# (normally Xvfb). Adapted from Zmail's scripts/capture-release-screenshot.sh
# (its --matrix mode).
#
#   scripts/capture-ui-screenshots.sh --matrix OUTPUT_DIR [STATE...]
#       One PNG per UI state, named after the state, with no network,
#       keychain or profile. Each state launches
#       `familychat launch-ui-fixture VIEW THEME`, which opens the window on
#       familychat-test-support's fixtures in that view. Pass state names to
#       capture only those; `--list` prints them.
#
# It expects a running X server in DISPLAY with room for the window, for
# example:
#
#   cargo build -p familychat-app --release --locked --features ui-fixture
#   xvfb-run -a --server-args="-screen 0 1920x1080x24" \
#     scripts/capture-ui-screenshots.sh --matrix artifacts/ui-screenshots
#
# Requires openbox, xdotool and ImageMagick's `import`; GPUI renders through
# wgpu on Vulkan, so headless runs need Mesa's lavapipe driver
# (mesa-vulkan-drivers). FAMILYCHAT_APP_BINARY overrides the binary
# (default target/release/familychat, built with the `ui-fixture` feature)
# and FAMILYCHAT_CAPTURE_LOG_DIR the directory for the app and openbox logs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_BINARY="${FAMILYCHAT_APP_BINARY:-${REPO_ROOT}/target/release/familychat}"
WINDOW_TITLE="Family Chat"
LOG_DIR="${FAMILYCHAT_CAPTURE_LOG_DIR:-${TMPDIR:-/tmp}}"
APP_LOG="${LOG_DIR}/familychat-ui-screenshots-app.log"
OPENBOX_LOG="${LOG_DIR}/familychat-ui-screenshots-openbox.log"
APP_PID=""
OPENBOX_PID=""

# The matrix: NAME VIEW THEME WIDTH HEIGHT. 1040x760 is the sign-in
# screens' size and 1180x780 the rooms' (the window's default); a few
# states are also captured at the minimum width (560), where the rooms show
# the list or the room, and wide (1600), where the member list sits beside
# the timeline, to show the layout holds. Every fixture view is captured in dark and
# light (crates/familychat-app/src/ui_fixture.rs checks this list against
# its views). The settings-* views are captured from the Settings window,
# which opens over the main window (window_title_for); 860 wide is its
# default width, and two are captured at the narrowest it gets.
MATRIX=(
  "welcome-dark welcome dark 1040 760"
  "welcome-light welcome light 1040 760"
  "command-palette-dark command-palette dark 1040 760"
  "command-palette-light command-palette light 1040 760"
  "code-dark code dark 1040 760"
  "code-light code light 1040 760"
  "code-not-a-link-dark code-not-a-link dark 1040 760"
  "code-not-a-link-light code-not-a-link light 1040 760"
  "code-no-qr-dark code-no-qr dark 1040 760"
  "code-no-qr-light code-no-qr light 1040 760"
  "confirm-dark confirm dark 1040 760"
  "confirm-light confirm light 1040 760"
  "redeeming-dark redeeming dark 1040 760"
  "redeeming-light redeeming light 1040 760"
  "code-used-or-expired-dark code-used-or-expired dark 1040 760"
  "code-used-or-expired-light code-used-or-expired light 1040 760"
  "code-not-used-dark code-not-used dark 1040 760"
  "code-not-used-light code-not-used light 1040 760"
  "already-signed-in-dark already-signed-in dark 1040 760"
  "already-signed-in-light already-signed-in light 1040 760"
  "password-dark password dark 1040 760"
  "password-light password light 1040 760"
  "password-wrong-dark password-wrong dark 1040 760"
  "password-wrong-light password-wrong light 1040 760"
  "password-rate-limited-dark password-rate-limited dark 1040 760"
  "password-rate-limited-light password-rate-limited light 1040 760"
  "password-not-family-chat-dark password-not-family-chat dark 1040 760"
  "password-not-family-chat-light password-not-family-chat light 1040 760"
  "checking-server-dark checking-server dark 1040 760"
  "checking-server-light checking-server light 1040 760"
  "other-server-dark other-server dark 1040 760"
  "other-server-light other-server light 1040 760"
  "keychain-locked-dark keychain-locked dark 1040 760"
  "keychain-locked-light keychain-locked light 1040 760"
  "keychain-unavailable-dark keychain-unavailable dark 1040 760"
  "keychain-unavailable-light keychain-unavailable light 1040 760"
  "lost-key-dark lost-key dark 1040 760"
  "lost-key-light lost-key light 1040 760"
  "signed-in-dark signed-in dark 1180 780"
  "signed-in-light signed-in light 1180 780"
  "signed-in-connecting-dark signed-in-connecting dark 1180 780"
  "signed-in-connecting-light signed-in-connecting light 1180 780"
  "signed-in-offline-dark signed-in-offline dark 1180 780"
  "signed-in-offline-light signed-in-offline light 1180 780"
  "signed-in-suspended-dark signed-in-suspended dark 1180 780"
  "signed-in-suspended-light signed-in-suspended light 1180 780"
  "signed-in-signed-out-dark signed-in-signed-out dark 1180 780"
  "signed-in-signed-out-light signed-in-signed-out light 1180 780"
  "signed-in-not-supported-dark signed-in-not-supported dark 1180 780"
  "signed-in-not-supported-light signed-in-not-supported light 1180 780"
  "welcome-dark-560 welcome dark 560 720"
  "code-light-560 code light 560 720"
  "password-dark-560 password dark 560 720"
  "rooms-unread-dark rooms-unread dark 1180 780"
  "rooms-unread-light rooms-unread light 1180 780"
  "rooms-people-dark rooms-people dark 1180 780"
  "rooms-people-light rooms-people light 1180 780"
  "rooms-invites-dark rooms-invites dark 1180 780"
  "rooms-invites-light rooms-invites light 1180 780"
  "rooms-filter-empty-dark rooms-filter-empty dark 1180 780"
  "rooms-filter-empty-light rooms-filter-empty light 1180 780"
  "rooms-empty-dark rooms-empty dark 1180 780"
  "rooms-empty-light rooms-empty light 1180 780"
  "rooms-loading-dark rooms-loading dark 1180 780"
  "rooms-loading-light rooms-loading light 1180 780"
  "room-dark room dark 1180 780"
  "room-light room light 1180 780"
  "room-dm-dark room-dm dark 1180 780"
  "room-dm-light room-dm light 1180 780"
  "room-rich-dark room-rich dark 1180 780"
  "room-rich-light room-rich light 1180 780"
  "room-media-dark room-media dark 1180 780"
  "room-media-light room-media light 1180 780"
  "room-changes-dark room-changes dark 1180 780"
  "room-changes-light room-changes light 1180 780"
  "room-changes-expanded-dark room-changes-expanded dark 1180 780"
  "room-changes-expanded-light room-changes-expanded light 1180 780"
  "room-unreadable-dark room-unreadable dark 1180 780"
  "room-unreadable-light room-unreadable light 1180 780"
  "room-not-encrypted-dark room-not-encrypted dark 1180 780"
  "room-not-encrypted-light room-not-encrypted light 1180 780"
  "room-empty-dark room-empty dark 1180 780"
  "room-empty-light room-empty light 1180 780"
  "room-members-dark room-members dark 1180 780"
  "room-members-light room-members light 1180 780"
  "room-invite-dark room-invite dark 1180 780"
  "room-invite-light room-invite light 1180 780"
  "link-prompt-dark link-prompt dark 1180 780"
  "link-prompt-light link-prompt light 1180 780"
  "link-prompt-mismatched-dark link-prompt-mismatched dark 1180 780"
  "link-prompt-mismatched-light link-prompt-mismatched light 1180 780"
  "room-large-dark room-large dark 1180 780"
  "room-large-light room-large light 1180 780"
  "forwarded-link-dark forwarded-link dark 1180 780"
  "forwarded-link-light forwarded-link light 1180 780"
  "signed-in-dark-560 signed-in dark 560 720"
  "room-dark-560 room dark 560 720"
  "room-media-dark-560 room-media dark 560 720"
  "room-members-dark-1600 room-members dark 1600 900"
  "room-rich-dark-1600 room-rich dark 1600 900"
  "signed-in-light-560 signed-in light 560 720"
  "room-light-560 room light 560 720"
  "room-media-light-560 room-media light 560 720"
  "room-members-light-1600 room-members light 1600 900"
  "room-rich-light-1600 room-rich light 1600 900"
  "composer-text-dark composer-text dark 1180 780"
  "composer-text-light composer-text light 1180 780"
  "composer-reply-dark composer-reply dark 1180 780"
  "composer-reply-light composer-reply light 1180 780"
  "composer-edit-dark composer-edit dark 1180 780"
  "composer-edit-light composer-edit light 1180 780"
  "composer-mentions-dark composer-mentions dark 1180 780"
  "composer-mentions-light composer-mentions light 1180 780"
  "composer-attachment-dark composer-attachment dark 1180 780"
  "composer-attachment-light composer-attachment light 1180 780"
  "composer-too-large-dark composer-too-large dark 1180 780"
  "composer-too-large-light composer-too-large light 1180 780"
  "room-paused-dark room-paused dark 1180 780"
  "room-paused-light room-paused light 1180 780"
  "message-actions-dark message-actions dark 1180 780"
  "message-actions-light message-actions light 1180 780"
  "message-delete-dark message-delete dark 1180 780"
  "message-delete-light message-delete light 1180 780"
  "composer-text-dark-560 composer-text dark 560 720"
  "composer-reply-light-560 composer-reply light 560 720"
  "room-paused-dark-560 room-paused dark 560 720"
  "message-actions-light-560 message-actions light 560 720"
  "composer-mentions-dark-1600 composer-mentions dark 1600 900"
  "composer-attachment-light-1600 composer-attachment light 1600 900"
  "message-actions-dark-1600 message-actions dark 1600 900"
  "confirm-identity-dark confirm-identity dark 1180 780"
  "confirm-identity-light confirm-identity light 1180 780"
  "confirm-recovery-key-dark confirm-recovery-key dark 1180 780"
  "confirm-recovery-key-light confirm-recovery-key light 1180 780"
  "confirm-skip-dark confirm-skip dark 1180 780"
  "confirm-skip-light confirm-skip light 1180 780"
  "confirm-skipped-dark confirm-skipped dark 1180 780"
  "confirm-skipped-light confirm-skipped light 1180 780"
  "confirm-password-dark confirm-password dark 1180 780"
  "confirm-password-light confirm-password light 1180 780"
  "verify-waiting-dark verify-waiting dark 1180 780"
  "verify-waiting-light verify-waiting light 1180 780"
  "verify-qr-dark verify-qr dark 1180 780"
  "verify-qr-light verify-qr light 1180 780"
  "verify-emoji-dark verify-emoji dark 1180 780"
  "verify-emoji-light verify-emoji light 1180 780"
  "verify-done-dark verify-done dark 1180 780"
  "verify-done-light verify-done light 1180 780"
  "verify-incoming-dark verify-incoming dark 1180 780"
  "verify-incoming-light verify-incoming light 1180 780"
  "recovery-offer-dark recovery-offer dark 1180 780"
  "recovery-offer-light recovery-offer light 1180 780"
  "recovery-setup-dark recovery-setup dark 1180 780"
  "recovery-setup-light recovery-setup light 1180 780"
  "recovery-key-dark recovery-key dark 1180 780"
  "recovery-key-light recovery-key light 1180 780"
  "devices-dark devices dark 1180 780"
  "devices-light devices light 1180 780"
  "device-sign-out-dark device-sign-out dark 1180 780"
  "device-sign-out-light device-sign-out light 1180 780"
  "keys-export-dark keys-export dark 1180 780"
  "keys-export-light keys-export light 1180 780"
  "keys-import-dark keys-import dark 1180 780"
  "keys-import-light keys-import light 1180 780"
  "room-identity-changed-dark room-identity-changed dark 1180 780"
  "room-identity-changed-light room-identity-changed light 1180 780"
  "room-not-sent-dark room-not-sent dark 1180 780"
  "room-not-sent-light room-not-sent light 1180 780"
  "confirm-identity-dark-560 confirm-identity dark 560 720"
  "confirm-password-light-560 confirm-password light 560 720"
  "verify-emoji-light-560 verify-emoji light 560 720"
  "verify-qr-dark-560 verify-qr dark 560 720"
  "devices-dark-560 devices dark 560 720"
  "room-identity-changed-light-560 room-identity-changed light 560 720"
  "room-identity-changed-dark-1600 room-identity-changed dark 1600 900"
  "recovery-offer-light-1600 recovery-offer light 1600 900"
  "room-not-sent-review-dark room-not-sent-review dark 1180 780"
  "room-not-sent-review-light room-not-sent-review light 1180 780"
  "settings-account-dark settings-account dark 860 900"
  "settings-account-light settings-account light 860 900"
  "settings-account-other-dark settings-account-other dark 860 720"
  "settings-account-other-light settings-account-other light 860 720"
  "settings-panel-link-dark settings-panel-link dark 860 900"
  "settings-panel-link-light settings-panel-link light 860 900"
  "settings-sign-out-dark settings-sign-out dark 860 900"
  "settings-sign-out-light settings-sign-out light 860 900"
  "settings-sessions-dark settings-sessions dark 860 720"
  "settings-sessions-light settings-sessions light 860 720"
  "settings-privacy-dark settings-privacy dark 860 900"
  "settings-privacy-light settings-privacy light 860 900"
  "settings-reset-key-dark settings-reset-key dark 860 900"
  "settings-reset-key-light settings-reset-key light 860 900"
  "settings-reset-identity-dark settings-reset-identity dark 860 900"
  "settings-reset-identity-light settings-reset-identity light 860 900"
  "settings-reset-identity-password-dark settings-reset-identity-password dark 860 900"
  "settings-reset-identity-password-light settings-reset-identity-password light 860 900"
  "settings-appearance-dark settings-appearance dark 860 720"
  "settings-appearance-light settings-appearance light 860 720"
  "settings-notifications-dark settings-notifications dark 860 720"
  "settings-notifications-light settings-notifications light 860 720"
  "settings-notifications-hidden-dark settings-notifications-hidden dark 860 720"
  "settings-notifications-hidden-light settings-notifications-hidden light 860 720"
  "settings-notifications-off-dark settings-notifications-off dark 860 720"
  "settings-notifications-off-light settings-notifications-off light 860 720"
  "settings-diagnostics-dark settings-diagnostics dark 860 720"
  "settings-diagnostics-light settings-diagnostics light 860 720"
  "settings-appearance-large-dark settings-appearance-large dark 1280 900"
  "settings-appearance-large-light settings-appearance-large light 1280 900"
  "room-text-200-dark room-text-200 dark 1280 900"
  "room-text-200-light room-text-200 light 1280 900"
  "settings-account-dark-560 settings-account dark 560 720"
  "settings-privacy-light-560 settings-privacy light 560 720"
)

usage() {
  echo "usage: scripts/capture-ui-screenshots.sh --matrix OUTPUT_DIR [STATE...]" >&2
  echo "       scripts/capture-ui-screenshots.sh --list" >&2
  exit 1
}

stop_app() {
  if [[ -n "${APP_PID}" ]] && kill -0 "${APP_PID}" 2>/dev/null; then
    kill "${APP_PID}" 2>/dev/null || true
  fi
  if [[ -n "${APP_PID}" ]]; then
    wait "${APP_PID}" 2>/dev/null || true
  fi
  APP_PID=""
}

cleanup() {
  local exit_code=$?
  stop_app
  if [[ -n "${OPENBOX_PID}" ]] && kill -0 "${OPENBOX_PID}" 2>/dev/null; then
    kill "${OPENBOX_PID}" 2>/dev/null || true
  fi
  if [[ -n "${OPENBOX_PID}" ]]; then
    wait "${OPENBOX_PID}" 2>/dev/null || true
  fi
  exit "${exit_code}"
}

# The title of the window a fixture view is captured from: the Settings
# window for the settings views, the main window otherwise.
window_title_for() {
  case "$1" in
    settings-*) echo "Settings" ;;
    *) echo "${WINDOW_TITLE}" ;;
  esac
}

# Prints the id of the app's visible window titled $1 (default: the main
# window) once it is mapped, waiting up to 90 seconds; fails if the app
# exits first.
wait_for_window() {
  local title="${1:-${WINDOW_TITLE}}"
  local window_id=""
  for _ in $(seq 1 90); do
    if ! kill -0 "${APP_PID}" 2>/dev/null; then
      echo "familychat exited before its window appeared" >&2
      cat "${APP_LOG}" >&2
      return 1
    fi
    window_id="$(xdotool search --onlyvisible --name "^${title}\$" 2>/dev/null | head -n 1 || true)"
    if [[ -n "${window_id}" ]]; then
      echo "${window_id}"
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for the ${title} window" >&2
  cat "${APP_LOG}" >&2
  return 1
}

start_session() {
  mkdir -p "${LOG_DIR}"
  : > "${APP_LOG}"
  : > "${OPENBOX_LOG}"

  export NO_AT_BRIDGE=1
  export LIBGL_ALWAYS_SOFTWARE=1
  export XDG_SESSION_TYPE=x11
  unset WAYLAND_DISPLAY
  # The fixture never touches a profile, but keep any stray write away
  # from the real one.
  FAMILYCHAT_DATA_DIR="$(mktemp -d)"
  export FAMILYCHAT_DATA_DIR

  openbox >"${OPENBOX_LOG}" 2>&1 &
  OPENBOX_PID=$!
}

# Captures one matrix state into OUTPUT_DIR/NAME.png.
capture_state() {
  local output_dir="$1" name="$2" view="$3" theme="$4" width="$5" height="$6"
  local output_path="${output_dir}/${name}.png"

  echo "capturing ${name} (${view}, ${theme}, ${width}x${height})"
  echo "=== ${name}" >> "${APP_LOG}"
  "${APP_BINARY}" launch-ui-fixture "${view}" "${theme}" >>"${APP_LOG}" 2>&1 &
  APP_PID=$!

  local window_id
  window_id="$(wait_for_window "$(window_title_for "${view}")")"
  xdotool windowactivate "${window_id}" >/dev/null 2>&1 || true
  xdotool windowmove "${window_id}" 0 0 >/dev/null 2>&1 || true
  xdotool windowsize "${window_id}" "${width}" "${height}"
  # The pointer off the window, so no row shows its hover state.
  xdotool mousemove 1919 1079 >/dev/null 2>&1 || true

  # Wait for the window manager to apply the size, then for GPUI to draw
  # the new frame.
  local actual=""
  for _ in $(seq 1 20); do
    actual="$(xdotool getwindowgeometry "${window_id}" | sed -n 's/^ *Geometry: //p')"
    [[ "${actual}" == "${width}x${height}" ]] && break
    sleep 0.5
  done
  if [[ "${actual}" != "${width}x${height}" ]]; then
    echo "the ${name} window is ${actual}, not ${width}x${height}" >&2
    return 1
  fi
  sleep 3

  import -window "${window_id}" "${output_path}"
  stop_app
  # Let the window unmap so the next state cannot find this one.
  sleep 1
}

capture_matrix() {
  local output_dir="$1"
  shift
  mkdir -p "${output_dir}"
  start_session

  local entry name selected
  local captured=0
  for entry in "${MATRIX[@]}"; do
    read -r name _ <<< "${entry}"
    if [[ $# -gt 0 ]]; then
      selected=0
      for wanted in "$@"; do
        [[ "${wanted}" == "${name}" ]] && selected=1
      done
      [[ "${selected}" -eq 1 ]] || continue
    fi
    # shellcheck disable=SC2086 # Split the entry into its fields.
    capture_state "${output_dir}" ${entry}
    captured=$((captured + 1))
  done

  if [[ "${captured}" -eq 0 ]]; then
    echo "no matrix state matched: $*" >&2
    return 1
  fi
  echo "captured ${captured} screenshot(s) in ${output_dir}"
}

case "${1:-}" in
  --list)
    for entry in "${MATRIX[@]}"; do
      echo "${entry%% *}"
    done
    exit 0
    ;;
  --matrix)
    [[ $# -ge 2 ]] || usage
    ;;
  *)
    usage
    ;;
esac

trap cleanup EXIT

if [[ ! -x "${APP_BINARY}" ]]; then
  echo "expected a familychat binary built with --features ui-fixture at ${APP_BINARY}" >&2
  exit 1
fi
if ! "${APP_BINARY}" launch-ui-fixture --list >/dev/null 2>&1; then
  echo "${APP_BINARY} was built without the ui-fixture feature" >&2
  exit 1
fi

shift
capture_matrix "$@"
