//! A room's timeline as both frontends draw it (PLAN.md, section 5.6):
//! messages and the events around them, date dividers, the read marker and
//! the start of the room.
//!
//! Message text is held in [`MessageBody`] and [`RichText`], whose `Debug`
//! prints only sizes, so a timeline item that reaches a log by mistake
//! carries no message (PLAN.md, section 5.4).
//!
//! [`rows`] groups the items the way both frontends draw them: runs of
//! membership and room changes fold into one summary row, and a message
//! that follows another from the same sender within a few minutes
//! continues its group. The sentences for state changes ([`describe`]),
//! day dividers ([`day_label`]) and times ([`time_label`]) live here too,
//! so the window and the terminal UI say the same.

use std::{
    collections::hash_map::DefaultHasher,
    fmt,
    hash::{Hash, Hasher},
    ops::Range,
};

use chrono::{DateTime, Datelike, Local, TimeZone};
use ruma::{OwnedEventId, OwnedUserId};

pub use crate::encryption::UtdReason;
use crate::{diff::ListDiff, media::MediaInfo, rich_text::RichText};

/// A timeline item's id: stable while the item is in the timeline, also
/// for a message being sent that has no event id yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimelineItemId(pub String);

/// A message's text. `Debug` prints its length, never the text.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// How someone's membership of a room changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MembershipChange {
    /// Joined.
    Joined,
    /// Left.
    Left,
    /// Was invited.
    Invited,
    /// Accepted an invitation.
    InvitationAccepted,
    /// Declined an invitation.
    InvitationRejected,
    /// Had an invitation withdrawn.
    InvitationRevoked,
    /// Was removed.
    Kicked,
    /// Was banned (or removed and banned).
    Banned,
    /// Was unbanned.
    Unbanned,
    /// Asked to join.
    Knocked,
    /// Was let in after asking.
    KnockAccepted,
    /// Withdrew a request to join.
    KnockRetracted,
    /// Was refused after asking.
    KnockDenied,
    /// Anything else (no change, or a change the SDK doesn't name).
    Other,
}

/// A room state change the timeline describes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StateChange {
    /// The room was created.
    Created,
    /// The room's name changed (to `None`: removed).
    Name(Option<String>),
    /// The room's topic changed.
    Topic(Option<String>),
    /// The room's picture changed.
    Avatar,
    /// End-to-end encryption was turned on.
    Encryption,
    /// Who can join changed.
    JoinRules,
    /// Who can read the history changed.
    HistoryVisibility,
    /// Roles and permissions changed.
    PowerLevels,
    /// The pinned messages changed.
    PinnedEvents,
    /// The room was replaced by a newer one.
    Tombstone,
    /// Anything else (the event type says what).
    Other,
}

/// What a timeline event shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TimelineContent {
    /// A message.
    Message {
        /// Its kind.
        kind: MessageKind,
        /// Its plain text (or a media item's caption or file name): what
        /// a screen reader and the room list read.
        body: MessageBody,
        /// Its text to draw: the sanitised formatted body, or the plain
        /// body with its links (ADR 0007).
        rich: RichText,
        /// It has been edited.
        edited: bool,
        /// For an image, video, audio clip or file: the media.
        media: Option<MediaInfo>,
    },
    /// A sticker.
    Sticker {
        /// Its description.
        body: MessageBody,
    },
    /// A message that was deleted.
    Redacted,
    /// An encrypted message this device can't read (yet), and why.
    UnableToDecrypt(UtdReason),
    /// Someone joined, left, was invited, removed or banned.
    Membership {
        /// Whose membership changed.
        user_id: OwnedUserId,
        /// Their display name, when known.
        display_name: Option<String>,
        /// How it changed.
        change: MembershipChange,
    },
    /// Someone changed their display name or avatar.
    ProfileChange {
        /// Whose profile changed.
        user_id: OwnedUserId,
        /// The display name before, when it changed.
        old_name: Option<String>,
        /// The display name after, when it changed.
        new_name: Option<String>,
        /// Whether the name changed (the names may both be `None`).
        name_changed: bool,
        /// Whether the avatar changed.
        avatar_changed: bool,
    },
    /// Another room state change (name, topic, encryption, ...).
    State {
        /// The event type.
        event_type: String,
        /// What changed.
        change: StateChange,
    },
    /// An event this app does not show yet (polls, calls, unparsable
    /// events).
    Other {
        /// The event type, when known.
        event_type: Option<String>,
    },
}

impl TimelineContent {
    /// A text message with `text` as its plain body (fixtures and tests).
    pub fn text(text: &str) -> Self {
        Self::Message {
            kind: MessageKind::Text,
            body: MessageBody::new(text),
            rich: RichText::plain(text),
            edited: false,
            media: None,
        }
    }

    /// Whether it is a membership, profile or room change: what the
    /// timeline folds into summaries.
    pub fn is_state(&self) -> bool {
        matches!(
            self,
            Self::Membership { .. } | Self::ProfileChange { .. } | Self::State { .. }
        )
    }

    /// Whether it is a message that starts or continues a sender's group.
    fn groups_with_sender(&self) -> bool {
        matches!(
            self,
            Self::Message { .. } | Self::Sticker { .. } | Self::Redacted | Self::UnableToDecrypt(_)
        )
    }
}

/// The message a reply answers, as far as the timeline knows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplyPreview {
    /// The answered event.
    pub event_id: OwnedEventId,
    /// Who sent it: their name, or their user id (`None` while it loads).
    pub sender: Option<String>,
    /// One line of it (`None` while it loads, or when it can't be had).
    pub text: Option<MessageBody>,
}

/// One reaction key on a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reaction {
    /// The key: usually one emoji.
    pub key: String,
    /// How many people reacted with it.
    pub count: usize,
    /// The account is one of them.
    pub own: bool,
    /// Who they are, by name or user id, for the tooltip.
    pub senders: Vec<String>,
}

/// Where a message being sent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendState {
    /// Waiting to be sent, or being sent.
    Sending,
    /// Sending failed.
    Failed {
        /// The send queue will try again on its own.
        recoverable: bool,
        /// Why it stopped, when the encryption stopped it.
        problem: Option<SendProblem>,
    },
    /// The server has it; the event id arrives with it.
    Sent,
}

/// Why the encryption stopped a message going (ADR 0009: room keys go to
/// devices as Element X sends them, which stops when a verified person's
/// devices or identity need a look).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendProblem {
    /// A verified person (often the account itself) has a device they
    /// haven't confirmed; "Send anyway" sends to it too.
    UnconfirmedDevices,
    /// A verified person's identity changed; "Send anyway" withdraws the
    /// old verification and sends.
    IdentityChanged,
    /// This device has to be confirmed first.
    ConfirmThisDevice,
}

impl SendProblem {
    /// What the message says under it.
    pub fn message(self) -> &'static str {
        match self {
            Self::UnconfirmedDevices => crate::encryption::wording::UNCONFIRMED_DEVICES_NOT_SENT,
            Self::IdentityChanged => crate::encryption::wording::IDENTITY_CHANGED_NOT_SENT,
            Self::ConfirmThisDevice => crate::encryption::wording::CONFIRM_THIS_DEVICE_NOT_SENT,
        }
    }

    /// Whether "Send anyway" can resolve it.
    pub fn can_send_anyway(self) -> bool {
        !matches!(self, Self::ConfirmThisDevice)
    }
}

/// An event in the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// The message this one replies to.
    pub in_reply_to: Option<ReplyPreview>,
    /// The reactions, in the order they were first given.
    pub reactions: Vec<Reaction>,
}

impl TimelineEvent {
    /// The sender's name: their display name, or their user id.
    pub fn sender_label(&self) -> String {
        self.sender_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.sender.to_string())
    }
}

/// What a timeline item is.
// Most items are events, so boxing them would cost an allocation per item
// for the few dividers it saves.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

// ---------------------------------------------------------------------------
// Wording
// ---------------------------------------------------------------------------

/// What a membership, profile or room change says, with `who` the name of
/// the person it is about (or who made it).
pub fn describe(content: &TimelineContent, who: &str) -> Option<String> {
    Some(match content {
        TimelineContent::Membership {
            display_name,
            user_id,
            change,
        } => {
            let name = display_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| user_id.to_string());
            match change {
                MembershipChange::Joined | MembershipChange::InvitationAccepted => {
                    format!("{name} joined")
                }
                MembershipChange::Left => format!("{name} left"),
                MembershipChange::Invited => format!("{who} invited {name}"),
                MembershipChange::InvitationRejected => {
                    format!("{name} declined the invitation")
                }
                MembershipChange::InvitationRevoked => {
                    format!("{who} withdrew {name}'s invitation")
                }
                MembershipChange::Kicked => format!("{who} removed {name}"),
                MembershipChange::Banned => format!("{who} banned {name}"),
                MembershipChange::Unbanned => format!("{who} unbanned {name}"),
                MembershipChange::Knocked => format!("{name} asked to join"),
                MembershipChange::KnockAccepted => format!("{who} let {name} in"),
                MembershipChange::KnockRetracted => {
                    format!("{name} withdrew their request to join")
                }
                MembershipChange::KnockDenied => {
                    format!("{who} turned down {name}'s request to join")
                }
                MembershipChange::Other => format!("{name}'s membership changed"),
            }
        }
        TimelineContent::ProfileChange {
            old_name,
            new_name,
            name_changed,
            avatar_changed,
            ..
        } => match (name_changed, new_name, old_name) {
            (true, Some(new), Some(old)) => format!("{old} changed their name to {new}"),
            (true, Some(new), None) => format!("{who} set their name to {new}"),
            (true, None, Some(old)) => format!("{old} removed their name"),
            _ if *avatar_changed => format!("{who} changed their picture"),
            _ => format!("{who} changed their profile"),
        },
        TimelineContent::State { change, .. } => match change {
            StateChange::Created => format!("{who} created the room"),
            StateChange::Name(Some(name)) => format!("{who} renamed the room to {name}"),
            StateChange::Name(None) => format!("{who} removed the room's name"),
            StateChange::Topic(Some(topic)) => format!("{who} set the topic to {topic}"),
            StateChange::Topic(None) => format!("{who} removed the topic"),
            StateChange::Avatar => format!("{who} changed the room's picture"),
            StateChange::Encryption => format!("{who} turned on end-to-end encryption"),
            StateChange::JoinRules => format!("{who} changed who can join"),
            StateChange::HistoryVisibility => {
                format!("{who} changed who can read the history")
            }
            StateChange::PowerLevels => format!("{who} changed roles and permissions"),
            StateChange::PinnedEvents => format!("{who} changed the pinned messages"),
            StateChange::Tombstone => format!("{who} replaced this room with a new one"),
            StateChange::Other => format!("{who} changed the room's settings"),
        },
        _ => return None,
    })
}

/// What a summary of `count` folded changes says when collapsed: the first
/// change, then how many more ("Ana joined and 4 more changes").
pub fn summary_label(first: &str, count: usize) -> String {
    match count {
        0 | 1 => first.to_owned(),
        2 => format!("{first} and 1 more change"),
        _ => format!("{first} and {} more changes", count - 1),
    }
}

/// The placeholder a message shows when it can't be drawn as text.
pub fn placeholder(content: &TimelineContent) -> Option<&'static str> {
    match content {
        TimelineContent::Redacted => Some("Message deleted"),
        TimelineContent::UnableToDecrypt(reason) => Some(reason.message()),
        TimelineContent::Other { .. } => Some("Family Chat can't show this kind of message yet."),
        _ => None,
    }
}

/// Local midnight of the day of `timestamp_ms`, as a date.
fn local_date(timestamp_ms: u64) -> Option<DateTime<Local>> {
    let millis = i64::try_from(timestamp_ms).ok()?;
    Local.timestamp_millis_opt(millis).single()
}

/// A day divider's label: "Today", "Yesterday", the weekday within a week
/// ("Monday"), else the date ("3 November", with the year when it is not
/// this year's).
pub fn day_label(timestamp_ms: u64, now_ms: u64) -> String {
    let (Some(day), Some(now)) = (local_date(timestamp_ms), local_date(now_ms)) else {
        return "Some time ago".to_owned();
    };
    let days = now
        .date_naive()
        .signed_duration_since(day.date_naive())
        .num_days();
    match days {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => day.format("%A").to_string(),
        _ if day.year() == now.year() => day.format("%-d %B").to_string(),
        _ => day.format("%-d %B %Y").to_string(),
    }
}

/// A message's time of day, on the 24-hour clock ("14:05").
pub fn time_label(timestamp_ms: u64) -> String {
    local_date(timestamp_ms)
        .map(|time| time.format("%H:%M").to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// How long after a sender's message their next one still continues the
/// group (no name and avatar again).
pub const GROUP_WINDOW_MS: u64 = 5 * 60 * 1000;

/// One row of a timeline as the frontends draw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineRow {
    /// An item on its own. `continues` is true for a message that
    /// continues its sender's group.
    Item {
        /// The item's index.
        index: usize,
        /// It continues the sender's group: draw it without the name.
        continues: bool,
    },
    /// Two or more consecutive membership and room changes, folded.
    StateGroup {
        /// The items' indices.
        range: Range<usize>,
    },
}

impl TimelineRow {
    /// The items the row draws.
    pub fn items(&self) -> Range<usize> {
        match self {
            Self::Item { index, .. } => *index..*index + 1,
            Self::StateGroup { range } => range.clone(),
        }
    }
}

fn event_of(item: &TimelineItem) -> Option<&TimelineEvent> {
    match &item.kind {
        TimelineItemKind::Event(event) => Some(event),
        _ => None,
    }
}

/// The rows of `items`, in order.
pub fn rows(items: &[TimelineItem]) -> Vec<TimelineRow> {
    let mut rows = Vec::with_capacity(items.len());
    let mut index = 0;
    while index < items.len() {
        let is_state = |item: &TimelineItem| event_of(item).is_some_and(|e| e.content.is_state());
        if is_state(&items[index]) {
            let start = index;
            while index < items.len() && is_state(&items[index]) {
                index += 1;
            }
            if index - start >= 2 {
                rows.push(TimelineRow::StateGroup {
                    range: start..index,
                });
            } else {
                rows.push(TimelineRow::Item {
                    index: start,
                    continues: false,
                });
            }
            continue;
        }
        let continues = index > 0
            && match (event_of(&items[index - 1]), event_of(&items[index])) {
                (Some(previous), Some(current)) => {
                    previous.content.groups_with_sender()
                        && current.content.groups_with_sender()
                        && previous.sender == current.sender
                        && current.in_reply_to.is_none()
                        && current.timestamp_ms.saturating_sub(previous.timestamp_ms)
                            <= GROUP_WINDOW_MS
                }
                _ => false,
            };
        rows.push(TimelineRow::Item { index, continues });
        index += 1;
    }
    rows
}

/// A fingerprint of a row's content: rows whose items have not changed
/// keep it, so a frontend can find the rows a batch of changes touched
/// without keeping a second copy of the items.
pub fn row_fingerprint(row: &TimelineRow, items: &[TimelineItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    match row {
        TimelineRow::Item { continues, .. } => (0u8, *continues).hash(&mut hasher),
        TimelineRow::StateGroup { range } => (1u8, range.len()).hash(&mut hasher),
    }
    for item in &items[row.items()] {
        item.hash(&mut hasher);
    }
    hasher.finish()
}

/// The rows that changed between two fingerprint lists: the range of
/// `old` to replace and how many rows of `new` replace it. `None` when
/// nothing changed.
pub fn changed_rows(old: &[u64], new: &[u64]) -> Option<(Range<usize>, usize)> {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(old, new)| old == new)
        .count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    Some((prefix..old.len() - suffix, new.len() - suffix - prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sender: &str, at: u64, content: TimelineContent) -> TimelineItem {
        TimelineItem {
            id: TimelineItemId(format!("{sender}-{at}")),
            kind: TimelineItemKind::Event(TimelineEvent {
                event_id: None,
                sender: format!("@{sender}:x.org").try_into().expect("a user id"),
                sender_name: None,
                timestamp_ms: at,
                is_own: false,
                content,
                send_state: None,
                in_reply_to: None,
                reactions: Vec::new(),
            }),
        }
    }

    fn joined(user: &str) -> TimelineContent {
        TimelineContent::Membership {
            user_id: format!("@{user}:x.org").try_into().expect("a user id"),
            display_name: None,
            change: MembershipChange::Joined,
        }
    }

    #[test]
    fn rows_fold_state_runs_and_group_senders() {
        let items = vec![
            event("ana", 0, joined("ana")),
            event("ben", 1, joined("ben")),
            event("ana", 2, TimelineContent::text("hi")),
            event("ana", 3, TimelineContent::text("again")),
            event(
                "ana",
                3 + GROUP_WINDOW_MS + 1,
                TimelineContent::text("later"),
            ),
            event("ben", 3 + GROUP_WINDOW_MS + 2, TimelineContent::text("yo")),
            event("cat", 3 + GROUP_WINDOW_MS + 3, joined("cat")),
            TimelineItem {
                id: TimelineItemId("rm".to_owned()),
                kind: TimelineItemKind::ReadMarker,
            },
            event("ben", 3 + GROUP_WINDOW_MS + 4, TimelineContent::text("new")),
        ];
        let rows = rows(&items);
        assert_eq!(
            rows,
            [
                TimelineRow::StateGroup { range: 0..2 },
                TimelineRow::Item {
                    index: 2,
                    continues: false
                },
                TimelineRow::Item {
                    index: 3,
                    continues: true
                },
                TimelineRow::Item {
                    index: 4,
                    continues: false
                },
                TimelineRow::Item {
                    index: 5,
                    continues: false
                },
                TimelineRow::Item {
                    index: 6,
                    continues: false
                },
                TimelineRow::Item {
                    index: 7,
                    continues: false
                },
                TimelineRow::Item {
                    index: 8,
                    continues: false
                },
            ]
        );
    }

    #[test]
    fn changed_rows_finds_the_smallest_range() {
        assert_eq!(changed_rows(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(changed_rows(&[1, 2, 3], &[1, 9, 3]), Some((1..2, 1)));
        assert_eq!(changed_rows(&[1, 2, 3], &[1, 2, 3, 4]), Some((3..3, 1)));
        assert_eq!(changed_rows(&[1, 2, 3], &[0, 1, 2, 3]), Some((0..0, 1)));
        assert_eq!(changed_rows(&[1, 2, 3], &[]), Some((0..3, 0)));
        assert_eq!(changed_rows(&[1, 1], &[1]), Some((1..2, 0)));
        let items = vec![event("ana", 0, TimelineContent::text("a"))];
        let row = TimelineRow::Item {
            index: 0,
            continues: false,
        };
        let mut edited = items.clone();
        if let TimelineItemKind::Event(event) = &mut edited[0].kind {
            event.content = TimelineContent::text("b");
        }
        assert_ne!(
            row_fingerprint(&row, &items),
            row_fingerprint(&row, &edited)
        );
    }

    #[test]
    fn state_changes_have_sentences() {
        assert_eq!(
            describe(&joined("ana"), "Ana").as_deref(),
            Some("@ana:x.org joined")
        );
        let rename = TimelineContent::State {
            event_type: "m.room.name".to_owned(),
            change: StateChange::Name(Some("Smiths".to_owned())),
        };
        assert_eq!(
            describe(&rename, "Mum").as_deref(),
            Some("Mum renamed the room to Smiths")
        );
        let profile = TimelineContent::ProfileChange {
            user_id: "@ana:x.org".try_into().expect("a user id"),
            old_name: Some("Ana".to_owned()),
            new_name: Some("Ana S".to_owned()),
            name_changed: true,
            avatar_changed: false,
        };
        assert_eq!(
            describe(&profile, "Ana").as_deref(),
            Some("Ana changed their name to Ana S")
        );
        assert_eq!(describe(&TimelineContent::Redacted, "x"), None);
        assert_eq!(summary_label("Ana joined", 1), "Ana joined");
        assert_eq!(
            summary_label("Ana joined", 5),
            "Ana joined and 4 more changes"
        );
    }

    #[test]
    fn days_and_times_read_naturally() {
        let now = 1_762_340_400_000; // 5 November 2025, 11:00 UTC.
        let day = 24 * 60 * 60 * 1000;
        assert_eq!(day_label(now, now), "Today");
        assert_eq!(day_label(now - day, now), "Yesterday");
        assert!(!day_label(now - 3 * day, now).contains(' '), "a weekday");
        assert!(day_label(now - 400 * day, now).ends_with("2024"));
        assert_eq!(time_label(now).len(), 5);
    }

    #[test]
    fn a_message_body_never_prints() {
        let content = TimelineContent::text("the secret plan");
        let debug = format!("{content:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(debug.contains("15 bytes"), "{debug}");
        let TimelineContent::Message { body, .. } = content else {
            panic!("a message");
        };
        assert_eq!(body.as_str(), "the secret plan");
    }
}
