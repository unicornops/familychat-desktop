//! The notifier's rules: coalescing per room, the focused room, hidden
//! content, the flood cap (adapted from Zmail's `zmail-notify` tests) and
//! the text made safe to show.

use std::time::{Duration, Instant};

use familychat_core::{
    OwnedRoomId,
    notifications::{IncomingNotification, NotificationContent},
    session::AccountKey,
};

use super::*;

fn account() -> AccountKey {
    AccountKey("account-1".to_owned())
}

fn room_id(name: &str) -> OwnedRoomId {
    format!("!{name}:smith.safechat.family")
        .try_into()
        .expect("a valid room id")
}

fn message(room: &str, sender: &str, text: &str) -> IncomingNotification {
    IncomingNotification {
        account: account(),
        room_id: room_id(room),
        room_name: format!("Room {room}"),
        sender_name: sender.to_owned(),
        is_direct: false,
        content: NotificationContent::Text(text.to_owned()),
    }
}

fn direct(sender: &str, content: NotificationContent) -> IncomingNotification {
    IncomingNotification {
        account: account(),
        room_id: room_id("dm"),
        room_name: sender.to_owned(),
        sender_name: sender.to_owned(),
        is_direct: true,
        content,
    }
}

#[test]
fn a_message_names_the_room_and_the_sender() {
    let mut notifier = Notifier::new();
    let now = Instant::now();
    let posted = notifier.notify(vec![message("a", "Ana", "See you at six")], None, now);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].title, "Room a");
    assert_eq!(posted[0].body, "Ana: See you at six");
    assert_eq!(posted[0].opens.room_id, room_id("a"));
    assert!(posted[0].tag.starts_with("familychat-room-"));
    assert!(
        !posted[0].tag.contains("smith") && !posted[0].tag.contains('!'),
        "the tag holds no Matrix id: {}",
        posted[0].tag
    );

    let posted = notifier.notify(
        vec![direct("Ben", NotificationContent::Text("Hi".to_owned()))],
        None,
        now,
    );
    assert_eq!(
        (posted[0].title.as_str(), posted[0].body.as_str()),
        ("Ben", "Hi")
    );
}

#[test]
fn hidden_content_says_new_message() {
    let mut notifier = Notifier::new();
    notifier.set_show_content(false);
    assert!(!notifier.shows_content());
    let now = Instant::now();
    let posted = notifier.notify(
        vec![
            message("a", "Ana", "the secret plan"),
            direct(
                "Ben",
                NotificationContent::Emote("waves secretly".to_owned()),
            ),
        ],
        None,
        now,
    );
    assert_eq!(posted.len(), 2);
    assert_eq!(posted[0].title, "Room a");
    assert_eq!(posted[0].body, "Ana: New message");
    assert_eq!(posted[1].title, "Ben");
    assert_eq!(posted[1].body, NEW_MESSAGE);
    for notification in &posted {
        assert!(
            !notification.body.contains("secret"),
            "{}",
            notification.body
        );
    }

    let mut invite = direct("Cara", NotificationContent::Invite);
    invite.room_id = room_id("invite");
    let posted = notifier.notify(vec![invite], None, now);
    assert_eq!(posted[0].body, NEW_INVITATION);
    assert!(posted[0].opens.is_invite);

    notifier.set_show_content(true);
    let posted = notifier.notify(vec![message("b", "Ana", "visible")], None, now);
    assert_eq!(posted[0].body, "Ana: visible");
}

#[test]
fn messages_in_one_room_coalesce_under_one_tag() {
    let mut notifier = Notifier::new();
    let now = Instant::now();
    let posted = notifier.notify(
        vec![
            message("a", "Ana", "one"),
            message("b", "Ben", "other room"),
            message("a", "Ana", "two"),
            message("a", "Cara", "three"),
        ],
        None,
        now,
    );
    assert_eq!(posted.len(), 2, "one per room");
    assert_eq!(posted[0].opens.room_id, room_id("b"));
    assert_eq!(posted[1].body, "3 new messages\nCara: three");

    let later = notifier.notify(vec![message("a", "Ana", "four")], None, now);
    assert_eq!(
        later[0].tag, posted[1].tag,
        "the room's notification is replaced"
    );
    assert_eq!(later[0].body, "4 new messages\nAna: four");

    // Seeing the room starts the count again and takes its notification
    // down; a room with nothing new has nothing to take down.
    assert_eq!(
        notifier.room_seen(&account(), &room_id("a")),
        Some(posted[1].tag.clone())
    );
    assert_eq!(notifier.room_seen(&account(), &room_id("a")), None);
    let again = notifier.notify(vec![message("a", "Ana", "five")], None, now);
    assert_eq!(again[0].body, "Ana: five");
}

#[test]
fn nothing_for_the_focused_room() {
    let mut notifier = Notifier::new();
    let now = Instant::now();
    notifier.notify(vec![message("a", "Ana", "before")], None, now);
    let focused_room = room_id("a");
    let posted = notifier.notify(
        vec![
            message("a", "Ana", "while looking"),
            message("b", "Ben", "elsewhere"),
        ],
        Some((&account(), &focused_room)),
        now,
    );
    assert_eq!(posted.len(), 1, "only the other room notifies");
    assert_eq!(posted[0].opens.room_id, room_id("b"));
    // Looking at it counted as seeing it.
    let after = notifier.notify(vec![message("a", "Ana", "after")], None, now);
    assert_eq!(after[0].body, "Ana: after");

    // The same room id on another account is another room.
    let mut other = message("a", "Ana", "other account");
    other.account = AccountKey("account-2".to_owned());
    let posted = notifier.notify(vec![other], Some((&account(), &focused_room)), now);
    assert_eq!(posted.len(), 1);
}

#[test]
fn the_flood_cap_stands_one_summary_in_for_the_rest() {
    let mut notifier = Notifier::new();
    let start = Instant::now();
    for index in 0..MAX_NOTIFICATIONS_PER_MINUTE {
        let posted = notifier.notify(
            vec![message(&format!("r{index}"), "Ana", "hello")],
            None,
            start,
        );
        assert_eq!(posted.len(), 1);
        assert_ne!(posted[0].tag, FLOOD_TAG);
    }

    // The next one is over the cap: one summary, then nothing.
    let posted = notifier.notify(vec![message("busy", "Ana", "secret")], None, start);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].tag, FLOOD_TAG);
    assert_eq!(posted[0].title, APP_NAME);
    assert_eq!(posted[0].body, FLOOD_MESSAGE);
    assert!(!posted[0].body.contains("secret"));
    assert_eq!(posted[0].opens.room_id, room_id("busy"));
    assert_eq!(
        notifier.target(FLOOD_TAG).map(|target| &target.room_id),
        Some(&room_id("busy"))
    );

    // A flood of a hundred in many rooms, inside the same minute.
    let flood: Vec<_> = (0..100)
        .map(|index| message(&format!("f{}", index % 25), "Ana", "spam"))
        .collect();
    let half_minute = start + Duration::from_secs(30);
    assert!(notifier.notify(flood, None, half_minute).is_empty());
    assert!(
        notifier
            .notify(vec![message("busy", "Ana", "more")], None, half_minute)
            .is_empty()
    );

    // Once the minute has passed, notifications flow again, and the count
    // remembers what the cap held back.
    let later = start + RATE_WINDOW + Duration::from_secs(1);
    let posted = notifier.notify(vec![message("busy", "Ana", "after")], None, later);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].body, "3 new messages\nAna: after");
}

#[test]
fn a_rolling_minute_frees_up_one_slot_at_a_time() {
    let mut notifier = Notifier::new();
    let start = Instant::now();
    for index in 0..MAX_NOTIFICATIONS_PER_MINUTE {
        let at = start + Duration::from_secs(index as u64 * 10);
        notifier.notify(vec![message(&format!("r{index}"), "Ana", "x")], None, at);
    }
    // 61 s after the first: one slot is free, so a normal notification.
    let at = start + Duration::from_secs(61);
    let posted = notifier.notify(vec![message("next", "Ana", "x")], None, at);
    assert_ne!(posted[0].tag, FLOOD_TAG);
    let posted = notifier.notify(vec![message("next2", "Ana", "x")], None, at);
    assert_eq!(posted[0].tag, FLOOD_TAG);
}

#[test]
fn clicks_open_the_rooms_that_notified_recently() {
    let mut notifier = Notifier::new();
    let start = Instant::now();
    let first = notifier.notify(vec![message("first", "Ana", "x")], None, start);
    let tag = first[0].tag.clone();
    assert_eq!(
        notifier.target(&tag).map(|target| &target.room_id),
        Some(&room_id("first"))
    );
    for index in 0..REMEMBERED_ROOMS {
        // Spread out so the cap never applies.
        let at = start + RATE_WINDOW * u32::try_from(index + 1).unwrap_or(u32::MAX);
        notifier.notify(vec![message(&format!("r{index}"), "Ana", "x")], None, at);
    }
    assert!(
        notifier.target(&tag).is_none(),
        "the oldest room is forgotten"
    );
    assert!(notifier.target("familychat-room-unknown").is_none());
}

#[test]
fn every_kind_of_message_is_described() {
    let mut notifier = Notifier::new();
    let now = Instant::now() + Duration::from_secs(0);
    let cases = [
        (NotificationContent::Image, "Sent a picture"),
        (NotificationContent::Video, "Sent a video"),
        (NotificationContent::Audio, "Sent an audio clip"),
        (NotificationContent::File, "Sent a file"),
        (NotificationContent::Location, "Shared a location"),
        (NotificationContent::Sticker, "Sent a sticker"),
        (NotificationContent::Unknown, NEW_MESSAGE),
        (NotificationContent::Emote("waves".to_owned()), "Ben waves"),
        (NotificationContent::Invite, "Ben invited you"),
    ];
    for (index, (content, expected)) in cases.into_iter().enumerate() {
        let mut incoming = direct("Ben", content);
        incoming.room_id = room_id(&format!("k{index}"));
        let at = now + RATE_WINDOW * u32::try_from(index).unwrap_or(0);
        let posted = notifier.notify(vec![incoming], None, at);
        assert_eq!(posted[0].body, expected);
    }
    // In a group room an emote reads as one, without "Ben:" in front.
    let mut emote = message("g", "Ben", "");
    emote.content = NotificationContent::Emote("waves".to_owned());
    let at = now + RATE_WINDOW * 20;
    assert_eq!(notifier.notify(vec![emote], None, at)[0].body, "Ben waves");
}

#[test]
fn text_is_made_safe_and_short() {
    assert_eq!(
        clean("  two\n\nlines\tand\u{7}bell ", 50),
        "two lines and bell"
    );
    assert_eq!(
        clean("evil\u{202E}txt.exe\u{2066}x\u{200B}y", 50),
        "eviltxt.exexy",
        "bidi overrides and zero-width spaces are dropped"
    );
    assert_eq!(
        clean("👨\u{200D}👩\u{200D}👧", 50),
        "👨\u{200D}👩\u{200D}👧"
    );
    let long = "word ".repeat(100);
    let cut = clean(&long, MAX_TEXT_CHARS);
    assert_eq!(cut.chars().count(), MAX_TEXT_CHARS);
    assert!(cut.ends_with('…'));
    assert!(!cut.ends_with(" …"));

    let mut notifier = Notifier::new();
    let mut nameless = message("a", "  ", "\u{202E}");
    nameless.room_name = "\u{200B}".to_owned();
    let posted = notifier.notify(vec![nameless], None, Instant::now());
    assert_eq!(posted[0].title, APP_NAME);
    assert_eq!(posted[0].body, "Someone: New message");
}

#[test]
fn debug_shows_no_names_or_text() {
    let mut notifier = Notifier::new();
    let posted = notifier.notify(
        vec![message("secret", "Secret sender", "secret text")],
        None,
        Instant::now(),
    );
    let debug = format!("{posted:?} {notifier:?}");
    assert!(!debug.contains("ecret"), "{debug}");
}

#[test]
fn the_window_title_counts_what_is_unread() {
    assert_eq!(window_title(0), "Family Chat");
    assert_eq!(window_title(3), "Family Chat (3)");
}
