#!/usr/bin/env bash
set -euo pipefail

# link-handler-smoke.sh - Checks that the installed Family Chat receives a
# familychat:// link opened through the operating system (ADR 0014), as a
# browser would hand it over: on Linux through the desktop entry the .deb
# installs (xdg-open), on macOS through the bundle's CFBundleURLTypes
# (open, Launch Services and an Apple Event). Windows has
# scripts/link-handler-smoke.ps1.
#
# Usage: scripts/link-handler-smoke.sh APP OUTPUT_DIR
#
#   APP         the installed executable: /usr/bin/familychat, or
#               "/Applications/Family Chat.app/Contents/MacOS/familychat"
#   OUTPUT_DIR  where the screenshot and the logs go
#
# It starts APP (the window) on a throwaway profile, waits until it
# listens for second launches, opens a sign-in link with a placeholder
# code through the OS, and checks:
#
#   - Linux: the OS started `familychat LINK`, which handed the link to
#     the window and exited (the window's log says a second launch was
#     served and delivered); macOS: the running app got the link from
#     Launch Services (its log says so) and no second copy started;
#   - the window is still running, and no log line or output holds the code.
#
# The screenshot (OUTPUT_DIR/link.png) shows the "Sign in as ...?"
# confirmation; nothing is redeemed without Continue, so no request is sent.
# Linux runs it under Xvfb (xvfb-run) inside scripts/keychain-session.sh and
# needs xdg-utils, xdotool and ImageMagick's `import`; macOS needs a
# logged-in session, which GitHub's runners have.

APP="${1:?usage: link-handler-smoke.sh APP OUTPUT_DIR}"
OUTPUT_DIR="${2:?usage: link-handler-smoke.sh APP OUTPUT_DIR}"
WAIT_SECONDS="${FAMILYCHAT_SMOKE_WAIT:-90}"
# A link of the shape the panel builds; the code is a placeholder that is
# never sent (the confirmation comes first).
PLACEHOLDER_CODE="fc-link-smoke-placeholder-code"
LINK="familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:%40alex%3Asmith.safechat.family&hs=smith.safechat.family&token=${PLACEHOLDER_CODE}"

mkdir -p "${OUTPUT_DIR}"
OUTPUT_DIR="$(cd "${OUTPUT_DIR}" && pwd)"
FAMILYCHAT_DATA_DIR="$(mktemp -d)"
export FAMILYCHAT_DATA_DIR
# The hand-off's outcome is logged at debug (never the link).
export FAMILYCHAT_LOG="warn,familychat=info,familychat_instance=debug,familychat_ui=info"
profile_log="${FAMILYCHAT_DATA_DIR}/logs/familychat.log"
app_output="${OUTPUT_DIR}/app-output.log"

"${APP}" > "${app_output}" 2>&1 &
pid=$!
cleanup() {
  kill "${pid}" 2>/dev/null || true
  wait "${pid}" 2>/dev/null || true
  if [[ -f "${profile_log}" ]]; then
    cp "${profile_log}" "${OUTPUT_DIR}/profile.log"
  fi
  rm -rf "${FAMILYCHAT_DATA_DIR}"
}
trap cleanup EXIT

fail() {
  echo "::error::$*"
  echo "--- app output"
  cat "${app_output}" || true
  echo "--- profile log"
  cat "${profile_log}" 2>/dev/null || true
  exit 1
}

# wait_for_log TEXT... - waits until the profile's log has a line holding
# every TEXT.
wait_for_log() {
  local _
  for _ in $(seq 1 "${WAIT_SECONDS}"); do
    kill -0 "${pid}" 2>/dev/null || fail "the app exited (waiting for: $*)"
    if [[ -f "${profile_log}" ]]; then
      local lines
      lines="$(cat "${profile_log}")"
      local text
      for text in "$@"; do
        lines="$(grep -F -- "${text}" <<<"${lines}" || true)"
      done
      [[ -n "${lines}" ]] && return 0
    fi
    sleep 1
  done
  fail "no log line with: $*"
}

# app_processes - how many familychat processes run (the window, and any
# second launch the OS started, whatever its path).
app_processes() {
  pgrep -x familychat | wc -l | tr -d ' '
}

wait_for_log "listening for second launches"
# Let the profile open, so the link lands on the sign-in screen.
sleep 5
echo "The window is listening (pid ${pid}); opening the link through the OS"

case "$(uname -s)" in
  Linux)
    echo "x-scheme-handler/familychat is handled by: $(xdg-mime query default x-scheme-handler/familychat)"
    xdg-open "${LINK}"
    wait_for_log "served a second launch" "Delivered"
    echo "The OS started familychat with the link, and it was handed to the window"
    ;;
  Darwin)
    open "${LINK}"
    wait_for_log "the OS handed the window" "sign-in link"
    echo "Launch Services handed the link to the running app"
    ;;
  *)
    fail "unsupported system $(uname -s)"
    ;;
esac

# The second launch (Linux) exits once it has handed the link over; macOS
# never starts one.
sleep 5
kill -0 "${pid}" 2>/dev/null || fail "the window exited after the link"
count="$(app_processes)"
[[ "${count}" == 1 ]] || fail "${count} Family Chat processes run, expected only the window"

case "$(uname -s)" in
  Linux) import -window root "${OUTPUT_DIR}/link.png" ;;
  Darwin) screencapture -x "${OUTPUT_DIR}/link.png" || echo "::warning::screencapture failed" ;;
esac
echo "Screenshot: ${OUTPUT_DIR}/link.png"

if grep -rqF -- "${PLACEHOLDER_CODE}" "${app_output}" "${FAMILYCHAT_DATA_DIR}"; then
  fail "the link's code reached a log or a file"
fi
echo "No log line or file holds the code"
