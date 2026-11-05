//! A room's timeline as both frontends draw it (PLAN.md, section 5.6):
//! messages and the events around them, date dividers, the read marker and
//! the start of the room.
//!
//! Message text is held in [`MessageBody`], whose `Debug` prints only its
//! length, so a timeline item that reaches a log by mistake carries no
//! message (PLAN.md, section 5.4).

use std::fmt;

use ruma::{OwnedEventId, OwnedUserId};

use crate::diff::ListDiff;

/// A timeline item's id: stable while the item is in the timeline, also
/// for a message being sent that has no event id yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimelineItemId(pub String);

/// A message's text. `Debug` prints its length, never the text.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageBody(String);

impl MessageBody {
    /// Wraps a message's text.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text, to draw. Never log it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MessageBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageBody({} bytes)", self.0.len())
    }
}

/// What kind of message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Text.
    Text,
    /// A notice (from a bot).
    Notice,
    /// An emote (`/me`).
    Emote,
    /// An image; the body is its caption or file name.
    Image,
    /// A video.
    Video,
    /// A voice or audio message.
    Audio,
    /// A file.
    File,
    /// A location.
    Location,
    /// Any other message type.
    Other,
}

/// What a timeline event shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineContent {
    /// A message.
    Message {
        /// Its kind.
        kind: MessageKind,
        /// Its text (or a media item's caption or file name).
        body: MessageBody,
        /// It has been edited.
        edited: bool,
    },
    /// A sticker.
    Sticker {
        /// Its description.
        body: MessageBody,
    },
    /// A message that was deleted.
    Redacted,
    /// An encrypted message this device can't read (yet).
    UnableToDecrypt,
    /// Someone joined, left, was invited, removed or banned.
    Membership {
        /// Whose membership changed.
        user_id: OwnedUserId,
    },
    /// Someone changed their display name or avatar.
    ProfileChange {
        /// Whose profile changed.
        user_id: OwnedUserId,
    },
    /// Another room state change (name, topic, encryption, ...).
    State {
        /// The event type.
        event_type: String,
    },
    /// An event this app does not show yet (polls, calls, unparsable
    /// events).
    Other {
        /// The event type, when known.
        event_type: Option<String>,
    },
}

/// Where a message being sent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendState {
    /// Waiting to be sent, or being sent.
    Sending,
    /// Sending failed.
    Failed {
        /// The send queue will try again on its own.
        recoverable: bool,
    },
    /// The server has it; the event id arrives with it.
    Sent,
}

/// An event in the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    /// The event id; `None` for a local echo not yet sent.
    pub event_id: Option<OwnedEventId>,
    /// Who sent it.
    pub sender: OwnedUserId,
    /// The sender's display name, when known.
    pub sender_name: Option<String>,
    /// When it was sent, in milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// The account's own event.
    pub is_own: bool,
    /// What it shows.
    pub content: TimelineContent,
    /// For the account's own messages being sent: where they are.
    pub send_state: Option<SendState>,
}

/// What a timeline item is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineItemKind {
    /// An event.
    Event(TimelineEvent),
    /// A date divider: the day of the events after it.
    DateDivider {
        /// A time on that day, in milliseconds since the Unix epoch.
        timestamp_ms: u64,
    },
    /// The user's read marker: what follows is new.
    ReadMarker,
    /// The start of the room: there is nothing older.
    TimelineStart,
}

/// One item of a room's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineItem {
    /// The item's id.
    pub id: TimelineItemId,
    /// What it is.
    pub kind: TimelineItemKind,
}

/// Loading older messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationState {
    /// Not loading.
    Idle,
    /// Loading older messages.
    Loading,
    /// The start of the room is loaded; there is nothing older.
    ReachedStart,
    /// Loading failed; asking again may work.
    Failed {
        /// What went wrong, for the user.
        message: String,
    },
}

/// What an open timeline sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineUpdate {
    /// Changes to the items, in order. The first update is a
    /// [`ListDiff::Reset`] with the items the timeline starts with.
    Items(Vec<ListDiff<TimelineItem>>),
    /// Loading older messages moved on.
    Pagination(PaginationState),
}

/// What a frontend asks of an open timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineCommand {
    /// Load up to this many older events.
    PaginateBackwards {
        /// How many events.
        events: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_body_never_prints() {
        let content = TimelineContent::Message {
            kind: MessageKind::Text,
            body: MessageBody::new("the secret plan"),
            edited: false,
        };
        let debug = format!("{content:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(debug.contains("15 bytes"), "{debug}");
        let TimelineContent::Message { body, .. } = content else {
            panic!("a message");
        };
        assert_eq!(body.as_str(), "the secret plan");
    }
}
