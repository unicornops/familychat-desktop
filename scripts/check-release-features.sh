#!/usr/bin/env bash
set -euo pipefail

# check-release-features.sh - fails if a release build of familychat-app
# could include familychat-test-support, any `test-harness` feature (which
# trusts the homeserver harness's throwaway CA, or allows its
# *.familychat.test homeservers) or any `staging` feature (which allows
# Family Chat staging's *.familychat.dev homeservers): ADR 0004, ADR 0006,
# PLAN.md sections 6.5 and 7.
#
# It reads the dependency graph `cargo build -p familychat-app --release`
# builds, with its default features, for every target platform (normal and
# build dependencies; dev-dependencies never reach the binary), and then
# self-tests: the same check over familychat-test-support with its feature
# on, and over familychat-signin with each of its two, must fire, and
# familychat-signin must be in the release graph, so a change in cargo
# tree's output cannot make it pass vacuously.
#
# Usage: scripts/check-release-features.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}/.."

# tree ARGS... - the package graph with each package's enabled features.
tree() {
  cargo tree --locked --target all --edges normal,build --prefix none \
    --format '{p} [{f}]' "$@"
}

# offending - prints the lines of stdin that name the test-support crate, a
# test-harness feature, or a staging feature of a Family Chat crate (other
# crates have `staging` features of their own, such as wayland-protocols'
# staging protocols, which are not ours to refuse).
offending() {
  grep -E '^familychat-test-support |[[,]test-harness[],]|^familychat-[a-z-]+ v[^ ]+( \([^)]*\))? \[([^]]*,)?staging[],]' || true
}

release_tree="$(tree --package familychat-app)"
if ! grep -q '^familychat-app ' <<<"${release_tree}"; then
  echo "check-release-features: cargo tree printed no familychat-app; the check cannot run" >&2
  exit 1
fi
found="$(offending <<<"${release_tree}")"
if [[ -n "${found}" ]]; then
  echo "check-release-features: a release build of familychat-app includes the homeserver harness or staging:" >&2
  echo "${found}" >&2
  exit 1
fi

if ! grep -q '^familychat-signin ' <<<"${release_tree}"; then
  echo "check-release-features: self-test failed: familychat-signin is not in familychat-app's release graph, so its features are not checked" >&2
  exit 1
fi

for self_test in "familychat-test-support test-harness" "familychat-signin test-harness" "familychat-signin staging"; do
  read -r package feature <<<"${self_test}"
  if [[ -z "$(tree --package "${package}" --features "${feature}" | grep "^${package} " | offending)" ]]; then
    echo "check-release-features: self-test failed: the check does not see ${package}'s ${feature} feature" >&2
    exit 1
  fi
done

echo "check-release-features: familychat-app's release graph ($(wc -l <<<"${release_tree}") packages, every target) has no familychat-test-support and no test-harness or staging feature"
