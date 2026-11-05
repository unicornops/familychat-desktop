#!/usr/bin/env bash
set -euo pipefail

# window-smoke.sh - Opens the Family Chat window and checks it appears.
#
# Usage: scripts/window-smoke.sh BINARY OUTPUT_DIR
#
# Starts BINARY with no arguments (the GUI), waits for a window titled
# "Family Chat" owned by it, checks the process is still running a few
# seconds later, saves a screenshot and the app's output to OUTPUT_DIR,
# and quits it. Linux runs it under Xvfb (xvfb-run) and needs xdotool and
# ImageMagick's `import`; macOS needs a logged-in session, which GitHub's
# runners have, and uses `screencapture` and Swift for the window list.
# Windows has scripts/window-smoke.ps1.

BINARY="${1:?usage: window-smoke.sh BINARY OUTPUT_DIR}"
OUTPUT_DIR="${2:?usage: window-smoke.sh BINARY OUTPUT_DIR}"
TITLE="Family Chat"
WAIT_SECONDS="${FAMILYCHAT_SMOKE_WAIT:-60}"

mkdir -p "$OUTPUT_DIR"
log="${OUTPUT_DIR}/app.log"

"$BINARY" > "$log" 2>&1 &
pid=$!
cleanup() {
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
trap cleanup EXIT

# Prints the titles of the app's windows, one per line. On Linux that is
# every named window on the display, which under xvfb-run is the app's
# alone.
window_titles() {
  case "$(uname -s)" in
    Linux)
      local id
      for id in $(xdotool search --name '.' 2>/dev/null); do
        xdotool getwindowname "$id" 2>/dev/null || true
      done
      ;;
    Darwin)
      swift "$titles_swift" "$pid" 2>/dev/null || true
      ;;
  esac
}

if [[ "$(uname -s)" == "Darwin" ]]; then
  titles_swift="$(mktemp -d)/window-titles.swift"
  cat > "$titles_swift" <<'SWIFT'
import CoreGraphics

// The on-screen windows of the process whose id is the first argument.
let pid = Int(CommandLine.arguments[1]) ?? -1
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows where (window[kCGWindowOwnerPID as String] as? Int) == pid {
    // Window names need the screen recording permission; without it the
    // window is still listed, under its owner's name.
    let name = window[kCGWindowName as String] as? String
    print(name ?? (window[kCGWindowOwnerName as String] as? String ?? ""))
}
SWIFT
fi

found=""
for _ in $(seq 1 "$WAIT_SECONDS"); do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "::error::the app exited before its window appeared"
    cat "$log"
    exit 1
  fi
  titles="$(window_titles)"
  if grep -Fxq "$TITLE" <<<"$titles" || { [[ "$(uname -s)" == "Darwin" ]] && grep -Fxq "familychat" <<<"$titles"; }; then
    found="yes"
    break
  fi
  sleep 1
done

if [[ -z "$found" ]]; then
  echo "::error::no \"${TITLE}\" window appeared within ${WAIT_SECONDS} seconds"
  echo "Windows owned by the app:"
  window_titles || true
  cat "$log"
  exit 1
fi
echo "The \"${TITLE}\" window is open:"
window_titles | sed 's/^/  /'

# Let it draw a few frames, then check it is still running.
sleep 3
if ! kill -0 "$pid" 2>/dev/null; then
  echo "::error::the app exited after opening its window"
  cat "$log"
  exit 1
fi

case "$(uname -s)" in
  Linux) import -window root "${OUTPUT_DIR}/window.png" ;;
  Darwin) screencapture -x "${OUTPUT_DIR}/window.png" || echo "::warning::screencapture failed" ;;
esac
echo "Screenshot: ${OUTPUT_DIR}/window.png"
echo "App output:"
cat "$log"
