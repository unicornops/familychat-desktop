# Releases and packages

How Family Chat is released, what each release attaches, how it is
signed, and how to check it. The decisions are ADR 0014
(`docs/adr/0014-packaging-signing-and-distribution.md`); the workflow is
`.github/workflows/release-please.yml`.

## How a release happens

1. Conventional Commits land on `main`. release-please keeps a release
   pull request open (`chore(main): release X.Y.Z`), and
   `scripts/render-release-pr.sh` adds the commits outside
   `crates/familychat-app` that release-please does not count.
2. Merging the release pull request tags `vX.Y.Z` and creates the GitHub
   release. The asset jobs then build from the release commit:
   - **Third-Party Notices**: `THIRD_PARTY.html` with cargo-about;
   - **Release Screenshot**: the window on the screenshot fixtures
     (`room-dark`), from a fixture build that is never shipped;
   - **build_release_assets** on Linux, macOS and Windows: the archive and
     the installer, signed where the secrets allow, inspected, installed,
     given a `familychat://` link through the OS, and uploaded;
   - **Checksums, Attestations and Notes**: `SHA256SUMS`, a build
     provenance attestation for every file, the screenshot, and the
     "Downloads and signing" section of the release notes.
3. Nothing else is needed. A failed Windows job after the rest is
   published can be redone with **Release Asset Backfill**
   (`release-backfill.yml`, a manual run with the tag), which rebuilds the
   zip and the MSI, replaces their lines in `SHA256SUMS` and attests them.

Never tag by hand and never edit a release's files by hand: rerun the
workflow (**Release Please**, manual run with `tag_name`) instead, which
replaces every file and the notes' signing section.

## What a release attaches

| File | For | Contents |
| --- | --- | --- |
| `familychat-macos.dmg` | macOS 12 or later, Apple silicon | `Family Chat.app` (the licences in `Contents/Resources`) and an Applications link |
| `familychat-macos.tar.gz` | the macOS terminal | `familychat`, `LICENSE`, `THIRD_PARTY.html`, `licenses/` |
| `familychat-windows-x86_64.msi` | Windows 10 and 11, x86-64 | per-user install of `familychat.exe`, `familychat-launcher.exe` and the licences |
| `familychat-windows-x86_64.zip` | the Windows terminal | `familychat.exe`, `LICENSE`, `THIRD_PARTY.html`, `licenses\` |
| `familychat-linux-x86_64.deb` | Debian 13, Ubuntu 24.04 or later, x86-64 | `/usr/bin/familychat`, the desktop entry, icons, `/usr/share/doc/familychat` |
| `familychat-linux-x86_64.tar.gz` | other Linux, x86-64 | `familychat`, `LICENSE`, `THIRD_PARTY.html`, `licenses/` |
| `familychat-screenshot.png` | the release page | the window on a fixture room |
| `SHA256SUMS` | checking downloads | the SHA-256 of every file above |

`licenses/` holds `Inter-OFL.txt` (the Inter font Linux builds embed),
`Lucide-LICENSE.txt` (the icons) and `GPUI-Apache-2.0.txt` (the code
ported from Zed). `scripts/stage-licences.sh` is the list every package
copies.

## What each installer registers

- **macOS**: `Info.plist` (`packaging/macos/Info.plist`) has the bundle id
  `family.safechat.desktop` (which notifications need) and
  `CFBundleURLTypes` for `familychat`. Launch Services hands a link to the
  running app as an Apple Event (GPUI's `on_open_urls`), or starts the app
  and then hands it over.
- **Windows**: the MSI (`crates/familychat-app/wix/main.wxs`) installs to
  `%LOCALAPPDATA%\Programs\Family Chat` without an administrator prompt,
  and writes `HKCU\Software\Classes\familychat` (`URL Protocol`,
  `DefaultIcon`, `shell\open\command` =
  `"%LOCALAPPDATA%\Programs\Family Chat\familychat-launcher.exe" "%1"`), a
  Start-menu shortcut with the app user model id `family.safechat.desktop`,
  and the folder on the user's `PATH`. The launcher starts `familychat.exe`
  with the link and no console window; `familychat.exe` hands the link to
  the running window over its pipe (ADR 0013). Uninstalling (Settings,
  Apps) removes all of it; the profile in `%APPDATA%\Family Chat` stays.
- **Linux**: `/usr/share/applications/family.safechat.desktop.desktop`
  (`packaging/linux/`) with `Exec=familychat %u` and
  `MimeType=x-scheme-handler/familychat;`, and icons in the hicolor theme.
  The desktop starts `familychat LINK`, which hands the link to the running
  window over its socket.

A browser asks before it opens a `familychat://` link in an app; the app
then asks "Sign in as …?" before it uses the code.

## Signing

| OS | Today | When the secrets exist |
| --- | --- | --- |
| macOS | Developer ID, hardened runtime, notarised; the app and the DMG stapled | the org-wide `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_DEVELOPER_ID`, `APPLE_APP_PASSWORD`, `APPLE_TEAM_ID` (shared with the other UnicornOps macOS apps) |
| Windows | **not signed**: SmartScreen warns ("Windows protected your PC", More info, Run anyway) | Azure Artifact Signing: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_ARTIFACT_SIGNING_ENDPOINT` (for example `https://weu.codesigning.azure.net/`), `AZURE_ARTIFACT_SIGNING_ACCOUNT`, `AZURE_ARTIFACT_SIGNING_PROFILE` |
| Linux | **not signed** during the alpha | (no plan yet; an apt repository would sign its index) |

- Without the Apple certificate, the app and the archive's binary get an ad
  hoc signature: macOS refuses to open the app from a download until you
  Control-click it and choose Open (or allow it in System Settings, Privacy
  & Security). With the certificate but without the Apple ID secrets, the
  files are signed but not notarised, which Gatekeeper treats the same way.
- The Azure step runs only when all six secrets are set; some but not all
  fails the job. The App Registration (the client id and secret) needs the
  certificate profile signer role on the signing account (see Microsoft's
  Artifact Signing documentation). With them set, both executables are signed before they are
  packed and the MSI after it is built, and the job checks every signature
  is valid.
- Every release's notes have a "Downloads and signing" section, written by
  `scripts/release-signing-notes.sh` from what each job actually did, that
  names every file that is not signed. Nothing describes a file as signed
  unless its job signed it.
- No signing step prints a secret, and none blocks a release when its
  secrets are absent.

## Checking a download

```bash
# Checksums: every file is in SHA256SUMS.
sha256sum -c SHA256SUMS --ignore-missing

# Provenance: the file was built by this repository's release workflow.
gh attestation verify familychat-linux-x86_64.deb --repo unicornops/familychat-desktop

# macOS: the signature and the notarisation ticket.
spctl --assess --type open --context context:primary-signature -vv familychat-macos.dmg
codesign --verify --deep --strict -vv "/Applications/Family Chat.app"
xcrun stapler validate "/Applications/Family Chat.app"

# Windows (PowerShell): NotSigned until Azure Artifact Signing is set up.
Get-AuthenticodeSignature .\familychat-windows-x86_64.msi
```

## Building the packages locally

The workflow is the reference; these are the same steps by hand. Every
one starts from a release build and the staged licences:

```bash
cargo build -p familychat-app --release --locked
cargo about generate --locked --fail about.hbs -o THIRD_PARTY.html
scripts/stage-licences.sh target/dist/licences
```

**Linux** (needs `librsvg2-bin`; cargo-deb 3.8.0):

```bash
scripts/generate-app-icon.sh linux target/dist/icons
cargo install cargo-deb --version 3.8.0 --locked
cargo deb -p familychat-app --no-build --locked --output familychat-linux-x86_64.deb
dpkg-deb --info familychat-linux-x86_64.deb && dpkg-deb --contents familychat-linux-x86_64.deb
```

**macOS** (needs `brew install librsvg` and Xcode's command line tools):

```bash
scripts/generate-app-icon.sh macos target/dist/FamilyChat.icns target/dist/Assets.car
scripts/create-macos-bundle.sh "target/dist/Family Chat.app" target/release/familychat 0.0.1 target/dist target/dist/licences
codesign --force --sign - "target/dist/Family Chat.app"       # or your Developer ID, with --options runtime --timestamp
scripts/create-dmg.sh "target/dist/Family Chat.app" familychat-macos.dmg "Family Chat 0.0.1"
# Notarising: APPLE_ID, APPLE_PASSWORD and TEAM_ID in the environment.
scripts/notarize-macos.sh familychat-macos.dmg familychat-macos.dmg
```

**Windows** (PowerShell; WiX Toolset 3, which GitHub's Windows runners
have; cargo-wix 0.3.9):

```powershell
Copy-Item packaging/windows/familychat.ico target/dist/
cargo install cargo-wix --version 0.3.9 --locked
cargo wix -p familychat-app --no-build --nocapture --output familychat-windows-x86_64.msi
msiexec /i familychat-windows-x86_64.msi       # per-user, no administrator prompt
```

The app icon is `packaging/icons/familychat.svg` (Family Chat's teal tile
and speech bubble). The macOS and Linux icons are rendered from it at
build time; the Windows `.ico` is committed because `build.rs` embeds it in
every Windows build: run `scripts/generate-app-icon.sh windows
packaging/windows/familychat.ico` (needs `rsvg-convert` and ImageMagick)
after changing the SVG, and commit the result.

## Dry runs

Pull requests that change the release workflow, `packaging/`, `wix/`,
`build.rs`, the launcher, the packaging scripts or the notices run every
asset job as a dry run: everything is built, signed and notarised where the
secrets are visible to the pull request (not from forks), installed and
checked, and uploaded as workflow artefacts (one per OS, plus
`link-handler-<OS>` with the link check's screenshot and logs, and
`release-screenshot`), never attached to a release. The last job's summary
shows the sizes, `SHA256SUMS` and the signing section the release notes
would get. A manual run of **Release Please** with `tag_name` set to a
branch and `dry_run` ticked does the same for any ref.

The link checks (`scripts/link-handler-smoke.sh` and `.ps1`) open
`familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:%40alex%3Asmith.safechat.family&hs=smith.safechat.family&token=<placeholder>`
through the OS against the installed app on a throwaway profile, check
the running window received it and that the code reached no log or file,
and capture the "Sign in as …?" confirmation. Nothing is redeemed, so no
request is sent.

## Manual checks per OS

CI cannot click a link in a real browser. Before calling a release good,
on each OS, with a Family Chat parent account to make a sign-in code:

1. **macOS.** Download the DMG in Safari; open it: no warning beyond
   "downloaded from the internet" (Developer ID and notarised). Drag
   Family Chat to Applications and start it from Launchpad; the Dock and
   Launchpad show the teal icon. In a browser, open a sign-in link
   (`familychat://safechat.family/app/login?...` from the panel, or the
   website's "Open in Family Chat for desktop" button once it exists) and
   allow the browser to open Family Chat: the running window comes
   forward with "Sign in as …?". Quit Family Chat and open the link again:
   it starts and shows the same. Notifications: allow them when asked; a
   message in another room shows one.
2. **Windows.** Download and run the MSI: SmartScreen warns while it is
   unsigned (More info, Run anyway); no administrator prompt. Start Family
   Chat from the Start menu: no console window appears, the taskbar shows
   the teal icon. Open a sign-in link in Edge or Chrome and allow it: the
   window comes forward with "Sign in as …?", and no console window
   flashes. `familychat --version` works in a new terminal. Uninstall from
   Settings, Apps: the link no longer opens anything.
3. **Linux** (GNOME on Wayland, and KDE or another desktop if possible).
   `sudo apt install ./familychat-linux-x86_64.deb`. Family Chat is in the
   app grid with its icon, and the window's dock icon matches it. Open a
   sign-in link in Firefox or Chromium and allow it: the window comes
   forward with "Sign in as …?". `sudo apt remove familychat` removes the
   entry.
4. **Every OS.** Continue signs in; Cancel goes back. The profile's
   `logs/familychat.log` holds no `token=` value.
