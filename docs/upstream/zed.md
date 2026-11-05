# Zed Upstream Record

Family Chat's GUI is built on GPUI, Zed's UI framework. GPUI is not
consumed from crates.io: the workspace pins it to a Zed commit through a
git dependency. [ADR 0002](../adr/0002-gpui-zed-stable-pin.md) says that
commit is a **Zed stable release tag**, and a scheduled workflow proposes
a bump whenever Zed ships a newer stable release. The same arrangement as
Zmail's (unicornops/zmail, its ADR 0003), without Zmail's current
deviation to a Zed pull request, which was for its web renderer and does
not apply here.

## Current Pin

<!-- zed-pin:start -->
| | |
| --- | --- |
| Zed release tag | [`v1.27.0`](https://github.com/zed-industries/zed/releases/tag/v1.27.0) |
| Commit | [`c84af5b99cbc366d675064e52b23664a015b6e35`](https://github.com/zed-industries/zed/commit/c84af5b99cbc366d675064e52b23664a015b6e35) |
| Released | 2026-11-04 |
| Rust toolchain (Zed's `rust-toolchain.toml`) | `1.98.1` |
<!-- zed-pin:end -->

The table between the `zed-pin` markers is rewritten by
`scripts/bump-zed-gpui.sh`; keep the markers. `Cargo.lock` is the source of
truth for the exact commit, and `familychat --version` and the window read
the tag and commit from it at build time (`crates/familychat-app/build.rs`).

Other upstream inputs:

- Theme schema: `https://zed.dev/schema/themes/v0.2.0.json`, implemented
  by the local compatibility layer in `crates/familychat-theme`.

## What The Pin Consists Of

- `Cargo.toml` `[workspace.dependencies]`: `gpui` and `gpui_platform`, both
  `git = "https://github.com/zed-industries/zed"` with the same `tag`.
  `gpui_platform` picks the OS backend (`gpui_platform::application()`);
  the platform code lives in `gpui_macos`, `gpui_windows`, `gpui_linux` and
  `gpui_wgpu`, and an app cannot start from `gpui::Application::new()`.
- `Cargo.toml` `[patch.crates-io]`: Zed's root manifest patches some
  crates.io crates, but `[patch]` only applies to the workspace that
  declares it. Family Chat repeats the ones that change GPUI's runtime
  behaviour, at the revisions Zed uses:
  - `async-task` (GPUI's executor), from `zed-industries/async-task`
    (Zmail's older tag took it from `smol-rs/async-task`);
  - `calloop` (the Linux event loop's idle budgeting), from
    `zed-industries/calloop`;
  - `khronos-egl` (no panic when a failed EGL call reports `EGL_SUCCESS`,
    in wgpu's GL backend on Linux), from `zed-industries/khronos-egl`.

  Zed's other patches (`cpal`, `tree-sitter-language`, `async-process`,
  `windows-capture`, `livekit`, `libwebrtc`, `webrtc-sys`, `notify`,
  `notify-types`, `crash-context` and the local `scratch`) cover crates
  this build does not compile. The bump script re-syncs the mirrored
  revisions and lists any patch Zed adds.
- `rust-toolchain.toml` and `rust-version`: the toolchain Zed pins at that
  tag. GPUI follows new stable Rust quickly, so tracking Zed's toolchain
  keeps the build on what upstream tests. CI reads the channel from
  `rust-toolchain.toml`.
- `deny.toml` `allow-git`: exactly the git repositories the pinned graph
  uses: Zed itself; its `font-kit` (macOS fonts), `xim-rs` (X11 input
  methods) and `oo7` (Secret Service) forks; the `gfx-rs/wgpu` revision
  `gpui_wgpu` pins; and the three patch sources. `Cargo.lock` also holds
  Zed's `scap` fork, for `gpui_linux`'s screen capture, which this build
  does not enable, so it is not allowed.

## Why Stable Release Tags

- crates.io stopped tracking GPUI: `gpui 0.2.2` (2025-10-22) is still the
  newest release a year later, and lacks AccessKit screen-reader support,
  system notifications and the wgpu Linux renderer.
- Arbitrary `main` commits are not tested as a release; a stable tag is
  what Zed ships to its own users, gets a changelog, and is followed by
  patch releases when something breaks.
- The third-party `gpui-pre` crate (weekly snapshots republished outside
  Zed) is a supply-chain risk.

## Automatic Bumps

`.github/workflows/zed-upstream.yml` runs every Monday and on demand
(`workflow_dispatch`, optionally with a specific tag). It calls
`scripts/bump-zed-gpui.sh`, which:

1. reads the latest Zed release from
   `gh api repos/zed-industries/zed/releases/latest` (never a draft or
   pre-release) and stops when it is not newer than the pinned tag;
2. moves the `gpui`/`gpui_platform` tag, `rust-toolchain.toml`,
   `rust-version` and the mirrored patches to what Zed uses at the new tag;
3. runs `cargo update -p gpui -p gpui_platform`, falling back to a full
   `cargo update` only when the targeted update cannot resolve;
4. rewrites the pin table above.

The workflow then force-pushes the result to the `chore/zed-gpui-bump`
branch and opens, or updates, a single pull request titled
`chore(deps): bump Zed GPUI to vX.Y.Z` with links to the release notes of
every stable release since the current pin. `chore` keeps these bumps out
of the release changelog; a bump that needs real code changes should be
merged with a `feat`/`fix` title instead.

The workflow pushes and opens the PR with `secrets.RELEASE_PLEASE_TOKEN`
when it is set, like `release-please.yml`. Without it, it falls back to the
default `GITHUB_TOKEN`, and pull requests opened with that token do not
trigger CI: close and reopen the PR, or push to its branch, to run the
checks. The bump never edits `.github/workflows`, so the token needs no
`workflows` scope.

A pull request that changes the workflow or the script runs the script as
a check: it reports whether the pin is current and never pushes or opens
a pull request.

## Bumping Manually

```bash
scripts/bump-zed-gpui.sh            # latest stable release
scripts/bump-zed-gpui.sh v1.28.0    # a specific stable tag
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check licenses advisories bans sources
cargo build -p familychat-app --release --locked
```

The script needs an authenticated `gh` CLI, `cargo`, `jq` and Python 3.11
or newer. Then launch the app to check that it still renders (CI's build
job does that on all three platforms: the window smoke test and its
`window-smoke-*` screenshots), and fix any GPUI API breakage on the same
branch.

## History

- **Phase 0 (November 2026, issue #1):** pinned to Zed `v1.27.0`, the
  newest stable release (published 2026-11-04; `v1.28.0-pre` the same day
  is a pre-release and is not pinned). The decision and the build costs
  are in [ADR 0002](../adr/0002-gpui-zed-stable-pin.md).
