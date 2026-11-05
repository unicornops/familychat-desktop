#!/usr/bin/env bash
set -euo pipefail

# check-release-features.sh - fails if a release build of familychat-app
# could include familychat-test-support or any `test-harness` feature, which
# trusts the homeserver harness's throwaway CA (ADR 0004, PLAN.md section 7).
#
# It reads the dependency graph `cargo build -p familychat-app --release`
# builds, with its default features, for every target platform (normal and
# build dependencies; dev-dependencies never reach the binary), and then
# self-tests: the same check over familychat-test-support with the feature
# on must fire, so a change in cargo tree's output cannot make it pass
# vacuously.
#
# Usage: scripts/check-release-features.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}/.."

# tree ARGS... - the package graph with each package's enabled features.
tree() {
  cargo tree --locked --target all --edges normal,build --prefix none \
    --format '{p} [{f}]' "$@"
}

# offending - prints the lines of stdin that name the test-support crate or
# a test-harness feature.
offending() {
  grep -E '^familychat-test-support |[[,]test-harness[],]' || true
}

release_tree="$(tree --package familychat-app)"
if ! grep -q '^familychat-app ' <<<"${release_tree}"; then
  echo "check-release-features: cargo tree printed no familychat-app; the check cannot run" >&2
  exit 1
fi
found="$(offending <<<"${release_tree}")"
if [[ -n "${found}" ]]; then
  echo "check-release-features: a release build of familychat-app includes the homeserver harness:" >&2
  echo "${found}" >&2
  exit 1
fi

self_test="$(tree --package familychat-test-support --features test-harness | offending)"
if [[ -z "${self_test}" ]]; then
  echo "check-release-features: self-test failed: the check does not see familychat-test-support's test-harness feature" >&2
  exit 1
fi

echo "check-release-features: familychat-app's release graph ($(wc -l <<<"${release_tree}") packages, every target) has no familychat-test-support and no test-harness feature"
