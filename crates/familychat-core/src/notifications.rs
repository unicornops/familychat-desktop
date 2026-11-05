//! New-message notifications as the session reports them (PLAN.md,
//! section 5.9; ADR 0013).
//!
//! The session registers matrix-sdk's notification handler on every
//! account (push rules evaluated, the event decrypted where it can be)
//! and turns each notification into an [`IncomingNotification`]: the
//! account, the room, the names to show and what the message is. It
//! arrives on [`SessionApi::notifications`](crate::session::SessionApi)'s
//! receiver; `familychat-notify` composes what the OS shows from it
//! (coalescing, the flood cap, hidden content), and a frontend posts it.
//!
//! Nothing here prints a name or a message: `Debug` shows only what kind
//! of message it is, so a notification can't reach a log by accident.

use std::fmt;

use ruma::OwnedRoomId;

use crate::session::AccountKey;

/// What a notification's message is. The text variants hold the message's
/// plain text; the rest are described in words by `familychat-notify`.
#[derive(Clone, PartialEq, Eq)]
pub enum NotificationContent {
    /// A text message or a notice: its plain body.
    Text(String),
    /// An emote (`/me waves`): its plain body, without the sender.
    Emote(String),
    /// A picture.
    Image,
    /// A video.
    Video,
    /// An audio clip or a voice message.
    Audio,
    /// A file.
    File,
    /// A shared location.
    Location,
    /// A sticker.
    Sticker,
    /// An invitation to the room.
    Invite,
    /// Something the session can't describe (a message it could not
    /// decrypt, a call, a poll): shown as "New message".
    Unknown,
}

impl NotificationContent {
    /// The kind, without the text.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Emote(_) => "emote",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::File => "file",
            Self::Location => "location",
            Self::Sticker => "sticker",
            Self::Invite => "invite",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Debug for NotificationContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind())
    }
}

/// One notification from an account's sync.
#[derive(Clone, PartialEq, Eq)]
pub struct IncomingNotification {
    /// The account it is for.
    pub account: AccountKey,
    /// The room the message is in (and the room a click opens).
    pub room_id: OwnedRoomId,
    /// The room's name, as the room list shows it.
    pub room_name: String,
    /// Who sent it, by display name (or user id).
    pub sender_name: String,
    /// A direct chat: the notification names the sender, not the room.
    pub is_direct: bool,
    /// What it is.
    pub content: NotificationContent,
}

impl fmt::Debug for IncomingNotification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingNotification")
            .field("account", &self.account)
            .field("is_direct", &self.is_direct)
            .field("content", &self.content)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_no_names_or_text() {
        let incoming = IncomingNotification {
            account: AccountKey("account-1".to_owned()),
            room_id: "!secret-room:example.org"
                .try_into()
                .expect("a valid room id"),
            room_name: "Secret room name".to_owned(),
            sender_name: "Secret sender".to_owned(),
            is_direct: false,
            content: NotificationContent::Text("secret text".to_owned()),
        };
        let debug = format!("{incoming:?} {incoming:#?}");
        assert!(!debug.contains("ecret"), "{debug}");
        assert!(debug.contains("text"), "{debug}");
        assert_eq!(
            format!("{:?}", NotificationContent::Emote("secret".to_owned())),
            "emote"
        );
    }
}
