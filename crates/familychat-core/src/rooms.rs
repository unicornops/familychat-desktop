//! The room list: one [`RoomSummary`] per room, and the filters both
//! frontends offer (PLAN.md, section 5.6); a room's members
//! ([`RoomMember`]); and the sentences both frontends say about them.

use std::fmt;

use ruma::{OwnedRoomId, OwnedUserId};

use crate::timeline::MessageBody;

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

/// The latest message in a room, as the room list previews it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPreview {
    /// Who sent it: their display name, or their user id.
    pub sender: String,
    /// The account sent it.
    pub is_own: bool,
    /// One line of it: its text, or what it is ("Sent an image").
    pub text: MessageBody,
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
    /// The room's topic.
    pub topic: Option<String>,
    /// How many members have joined.
    pub joined_members: u64,
    /// The latest message, for the list's second line.
    pub preview: Option<RoomPreview>,
    /// For an invitation: who sent it, by name or user id.
    pub invited_by: Option<String>,
}

impl RoomSummary {
    /// A joined room called `name`, with nothing else known (fixtures and
    /// tests fill in the rest).
    pub fn new(room_id: OwnedRoomId, name: impl Into<String>) -> Self {
        Self {
            room_id,
            name: name.into(),
            membership: Membership::Joined,
            is_direct: false,
            encryption: Encryption::Unknown,
            unread: UnreadCounts::default(),
            last_activity_ms: None,
            topic: None,
            joined_members: 0,
            preview: None,
            invited_by: None,
        }
    }

    /// Whether this is an invitation.
    pub fn is_invite(&self) -> bool {
        self.membership == Membership::Invited
    }

    /// The room list row's accessible name: the room, then what is new
    /// ("Smith family, 3 unread, 1 mention").
    pub fn accessible_label(&self) -> String {
        let mut label = self.name.clone();
        if self.is_invite() {
            label.push_str(", invitation");
            if let Some(inviter) = &self.invited_by {
                label.push_str(&format!(" from {inviter}"));
            }
            return label;
        }
        if self.is_direct {
            label.push_str(", direct chat");
        }
        let unread = self.unread;
        if unread.notifications > 0 {
            label.push_str(&format!(", {} unread", unread.notifications));
        } else if unread.marked_unread {
            label.push_str(", marked unread");
        }
        match unread.mentions {
            0 => {}
            1 => label.push_str(", 1 mention"),
            mentions => label.push_str(&format!(", {mentions} mentions")),
        }
        label
    }
}

impl Encryption {
    /// The room header's words for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Encrypted => "Encrypted",
            Self::NotEncrypted => "Not encrypted",
            Self::Unknown => "Encryption unknown",
        }
    }

    /// The longer sentence the header's badge explains it with.
    pub fn description(self) -> &'static str {
        match self {
            Self::Encrypted => {
                "Messages here are end-to-end encrypted: only the people in the room can read them."
            }
            Self::NotEncrypted => {
                "Messages here are not end-to-end encrypted: the server can read them."
            }
            Self::Unknown => "Family Chat doesn't know yet whether this room is encrypted.",
        }
    }
}

/// A member's role, from the room's power levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberRole {
    /// Power level 100 or more.
    Admin,
    /// Power level 50 or more.
    Moderator,
    /// Everyone else.
    Member,
}

impl MemberRole {
    /// The role for a power level.
    pub fn from_power_level(level: i64) -> Self {
        if level >= 100 {
            Self::Admin
        } else if level >= 50 {
            Self::Moderator
        } else {
            Self::Member
        }
    }

    /// Its name in the member list (`None` for an ordinary member).
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Admin => Some("Admin"),
            Self::Moderator => Some("Moderator"),
            Self::Member => None,
        }
    }
}

/// A member of a room.
#[derive(Clone, PartialEq, Eq)]
pub struct RoomMember {
    /// Their user id.
    pub user_id: OwnedUserId,
    /// Their display name in this room, when they set one.
    pub display_name: Option<String>,
    /// Joined or invited.
    pub membership: Membership,
    /// Their role.
    pub role: MemberRole,
    /// The account itself.
    pub is_own: bool,
}

impl fmt::Debug for RoomMember {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoomMember")
            .field("membership", &self.membership)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl RoomMember {
    /// The name to show: the display name, or the user id.
    pub fn name(&self) -> String {
        self.display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.user_id.to_string())
    }
}

/// Sorts members as the member list shows them: joined before invited,
/// admins and moderators first, then by name.
pub fn sort_members(members: &mut [RoomMember]) {
    members.sort_by_cached_key(|member| {
        (
            member.membership != Membership::Joined,
            member.role,
            member.name().to_lowercase(),
        )
    });
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

    /// What an empty list under this filter is called (the window's and
    /// the terminal's empty states).
    pub fn empty_title(self) -> &'static str {
        match self {
            Self::All => "No rooms yet",
            Self::Unread => "You're all caught up",
            Self::People => "No direct chats",
            Self::Invites => "No invitations",
        }
    }

    /// The sentence under [`RoomListFilter::empty_title`].
    pub fn empty_detail(self) -> &'static str {
        match self {
            Self::All => "When someone in your family adds you to a room, it shows up here.",
            Self::Unread => "Rooms with new messages show up here.",
            Self::People => "Chats with one other person show up here.",
            Self::Invites => "When someone invites you to a room, it shows up here.",
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
    fn rows_say_what_is_new() {
        let mut room = RoomSummary::new(
            "!a:smith.safechat.family".try_into().expect("a room id"),
            "Smith family",
        );
        assert_eq!(room.accessible_label(), "Smith family");
        room.unread.notifications = 3;
        room.unread.mentions = 1;
        assert_eq!(room.accessible_label(), "Smith family, 3 unread, 1 mention");
        room.membership = Membership::Invited;
        room.invited_by = Some("Ben".to_owned());
        assert_eq!(room.accessible_label(), "Smith family, invitation from Ben");
    }

    #[test]
    fn members_sort_by_role_then_name() {
        let member = |name: &str, role, membership| RoomMember {
            user_id: format!("@{}:x.org", name.to_lowercase())
                .try_into()
                .expect("a user id"),
            display_name: Some(name.to_owned()),
            membership,
            role,
            is_own: false,
        };
        let mut members = vec![
            member("zed", MemberRole::Member, Membership::Joined),
            member("Ann", MemberRole::Member, Membership::Invited),
            member("Bob", MemberRole::Member, Membership::Joined),
            member("Mum", MemberRole::Admin, Membership::Joined),
        ];
        sort_members(&mut members);
        let names: Vec<_> = members.iter().map(RoomMember::name).collect();
        assert_eq!(names, ["Mum", "Bob", "zed", "Ann"]);
        assert_eq!(MemberRole::from_power_level(50), MemberRole::Moderator);
        assert!(!format!("{:?}", members[0]).contains("Mum"));
    }

    #[test]
    fn every_filter_has_a_label() {
        let labels: Vec<_> = RoomListFilter::ALL.iter().map(|f| f.label()).collect();
        assert_eq!(labels, ["All", "Unread", "People", "Invites"]);
        assert_eq!(RoomListFilter::default(), RoomListFilter::All);
    }
}
