//! The packages agree with the app (ADR 0014): the macOS bundle's
//! Info.plist, the Linux desktop entry, the Windows MSI and the .deb name
//! the identifiers in `familychat_core::identity` and register the
//! `familychat` URL scheme the way the app expects to receive a link, and
//! every licence file in the repository is staged into every artefact by
//! `scripts/stage-licences.sh`. These are text checks of the files the
//! release workflow packages from; the workflow's dry run builds and
//! inspects the packages themselves.

use std::path::{Path, PathBuf};

use familychat_core::identity::{APP_ID, APP_NAME, BINARY_NAME, URL_SCHEME};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The file's text, with Windows line endings (a checkout with
/// `core.autocrlf`) made Unix ones.
fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn the_macos_bundle_registers_the_scheme_under_the_app_id() {
    let plist = read("packaging/macos/Info.plist");
    for (key, value) in [
        ("CFBundleIdentifier", APP_ID),
        ("CFBundleExecutable", BINARY_NAME),
        ("CFBundleName", APP_NAME),
        ("CFBundleDisplayName", APP_NAME),
        ("CFBundleURLName", APP_ID),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", "@VERSION@"),
    ] {
        let entry = format!("<key>{key}</key><string>{value}</string>");
        assert!(plist.contains(&entry), "Info.plist lacks {entry}");
    }
    let schemes = plist
        .split_once("<key>CFBundleURLSchemes</key>")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("</array>"))
        .map(|(array, _)| array)
        .expect("Info.plist has CFBundleURLSchemes");
    assert!(
        schemes.contains(&format!("<string>{URL_SCHEME}</string>")),
        "CFBundleURLSchemes lacks {URL_SCHEME}"
    );
}

#[test]
fn the_desktop_entry_is_named_after_the_app_id_and_handles_the_scheme() {
    let relative = format!("packaging/linux/{APP_ID}.desktop");
    let entry = read(&relative);
    let lines: Vec<&str> = entry.lines().collect();
    assert!(lines.contains(&"[Desktop Entry]"));
    for expected in [
        format!("Name={APP_NAME}"),
        // %u: the desktop passes one link, which `familychat LINK` takes.
        format!("Exec={BINARY_NAME} %u"),
        format!("TryExec={BINARY_NAME}"),
        format!("Icon={APP_ID}"),
        format!("StartupWMClass={APP_ID}"),
        format!("MimeType=x-scheme-handler/{URL_SCHEME};"),
        "Terminal=false".to_owned(),
        "Type=Application".to_owned(),
    ] {
        assert!(
            lines.contains(&expected.as_str()),
            "{relative} lacks the line {expected}"
        );
    }
}

#[test]
fn the_msi_installs_per_user_and_hands_links_to_the_launcher() {
    let wxs = read("crates/familychat-app/wix/main.wxs");
    for expected in [
        format!("Name=\"{APP_NAME}\""),
        "InstallScope=\"perUser\"".to_owned(),
        "InstallPrivileges=\"limited\"".to_owned(),
        format!("Name=\"{BINARY_NAME}.exe\""),
        format!("Name=\"{BINARY_NAME}-launcher.exe\""),
        format!("Key=\"Software\\Classes\\{URL_SCHEME}\""),
        "Name=\"URL Protocol\"".to_owned(),
        // The whole link, quoted, to the launcher (which starts
        // familychat.exe without a console).
        format!(
            "Key=\"shell\\open\\command\"\n                                   Type=\"string\"\n                                   Value=\"&quot;[APPLICATIONFOLDER]{BINARY_NAME}-launcher.exe&quot; &quot;%1&quot;\""
        ),
        format!("Target=\"[APPLICATIONFOLDER]{BINARY_NAME}-launcher.exe\""),
        format!(
            "<ShortcutProperty Key=\"System.AppUserModel.ID\"\n                                      Value=\"{APP_ID}\" />"
        ),
    ] {
        assert!(wxs.contains(&expected), "main.wxs lacks {expected}");
    }
    // Per-user: nothing is written to the machine's registry or folders.
    for machine_wide in ["HKLM", "ProgramFiles", "perMachine"] {
        assert!(
            !wxs.contains(machine_wide),
            "main.wxs mentions {machine_wide}"
        );
    }
}

#[test]
fn the_deb_is_the_binary_name_and_installs_the_desktop_entry() {
    let manifest = read("crates/familychat-app/Cargo.toml");
    let deb = manifest
        .split_once("[package.metadata.deb]")
        .map(|(_, rest)| rest)
        .expect("Cargo.toml has [package.metadata.deb]");
    assert!(deb.contains(&format!("name = \"{BINARY_NAME}\"")));
    assert!(deb.contains(&format!(
        "[\"../../packaging/linux/{APP_ID}.desktop\", \"usr/share/applications/\", \"644\"]"
    )));
    assert!(deb.contains(&format!(
        "[\"target/release/{BINARY_NAME}\", \"usr/bin/\", \"755\"]"
    )));
    assert!(deb.contains(&format!(
        "apps/{APP_ID}.svg\", \"usr/share/icons/hicolor/scalable/apps/\""
    )));
}

/// Every licence file in the repository (outside `target` and `.git`).
fn licence_files(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | "node_modules") {
                licence_files(&path, root, found);
            }
        } else if name.starts_with("LICENSE")
            && let Ok(relative) = path.strip_prefix(root)
        {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn every_licence_in_the_repository_is_staged_into_every_artefact() {
    let root = repo_root().canonicalize().expect("the repository root");
    let mut found = Vec::new();
    licence_files(&root, &root, &mut found);
    found.sort();
    assert!(
        found.len() >= 4,
        "expected LICENSE and the bundled licences, found {found:?}"
    );
    let stage = read("scripts/stage-licences.sh");
    let wxs = read("crates/familychat-app/wix/main.wxs");
    for licence in &found {
        let line = stage
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("\"{licence} ")))
            .unwrap_or_else(|| panic!("scripts/stage-licences.sh does not stage {licence}"));
        // The MSI lists its files one by one: each staged licence is in it.
        let destination = line
            .trim()
            .trim_matches('"')
            .rsplit('/')
            .next()
            .and_then(|last| last.split_whitespace().last())
            .expect("a destination");
        assert!(
            wxs.contains(&format!("Name=\"{destination}\"")),
            "main.wxs does not install {destination}"
        );
    }
}
