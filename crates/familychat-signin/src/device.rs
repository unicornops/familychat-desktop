//! The device display name a new sign-in gives the homeserver (PLAN.md,
//! section 6.2), after the contract's `Family Chat (iPhone)`.

/// `Family Chat (macOS)`, `Family Chat (Windows)` or `Family Chat (Linux)`
/// for the OS this build runs on.
pub fn device_display_name() -> &'static str {
    device_display_name_for(std::env::consts::OS)
}

/// The device name for `os`, one of [`std::env::consts::OS`]'s values.
/// Family Chat builds only for the three desktop systems; anything else
/// is named generically.
pub fn device_display_name_for(os: &str) -> &'static str {
    match os {
        "macos" => "Family Chat (macOS)",
        "windows" => "Family Chat (Windows)",
        "linux" => "Family Chat (Linux)",
        _ => "Family Chat (desktop)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_each_desktop_system() {
        assert_eq!(device_display_name_for("macos"), "Family Chat (macOS)");
        assert_eq!(device_display_name_for("windows"), "Family Chat (Windows)");
        assert_eq!(device_display_name_for("linux"), "Family Chat (Linux)");
        assert_eq!(device_display_name_for("freebsd"), "Family Chat (desktop)");
        let expected = if cfg!(target_os = "macos") {
            "Family Chat (macOS)"
        } else if cfg!(target_os = "windows") {
            "Family Chat (Windows)"
        } else {
            "Family Chat (Linux)"
        };
        assert_eq!(device_display_name(), expected);
    }
}
