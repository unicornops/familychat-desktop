//! The fixture family's rooms, timelines, members and pictures, for the
//! screenshot matrix and the view tests (PLAN.md, sections 5.3 and 9).
//!
//! The Smiths (`smith.safechat.family`): Ana (the signed-in account), Mum,
//! Dad, Ben, Grandad and Aunt Clare, with the family room, direct chats, a
//! supervised homework room that is not encrypted, an invitation and a
//! room on another server. Each fixture room shows one family of timeline
//! items (rich text, media, room changes, replies and reactions, messages
//! that can't be read), so every way a message draws has a screenshot.
//!
//! Formatted messages go through `familychat-security`'s real Matrix HTML
//! sanitiser, so the screenshots show what the app makes of the markup.
//! Timestamps count back from the moment the fixture is built, so the day
//! dividers say "Today" and "Yesterday".

use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use familychat_core::{
    OwnedRoomId, OwnedUserId,
    media::{DecodedImage, ImageFrame, MediaHandle, MediaInfo},
    rich_text::{Block, BlockKind, RichLink, RichText, Span, SpanStyle},
    rooms::{
        Encryption, MemberRole, Membership, RoomMember, RoomPreview, RoomSummary, UnreadCounts,
    },
    timeline::{
        MembershipChange, MessageBody, MessageKind, Reaction, ReplyPreview, SendState, StateChange,
        TimelineContent, TimelineEvent, TimelineItem, TimelineItemId, TimelineItemKind,
    },
};
use familychat_security::matrix_html::message_text;

use super::FIXTURE_SERVER;

const MINUTE: u64 = 60 * 1000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// The family room.
pub const FAMILY_ROOM: &str = "!family:smith.safechat.family";
/// Mum's direct chat with Ana.
pub const MUM_ROOM: &str = "!mum:smith.safechat.family";
/// Grandad's direct chat with Ana.
pub const GRANDAD_ROOM: &str = "!grandad:smith.safechat.family";
/// Ben's direct chat with Ana.
pub const BEN_ROOM: &str = "!ben:smith.safechat.family";
/// The homework room: supervised, so not encrypted.
pub const HOMEWORK_ROOM: &str = "!homework:smith.safechat.family";
/// The holiday room: formatted messages (lists, quotes, code, links).
pub const HOLIDAY_ROOM: &str = "!holiday:smith.safechat.family";
/// The photos room: images and files.
pub const PHOTOS_ROOM: &str = "!photos:smith.safechat.family";
/// The football team, on another server: room changes folded.
pub const FOOTBALL_ROOM: &str = "!football:matrix.example.org";
/// The cousins' room, which Ana is invited to.
pub const INVITE_ROOM: &str = "!cousins:smith.safechat.family";
/// A new room with nothing in it yet.
pub const EMPTY_ROOM: &str = "!new:smith.safechat.family";
/// The secrets room: messages that can't be read, deleted and edited.
pub const SECRETS_ROOM: &str = "!secrets:smith.safechat.family";

/// The handle of a fixture image that is an SVG (refused).
pub const SVG_HANDLE: &str = "fixture-media:svg";
/// The handle of a fixture image that is too large to decode.
pub const HUGE_HANDLE: &str = "fixture-media:huge";

/// The current time, in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// A user id on the family's server.
pub fn user(localpart: &str) -> OwnedUserId {
    OwnedUserId::try_from(format!("@{localpart}:{FIXTURE_SERVER}"))
        .unwrap_or_else(|_| unreachable!("a valid user id"))
}

/// A room id.
pub fn room_id(id: &str) -> OwnedRoomId {
    OwnedRoomId::try_from(id).unwrap_or_else(|_| unreachable!("a valid room id"))
}

/// The family's display names.
fn display_name(localpart: &str) -> &'static str {
    match localpart {
        "ana" => "Ana",
        "mum" => "Mum",
        "dad" => "Dad",
        "ben" => "Ben",
        "grandad" => "Grandad",
        "clare" => "Aunt Clare",
        "bot" => "Family Chat",
        "coach" => "Coach Rivera",
        _ => "Someone",
    }
}

fn preview(sender: &str, text: &str) -> Option<RoomPreview> {
    Some(RoomPreview {
        sender: display_name(sender).to_owned(),
        is_own: sender == "ana",
        text: MessageBody::new(text),
    })
}

fn summary(
    id: &str,
    name: &str,
    encryption: Encryption,
    members: u64,
    ago_ms: u64,
    now: u64,
) -> RoomSummary {
    RoomSummary {
        encryption,
        joined_members: members,
        last_activity_ms: Some(now - ago_ms),
        ..RoomSummary::new(room_id(id), name)
    }
}

/// The fixture family's room list, most recent first.
pub fn family_rooms(now: u64) -> Vec<RoomSummary> {
    let mut invite = summary(
        INVITE_ROOM,
        "Cousins",
        Encryption::Encrypted,
        4,
        20 * MINUTE,
        now,
    );
    invite.membership = Membership::Invited;
    invite.invited_by = Some("Aunt Clare".to_owned());

    let mut family = summary(
        FAMILY_ROOM,
        "Smith family",
        Encryption::Encrypted,
        5,
        2 * MINUTE,
        now,
    );
    family.topic = Some("Plans, photos and who's picking up whom".to_owned());
    family.unread = UnreadCounts {
        messages: 3,
        notifications: 3,
        mentions: 1,
        marked_unread: false,
    };
    family.preview = preview("mum", "Dinner at 7, don't be late! @Ana");

    let mut mum = summary(MUM_ROOM, "Mum", Encryption::Encrypted, 2, 35 * MINUTE, now);
    mum.is_direct = true;
    mum.preview = preview("ana", "Love you too ❤️");

    let mut grandad = summary(
        GRANDAD_ROOM,
        "Grandad",
        Encryption::Encrypted,
        2,
        3 * HOUR,
        now,
    );
    grandad.is_direct = true;
    grandad.unread = UnreadCounts {
        messages: 2,
        notifications: 2,
        mentions: 0,
        marked_unread: false,
    };
    grandad.preview = preview("grandad", "Sent an image");

    let mut photos = summary(
        PHOTOS_ROOM,
        "Holiday photos",
        Encryption::Encrypted,
        5,
        5 * HOUR,
        now,
    );
    photos.preview = preview("dad", "Sent an image");

    let mut holiday = summary(
        HOLIDAY_ROOM,
        "Summer holiday 2027",
        Encryption::Encrypted,
        5,
        9 * HOUR,
        now,
    );
    holiday.topic = Some("Cornwall, 2 to 16 August".to_owned());
    holiday.preview = preview("dad", "Packing list is in the pinned message");

    let mut homework = summary(
        HOMEWORK_ROOM,
        "Homework help",
        Encryption::NotEncrypted,
        3,
        DAY + 2 * HOUR,
        now,
    );
    homework.topic = Some("Supervised by Mum and Dad".to_owned());
    homework.preview = preview("ben", "What's 7 × 8 again?");

    let mut ben = summary(
        BEN_ROOM,
        "Ben",
        Encryption::Encrypted,
        2,
        DAY + 5 * HOUR,
        now,
    );
    ben.is_direct = true;
    ben.unread.marked_unread = true;
    ben.preview = preview("ben", "can I borrow your charger");

    let mut football = summary(
        FOOTBALL_ROOM,
        "Under-14s football",
        Encryption::NotEncrypted,
        18,
        2 * DAY,
        now,
    );
    football.preview = preview("coach", "Training moves to Thursday this week");

    let mut secrets = summary(
        SECRETS_ROOM,
        "Birthday surprise",
        Encryption::Encrypted,
        4,
        3 * DAY,
        now,
    );
    secrets.preview = preview("dad", "Message deleted");

    let mut empty = summary(
        EMPTY_ROOM,
        "Book club",
        Encryption::Encrypted,
        1,
        4 * DAY,
        now,
    );
    empty.preview = None;

    vec![
        invite, family, mum, grandad, photos, holiday, homework, ben, football, secrets, empty,
    ]
}

/// The members of a fixture room.
pub fn family_members(room: &str) -> Vec<RoomMember> {
    let member = |localpart: &str, role: MemberRole, membership: Membership| RoomMember {
        user_id: user(localpart),
        display_name: Some(display_name(localpart).to_owned()),
        membership,
        role,
        is_own: localpart == "ana",
    };
    let mut members = match room {
        MUM_ROOM => vec![
            member("mum", MemberRole::Admin, Membership::Joined),
            member("ana", MemberRole::Admin, Membership::Joined),
        ],
        GRANDAD_ROOM => vec![
            member("grandad", MemberRole::Admin, Membership::Joined),
            member("ana", MemberRole::Admin, Membership::Joined),
        ],
        BEN_ROOM => vec![
            member("ben", MemberRole::Admin, Membership::Joined),
            member("ana", MemberRole::Admin, Membership::Joined),
        ],
        EMPTY_ROOM => vec![member("ana", MemberRole::Admin, Membership::Joined)],
        _ => vec![
            member("mum", MemberRole::Admin, Membership::Joined),
            member("dad", MemberRole::Admin, Membership::Joined),
            member("ana", MemberRole::Member, Membership::Joined),
            member("ben", MemberRole::Member, Membership::Joined),
            member("grandad", MemberRole::Moderator, Membership::Joined),
            member("clare", MemberRole::Member, Membership::Invited),
        ],
    };
    familychat_core::rooms::sort_members(&mut members);
    members
}

// ---------------------------------------------------------------------------
// Timelines
// ---------------------------------------------------------------------------

/// Builds a timeline in order, from the oldest item.
struct Builder {
    room: String,
    items: Vec<TimelineItem>,
    next: usize,
}

impl Builder {
    fn new(room: &str) -> Self {
        Self {
            room: room.to_owned(),
            items: Vec::new(),
            next: 0,
        }
    }

    fn id(&mut self) -> TimelineItemId {
        self.next += 1;
        TimelineItemId(format!("{}-{}", self.room, self.next))
    }

    fn push(&mut self, kind: TimelineItemKind) -> &mut Self {
        let id = self.id();
        self.items.push(TimelineItem { id, kind });
        self
    }

    fn start(&mut self) -> &mut Self {
        self.push(TimelineItemKind::TimelineStart)
    }

    fn day(&mut self, at: u64) -> &mut Self {
        self.push(TimelineItemKind::DateDivider { timestamp_ms: at })
    }

    fn read_marker(&mut self) -> &mut Self {
        self.push(TimelineItemKind::ReadMarker)
    }

    fn event(&mut self, sender: &str, at: u64, content: TimelineContent) -> &mut TimelineEvent {
        let number = self.next + 1;
        let event = TimelineEvent {
            event_id: familychat_core::OwnedEventId::try_from(format!(
                "${}-{number}",
                self.room.trim_start_matches('!').replace(':', "-")
            ))
            .ok(),
            sender: user(sender),
            sender_name: Some(display_name(sender).to_owned()),
            timestamp_ms: at,
            is_own: sender == "ana",
            content,
            send_state: None,
            in_reply_to: None,
            reactions: Vec::new(),
        };
        self.push(TimelineItemKind::Event(event));
        match self.items.last_mut().map(|item| &mut item.kind) {
            Some(TimelineItemKind::Event(event)) => event,
            _ => unreachable!("an event was just pushed"),
        }
    }

    fn text(&mut self, sender: &str, at: u64, text: &str) -> &mut TimelineEvent {
        self.event(sender, at, TimelineContent::text(text))
    }

    fn html(&mut self, sender: &str, at: u64, body: &str, html: &str) -> &mut TimelineEvent {
        self.event(sender, at, formatted(MessageKind::Text, body, html))
    }

    fn kind(&mut self, sender: &str, at: u64, kind: MessageKind, text: &str) -> &mut TimelineEvent {
        self.event(
            sender,
            at,
            TimelineContent::Message {
                kind,
                body: MessageBody::new(text),
                rich: RichText::plain(text),
                edited: false,
                media: None,
            },
        )
    }

    fn state(&mut self, sender: &str, at: u64, change: StateChange, event_type: &str) {
        self.event(
            sender,
            at,
            TimelineContent::State {
                event_type: event_type.to_owned(),
                change,
            },
        );
    }

    fn membership(&mut self, sender: &str, at: u64, who: &str, change: MembershipChange) {
        self.event(
            sender,
            at,
            TimelineContent::Membership {
                user_id: user(who),
                display_name: Some(display_name(who).to_owned()),
                change,
            },
        );
    }

    fn reply_to(&self, index_from_end: usize) -> Option<ReplyPreview> {
        let item = self.items.iter().rev().nth(index_from_end)?;
        let TimelineItemKind::Event(event) = &item.kind else {
            return None;
        };
        let text = match &event.content {
            TimelineContent::Message { body, .. } => Some(body.clone()),
            _ => None,
        };
        Some(ReplyPreview {
            event_id: event.event_id.clone()?,
            sender: event.sender_name.clone(),
            text,
        })
    }

    fn finish(&mut self) -> Vec<TimelineItem> {
        std::mem::take(&mut self.items)
    }
}

fn formatted(kind: MessageKind, body: &str, html: &str) -> TimelineContent {
    TimelineContent::Message {
        kind,
        body: MessageBody::new(body),
        rich: message_text(body, Some(html)),
        edited: false,
        media: None,
    }
}

fn reaction(key: &str, senders: &[&str], own: bool) -> Reaction {
    Reaction {
        key: key.to_owned(),
        count: senders.len(),
        own,
        senders: senders
            .iter()
            .map(|sender| display_name(sender).to_owned())
            .collect(),
    }
}

/// A fixture media item.
fn media(
    handle: &str,
    filename: &str,
    mimetype: &str,
    size: u64,
    dims: Option<(u32, u32)>,
) -> MediaInfo {
    MediaInfo {
        source: MediaHandle::new(handle),
        thumbnail: None,
        filename: filename.to_owned(),
        mimetype: Some(mimetype.to_owned()),
        size: Some(size),
        width: dims.map(|(width, _)| width),
        height: dims.map(|(_, height)| height),
    }
}

fn media_message(kind: MessageKind, caption: &str, info: MediaInfo) -> TimelineContent {
    TimelineContent::Message {
        kind,
        body: MessageBody::new(caption),
        rich: RichText::plain(caption),
        edited: false,
        media: Some(info),
    }
}

/// The family room: an ordinary day of messages, with a reply, reactions,
/// an emote, a bot's notice, a mention and the read marker.
fn family_timeline(now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(FAMILY_ROOM);
    let yesterday = now - DAY;
    b.day(yesterday - 3 * HOUR);
    b.text(
        "dad",
        yesterday - 3 * HOUR,
        "Who's in for pancakes tomorrow morning?",
    );
    b.text("ben", yesterday - 3 * HOUR + MINUTE, "ME");
    b.text(
        "ben",
        yesterday - 3 * HOUR + 2 * MINUTE,
        "with chocolate chips pls",
    );
    b.kind(
        "dad",
        yesterday - 2 * HOUR,
        MessageKind::Emote,
        "is buying chocolate chips",
    );
    b.text(
        "ana",
        yesterday - 2 * HOUR + 5 * MINUTE,
        "Yes please! Can we have them before football?",
    )
    .reactions = vec![reaction("👍", &["dad", "mum"], false)];
    b.kind(
        "bot",
        yesterday - HOUR,
        MessageKind::Notice,
        "Quiet time starts at 21:00. Chat is paused until 07:00.",
    );
    b.day(now - 3 * HOUR);
    b.text("mum", now - 3 * HOUR, "Morning all. Pancakes are ready!");
    b.html(
        "dad",
        now - 2 * HOUR,
        "Ana: leaving for football at 10. Bring your boots.",
        "<a href=\"https://matrix.to/#/@ana:smith.safechat.family\">Ana</a>: leaving for \
         football at <strong>10</strong>. Bring your boots.",
    )
    .reactions = vec![
        reaction("👍", &["ana"], true),
        reaction("⚽", &["ben", "mum", "grandad"], false),
    ];
    let reply = b.reply_to(0);
    let answer = b.text(
        "ana",
        now - 2 * HOUR + 3 * MINUTE,
        "On it. They're by the door.",
    );
    answer.in_reply_to = reply;
    b.read_marker();
    b.text(
        "grandad",
        now - 40 * MINUTE,
        "Good luck today, Ana! Score one for me.",
    );
    b.text(
        "ben",
        now - 10 * MINUTE,
        "can someone bring my water bottle",
    );
    let late = b.html(
        "mum",
        now - 2 * MINUTE,
        "Dinner at 7, don't be late! @Ana",
        "Dinner at 7, <em>don't be late</em>! \
         <a href=\"https://matrix.to/#/@ana:smith.safechat.family\">Ana</a>",
    );
    late.reactions = vec![reaction("❤️", &["dad"], false)];
    let own = b.text("ana", now - MINUTE, "See you then");
    own.send_state = Some(SendState::Sending);
    b.finish()
}

/// The holiday room: formatted messages, every kind of block and style,
/// and links (in the app, external, and one whose text shows another site).
fn holiday_timeline(now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(HOLIDAY_ROOM);
    let at = now - 10 * HOUR;
    b.day(at);
    b.html(
        "dad",
        at,
        "Packing list",
        "<h3>Packing list</h3><ul><li>Swimming things</li><li>Rain coats \
         (<em>it's Cornwall</em>)</li><li>Chargers<ul><li>Phones</li><li>The \
         <strong>good</strong> camera</li></ul></li></ul>",
    );
    b.html(
        "mum",
        at + 5 * MINUTE,
        "The plan",
        "<ol><li>Leave at <strong>6am</strong></li><li>Stop at Exeter for \
         breakfast</li><li>Beach by lunchtime</li></ol>",
    );
    b.html(
        "grandad",
        at + 30 * MINUTE,
        "> Beach by lunchtime\nOptimistic!",
        "<blockquote><p>Beach by lunchtime</p></blockquote><p>Optimistic! Traffic on \
         the A30 is <s>bad</s> <u>terrible</u> in August.</p>",
    );
    b.html(
        "dad",
        at + HOUR,
        "Cottage wifi",
        "Cottage wifi is <code>SeaView-5G</code>, the code is in the welcome book:<pre><code>\
         Network:  SeaView-5G\nCheck-in: 16:00\nCheck-out: 10:00</code></pre>",
    );
    b.html(
        "dad",
        at + HOUR + 10 * MINUTE,
        "Booking: https://cottages.example/booking/4471 Surfing lessons: https://surf.example/kids",
        "Booking confirmation: <a href=\"https://cottages.example/booking/4471\">cottages.example</a>. \
         Surfing lessons for the kids at <a href=\"https://surf.example/kids\">surf.example</a>.",
    );
    b.html(
        "ben",
        at + 2 * HOUR,
        "Free tickets: https://www.bank.example",
        "Free tickets!! <a href=\"https://tickets.evil.example/claim\">https://www.bank.example</a>",
    );
    b.html(
        "mum",
        at + 2 * HOUR + 2 * MINUTE,
        "Ben, don't click links like that. Ask in #family first.",
        "<strong>Ben</strong>, don't click links like that. Ask in \
         <a href=\"matrix:r/family:smith.safechat.family\">#family</a> first.",
    );
    b.html(
        "ana",
        at + 3 * HOUR,
        "Spoiler: the surprise is a boat trip",
        "Spoiler for Ben: <span data-mx-spoiler=\"\">the surprise is a boat trip</span> 🚤",
    );
    b.finish()
}

/// The photos room: files (a document, a voice message, a video), a refused
/// SVG, an image too large to draw, and two photos, newest last.
fn photos_timeline(now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(PHOTOS_ROOM);
    let at = now - 6 * HOUR;
    let item = |kind, caption: &str, handle: &str, name: &str, mime: &str, size, dims| {
        media_message(kind, caption, media(handle, name, mime, size, dims))
    };
    b.day(at);
    b.event(
        "dad",
        at,
        item(
            MessageKind::File,
            "Cottage booking.pdf",
            "fixture-media:booking",
            "Cottage booking.pdf",
            "application/pdf",
            184_000,
            None,
        ),
    );
    b.event(
        "mum",
        at + 10 * MINUTE,
        item(
            MessageKind::Audio,
            "Voice message",
            "fixture-media:voice",
            "voice-message.ogg",
            "audio/ogg",
            96_000,
            None,
        ),
    );
    b.event(
        "dad",
        at + 20 * MINUTE,
        item(
            MessageKind::Video,
            "waves.mp4",
            "fixture-media:waves",
            "waves.mp4",
            "video/mp4",
            14_200_000,
            Some((1920, 1080)),
        ),
    );
    b.event(
        "ben",
        at + 30 * MINUTE,
        item(
            MessageKind::Image,
            "drawing.svg",
            SVG_HANDLE,
            "drawing.svg",
            "image/svg+xml",
            12_000,
            Some((800, 600)),
        ),
    );
    b.event(
        "grandad",
        at + 40 * MINUTE,
        item(
            MessageKind::Image,
            "panorama.png",
            HUGE_HANDLE,
            "panorama.png",
            "image/png",
            31_000_000,
            Some((40000, 8000)),
        ),
    );
    b.event(
        "mum",
        at + 50 * MINUTE,
        item(
            MessageKind::Image,
            "lighthouse.jpg",
            "fixture-media:lighthouse",
            "lighthouse.jpg",
            "image/jpeg",
            1_800_000,
            Some((900, 1200)),
        ),
    );
    b.event(
        "dad",
        at + HOUR,
        item(
            MessageKind::Image,
            "Beach at sunset",
            "fixture-media:sunset",
            "sunset.jpg",
            "image/jpeg",
            2_400_000,
            Some((1600, 1067)),
        ),
    )
    .reactions = vec![reaction("😍", &["mum", "ana", "grandad"], true)];
    b.finish()
}

/// The football room: room changes, which fold into summaries.
fn football_timeline(now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(FOOTBALL_ROOM);
    let at = now - 3 * DAY;
    b.start();
    b.day(at);
    b.state("coach", at, StateChange::Created, "m.room.create");
    b.state(
        "coach",
        at + MINUTE,
        StateChange::Name(Some("Under-14s football".to_owned())),
        "m.room.name",
    );
    b.state(
        "coach",
        at + 2 * MINUTE,
        StateChange::Topic(Some("Fixtures, training and lifts".to_owned())),
        "m.room.topic",
    );
    b.membership("coach", at + 3 * MINUTE, "coach", MembershipChange::Joined);
    b.membership("coach", at + 4 * MINUTE, "ana", MembershipChange::Invited);
    b.membership(
        "ana",
        at + 5 * MINUTE,
        "ana",
        MembershipChange::InvitationAccepted,
    );
    b.membership("dad", at + 6 * MINUTE, "dad", MembershipChange::Joined);
    b.text(
        "coach",
        at + HOUR,
        "Welcome everyone! Training is Tuesdays at 6.",
    );
    b.membership("ben", at + 2 * HOUR, "ben", MembershipChange::Joined);
    b.text("ben", at + 2 * HOUR + MINUTE, "hi coach");
    b.event(
        "dad",
        at + DAY,
        TimelineContent::ProfileChange {
            user_id: user("dad"),
            old_name: Some("Dad".to_owned()),
            new_name: Some("Dad (Ana's)".to_owned()),
            name_changed: true,
            avatar_changed: false,
        },
    );
    b.state(
        "coach",
        at + DAY + HOUR,
        StateChange::PowerLevels,
        "m.room.power_levels",
    );
    b.membership(
        "coach",
        at + DAY + 2 * HOUR,
        "grandad",
        MembershipChange::Kicked,
    );
    b.day(now - DAY);
    b.text("coach", now - DAY, "Training moves to Thursday this week");
    b.finish()
}

/// The birthday room: what can't be shown (unable to decrypt, deleted,
/// unknown events), and edits.
fn secrets_timeline(now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(SECRETS_ROOM);
    let at = now - 3 * DAY - 2 * HOUR;
    b.day(at);
    b.state("dad", at, StateChange::Encryption, "m.room.encryption");
    b.event("mum", at + MINUTE, TimelineContent::UnableToDecrypt);
    b.event("mum", at + 2 * MINUTE, TimelineContent::UnableToDecrypt);
    b.text("dad", at + HOUR, "Cake is ordered. Chocolate, obviously.");
    if let TimelineContent::Message { edited, .. } = &mut b
        .text(
            "ana",
            at + HOUR + 5 * MINUTE,
            "I'll make the banner. Purple and gold?",
        )
        .content
    {
        *edited = true;
    }
    b.event("dad", at + 2 * HOUR, TimelineContent::Redacted);
    b.event(
        "grandad",
        at + 3 * HOUR,
        TimelineContent::Other {
            event_type: Some("m.poll.start".to_owned()),
        },
    );
    b.event(
        "mum",
        at + 4 * HOUR,
        TimelineContent::Sticker {
            body: MessageBody::new("A party popper"),
        },
    );
    b.finish()
}

/// The homework room (not encrypted): a short conversation.
fn homework_timeline(now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(HOMEWORK_ROOM);
    let at = now - DAY - 3 * HOUR;
    b.day(at);
    b.text("ben", at, "What's 7 × 8 again?");
    let reply = b.reply_to(0);
    b.text(
        "ana",
        at + 2 * MINUTE,
        "56. Use the trick: 5, 6, 7, 8 → 56 = 7 × 8",
    )
    .in_reply_to = reply;
    b.text("mum", at + 30 * MINUTE, "Well done both of you.");
    b.finish()
}

fn dm_timeline(
    room: &str,
    other: &str,
    lines: &[(&str, u64, &str)],
    now: u64,
) -> Vec<TimelineItem> {
    let mut b = Builder::new(room);
    if let Some((_, first, _)) = lines.first() {
        b.day(now - first);
    }
    for (sender, ago, text) in lines {
        let sender = if *sender == "them" { other } else { sender };
        b.text(sender, now - ago, text);
    }
    b.finish()
}

/// The timeline of a fixture room.
pub fn family_timeline_for(room: &str, now: u64) -> Vec<TimelineItem> {
    match room {
        FAMILY_ROOM => family_timeline(now),
        HOLIDAY_ROOM => holiday_timeline(now),
        PHOTOS_ROOM => photos_timeline(now),
        FOOTBALL_ROOM => football_timeline(now),
        SECRETS_ROOM => secrets_timeline(now),
        HOMEWORK_ROOM => homework_timeline(now),
        MUM_ROOM => dm_timeline(
            room,
            "mum",
            &[
                ("them", 50 * MINUTE, "Have you got your keys?"),
                ("ana", 45 * MINUTE, "Yes Mum 🙄"),
                ("them", 40 * MINUTE, "Love you"),
                ("ana", 35 * MINUTE, "Love you too ❤️"),
            ],
            now,
        ),
        GRANDAD_ROOM => {
            let mut items = dm_timeline(
                room,
                "grandad",
                &[("them", 3 * HOUR + 5 * MINUTE, "Look what I grew this year!")],
                now,
            );
            let mut b = Builder::new(room);
            b.next = items.len();
            b.read_marker();
            b.event(
                "grandad",
                now - 3 * HOUR,
                media_message(
                    MessageKind::Image,
                    "tomatoes.jpg",
                    media(
                        "fixture-media:tomatoes",
                        "tomatoes.jpg",
                        "image/jpeg",
                        980_000,
                        Some((1200, 900)),
                    ),
                ),
            );
            items.extend(b.finish());
            items
        }
        BEN_ROOM => dm_timeline(
            room,
            "ben",
            &[
                ("them", DAY + 6 * HOUR, "are you home"),
                ("ana", DAY + 5 * HOUR + 50 * MINUTE, "In 10 mins"),
                ("them", DAY + 5 * HOUR, "can I borrow your charger"),
            ],
            now,
        ),
        EMPTY_ROOM => {
            let mut b = Builder::new(room);
            b.start();
            b.day(now - 4 * DAY);
            b.state("ana", now - 4 * DAY, StateChange::Created, "m.room.create");
            b.finish()
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The large fixture
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Hostile text
// ---------------------------------------------------------------------------

/// The hostile room: every string in it carries terminal escape sequences.
pub const HOSTILE_ROOM: &str = "!hostile:smith.safechat.family";
/// A hostile direct chat.
pub const HOSTILE_DM: &str = "!hostile-dm:smith.safechat.family";
/// A hostile invitation.
pub const HOSTILE_INVITE: &str = "!hostile-invite:smith.safechat.family";

/// What a hostile server could put in a name: a screen clear and cursor
/// home, a window title, a clipboard write, a hyperlink, colours, a bidi
/// override and zero-width characters.
pub const HOSTILE_NAME: &str =
    "Smith\x1b[2J\x1b[H family\x1b]0;pwned\x07\x1b]52;c;ZXZpbA==\x07\u{202E}\u{200B}";

/// The hostile rooms: names, topics, previews and an inviter full of
/// escape sequences and controls.
pub fn hostile_rooms(now: u64) -> Vec<RoomSummary> {
    let mut room = summary(
        HOSTILE_ROOM,
        HOSTILE_NAME,
        Encryption::Encrypted,
        3,
        MINUTE,
        now,
    );
    room.topic = Some("\x1b]52;c;ZXZpbA==\x07Topic\u{202E}txt.exe\x1b[?1049h".to_owned());
    room.unread = UnreadCounts {
        messages: 2,
        notifications: 2,
        mentions: 1,
        marked_unread: false,
    };
    room.preview = Some(RoomPreview {
        sender: "\x1b[31mMum\x1b[0m".to_owned(),
        is_own: false,
        text: MessageBody::new("hi\x1b[1A\x1b[2K\rFrom: bank@example.com"),
    });
    let mut dm = summary(
        HOSTILE_DM,
        "Ben\u{200B}\u{202E}nimda\x08\x08\x08\x08\x08",
        Encryption::Encrypted,
        2,
        5 * MINUTE,
        now,
    );
    dm.is_direct = true;
    dm.preview = Some(RoomPreview {
        sender: "Ben\u{9b}2J".to_owned(),
        is_own: false,
        text: MessageBody::new("\x1bPtmux;\x1b\x1b]52;c;ZXZpbA==\x07\x1b\\ok"),
    });
    let mut invite = summary(
        HOSTILE_INVITE,
        "\u{9d}0;title\u{9c}Cousins\x07",
        Encryption::Encrypted,
        4,
        10 * MINUTE,
        now,
    );
    invite.membership = Membership::Invited;
    invite.invited_by = Some("Aunt\x1b[5m Clare\x1b[25m".to_owned());
    vec![room, dm, invite]
}

/// Members whose display names try to rewrite the screen.
pub fn hostile_members() -> Vec<RoomMember> {
    let member = |localpart: &str, name: &str, role: MemberRole| RoomMember {
        user_id: user(localpart),
        display_name: Some(name.to_owned()),
        membership: Membership::Joined,
        role,
        is_own: localpart == "ana",
    };
    vec![
        member("mum", "Mum\x1b[2K\rDad", MemberRole::Admin),
        member("ana", "Ana", MemberRole::Member),
        member(
            "ben",
            "\x1b]8;;https://evil.example/\x1b\\Ben\x1b]8;;\x1b\\",
            MemberRole::Member,
        ),
    ]
}

/// A hostile room's timeline: senders, bodies, a formatted message's text
/// and link, a reply, a file name, a reaction and a room change, each with
/// escape sequences in it.
pub fn hostile_timeline(room: &str, now: u64) -> Vec<TimelineItem> {
    let mut b = Builder::new(room);
    let at = now - 30 * MINUTE;
    b.day(at);
    let first = b.text(
        "mum",
        at,
        "Hello\x1b[31m red\x1b[0m\n\x1b]8;;https://evil.example/\x1b\\click\x1b]8;;\x1b\\\tthere\r\nok",
    );
    first.sender_name = Some("Mum\x1b[2K\rDad".to_owned());
    // A formatted message whose text and link carry escapes (the session's
    // HTML sanitiser never makes this; a hostile fixture can).
    let text = "Visit \x1b[5mbank\x1b[0m now\u{202E}".to_owned();
    let link_start = text.find("bank").unwrap_or(0) - "\x1b[5m".len();
    let link_end = text.find(" now").unwrap_or(text.len());
    let mut block = Block::new(BlockKind::Paragraph, text);
    block.spans.push(Span {
        range: link_start..link_end,
        style: SpanStyle {
            link: Some(0),
            bold: true,
            ..SpanStyle::default()
        },
    });
    let mut quote = Block::new(BlockKind::Paragraph, "quoted \x1b]0;title\x07text");
    quote.quote_depth = 1;
    let rich = RichText {
        blocks: vec![block, quote],
        links: vec![RichLink {
            href: "https://evil.example/\x1b]0;x\x07path".to_owned(),
        }],
    };
    b.event(
        "ben",
        at + MINUTE,
        TimelineContent::Message {
            kind: MessageKind::Text,
            body: MessageBody::new("Visit bank now"),
            rich,
            edited: true,
            media: None,
        },
    )
    .sender_name = Some("\x1b]8;;https://evil.example/\x1b\\Ben\x1b]8;;\x1b\\".to_owned());
    let reply = b.reply_to(0).map(|reply| ReplyPreview {
        sender: Some("\x1b]2;x\x07Ben".to_owned()),
        text: Some(MessageBody::new("quoted\x1b[2J\x1b[H")),
        ..reply
    });
    let answer = b.text("ana", at + 2 * MINUTE, "Not clicking that.");
    answer.in_reply_to = reply;
    answer.reactions = vec![Reaction {
        key: "\x1b[5m😀".to_owned(),
        count: 1,
        own: false,
        senders: vec!["\x1b[31mMum".to_owned()],
    }];
    b.event(
        "mum",
        at + 3 * MINUTE,
        media_message(
            MessageKind::File,
            "report\x1b[8m.pdf",
            media(
                "fixture-media:hostile-file",
                "report\x1b[8m.pdf\u{202E}fdp.exe",
                "application/pdf",
                1_000,
                None,
            ),
        ),
    );
    b.event(
        "mum",
        at + 4 * MINUTE,
        TimelineContent::Membership {
            user_id: user("eve"),
            display_name: Some("\x1b[1mEve\x1b[0m\x07".to_owned()),
            change: MembershipChange::Joined,
        },
    );
    b.kind(
        "bot",
        at + 5 * MINUTE,
        MessageKind::Notice,
        "\x1bPq#0;2;0;0;0\x1b\\Notice\x1b_Gf=100;AAAA\x1b\\ end",
    );
    b.finish()
}

/// The large fixture's room count (PLAN.md, section 13.6).
pub const LARGE_ROOMS: usize = 1_000;
/// The large fixture's event count in its first room.
pub const LARGE_EVENTS: usize = 10_000;

/// Words for the large fixture's messages.
const WORDS: &[&str] = &[
    "pancakes",
    "football",
    "homework",
    "Cornwall",
    "train",
    "dinner",
    "school",
    "garden",
    "birthday",
    "weekend",
    "bus",
    "rain",
    "cinema",
    "piano",
    "swimming",
    "library",
    "Grandad",
    "tomorrow",
    "tonight",
    "later",
    "maybe",
    "definitely",
    "picture",
    "music",
    "chocolate",
];

/// A small deterministic generator (xorshift), so the large fixture is the
/// same on every run.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        usize::try_from(self.next() % limit.max(1) as u64).unwrap_or(0)
    }
}

/// `rooms` rooms, most recent first.
pub fn large_rooms(rooms: usize, now: u64) -> Vec<RoomSummary> {
    let mut random = Seeded(0x5eed_f00d);
    (0..rooms)
        .map(|index| {
            let id = if index == 0 {
                FAMILY_ROOM.to_owned()
            } else {
                format!("!room{index}:{FIXTURE_SERVER}")
            };
            let mut room = summary(
                &id,
                &format!("Room {index} {}", WORDS[random.below(WORDS.len())]),
                if index % 7 == 0 {
                    Encryption::NotEncrypted
                } else {
                    Encryption::Encrypted
                },
                2 + random.below(30) as u64,
                index as u64 * 7 * MINUTE,
                now,
            );
            room.is_direct = index % 5 == 1;
            room.membership = if index % 97 == 3 {
                Membership::Invited
            } else {
                Membership::Joined
            };
            if index % 4 == 0 {
                room.unread.notifications = 1 + random.below(40) as u64;
                room.unread.messages = room.unread.notifications;
            }
            if index % 23 == 0 {
                room.unread.mentions = 1;
            }
            room.preview = preview(
                ["mum", "dad", "ben", "grandad"][random.below(4)],
                &sentence(&mut random, 8),
            );
            room
        })
        .collect()
}

fn sentence(random: &mut Seeded, max_words: usize) -> String {
    let words = 2 + random.below(max_words);
    let mut text = String::new();
    for index in 0..words {
        if index > 0 {
            text.push(' ');
        }
        text.push_str(WORDS[random.below(WORDS.len())]);
    }
    text
}

/// `events` events in one room, a minute or so apart, with day dividers,
/// a mix of plain and formatted messages, some long, and reactions.
pub fn large_timeline(events: usize, now: u64) -> Vec<TimelineItem> {
    let mut random = Seeded(0xfeed_beef);
    let mut b = Builder::new(FAMILY_ROOM);
    let senders = ["mum", "dad", "ben", "grandad", "ana"];
    let start = now - events as u64 * 3 * MINUTE;
    let mut day = None;
    for index in 0..events {
        let at = start + index as u64 * 3 * MINUTE;
        if day != Some(at / DAY) {
            day = Some(at / DAY);
            b.day(at);
        }
        let sender = senders[random.below(senders.len())];
        let text = sentence(&mut random, if index % 9 == 0 { 60 } else { 12 });
        let event = if index % 10 == 3 {
            b.html(
                sender,
                at,
                &text,
                &format!("<strong>{text}</strong> <em>ok</em>"),
            )
        } else {
            b.text(sender, at, &text)
        };
        if index % 6 == 0 {
            event.reactions = vec![reaction("👍", &["mum", "dad"], index % 12 == 0)];
        }
        if index + 30 == events {
            b.read_marker();
        }
    }
    b.finish()
}

// ---------------------------------------------------------------------------
// Pictures
// ---------------------------------------------------------------------------

/// A picture's sky top, sky bottom and ground colours.
type Palette = ([u8; 3], [u8; 3], [u8; 3]);

/// A procedural picture for a fixture image: a sky, the sun and hills, in
/// the colours of the handle's name, `width` by `height` (scaled to fit
/// 960 pixels). No file is read.
pub fn picture(handle: &str, width: u32, height: u32) -> DecodedImage {
    let scale = (960.0 / width.max(height) as f32).min(1.0);
    let width = ((width as f32 * scale) as u32).max(1);
    let height = ((height as f32 * scale) as u32).max(1);
    let palettes: BTreeMap<&str, Palette> = [
        (
            "fixture-media:sunset",
            ([250, 140, 90], [120, 60, 140], [40, 50, 70]),
        ),
        (
            "fixture-media:lighthouse",
            ([150, 200, 235], [235, 245, 250], [60, 110, 90]),
        ),
        (
            "fixture-media:tomatoes",
            ([200, 230, 170], [250, 250, 220], [200, 60, 50]),
        ),
    ]
    .into_iter()
    .collect();
    let (top, bottom, ground) =
        palettes
            .get(handle)
            .copied()
            .unwrap_or(([120, 180, 220], [220, 235, 245], [70, 130, 80]));
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let (sun_x, sun_y, sun_r) = (
        width as f32 * 0.72,
        height as f32 * 0.35,
        height as f32 * 0.11,
    );
    for y in 0..height {
        let t = y as f32 / height as f32;
        for x in 0..width {
            let fx = x as f32;
            let hill = height as f32
                * (0.68 + 0.08 * ((fx / width as f32) * std::f32::consts::TAU * 1.3).sin());
            let colour = if (y as f32) > hill {
                let shade = 1.0 - 0.25 * ((y as f32 - hill) / (height as f32 - hill + 1.0));
                ground.map(|channel| (channel as f32 * shade) as u8)
            } else if (fx - sun_x).powi(2) + (y as f32 - sun_y).powi(2) < sun_r * sun_r {
                [255, 236, 170]
            } else {
                [0, 1, 2].map(|i| (top[i] as f32 * (1.0 - t) + bottom[i] as f32 * t) as u8)
            };
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }
    DecodedImage {
        width,
        height,
        frames: vec![ImageFrame { rgba, delay_ms: 0 }],
    }
}
