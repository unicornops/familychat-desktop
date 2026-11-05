//! Safe file names for downloads (PLAN.md, section 7; ADR 0007).
//!
//! A received file's name comes from the sender. Before it names a file on
//! disk (the name the save dialog suggests), it goes through
//! [`sanitize_filename`]; [`numbered_filename`] names a copy beside a file
//! of the same name.
//!
//! Copied from Zmail's `zmail-security` (`src/attachment_safety.rs`):
//! `sanitize_filename`, its helpers and tests, and `numbered_filename`.
//! Family Chat never opens a downloaded file itself in this phase, so the
//! open-risk checks stay in Zmail.

/// What a file is called when its name sanitises to nothing.
pub const FALLBACK_FILENAME: &str = "attachment";

/// Longest sanitised name, in bytes. Most file systems allow 255.
const MAX_FILENAME_BYTES: usize = 255;

/// Device names Windows reserves in every directory, with or without an
/// extension (`CON`, `con.txt`, `LPT1.log`).
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
];

/// Turns an attachment's name into one file name that is safe on macOS,
/// Linux and Windows:
///
/// - path separators (`/`, `\`) and the other characters Windows forbids
///   (`<>:"|?*`) become `_`, so the name can never leave its directory;
/// - control characters and the invisible bidirectional and zero-width
///   marks are removed, so `invoice\u{202E}fdp.exe` cannot pose as a PDF;
/// - leading and trailing dots and spaces are trimmed, so `..` and `.`
///   are gone, the file is not hidden, and Windows does not drop a
///   trailing `.` from `setup.exe.`;
/// - Windows device names (`CON`, `NUL`, `COM1`, …) get a leading `_`;
/// - the name is cut to 255 bytes, keeping its extension;
/// - an empty result becomes [`FALLBACK_FILENAME`].
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|&ch| !ch.is_control() && !is_invisible_format_char(ch))
        .map(|ch| match ch {
            '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|ch: char| ch == '.' || ch.is_whitespace());
    if trimmed.is_empty() {
        return FALLBACK_FILENAME.to_owned();
    }

    let mut sanitized = trimmed.to_owned();
    let stem = sanitized.split('.').next().unwrap_or_default().trim_end();
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
    {
        sanitized.insert(0, '_');
    }
    truncate_keeping_extension(&sanitized, MAX_FILENAME_BYTES)
}

/// Bidirectional overrides, isolates and marks, and zero-width characters:
/// they change how a name looks without being visible themselves.
pub fn is_invisible_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Cuts `name` to at most `max` bytes on a character boundary, keeping a
/// short extension (up to 16 bytes) intact.
fn truncate_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && extension.len() < 16 => {
            (stem, Some(extension))
        }
        _ => (name, None),
    };
    let budget = max - extension.map_or(0, |extension| extension.len() + 1);
    let mut end = budget.min(stem.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let stem = stem[..end].trim_end_matches(|ch: char| ch == '.' || ch.is_whitespace());
    match extension {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem.to_owned(),
    }
}

/// How many numbered names [`numbered_filename`] callers try before giving
/// up on a file: `report (999).pdf` is the last.
pub const MAX_NUMBERED_NAMES: u32 = 999;

/// `filename` with ` (number)` before its extension, for saving beside a
/// file of the same name without replacing it; `number` 0 is the name
/// itself.
pub fn numbered_filename(filename: &str, number: u32) -> String {
    if number == 0 {
        return filename.to_owned();
    }
    match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({number}).{extension}"),
        _ => format!("{filename} ({number})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbered_names_go_before_the_extension() {
        assert_eq!(numbered_filename("report.pdf", 0), "report.pdf");
        assert_eq!(numbered_filename("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered_filename("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(numbered_filename("README", 3), "README (3)");
        assert_eq!(numbered_filename(".profile", 4), ".profile (4)");
    }

    #[test]
    fn ordinary_names_are_kept() {
        for name in [
            "report.pdf",
            "Q4 roadmap (draft).docx",
            "photo 2026-03-24.jpeg",
            "résumé.pdf",
            "日本語のファイル.txt",
            "archive.tar.gz",
        ] {
            assert_eq!(sanitize_filename(name), name);
        }
    }

    #[test]
    fn path_separators_and_traversal_cannot_leave_the_directory() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(
            sanitize_filename("..\\..\\Windows\\System32\\evil.dll"),
            "_.._Windows_System32_evil.dll"
        );
        assert_eq!(
            sanitize_filename("/absolute/path.txt"),
            "_absolute_path.txt"
        );
        assert_eq!(sanitize_filename("C:\\temp\\a.txt"), "C__temp_a.txt");
        for name in ["..", ".", "...", " . ", ""] {
            assert_eq!(sanitize_filename(name), FALLBACK_FILENAME, "{name:?}");
        }
        for name in [
            "../../etc/passwd",
            "..\\..\\x",
            "a/../../b",
            "/",
            "\\\\server\\share\\x",
        ] {
            let sanitized = sanitize_filename(name);
            assert!(!sanitized.contains('/') && !sanitized.contains('\\'));
            assert_ne!(sanitized, "..");
            assert!(!sanitized.starts_with('.'), "{sanitized}");
        }
    }

    #[test]
    fn windows_forbidden_characters_are_replaced() {
        assert_eq!(
            sanitize_filename("a<b>c:d\"e|f?g*h.txt"),
            "a_b_c_d_e_f_g_h.txt"
        );
    }

    #[test]
    fn control_and_invisible_characters_are_removed() {
        assert_eq!(sanitize_filename("re\u{0}port\n.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("tab\there.txt"), "tabhere.txt");
        // A right-to-left override (U+202E) after "invoice" would show
        // "invoice<RLO>fdp.exe" as "invoiceexe.pdf".
        assert_eq!(
            sanitize_filename("invoice\u{202E}fdp.exe"),
            "invoicefdp.exe"
        );
        assert_eq!(sanitize_filename("zero\u{200B}width.txt"), "zerowidth.txt");
        assert_eq!(sanitize_filename("\u{FEFF}bom.txt"), "bom.txt");
    }

    #[test]
    fn leading_dots_and_trailing_dots_and_spaces_are_trimmed() {
        assert_eq!(sanitize_filename(".bashrc"), "bashrc");
        assert_eq!(sanitize_filename("setup.exe."), "setup.exe");
        assert_eq!(sanitize_filename("setup.exe . . "), "setup.exe");
        assert_eq!(sanitize_filename("  spaced.txt  "), "spaced.txt");
    }

    #[test]
    fn windows_reserved_names_are_prefixed() {
        for name in [
            "CON",
            "con.txt",
            "Nul",
            "aux.tar.gz",
            "COM1.log",
            "lpt9",
            "PRN .txt",
        ] {
            let sanitized = sanitize_filename(name);
            assert!(sanitized.starts_with('_'), "{name} became {sanitized}");
        }
        for name in ["console.txt", "CONTRACT.pdf", "COM10.txt", "nullable.rs"] {
            assert_eq!(sanitize_filename(name), name);
        }
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary_keeping_the_extension() {
        let long = format!("{}.pdf", "é".repeat(300));
        let sanitized = sanitize_filename(&long);
        assert!(sanitized.len() <= MAX_FILENAME_BYTES);
        assert!(sanitized.ends_with(".pdf"));
        let no_extension = "x".repeat(400);
        assert_eq!(sanitize_filename(&no_extension).len(), MAX_FILENAME_BYTES);
    }
}
