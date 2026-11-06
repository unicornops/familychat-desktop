# ADR 0014: Packaging, Signing and Distribution

## Status

Proposed with Phase 12 (issue #13); accepted when that pull request
merges. It records PLAN.md sections 10 and 13.12 as built, and finishes
what ADR 0013 left to this phase: registering `familychat://` with each
OS, and the macOS bundle without which `on_open_urls` receives nothing
and notifications have no bundle id.

## Context

Until now a release attached three archives holding the bare `familychat`
binary and its licences. Nobody could install Family Chat the way they
install other desktop apps, a sign-in link in a browser went nowhere, and
on macOS an unbundled binary has no bundle id, so `UNUserNotificationCenter`
may refuse its notifications. PLAN.md section 10 asks for a DMG, an MSI
and a `.deb`, each registering the URL scheme, signed where credentials
exist, with checksums, provenance attestations and a release screenshot,
copied from Zmail's release flow where it fits.

The org-wide `APPLE_*` secrets (Developer ID certificate, Apple ID and
team) are visible to this repository. There are no Azure Artifact
Signing secrets yet (Open decision 8), and Linux packages stay unsigned
during the alpha.

## Decision

### Artefacts

Every release attaches these, built by `release-please.yml` from the
release commit (`docs/release.md` has the details and the commands):

| File | What |
| --- | --- |
| `familychat-macos.dmg` | `Family Chat.app` and an Applications link (Apple silicon) |
| `familychat-macos.tar.gz` | the bare binary, for the terminal |
| `familychat-windows-x86_64.msi` | per-user installer |
| `familychat-windows-x86_64.zip` | the bare `familychat.exe` |
| `familychat-linux-x86_64.deb` | Debian and Ubuntu package `familychat` |
| `familychat-linux-x86_64.tar.gz` | the bare binary |
| `familychat-screenshot.png` | the release screenshot |
| `SHA256SUMS` | the checksums of all of the above |

The names carry no version, so `releases/latest/download/<file>` links
stay stable. Every archive and installer carries `LICENSE`,
`THIRD_PARTY.html` (cargo-about) and `licenses/` (Inter's SIL OFL 1.1,
Lucide's ISC/MIT, and the Apache-2.0 licence of the code ported from Zed,
`crates/familychat-ui/LICENSE-APACHE`, which the archives lacked until
now). `scripts/stage-licences.sh` is the one list; a test fails when a
licence file in the repository is missing from it or from the MSI.

### The identifiers, and one test that keeps them

Every package names the app with `familychat_core::identity`: bundle id,
Windows app user model id, desktop entry name, Wayland app id and X11
class `family.safechat.desktop`; binary and Debian package `familychat`;
URL scheme `familychat`. `crates/familychat-app/tests/packaging.rs` reads
`packaging/macos/Info.plist`, `packaging/linux/family.safechat.desktop.desktop`,
`crates/familychat-app/wix/main.wxs` and the `.deb` metadata and checks each
against those constants on every OS.

### macOS: a bundle, a DMG, Developer ID and notarisation

`scripts/create-macos-bundle.sh` builds `Family Chat.app` from
`packaging/macos/Info.plist` (adapted from the plist Zmail's workflow
writes): `CFBundleIdentifier` `family.safechat.desktop`,
`CFBundleExecutable` `familychat`, `CFBundleURLTypes` for `familychat`
(role Viewer), `LSMinimumSystemVersion` 12.0, the icns and an asset catalog
(`scripts/generate-app-icon.sh`, from Zmail's), and the licences in
`Contents/Resources`. Notifications need no Info.plist key beyond the
bundle id; the app asks for permission the first time it posts.

With the certificate secret present, the release job imports it into a
throwaway keychain (its password masked in the log) and signs the bundle
and the archive's binary with Developer ID, the hardened runtime and a
secure timestamp. No entitlements are needed: nothing uses JIT, and the
keychain items are the app's own. Both go to Apple in one notarisation
submission (`scripts/notarize-macos.sh`, Zmail's retry and polling loop as
a script); the app's ticket is stapled, the bare binary's is checked
online (a bare Mach-O cannot be stapled). The DMG (`scripts/create-dmg.sh`,
copied from Zmail) is then signed and notarised itself and stapled, so
Gatekeeper accepts it before it is opened. Without the certificate, both
get an ad hoc signature and the release notes say so; without the Apple ID
secrets, the Developer ID signature is kept but nothing is notarised, and
the notes say that too. The release never blocks on absent credentials.

A link opened in a browser reaches the running app as an Apple Event,
through GPUI's `on_open_urls` (macOS never starts a second process for
it); the window logs how many links arrived (never what), which the
release job's link check looks for.

### Windows: a per-user MSI and a launcher

The MSI (`cargo wix`, WiX v3, from Zmail's `main.wxs`) installs for the
current user, with no administrator prompt, to
`%LOCALAPPDATA%\Programs\Family Chat`; per-user because the app, its
profile and its keychain entries are one person's anyway, and a
per-user URL scheme lives under `HKCU\Software\Classes`. It writes:

- `HKCU\Software\Classes\familychat` with `URL Protocol`, a `DefaultIcon`
  and `shell\open\command` = `"…\familychat-launcher.exe" "%1"`;
- a Start-menu shortcut to the launcher carrying
  `System.AppUserModel.ID` = `family.safechat.desktop`, the id the app
  sets for itself, so its windows group with the shortcut and Windows
  attributes its toast notifications to it;
- the folder on the user's `PATH`, so `familychat tui` works in a
  terminal;
- the licences. Every component keeps an HKCU value as its key path and
  removes its folders on uninstall (ICE38, ICE64); uninstalling removes
  the handler.

**The launcher.** `familychat.exe` is a console program so that its
commands work in a terminal. Started from Explorer or a browser, a console
program gets a console window of its own (a Windows Terminal window on
Windows 11) for as long as it runs, and closing that window ends the app.
Rather than turning `familychat.exe` into a window-subsystem program (its
commands would lose their output) or reattaching a console with Win32
calls (more `unsafe`), the package adds `familychat-launcher.exe`, a
window-subsystem binary in safe `std` that starts `familychat.exe` from
its own folder with the same arguments and `CREATE_NO_WINDOW`, and exits.
The shortcut and the URL handler start the launcher; everything else (the
lock, the hand-off, the window) is `familychat.exe`'s, unchanged. This
departs from the issue's `"…\familychat.exe" "%1"`; the link still
reaches `familychat LINK` with the same argument. Elsewhere the launcher
only says it isn't used.

`build.rs` now embeds the app icon (`packaging/windows/familychat.ico`) in
the executables alongside the version resource the MSI binds its version
to.

**Signing.** Azure Artifact Signing (`azure/artifact-signing-action`,
pinned) signs both executables before they are packed and the MSI after,
only when all six of its secrets exist (`AZURE_TENANT_ID`,
`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_ARTIFACT_SIGNING_ENDPOINT`,
`AZURE_ARTIFACT_SIGNING_ACCOUNT`, `AZURE_ARTIFACT_SIGNING_PROFILE`); some
but not all of them fails the job. A check after packaging requires every
signature `Valid` when signing ran, and `NotSigned` otherwise, so nothing
claims a signature it lacks. Today the files are unsigned and the release
notes say so; SmartScreen will warn.

### Linux: a `.deb`, a desktop entry and icons

`cargo deb --no-build` (cargo-deb 3.8.0) packages the release build as
`familychat`: `/usr/bin/familychat`; the desktop entry
`/usr/share/applications/family.safechat.desktop.desktop` (named after the
app id so docks match it to the window) with
`MimeType=x-scheme-handler/familychat;` and `Exec=familychat %u`; hicolor
icons from 16 to 512 px and scalable; the licences in
`/usr/share/doc/familychat`. `Depends` comes from `dpkg-shlibdeps` plus
`libvulkan1 | libegl1`; it recommends a Secret Service and `xdg-utils`.
The desktop-file-utils and icon cache triggers register the entry, so
there are no maintainer scripts. The package is built on the
`ubuntu-latest` runner (24.04), so it needs that glibc or newer. A link a
browser opens starts `familychat LINK`, which hands it to the running
window over the single-instance hand-off (ADR 0013).

### Checksums, attestations, the screenshot and the notes

A final job downloads every package and the screenshot, writes one
`SHA256SUMS`, and attests all of them with `actions/attest-build-provenance`
(`subject-checksums`), so `gh attestation verify FILE --repo
unicornops/familychat-desktop` checks any file. Each build job records one
line per file in `signing-<OS>.txt` (signed, ad hoc or unsigned, and what
was done); `scripts/release-signing-notes.sh` turns them into a
"Downloads and signing" section that names every file that isn't signed,
added once to the release notes (a rerun replaces it). The per-OS
`SHA256SUMS-<OS>` files are gone. The screenshot is the window on the
screenshot fixtures (the `room-dark` state of the matrix), captured under
Xvfb from a fixture build that is never shipped.

### Every package is installed and given a link in CI

Pull requests that change the release workflow, the packaging or the
notices run the whole build as a dry run (workflow artefacts only, never
a release), with signing and notarisation when the secrets are visible.
Each job then installs what it built and opens a `familychat://` sign-in
link with a placeholder code through the OS
(`scripts/link-handler-smoke.sh`, `.ps1`): `xdg-open` after `apt install`
of the `.deb`; `open` after copying the app out of the DMG into
`/Applications`; `Start-Process` after `msiexec /i`. Each checks the
running window received it (the hand-off's log line on Linux and Windows,
the `on_open_urls` line on macOS), that only the window still runs, and
that no log or file holds the code, and saves a screenshot of the
"Sign in as …?" confirmation. Nothing is redeemed without Continue, so no
request leaves the runner. The `.deb` is also removed and the MSI
uninstalled, checking the handler goes with them; the MSI's tables are
printed through Windows Installer, the DMG's listing and Info.plist
through `plutil`.

## Consequences

- Installing is a download and a double-click (or `apt install`), and a
  sign-in link from the panel opens in Family Chat on every OS. A browser
  still asks before handing a link to an app.
- Windows SmartScreen warns about the unsigned MSI until Azure Artifact
  Signing is set up; the release notes say so. Linux packages stay
  unsigned during the alpha, and there is no apt repository.
- The macOS build is Apple silicon only, as before.
- The `familychat` package name and the `familychat://` scheme are also
  claimed by the Electron desktop build in familychat-web (Open decision
  3); installing both on one machine leaves the scheme with whichever the
  OS chose last.
- On Linux and Windows the link is in the second process's `argv`
  (PLAN.md section 12's risk): it forwards at once and logs nothing.
- Pull requests that touch the packaging spend about the time of a
  release build on each OS, plus two notarisation round trips.
- A release no longer has `SHA256SUMS-<OS>`; `release-backfill.yml`
  rebuilds the zip and the MSI and replaces their lines in `SHA256SUMS`.

## Follow-ups

- Azure Artifact Signing: the signing account, the certificate profile
  and where the secrets live (Open decision 8; a protected `release`
  environment through gitops-environments was proposed). Workload
  identity federation instead of a client secret would need `azure/login`
  before the signing steps.
- An AppStream metainfo file and a Flatpak, and an apt repository, when
  distribution beyond GitHub Releases is decided (Open decision 9).
- A universal (Intel and Apple silicon) macOS build, if wanted.
- Retiring the Electron build's claim on `familychat://` (Open decision 3).
