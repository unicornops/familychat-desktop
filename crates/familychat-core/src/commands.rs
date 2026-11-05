//! The commands table (PLAN.md, section 5.3): every command both frontends
//! offer, with its name, what it does and its default keys in the window
//! and in the terminal UI.
//!
//! The window defines one GPUI action per command (`familychat::<id>`),
//! binds it in its keymap files (`keymaps/default.json`, `macos.json`) to
//! the keys listed here, and builds its command palette from this table;
//! the terminal UI (Phase 10) builds its command bar and `:` palette from
//! it. A command without a key in either frontend fails the tests here and
//! in `familychat-ui`, so the two cannot drift apart.
//!
//! GUI keys use GPUI's keystroke syntax: `secondary-` is Cmd on macOS and
//! Ctrl elsewhere, so one keymap serves every platform. TUI keys are the
//! terminal UI's names (`q`, `:`, `esc`, `ctrl-v`).

/// Where a command makes sense, so a frontend offers it only there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandScope {
    /// Anywhere.
    Global,
    /// While nobody is signed in, or a sign-in is being added.
    SignIn,
    /// While the rooms and a timeline show.
    Rooms,
}

/// One command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// The stable id: the GPUI action's name after `familychat::`, and the
    /// terminal UI's command name.
    pub id: &'static str,
    /// What menus and the palette call it.
    pub name: &'static str,
    /// One sentence on what it does (the palette and the cheat sheet).
    pub description: &'static str,
    /// Its default keys in the window, in GPUI syntax, first one shown.
    pub gui_keys: &'static [&'static str],
    /// Its default key in the terminal UI.
    pub tui_key: &'static str,
    /// Where it makes sense.
    pub scope: CommandScope,
    /// An icon for the palette (a Lucide name in `assets/icons`).
    pub icon: &'static str,
}

/// Every command, in the order the palette lists them.
pub const COMMANDS: &[Command] = &[
    Command {
        id: "ToggleCommandPalette",
        name: "Command palette",
        description: "Search every command and run one.",
        gui_keys: &["secondary-k"],
        tui_key: ":",
        scope: CommandScope::Global,
        icon: "search",
    },
    Command {
        id: "SignInWithCode",
        name: "Sign in with a code from your parent",
        description: "Paste the sign-in link, or paste or drop the QR code image.",
        gui_keys: &["secondary-1"],
        tui_key: "c",
        scope: CommandScope::SignIn,
        icon: "qr-code",
    },
    Command {
        id: "SignInWithPassword",
        name: "Sign in with your password",
        description: "Sign in to your family's Family Chat server with your password.",
        gui_keys: &["secondary-2"],
        tui_key: "p",
        scope: CommandScope::SignIn,
        icon: "key-round",
    },
    Command {
        id: "UseAnotherServer",
        name: "Use another Matrix server",
        description: "Sign in to any Matrix homeserver with a password.",
        gui_keys: &["secondary-3"],
        tui_key: "o",
        scope: CommandScope::SignIn,
        icon: "globe",
    },
    Command {
        id: "PasteSignInCode",
        name: "Paste a sign-in link or QR code",
        description: "Use the link or the QR code image on the clipboard to sign in.",
        gui_keys: &["secondary-shift-v"],
        tui_key: "ctrl-v",
        scope: CommandScope::SignIn,
        icon: "clipboard-paste",
    },
    Command {
        id: "GoBack",
        name: "Back",
        description: "Leave this step and go back.",
        gui_keys: &["escape"],
        tui_key: "esc",
        scope: CommandScope::SignIn,
        icon: "arrow-left",
    },
    Command {
        id: "NextRoom",
        name: "Next room",
        description: "Open the room below this one in the room list.",
        gui_keys: &["alt-down"],
        tui_key: "ctrl-n",
        scope: CommandScope::Rooms,
        icon: "arrow-down",
    },
    Command {
        id: "PreviousRoom",
        name: "Previous room",
        description: "Open the room above this one in the room list.",
        gui_keys: &["alt-up"],
        tui_key: "ctrl-p",
        scope: CommandScope::Rooms,
        icon: "arrow-up",
    },
    Command {
        id: "NextUnreadRoom",
        name: "Next unread room",
        description: "Open the next room below with unread messages.",
        gui_keys: &["alt-shift-down"],
        tui_key: "n",
        scope: CommandScope::Rooms,
        icon: "chevrons-down",
    },
    Command {
        id: "PreviousUnreadRoom",
        name: "Previous unread room",
        description: "Open the next room above with unread messages.",
        gui_keys: &["alt-shift-up"],
        tui_key: "N",
        scope: CommandScope::Rooms,
        icon: "chevrons-up",
    },
    Command {
        id: "JumpToOldestUnread",
        name: "Jump to the oldest unread message",
        description: "Scroll the timeline to where the new messages start.",
        gui_keys: &["shift-pageup"],
        tui_key: "u",
        scope: CommandScope::Rooms,
        icon: "arrow-up-to-line",
    },
    Command {
        id: "JumpToLatest",
        name: "Jump to the latest message",
        description: "Scroll the timeline to the newest message.",
        gui_keys: &["secondary-end"],
        tui_key: "G",
        scope: CommandScope::Rooms,
        icon: "arrow-down-to-line",
    },
    Command {
        id: "ToggleMemberList",
        name: "Show or hide the members",
        description: "Show who is in this room, or hide the list.",
        gui_keys: &["secondary-."],
        tui_key: "m",
        scope: CommandScope::Rooms,
        icon: "users",
    },
    Command {
        id: "ShowAllRooms",
        name: "Show all rooms",
        description: "List every room and invitation.",
        gui_keys: &["secondary-alt-1"],
        tui_key: "1",
        scope: CommandScope::Rooms,
        icon: "messages-square",
    },
    Command {
        id: "ShowUnreadRooms",
        name: "Show unread rooms",
        description: "List only the rooms with unread messages.",
        gui_keys: &["secondary-alt-2"],
        tui_key: "2",
        scope: CommandScope::Rooms,
        icon: "message-circle-more",
    },
    Command {
        id: "ShowPeople",
        name: "Show people",
        description: "List only your direct chats.",
        gui_keys: &["secondary-alt-3"],
        tui_key: "3",
        scope: CommandScope::Rooms,
        icon: "user",
    },
    Command {
        id: "ShowInvites",
        name: "Show invitations",
        description: "List only the rooms you are invited to.",
        gui_keys: &["secondary-alt-4"],
        tui_key: "4",
        scope: CommandScope::Rooms,
        icon: "mail-plus",
    },
    Command {
        id: "NotificationSettings",
        name: "Notification settings",
        description: "Choose whether notifications show what was written.",
        gui_keys: &["secondary-alt-n"],
        tui_key: "ctrl-o",
        scope: CommandScope::Global,
        icon: "settings",
    },
    Command {
        id: "ConfirmIdentity",
        name: "Confirm it's you",
        description: "Confirm this device with another device or your recovery key.",
        gui_keys: &["secondary-alt-v"],
        tui_key: "V",
        scope: CommandScope::Rooms,
        icon: "shield-check",
    },
    Command {
        id: "SetUpRecovery",
        name: "Set up recovery",
        description: "Make a recovery key, so a new device can read your messages.",
        gui_keys: &["secondary-alt-r"],
        tui_key: "ctrl-r",
        scope: CommandScope::Rooms,
        icon: "key-round",
    },
    Command {
        id: "ManageDevices",
        name: "Your devices",
        description: "See the devices signed in to your account, verify them or sign them out.",
        gui_keys: &["secondary-shift-d"],
        tui_key: "ctrl-d",
        scope: CommandScope::Rooms,
        icon: "laptop",
    },
    Command {
        id: "ExportRoomKeys",
        name: "Export room keys",
        description: "Save the keys to your encrypted messages in a file with a passphrase.",
        gui_keys: &["secondary-alt-e"],
        tui_key: "X",
        scope: CommandScope::Rooms,
        icon: "download",
    },
    Command {
        id: "ImportRoomKeys",
        name: "Import room keys",
        description: "Read keys to encrypted messages from a file exported earlier.",
        gui_keys: &["secondary-alt-i"],
        tui_key: "I",
        scope: CommandScope::Rooms,
        icon: "upload",
    },
    Command {
        id: "FocusComposer",
        name: "Write a message",
        description: "Move to the message field of the open room.",
        gui_keys: &["secondary-l"],
        tui_key: "i",
        scope: CommandScope::Rooms,
        icon: "send",
    },
    Command {
        id: "AttachFile",
        name: "Attach a file",
        description: "Pick files to send with your next message. You can also paste or drop them.",
        gui_keys: &["secondary-shift-a"],
        tui_key: "A",
        scope: CommandScope::Rooms,
        icon: "paperclip",
    },
    Command {
        id: "SelectPreviousMessage",
        name: "Select the previous message",
        description: "Move to the message above and show what you can do with it.",
        gui_keys: &["secondary-alt-up"],
        tui_key: "[",
        scope: CommandScope::Rooms,
        icon: "arrow-up",
    },
    Command {
        id: "SelectNextMessage",
        name: "Select the next message",
        description: "Move to the message below and show what you can do with it.",
        gui_keys: &["secondary-alt-down"],
        tui_key: "]",
        scope: CommandScope::Rooms,
        icon: "arrow-down",
    },
    Command {
        id: "ReplyToMessage",
        name: "Reply",
        description: "Reply to the selected message, or the newest one.",
        gui_keys: &["secondary-shift-r"],
        tui_key: "R",
        scope: CommandScope::Rooms,
        icon: "reply",
    },
    Command {
        id: "EditMessage",
        name: "Edit your message",
        description: "Change the selected message, or your newest one.",
        gui_keys: &["secondary-shift-e"],
        tui_key: "E",
        scope: CommandScope::Rooms,
        icon: "pencil",
    },
    Command {
        id: "ReactToMessage",
        name: "React",
        description: "Pick a reaction for the selected message, or the newest one.",
        gui_keys: &["secondary-shift-l"],
        tui_key: "+",
        scope: CommandScope::Rooms,
        icon: "face-slightly-smiling-plus",
    },
    Command {
        id: "DeleteMessage",
        name: "Delete your message",
        description: "Delete the selected message, after asking. It goes for everyone in the room.",
        gui_keys: &["secondary-shift-backspace"],
        tui_key: "D",
        scope: CommandScope::Rooms,
        icon: "trash-2",
    },
    Command {
        id: "LeaveRoom",
        name: "Leave the room",
        description: "Leave the open room, after asking.",
        gui_keys: &["secondary-shift-w"],
        tui_key: "L",
        scope: CommandScope::Rooms,
        icon: "door-open",
    },
    Command {
        id: "Quit",
        name: "Quit Family Chat",
        description: "Close every window and quit.",
        gui_keys: &["secondary-q"],
        tui_key: "q",
        scope: CommandScope::Global,
        icon: "log-out",
    },
];

/// The command with this id.
pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.id == id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_command_has_a_key_in_both_frontends() {
        for command in COMMANDS {
            assert!(
                !command.gui_keys.is_empty() && command.gui_keys.iter().all(|key| !key.is_empty()),
                "{} has no window key",
                command.id
            );
            assert!(!command.tui_key.is_empty(), "{} has no TUI key", command.id);
            assert!(!command.name.is_empty() && !command.description.is_empty());
            assert!(!command.icon.is_empty());
        }
    }

    #[test]
    fn ids_and_keys_are_unique() {
        let ids: BTreeSet<_> = COMMANDS.iter().map(|command| command.id).collect();
        assert_eq!(ids.len(), COMMANDS.len(), "two commands share an id");
        let tui: BTreeSet<_> = COMMANDS.iter().map(|command| command.tui_key).collect();
        assert_eq!(tui.len(), COMMANDS.len(), "two commands share a TUI key");
        let gui: BTreeSet<_> = COMMANDS
            .iter()
            .flat_map(|command| command.gui_keys.iter())
            .collect();
        assert_eq!(
            gui.len(),
            COMMANDS
                .iter()
                .map(|command| command.gui_keys.len())
                .sum::<usize>(),
            "two commands share a window key"
        );
        assert!(ids.iter().all(|id| id.chars().all(char::is_alphanumeric)));
    }

    #[test]
    fn commands_are_found_by_id() {
        assert_eq!(command("Quit").map(|command| command.tui_key), Some("q"));
        assert!(command("Nope").is_none());
    }
}
