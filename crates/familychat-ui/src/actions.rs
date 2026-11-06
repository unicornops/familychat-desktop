//! Every command the window offers, as a GPUI action: one per entry of the
//! commands table (`familychat_core::commands`), named `familychat::<id>`
//! as the keymap files write them, plus the macOS window conventions.
//!
//! Buttons dispatch these actions instead of calling view methods, and the
//! keymap binds keys to them, so a command behaves the same however it is
//! triggered (Zmail's pattern).
//!
//! Contexts, from the outside in (deeper contexts win when two bindings
//! share a keystroke):
//!
//! | Context | Element |
//! |---|---|
//! | `Shell` | the window root |
//! | `Signin` | the sign-in screens |
//! | `Rooms` | the room list, the open room and its timeline |
//! | `Composer` | the open room's message field and its files (`showing_mentions` while the mention list shows) |
//! | `CommandPalette` | the command palette's search field and results |
//! | `Modal`, `Menu` | an open dialog or menu (the components' own keys) |
//! | `TextInput` | any text field (owned by the text input component) |
//!
//! Quit, Settings, Hide, Hide Others and Minimize have no context:
//! [`register`] handles them app-wide, so they work from the Settings
//! window and from the macOS menu bar even when no Family Chat element has
//! focus.

use gpui::{App, actions};

actions!(
    familychat,
    [
        /// Open the command palette, or close it if it is open.
        ToggleCommandPalette,
        /// "Sign in with a code from your parent".
        SignInWithCode,
        /// "Sign in with your password".
        SignInWithPassword,
        /// "Use another Matrix server".
        UseAnotherServer,
        /// Sign in with the link or QR image on the clipboard.
        PasteSignInCode,
        /// Leave the current step.
        GoBack,
        /// Open the room below in the room list.
        NextRoom,
        /// Open the room above in the room list.
        PreviousRoom,
        /// Open the next unread room below.
        NextUnreadRoom,
        /// Open the next unread room above.
        PreviousUnreadRoom,
        /// Scroll the timeline to the read marker.
        JumpToOldestUnread,
        /// Scroll the timeline to the newest message.
        JumpToLatest,
        /// Show or hide the member list.
        ToggleMemberList,
        /// List every room.
        ShowAllRooms,
        /// List the unread rooms.
        ShowUnreadRooms,
        /// List the direct chats.
        ShowPeople,
        /// List the invitations.
        ShowInvites,
        /// Open Settings.
        OpenSettings,
        /// Open Settings at Notifications.
        NotificationSettings,
        /// "Confirm it's you": verify this device (ADR 0009).
        ConfirmIdentity,
        /// Set recovery up and show the recovery key once.
        SetUpRecovery,
        /// Show the account's devices.
        ManageDevices,
        /// Show a QR code for a new device to sign in with (ADR 0010).
        LinkDevice,
        /// Save the room keys to a passphrase-protected file.
        ExportRoomKeys,
        /// Read room keys from such a file.
        ImportRoomKeys,
        /// Move to the message field.
        FocusComposer,
        /// Pick files to send.
        AttachFile,
        /// Select the message above.
        SelectPreviousMessage,
        /// Select the message below.
        SelectNextMessage,
        /// Reply to the selected (or newest) message.
        ReplyToMessage,
        /// Edit the selected (or newest own) message.
        EditMessage,
        /// React to the selected (or newest) message.
        ReactToMessage,
        /// Delete the selected own message, after asking.
        DeleteMessage,
        /// Leave the open room, after asking.
        LeaveRoom,
        /// Quit Family Chat.
        Quit,
        /// Hide Family Chat (macOS).
        Hide,
        /// Hide every other application (macOS).
        HideOthers,
        /// Minimise the active window (macOS).
        Minimize,
    ]
);

/// Actions outside the commands table, with why: the macOS window
/// conventions, which the terminal UI has no use for.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const PLATFORM_ACTIONS: &[(&str, &str)] = &[
    ("Hide", "macOS only (Cmd+H)"),
    ("HideOthers", "macOS only (Alt+Cmd+H)"),
    ("Minimize", "macOS only (Cmd+M)"),
];

/// Handles the app-wide actions. Call it once, before the first window.
pub(crate) fn register(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    // Settings opens from any window, and from the macOS menu bar with
    // no window focused (the main window's own handler closes its palette
    // first).
    cx.on_action(|_: &OpenSettings, cx| crate::settings::open(None, cx));
    cx.on_action(|_: &NotificationSettings, cx| {
        crate::settings::open(Some(crate::settings::SettingsSection::Notifications), cx);
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
}
