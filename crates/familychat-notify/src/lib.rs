//! New-message notifications (PLAN.md, section 5.9; ADR 0013).
//!
//! The session reports each notification its accounts' syncs raise (push
//! rules applied, the message decrypted where it can be) as an
//! [`IncomingNotification`]. A frontend hands a batch of them (those that
//! arrived together) to a [`Notifier`], which decides what the operating
//! system shows:
//!
//! - nothing for the room the person is looking at (the open room of the
//!   focused window), which also counts as having seen it;
//! - one notification per room, coalesced: a room's notification is
//!   replaced by the next one (the same tag) and counts what arrived
//!   since the room was last seen ("3 new messages");
//! - the sender and the room, and the message's text, or "New message"
//!   when the person hides the content (the text never leaves the
//!   computer either way: the OS's own notification centre shows it);
//! - Zmail's flood cap: beyond [`MAX_NOTIFICATIONS_PER_MINUTE`] in a
//!   rolling minute, one summary notification stands in for the rest
//!   until the minute frees up again.
//!
//! Each notification has an opaque tag (`familychat-room-<n>`); the room a
//! click opens stays here ([`Notifier::target`]), so no Matrix id reaches
//! the OS's notification store.
//!
//! Adapted from Zmail's `zmail-notify` (unicornops/zmail, GPL-3.0-only
//! like this project): the flood cap, its rolling window and its tests;
//! the per-batch notification became per-room coalescing, and the summary
//! line one notification that stands in for the rest.

use std::{
    collections::VecDeque,
    fmt,
    time::{Duration, Instant},
};

use familychat_core::{
    OwnedRoomId, RoomId,
    identity::APP_NAME,
    notifications::{IncomingNotification, NotificationContent},
    session::AccountKey,
};

/// Maximum number of notifications per rolling minute (Zmail's cap).
pub const MAX_NOTIFICATIONS_PER_MINUTE: usize = 6;

/// Window of the flood cap.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

/// The longest message text a notification shows, in characters.
pub const MAX_TEXT_CHARS: usize = 140;

/// The longest name (a sender, a room) a notification shows.
pub const MAX_NAME_CHARS: usize = 64;

/// How many rooms' notifications a click can still open.
pub const REMEMBERED_ROOMS: usize = 64;

/// The tag of the summary that stands in for notifications over the cap.
pub const FLOOD_TAG: &str = "familychat-flood";

/// What a notification says instead of the message when the content is
/// hidden, or when the message can't be described.
pub const NEW_MESSAGE: &str = "New message";

/// What an invitation says when the content is hidden.
pub const NEW_INVITATION: &str = "New invitation";

/// The summary's text.
pub const FLOOD_MESSAGE: &str = "Messages are arriving quickly. Open Family Chat to read them.";

/// The window's title: the app's name, and the unread total when there
/// is one ("Family Chat (3)"), as the taskbar and the window switcher
/// show it.
pub fn window_title(unread: u64) -> String {
    if unread == 0 {
        APP_NAME.to_owned()
    } else {
        format!("{APP_NAME} ({unread})")
    }
}

/// The room a notification opens.
#[derive(Clone, PartialEq, Eq)]
pub struct RoomTarget {
    /// The account.
    pub account: AccountKey,
    /// The room.
    pub room_id: OwnedRoomId,
    /// The room's name when the notification came (for a room not in the
    /// list yet).
    pub room_name: String,
    /// A direct chat.
    pub is_direct: bool,
    /// The notification was an invitation.
    pub is_invite: bool,
}

impl fmt::Debug for RoomTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoomTarget")
            .field("account", &self.account)
            .field("is_direct", &self.is_direct)
            .field("is_invite", &self.is_invite)
            .finish_non_exhaustive()
    }
}

/// One notification to post. `Debug` shows only its tag.
#[derive(Clone, PartialEq, Eq)]
pub struct Notification {
    /// The opaque tag: posting a notification with the same tag replaces
    /// the earlier one where the OS supports it.
    pub tag: String,
    /// The headline: the room, or the sender of a direct chat.
    pub title: String,
    /// The lines below it.
    pub body: String,
    /// The room a click opens.
    pub opens: RoomTarget,
}

impl fmt::Debug for Notification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notification")
            .field("tag", &self.tag)
            .finish_non_exhaustive()
    }
}

/// A room that has notified.
struct RoomEntry {
    tag: String,
    target: RoomTarget,
    /// Notifications since the room was last seen.
    unseen: u32,
}

/// Decides what the OS shows. Lives on the frontend's thread; every call
/// is given the time, so the cap is tested without waiting.
pub struct Notifier {
    show_content: bool,
    /// When each recent notification was posted (the flood cap).
    recent: VecDeque<Instant>,
    /// When the summary was posted, while the cap holds.
    flood_posted: Option<Instant>,
    /// The flood summary's room.
    flood_target: Option<RoomTarget>,
    /// The rooms that have notified, least recently used first.
    rooms: VecDeque<RoomEntry>,
    next_tag: u64,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Notifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notifier")
            .field("show_content", &self.show_content)
            .field("rooms", &self.rooms.len())
            .finish_non_exhaustive()
    }
}

impl Notifier {
    /// A notifier that shows the message's text.
    pub fn new() -> Self {
        Self {
            show_content: true,
            recent: VecDeque::new(),
            flood_posted: None,
            flood_target: None,
            rooms: VecDeque::new(),
            next_tag: 0,
        }
    }

    /// Shows the message's text, or "New message" instead.
    pub fn set_show_content(&mut self, show: bool) {
        self.show_content = show;
    }

    /// Whether the message's text is shown.
    pub fn shows_content(&self) -> bool {
        self.show_content
    }

    /// What to post for `batch` (notifications that arrived together, in
    /// order), at `now`. `focused` is the room the person is looking at,
    /// if any: the open room, while the window has focus.
    pub fn notify(
        &mut self,
        batch: Vec<IncomingNotification>,
        focused: Option<(&AccountKey, &RoomId)>,
        now: Instant,
    ) -> Vec<Notification> {
        // The latest notification of each room, in the order rooms last
        // notified.
        let mut latest: Vec<IncomingNotification> = Vec::new();
        for incoming in batch {
            let is_focused = focused.is_some_and(|(account, room)| {
                *account == incoming.account && *room == *incoming.room_id
            });
            if is_focused {
                if let Some(index) = self.position(&incoming.account, &incoming.room_id) {
                    self.rooms[index].unseen = 0;
                }
                latest.retain(|other| {
                    other.account != incoming.account || other.room_id != incoming.room_id
                });
                continue;
            }
            let index = self.entry(&incoming);
            self.rooms[index].unseen = self.rooms[index].unseen.saturating_add(1);
            latest.retain(|other| {
                other.account != incoming.account || other.room_id != incoming.room_id
            });
            latest.push(incoming);
        }

        let mut posted = Vec::new();
        for incoming in latest {
            let Some(index) = self.position(&incoming.account, &incoming.room_id) else {
                continue;
            };
            let target = self.rooms[index].target.clone();
            if self.over_cap(now) {
                if self.flood_posted.is_none() {
                    self.flood_posted = Some(now);
                    self.flood_target = Some(target.clone());
                    posted.push(Notification {
                        tag: FLOOD_TAG.to_owned(),
                        title: APP_NAME.to_owned(),
                        body: FLOOD_MESSAGE.to_owned(),
                        opens: target,
                    });
                }
                continue;
            }
            self.recent.push_back(now);
            let entry = &self.rooms[index];
            let (title, body) = compose(&incoming, entry.unseen, self.show_content);
            posted.push(Notification {
                tag: entry.tag.clone(),
                title,
                body,
                opens: target,
            });
        }
        posted
    }

    /// The room has been seen (opened while the window has focus, or its
    /// notification clicked): its count starts again. Returns its tag when
    /// it had a notification showing, so the frontend can take it down.
    pub fn room_seen(&mut self, account: &AccountKey, room: &RoomId) -> Option<String> {
        let index = self.position(account, room)?;
        let entry = &mut self.rooms[index];
        let had_unseen = entry.unseen > 0;
        entry.unseen = 0;
        had_unseen.then(|| entry.tag.clone())
    }

    /// The room the notification tagged `tag` opens, if it is one of the
    /// last [`REMEMBERED_ROOMS`] rooms that notified.
    pub fn target(&self, tag: &str) -> Option<&RoomTarget> {
        if tag == FLOOD_TAG {
            return self.flood_target.as_ref();
        }
        self.rooms
            .iter()
            .find(|entry| entry.tag == tag)
            .map(|entry| &entry.target)
    }

    /// Whether a notification now would be over the cap. Forgets what
    /// left the window, and lifts the summary once the window frees up.
    fn over_cap(&mut self, now: Instant) -> bool {
        while let Some(front) = self.recent.front()
            && now.saturating_duration_since(*front) >= RATE_WINDOW
        {
            self.recent.pop_front();
        }
        if self
            .flood_posted
            .is_some_and(|posted| now.saturating_duration_since(posted) >= RATE_WINDOW)
        {
            self.flood_posted = None;
        }
        self.recent.len() >= MAX_NOTIFICATIONS_PER_MINUTE
    }

    fn position(&self, account: &AccountKey, room: &RoomId) -> Option<usize> {
        self.rooms
            .iter()
            .position(|entry| entry.target.account == *account && entry.target.room_id == room)
    }

    /// The room's entry, made or moved to the back (most recently used),
    /// with what the notification says about the room now.
    fn entry(&mut self, incoming: &IncomingNotification) -> usize {
        let target = RoomTarget {
            account: incoming.account.clone(),
            room_id: incoming.room_id.clone(),
            room_name: incoming.room_name.clone(),
            is_direct: incoming.is_direct,
            is_invite: incoming.content == NotificationContent::Invite,
        };
        let entry = match self.position(&incoming.account, &incoming.room_id) {
            Some(index) => {
                let mut entry = self.rooms.remove(index).unwrap_or_else(|| unreachable!());
                entry.target = target;
                entry
            }
            None => {
                self.next_tag += 1;
                if self.rooms.len() == REMEMBERED_ROOMS {
                    self.rooms.pop_front();
                }
                RoomEntry {
                    tag: format!("familychat-room-{}", self.next_tag),
                    target,
                    unseen: 0,
                }
            }
        };
        self.rooms.push_back(entry);
        self.rooms.len() - 1
    }
}

/// The title and body of a room's notification: `unseen` messages since
/// the room was last seen, the latest being `incoming`.
fn compose(incoming: &IncomingNotification, unseen: u32, show_content: bool) -> (String, String) {
    let sender = clean_or(&incoming.sender_name, MAX_NAME_CHARS, "Someone");
    let room = clean_or(&incoming.room_name, MAX_NAME_CHARS, APP_NAME);
    let title = if incoming.is_direct {
        sender.clone()
    } else {
        room
    };
    let line = if incoming.content == NotificationContent::Invite {
        if show_content {
            format!("{sender} invited you")
        } else {
            NEW_INVITATION.to_owned()
        }
    } else {
        let what = if show_content {
            describe(&incoming.content, &sender)
        } else {
            NEW_MESSAGE.to_owned()
        };
        let emote = show_content && matches!(incoming.content, NotificationContent::Emote(_));
        if incoming.is_direct || emote {
            what
        } else {
            format!("{sender}: {what}")
        }
    };
    let body = if unseen > 1 {
        format!("{unseen} new messages\n{line}")
    } else {
        line
    };
    (title, body)
}

/// A message, in words.
fn describe(content: &NotificationContent, sender: &str) -> String {
    match content {
        NotificationContent::Text(text) => clean_or(text, MAX_TEXT_CHARS, NEW_MESSAGE),
        NotificationContent::Emote(text) => {
            clean_or(&format!("{sender} {text}"), MAX_TEXT_CHARS, NEW_MESSAGE)
        }
        NotificationContent::Image => "Sent a picture".to_owned(),
        NotificationContent::Video => "Sent a video".to_owned(),
        NotificationContent::Audio => "Sent an audio clip".to_owned(),
        NotificationContent::File => "Sent a file".to_owned(),
        NotificationContent::Location => "Shared a location".to_owned(),
        NotificationContent::Sticker => "Sent a sticker".to_owned(),
        NotificationContent::Invite => "Invited you".to_owned(),
        NotificationContent::Unknown => NEW_MESSAGE.to_owned(),
    }
}

/// Text a sender chose, made safe to show: control characters, bidi
/// overrides and invisible characters are dropped, whitespace (newlines
/// included) folds to single spaces, and it is cut to `max` characters,
/// the last of them an ellipsis.
pub fn clean(text: &str, max: usize) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !folded.is_empty();
        } else if !is_invisible(c) {
            if pending_space {
                folded.push(' ');
                pending_space = false;
            }
            folded.push(c);
        }
    }
    if folded.chars().count() <= max {
        return folded;
    }
    let mut cut: String = folded.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

fn clean_or(text: &str, max: usize, fallback: &str) -> String {
    let cleaned = clean(text, max);
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

/// Characters that change how text around them reads without showing:
/// bidi marks, embeddings, overrides and isolates, and zero-width spaces.
/// The zero-width joiner stays: emoji sequences need it.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests;
