# ADR 0002: Pin GPUI To Zed Stable Release Tags

## Status

Accepted on November 5, 2026, with Phase 0 (issue #1). Carried over from
Zmail's ADR 0003 (unicornops/zmail), which made the same decision in
October 2026.

## Context

Family Chat Desktop's window is built on GPUI. The crates.io `gpui` crate
stopped at `0.2.2` (October 22, 2025, Zed commit `69e2130`) and lacks what
this app needs from day one and later:

- AccessKit screen-reader support (Zed #56065), on by default through
  `gpui_platform::application()`;
- system notifications (Zed #61189), which Phase 9 posts new messages
  with;
- the wgpu renderer on Linux and Windows, and the platform split into
  `gpui_macos`, `gpui_windows`, `gpui_linux` and `gpui_wgpu`.

The options are the same as Zmail had: (a) a git dependency on Zed,
pinned; (b) the stale crates.io release; (c) the third-party `gpui-pre`
crate, weekly snapshots republished outside Zed and therefore a
supply-chain concern.

## Decision

Family Chat depends on GPUI (`gpui` and `gpui_platform`) through a git
dependency on the Zed repository pinned to a **Zed stable release tag**,
`v1.27.0` (commit `c84af5b`, released November 4, 2026) at the time of
writing, not an arbitrary `main` commit and never a pre-release:

- stable tags are what Zed ships to its own users and are followed by
  patch releases when something breaks;
- the Rust toolchain follows the one Zed pins at that tag (`1.98.1`), in
  `rust-toolchain.toml` and `rust-version`, and CI reads it from there;
- the `[patch.crates-io]` entries that change GPUI's runtime behaviour are
  mirrored from Zed's root manifest (`async-task`, `calloop`,
  `khronos-egl`), because `[patch]` only applies to the workspace that
  declares it;
- a weekly workflow (`.github/workflows/zed-upstream.yml`,
  `scripts/bump-zed-gpui.sh`) opens a single
  `chore(deps): bump Zed GPUI to vX.Y.Z` pull request when a newer stable
  release appears, moving the toolchain, the patches and `Cargo.lock`
  with it;
- `deny.toml` allows exactly the git repositories the pinned graph uses;
- Dependabot ignores `gpui` and `gpui_platform`.

Zmail currently deviates from its ADR 0003 by pinning a Zed pull request
(#62379, native window composition) for its HTML mail web renderer. Family
Chat draws message HTML natively and has no web view (PLAN.md, section
5.8), so it does not need that deviation and the bump script's pull
request logic was left out.

The mechanics, the current pin and how to bump by hand are in
[`docs/upstream/zed.md`](../upstream/zed.md).

## Consequences

Phase 0 needed no workarounds for `v1.27.0`. The API Zmail's ADR 0003
describes still applies: `gpui_platform::application()` to start,
`cx.background_executor().timer(..)` for timers, `Entity::update` on an
async context without a `Result`. Zed also ships `gpui_tokio`; it is not
used, for the reasons in ADR 0001.

The six "unmaintained" advisories the pinned graph triggers (`paste`,
`ttf-parser`, `rustybuzz`, and `zvariant`, `zvariant_derive` and
`zbus_names` under the Linux accessibility stack) are the same ones Zmail
carries. They are ignored in `deny.toml` with their reasons, and
`cargo audit` reports them as warnings only. None is a vulnerability.

Costs of the Phase 0 binary (the GPUI window, the terminal UI and the
Tokio runtime; no matrix-sdk yet), measured on the same Linux host as
Zmail's figures (x86-64, 12 cores, warm Cargo caches):

| | Phase 0, Zed v1.27.0 |
| --- | --- |
| Clean release build of `familychat-app` | 103 s |
| Release binary (`target/release/familychat`) | 39.8 MB (37.9 MiB) |
| Stripped binary | 29.6 MB |
| gzip-compressed binary | 12.7 MB |

Zmail's release binary on Zed `v1.24.2` was 60.1 MB with its whole mail
stack; most of Family Chat's 39.8 MB is GPUI with wgpu (Vulkan and GL
backends with the naga shader compiler), AccessKit and the text stack.
CI's build job records the release binary size on each platform in its
job summary, so growth from matrix-sdk (Phase 3) onwards shows up per pull
request (PLAN.md, section 12). The first fetch of the Zed repository takes
about 400 MB of Cargo git database, which CI caches.

### Binary size from Phase 5

Phase 5 is the first build whose binary links matrix-sdk (earlier phases
depended on it, but nothing called it, so the linker dropped it). Clean
release builds of `familychat-app` on the same host (x86-64, 12 cores,
warm Cargo caches, each in an empty target directory):

| Release profile | Clean build | Binary |
| --- | --- | --- |
| Cargo's defaults (no LTO, 16 codegen units, symbols kept) | 329 s | 165.0 MB (157.3 MiB) |
| The same, stripped afterwards | 329 s | 117.2 MB |
| **`lto = "thin"`, `codegen-units = 1`, `strip = "symbols"` (chosen)** | **448 s** | **93.4 MB (89.1 MiB)** |
| `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"` | 824 s | 84.4 MB (80.5 MiB) |

The workspace's `[profile.release]` is the thin-LTO row: 43% smaller than
the default for 36% more build time. Fat LTO saves a further 10% for
nearly twice the build time again, which every release, the build job's
release binary on each OS and the UI Screenshots job would pay (CI
runners have a third of this host's cores). The symbol table alone was
48 MB; release builds carry no debug information (`debug = 0`), so there
is nothing for split debug info to keep, and the crash log (ADR 0003)
writes the panic message and location, not a backtrace, so stripping
loses nothing it shows. If symbolicated crash reports are wanted later,
Phase 12 can ship separate symbol files. The panic strategy stays
`unwind`. `opt-level` stays 3: the window's rendering and the SDK's
crypto are the hot paths.

On Linux the window renders through wgpu on Vulkan (lavapipe in headless
runs) with an EGL/GL fallback, so it needs `libvulkan1` or `libegl1` at
runtime.
