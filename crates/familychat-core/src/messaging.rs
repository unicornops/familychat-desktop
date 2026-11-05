//! What a frontend asks the session to do in a room (PLAN.md, sections 5.3
//! and 13.7; ADR 0012): send, reply, edit, react, redact, read markers,
//! typing, files, invitations, new rooms, and the drafts kept between
//! visits.
//!
//! These are the session's primitives. Frontends never build them by
//! hand: `familychat-actions` turns what the person wrote into an
//! [`OutgoingMessage`] (Markdown, mentions, the sanitiser's check), checks
//! a file against the server's [`MediaLimits`] and takes its location out
//! before it becomes an [`OutgoingFile`], decides whether a new room is
//! encrypted from the [`ServerDefaults`], and refuses everything while the
//! account is paused. The session only carries them out, through the SDK's
//! send queue where there is one.
//!
//! Message text and file contents are held in types whose `Debug` prints
//! sizes only, so a request that reaches a log carries no message.

use std::{fmt, sync::Arc};

use ruma::{OwnedEventId, OwnedUserId};

use crate::timeline::{MessageBody, TimelineItemId};

/// Someone a message mentions: who, and the text that names them in the
/// message (their display name when the composer inserted it).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Mention {
    /// Their user id.
    pub user_id: OwnedUserId,
    /// The text the message names them with.
    pub label: String,
}

impl fmt::Debug for Mention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mention({} bytes)", self.label.len())
    }
}

/// A message ready to send: its plain body, its formatted body (already
/// through Family Chat's sanitiser), and whom it mentions (`m.mentions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    /// The plain text every client can show.
    pub body: MessageBody,
    /// `org.matrix.custom.html`, when the text has formatting or mentions.
    pub html: Option<MessageBody>,
    /// The people it mentions, by user id.
    pub mentions: Vec<OwnedUserId>,
}

impl OutgoingMessage {
    /// A plain message with no formatting (tests and fixtures).
    pub fn plain(text: &str) -> Self {
        Self {
            body: MessageBody::new(text),
            html: None,
            mentions: Vec::new(),
        }
    }
}

/// The message an action is about: its event id once the server has it,
/// and its timeline item (which also names a message still being sent).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef {
    /// The timeline item.
    pub item: TimelineItemId,
    /// The event id, when it has been sent.
    pub event_id: Option<OwnedEventId>,
}

/// A file ready to send: checked against the server's upload limit and,
/// for a picture, without its location. `Debug` prints sizes only.
#[derive(Clone, PartialEq, Eq)]
pub struct OutgoingFile {
    /// The name it is sent under.
    pub file_name: String,
    /// Its MIME type.
    pub mime_type: String,
    /// Its bytes.
    pub data: Arc<[u8]>,
    /// For a picture: its width and height, when they could be read.
    pub image_size: Option<(u32, u32)>,
}

impl fmt::Debug for OutgoingFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutgoingFile")
            .field("mime_type", &self.mime_type)
            .field("bytes", &self.data.len())
            .field("image_size", &self.image_size)
            .finish_non_exhaustive()
    }
}

/// What to do in a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomRequest {
    /// Send a message, as a reply to `reply_to` when it is set.
    Send {
        /// The message.
        message: OutgoingMessage,
        /// The message it answers.
        reply_to: Option<OwnedEventId>,
    },
    /// Replace one of the account's messages with new text.
    Edit {
        /// The message.
        target: MessageRef,
        /// Its new text.
        message: OutgoingMessage,
    },
    /// Add the account's reaction `key`, or take it back if it is there.
    ToggleReaction {
        /// The message.
        target: MessageRef,
        /// The reaction, usually one emoji.
        key: String,
    },
    /// Delete a message (the account's own; a message still being sent is
    /// cancelled).
    Redact {
        /// The message.
        target: MessageRef,
        /// Why, when the person said.
        reason: Option<String>,
    },
    /// Move the read receipt and the read marker to the newest message.
    MarkRead,
    /// Tell the room the account is typing, or has stopped.
    Typing(bool),
    /// Send a file, as a reply to `reply_to` when it is set.
    SendFile {
        /// The file.
        file: OutgoingFile,
        /// The message it answers.
        reply_to: Option<OwnedEventId>,
    },
    /// Invite someone.
    Invite(OwnedUserId),
    /// Accept (`true`) or decline an invitation to this room.
    AnswerInvite(bool),
    /// Leave the room.
    Leave,
}

/// How a [`RoomRequest`] went, when it went well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomDone {
    /// Done, or queued to be sent (the timeline shows it at once).
    Done,
    /// A reaction was added (`true`) or taken back.
    Reaction(bool),
}

/// A new room or direct chat.
#[derive(Clone, PartialEq, Eq)]
pub struct NewRoom {
    /// Its name; `None` for a direct chat, named after its members.
    pub name: Option<String>,
    /// Who to invite.
    pub invite: Vec<OwnedUserId>,
    /// A direct chat (`is_direct`, the "People" filter).
    pub direct: bool,
    /// End-to-end encrypted from the start.
    pub encrypted: bool,
}

impl fmt::Debug for NewRoom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewRoom")
            .field("invite", &self.invite.len())
            .field("direct", &self.direct)
            .field("encrypted", &self.encrypted)
            .finish_non_exhaustive()
    }
}

/// What the server accepts as uploads (`/_matrix/client/v1/media/config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaLimits {
    /// The largest upload, in bytes, when the server says (Family Chat's
    /// Palpo allows 11 MiB).
    pub max_upload_bytes: Option<u64>,
}

/// What the account's server says about new rooms in its
/// `.well-known/matrix/client`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServerDefaults {
    /// `io.element.e2ee.default`: `Some(false)` is how a server says new
    /// rooms are not encrypted there; `None` when it says nothing (or its
    /// well-known can't be read).
    pub encryption_default: Option<bool>,
}

/// What the person was writing in a room, kept between visits (encrypted
/// in the app store, ADR 0003).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Draft {
    /// The text.
    pub text: MessageBody,
    /// The mentions the composer inserted into it.
    pub mentions: Vec<Mention>,
    /// The message it answers.
    pub reply_to: Option<OwnedEventId>,
    /// The message it replaces, while editing.
    pub editing: Option<OwnedEventId>,
}

impl Draft {
    /// Whether there is nothing to keep.
    pub fn is_empty(&self) -> bool {
        self.text.as_str().trim().is_empty() && self.reply_to.is_none() && self.editing.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_no_text_or_bytes() {
        let message = OutgoingMessage {
            body: MessageBody::new("the secret plan"),
            html: Some(MessageBody::new("<b>the secret plan</b>")),
            mentions: Vec::new(),
        };
        let file = OutgoingFile {
            file_name: "secret-plan.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            data: Arc::from(b"secret".as_slice()),
            image_size: None,
        };
        let mention = Mention {
            user_id: OwnedUserId::try_from("@ana:smith.safechat.family").expect("an id"),
            label: "Ana Secret".to_owned(),
        };
        let printed = format!("{message:?} {file:?} {mention:?}");
        assert!(!printed.contains("secret"), "{printed}");
        assert!(!printed.contains("Secret"), "{printed}");
    }

    #[test]
    fn a_draft_with_only_spaces_is_empty_unless_it_replies_or_edits() {
        let mut draft = Draft {
            text: MessageBody::new("  \n"),
            ..Draft::default()
        };
        assert!(draft.is_empty());
        draft.reply_to = Some(OwnedEventId::try_from("$a:b.c").expect("an id"));
        assert!(!draft.is_empty());
    }
}
