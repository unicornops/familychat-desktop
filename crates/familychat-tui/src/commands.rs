//! The terminal UI's commands (ADR 0008, "Commands and keys").
//!
//! Every command of `familychat-core`'s shared table
//! ([`familychat_core::commands::COMMANDS`]) is a [`Cmd`] here, on the
//! table's terminal key, with the table's name and description in the
//! palette: the window and the terminal offer the same commands under the
//! same names. The terminal adds its own movement and answer keys (Enter,
//! the arrows, Tab between links, `y` to copy, the answers to a question),
//! which the window has no need for because it has a pointer and focus;
//! they are listed in [`LOCAL`] and never reuse a table key.
//!
//! A new row in the table fails `every_table_command_has_a_terminal_command`
//! until it is mapped in [`SHARED`] (or listed in [`LATER`] with the phase
//! that brings it to the terminal), so the two frontends cannot drift.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use familychat_core::commands::command;

/// A command the terminal UI runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cmd {
    // -- From the shared table ------------------------------------------
    /// `ToggleCommandPalette`.
    Palette,
    /// `SignInWithCode`.
    SignInWithCode,
    /// `SignInWithPassword`.
    SignInWithPassword,
    /// `UseAnotherServer`.
    UseAnotherServer,
    /// `PasteSignInCode`.
    PasteSignInCode,
    /// `GoBack`.
    GoBack,
    /// `NextRoom`.
    NextRoom,
    /// `PreviousRoom`.
    PreviousRoom,
    /// `NextUnreadRoom`.
    NextUnreadRoom,
    /// `PreviousUnreadRoom`.
    PreviousUnreadRoom,
    /// `JumpToOldestUnread`.
    JumpToOldestUnread,
    /// `JumpToLatest`.
    JumpToLatest,
    /// `ToggleMemberList`.
    ToggleMemberList,
    /// `ShowAllRooms`.
    ShowAllRooms,
    /// `ShowUnreadRooms`.
    ShowUnreadRooms,
    /// `ShowPeople`.
    ShowPeople,
    /// `ShowInvites`.
    ShowInvites,
    /// `Quit`.
    Quit,

    // -- The terminal's own ----------------------------------------------
    /// Enter: open the room, use the option, sign in.
    Select,
    /// Esc or `<` in the rooms: back to the room list.
    Back,
    /// Up a line or a row.
    Up,
    /// Down a line or a row.
    Down,
    /// A page up.
    PageUp,
    /// A page down.
    PageDown,
    /// Tab: the next link or file in the room.
    NextRef,
    /// Shift-Tab: the previous link or file.
    PrevRef,
    /// `l`: a link or file by its number.
    OpenRef,
    /// `s`: save the file under the cursor.
    SaveFile,
    /// `y`: copy the selection.
    Copy,
    /// `t`: try again (the keychain, the profile, the sync).
    TryAgain,
    /// `s` on the lost-key screen: start over.
    StartOver,
    /// `v`: show or hide spoilers.
    Spoilers,
    /// `b`: the bell for new messages, on or off.
    Bell,
    /// Tab on a form: the next field.
    NextField,
    /// `O`: the command bar's next page.
    OtherCmds,
    /// A question's answer: the key it stands for.
    Answer(char),
}

/// The table's commands and the terminal command each is.
pub const SHARED: &[(&str, Cmd)] = &[
    ("ToggleCommandPalette", Cmd::Palette),
    ("SignInWithCode", Cmd::SignInWithCode),
    ("SignInWithPassword", Cmd::SignInWithPassword),
    ("UseAnotherServer", Cmd::UseAnotherServer),
    ("PasteSignInCode", Cmd::PasteSignInCode),
    ("GoBack", Cmd::GoBack),
    ("NextRoom", Cmd::NextRoom),
    ("PreviousRoom", Cmd::PreviousRoom),
    ("NextUnreadRoom", Cmd::NextUnreadRoom),
    ("PreviousUnreadRoom", Cmd::PreviousUnreadRoom),
    ("JumpToOldestUnread", Cmd::JumpToOldestUnread),
    ("JumpToLatest", Cmd::JumpToLatest),
    ("ToggleMemberList", Cmd::ToggleMemberList),
    ("ShowAllRooms", Cmd::ShowAllRooms),
    ("ShowUnreadRooms", Cmd::ShowUnreadRooms),
    ("ShowPeople", Cmd::ShowPeople),
    ("ShowInvites", Cmd::ShowInvites),
    ("Quit", Cmd::Quit),
];

/// Table commands the terminal does not offer yet, with the phase that
/// brings them (PLAN.md, section 13.11: writing in the terminal).
#[cfg_attr(not(test), allow(dead_code))]
pub const LATER: &[(&str, &str)] = &[];

/// The terminal's own commands: the key the bar shows, the bar's label
/// and what the palette says it does.
pub const LOCAL: &[(Cmd, &str, &str, &str)] = &[
    (
        Cmd::Select,
        "Enter",
        "Open",
        "Open the room, or use the option under the cursor.",
    ),
    (Cmd::Back, "<", "Rooms", "Go back to the room list."),
    (Cmd::Up, "Up", "Up", "Move up a row or a line."),
    (Cmd::Down, "Down", "Down", "Move down a row or a line."),
    (
        Cmd::PageUp,
        "PgUp",
        "Older",
        "Scroll up a page, to older messages.",
    ),
    (
        Cmd::PageDown,
        "PgDn",
        "Newer",
        "Scroll down a page, to newer messages.",
    ),
    (
        Cmd::NextRef,
        "Tab",
        "NextLink",
        "Move to the next link or file in the room.",
    ),
    (
        Cmd::PrevRef,
        "S-Tab",
        "PrevLink",
        "Move to the previous link or file in the room.",
    ),
    (
        Cmd::OpenRef,
        "l",
        "Link#",
        "Open a link or a file by its number.",
    ),
    (
        Cmd::SaveFile,
        "s",
        "Save",
        "Save the file under the cursor where you choose.",
    ),
    (Cmd::Copy, "y", "Copy", "Copy the selected text."),
    (Cmd::TryAgain, "t", "TryAgain", "Try again now."),
    (
        Cmd::StartOver,
        "s",
        "StartOvr",
        "Move the data that can't be read aside and start again.",
    ),
    (
        Cmd::Spoilers,
        "v",
        "Spoilers",
        "Show or hide the spoilers in this room.",
    ),
    (
        Cmd::Bell,
        "b",
        "Bell",
        "Ring the terminal bell for new messages, or stop.",
    ),
    (Cmd::NextField, "Tab", "NextFld", "Move to the next field."),
    (
        Cmd::OtherCmds,
        "O",
        "OTHER",
        "Show the command bar's other commands.",
    ),
];

impl Cmd {
    /// The table row this command is, if it is one.
    pub fn shared(self) -> Option<&'static familychat_core::commands::Command> {
        SHARED
            .iter()
            .find(|(_, cmd)| *cmd == self)
            .and_then(|(id, _)| command(id))
    }

    fn local(self) -> Option<&'static (Cmd, &'static str, &'static str, &'static str)> {
        LOCAL.iter().find(|(cmd, ..)| *cmd == self)
    }

    /// The key the command bar shows for it.
    pub fn key(self) -> String {
        if let Cmd::Answer(ch) = self {
            return ch.to_ascii_uppercase().to_string();
        }
        if let Some(shared) = self.shared() {
            return key_label(shared.tui_key);
        }
        self.local()
            .map(|(_, key, ..)| (*key).to_owned())
            .unwrap_or_default()
    }

    /// The command bar's short label for it.
    pub fn label(self) -> &'static str {
        match self {
            Cmd::Palette => "Commands",
            Cmd::SignInWithCode => "Code",
            Cmd::SignInWithPassword => "Password",
            Cmd::UseAnotherServer => "OtherSrv",
            Cmd::PasteSignInCode => "Paste",
            Cmd::GoBack => "Back",
            Cmd::NextRoom => "NextRoom",
            Cmd::PreviousRoom => "PrevRoom",
            Cmd::NextUnreadRoom => "NextNew",
            Cmd::PreviousUnreadRoom => "PrevNew",
            Cmd::JumpToOldestUnread => "FirstNew",
            Cmd::JumpToLatest => "Latest",
            Cmd::ToggleMemberList => "Members",
            Cmd::ShowAllRooms => "All",
            Cmd::ShowUnreadRooms => "Unread",
            Cmd::ShowPeople => "People",
            Cmd::ShowInvites => "Invites",
            Cmd::Quit => "Quit",
            Cmd::Answer(_) => "",
            other => other.local().map(|(_, _, label, _)| *label).unwrap_or(""),
        }
    }

    /// What the palette says it does.
    pub fn description(self) -> &'static str {
        if let Some(shared) = self.shared() {
            return shared.description;
        }
        self.local()
            .map(|(_, _, _, description)| *description)
            .unwrap_or("")
    }

    /// The palette's name for it: the table's name, or the bar's label.
    pub fn name(self) -> &'static str {
        match self.shared() {
            Some(shared) => shared.name,
            None => self.label(),
        }
    }

    /// Whether `key` is this table command's terminal key.
    pub fn matches(self, key: &KeyEvent) -> bool {
        self.shared()
            .is_some_and(|shared| key_matches(shared.tui_key, key))
    }
}

/// A table key as the command bar shows it: `^V` for `ctrl-v`, `Esc` for
/// `esc`, a character as itself.
pub fn key_label(spec: &str) -> String {
    if let Some(rest) = spec.strip_prefix("ctrl-") {
        return format!("^{}", rest.to_ascii_uppercase());
    }
    match spec {
        "esc" => "Esc".to_owned(),
        "enter" => "Enter".to_owned(),
        "tab" => "Tab".to_owned(),
        other => other.to_owned(),
    }
}

/// Whether `key` is the table's terminal key `spec` (`q`, `N`, `:`,
/// `esc`, `ctrl-v`). Letters match exactly (`n` is not `N`); Shift is
/// ignored for characters, since the character already says it.
pub fn key_matches(spec: &str, key: &KeyEvent) -> bool {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if let Some(rest) = spec.strip_prefix("ctrl-") {
        let mut chars = rest.chars();
        return match (chars.next(), chars.next(), key.code) {
            (Some(want), None, KeyCode::Char(got)) => {
                control && !alt && got.eq_ignore_ascii_case(&want)
            }
            _ => false,
        };
    }
    if control || alt {
        return false;
    }
    match spec {
        "esc" => key.code == KeyCode::Esc,
        "enter" => key.code == KeyCode::Enter,
        "tab" => key.code == KeyCode::Tab,
        _ => {
            let mut chars = spec.chars();
            matches!((chars.next(), chars.next(), key.code), (Some(want), None, KeyCode::Char(got)) if got == want)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use familychat_core::commands::COMMANDS;

    use super::*;

    #[test]
    fn every_table_command_has_a_terminal_command() {
        for row in COMMANDS {
            let mapped = SHARED.iter().any(|(id, _)| *id == row.id);
            let later = LATER.iter().any(|(id, _)| *id == row.id);
            assert!(
                mapped || later,
                "{} is in the commands table but not in the terminal UI: map it in SHARED, or \
                 list it in LATER with the phase that brings it",
                row.id
            );
            assert!(!(mapped && later), "{} is both mapped and later", row.id);
        }
        for (id, cmd) in SHARED {
            let row = command(id).unwrap_or_else(|| panic!("{id} is not in the table"));
            assert_eq!(cmd.shared().map(|row| row.id), Some(row.id));
            assert!(!cmd.label().is_empty(), "{id} has no bar label");
        }
        let cmds: BTreeSet<_> = SHARED.iter().map(|(_, cmd)| format!("{cmd:?}")).collect();
        assert_eq!(cmds.len(), SHARED.len(), "two ids share a command");
    }

    #[test]
    fn every_table_key_parses_and_matches_only_itself() {
        let keys = [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        ];
        for row in COMMANDS {
            let matching = keys
                .iter()
                .filter(|key| key_matches(row.tui_key, key))
                .count();
            assert!(matching <= 1, "{} matches {matching} keys", row.tui_key);
        }
        assert!(key_matches("N", &keys[1]));
        assert!(!key_matches("n", &keys[1]));
        assert!(key_matches("ctrl-v", &keys[4]));
        assert!(!key_matches("v", &keys[4]));
        assert!(!key_matches(
            "q",
            &KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
        ));
        assert_eq!(key_label("ctrl-v"), "^V");
        assert_eq!(key_label("esc"), "Esc");
    }

    /// A terminal key never shadows a table key of a scope it shows in:
    /// the room keys and the global ones, and the sign-in keys and the
    /// global ones.
    #[test]
    fn local_keys_do_not_reuse_table_keys() {
        let table: BTreeSet<&str> = COMMANDS.iter().map(|row| row.tui_key).collect();
        for (cmd, key, label, description) in LOCAL {
            assert!(!table.contains(key), "{cmd:?} reuses the table key {key}");
            assert!(!label.is_empty() && !description.is_empty());
        }
    }
}
