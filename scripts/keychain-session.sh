#!/usr/bin/env bash
set -euo pipefail

# keychain-session.sh - Runs a command against a throwaway Secret Service.
#
# Usage: scripts/keychain-session.sh COMMAND [ARGS...]
#
# Starts a private D-Bus session with gnome-keyring whose keyrings live in
# a new temporary directory, with an unencrypted (always unlocked) default
# collection, and runs COMMAND in it. The real keychain-tests
# (`cargo test -p familychat-security -- --ignored real_keychain`) and
# `familychat accounts` then reach a real Secret Service on a headless
# Linux machine or CI runner without ever seeing the user's own keyrings:
# the XDG directories, which gnome-keyring and D-Bus activation read, all
# point into the temporary directory. HOME is left alone so cargo and
# rustup still find their toolchains.
#
# Needs dbus-run-session (the `dbus` package) and gnome-keyring. The
# temporary directories are left in place for inspection.

if [[ $# -eq 0 ]]; then
  echo "usage: keychain-session.sh COMMAND [ARGS...]" >&2
  exit 2
fi

base="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/familychat-keychain.XXXXXX")"
# The runtime directory holds gnome-keyring's control socket, and Unix
# socket paths are limited to 108 bytes, so it gets a short name.
runtime="$(mktemp -d /tmp/fckr.XXXXXX)"
chmod 700 "$runtime"
mkdir -p "$base/data/keyrings" "$base/cache" "$base/config"
printf '[keyring]\ndisplay-name=test\nctime=0\nmtime=0\nlock-on-idle=false\nlock-after=false\n' \
  > "$base/data/keyrings/test.keyring"
printf 'test' > "$base/data/keyrings/default"

export XDG_DATA_HOME="$base/data"
export XDG_CACHE_HOME="$base/cache"
export XDG_CONFIG_HOME="$base/config"
export XDG_RUNTIME_DIR="$runtime"

exec dbus-run-session -- bash -c '
  gnome-keyring-daemon --start --components=secrets > /dev/null
  exec "$@"
' keychain-session "$@"
