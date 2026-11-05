//! The room list: one [`RoomSummary`] per room, and the filters both
//! frontends offer (PLAN.md, section 5.6).

use ruma::OwnedRoomId;

/// The account's membership of a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// Joined.
    Joined,
    /// Invited, not yet joined.
    Invited,
    /// Asked to join (knocked).
    Knocked,
    /// Left, or was removed.
    Left,
    /// Banned.
    Banned,
}

/// Whether a room is end-to-end encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// Encrypted.
    Encrypted,
    /// Not encrypted (a supervised family room, or a server that turned
    /// encryption off): frontends show a banner (PLAN.md, section 6.8).
    NotEncrypted,
    /// Not known yet (the room's state has not synced).
    Unknown,
}

/// Unread counts, as the server and the read receipts give them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadCounts {
    /// Unread messages.
    pub messages: u64,
    /// Unread messages that notify.
    pub notifications: u64,
    /// Unread mentions of the user.
    pub mentions: u64,
    /// The user marked the room unread.
    pub marked_unread: bool,
}

impl UnreadCounts {
    /// Whether the room shows as unread.
    pub fn is_unread(&self) -> bool {
        self.notifications > 0 || self.marked_unread
    }
}

/// A room in the room list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSummary {
    /// The room's id.
    pub room_id: OwnedRoomId,
    /// The name to show: the room's name, or one made from its members.
    pub name: String,
    /// The account's membership.
    pub membership: Membership,
    /// A direct chat with one or more people (the "People" filter).
    pub is_direct: bool,
    /// Whether the room is encrypted.
    pub encryption: Encryption,
    /// Unread counts.
    pub unread: UnreadCounts,
    /// When the latest event the list knows of was sent, in milliseconds
    /// since the Unix epoch.
    pub last_activity_ms: Option<u64>,
}

/// Which rooms the room list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RoomListFilter {
    /// Every room not left, invites included.
    #[default]
    All,
    /// Rooms with unread messages that notify, or marked unread.
    Unread,
    /// Direct chats.
    People,
    /// Invitations.
    Invites,
}

impl RoomListFilter {
    /// Every filter, in the order the frontends offer them.
    pub const ALL: [Self; 4] = [Self::All, Self::Unread, Self::People, Self::Invites];

    /// The filter's name, as a tab or a key hint shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "Unread",
            Self::People => "People",
            Self::Invites => "Invites",
        }
    }
}

/// Whether the room list has loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomListLoading {
    /// Nothing to show yet: the first sync has not answered.
    NotLoaded,
    /// Loaded; `total` is the number of rooms the server says the account
    /// has, when it says.
    Loaded {
        /// The server's count.
        total: Option<u32>,
    },
}

/// What a frontend asks of an open room list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomListCommand {
    /// Show another set of rooms (the list resets to it).
    SetFilter(RoomListFilter),
    /// Show one more page of rooms.
    LoadMore,
    /// Go back to showing the first page.
    FirstPageOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_follows_notifications_and_the_mark() {
        assert!(!UnreadCounts::default().is_unread());
        let quiet = UnreadCounts {
            messages: 3,
            ..UnreadCounts::default()
        };
        assert!(!quiet.is_unread(), "messages that do not notify");
        assert!(
            UnreadCounts {
                notifications: 1,
                ..UnreadCounts::default()
            }
            .is_unread()
        );
        assert!(
            UnreadCounts {
                marked_unread: true,
                ..UnreadCounts::default()
            }
            .is_unread()
        );
    }

    #[test]
    fn every_filter_has_a_label() {
        let labels: Vec<_> = RoomListFilter::ALL.iter().map(|f| f.label()).collect();
        assert_eq!(labels, ["All", "Unread", "People", "Invites"]);
        assert_eq!(RoomListFilter::default(), RoomListFilter::All);
    }
}
